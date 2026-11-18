//! Collaborative documents (SPEC §13.4): `ydoc` fields edited through the
//! y-protocols sync and awareness protocol.
//!
//! One room per open (record, field) holds the document in memory. Updates
//! from writers are applied, relayed to the other peers and appended to
//! `_ydoc_updates` every 250 ms; compaction folds them into the record's
//! BLOB column. Readers (view rule, but not the update rule) receive the
//! document and awareness but their updates are dropped and answered with
//! a custom message (type 100). Rooms unload 60 s after the last peer left.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Map, json};
use time::OffsetDateTime;
use tokio::sync::mpsc::UnboundedReceiver;
use uuid::Uuid;

use yrs::block::ClientID;
use yrs::sync::{Awareness, Message, MessageReader, SyncMessage};
use yrs::updates::decoder::{Decode, DecoderV1};
use yrs::updates::encoder::Encode;
use yrs::{Doc, ReadTxn, StateVector, Transact, Update};

use super::records::viewable;
use super::{App, Principal, RequestCtx};
use crate::auth::token::{hash_token, random_token};
use crate::domain::field::FieldKind;
use crate::domain::{Collection, RuleOp};
use crate::ports::StoreExt as _;
use crate::realtime::{Action, Client, Out, RecordChange};
use crate::rules::RuleState;
use crate::time::format_timestamp;
use crate::{Error, Result};

/// Custom y-protocols message type for errors (JSON `{code, message}`).
pub const MSG_ERROR: u8 = 100;
/// Persistence tick.
pub const FLUSH_EVERY: Duration = Duration::from_millis(250);
/// Compaction threshold (stored updates).
pub const COMPACT_AFTER_UPDATES: usize = 500;
/// Compaction after this long without updates.
pub const COMPACT_IDLE: Duration = Duration::from_secs(30);
/// `updated` touch and record event throttle.
pub const TOUCH_EVERY: Duration = Duration::from_secs(30);
/// Rooms unload this long after the last peer left.
pub const UNLOAD_AFTER: Duration = Duration::from_secs(60);
/// Peers are re-authorized this often.
pub const RECHECK_EVERY: Duration = Duration::from_secs(30);
/// Ticket lifetime.
pub const TICKET_TTL: time::Duration = time::Duration::seconds(30);

/// Identifies a document.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DocKey {
    /// Collection id.
    pub collection_id: Uuid,
    /// Record id.
    pub record_id: String,
    /// `ydoc` field name.
    pub field: String,
}

#[derive(Debug)]
struct Peer {
    id: u64,
    client: Arc<Client>,
    writable: bool,
    awareness_ids: HashSet<ClientID>,
}

struct RoomInner {
    awareness: Awareness,
    peers: Vec<Peer>,
    /// Updates not yet appended.
    pending: Vec<Vec<u8>>,
    /// Updates appended since the last compaction.
    stored: usize,
    /// Id of the last appended update.
    last_id: i64,
    /// Compacted size plus the updates since (estimate).
    size: usize,
    max_size: usize,
    last_update: Instant,
    touch_due: bool,
    last_touch: Option<Instant>,
    empty_since: Option<Instant>,
    /// Removed from the map; nothing is persisted any more.
    closed: bool,
}

