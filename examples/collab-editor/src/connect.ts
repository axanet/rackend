// Connects a Y.Doc to a rackend `ydoc` field with y-websocket.
//
// Browsers cannot set headers on WebSockets, so each connection uses a
// single-use ticket (valid 30 s) fetched with the access token. Tickets live
// in the memory of the server that issued them, so a restart forgets them:
// after every closed connection, failed attempts included, a fresh ticket is
// fetched before y-websocket tries again.
import * as decoding from 'lib0/decoding';
import { WebsocketProvider } from 'y-websocket';
import type * as Y from 'yjs';

/** rackend's custom y-protocols message type for errors. */
export const MSG_ERROR = 100;

export type CollabError = { code: string; message: string };

export type Target = {
	/** e.g. http://localhost:8090 */
	server: string;
	collection: string;
	record: string;
	field: string;
	/** Access token of the signed-in user. */
	token: string;
};

async function ticket(t: Target): Promise<string> {
	const path = `/api/collab/${t.collection}/${t.record}/${t.field}/ticket`;
	const res = await fetch(t.server + path, {
		method: 'POST',
		headers: { authorization: `Bearer ${t.token}` }
	});
	if (!res.ok) throw new Error(`ticket: ${res.status} ${await res.text()}`);
	return (await res.json()).ticket;
}

export async function connect(
	t: Target,
	doc: Y.Doc,
	onError: (e: CollabError) => void = () => {}
): Promise<WebsocketProvider> {
	const ws = t.server.replace(/^http/, 'ws');
	const provider = new WebsocketProvider(
		`${ws}/api/collab/${t.collection}/${t.record}`,
		t.field,
		doc,
		// No BroadcastChannel between tabs: it would bypass the server, and
		// with it the read-only check.
		{ params: { ticket: await ticket(t) }, maxBackoffTime: 1000, disableBc: true }
	);
	provider.messageHandlers[MSG_ERROR] = (_encoder, decoder) => {
		const body = new TextDecoder().decode(decoding.readVarUint8Array(decoder));
		onError(JSON.parse(body));
	};
	// Not `status: disconnected`: that only fires when an open connection
	// drops, so a ticket the server rejects (say, one the previous process
	// issued while shutting down) would be retried forever.
	let refreshing = false;
	provider.on('connection-close', async () => {
		if (refreshing) return;
		refreshing = true;
		// The server may be restarting: keep trying until a ticket arrives.
		while (provider.shouldConnect) {
			try {
				provider.params.ticket = await ticket(t);
				break;
			} catch {
				await new Promise((r) => setTimeout(r, 500));
			}
		}
		refreshing = false;
	});
	return provider;
}
