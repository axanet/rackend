//! Realtime use cases (SPEC §13.2): subscriptions, per-subscriber
//! authorization of every event, session-revocation disconnects.
//!
//! Dispatch groups subscribers that authorize identically (same subject,
//! topic, filter, expand and fields) and evaluates each group once, in one
//! read transaction per event. Rules are checked against the record values
//! of the event (before and after), so deletes are judged by the record as
//! it was, and an update that hides a record sends `delete` with
//! `reason: "no_longer_visible"`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value, json};
use uuid::Uuid;

use super::records::{ExpandTree, access, expand, parse_expand, project};
use super::sessions::user_principal;
use super::{App, Principal, RequestCtx};
use crate::domain::{Collection, Record, RuleOp};
use crate::error::FieldError;
use crate::ports::StoreExt;
use crate::realtime::{Action, Client, ClientAuth, Frame, Out, RecordChange, Subscription, Topic};
use crate::rules::{Mode, TypedRule, compile};
use crate::time::format_timestamp;
use crate::{Error, Result};

/// How often sessions of connected clients are checked (revocation closes
/// the connection within 5 s, SPEC §13.2).
const SWEEP_EVERY: Duration = Duration::from_secs(2);

/// A subscription request from a transport.
#[derive(Debug, Clone, Default)]
pub struct SubRequest {
    /// WebSocket subscription id.
    pub id: Option<u64>,
    /// `records:{collection}` or `records:{collection}/{id}`.
    pub topic: String,
    /// Filter.
    pub filter: Option<String>,
    /// Expand.
    pub expand: Option<String>,
    /// Fields.
    pub fields: Option<String>,
}

/// Realtime limits from the settings (SPEC §13.1).
#[derive(Debug, Clone, Copy)]
pub struct RealtimeLimits {
    /// Attached connections per IP.
    pub connections_per_ip: usize,
    /// Subscriptions per connection.
    pub subscriptions: usize,
    /// Largest inbound message.
    pub message_bytes: usize,
    /// Largest outbound queue before `slow_consumer`.
    pub outbound_bytes: usize,
}

