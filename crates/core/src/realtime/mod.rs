//! The realtime hub (SPEC §13.1): connected clients, their subscriptions
//! and outbound queues. Transports (SSE, WebSocket) live in the HTTP
//! adapter; authorization and dispatch are use cases in
//! [`crate::usecases::realtime`].
//!
//! Events come only from the post-commit outbox of the use cases, never
//! from database hooks, so they carry the full before and after records.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use uuid::Uuid;

use crate::domain::Record;
use crate::usecases::Principal;

/// Queued committed changes before dispatch; beyond this, subscribers of the
/// affected collections get `resync` instead (SPEC §13.1).
pub const DISPATCH_CAPACITY: usize = 1024;
/// Events kept per client for SSE resume (`Last-Event-ID`).
pub const REPLAY_EVENTS: usize = 256;
/// How long a disconnected SSE client may resume.
pub const RESUME_WINDOW: Duration = Duration::from_secs(120);

/// What happened to a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Created.
    Create,
    /// Updated.
    Update,
    /// Deleted.
    Delete,
}

impl Action {
    /// Wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Update => "update",
            Self::Delete => "delete",
        }
    }
}

/// A committed record change.
#[derive(Debug, Clone)]
pub struct RecordChange {
    /// Collection id.
    pub collection_id: Uuid,
    /// Kind of change.
    pub action: Action,
    /// The record before (update, delete).
    pub before: Option<Record>,
    /// The record after (create, update).
    pub after: Option<Record>,
}

/// A subscription topic.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Topic {
    /// `records:{collection}`.
    Collection {
        /// Collection id.
        id: Uuid,
        /// The name as the client wrote it (echoed in events).
        name: String,
    },
    /// `records:{collection}/{record_id}`.
    Record {
        /// Collection id.
        id: Uuid,
        /// The name as the client wrote it.
        name: String,
        /// Record id.
        record: String,
    },
}

impl Topic {
    /// The collection this topic watches.
    pub fn collection_id(&self) -> Uuid {
        match self {
            Self::Collection { id, .. } | Self::Record { id, .. } => *id,
        }
    }

    /// The topic string as the client wrote it.
    pub fn as_string(&self) -> String {
        match self {
            Self::Collection { name, .. } => format!("records:{name}"),
            Self::Record { name, record, .. } => format!("records:{name}/{record}"),
        }
    }

    /// Whether a change of `record_id` in `collection` concerns this topic.
    pub fn covers(&self, collection: Uuid, record_id: &str) -> bool {
        match self {
            Self::Collection { id, .. } => *id == collection,
            Self::Record { id, record, .. } => *id == collection && record == record_id,
        }
    }
}

/// One subscription of a client.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Subscription {
    /// Client-chosen id (WebSocket `sub`), for `unsub` and errors.
    pub id: Option<u64>,
    /// What to watch.
    pub topic: Topic,
    /// Rule-language filter.
    pub filter: Option<String>,
    /// `expand` parameter.
    pub expand: Option<String>,
    /// `fields` parameter.
    pub fields: Option<String>,
}

/// The identity a client acts as.
#[derive(Debug, Clone)]
pub struct ClientAuth {
    /// The principal for rule evaluation.
    pub principal: Principal,
    /// Its session, checked for revocation.
    pub session_id: Option<String>,
}

impl ClientAuth {
    /// Anonymous.
    pub fn anonymous() -> Self {
        Self {
            principal: Principal::Anonymous,
            session_id: None,
        }
    }

    /// A key shared by clients that authorize identically.
    pub fn subject_key(&self) -> String {
        match &self.principal {
            Principal::Anonymous => "anon".into(),
            Principal::Superuser { id, .. } => format!("su:{id}"),
            Principal::User {
                id, collection_id, ..
            } => format!("u:{collection_id}:{id}"),
        }
    }
}

