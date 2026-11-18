// M9 acceptance (SPEC §20 M9), headless: two y-websocket editors co-edit a
// `ydoc` field, the server is killed and restarted mid-session (edits made
// while it is down included), and no edit is lost; a read-only user's
// updates are rejected with message type 100 and never persisted.
//
// Usage: bun acceptance.ts [path to the rackend binary]
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import * as Y from 'yjs';
import { type CollabError, connect } from './src/connect';

const bin = resolve(process.argv[2] ?? '../../target/debug/rackend');
const port = 18093;
const server = `http://127.0.0.1:${port}`;
const data = mkdtempSync(join(tmpdir(), 'rackend-collab-'));
const PW = 'collab acceptance password';

let proc: ReturnType<typeof Bun.spawn> | null = null;

async function start() {
	proc = Bun.spawn([bin, 'serve'], {
		env: {
			...process.env,
			RACKEND_DATA_DIR: data,
			RACKEND_HTTP_ADDR: `127.0.0.1:${port}`,
			RACKEND_PUBLIC_URL: server,
			RACKEND_SUPERUSER_EMAIL: 'admin@example.com',
			RACKEND_SUPERUSER_PASSWORD: PW,
			RACKEND_LOG_LEVEL: 'warn'
		},
		stdout: 'inherit',
		stderr: 'inherit'
	});
	for (let i = 0; i < 100; i++) {
		if (await fetch(`${server}/api/health`).then((r) => r.ok, () => false)) return;
		await Bun.sleep(100);
	}
	throw new Error('rackend did not start');
}

async function stop(signal: 'SIGTERM' | 'SIGKILL') {
	proc?.kill(signal);
	await proc?.exited;
	proc = null;
}

async function api(path: string, token: string | null, body?: unknown, method = body ? 'POST' : 'GET') {
	const res = await fetch(server + path, {
		method,
		headers: {
			...(token ? { authorization: `Bearer ${token}` } : {}),
			...(body ? { 'content-type': 'application/json' } : {})
		},
		body: body ? JSON.stringify(body) : undefined
	});
	if (!res.ok) throw new Error(`${method} ${path}: ${res.status} ${await res.text()}`);
	return res.status === 204 ? null : res.headers.get('content-type')?.includes('json') ? res.json() : res.arrayBuffer();
}

async function signIn(email: string) {
	await api('/api/collections/users/records', null, { email, password: PW });
	const out = await api('/api/collections/users/auth-with-password', null, { email, password: PW });
	return { token: out.access_token as string, id: out.record.id as string };
}

async function until(what: string, f: () => boolean, ms = 10_000) {
	const end = Date.now() + ms;
	while (!f()) {
		if (Date.now() > end) throw new Error(`timed out: ${what}`);
		await Bun.sleep(50);
	}
}

const text = (d: Y.Doc) => d.getText('body').toString();
const check = (ok: boolean, msg: string) => {
	if (!ok) throw new Error(msg);
	console.log(`ok: ${msg}`);
};

try {
	await start();
	const su = (await api('/api/superusers/auth-with-password', null, { email: 'admin@example.com', password: PW }))
		.access_token;
	const a = await signIn('alice@example.com');
	const b = await signIn('bob@example.com');
	const r = await signIn('reader@example.com');
	await api('/api/collections', su, {
		name: 'notes',
		fields: [
			{ name: 'title', type: 'text' },
			{ name: 'body', type: 'ydoc' }
		],
		rules: {
			list: "@request.auth.id != ''",
			view: "@request.auth.id != ''",
			create: "@request.auth.id != ''",
			update: `@request.auth.id = '${a.id}' || @request.auth.id = '${b.id}'`,
			delete: null
		}
	});
	const rec = await api('/api/collections/notes/records', a.token, { title: 'Plan' });
	const target = (token: string) => ({ server, collection: 'notes', record: rec.id, field: 'body', token });

	const docA = new Y.Doc();
	const docB = new Y.Doc();
	const docR = new Y.Doc();
	const rejected: CollabError[] = [];
	const pa = await connect(target(a.token), docA);
	const pb = await connect(target(b.token), docB);
	const pr = await connect(target(r.token), docR, (e) => rejected.push(e));
	await until('editors synced', () => pa.synced && pb.synced && pr.synced);

	docA.getText('body').insert(0, 'Alpha. ');
	docB.getText('body').insert(0, 'Bravo. ');
	await until('both edits everywhere', () =>
		[docA, docB, docR].every((d) => text(d).includes('Alpha') && text(d).includes('Bravo'))
	);
	check(text(docA) === text(docB), `editors converge: "${text(docA)}"`);

	docR.getText('body').insert(0, 'Reader. ');
	await until('read-only rejection', () => rejected.length > 0);
	check(rejected[0].code === 'read_only', 'the read-only update is rejected with type 100');
	await Bun.sleep(300);
	check(!text(docA).includes('Reader'), 'the read-only update is not relayed');

	// Kill the server mid-session (no graceful flush) and keep editing.
	docA.getText('body').insert(text(docA).length, 'Charlie. ');
	await Bun.sleep(50);
	await stop('SIGKILL');
	docB.getText('body').insert(text(docB).length, 'Delta (offline). ');
	await start();
	await until(
		'convergence after the restart',
		() => pa.synced && pb.synced && ['Alpha', 'Bravo', 'Charlie', 'Delta'].every((w) => text(docA).includes(w) && text(docB).includes(w)),
		20_000
	);
	check(text(docA) === text(docB), `no edits lost across the restart: "${text(docA)}"`);

	// Graceful restart: what the server stored is the whole document,
	// without the reader's text.
	await Bun.sleep(500);
	await stop('SIGTERM');
	await start();
	const state = new Uint8Array(await api(`/api/collab/notes/${rec.id}/body/state`, r.token));
	const stored = new Y.Doc();
	Y.applyUpdate(stored, state);
	await until('editors back', () => pa.synced && pb.synced);
	check(text(stored) === text(docA), 'the stored document has every edit');
	check(!text(stored).includes('Reader'), "the read-only user's update was never persisted");

	for (const p of [pa, pb, pr]) p.destroy();
	console.log('collab acceptance passed');
} finally {
	await stop('SIGTERM');
	rmSync(data, { recursive: true, force: true });
}
process.exit(0);
