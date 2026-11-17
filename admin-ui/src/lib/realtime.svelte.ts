// Realtime over Server-Sent Events (SPEC §13.2.1). One EventSource for the
// admin UI; the session cookie authorizes the subscription request, so no
// token ever appears in a URL. The browser reconnects on its own and the
// server replays missed events (Last-Event-ID) or asks for a resync.

import { post } from './api';

type Handler = (event: { topic: string; action?: string; resync?: boolean }) => void;

class Live {
	connected = $state(false);
	private source: EventSource | null = null;
	private clientId = '';
	private topics: string[] = [];
	private handler: Handler | null = null;
	private listened = new Set<string>();

	/** Watches `topics`, replacing the previous set. */
	watch(topics: string[], handler: Handler) {
		this.topics = topics;
		this.handler = handler;
		for (const t of topics) this.listen(t);
		if (!this.source) this.open();
		else if (this.clientId) void this.bind();
	}

	/** Stops watching (keeps the stream for the next page). */
	unwatch() {
		this.topics = [];
		this.handler = null;
		if (this.clientId) void this.bind();
	}

	private open() {
		const es = new EventSource('/api/realtime');
		this.source = es;
		es.addEventListener('connect', (e) => {
			this.clientId = JSON.parse((e as MessageEvent).data).client_id;
			this.connected = true;
			void this.bind();
		});
		es.addEventListener('resync', (e) => {
			const topic = JSON.parse((e as MessageEvent).data).topic as string;
			this.handler?.({ topic, resync: true });
		});
		es.addEventListener('close', () => {
			// Session ended or server shutdown: start over after a pause.
			this.connected = false;
			es.close();
			this.source = null;
			setTimeout(() => this.topics.length && this.open(), 3000);
		});
		es.onerror = () => {
			this.connected = es.readyState === EventSource.OPEN;
		};
		for (const t of this.listened) this.attach(t);
	}

	private listen(topic: string) {
		if (this.listened.has(topic)) return;
		this.listened.add(topic);
		this.attach(topic);
	}

	private attach(topic: string) {
		this.source?.addEventListener(topic, (e) => {
			if (!this.topics.includes(topic)) return;
			const data = JSON.parse((e as MessageEvent).data);
			this.handler?.({ topic, action: data.action });
		});
	}

	private async bind() {
		try {
			await post('/api/realtime', { client_id: this.clientId, subscriptions: this.topics.map((topic) => ({ topic })) });
		} catch {
			this.connected = false;
		}
	}
}

export const live = new Live();