/// A serialized event, shared by every client that receives it.
#[derive(Debug)]
pub struct Frame {
    /// Topic string.
    pub topic: String,
    /// WebSocket JSON (`{"t":"record",...}`).
    pub ws: String,
    /// SSE data JSON (`{"type":"record",...}`).
    pub sse: String,
}

/// Something for a transport to send.
#[derive(Debug, Clone)]
pub enum Out {
    /// An event with the client's sequence number.
    Event {
        /// Per-client sequence number (SSE `id`).
        seq: u64,
        /// The event.
        frame: Arc<Frame>,
    },
    /// Events were dropped; the client should refetch this topic.
    Resync {
        /// Topic string.
        topic: String,
    },
    /// Close the connection.
    Close {
        /// `session_revoked`, `slow_consumer`, `shutdown`.
        reason: &'static str,
    },
}

impl Out {
    /// Approximate bytes this item holds in a queue.
    fn weight(&self) -> usize {
        match self {
            Self::Event { frame, .. } => frame.ws.len().max(frame.sse.len()),
            Self::Resync { topic } => topic.len() + 32,
            Self::Close { .. } => 32,
        }
    }
}

#[derive(Debug)]
struct ClientState {
    auth: ClientAuth,
    subs: Vec<Subscription>,
    out: Option<mpsc::UnboundedSender<Out>>,
    ring: VecDeque<(u64, Instant, Out)>,
    next_seq: u64,
    detached_at: Option<Instant>,
    closed: bool,
}

/// A connected (or recently disconnected) client.
#[derive(Debug)]
pub struct Client {
    /// Secret id: binding subscriptions needs it.
    pub id: String,
    /// Client IP, for the per-IP limit.
    pub ip: Option<String>,
    state: Mutex<ClientState>,
    queued: AtomicUsize,
}

/// A client's snapshot for dispatch.
#[derive(Debug, Clone)]
pub struct Interest {
    /// The client.
    pub client: Arc<Client>,
    /// Its identity.
    pub auth: ClientAuth,
    /// Subscriptions on the changed collection.
    pub subs: Vec<Subscription>,
}

impl Client {
    fn lock(&self) -> std::sync::MutexGuard<'_, ClientState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The client's identity.
    pub fn auth(&self) -> ClientAuth {
        self.lock().auth.clone()
    }

    /// Its subscriptions.
    pub fn subscriptions(&self) -> Vec<Subscription> {
        self.lock().subs.clone()
    }

    /// Bytes waiting in the outbound queue.
    pub fn queued_bytes(&self) -> usize {
        self.queued.load(Ordering::Relaxed)
    }

    /// The transport took `out` from the queue.
    pub fn sent(&self, out: &Out) {
        let w = out.weight();
        let _ = self
            .queued
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |q| {
                Some(q.saturating_sub(w))
            });
    }

    /// Queues `out`. Returns false when the outbound queue exceeds `limit`
    /// bytes; the client is then closed with `slow_consumer`.
    pub fn push(&self, out: Out, limit: usize) -> bool {
        let mut st = self.lock();
        if st.closed {
            return false;
        }
        let out = match out {
            Out::Event { frame, .. } => {
                st.next_seq += 1;
                Out::Event {
                    seq: st.next_seq,
                    frame,
                }
            }
            other => other,
        };
        if let Out::Event { seq, .. } = &out {
            st.ring.push_back((*seq, Instant::now(), out.clone()));
            while st.ring.len() > REPLAY_EVENTS {
                st.ring.pop_front();
            }
        }
        let Some(tx) = st.out.clone() else {
            // Detached: events wait in the ring for a resume.
            return true;
        };
        let w = out.weight();
        if self.queued.fetch_add(w, Ordering::Relaxed) + w > limit {
            st.closed = true;
            let _ = tx.send(Out::Close {
                reason: "slow_consumer",
            });
            return false;
        }
        tx.send(out).is_ok()
    }

    /// Closes the client with `reason`.
    pub fn close(&self, reason: &'static str) {
        let mut st = self.lock();
        if st.closed {
            return;
        }
        st.closed = true;
        if let Some(tx) = &st.out {
            let _ = tx.send(Out::Close { reason });
        }
    }

    /// Whether the client was closed.
    pub fn is_closed(&self) -> bool {
        self.lock().closed
    }
}

