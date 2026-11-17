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
	// FormData sets its own multipart content type with the boundary.
	const form = body instanceof FormData;
	return fetch(path, {
		method,
		credentials: 'same-origin',
		headers: {
			Accept: 'application/json',
			...(body !== undefined && !form ? { 'Content-Type': 'application/json' } : {}),
			...headers
		},
		body: form ? body : body !== undefined ? JSON.stringify(body) : undefined
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

/** A multipart body: the JSON fields in `@json`, files under their field name. */
export function formBody(json: Record<string, unknown>, files: [string, File][]): FormData {
	const fd = new FormData();
	fd.append('@json', JSON.stringify(json));
	for (const [field, file] of files) fd.append(field, file, file.name);
	return fd;
}

let fileToken: { token: string; until: number } | null = null;

/** A short-lived token for protected files, reused while it is fresh. */
export async function getFileToken(): Promise<string> {
	if (fileToken && Date.now() < fileToken.until) return fileToken.token;
	const t = await post<{ token: string; expires_in: number }>('/api/files/token');
	fileToken = { token: t.token, until: Date.now() + (t.expires_in - 20) * 1000 };
	return t.token;
}

/** URL of a stored file (optionally a thumbnail or a forced download). */
export function fileUrl(collection: string, recordId: string, filename: string, opts: { thumb?: string; token?: string; download?: boolean } = {}): string {
	const q = new URLSearchParams();
	if (opts.thumb) q.set('thumb', opts.thumb);
	if (opts.token) q.set('token', opts.token);
	if (opts.download) q.set('download', '1');
	const qs = q.toString();
	return `/api/files/${encodeURIComponent(collection)}/${encodeURIComponent(recordId)}/${encodeURIComponent(filename)}${qs ? `?${qs}` : ''}`;
}

/** Whether thumbnails can be requested for a stored filename. */
export const isImageName = (name: string) => /\.(jpe?g|png|webp|gif)$/i.test(name);

/** Bytes as B, KB, MB or GB. */
export function formatBytes(n: number): string {
	if (n < 1024) return `${n} B`;
	const units = ['KB', 'MB', 'GB', 'TB'];
	let v = n / 1024;
	let i = 0;
	while (v >= 1024 && i < units.length - 1) {
		v /= 1024;
		i++;
	}
	return `${v < 10 ? v.toFixed(1) : Math.round(v)} ${units[i]}`;
}

/** Human message for any thrown value. */
export function message(e: unknown): string {
	if (e instanceof ApiError) {
		return e.requestId && e.status >= 500 ? `${e.message} (reference ${e.requestId})` : e.message;
	}
	return e instanceof Error ? e.message : String(e);
}

// ---- Types mirrored from the API -------------------------------------------

export type FieldType = 'text' | 'email' | 'url' | 'number' | 'bool' | 'date' | 'autodate' | 'select' | 'json' | 'relation' | 'file';

export interface Field {
	id?: string;
	name: string;
	type: FieldType;
	required?: boolean;
	/** System field of an auth collection (`email`, `email_verified`): fixed. */
	system?: boolean;
	[option: string]: unknown;
}

/** Options of an auth collection (flat in `options`). */
export interface AuthOptions {
	signup_enabled: boolean;
	email_visibility: boolean;
	methods: { password: boolean; passkey: boolean; otp: boolean; magic_link: boolean };
	password: { min_length: number; require_verified_email_to_login: boolean };
	session: { access_ttl_s: number; refresh_ttl_s: number; idle_timeout_s: number };
	oidc_claims?: Record<string, string>;
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
	options: { audit_records: boolean } & Partial<AuthOptions>;
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
