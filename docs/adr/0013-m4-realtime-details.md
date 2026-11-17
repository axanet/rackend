# 0013. M4 realtime details not fixed by the spec

- Status: accepted
- Date: 2026-11-17
- Spec: §13, §13.1, §13.2, §20 M4

## Context

M4 adds record subscriptions over Server-Sent Events and WebSocket. The spec fixes what a subscriber may see and the wire formats; this record covers how the hub is built and where the implementation fills gaps.

## Decision

### Events and dispatch

- Use cases collect record changes inside the write transaction (creates, updates, deletes, and the updates and deletes of relation cascades) and hand them to the hub only after commit. Rolled-back writes emit nothing.
- One dispatcher task consumes a bounded queue of 1,024 committed changes in commit order. When the queue is full, the change is dropped and every subscriber of that collection gets `resync`. This replaces the per-collection `broadcast` channels of SPEC §13.1 with the same behavior: nobody silently misses events, and the writer never blocks on realtime.
- For each change, subscribers are grouped by identical authorization (subject, topic, filter, expand, fields). Each group is evaluated once, in one read transaction per change, and the serialized event is shared by every connection in the group. 10,000 anonymous subscribers of one topic cost one evaluation.
- Rules are checked against the record values of the event, not the table: the view rule and the filter run in SQLite against a one-row `SELECT` with the record's columns, so deletes are judged by the record as it was, and every event is judged by its own snapshot. The rule equivalence property test checks this path against the evaluator and the table-based SQL (12,000+ cases).
- Updates: visible after → `update`; visible before only → `delete` with `reason: "no_longer_visible"`; otherwise nothing. Records that become visible arrive as `update`.
- Expansion in events applies the related collections' view rules (the same code as the REST API). Deletes are sent without expansion.
- When an auth record changes, the `@request.auth.*` fields of its open connections are refreshed.

### Connections

- Every outbound queue is byte-counted. Above `realtime.max_outbound_queue_kb` the connection is closed with `slow_consumer`.
- Sessions of connected clients are checked every 2 seconds in one query; revoked or expired sessions close their connections with `session_revoked`, within the 5 seconds of SPEC §13.2. This covers every revocation path (logout, refresh reuse, password change, user deletion) without each one notifying the hub.
- A connection bound to one account cannot be rebound to another (`403`). An anonymous request does not unbind it.
- `max_connections_per_ip` exceeded → `429 rate_limited`.
- WebSocket buffers are 4 KB each (the defaults of 128 KB would cost about 2.5 GB for 10,000 connections). Inbound messages are limited to `realtime.max_message_kb`.

### Server-Sent Events

- Event ids are `<client_id>.<seq>` rather than a bare sequence number. The browser sends the last id on reconnect, and the client id in it is what lets the server find the stream to resume. Resume works for two minutes after a disconnect, from a ring of the last 256 events; otherwise the client gets `resync` for each of its topics.
- The client id is 18 random bytes. Binding needs it plus credentials, and binding uses `POST`, so the CSRF origin check protects cookie sessions.
- Event names are the topic; `connect`, `resync` and `close` are the other names. Data is `{"type":"record",...}`.

### WebSocket

- The server sends `{"t":"connect","client_id":...}` first. `auth` is accepted at any time; without it the connection stays anonymous (public collections work without signing in).
- A cookie session on a WebSocket upgrade needs an allowed `Origin`, like unsafe requests (cross-site WebSocket hijacking). Bearer tokens and the `auth` message are not affected.
- `sub` replies with `ack` or `err` (`code` is a problem code, plus `errors` for validation). Channel messages (`join`, `leave`, `pub`, `meta`) answer `err` until M9.
- Close frames use code 4000 with the reason (`session_revoked`, `slow_consumer`, `shutdown`, `timeout`).

### Ports

- `Spawner` gains `sleep`, because the core may only use tokio's sync primitives and the sweeper needs a timer.
- New store methods: `record_values_match` and `sessions_live`.
- New core error `RateLimited`, mapped to the existing `rate_limited` problem.

### Measurement

- 10,000 idle subscribed WebSocket connections: about 9 KB each; 147 MB RSS on an Apple M6 (see `docs/performance.md`). The Linux reference hardware of SPEC §2 was not available; numbers from it should be added.

## Consequences

- One dispatcher orders all events but is also the throughput ceiling for realtime; it does the authorization reads on the reader pool while the writer keeps going.
- Clients must treat `resync` as "refetch this topic".