/// An open document.
pub struct Room {
    key: DocKey,
    inner: Mutex<RoomInner>,
    /// Serializes persistence (flush, compaction, unload).
    persist: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for Room {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Room")
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

impl Room {
    fn lock(&self) -> std::sync::MutexGuard<'_, RoomInner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[derive(Debug)]
struct Ticket {
    principal: Principal,
    key: DocKey,
    expires: OffsetDateTime,
}

/// Open rooms and connection tickets.
#[derive(Debug, Default)]
pub struct CollabState {
    rooms: Mutex<HashMap<DocKey, Arc<Room>>>,
    tickets: Mutex<HashMap<String, Ticket>>,
    next_peer: AtomicU64,
    /// Updates applied (metrics, tests).
    pub applied: AtomicU64,
}

impl CollabState {
    fn rooms(&self) -> std::sync::MutexGuard<'_, HashMap<DocKey, Arc<Room>>> {
        self.rooms
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn all(&self) -> Vec<Arc<Room>> {
        self.rooms().values().cloned().collect()
    }

    /// Number of open documents.
    pub fn open_documents(&self) -> usize {
        self.rooms().len()
    }
}

/// One collab connection.
#[derive(Debug)]
pub struct CollabConn {
    /// Realtime client (outbound queue, revocation).
    pub client: Arc<Client>,
    room: Arc<Room>,
    peer: u64,
    /// Whether updates are accepted.
    pub writable: bool,
}

pub fn error_message(code: &str, message: &str) -> Vec<u8> {
    Message::Custom(
        MSG_ERROR,
        json!({ "code": code, "message": message })
            .to_string()
            .into_bytes(),
    )
    .encode_v1()
}

fn bad(msg: &str) -> Error {
    Error::BadRequest(msg.into())
}

/// The collection, the field's size limit and the record id of a document.
fn resolve(
    compiled: &super::CompiledSchema,
    collection: &str,
    record_id: &str,
    field: &str,
) -> Result<(Arc<Collection>, DocKey, usize)> {
    let c = compiled
        .schema
        .by_id_or_name(collection)
        .ok_or(Error::NotFound)?
        .clone();
    let f = c.field(field).ok_or(Error::NotFound)?;
    let FieldKind::Ydoc { max_size } = f.kind else {
        return Err(bad(&format!(
            "'{field}' is not a collaborative document field."
        )));
    };
    let record_id = Uuid::try_parse(record_id)
        .map_err(|_| Error::NotFound)?
        .to_string();
    let key = DocKey {
        collection_id: c.id,
        record_id,
        field: field.to_owned(),
    };
    Ok((c, key, usize::try_from(max_size).unwrap_or(usize::MAX)))
}

/// A document built from the stored state and updates.
fn build_doc(state: Option<&[u8]>, updates: &[(i64, Vec<u8>)]) -> Result<Doc> {
    let doc = Doc::new();
    {
        let mut txn = doc.transact_mut();
        for bytes in state
            .into_iter()
            .chain(updates.iter().map(|(_, u)| u.as_slice()))
        {
            if bytes.is_empty() {
                continue;
            }
            let u = Update::decode_v1(bytes)
                .map_err(|e| Error::internal(format!("stored document update is invalid: {e}")))?;
            txn.apply_update(u)
                .map_err(|e| Error::internal(format!("stored document update failed: {e}")))?;
        }
    }
    Ok(doc)
}

fn encode_state(doc: &Doc) -> Vec<u8> {
    doc.transact()
        .encode_state_as_update_v1(&StateVector::default())
}

impl App {
    /// `(viewable, writable)` of a document's record for `principal`.
    async fn collab_access(
        &self,
        principal: &Principal,
        c: &Arc<Collection>,
        record_id: &str,
    ) -> Result<(bool, bool)> {
        let superuser = matches!(principal, Principal::Superuser { .. });
        let compiled = self.schema.get();
        let mut req = RequestCtx::system(principal.clone(), self.clock.now()).request_values();
        req.context = "realtime".into();
        let (c, id) = (Arc::clone(c), record_id.to_owned());
        self.store
            .read(move |tx| {
                if tx.record_get(&c, &id)?.is_none() {
                    return Ok((false, false));
                }
                if !viewable(tx, &compiled, &c, &id, &req, superuser)? {
                    return Ok((false, false));
                }
                let writable = superuser
                    || match compiled.rule(&c, RuleOp::Update) {
                        RuleState::Locked => false,
                        RuleState::Public => true,
                        RuleState::Expr(r) => tx.record_matches(&c, &id, r, &req)?,
                    };
                Ok((true, writable))
            })
            .await
    }

    /// `POST /api/collab/{c}/{id}/{field}/ticket`: a single-use ticket for
    /// the WebSocket URL (browsers cannot set headers on WebSockets), valid
    /// for 30 s.
    pub async fn collab_ticket(
        &self,
        ctx: &RequestCtx,
        collection: &str,
        record_id: &str,
        field: &str,
    ) -> Result<serde_json::Value> {
        let compiled = self.schema.get();
        let (c, key, _) = resolve(&compiled, collection, record_id, field)?;
        if !self
            .collab_access(&ctx.principal, &c, &key.record_id)
            .await?
            .0
        {
            return Err(Error::NotFound);
        }
        let ticket = random_token();
        let mut tickets = self
            .collab
            .tickets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        tickets.retain(|_, t| t.expires > ctx.now);
        tickets.insert(
            hash_token(&ticket),
            Ticket {
                principal: ctx.principal.clone(),
                key,
                expires: ctx.now + TICKET_TTL,
            },
        );
        Ok(json!({ "ticket": ticket, "expires_in": TICKET_TTL.whole_seconds() }))
    }

    /// Redeems a ticket for the document it was issued for.
    pub fn collab_redeem(
        &self,
        ticket: &str,
        collection: &str,
        record_id: &str,
        field: &str,
    ) -> Result<Principal> {
        let compiled = self.schema.get();
        let (_, key, _) = resolve(&compiled, collection, record_id, field)?;
        let t = self
            .collab
            .tickets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&hash_token(ticket))
            .ok_or(Error::Unauthorized)?;
        if t.expires <= self.clock.now() || t.key != key {
            return Err(Error::Unauthorized);
        }
        Ok(t.principal)
    }

    async fn collab_room(
        &self,
        c: &Arc<Collection>,
        key: &DocKey,
        max_size: usize,
    ) -> Result<Arc<Room>> {
        if let Some(r) = self.collab.rooms().get(key) {
            return Ok(Arc::clone(r));
        }
        let (c2, id, field) = (Arc::clone(c), key.record_id.clone(), key.field.clone());
        let stored = self
            .store
            .read(move |tx| tx.ydoc_load(&c2, &id, &field))
            .await?
            .ok_or(Error::NotFound)?;
        let doc = build_doc(stored.state.as_deref(), &stored.updates)?;
        let size = stored.state.as_ref().map_or(0, Vec::len)
            + stored.updates.iter().map(|(_, u)| u.len()).sum::<usize>();
        let room = Arc::new(Room {
            key: key.clone(),
            inner: Mutex::new(RoomInner {
                awareness: Awareness::new(doc),
                peers: Vec::new(),
                pending: Vec::new(),
                stored: stored.updates.len(),
                last_id: stored.updates.last().map_or(0, |(id, _)| *id),
                size,
                max_size,
                last_update: Instant::now(),
                touch_due: false,
                last_touch: None,
                empty_since: Some(Instant::now()),
                closed: false,
            }),
            persist: tokio::sync::Mutex::new(()),
        });
        // Another connection may have loaded it meanwhile.
        Ok(Arc::clone(
            self.collab.rooms().entry(key.clone()).or_insert(room),
        ))
    }

    /// Opens a connection to a document: requires the view rule; the
    /// update rule decides whether it is writable. Returns the first
    /// messages to send (sync step 1 and the awareness states).
    pub async fn collab_open(
        &self,
        ctx: &RequestCtx,
        collection: &str,
        record_id: &str,
        field: &str,
    ) -> Result<(CollabConn, UnboundedReceiver<Out>, Vec<Vec<u8>>)> {
        let compiled = self.schema.get();
        let (c, key, max_size) = resolve(&compiled, collection, record_id, field)?;
        let (view, writable) = self
            .collab_access(&ctx.principal, &c, &key.record_id)
            .await?;
        if !view {
            return Err(Error::NotFound);
        }
        let room = self.collab_room(&c, &key, max_size).await?;
        let (client, rx) = self.realtime_connect(ctx.ip.clone())?;
        if let Err(e) = self.realtime_auth(&client, ctx) {
            self.hub.disconnect(&client.id, false);
            return Err(e);
        }
        let peer = self.collab.next_peer.fetch_add(1, Ordering::Relaxed);
        let first = {
            let mut r = room.lock();
            if r.closed {
                drop(r);
                self.hub.disconnect(&client.id, false);
                return Err(Error::Conflict(
                    "The document was just closed; reconnect.".into(),
                ));
            }
            r.peers.push(Peer {
                id: peer,
                client: Arc::clone(&client),
                writable,
                awareness_ids: HashSet::new(),
            });
            r.empty_since = None;
            let sv = r.awareness.doc().transact().state_vector();
            let mut first = vec![Message::Sync(SyncMessage::SyncStep1(sv)).encode_v1()];
            if let Ok(u) = r.awareness.update()
                && !u.clients.is_empty()
            {
                first.push(Message::Awareness(u).encode_v1());
            }
            first
        };
        Ok((
            CollabConn {
                client,
                room,
                peer,
                writable,
            },
            rx,
            first,
        ))
    }

    /// Handles one binary frame; returns the replies to the sender.
    pub fn collab_message(&self, conn: &CollabConn, data: &[u8]) -> Result<Vec<Vec<u8>>> {
        let limit = self.realtime_limits().outbound_bytes;
        let mut replies = Vec::new();
        let mut decoder = DecoderV1::from(data);
        let mut r = conn.room.lock();
        let writable = r
            .peers
            .iter()
            .find(|p| p.id == conn.peer)
            .is_some_and(|p| p.writable);
        for msg in MessageReader::new(&mut decoder) {
            let msg = msg.map_err(|e| bad(&format!("Invalid y-protocols message: {e}")))?;
            match msg {
                Message::Sync(SyncMessage::SyncStep1(sv)) => {
                    let diff = r.awareness.doc().transact().encode_diff_v1(&sv);
                    replies.push(Message::Sync(SyncMessage::SyncStep2(diff)).encode_v1());
                }
                Message::Sync(SyncMessage::SyncStep2(bytes) | SyncMessage::Update(bytes)) => {
                    let update = Update::decode_v1(&bytes)
                        .map_err(|e| bad(&format!("Invalid document update: {e}")))?;
                    if update.is_empty() {
                        continue;
                    }
                    if !writable {
                        // Sync step 2 always carries the delete set: only new
                        // content is worth telling the client about.
                        let sv = r.awareness.doc().transact().state_vector();
                        if update.extends(&sv) {
                            replies.push(error_message(
                                "read_only",
                                "You can view this document but not edit it; the update was dropped.",
                            ));
                        }
                        continue;
                    }
                    if r.size + bytes.len() > r.max_size {
                        replies.push(error_message(
                            "too_large",
                            &format!("The document reached its size limit of {} bytes; the update was dropped.", r.max_size),
                        ));
                        continue;
                    }
                    r.awareness
                        .doc()
                        .transact_mut()
                        .apply_update(update)
                        .map_err(|e| bad(&format!("The update could not be applied: {e}")))?;
                    r.size += bytes.len();
                    r.last_update = Instant::now();
                    r.touch_due = true;
                    let relay: Arc<[u8]> = Message::Sync(SyncMessage::Update(bytes.clone()))
                        .encode_v1()
                        .into();
                    r.pending.push(bytes);
                    for p in r.peers.iter().filter(|p| p.id != conn.peer) {
                        p.client.push(Out::Binary(Arc::clone(&relay)), limit);
                    }
                    self.collab.applied.fetch_add(1, Ordering::Relaxed);
                }
                Message::Awareness(update) => {
                    let relay: Arc<[u8]> = Message::Awareness(update.clone()).encode_v1().into();
                    let ids: Vec<ClientID> = update.clients.keys().copied().collect();
                    if let Ok(Some(summary)) = r.awareness.apply_update_summary(update)
                        && let Some(p) = r.peers.iter_mut().find(|p| p.id == conn.peer)
                    {
                        for id in &summary.removed {
                            p.awareness_ids.remove(id);
                        }
                        p.awareness_ids
                            .extend(ids.into_iter().filter(|id| !summary.removed.contains(id)));
                    }
                    for p in r.peers.iter().filter(|p| p.id != conn.peer) {
                        p.client.push(Out::Binary(Arc::clone(&relay)), limit);
                    }
                }
                Message::AwarenessQuery => {
                    if let Ok(u) = r.awareness.update() {
                        replies.push(Message::Awareness(u).encode_v1());
                    }
                }
                Message::Auth(_) | Message::Custom(..) => {}
            }
        }
        Ok(replies)
    }

    /// The connection ended: its awareness states are removed for the
    /// other peers.
    pub fn collab_close(&self, conn: &CollabConn) {
        let limit = self.realtime_limits().outbound_bytes;
        let mut r = conn.room.lock();
        let Some(i) = r.peers.iter().position(|p| p.id == conn.peer) else {
            return;
        };
        let peer = r.peers.remove(i);
        if !peer.awareness_ids.is_empty() {
            for id in &peer.awareness_ids {
                r.awareness.remove_state(*id);
            }
            if let Ok(u) = r
                .awareness
                .update_with_clients(peer.awareness_ids.iter().copied())
            {
                let relay: Arc<[u8]> = Message::Awareness(u).encode_v1().into();
                for p in &r.peers {
                    p.client.push(Out::Binary(Arc::clone(&relay)), limit);
                }
            }
        }
        if r.peers.is_empty() {
            r.empty_since = Some(Instant::now());
        }
    }

    /// Starts persistence, unloading and re-authorization (called once by
    /// `App::load`).
    pub(crate) fn collab_start(self: &Arc<Self>) {
        let me = Arc::downgrade(self);
        let sleep = Arc::clone(&self.spawner);
        self.spawner.spawn(
            "collab.persist",
            Box::pin(async move {
                let mut last_check = Instant::now();
                loop {
                    sleep.sleep(FLUSH_EVERY).await;
                    let Some(app) = me.upgrade() else { return };
                    app.collab_tick(false).await;
                    if last_check.elapsed() >= RECHECK_EVERY {
                        last_check = Instant::now();
                        app.collab_recheck().await;
                    }
                }
            }),
        );
    }

    /// Flushes, compacts, touches and unloads; `shutdown` persists and
    /// compacts every room.
    pub async fn collab_tick(&self, shutdown: bool) {
        for room in self.collab.all() {
            if let Err(e) = self.collab_persist(&room, shutdown).await {
                tracing::warn!(error = %e, collection = %room.key.collection_id, record = %room.key.record_id,
                    field = %room.key.field, "collaborative document persistence failed");
            }
        }
    }

    /// Persists and compacts every room (graceful shutdown).
    pub async fn collab_shutdown(&self) {
        self.collab_tick(true).await;
    }

    async fn collab_persist(&self, room: &Arc<Room>, shutdown: bool) -> Result<()> {
        let _guard = room.persist.lock().await;
        let compiled = self.schema.get();
        let c = compiled
            .schema
            .by_id(room.key.collection_id)
            .filter(|c| {
                c.field(&room.key.field)
                    .is_some_and(|f| matches!(f.kind, FieldKind::Ydoc { .. }))
            })
            .cloned();
        let Some(c) = c else {
            // The collection or field is gone.
            self.collab_drop(&room.key, "deleted");
            return Ok(());
        };
        let (pending, closed) = {
            let mut r = room.lock();
            (std::mem::take(&mut r.pending), r.closed)
        };
        if closed {
            return Ok(());
        }
        if !pending.is_empty() {
            let (cid, rid, field, now) = (
                c.id.to_string(),
                room.key.record_id.clone(),
                room.key.field.clone(),
                format_timestamp(self.clock.now()),
            );
            let n = pending.len();
            let batch = pending.clone();
            match self
                .store
                .write(move |tx| tx.ydoc_append(&cid, &rid, &field, &batch, &now))
                .await
            {
                Ok(last) => {
                    let mut r = room.lock();
                    r.stored += n;
                    r.last_id = last;
                }
                Err(e) => {
                    let mut r = room.lock();
                    let newer = std::mem::take(&mut r.pending);
                    r.pending = pending;
                    r.pending.extend(newer);
                    return Err(e);
                }
            }
        }
        let (compact, unload, touch) = {
            let r = room.lock();
            let idle = r.last_update.elapsed() >= COMPACT_IDLE;
            let unload = r.peers.is_empty()
                && (shutdown || r.empty_since.is_some_and(|t| t.elapsed() >= UNLOAD_AFTER))
                && r.pending.is_empty();
            let compact =
                r.stored > 0 && (shutdown || unload || idle || r.stored > COMPACT_AFTER_UPDATES);
            let touch = r.touch_due
                && (shutdown || r.last_touch.is_none_or(|t| t.elapsed() >= TOUCH_EVERY));
            (compact, unload, touch)
        };
        if compact {
            let (state, up_to) = {
                let r = room.lock();
                (encode_state(r.awareness.doc()), r.last_id)
            };
            let len = state.len();
            let (c2, rid, field) = (
                Arc::clone(&c),
                room.key.record_id.clone(),
                room.key.field.clone(),
            );
            self.store
                .write(move |tx| tx.ydoc_compact(&c2, &rid, &field, &state, up_to))
                .await?;
            // Appends happen only under the persistence lock: every stored
            // update is folded in.
            let mut r = room.lock();
            r.stored = 0;
            r.size = len + r.pending.iter().map(Vec::len).sum::<usize>();
        }
        if touch {
            self.collab_touch(&c, &room.key.record_id).await?;
            let mut r = room.lock();
            r.touch_due = false;
            r.last_touch = Some(Instant::now());
        }
        if unload {
            let mut rooms = self.collab.rooms();
            let mut r = room.lock();
            if r.peers.is_empty() && r.pending.is_empty() {
                r.closed = true;
                rooms.remove(&room.key);
            }
        }
        Ok(())
    }

    /// Bumps `updated` and publishes a record update event.
    async fn collab_touch(&self, c: &Arc<Collection>, record_id: &str) -> Result<()> {
        let (c2, id, now) = (Arc::clone(c), record_id.to_owned(), self.clock.now());
        let change = self
            .store
            .write(move |tx| {
                let Some(before) = tx.record_get(&c2, &id)? else {
                    return Ok(None);
                };
                let ts = format_timestamp(now);
                tx.record_update(&c2, &id, &ts, &Map::new())?;
                let mut after = before.clone();
                after.updated = ts;
                Ok(Some(RecordChange {
                    collection_id: c2.id,
                    action: Action::Update,
                    before: Some(before),
                    after: Some(after),
                }))
            })
            .await?;
        if let Some(ch) = change {
            self.hub.publish(vec![ch]);
        }
        Ok(())
    }

    /// Closes a room without persisting (record or field deleted, state
    /// replaced).
    fn collab_drop(&self, key: &DocKey, reason: &'static str) {
        let room = self.collab.rooms().remove(key);
        if let Some(room) = room {
            let mut r = room.lock();
            r.closed = true;
            r.pending.clear();
            for p in &r.peers {
                p.client.close(reason);
            }
        }
    }

    /// A record was deleted: its open documents close.
    pub(crate) fn collab_record_deleted(&self, collection_id: Uuid, record_id: &str) {
        let keys: Vec<DocKey> = self
            .collab
            .rooms()
            .keys()
            .filter(|k| k.collection_id == collection_id && k.record_id == record_id)
            .cloned()
            .collect();
        for k in keys {
            self.collab_drop(&k, "deleted");
        }
    }

    /// Re-authorizes every peer: no view access closes the connection, no
    /// update access makes it read-only.
    async fn collab_recheck(&self) {
        let compiled = self.schema.get();
        for room in self.collab.all() {
            let Some(c) = compiled.schema.by_id(room.key.collection_id).cloned() else {
                continue;
            };
            let peers: Vec<(u64, Arc<Client>)> = room
                .lock()
                .peers
                .iter()
                .map(|p| (p.id, Arc::clone(&p.client)))
                .collect();
            for (id, client) in peers {
                match self
                    .collab_access(&client.auth().principal, &c, &room.key.record_id)
                    .await
                {
                    Ok((false, _)) => client.close("forbidden"),
                    Ok((true, w)) => {
                        if let Some(p) = room.lock().peers.iter_mut().find(|p| p.id == id) {
                            p.writable = w;
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "collab re-check failed"),
                }
            }
        }
    }

    /// `GET /api/collab/{c}/{id}/{field}/state`: the document as one Yjs
    /// update (v1).
    pub async fn collab_state(
        &self,
        ctx: &RequestCtx,
        collection: &str,
        record_id: &str,
        field: &str,
    ) -> Result<Vec<u8>> {
        let compiled = self.schema.get();
        let (c, key, _) = resolve(&compiled, collection, record_id, field)?;
        if !self
            .collab_access(&ctx.principal, &c, &key.record_id)
            .await?
            .0
        {
            return Err(Error::NotFound);
        }
        if let Some(room) = self.collab.rooms().get(&key).cloned() {
            return Ok(encode_state(room.lock().awareness.doc()));
        }
        let (rid, f) = (key.record_id.clone(), key.field.clone());
        let stored = self
            .store
            .read(move |tx| tx.ydoc_load(&c, &rid, &f))
            .await?
            .ok_or(Error::NotFound)?;
        Ok(encode_state(&build_doc(
            stored.state.as_deref(),
            &stored.updates,
        )?))
    }

    /// `PUT /api/collab/{c}/{id}/{field}/state` (update rule): replaces the
    /// document. Connected editors are disconnected (`replaced`).
    pub async fn collab_replace(
        &self,
        ctx: &RequestCtx,
        collection: &str,
        record_id: &str,
        field: &str,
        body: &[u8],
    ) -> Result<()> {
        let compiled = self.schema.get();
        let (c, key, max_size) = resolve(&compiled, collection, record_id, field)?;
        let (view, writable) = self
            .collab_access(&ctx.principal, &c, &key.record_id)
            .await?;
        if !view {
            return Err(Error::NotFound);
        }
        if !writable {
            return Err(Error::Forbidden("You cannot edit this document.".into()));
        }
        if body.len() > max_size {
            return Err(Error::PayloadTooLarge {
                max_bytes: max_size as u64,
            });
        }
        // Normalize through a fresh document: the body must be a valid update.
        let doc = build_doc(Some(body), &[])
            .map_err(|_| bad("The body is not a Yjs document update (v1)."))?;
        let state = encode_state(&doc);
        // Hold the persistence lock of an open room so no flush interleaves.
        let open = self.collab.rooms().get(&key).cloned();
        let _guard = match &open {
            Some(r) => Some(r.persist.lock().await),
            None => None,
        };
        self.collab_drop(&key, "replaced");
        let (c2, rid, f) = (Arc::clone(&c), key.record_id.clone(), key.field.clone());
        self.store
            .write(move |tx| tx.ydoc_compact(&c2, &rid, &f, &state, i64::MAX))
            .await?;
        self.collab_touch(&c, &key.record_id).await
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use yrs::{GetString, Text};

    #[test]
    fn documents_rebuild_from_state_and_updates() {
        let a = Doc::new();
        let text = a.get_or_insert_text("t");
        text.insert(&mut a.transact_mut(), 0, "hello");
        let state = encode_state(&a);
        let sv = a.transact().state_vector();
        text.insert(&mut a.transact_mut(), 5, " world");
        let update = a.transact().encode_diff_v1(&sv);
        let b = build_doc(Some(&state), &[(1, update)]).unwrap();
        let t = b.get_or_insert_text("t");
        assert_eq!(t.get_string(&b.transact()), "hello world");
        assert!(build_doc(Some(b"nope"), &[]).is_err());
    }

    #[test]
    fn error_messages_use_the_custom_type() {
        let m = error_message("read_only", "x");
        assert_eq!(m[0], MSG_ERROR);
        let mut d = DecoderV1::from(m.as_slice());
        let Some(Ok(Message::Custom(MSG_ERROR, body))) = MessageReader::new(&mut d).next() else {
            panic!("not a custom message")
        };
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["code"], "read_only");
    }
}
