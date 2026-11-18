# Collaborative editor example

A [Tiptap](https://tiptap.dev) editor on a rackend `ydoc` field, synced with the standard [`y-websocket`](https://github.com/yjs/y-websocket) provider. Open it in two windows to co-edit, with live carets.

`src/connect.ts` is the part to copy into your app: it fetches a single-use ticket with the user's access token (browsers cannot set headers on WebSockets), fetches a new one whenever the connection drops, reads rackend's error message (type 100) and turns off y-websocket's BroadcastChannel, which would sync tabs past the server's read-only check.

1. Start rackend in dev mode (it allows `http://localhost:*` origins):

   ```sh
   RACKEND_DEV=true RACKEND_PUBLIC_URL=http://localhost:8090 RACKEND_HTTP_ADDR=127.0.0.1:8090 rackend serve
   ```

2. In the admin UI, create a collection `notes` with a `ydoc` field `body`, rules that let signed-in users view and update (`@request.auth.id != ""`), two users, and one record.

3. Run the example with [bun](https://bun.sh):

   ```sh
   bun install
   bun run dev
   ```

4. Open <http://localhost:5173/> in two windows and sign in. `?server=`, `?collection=`, `?record=` and `?field=` point it elsewhere; without `record` it opens the first record of the collection.

## Acceptance test

`bun acceptance.ts [path to rackend]` runs the M9 acceptance headless against a fresh instance: two editors co-edit, the server is killed and restarted while one keeps typing, nothing is lost, and a read-only user's edits are rejected and never stored. `scripts/ci.sh` runs it.
