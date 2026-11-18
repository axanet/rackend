//! Channel instances and presence (SPEC §13.3), in memory only.
//!
//! An instance exists while it has members. Messages are delivered at most
//! once to the members joined at that moment. A member whose connection
//! drops stays listed for [`DEBOUNCE`]: when the same account joins again
//! within that window, the new member replaces the old one with a single
//! `join` that names the replaced connection, so online lists do not flap.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{Client, Out};
use crate::domain::channel::{ChannelDef, ChannelOptions};

/// How long a disconnected member stays listed.
pub const DEBOUNCE: Duration = Duration::from_secs(3);
/// Longest `event` name of a message.
pub const MAX_EVENT_LEN: usize = 64;

/// What other members see of a member.
#[derive(Debug, Clone, PartialEq)]
pub struct MemberInfo {
    /// Account id; `None` for anonymous members.
    pub user_id: Option<String>,
    /// Public connection id.
    pub conn_id: String,
    /// Auth collection name (`_superusers` for superusers).
    pub collection: Option<String>,
    /// Presence metadata (at most 1 KB of JSON).
    pub meta: Value,
    /// Join time.
    pub joined_at: String,
}

impl MemberInfo {
    fn json(&self) -> Value {
        json!({
            "user_id": self.user_id,
            "conn_id": self.conn_id,
            "collection": self.collection,
            "meta": self.meta,
            "joined_at": self.joined_at,
        })
    }
}

#[derive(Debug)]
struct Member {
    info: MemberInfo,
    client: Arc<Client>,
    /// Same account (for replacing a disconnected member).
    subject: Option<String>,
    can_publish: bool,
    tokens: f64,
    refilled: Instant,
    gone_at: Option<Instant>,
}

#[derive(Debug)]
struct Instance {
    def_id: String,
    presence: bool,
    options: ChannelOptions,
    members: Vec<Member>,
    messages: u64,
}

impl Instance {
    fn send(&self, text: &Arc<str>, except: Option<&str>, limit: usize) {
        for m in &self.members {
            if m.gone_at.is_none() && except != Some(m.client.id.as_str()) {
                m.client.push(Out::Text(Arc::clone(text)), limit);
            }
        }
    }

    fn presence(
        &self,
        channel: &str,
        kind: &str,
        members: &[&MemberInfo],
        extra: Option<(&str, &str)>,
    ) -> Arc<str> {
        let mut v = json!({
            "t": "presence",
            "channel": channel,
            "kind": kind,
            "members": members.iter().map(|m| m.json()).collect::<Vec<_>>(),
        });
        if let Some((k, x)) = extra {
            v[k] = Value::from(x);
        }
        v.to_string().into()
    }
}

/// Why a channel operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelError {
    /// The client has not joined the channel.
    NotJoined,
    /// The channel instance is full.
    Full(u32),
    /// The publish rule denies the member.
    Forbidden,
    /// More messages per second than allowed.
    RateLimited,
    /// `data` is larger than allowed.
    TooLarge(usize),
}

/// A channel instance as the admin UI shows it.
#[derive(Debug, Clone)]
pub struct InstanceView {
    /// Channel name, e.g. `room:42`.
    pub channel: String,
    /// Definition id.
    pub def_id: String,
    /// Members (including ones in the disconnect window).
    pub members: Vec<MemberInfo>,
    /// Messages delivered since the instance appeared.
    pub messages: u64,
}

/// All channel instances.
#[derive(Debug, Default)]
pub struct Channels {
    instances: Mutex<HashMap<String, Instance>>,
    /// Messages published (metrics).
    pub published: AtomicU64,
}

/// A join request after authorization.
#[derive(Debug)]
pub struct Join<'a> {
    /// Channel name.
    pub channel: &'a str,
    /// Its definition.
    pub def: &'a ChannelDef,
    /// The member.
    pub info: MemberInfo,
    /// Account key (`None` for anonymous members).
    pub subject: Option<String>,
    /// Whether the publish rule allows the member.
    pub can_publish: bool,
}