/// The hub.
#[derive(Debug)]
pub struct Hub {
    clients: RwLock<HashMap<String, Arc<Client>>>,
    queue: mpsc::Sender<RecordChange>,
    receiver: Mutex<Option<mpsc::Receiver<RecordChange>>>,
    overflowed: Mutex<HashSet<Uuid>>,
    /// Events dispatched (tests, metrics).
    pub dispatched: AtomicU64,
}

impl Default for Hub {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel(DISPATCH_CAPACITY);
        Self {
            clients: RwLock::new(HashMap::new()),
            queue: tx,
            receiver: Mutex::new(Some(rx)),
            overflowed: Mutex::new(HashSet::new()),
            dispatched: AtomicU64::new(0),
        }
    }
}

fn new_client_id() -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(crate::auth::token::random_bytes::<18>())
}

impl Hub {
    fn read(&self) -> std::sync::RwLockReadGuard<'_, HashMap<String, Arc<Client>>> {
        self.clients
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, HashMap<String, Arc<Client>>> {
        self.clients
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Queues committed changes for dispatch. A full queue marks the
    /// collections for `resync` instead of blocking the writer.
    pub fn publish(&self, changes: Vec<RecordChange>) {
        for c in changes {
            let id = c.collection_id;
            if self.queue.try_send(c).is_err() {
                self.overflowed
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(id);
            }
        }
    }

    /// The dispatcher's input (taken once).
    pub fn take_receiver(&self) -> Option<mpsc::Receiver<RecordChange>> {
        self.receiver
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    /// Collections whose events were dropped since the last call.
    pub fn take_overflowed(&self) -> Vec<Uuid> {
        self.overflowed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain()
            .collect()
    }

    /// Connected clients (attached) from `ip`.
    pub fn connections_from(&self, ip: &str) -> usize {
        self.read()
            .values()
            .filter(|c| c.ip.as_deref() == Some(ip) && c.lock().out.is_some())
            .count()
    }

    /// Number of known clients, attached or waiting for a resume.
    pub fn len(&self) -> usize {
        self.read().len()
    }

    /// Whether no client is known.
    pub fn is_empty(&self) -> bool {
        self.read().is_empty()
    }

    /// Number of attached clients.
    pub fn connections(&self) -> usize {
        self.read()
            .values()
            .filter(|c| c.lock().out.is_some())
            .count()
    }

    /// Registers a new client.
    pub fn connect(&self, ip: Option<String>) -> (Arc<Client>, mpsc::UnboundedReceiver<Out>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let client = Arc::new(Client {
            id: new_client_id(),
            ip,
            state: Mutex::new(ClientState {
                auth: ClientAuth::anonymous(),
                subs: Vec::new(),
                out: Some(tx),
                ring: VecDeque::new(),
                next_seq: 0,
                detached_at: None,
                closed: false,
            }),
            queued: AtomicUsize::new(0),
        });
        self.write().insert(client.id.clone(), Arc::clone(&client));
        (client, rx)
    }

    /// Looks up a client.
    pub fn get(&self, id: &str) -> Option<Arc<Client>> {
        self.read().get(id).cloned()
    }

    /// Reattaches a detached client and replays events after `last_seq`.
    /// `None` when the client is gone; `Some((client, rx, false))` when the
    /// gap could not be replayed (the caller sends `resync`).
    pub fn resume(
        &self,
        id: &str,
        last_seq: u64,
    ) -> Option<(Arc<Client>, mpsc::UnboundedReceiver<Out>, bool)> {
        let client = self.get(id)?;
        let (tx, rx) = mpsc::unbounded_channel();
        let complete = {
            let mut st = client.lock();
            if st.closed || st.out.is_some() {
                return None;
            }
            // Complete when nothing was missed or the ring still holds
            // every event after `last_seq`.
            let first = st.ring.front().map(|(s, _, _)| *s);
            let complete =
                last_seq >= st.next_seq || first.is_some_and(|f| f <= last_seq.saturating_add(1));
            if complete {
                for (s, _, out) in &st.ring {
                    if *s > last_seq {
                        let _ = tx.send(out.clone());
                    }
                }
            }
            st.out = Some(tx);
            st.detached_at = None;
            complete
        };
        client.queued.store(0, Ordering::Relaxed);
        Some((client, rx, complete))
    }

    /// The transport ended. SSE clients stay for [`RESUME_WINDOW`];
    /// `keep: false` removes the client at once (WebSocket).
    pub fn disconnect(&self, id: &str, keep: bool) {
        let Some(client) = self.get(id) else { return };
        let closed = {
            let mut st = client.lock();
            st.out = None;
            st.detached_at = Some(Instant::now());
            st.closed
        };
        if !keep || closed {
            self.write().remove(id);
        }
    }

    /// Removes detached clients past the resume window.
    pub fn prune(&self) {
        let stale: Vec<String> = self
            .read()
            .values()
            .filter(|c| {
                c.lock()
                    .detached_at
                    .is_some_and(|t| t.elapsed() > RESUME_WINDOW)
            })
            .map(|c| c.id.clone())
            .collect();
        let mut w = self.write();
        for id in stale {
            w.remove(&id);
        }
    }

    /// Sets a client's identity.
    pub fn set_auth(&self, client: &Client, auth: ClientAuth) {
        client.lock().auth = auth;
    }

    /// Replaces a client's subscriptions.
    pub fn set_subscriptions(&self, client: &Client, subs: Vec<Subscription>) {
        client.lock().subs = subs;
    }

    /// Adds or replaces (same id) a subscription.
    pub fn add_subscription(&self, client: &Client, sub: Subscription) -> usize {
        let mut st = client.lock();
        if let Some(id) = sub.id {
            st.subs.retain(|s| s.id != Some(id));
        }
        st.subs.push(sub);
        st.subs.len()
    }

    /// Removes subscriptions by id or topic; returns how many.
    pub fn remove_subscriptions(
        &self,
        client: &Client,
        id: Option<u64>,
        topic: Option<&str>,
    ) -> usize {
        let mut st = client.lock();
        let before = st.subs.len();
        st.subs.retain(|s| {
            let by_id = id.is_some_and(|i| s.id == Some(i));
            let by_topic = topic.is_some_and(|t| s.topic.as_string() == t);
            !(by_id || by_topic)
        });
        before - st.subs.len()
    }

    /// Clients interested in a change of `record_id` in `collection`.
    pub fn interested(&self, collection: Uuid, record_id: &str) -> Vec<Interest> {
        self.read()
            .values()
            .filter_map(|c| {
                let st = c.lock();
                if st.closed {
                    return None;
                }
                let subs: Vec<Subscription> = st
                    .subs
                    .iter()
                    .filter(|s| s.topic.covers(collection, record_id))
                    .cloned()
                    .collect();
                (!subs.is_empty()).then(|| Interest {
                    client: Arc::clone(c),
                    auth: st.auth.clone(),
                    subs,
                })
            })
            .collect()
    }

    /// Clients subscribed to any topic of `collection` (for `resync`).
    pub fn subscribers_of(&self, collection: Uuid) -> Vec<(Arc<Client>, Vec<String>)> {
        self.read()
            .values()
            .filter_map(|c| {
                let st = c.lock();
                let topics: Vec<String> = st
                    .subs
                    .iter()
                    .filter(|s| s.topic.collection_id() == collection)
                    .map(|s| s.topic.as_string())
                    .collect::<HashSet<_>>()
                    .into_iter()
                    .collect();
                (!topics.is_empty()).then(|| (Arc::clone(c), topics))
            })
            .collect()
    }

    /// Session ids of bound clients.
    pub fn sessions(&self) -> Vec<(Arc<Client>, String)> {
        self.read()
            .values()
            .filter_map(|c| {
                let st = c.lock();
                st.auth
                    .session_id
                    .clone()
                    .filter(|_| !st.closed)
                    .map(|s| (Arc::clone(c), s))
            })
            .collect()
    }

    /// Refreshes the auth fields of every client signed in as `user_id`
    /// (they feed `@request.auth.*`).
    pub fn update_user_fields(
        &self,
        user_id: &str,
        new_fields: &serde_json::Map<String, serde_json::Value>,
    ) {
        for c in self.read().values() {
            let mut st = c.lock();
            if let Principal::User { id, fields, .. } = &mut st.auth.principal
                && id == user_id
            {
                *fields = new_fields.clone();
            }
        }
    }

    /// Closes every client (shutdown).
    pub fn close_all(&self, reason: &'static str) {
        for c in self.read().values() {
            c.close(reason);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn frame() -> Arc<Frame> {
        Arc::new(Frame {
            topic: "records:x".into(),
            ws: "{}".into(),
            sse: "{}".into(),
        })
    }

    #[test]
    fn a_full_dispatch_queue_marks_collections_for_resync() {
        let hub = Hub::default();
        let id = Uuid::now_v7();
        let change = || RecordChange {
            collection_id: id,
            action: Action::Delete,
            before: None,
            after: None,
        };
        // Nobody drains the queue here.
        hub.publish((0..DISPATCH_CAPACITY).map(|_| change()).collect());
        assert!(hub.take_overflowed().is_empty());
        hub.publish(vec![change()]);
        assert_eq!(hub.take_overflowed(), vec![id]);
        assert!(hub.take_overflowed().is_empty(), "reported once");
    }

    #[test]
    fn slow_consumers_are_closed() {
        let hub = Hub::default();
        let (c, mut rx) = hub.connect(None);
        assert!(c.push(
            Out::Event {
                seq: 0,
                frame: frame()
            },
            10
        ));
        assert!(!c.push(
            Out::Event {
                seq: 0,
                frame: frame()
            },
            3
        ));
        assert!(matches!(rx.try_recv().unwrap(), Out::Event { seq: 1, .. }));
        assert!(matches!(
            rx.try_recv().unwrap(),
            Out::Close {
                reason: "slow_consumer"
            }
        ));
        assert!(c.is_closed());
    }

    #[test]
    fn detached_clients_resume_with_replay() {
        let hub = Hub::default();
        let (c, mut rx) = hub.connect(None);
        c.push(
            Out::Event {
                seq: 0,
                frame: frame(),
            },
            1 << 20,
        );
        let Out::Event { seq: first, .. } = rx.try_recv().unwrap() else {
            unreachable!()
        };
        hub.disconnect(&c.id, true);
        c.push(
            Out::Event {
                seq: 0,
                frame: frame(),
            },
            1 << 20,
        );
        c.push(
            Out::Event {
                seq: 0,
                frame: frame(),
            },
            1 << 20,
        );
        let (_, mut rx, complete) = hub.resume(&c.id, first).unwrap();
        assert!(complete);
        let seqs: Vec<u64> = std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|o| match o {
                Out::Event { seq, .. } => Some(seq),
                _ => None,
            })
            .collect();
        assert_eq!(seqs, [2, 3]);
        assert!(hub.resume(&c.id, 3).is_none(), "already attached");
        hub.disconnect(&c.id, false);
        assert!(hub.get(&c.id).is_none());
    }
}
