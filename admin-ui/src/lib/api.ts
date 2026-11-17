// Thin client for the rackend REST API. Uses cookie mode (HttpOnly session
// cookies, SPEC §11.6) so no token is ever readable by page scripts.

export interface FieldError {
	code: string;
	message: string;
	params?: Record<string, unknown>;
}

/** An RFC 9457 problem returned by the API. */
export class ApiError extends Error {
	status: number;
	code: string;
	detail?: string;
	errors: Record<string, FieldError[]>;
	position?: { line: number; column: number; offset: number };
	requestId?: string;
	raw: Record<string, unknown>;

	constructor(status: number, body: Record<string, unknown>) {
		const detail = (body.detail as string | undefined) ?? (body.title as string | undefined) ?? `HTTP ${status}`;
		super(detail);
		this.status = status;
		this.code = (body.code as string | undefined) ?? 'unknown';
		this.detail = body.detail as string | undefined;
		this.errors = (body.errors as Record<string, FieldError[]>) ?? {};
		this.position = body.position as ApiError['position'];
		this.requestId = body.request_id as string | undefined;
		this.raw = body;
	}

	/** First error message for a field path, if any. */
	field(path: string): string | undefined {
		return this.errors[path]?.[0]?.message;
	}
}

type Method = 'GET' | 'POST' | 'PATCH' | 'DELETE';

let refreshing: Promise<boolean> | null = null;
let onUnauthorized: (() => void) | null = null;

/** Called when the session can't be refreshed (redirect to login). */
export function setUnauthorizedHandler(fn: () => void) {
	onUnauthorized = fn;
}

async function raw(method: Method, path: string, body?: unknown, headers: Record<string, string> = {}): Promise<Response> {
	return fetch(path, {
		method,
		credentials: 'same-origin',
		headers: {
			Accept: 'application/json',
			...(body !== undefined ? { 'Content-Type': 'application/json' } : {}),
			...headers
		},
		body: body !== undefined ? JSON.stringify(body) : undefined
	});
}

async function refresh(): Promise<boolean> {
	refreshing ??= raw('POST', '/api/superusers/auth-refresh', undefined, { 'X-Rackend-Auth-Mode': 'cookie' })
		.then((r) => r.ok)
		.catch(() => false)
		.finally(() => setTimeout(() => (refreshing = null), 0));
	return refreshing;
}

/** Calls the API; on 401 tries one cookie refresh, then gives up. */
export async function api<T = unknown>(method: Method, path: string, body?: unknown, headers: Record<string, string> = {}): Promise<T> {
	let res = await raw(method, path, body, headers);
	if (res.status === 401 && !path.startsWith('/api/superusers/auth-')) {
		if (await refresh()) {
			res = await raw(method, path, body, headers);
		}
		if (res.status === 401) {
			onUnauthorized?.();
		}
	}
	if (res.status === 204) {
		return undefined as T;
	}
	const text = await res.text();
	const json = text ? JSON.parse(text) : {};
	if (!res.ok) {
		throw new ApiError(res.status, json);
	}
	return json as T;
}

export const get = <T>(path: string) => api<T>('GET', path);
export const post = <T>(path: string, body?: unknown, headers?: Record<string, string>) => api<T>('POST', path, body, headers);
export const patch = <T>(path: string, body?: unknown, headers?: Record<string, string>) => api<T>('PATCH', path, body, headers);
export const del = <T>(path: string, headers?: Record<string, string>) => api<T>('DELETE', path, undefined, headers);

/** Human message for any thrown value. */
export function message(e: unknown): string {
	if (e instanceof ApiError) {
		return e.requestId && e.status >= 500 ? `${e.message} (reference ${e.requestId})` : e.message;
	}
	return e instanceof Error ? e.message : String(e);
}

// ---- Types mirrored from the API -------------------------------------------

export type FieldType = 'text' | 'email' | 'url' | 'number' | 'bool' | 'date' | 'autodate' | 'select' | 'json' | 'relation';

export interface Field {
	id?: string;
	name: string;
	type: FieldType;
	required?: boolean;
	[option: string]: unknown;
}

export interface IndexDef {
	name: string;
	fields: string[];
	unique: boolean;
}

export type RuleOp = 'list' | 'view' | 'create' | 'update' | 'delete';

export interface Collection {
	id: string;
	name: string;
	type: 'base' | 'auth';
	fields: Field[];
	indexes: IndexDef[];
	rules: Record<RuleOp, string | null>;
	options: { audit_records: boolean };
	created: string;
	updated: string;
	ddl?: string;
	warnings?: string[];
}

export interface Page<T> {
	page: number;
	per_page: number;
	total_items: number;
	total_pages: number;
	items: T[];
}

export type RecordRow = { id: string; created: string; updated: string } & Record<string, unknown>;
