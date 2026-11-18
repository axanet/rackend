//! Channels (SPEC §13.3): definitions (admin CRUD), join and publish
//! authorization, and the periodic re-check of joined members.
//!
//! The join and publish rules are evaluated on join and re-evaluated every
//! 30 s; a member who no longer passes the join rule is removed, one who no
//! longer passes the publish rule stays but cannot publish.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use serde_json::{Map, Value, json};
use uuid::Uuid;

use super::audit::entry;
use super::{App, Principal, RequestCtx};
use crate::domain::channel::{ChannelDef, ChannelOptions, MAX_META_BYTES, valid_pattern};
use crate::error::{FieldError, ValidationErrors};
use crate::ports::StoreExt as _;
use crate::realtime::channels::{ChannelError, Join, MAX_EVENT_LEN, MemberInfo};
use crate::realtime::{Client, ClientAuth};
use crate::rules::{Mode, compile};
use crate::time::format_timestamp;
use crate::{Error, Result};

/// How often joined members are re-authorized.
pub const RECHECK_EVERY: Duration = Duration::from_secs(30);

/// Cached channel definitions.
#[derive(Debug, Default)]
pub struct ChannelCache(RwLock<Arc<Vec<ChannelDef>>>);

impl ChannelCache {
    fn get(&self) -> Arc<Vec<ChannelDef>> {
        Arc::clone(
            &self
                .0
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    fn set(&self, v: Vec<ChannelDef>) {
        *self
            .0
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::new(v);
    }
}

fn channel_err(e: ChannelError) -> Error {
    match e {
        ChannelError::NotJoined => Error::BadRequest("Join the channel first.".into()),
        ChannelError::Full(n) => Error::Conflict(format!("The channel is full ({n} members).")),
        ChannelError::Forbidden => Error::Forbidden("You cannot publish to this channel.".into()),
        ChannelError::RateLimited => Error::RateLimited { retry_after_s: 1 },
        ChannelError::TooLarge(n) => Error::PayloadTooLarge {
            max_bytes: n as u64,
        },
    }
}

fn meta_of(meta: Option<Value>) -> Result<Value> {
    let meta = meta.unwrap_or(Value::Null);
    if !(meta.is_null() || meta.is_object()) {
        return Err(Error::field(
            "meta",
            FieldError::new("invalid", "meta is a JSON object."),
        ));
    }
    if meta.to_string().len() > MAX_META_BYTES {
        return Err(Error::field(
            "meta",
            FieldError::new(
                "too_large",
                format!("meta is at most {MAX_META_BYTES} bytes of JSON."),
            )
            .param("max", MAX_META_BYTES),
        ));
    }
    Ok(meta)
}

fn def_json(d: &ChannelDef, members: usize, instances: usize) -> Value {
    json!({
        "id": d.id,
        "name": d.name,
        "pattern": d.pattern,
        "join_rule": d.join_rule,
        "publish_rule": d.publish_rule,
        "presence": d.presence,
        "options": {
            "max_members": d.options.max_members,
            "rate_per_s": d.options.rate_per_s,
            "echo": d.options.echo,
            "max_payload": d.options.max_payload,
        },
        "created": d.created,
        "updated": d.updated,
        "instances": instances,
        "members": members,
    })
}

impl App {
    /// Re-reads the channel definitions.
    pub(crate) async fn reload_channels(&self) -> Result<()> {
        let defs = self.store.read(|tx| tx.channels_list()).await?;
        self.channel_defs.set(defs);
        Ok(())
    }

    fn channel_def_for(&self, channel: &str) -> Result<(ChannelDef, Option<String>)> {
        self.channel_defs
            .get()
            .iter()
            .find_map(|d| d.key_of(channel).map(|k| (d.clone(), k)))
            .ok_or_else(|| {
                Error::field(
                    "channel",
                    FieldError::new("unknown_channel", "No channel matches this name."),
                )
            })
    }

    /// Whether `auth` passes `rule` (`None`: superusers only, `""`: everyone).
    async fn channel_rule(
        &self,
        rule: Option<&str>,
        auth: &ClientAuth,
        key: Option<&str>,
    ) -> Result<bool> {
        if matches!(auth.principal, Principal::Superuser { .. }) {
            return Ok(true);
        }
        let Some(src) = rule else { return Ok(false) };
        if src.trim().is_empty() {
            return Ok(true);
        }
        let typed = compile(&self.schema.get().schema, None, src, Mode::Channel)?;
        let mut req = RequestCtx::system(auth.principal.clone(), self.clock.now()).request_values();
        req.context = "realtime".into();
        req.channel_key = key.map(str::to_owned);
        self.store.read(move |tx| tx.rule_holds(&typed, &req)).await
    }

    /// WebSocket `join`.
    pub async fn channel_join(
        &self,
        client: &Arc<Client>,
        channel: &str,
        meta: Option<Value>,
    ) -> Result<()> {
        let meta = meta_of(meta)?;
        let (def, key) = self.channel_def_for(channel)?;
        let auth = client.auth();
        if !self
            .channel_rule(def.join_rule.as_deref(), &auth, key.as_deref())
            .await?
        {
            return Err(Error::Forbidden("You cannot join this channel.".into()));
        }
        let can_publish = self
            .channel_rule(def.publish_rule.as_deref(), &auth, key.as_deref())
            .await?;
        let (user_id, collection) = match &auth.principal {
            Principal::Anonymous => (None, None),
            Principal::Superuser { id, .. } => (Some(id.clone()), Some("_superusers".to_owned())),
            Principal::User {
                id,
                collection_name,
                ..
            } => (Some(id.clone()), Some(collection_name.clone())),
        };
        let subject = user_id.is_some().then(|| auth.subject_key());
        let info = MemberInfo {
            user_id,
            conn_id: client.conn_id.clone(),
            collection,
            meta,
            joined_at: format_timestamp(self.clock.now()),
        };
        let limit = self.realtime_limits().outbound_bytes;
        self.hub
            .channels
            .join(
                client,
                Join {
                    channel,
                    def: &def,
                    info,
                    subject,
                    can_publish,
                },
                limit,
            )
            .map_err(channel_err)
    }

    /// WebSocket `leave`.
    pub fn channel_leave(&self, client: &Client, channel: &str) -> Result<()> {
        let limit = self.realtime_limits().outbound_bytes;
        if self.hub.channels.leave(&client.id, channel, limit) {
            Ok(())
        } else {
            Err(channel_err(ChannelError::NotJoined))
        }
    }

    /// WebSocket `pub`.
    pub fn channel_publish(
        &self,
        client: &Client,
        channel: &str,
        event: &str,
        data: &Value,
    ) -> Result<()> {
        if event.is_empty() || event.len() > MAX_EVENT_LEN {
            return Err(Error::field(
                "event",
                FieldError::new(
                    "invalid",
                    format!("event is 1 to {MAX_EVENT_LEN} characters."),
                )
                .param("max", MAX_EVENT_LEN),
            ));
        }
        let limit = self.realtime_limits().outbound_bytes;
        self.hub
            .channels
            .publish(client, channel, event, data, limit)
            .map_err(channel_err)
    }

    /// WebSocket `meta`.
    pub fn channel_meta(&self, client: &Client, channel: &str, meta: Option<Value>) -> Result<()> {
        let meta = meta_of(meta)?;
        let limit = self.realtime_limits().outbound_bytes;
        self.hub
            .channels
            .set_meta(&client.id, channel, meta, limit)
            .map_err(channel_err)
    }

    /// Re-authorizes every joined member (every [`RECHECK_EVERY`]).
    pub async fn channels_recheck(&self) -> Result<()> {
        let defs = self.channel_defs.get();
        let limit = self.realtime_limits().outbound_bytes;
        for (channel, def_id, client) in self.hub.channels.members() {
            let Some(def) = defs.iter().find(|d| d.id == def_id) else {
                self.hub.channels.kick(&client.id, &channel, limit);
                continue;
            };
            let key = def.key_of(&channel).flatten();
            let auth = client.auth();
            if !self
                .channel_rule(def.join_rule.as_deref(), &auth, key.as_deref())
                .await?
            {
                self.hub.channels.kick(&client.id, &channel, limit);
                continue;
            }
            let can = self
                .channel_rule(def.publish_rule.as_deref(), &auth, key.as_deref())
                .await?;
            self.hub.channels.set_can_publish(&client.id, &channel, can);
        }
        Ok(())
    }

    // Admin ----------------------------------------------------------------

    /// `GET /api/channels`.
    pub fn channels_list(&self, ctx: &RequestCtx) -> Result<Value> {
        ctx.require_superuser()?;
        let live = self.hub.channels.snapshot();
        let items: Vec<Value> = self
            .channel_defs
            .get()
            .iter()
            .map(|d| {
                let mine: Vec<_> = live.iter().filter(|i| i.def_id == d.id).collect();
                def_json(d, mine.iter().map(|i| i.members.len()).sum(), mine.len())
            })
            .collect();
        Ok(json!({ "items": items }))
    }

    /// `GET /api/channels/live`: running instances with their members.
    pub fn channels_live(&self, ctx: &RequestCtx) -> Result<Value> {
        ctx.require_superuser()?;
        let items: Vec<Value> = self
            .hub
            .channels
            .snapshot()
            .into_iter()
            .map(|i| {
                json!({
                    "channel": i.channel,
                    "channel_id": i.def_id,
                    "messages": i.messages,
                    "members": i.members.iter().map(|m| json!({
                        "user_id": m.user_id, "conn_id": m.conn_id, "collection": m.collection,
                        "meta": m.meta, "joined_at": m.joined_at,
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();
        Ok(json!({
            "items": items,
            "published": self.hub.channels.published.load(std::sync::atomic::Ordering::Relaxed),
        }))
    }

    /// `POST /api/channels` (no `id`) or `PATCH /api/channels/{id}`.
    pub async fn channel_save(
        &self,
        ctx: &RequestCtx,
        id: Option<&str>,
        body: Value,
    ) -> Result<Value> {
        ctx.require_superuser()?;
        let body: Map<String, Value> = body
            .as_object()
            .cloned()
            .ok_or_else(|| Error::BadRequest("Send a JSON object.".into()))?;
        let defs = self.channel_defs.get();
        let existing = match id {
            Some(id) => Some(
                defs.iter()
                    .find(|d| d.id == id)
                    .cloned()
                    .ok_or(Error::NotFound)?,
            ),
            None => None,
        };
        let now = format_timestamp(ctx.now);
        let mut d = existing.clone().unwrap_or_else(|| ChannelDef {
            id: Uuid::now_v7().to_string(),
            name: String::new(),
            pattern: String::new(),
            join_rule: None,
            publish_rule: None,
            presence: true,
            options: ChannelOptions::default(),
            created: now.clone(),
            updated: now.clone(),
        });
        d.updated = now;
        let mut errs = ValidationErrors::default();
        if let Some(v) = body.get("name") {
            d.name = v.as_str().unwrap_or_default().trim().to_owned();
        }
        if d.name.is_empty() || d.name.chars().count() > 64 {
            errs.add(
                "name",
                FieldError::new("invalid", "Use 1 to 64 characters."),
            );
        }
        if let Some(v) = body.get("pattern") {
            d.pattern = v.as_str().unwrap_or_default().trim().to_owned();
        }
        if !valid_pattern(&d.pattern) {
            errs.add(
                "pattern",
                FieldError::new(
                    "invalid_pattern",
                    "Use a lowercase name such as lobby, optionally followed by :* (room:*).",
                ),
            );
        }
        for (k, slot) in [
            ("join_rule", &mut d.join_rule),
            ("publish_rule", &mut d.publish_rule),
        ] {
            match body.get(k) {
                None => {}
                Some(Value::Null) => *slot = None,
                Some(Value::String(s)) => *slot = Some(s.trim().to_owned()),
                Some(_) => errs.add(k, FieldError::new("invalid", "A rule is a string or null.")),
            }
        }
        if let Some(v) = body.get("presence") {
            match v.as_bool() {
                Some(b) => d.presence = b,
                None => errs.add("presence", FieldError::new("invalid", "true or false.")),
            }
        }
        if let Some(o) = body.get("options") {
            let Some(o) = o.as_object() else {
                return Err(Error::field(
                    "options",
                    FieldError::new("invalid", "options is an object."),
                ));
            };
            let num = |k: &str, min: u64, max: u64, errs: &mut ValidationErrors| -> Option<u64> {
                let v = o.get(k)?;
                match v.as_u64().filter(|n| (min..=max).contains(n)) {
                    Some(n) => Some(n),
                    None => {
                        errs.add(
                            format!("options.{k}"),
                            FieldError::new("out_of_range", format!("Use {min} to {max}."))
                                .param("min", min)
                                .param("max", max),
                        );
                        None
                    }
                }
            };
            if let Some(n) = num("max_members", 1, 100_000, &mut errs) {
                d.options.max_members = u32::try_from(n).unwrap_or(u32::MAX);
            }
            if let Some(n) = num("rate_per_s", 1, 1000, &mut errs) {
                d.options.rate_per_s = u32::try_from(n).unwrap_or(u32::MAX);
            }
            if let Some(n) = num("max_payload", 1, 64 * 1024, &mut errs) {
                d.options.max_payload = usize::try_from(n).unwrap_or(usize::MAX);
            }
            if let Some(v) = o.get("echo") {
                match v.as_bool() {
                    Some(b) => d.options.echo = b,
                    None => errs.add("options.echo", FieldError::new("invalid", "true or false.")),
                }
            }
        }
        if !errs.is_empty() {
            return Err(Error::Validation(errs));
        }
        {
            let schema = &self.schema.get().schema;
            for (k, rule) in [
                ("join_rule", &d.join_rule),
                ("publish_rule", &d.publish_rule),
            ] {
                if let Some(src) = rule.as_deref().filter(|s| !s.is_empty())
                    && let Err(e) = compile(schema, None, src, Mode::Channel)
                {
                    return Err(Error::RuleCompile {
                        message: format!("{k}: {}", e.message),
                        position: e.position,
                    });
                }
            }
        }
        let (row, actor, at, action) = (
            d.clone(),
            ctx.actor(),
            ctx.now,
            if existing.is_some() {
                "channel.update"
            } else {
                "channel.create"
            },
        );
        self.store
            .write(move |tx| {
                tx.channel_put(&row)?;
                let mut e = entry(&actor, action, at);
                e.meta = Some(json!({ "id": row.id, "pattern": row.pattern }));
                tx.audit_insert(&e)
            })
            .await?;
        self.reload_channels().await?;
        if existing.is_some() {
            self.hub
                .channels
                .def_changed(&d.id, Some(&d), self.realtime_limits().outbound_bytes);
        }
        Ok(def_json(&d, 0, 0))
    }

    /// `DELETE /api/channels/{id}`: removes the definition and every member
    /// of its instances.
    pub async fn channel_delete(&self, ctx: &RequestCtx, id: &str) -> Result<()> {
        ctx.require_superuser()?;
        let (id2, actor, at) = (id.to_owned(), ctx.actor(), ctx.now);
        self.store
            .write(move |tx| {
                if !tx.channel_delete(&id2)? {
                    return Err(Error::NotFound);
                }
                let mut e = entry(&actor, "channel.delete", at);
                e.meta = Some(json!({ "id": id2 }));
                tx.audit_insert(&e)
            })
            .await?;
        self.reload_channels().await?;
        self.hub
            .channels
            .def_changed(id, None, self.realtime_limits().outbound_bytes);
        Ok(())
    }
}
