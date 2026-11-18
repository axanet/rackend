# 0018. M9 channels, presence and collaborative editing details not fixed by the spec

- Status: accepted
- Date: 2026-11-18
- Spec: §13.3, §13.4, §20 M9

## Context

M9 adds channels with presence to the WebSocket transport and collaborative documents (`ydoc` fields) edited over the y-protocols. The acceptance criteria: two browser clients with Tiptap and `y-websocket` co-edit a document, the server restarts mid-session and no edits are lost; a read-only user's updates are rejected and never persisted. This record covers what the spec leaves open.

## Decision

### Channels

- Definitions live in `_channels` (migration 0007) and are cached in memory; `/api/channels` (superusers) creates, edits and deletes them, `/api/channels/live` lists running instances with their members. Saving compiles both rules with `@channel.key` (`/api/rules/check` accepts `operation: "channel"` for the admin UI's live check); errors name the rule (`join_rule: …`).
- Rules are evaluated in the store with `SELECT EXISTS(SELECT 1 FROM (SELECT 1) t WHERE …)`, the same SQL compiler as record rules, so `@collection.*` lookups work. `@request.context` is `realtime`. They run on join and every 30 seconds for every member: failing the join rule removes the member (`leave` with `reason: "forbidden"`, sent to the member too), failing the publish rule only stops publishing. Publishing itself checks a cached flag, so a message costs no database read.
- Instances exist while they have members, in a map inside the realtime hub. Members are identified to others by a public `conn_id`; the `client_id` stays secret because it binds SSE subscriptions.
- The 3-second disconnect debounce keeps the member listed. When the same account (same subject) joins again within the window, the new member replaces the old one and the others get one `join` with `replaced: <old conn_id>` instead of `leave` plus `join`. Expiry runs on a 1-second tick, so a `leave` arrives 3 to 4 seconds after a drop.
- Rate limiting is a token bucket per member (capacity and refill `rate_per_s`). `pub` is acknowledged only when it carries an `id`, so high-rate senders (cursors) are not flooded with acks; errors are always sent.
- Deleting a definition or changing its pattern removes the members of its instances; other changes (options, presence) apply to running instances.
- Channel messages travel through the client's outbound queue (`Out::Text`), so the slow-consumer limit applies; SSE clients never see them.

### Collaborative documents

- `yrs` 0.28 (feature `sync`) is added to `core`'s dependency allowlist: the protocol and the document model are domain logic, and `yrs` does no I/O.
- `FieldKind::Ydoc { max_size }` is a `BLOB` column. It is left out of record JSON (the select emits `NULL`), of OpenAPI, of rules (compile error) and of REST writes (`read_only` validation error).
- One room per open (record, field) holds a `yrs::Awareness` (document plus awareness states) behind a mutex, its peers and its pending updates. A background task every 250 ms appends pending updates to `_ydoc_updates` in one write, compacts (encode the state, write the column, delete updates up to the last appended id) after 500 stored updates, after 30 seconds idle, on unload and on shutdown, and unloads rooms 60 seconds after the last peer left. Persistence of a room is serialized by an async mutex.
- A SIGKILL loses at most the updates of the last 250 ms on the server. Clients still hold them and resend them with their next sync step 2, so no edit is lost; the acceptance test kills the server with SIGKILL while an editor keeps typing.
- Read-only connections never apply updates. They get custom message type 100 (`{"code": "read_only", "message": …}` as UTF-8 JSON in a var-length byte array) only when an update would add content (`Update::extends` the server's state vector): sync step 2 always carries the full delete set, so warning on every non-empty update would fire on every connect.
- The size check uses an estimate: the last compacted size plus the bytes of later updates. Compaction resets it to the real size.
- `updated` is touched (and a record `update` event published, without audit entry) on the first change and then at most every 30 seconds.
- Authentication: header or cookie as on `/api/ws`, or a single-use ticket (`POST …/ticket`, 30 seconds, bound to the document and the caller) in the `ticket` query parameter. The spec allowed a first auth message, but y-websocket sends sync step 1 as soon as the socket opens and has no hook for an earlier message; tickets keep tokens out of URLs and work with any y-websocket client. Tickets are in memory and do not survive a restart, so clients fetch a new one before every reconnect (the example does this on `status: disconnected`).
- Each collab connection is also a realtime hub client, so the per-IP connection limit, session revocation (closed within 5 seconds) and the outbound queue limit apply. Rules are re-checked every 30 seconds per peer.
- `PUT …/state` validates the body by applying it to a fresh document, writes it as the compacted state with every stored update deleted, and closes the room's connections with `replaced`. Deleting a record deletes its stored updates in the same transaction and closes its rooms with `deleted`.
- `y-websocket`'s BroadcastChannel syncs tabs of one browser directly. It would carry a read-only tab's edits into a writer tab of the same browser and so past the read-only check. The example disables it (`disableBc: true`) and the protocol documentation tells clients to.

### Admin UI

- Channels page: definitions table, add/edit dialog with live rule checks, and a live view that polls `/api/channels/live` every 2 seconds while the page is visible.
- The collection editor offers `ydoc` with a size limit; records show a note instead of an input, and the records table leaves `ydoc` columns out.

## Consequences

- Channel state and open documents are in memory and lost on restart; clients reconnect, rejoin and resync. This matches the single-instance design (no external broker).
- `PUT …/state` cannot force connected clients to drop their content: Yjs merges. The documentation says to start from a new `Y.Doc` after a replace.
- Deleting a collection or a `ydoc` field is a soft delete (trash), so their stored updates stay until the trash entry is purged, and a restore brings the documents back complete.
