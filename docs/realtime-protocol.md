# Realtime protocol

rackend pushes record changes to clients over server-sent events (SSE) or a WebSocket. The SSE handshake is in the OpenAPI document; this page describes everything OpenAPI cannot: the event stream, the WebSocket messages, and the channel and collaboration protocols that come with M9.

## Topics and events

A subscription names a topic and optional options:

| Topic | Receives |
|---|---|
| `records:{collection}` | Changes to any record of the collection |
| `records:{collection}/{record_id}` | Changes to one record |

| Option | Meaning |
|---|---|
| `filter` | Rule-language expression the record must match, e.g. `room = 'lobby'`. `@collection` is not available. |
| `expand` | Relations to include, as for `GET .../records`. |
| `fields` | Fields to send, as for `GET .../records`. |

An event:

```json
{ "topic": "records:messages", "action": "create", "record": { "id": "…", "text": "hi" }, "ts": "2026-11-17T12:00:00.000Z" }
```

- `action` is `create`, `update` or `delete`.
- You only receive a record you may view: the collection's view rule is checked for every event and every subscriber, and so is your filter. Expanded relations follow the view rules of their collections.
- When an update makes a record leave what you may view (or stop matching your filter), you receive `delete` with `"reason": "no_longer_visible"`.
- `delete` events carry the record as it was before it was deleted.
- When your session is revoked, the connection closes within about 5 seconds.

## Server-sent events

1. Open the stream: `GET /api/realtime`. The first event is `connect`:

   ```
   event: connect
   data: {"client_id":"01a1…","resumed":false}
   ```

   A comment line is sent every 25 seconds to keep proxies from closing the connection.

2. Set the subscriptions with a normal API request, authenticated like any other (`Authorization: Bearer …` or the session cookie). Tokens never go into the stream URL.

   ```bash
   curl -X POST "$RACKEND_URL/api/realtime" \
     -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
     -d '{"client_id":"01a1…","subscriptions":[{"topic":"records:messages","filter":"room = '\''lobby'\''"}]}'
   ```

   Each request replaces the whole subscription set. The first authenticated request binds the connection to that account; binding it to another account later fails with 403.

3. Events arrive with the topic as the event name and an id:

   ```
   event: records:messages
   id: 01a1….42
   data: {"topic":"records:messages","action":"create","record":{…},"ts":"…"}
   ```

Other events:

| Event | Data | Meaning |
|---|---|---|
| `connect` | `{client_id, resumed}` | Stream opened (or resumed). |
| `resync` | `{type: "resync", topic}` | Events of this topic may have been missed; reload what you show. |
| `close` | `{type: "close", reason}` | The server ends the stream: `session_revoked`, `slow_consumer` or `shutdown`. |

**Reconnecting.** Browsers' `EventSource` sends `Last-Event-ID` on reconnect. rackend keeps the last 256 events of each connection for 2 minutes; if everything since that id is still there, the stream resumes (`resumed: true`) and you keep your subscriptions. Otherwise you get a new `client_id` (subscribe again) or `resync` events for topics with gaps.

**Limits.** Too many connections from one IP answer 429. A client that cannot keep up is closed with `slow_consumer`.

## WebSocket

`GET /api/ws` upgrades to a WebSocket that carries JSON text messages. Browsers cannot set headers on WebSocket requests, so authenticate with an `auth` message (or the session cookie from an allowed origin). Connections without `auth` are anonymous and see what anonymous users may view.

The server's first message:

```json
{ "t": "connect", "client_id": "01a1…" }
```

Client to server:

```json
{ "t": "auth",  "token": "rk_at_…" }
{ "t": "sub",   "id": 1, "topic": "records:messages", "filter": "room = 'lobby'", "expand": "author" }
{ "t": "unsub", "id": 2, "topic": "records:messages" }
{ "t": "ping" }
```

- `id` is yours; the reply to that message carries it.
- `unsub` removes the subscription with that `id`, or all subscriptions of `topic`.
- `auth` can be sent again later, for example after refreshing the access token.

Server to client:

```json
{ "t": "ack",    "id": 1 }
{ "t": "err",    "id": 1, "code": "forbidden", "message": "…", "errors": { … } }
{ "t": "record", "topic": "records:messages", "action": "create", "record": { … }, "ts": "…" }
{ "t": "resync", "topic": "records:messages" }
{ "t": "pong" }
```

- `code` in `err` is a problem code from [errors.md](errors.md); `errors` is present for validation problems.
- The server pings every 25 seconds and closes the connection after 60 seconds without an answer.
- The server closes with code `4000` and the reason as close reason: `session_revoked`, `slow_consumer`, `shutdown` or `timeout`.

## Channels and presence (M9)

Not available yet: `join`, `leave`, `pub` and `meta` messages answer `err` with `bad_request`. When they arrive, channels carry ephemeral messages between members of a named channel (`room:42`), with presence lists, on the same WebSocket:

```json
{ "t": "join",  "id": 3, "channel": "room:42", "meta": { "status": "typing" } }
{ "t": "leave", "id": 4, "channel": "room:42" }
{ "t": "pub",   "id": 5, "channel": "room:42", "event": "cursor", "data": { "x": 10 } }
{ "t": "meta",  "id": 6, "channel": "room:42", "meta": { "status": "idle" } }

{ "t": "msg",      "channel": "room:42", "event": "cursor", "from": { "user_id": "…", "conn_id": "…" }, "data": { … } }
{ "t": "presence", "channel": "room:42", "kind": "sync", "members": [ … ] }
```

## Collaborative editing (M9)

Not available yet. `ydoc` fields will be edited through `GET /api/collab/{collection}/{record_id}/{field}`, a WebSocket that speaks the binary y-protocols sync and awareness protocol, so `y-websocket` clients (Tiptap, BlockNote, CodeMirror and Monaco bindings) work unchanged. Connecting needs the view rule; changes need the update rule.
