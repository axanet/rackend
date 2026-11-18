// A Tiptap editor on a rackend `ydoc` field. Open it in two windows (or
// sign in as two users) to co-edit.
import { Editor } from '@tiptap/core';
import Collaboration from '@tiptap/extension-collaboration';
import CollaborationCaret from '@tiptap/extension-collaboration-caret';
import StarterKit from '@tiptap/starter-kit';
import * as Y from 'yjs';
import { connect } from './connect';

const q = new URLSearchParams(location.search);
const server = q.get('server') ?? 'http://localhost:8090';
const collection = q.get('collection') ?? 'notes';
const field = q.get('field') ?? 'body';
const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const status = (s: string) => ($('status').textContent = s);

const COLORS = ['#2563eb', '#db2777', '#059669', '#d97706', '#7c3aed'];

$<HTMLFormElement>('signin').addEventListener('submit', async (e) => {
	e.preventDefault();
	const email = $<HTMLInputElement>('email').value;
	const password = $<HTMLInputElement>('password').value;
	const res = await fetch(`${server}/api/collections/users/auth-with-password`, {
		method: 'POST',
		headers: { 'content-type': 'application/json' },
		body: JSON.stringify({ email, password })
	});
	if (!res.ok) return status(`Sign-in failed: ${res.status}`);
	const { access_token: token } = await res.json();
	let record = q.get('record');
	if (!record) {
		const list = await fetch(`${server}/api/collections/${collection}/records?per_page=1`, {
			headers: { authorization: `Bearer ${token}` }
		}).then((r) => r.json());
		record = list.items?.[0]?.id;
	}
	if (!record) return status(`No record in ${collection}. Create one first.`);
	$('signin').hidden = true;

	const doc = new Y.Doc();
	const provider = await connect({ server, collection, record, field, token }, doc, (err) =>
		status(`${err.code}: ${err.message}`)
	);
	provider.on('status', ({ status: s }) => status(s === 'connected' ? `Editing ${collection}/${record}` : s));
	new Editor({
		element: $('editor'),
		extensions: [
			StarterKit.configure({ undoRedo: false }),
			Collaboration.configure({ document: doc }),
			CollaborationCaret.configure({
				provider,
				user: { name: email, color: COLORS[email.length % COLORS.length] }
			})
		]
	});
});