/// Subject, topic, filter, expand, fields: what makes authorization identical.
type GroupKey = (
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// One dispatch group: identical authorization for all its clients.
struct Group {
    auth: ClientAuth,
    sub: Subscription,
    clients: Vec<Arc<Client>>,
}

/// The decision for one group.
struct Delivery {
    action: Action,
    record: Value,
    reason: Option<&'static str>,
}

fn n(v: i64) -> usize {
    usize::try_from(v.max(0)).unwrap_or(usize::MAX)
}

impl App {
    /// Current realtime limits.
    pub fn realtime_limits(&self) -> RealtimeLimits {
        let s = self.settings();
        RealtimeLimits {
            connections_per_ip: n(s.int("realtime", "max_connections_per_ip", 100)),
            subscriptions: n(s.int("realtime", "max_subscriptions_per_connection", 100)),
            message_bytes: n(s.int("realtime", "max_message_kb", 64)) * 1024,
            outbound_bytes: n(s.int("realtime", "max_outbound_queue_kb", 1024)) * 1024,
        }
    }

    /// Starts the dispatcher and the sweeper (called once by `App::load`).
    pub(crate) fn realtime_start(self: &Arc<Self>) {
        if let Some(mut rx) = self.hub.take_receiver() {
            let me = Arc::downgrade(self);
            self.spawner.spawn(
                "realtime.dispatch",
                Box::pin(async move {
                    while let Some(change) = rx.recv().await {
                        let Some(app) = me.upgrade() else { return };
                        app.realtime_resync_overflowed();
                        app.realtime_dispatch(change).await;
                    }
                }),
            );
        }
        let me = Arc::downgrade(self);
        let sleep = Arc::clone(&self.spawner);
        self.spawner.spawn(
            "realtime.sweep",
            Box::pin(async move {
                loop {
                    sleep.sleep(SWEEP_EVERY).await;
                    let Some(app) = me.upgrade() else { return };
                    app.realtime_resync_overflowed();
                    if let Err(e) = app.realtime_sweep().await {
                        tracing::warn!(error = %e, "realtime session sweep failed");
                    }
                }
            }),
        );
    }

    /// A new connection. Fails when the IP has too many.
    pub fn realtime_connect(
        &self,
        ip: Option<String>,
    ) -> Result<(Arc<Client>, tokio::sync::mpsc::UnboundedReceiver<Out>)> {
        let limits = self.realtime_limits();
        if let Some(ip) = &ip
            && self.hub.connections_from(ip) >= limits.connections_per_ip
        {
            return Err(Error::RateLimited { retry_after_s: 10 });
        }
        Ok(self.hub.connect(ip))
    }

    /// Binds the caller's identity to a client. A client bound to one
    /// account cannot be taken over by another.
    pub fn realtime_auth(&self, client: &Client, ctx: &RequestCtx) -> Result<()> {
        let session_id = match &ctx.principal {
            Principal::Anonymous => None,
            Principal::Superuser { session_id, .. } | Principal::User { session_id, .. } => {
                Some(session_id.clone())
            }
        };
        let new = ClientAuth {
            principal: ctx.principal.clone(),
            session_id,
        };
        let current = client.auth();
        if !matches!(current.principal, Principal::Anonymous)
            && current.subject_key() != new.subject_key()
        {
            return Err(Error::Forbidden(
                "This connection belongs to another account.".into(),
            ));
        }
        if matches!(new.principal, Principal::Anonymous)
            && !matches!(current.principal, Principal::Anonymous)
        {
            // Keep the identity; an unauthenticated request does not sign out.
            return Ok(());
        }
        self.hub.set_auth(client, new);
        Ok(())
    }

    /// Validates a subscription for `auth`.
    fn parse_subscription(&self, auth: &ClientAuth, req: &SubRequest) -> Result<Subscription> {
        let bad = |code: &'static str, msg: &str| Error::field("topic", FieldError::new(code, msg));
        let rest = req.topic.strip_prefix("records:").ok_or_else(|| {
            bad(
                "invalid_topic",
                "Topics look like records:<collection> or records:<collection>/<id>.",
            )
        })?;
        let (name, record) = match rest.split_once('/') {
            Some((n, r)) => (n, Some(r)),
            None => (rest, None),
        };
        let compiled = self.schema.get();
        let c = compiled.schema.by_id_or_name(name).ok_or(Error::NotFound)?;
        let superuser = matches!(auth.principal, Principal::Superuser { .. });
        if !superuser && c.rules.view.is_none() {
            return Err(Error::Forbidden(
                "Only superusers can view this collection.".into(),
            ));
        }
        let topic = match record {
            None => Topic::Collection {
                id: c.id,
                name: name.to_owned(),
            },
            Some(r) => Topic::Record {
                id: c.id,
                name: name.to_owned(),
                record: Uuid::try_parse(r)
                    .map_err(|_| bad("invalid_topic", "The record id is not a UUID."))?
                    .to_string(),
            },
        };
        let filter = req.filter.clone().filter(|f| !f.trim().is_empty());
        if let Some(f) = &filter {
            compile(&compiled.schema, Some(c), f, Mode::Subscription)?;
        }
        parse_expand(req.expand.as_deref())?;
        Ok(Subscription {
            id: req.id,
            topic,
            filter,
            expand: req.expand.clone().filter(|e| !e.trim().is_empty()),
            fields: req.fields.clone().filter(|f| !f.trim().is_empty()),
        })
    }

    /// SSE: binds the caller and replaces the client's subscriptions.
    pub fn realtime_set_subscriptions(
        &self,
        ctx: &RequestCtx,
        client_id: &str,
        reqs: &[SubRequest],
    ) -> Result<()> {
        let client = self
            .hub
            .get(client_id)
            .filter(|c| !c.is_closed())
            .ok_or(Error::NotFound)?;
        self.realtime_auth(&client, ctx)?;
        let limits = self.realtime_limits();
        if reqs.len() > limits.subscriptions {
            return Err(Error::field(
                "subscriptions",
                FieldError::new(
                    "too_many",
                    format!("At most {} subscriptions.", limits.subscriptions),
                )
                .param("max", limits.subscriptions),
            ));
        }
        let auth = client.auth();
        let mut subs = Vec::with_capacity(reqs.len());
        for (i, r) in reqs.iter().enumerate() {
            match self.parse_subscription(&auth, r) {
                Ok(s) => subs.push(s),
                Err(Error::Validation(v)) => {
                    let mut out = crate::error::ValidationErrors::default();
                    for (k, errs) in v.0 {
                        for e in errs {
                            out.add(format!("subscriptions.{i}.{k}"), e);
                        }
                    }
                    return Err(Error::Validation(out));
                }
                Err(e) => return Err(e),
            }
        }
        self.hub.set_subscriptions(&client, subs);
        Ok(())
    }

    /// WebSocket: adds one subscription.
    pub fn realtime_subscribe(&self, client: &Client, req: &SubRequest) -> Result<()> {
        let sub = self.parse_subscription(&client.auth(), req)?;
        let limits = self.realtime_limits();
        let replacing = sub
            .id
            .is_some_and(|id| client.subscriptions().iter().any(|s| s.id == Some(id)));
        if !replacing && client.subscriptions().len() >= limits.subscriptions {
            return Err(Error::field(
                "topic",
                FieldError::new(
                    "too_many",
                    format!("At most {} subscriptions.", limits.subscriptions),
                )
                .param("max", limits.subscriptions),
            ));
        }
        self.hub.add_subscription(client, sub);
        Ok(())
    }

    /// WebSocket: removes subscriptions by id or topic.
    pub fn realtime_unsubscribe(
        &self,
        client: &Client,
        id: Option<u64>,
        topic: Option<&str>,
    ) -> usize {
        self.hub.remove_subscriptions(client, id, topic)
    }

    /// Sends `resync` to subscribers of collections whose events were dropped.
    fn realtime_resync_overflowed(&self) {
        let limit = self.realtime_limits().outbound_bytes;
        for cid in self.hub.take_overflowed() {
            for (client, topics) in self.hub.subscribers_of(cid) {
                for topic in topics {
                    client.push(Out::Resync { topic }, limit);
                }
            }
        }
    }

    /// Closes clients whose session was revoked or expired; prunes
    /// disconnected clients past the resume window.
    pub async fn realtime_sweep(&self) -> Result<usize> {
        self.hub.prune();
        let bound = self.hub.sessions();
        if bound.is_empty() {
            return Ok(0);
        }
        let ids: Vec<String> = bound
            .iter()
            .map(|(_, s)| s.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let now = format_timestamp(self.clock.now());
        let live: HashSet<String> = self
            .store
            .read(move |tx| tx.sessions_live(&ids, &now))
            .await?
            .into_iter()
            .collect();
        let mut closed = 0;
        for (client, sid) in bound {
            if !live.contains(&sid) {
                client.close("session_revoked");
                closed += 1;
            }
        }
        Ok(closed)
    }

    /// Delivers one committed change to every authorized subscriber.
    pub(crate) async fn realtime_dispatch(&self, change: RecordChange) {
        let Some(rec) = change.after.as_ref().or(change.before.as_ref()) else {
            return;
        };
        let record_id = rec.id.to_string();
        let compiled = self.schema.get();
        let Some(c) = compiled.schema.by_id(change.collection_id).cloned() else {
            return;
        };
        // A changed auth record refreshes `@request.auth.*` of its connections.
        if c.auth().is_some()
            && let (Action::Update, Some(after)) = (change.action, &change.after)
            && let Principal::User { fields, .. } = user_principal(&c, after, "")
        {
            self.hub.update_user_fields(&record_id, &fields);
        }
        let interests = self.hub.interested(c.id, &record_id);
        if interests.is_empty() {
            return;
        }
        let mut groups: BTreeMap<GroupKey, Group> = BTreeMap::new();
        for i in interests {
            for s in i.subs {
                let key = (
                    i.auth.subject_key(),
                    s.topic.as_string(),
                    s.filter.clone(),
                    s.expand.clone(),
                    s.fields.clone(),
                );
                let g = groups.entry(key).or_insert_with(|| Group {
                    auth: i.auth.clone(),
                    sub: s.clone(),
                    clients: Vec::new(),
                });
                if !g.clients.iter().any(|x| Arc::ptr_eq(x, &i.client)) {
                    g.clients.push(Arc::clone(&i.client));
                }
            }
        }
        let groups: Vec<Group> = groups.into_values().collect();
        let plans: Vec<(ClientAuth, Subscription)> = groups
            .iter()
            .map(|g| (g.auth.clone(), g.sub.clone()))
            .collect();
        let now = self.clock.now();
        let change2 = change.clone();
        let compiled2 = Arc::clone(&compiled);
        let decided = self
            .store
            .read(move |tx| {
                let mut filters: HashMap<String, TypedRule> = HashMap::new();
                let mut out = Vec::with_capacity(plans.len());
                for (auth, sub) in &plans {
                    out.push(decide(
                        tx,
                        &compiled2,
                        &c,
                        &change2,
                        auth,
                        sub,
                        &mut filters,
                        now,
                    )?);
                }
                Ok(out)
            })
            .await;
        let decided = match decided {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(error = %e, "realtime dispatch failed; subscribers get resync");
                let limit = self.realtime_limits().outbound_bytes;
                for g in &groups {
                    for client in &g.clients {
                        client.push(
                            Out::Resync {
                                topic: g.sub.topic.as_string(),
                            },
                            limit,
                        );
                    }
                }
                return;
            }
        };
        let limit = self.realtime_limits().outbound_bytes;
        let ts = format_timestamp(now);
        for (g, d) in groups.iter().zip(decided) {
            let Some(d) = d else { continue };
            let topic = g.sub.topic.as_string();
            let mut body = json!({
                "topic": topic, "action": d.action.as_str(), "record": d.record, "ts": ts,
            });
            if let Some(r) = d.reason {
                body["reason"] = Value::from(r);
            }
            let with = |k: &str, v: &str| {
                let mut m = Map::new();
                m.insert(k.into(), Value::from(v));
                if let Value::Object(o) = &body {
                    m.extend(o.clone());
                }
                Value::Object(m).to_string()
            };
            let frame = Arc::new(Frame {
                topic: topic.clone(),
                ws: with("t", "record"),
                sse: with("type", "record"),
            });
            for client in &g.clients {
                client.push(
                    Out::Event {
                        seq: 0,
                        frame: Arc::clone(&frame),
                    },
                    limit,
                );
            }
        }
        self.hub
            .dispatched
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Whether `auth` may see `record` under `sub`: the view rule and the filter.
#[allow(clippy::too_many_arguments)]
fn visible(
    tx: &mut dyn crate::ports::ReadTx,
    c: &Collection,
    view: Option<&TypedRule>,
    filter: Option<&TypedRule>,
    record: &Record,
    req: &crate::rules::eval::RequestValues,
) -> Result<bool> {
    if let Some(rule) = view
        && !tx.record_values_match(c, record, rule, req)?
    {
        return Ok(false);
    }
    if let Some(f) = filter
        && !tx.record_values_match(c, record, f, req)?
    {
        return Ok(false);
    }
    Ok(true)
}

/// The delivery for one group, or `None`.
#[allow(clippy::too_many_arguments)]
fn decide(
    tx: &mut dyn crate::ports::ReadTx,
    compiled: &super::CompiledSchema,
    c: &Arc<Collection>,
    change: &RecordChange,
    auth: &ClientAuth,
    sub: &Subscription,
    filters: &mut HashMap<String, TypedRule>,
    now: time::OffsetDateTime,
) -> Result<Option<Delivery>> {
    let superuser = matches!(auth.principal, Principal::Superuser { .. });
    let view = match access(compiled, c, RuleOp::View, superuser) {
        Ok(r) => r.cloned(),
        Err(Error::Forbidden(_)) => return Ok(None),
        Err(e) => return Err(e),
    };
    let filter = match &sub.filter {
        None => None,
        Some(src) => {
            if !filters.contains_key(src) {
                match compile(&compiled.schema, Some(c), src, Mode::Subscription) {
                    Ok(r) => {
                        filters.insert(src.clone(), r);
                    }
                    // The schema changed under the subscription; skip it.
                    Err(_) => return Ok(None),
                }
            }
            filters.get(src).cloned()
        }
    };
    let req = RequestCtx::system(auth.principal.clone(), now).request_values();
    let mut check = |r: &Record| visible(tx, c, view.as_deref(), filter.as_ref(), r, &req);
    let (action, record, reason) = match change.action {
        Action::Create => match &change.after {
            Some(a) if check(a)? => (Action::Create, a, None),
            _ => return Ok(None),
        },
        Action::Delete => match &change.before {
            Some(b) if check(b)? => (Action::Delete, b, None),
            _ => return Ok(None),
        },
        Action::Update => {
            let (Some(b), Some(a)) = (&change.before, &change.after) else {
                return Ok(None);
            };
            if check(a)? {
                (Action::Update, a, None)
            } else if check(b)? {
                (Action::Delete, b, Some("no_longer_visible"))
            } else {
                return Ok(None);
            }
        }
    };
    // Expansion applies the related collections' view rules; deletes are
    // sent without expansion (related records may be gone too).
    let tree = if action == Action::Delete {
        ExpandTree::default()
    } else {
        parse_expand(sub.expand.as_deref())?
    };
    let mut items = expand(
        tx,
        compiled,
        c,
        std::slice::from_ref(record),
        &tree,
        &req,
        superuser,
    )?;
    let Some(item) = items.pop() else {
        return Ok(None);
    };
    let item = sub
        .fields
        .as_deref()
        .map_or(item.clone(), |f| project(&item, f));
    Ok(Some(Delivery {
        action,
        record: Value::Object(item),
        reason,
    }))
}