impl Channels {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Instance>> {
        self.instances
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Joins (or, when already joined, updates the meta and resends the
    /// member list).
    pub fn join(
        &self,
        client: &Arc<Client>,
        j: Join<'_>,
        limit: usize,
    ) -> Result<(), ChannelError> {
        let mut all = self.lock();
        let inst = all.entry(j.channel.to_owned()).or_insert_with(|| Instance {
            def_id: j.def.id.clone(),
            presence: j.def.presence,
            options: j.def.options.clone(),
            members: Vec::new(),
            messages: 0,
        });
        if let Some(m) = inst
            .members
            .iter_mut()
            .find(|m| m.client.id == client.id && m.gone_at.is_none())
        {
            m.info.meta = j.info.meta;
            m.can_publish = j.can_publish;
            if inst.presence {
                let info = m.info.clone();
                let text = inst.presence(j.channel, "meta", &[&info], None);
                inst.send(&text, None, limit);
            }
            return Ok(());
        }
        let active = inst.members.iter().filter(|m| m.gone_at.is_none()).count();
        if active >= inst.options.max_members as usize {
            return Err(ChannelError::Full(inst.options.max_members));
        }
        let replaced = j.subject.as_ref().and_then(|s| {
            let i = inst
                .members
                .iter()
                .position(|m| m.gone_at.is_some() && m.subject.as_ref() == Some(s))?;
            Some(inst.members.remove(i).info.conn_id)
        });
        let rate = f64::from(inst.options.rate_per_s.max(1));
        inst.members.push(Member {
            info: j.info.clone(),
            client: Arc::clone(client),
            subject: j.subject,
            can_publish: j.can_publish,
            tokens: rate,
            refilled: Instant::now(),
            gone_at: None,
        });
        if inst.presence {
            let everyone: Vec<&MemberInfo> = inst.members.iter().map(|m| &m.info).collect();
            let sync = inst.presence(j.channel, "sync", &everyone, None);
            client.push(Out::Text(sync), limit);
            let joined = inst.presence(
                j.channel,
                "join",
                &[&j.info],
                replaced.as_deref().map(|r| ("replaced", r)),
            );
            inst.send(&joined, Some(&client.id), limit);
        }
        Ok(())
    }

    /// Leaves at once. Returns false when the client was not a member.
    pub fn leave(&self, client_id: &str, channel: &str, limit: usize) -> bool {
        let mut all = self.lock();
        let Some(inst) = all.get_mut(channel) else {
            return false;
        };
        let Some(i) = inst
            .members
            .iter()
            .position(|m| m.client.id == client_id && m.gone_at.is_none())
        else {
            return false;
        };
        let m = inst.members.remove(i);
        if inst.presence {
            let text = inst.presence(channel, "leave", &[&m.info], None);
            inst.send(&text, None, limit);
            m.client.push(Out::Text(text), limit);
        }
        if inst.members.is_empty() {
            all.remove(channel);
        }
        true
    }

    /// Sends a message to the other members (and the sender with `echo`).
    pub fn publish(
        &self,
        client: &Client,
        channel: &str,
        event: &str,
        data: &Value,
        limit: usize,
    ) -> Result<(), ChannelError> {
        let mut all = self.lock();
        let inst = all.get_mut(channel).ok_or(ChannelError::NotJoined)?;
        let (max_payload, rate, echo) = (
            inst.options.max_payload,
            f64::from(inst.options.rate_per_s.max(1)),
            inst.options.echo,
        );
        let m = inst
            .members
            .iter_mut()
            .find(|m| m.client.id == client.id && m.gone_at.is_none())
            .ok_or(ChannelError::NotJoined)?;
        if !m.can_publish {
            return Err(ChannelError::Forbidden);
        }
        let now = Instant::now();
        m.tokens = (m.tokens + now.duration_since(m.refilled).as_secs_f64() * rate).min(rate);
        m.refilled = now;
        if m.tokens < 1.0 {
            return Err(ChannelError::RateLimited);
        }
        let data = data.to_string();
        if data.len() > max_payload {
            return Err(ChannelError::TooLarge(max_payload));
        }
        m.tokens -= 1.0;
        let from = json!({ "user_id": m.info.user_id, "conn_id": m.info.conn_id });
        let text: Arc<str> = format!(
            r#"{{"t":"msg","channel":{},"event":{},"from":{from},"data":{data}}}"#,
            Value::from(channel),
            Value::from(event),
        )
        .into();
        inst.send(&text, (!echo).then_some(client.id.as_str()), limit);
        inst.messages += 1;
        self.published.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Replaces the member's presence meta.
    pub fn set_meta(
        &self,
        client_id: &str,
        channel: &str,
        meta: Value,
        limit: usize,
    ) -> Result<(), ChannelError> {
        let mut all = self.lock();
        let inst = all.get_mut(channel).ok_or(ChannelError::NotJoined)?;
        let m = inst
            .members
            .iter_mut()
            .find(|m| m.client.id == client_id && m.gone_at.is_none())
            .ok_or(ChannelError::NotJoined)?;
        m.info.meta = meta;
        let info = m.info.clone();
        if inst.presence {
            let text = inst.presence(channel, "meta", &[&info], None);
            inst.send(&text, None, limit);
        }
        Ok(())
    }

    /// The client's connection ended: its memberships enter the
    /// disconnect window.
    pub fn detach(&self, client_id: &str) {
        let now = Instant::now();
        for inst in self.lock().values_mut() {
            for m in &mut inst.members {
                if m.client.id == client_id && m.gone_at.is_none() {
                    m.gone_at = Some(now);
                }
            }
        }
    }

    /// Removes members whose disconnect window passed and sends `leave`.
    pub fn expire(&self, limit: usize) {
        let mut all = self.lock();
        for (name, inst) in all.iter_mut() {
            let (gone, kept): (Vec<Member>, Vec<Member>) = std::mem::take(&mut inst.members)
                .into_iter()
                .partition(|m| m.gone_at.is_some_and(|t| t.elapsed() >= DEBOUNCE));
            inst.members = kept;
            if inst.presence && !gone.is_empty() {
                let infos: Vec<&MemberInfo> = gone.iter().map(|m| &m.info).collect();
                let text = inst.presence(name, "leave", &infos, None);
                inst.send(&text, None, limit);
            }
        }
        all.retain(|_, i| !i.members.is_empty());
    }

    /// Removes `client_id` from `channel` (rules no longer allow it).
    /// The member itself receives the `leave` too.
    pub fn kick(&self, client_id: &str, channel: &str, limit: usize) {
        let mut all = self.lock();
        let Some(inst) = all.get_mut(channel) else {
            return;
        };
        let Some(i) = inst.members.iter().position(|m| m.client.id == client_id) else {
            return;
        };
        let m = inst.members.remove(i);
        let text = inst.presence(channel, "leave", &[&m.info], Some(("reason", "forbidden")));
        if inst.presence {
            inst.send(&text, None, limit);
        }
        m.client.push(Out::Text(text), limit);
        if inst.members.is_empty() {
            all.remove(channel);
        }
    }

    /// Applies a changed definition: a deleted definition or a new pattern
    /// removes every member; other changes apply to running instances.
    pub fn def_changed(&self, id: &str, def: Option<&ChannelDef>, limit: usize) {
        let affected: Vec<(String, Vec<String>)> = {
            let mut all = self.lock();
            let mut out = Vec::new();
            for (name, inst) in all.iter_mut() {
                if inst.def_id != id {
                    continue;
                }
                match def {
                    Some(d) if d.key_of(name).is_some() => {
                        inst.presence = d.presence;
                        inst.options = d.options.clone();
                    }
                    _ => out.push((
                        name.clone(),
                        inst.members.iter().map(|m| m.client.id.clone()).collect(),
                    )),
                }
            }
            out
        };
        for (name, clients) in affected {
            for c in clients {
                self.kick(&c, &name, limit);
            }
        }
    }

    /// Active members to re-authorize: (channel, def id, client).
    pub fn members(&self) -> Vec<(String, String, Arc<Client>)> {
        self.lock()
            .iter()
            .flat_map(|(name, inst)| {
                inst.members
                    .iter()
                    .filter(|m| m.gone_at.is_none())
                    .map(|m| (name.clone(), inst.def_id.clone(), Arc::clone(&m.client)))
            })
            .collect()
    }

    /// Updates whether a member may publish.
    pub fn set_can_publish(&self, client_id: &str, channel: &str, can: bool) {
        if let Some(inst) = self.lock().get_mut(channel)
            && let Some(m) = inst.members.iter_mut().find(|m| m.client.id == client_id)
        {
            m.can_publish = can;
        }
    }

    /// Channels `client_id` joined.
    pub fn joined(&self, client_id: &str) -> Vec<String> {
        self.lock()
            .iter()
            .filter(|(_, i)| {
                i.members
                    .iter()
                    .any(|m| m.client.id == client_id && m.gone_at.is_none())
            })
            .map(|(n, _)| n.clone())
            .collect()
    }

    /// Every instance, sorted by name.
    pub fn snapshot(&self) -> Vec<InstanceView> {
        let mut v: Vec<InstanceView> = self
            .lock()
            .iter()
            .map(|(name, i)| InstanceView {
                channel: name.clone(),
                def_id: i.def_id.clone(),
                members: i.members.iter().map(|m| m.info.clone()).collect(),
                messages: i.messages,
            })
            .collect();
        v.sort_by(|a, b| a.channel.cmp(&b.channel));
        v
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::realtime::Hub;

    fn def(echo: bool) -> ChannelDef {
        ChannelDef {
            id: "d".into(),
            name: "room".into(),
            pattern: "room:*".into(),
            join_rule: Some(String::new()),
            publish_rule: Some(String::new()),
            presence: true,
            options: ChannelOptions {
                max_members: 2,
                rate_per_s: 2,
                echo,
                max_payload: 32,
            },
            created: String::new(),
            updated: String::new(),
        }
    }

    fn info(c: &Client, user: &str) -> MemberInfo {
        MemberInfo {
            user_id: Some(user.into()),
            conn_id: c.conn_id.clone(),
            collection: Some("users".into()),
            meta: Value::Null,
            joined_at: String::new(),
        }
    }

    fn texts(rx: &mut tokio::sync::mpsc::UnboundedReceiver<Out>) -> Vec<Value> {
        std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|o| match o {
                Out::Text(t) => serde_json::from_str(&t).ok(),
                _ => None,
            })
            .collect()
    }

    fn join(
        ch: &Channels,
        c: &Arc<Client>,
        d: &ChannelDef,
        user: &str,
    ) -> Result<(), ChannelError> {
        ch.join(
            c,
            Join {
                channel: "room:1",
                def: d,
                info: info(c, user),
                subject: Some(user.into()),
                can_publish: true,
            },
            1 << 20,
        )
    }

    #[test]
    fn presence_messages_limits_and_debounce() {
        let hub = Hub::default();
        let ch = Channels::default();
        let d = def(false);
        let (a, mut ra) = hub.connect(None);
        let (b, mut rb) = hub.connect(None);
        join(&ch, &a, &d, "ua").unwrap();
        join(&ch, &b, &d, "ub").unwrap();
        let ta = texts(&mut ra);
        assert_eq!(ta[0]["kind"], "sync");
        assert_eq!(ta[1]["kind"], "join");
        assert_eq!(ta[1]["members"][0]["user_id"], "ub");
        assert_eq!(texts(&mut rb)[0]["members"].as_array().unwrap().len(), 2);
        let (c, _rc) = hub.connect(None);
        assert_eq!(join(&ch, &c, &d, "uc"), Err(ChannelError::Full(2)));

        // Messages: not echoed, rate limited, size limited.
        ch.publish(&a, "room:1", "cursor", &json!({"x": 1}), 1 << 20)
            .unwrap();
        assert!(texts(&mut ra).is_empty());
        let got = texts(&mut rb);
        assert_eq!(got[0]["t"], "msg");
        assert_eq!(got[0]["from"]["user_id"], "ua");
        assert_eq!(got[0]["data"]["x"], 1);
        ch.publish(&a, "room:1", "cursor", &json!(1), 1 << 20)
            .unwrap();
        assert_eq!(
            ch.publish(&a, "room:1", "cursor", &json!(1), 1 << 20),
            Err(ChannelError::RateLimited)
        );
        assert_eq!(
            ch.publish(&b, "room:1", "x", &json!("x".repeat(40)), 1 << 20),
            Err(ChannelError::TooLarge(32))
        );
        assert_eq!(
            ch.publish(&c, "room:1", "x", &json!(1), 1 << 20),
            Err(ChannelError::NotJoined)
        );
        texts(&mut rb);

        // A reconnect within the window replaces the member silently.
        ch.detach(&b.id);
        let (b2, mut rb2) = hub.connect(None);
        join(&ch, &b2, &d, "ub").unwrap();
        let ta = texts(&mut ra);
        assert_eq!(ta.len(), 1);
        assert_eq!(ta[0]["kind"], "join");
        assert_eq!(ta[0]["replaced"], b.conn_id.as_str());
        assert_eq!(texts(&mut rb2)[0]["members"].as_array().unwrap().len(), 2);

        // Leave.
        assert!(ch.leave(&b2.id, "room:1", 1 << 20));
        assert_eq!(texts(&mut ra)[0]["kind"], "leave");
        assert!(!ch.leave(&b2.id, "room:1", 1 << 20));
        ch.detach(&a.id);
        ch.expire(1 << 20);
        assert_eq!(ch.snapshot().len(), 1, "still in the window");
    }
}
