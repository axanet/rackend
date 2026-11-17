// Who is signed in. Tokens live in HttpOnly cookies; only the superuser's
// public profile is kept here.

import { get, post, type Collection } from './api';

export interface Superuser {
	id: string;
	email: string;
}

const KEY = 'rackend.superuser';

function load(): Superuser | null {
	try {
		const v = localStorage.getItem(KEY);
		return v ? (JSON.parse(v) as Superuser) : null;
	} catch {
		return null;
	}
}

class Session {
	user = $state<Superuser | null>(load());
	collections = $state<Collection[]>([]);

	signIn(user: Superuser) {
		this.user = { id: user.id, email: user.email };
		try {
			localStorage.setItem(KEY, JSON.stringify(this.user));
		} catch {
			/* storage unavailable: session still works for this tab */
		}
	}

	clear() {
		this.user = null;
		this.collections = [];
		try {
			localStorage.removeItem(KEY);
		} catch {
			/* ignore */
		}
	}

	async signOut() {
		try {
			await post('/api/superusers/logout');
		} finally {
			this.clear();
		}
	}

	async loadCollections() {
		const page = await get<{ items: Collection[] }>('/api/collections?per_page=500');
		this.collections = page.items;
	}
}

export const session = new Session();
