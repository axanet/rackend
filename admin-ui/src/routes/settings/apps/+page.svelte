<script lang="ts">
	// Settings: apps that sign users in through rackend's OIDC provider
	// (clients), and the keys that sign their tokens.
	import { ApiError, del, get, message, patch, post } from '#lib/api.ts';
	import Icon from '#lib/components/Icon.svelte';
	import { session } from '#lib/session.svelte.ts';

	interface Client {
		id: string;
		name: string;
		kind: 'public' | 'confidential';
		has_secret: boolean;
		redirect_uris: string[];
		post_logout_redirect_uris: string[];
		scopes: string[];
		collection: string | null;
		first_party: boolean;
		dev: boolean;
		require_pkce: boolean;
		id_token_alg: 'RS256' | 'ES256';
		access_ttl_s: number;
		refresh_ttl_s: number;
		enabled: boolean;
	}
	interface Key {
		kid: string;
		alg: string;
		state: 'active' | 'upcoming' | 'retired';
		activated_at: string | null;
		delete_after: string | null;
	}
	interface Draft {
		id: string | null;
		name: string;
		kind: 'public' | 'confidential';
		collection: string;
		redirect_uris: string;
		post_logout_redirect_uris: string;
		first_party: boolean;
		dev: boolean;
		require_pkce: boolean;
		id_token_alg: 'RS256' | 'ES256';
		offline: boolean;
		enabled: boolean;
	}

	let clients = $state<Client[]>([]);
	let keys = $state<Key[]>([]);
	let issuer = $state<string | null>(null);
	let error = $state<ApiError | null>(null);
	let notice = $state('');
	let busy = $state(false);
	let dialog = $state<HTMLDialogElement>();
	let draft = $state<Draft | null>(null);
	let draftError = $state<ApiError | null>(null);
	let secret = $state<{ name: string; value: string } | null>(null);
	let secretDialog = $state<HTMLDialogElement>();
	let rotateDialog = $state<HTMLDialogElement>();
	let copied = $state(false);

	const authCollections = $derived(session.collections.filter((c) => c.type === 'auth').map((c) => c.name));

	async function load() {
		try {
			const [c, k, s] = await Promise.all([
				get<{ items: Client[] }>('/api/oauth-clients'),
				get<{ items: Key[] }>('/api/keys'),
				get<{ app: { public_url: string | null } }>('/api/settings')
			]);
			clients = c.items;
			keys = k.items;
			issuer = s.app.public_url;
		} catch (e) {
			error = e instanceof ApiError ? e : new ApiError(0, { detail: message(e) });
		}
	}
	load();

	const lines = (s: string) =>
		s
			.split('\n')
			.map((x) => x.trim())
			.filter(Boolean);

	function open(c: Client | null) {
		draft = c
			? {
					id: c.id,
					name: c.name,
					kind: c.kind,
					collection: c.collection ?? '',
					redirect_uris: c.redirect_uris.join('\n'),
					post_logout_redirect_uris: c.post_logout_redirect_uris.join('\n'),
					first_party: c.first_party,
					dev: c.dev,
					require_pkce: c.require_pkce,
					id_token_alg: c.id_token_alg,
					offline: c.scopes.includes('offline_access'),
					enabled: c.enabled
				}
			: {
					id: null,
					name: '',
					kind: 'public',
					collection: authCollections[0] ?? 'users',
					redirect_uris: '',
					post_logout_redirect_uris: '',
					first_party: true,
					dev: false,
					require_pkce: true,
					id_token_alg: 'RS256',
					offline: true,
					enabled: true
				};
		draftError = null;
		dialog?.showModal();
	}

	function showSecret(name: string, value: string | undefined) {
		if (!value) return;
		secret = { name, value };
		copied = false;
		secretDialog?.showModal();
	}

	async function save(e: SubmitEvent) {
		e.preventDefault();
		if (!draft) return;
		busy = true;
		draftError = null;
		const d = $state.snapshot(draft);
		const body = {
			name: d.name,
			kind: d.kind,
			collection: d.collection,
			redirect_uris: lines(d.redirect_uris),
			post_logout_redirect_uris: lines(d.post_logout_redirect_uris),
			first_party: d.first_party,
			dev: d.dev,
			require_pkce: d.kind === 'public' ? true : d.require_pkce,
			id_token_alg: d.id_token_alg,
			scopes: ['openid', 'profile', 'email', ...(d.offline ? ['offline_access'] : [])],
			enabled: d.enabled
		};
		try {
			const out = d.id
				? await patch<Client & { client_secret?: string }>(`/api/oauth-clients/${d.id}`, body)
				: await post<Client & { client_secret?: string }>('/api/oauth-clients', body);
			dialog?.close();
			notice = `Saved ${d.name}.`;
			await load();
			showSecret(out.name, out.client_secret);
		} catch (err) {
			draftError = err instanceof ApiError ? err : new ApiError(0, { detail: message(err) });
		} finally {
			busy = false;
		}
	}

	async function rotateSecret(c: Client) {
		if (!confirm(`Create a new secret for ${c.name}? The current one stops working at once.`)) return;
		try {
			const out = await patch<Client & { client_secret?: string }>(`/api/oauth-clients/${c.id}`, { rotate_secret: true });
			showSecret(out.name, out.client_secret);
		} catch (e) {
			error = e instanceof ApiError ? e : new ApiError(0, { detail: message(e) });
		}
	}

	async function remove(c: Client) {
		if (!confirm(`Delete ${c.name}? Its users are signed out of the app.`)) return;
		try {
			await del(`/api/oauth-clients/${c.id}`);
			notice = `Deleted ${c.name}.`;
			await load();
		} catch (e) {
			error = e instanceof ApiError ? e : new ApiError(0, { detail: message(e) });
		}
	}

	async function rotateKeys() {
		busy = true;
		try {
			await post('/api/keys/rotate');
			rotateDialog?.close();
			notice = 'New signing keys are active. Old tokens stay valid until they expire.';
			await load();
		} catch (e) {
			error = e instanceof ApiError ? e : new ApiError(0, { detail: message(e) });
		} finally {
			busy = false;
		}
	}

	async function copySecret() {
		if (!secret) return;
		try {
			await navigator.clipboard.writeText(secret.value);
			copied = true;
		} catch {
			copied = false;
		}
	}

	const derr = (k: string) => draftError?.field(k);
	const date = (s: string | null) => (s ? new Date(s).toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' }) : 'none');
</script>

<div class="page">
	<header class="bar">
		<div class="title">
			<span class="eyebrow">Settings</span>
			<h1>Apps</h1>
		</div>
		<div class="actions">
			<button type="button" class="btn primary" onclick={() => open(null)}><Icon name="plus" />Add app</button>
		</div>
	</header>

	<div aria-live="polite">
		{#if error}
			<p class="notice danger" role="alert"><Icon name="alert" /><span>{error.message}</span></p>
		{:else if notice}
			<p class="notice ok"><Icon name="check" /><span>{notice}</span></p>
		{/if}
	</div>

	<section class="card section">
		<div class="card-head"><span class="ico"><Icon name="link" /></span><span class="eyebrow">OpenID provider</span></div>
		{#if issuer}
			<p class="hint">Apps sign users in with any standard OpenID Connect library. Point it at the issuer:</p>
			<code class="mono issuer">{issuer}</code>
			<p class="hint">Discovery: <a href={`${issuer}/.well-known/openid-configuration`} target="_blank" rel="noreferrer">{issuer}/.well-known/openid-configuration</a></p>
		{:else}
			<p class="notice write"><Icon name="alert" /><span>Set the public URL first: it is the issuer of every token.</span></p>
		{/if}
	</section>

	<section class="card section">
		<div class="card-head"><span class="ico"><Icon name="key" /></span><span class="eyebrow">Apps</span></div>
		<ul class="list">
			{#each clients as c (c.id)}
				<li>
					<div class="who">
						<strong>{c.name}</strong>
						<span class="subtle mono">{c.id}</span>
						{#if c.redirect_uris.length === 0 && !c.dev}<span class="warn">Add your app's redirect URI to use it.</span>{/if}
					</div>
					<span class="tag">{c.kind}</span>
					{#if c.first_party}<span class="tag">first party</span>{/if}
					{#if !c.enabled}<span class="tag">disabled</span>{/if}
					<div class="row-actions">
						<button type="button" class="btn sm" onclick={() => open(c)}><Icon name="pencil" />Edit</button>
						{#if c.kind === 'confidential'}<button type="button" class="btn sm" onclick={() => rotateSecret(c)}><Icon name="refresh" />New secret</button>{/if}
						<button type="button" class="btn sm icon ghost danger" aria-label={`Delete ${c.name}`} title="Delete" onclick={() => remove(c)}><Icon name="trash" /></button>
					</div>
				</li>
			{:else}
				<li class="hint">none</li>
			{/each}
		</ul>
	</section>

	<section class="card section">
		<div class="card-head">
			<span class="ico"><Icon name="lock" /></span><span class="eyebrow">Signing keys</span>
			<button type="button" class="btn sm head-action" onclick={() => rotateDialog?.showModal()}><Icon name="refresh" />Rotate now</button>
		</div>
		<p class="hint">Keys rotate every 90 days on their own; new keys are published 7 days before they sign. Rotate now only if a key may have leaked.</p>
		<div class="table-wrap">
			<table class="table">
				<thead><tr><th scope="col">Key</th><th scope="col">Algorithm</th><th scope="col">State</th><th scope="col">Signs from</th><th scope="col">Removed after</th></tr></thead>
				<tbody>
					{#each keys as k (k.kid)}
						<tr>
							<td class="mono kid" title={k.kid}>{k.kid.slice(0, 12)}…</td>
							<td class="mono">{k.alg}</td>
							<td><span class="tag" class:on={k.state === 'active'}>{k.state}</span></td>
							<td>{date(k.activated_at)}</td>
							<td>{date(k.delete_after)}</td>
						</tr>
					{:else}
						<tr><td colspan="5" class="hint">none</td></tr>
					{/each}
				</tbody>
			</table>
		</div>
	</section>
</div>

<dialog bind:this={dialog} aria-labelledby="client-title">
	{#if draft}
		<form class="dialog" onsubmit={save}>
			<h2 id="client-title">{draft.id ? `Edit ${draft.name}` : 'Add an app'}</h2>
			{#if draftError && Object.keys(draftError.errors ?? {}).length === 0}
				<p class="notice danger" role="alert"><Icon name="alert" /><span>{draftError.message}</span></p>
			{/if}
			<div class="grid">
				<label class="field">
					Name
					<input type="text" required bind:value={draft.name} aria-invalid={derr('name') ? 'true' : undefined} />
					<span class={derr('name') ? 'error-text' : 'hint'}>{derr('name') ?? 'Shown on the consent page.'}</span>
				</label>
				<label class="field">
					Users from
					<select bind:value={draft.collection}>
						{#each authCollections as c (c)}<option value={c}>{c}</option>{/each}
					</select>
				</label>
			</div>
			{#if !draft.id}
				<fieldset class="kinds">
					<legend>Type</legend>
					<label class="inline"><input type="radio" value="public" bind:group={draft.kind} />Browser or mobile app (public, PKCE)</label>
					<label class="inline"><input type="radio" value="confidential" bind:group={draft.kind} />Server app (with a client secret)</label>
				</fieldset>
			{/if}
			<label class="field">
				Redirect URIs
				<textarea class="mono" rows="3" bind:value={draft.redirect_uris} placeholder="https://app.example.com/callback" aria-invalid={derr('redirect_uris') ? 'true' : undefined}></textarea>
				<span class={derr('redirect_uris') ? 'error-text' : 'hint'}>{derr('redirect_uris') ?? 'One per line, exact match.'}</span>
			</label>
			<label class="field">
				After sign-out
				<textarea class="mono" rows="2" bind:value={draft.post_logout_redirect_uris} placeholder="https://app.example.com/"></textarea>
			</label>
			<div class="checks">
				<label class="inline"><input type="checkbox" bind:checked={draft.first_party} />Your own app (no consent page)</label>
				<label class="inline"><input type="checkbox" bind:checked={draft.offline} />Refresh tokens (offline access)</label>
				<label class="inline"><input type="checkbox" bind:checked={draft.dev} />Allow http://localhost redirect URIs (development)</label>
				{#if draft.kind === 'confidential'}
					<label class="inline"><input type="checkbox" bind:checked={draft.require_pkce} />Require PKCE</label>
				{/if}
				<label class="inline"><input type="checkbox" bind:checked={draft.enabled} />Enabled</label>
			</div>
			<label class="field narrow">
				ID token signature
				<select bind:value={draft.id_token_alg}><option>RS256</option><option>ES256</option></select>
			</label>
			<div class="actions">
				<button type="button" class="btn" onclick={() => dialog?.close()}>Cancel</button>
				<button type="submit" class="btn primary" disabled={busy}>Save</button>
			</div>
		</form>
	{/if}
</dialog>

<dialog bind:this={secretDialog} aria-labelledby="secret-title">
	{#if secret}
		<div class="dialog">
			<h2 id="secret-title">Client secret for {secret.name}</h2>
			<p class="notice write"><Icon name="alert" /><span>Copy it now. rackend stores only a hash and cannot show it again.</span></p>
			<code class="mono secret">{secret.value}</code>
			<div class="actions">
				<button type="button" class="btn" onclick={copySecret}><Icon name={copied ? 'check' : 'copy'} />{copied ? 'Copied' : 'Copy'}</button>
				<button type="button" class="btn primary" onclick={() => secretDialog?.close()}>Done</button>
			</div>
		</div>
	{/if}
</dialog>

<dialog bind:this={rotateDialog} aria-labelledby="rotate-title">
	<div class="dialog">
		<h2 id="rotate-title">Rotate the signing keys now?</h2>
		<p>New keys sign from now on. Apps that cache the key set may reject new tokens until they fetch it again. Tokens signed with the old keys stay valid until they expire.</p>
		<div class="actions">
			<button type="button" class="btn" onclick={() => rotateDialog?.close()}>Cancel</button>
			<button type="button" class="btn danger" disabled={busy} onclick={rotateKeys}>Rotate</button>
		</div>
	</div>
</dialog>

<style>
	.page {
		display: grid;
		gap: 16px;
		max-width: 1000px;
	}
	.bar {
		display: flex;
		align-items: flex-end;
		justify-content: space-between;
		gap: 16px;
		flex-wrap: wrap;
		margin-bottom: 8px;
	}
	.title {
		display: grid;
		gap: 6px;
	}
	.section {
		padding: 24px;
		display: grid;
		gap: 14px;
		min-width: 0;
	}
	.head-action {
		margin-left: auto;
	}
	.issuer,
	.secret {
		display: block;
		padding: 10px 12px;
		border-radius: var(--radius);
		background: var(--surface);
		font-size: 13.5px;
		overflow-wrap: anywhere;
	}
	.list {
		display: grid;
		margin: 0;
		padding: 0;
		list-style: none;
	}
	.list li {
		display: flex;
		align-items: center;
		gap: 10px;
		flex-wrap: wrap;
		padding: 12px 0;
		border-top: 1px solid var(--border);
	}
	.list li:first-child {
		border-top: 0;
	}
	.who {
		display: grid;
		gap: 2px;
		flex: 1 1 240px;
		min-width: 0;
	}
	.who .subtle {
		font-size: 12.5px;
		overflow-wrap: anywhere;
	}
	.warn {
		font-size: 13px;
		color: var(--warning, var(--danger));
	}
	.row-actions {
		display: flex;
		gap: 6px;
		flex-wrap: wrap;
	}
	.table-wrap {
		overflow-x: auto;
	}
	.kid {
		font-size: 12.5px;
	}
	.tag.on {
		color: var(--accent);
		border-color: var(--accent);
	}
	dialog {
		width: min(620px, calc(100vw - 32px));
	}
	.dialog {
		display: grid;
		gap: 14px;
	}
	.grid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
		gap: 14px;
	}
	.narrow {
		max-width: 260px;
	}
	.kinds {
		display: grid;
		gap: 4px;
		margin: 0;
		padding: 0;
		border: 0;
	}
	.kinds legend {
		font-size: 14px;
		font-weight: 500;
		margin-bottom: 4px;
	}
	.checks {
		display: grid;
		gap: 2px;
	}
	.inline {
		display: inline-flex;
		align-items: center;
		gap: 8px;
		min-height: 32px;
		font-size: 14px;
		cursor: pointer;
	}
	.actions {
		display: flex;
		gap: 8px;
		justify-content: flex-end;
	}
	@media (max-width: 480px) {
		.section {
			padding: 20px;
		}
	}
</style>
