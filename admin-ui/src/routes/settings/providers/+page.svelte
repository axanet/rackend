<script lang="ts">
	// Settings: external sign-in providers (OpenID Connect and the GitHub
	// preset) and the app redirect URIs that may receive sign-in results.
	import { ApiError, del, get, message, patch, post } from '#lib/api.ts';
	import Icon from '#lib/components/Icon.svelte';
	import { session } from '#lib/session.svelte.ts';

	const MASK = '••••';

	interface Provider {
		id: string;
		name: string;
		kind: 'oidc' | 'oauth2_preset';
		preset: string | null;
		issuer: string | null;
		client_id: string;
		client_secret: string | null;
		scopes: string[];
		collection: string | null;
		auto_link_verified_email: boolean;
		claim_mapping: Record<string, string>;
		enabled: boolean;
		redirect_uri: string | null;
	}
	interface Preset {
		preset: string;
		label: string;
		kind: 'oidc' | 'oauth2_preset';
		issuer: string | null;
		scopes: string[];
		docs: string;
		placeholders?: string[];
	}
	interface Draft {
		id: string | null;
		name: string;
		kind: 'oidc' | 'oauth2_preset';
		preset: string;
		issuer: string;
		client_id: string;
		client_secret: string;
		scopes: string;
		collection: string;
		auto_link_verified_email: boolean;
		enabled: boolean;
	}

	let providers = $state<Provider[]>([]);
	let presets = $state<Preset[]>([]);
	let publicUrl = $state<string | null>(null);
	let redirects = $state('');
	let savedRedirects = $state('');
	let error = $state<ApiError | null>(null);
	let notice = $state('');
	let busy = $state(false);
	let dialog = $state<HTMLDialogElement>();
	let draft = $state<Draft | null>(null);
	let draftError = $state<ApiError | null>(null);
	let discovery = $state<{ ok: boolean; text: string } | null>(null);

	const authCollections = $derived(session.collections.filter((c) => c.type === 'auth').map((c) => c.name));

	async function load() {
		try {
			const [p, s] = await Promise.all([
				get<{ items: Provider[]; presets: Preset[] }>('/api/auth-providers'),
				get<{ app: { public_url: string | null }; security: { app_redirect_uris: string[] } }>('/api/settings')
			]);
			providers = p.items;
			presets = p.presets;
			publicUrl = s.app.public_url;
			redirects = s.security.app_redirect_uris.join('\n');
			savedRedirects = redirects;
		} catch (e) {
			error = e instanceof ApiError ? e : new ApiError(0, { detail: message(e) });
		}
	}
	load();

	function callbackUrl(d: Draft): string {
		const base = (publicUrl ?? 'https://your-public-url').replace(/\/$/, '');
		return `${base}/api/collections/${d.collection || 'users'}/providers/${d.name || '{name}'}/callback`;
	}

	function openNew(p: Preset) {
		draft = {
			id: null,
			name: p.preset,
			kind: p.kind,
			preset: p.preset,
			issuer: p.issuer ?? '',
			client_id: '',
			client_secret: '',
			scopes: p.scopes.join(' '),
			collection: authCollections[0] ?? 'users',
			auto_link_verified_email: false,
			enabled: true
		};
		draftError = null;
		discovery = null;
		dialog?.showModal();
	}

	function openEdit(p: Provider) {
		draft = {
			id: p.id,
			name: p.name,
			kind: p.kind,
			preset: p.preset ?? '',
			issuer: p.issuer ?? '',
			client_id: p.client_id,
			client_secret: p.client_secret === MASK ? MASK : '',
			scopes: p.scopes.join(' '),
			collection: p.collection ?? '',
			auto_link_verified_email: p.auto_link_verified_email,
			enabled: p.enabled
		};
		draftError = null;
		discovery = null;
		dialog?.showModal();
	}

	const presetOf = (d: Draft) => presets.find((p) => p.preset === d.preset);

	async function discover() {
		if (!draft) return;
		discovery = null;
		try {
			const r = await post<{ metadata: { issuer: string; authorization_endpoint: string } }>('/api/auth-providers/discover', {
				issuer: draft.issuer,
				name: draft.name,
				collection: draft.collection
			});
			discovery = { ok: true, text: `Found ${r.metadata.issuer}.` };
		} catch (e) {
			discovery = { ok: false, text: e instanceof ApiError ? (e.field('issuer') ?? e.message) : message(e) };
		}
	}

	async function saveDraft(e: SubmitEvent) {
		e.preventDefault();
		if (!draft) return;
		busy = true;
		draftError = null;
		const d = $state.snapshot(draft);
		const body: Record<string, unknown> = {
			name: d.name,
			kind: d.kind,
			preset: d.preset || null,
			client_id: d.client_id,
			scopes: d.scopes,
			collection: d.collection,
			auto_link_verified_email: d.auto_link_verified_email,
			enabled: d.enabled
		};
		if (d.kind === 'oidc') body.issuer = d.issuer;
		if (d.client_secret !== MASK) body.client_secret = d.client_secret === '' ? null : d.client_secret;
		try {
			if (d.id) await patch(`/api/auth-providers/${d.id}`, body);
			else await post('/api/auth-providers', body);
			dialog?.close();
			notice = `Saved ${d.name}.`;
			await load();
		} catch (err) {
			draftError = err instanceof ApiError ? err : new ApiError(0, { detail: message(err) });
		} finally {
			busy = false;
		}
	}

	async function remove(p: Provider) {
		if (!confirm(`Remove ${p.name}? Linked accounts keep their identities and can sign in again if you add it back.`)) return;
		try {
			await del(`/api/auth-providers/${p.id}`);
			notice = `Removed ${p.name}.`;
			await load();
		} catch (e) {
			error = e instanceof ApiError ? e : new ApiError(0, { detail: message(e) });
		}
	}

	async function saveRedirects() {
		busy = true;
		error = null;
		try {
			const uris = redirects
				.split('\n')
				.map((s) => s.trim())
				.filter(Boolean);
			const s = await patch<{ security: { app_redirect_uris: string[] } }>('/api/settings', { security: { app_redirect_uris: uris } });
			redirects = s.security.app_redirect_uris.join('\n');
			savedRedirects = redirects;
			notice = 'Saved the redirect URIs.';
		} catch (e) {
			error = e instanceof ApiError ? e : new ApiError(0, { detail: message(e) });
		} finally {
			busy = false;
		}
	}

	const derr = (k: string) => draftError?.field(k);
</script>

<div class="page">
	<header class="bar">
		<div class="title">
			<span class="eyebrow">Settings</span>
			<h1>Sign-in providers</h1>
		</div>
	</header>

	<div aria-live="polite">
		{#if error}
			<p class="notice danger" role="alert"><Icon name="alert" /><span>{error.message}</span></p>
		{:else if notice}
			<p class="notice ok"><Icon name="check" /><span>{notice}</span></p>
		{/if}
	</div>

	{#if !publicUrl}
		<p class="notice write"><Icon name="alert" /><span>Set the public URL first: providers send users back to it.</span></p>
	{/if}

	<section class="card section">
		<div class="card-head"><span class="ico"><Icon name="key" /></span><span class="eyebrow">Configured</span></div>
		{#if providers.length === 0}
			<p class="hint">No providers yet. Add one below.</p>
		{:else}
			<ul class="list">
				{#each providers as p (p.id)}
					<li>
						<div class="who">
							<strong class="mono">{p.name}</strong>
							<span class="subtle">{p.kind === 'oidc' ? p.issuer : 'GitHub'} · {p.collection ?? 'none'}</span>
						</div>
						<span class="tag">{p.enabled ? 'enabled' : 'disabled'}</span>
						{#if p.auto_link_verified_email}<span class="tag">links verified emails</span>{/if}
						<div class="row-actions">
							<button type="button" class="btn sm" onclick={() => openEdit(p)}><Icon name="pencil" />Edit</button>
							<button type="button" class="btn sm icon ghost danger" aria-label={`Remove ${p.name}`} title="Remove" onclick={() => remove(p)}><Icon name="trash" /></button>
						</div>
					</li>
				{/each}
			</ul>
		{/if}
	</section>

	<section class="card section">
		<div class="card-head"><span class="ico"><Icon name="plus" /></span><span class="eyebrow">Add a provider</span></div>
		<div class="presets">
			{#each presets as p (p.preset)}
				<button type="button" class="preset" onclick={() => openNew(p)}>
					<strong>{p.label}</strong>
					<span class="subtle">{p.kind === 'oidc' ? 'OpenID Connect' : 'OAuth 2.0'}</span>
				</button>
			{/each}
			<button
				type="button"
				class="preset"
				onclick={() => openNew({ preset: '', label: 'OpenID Connect', kind: 'oidc', issuer: '', scopes: ['openid', 'email', 'profile'], docs: '' })}
			>
				<strong>Other</strong>
				<span class="subtle">Any OpenID Connect provider</span>
			</button>
		</div>
	</section>

	<section class="card section">
		<div class="card-head"><span class="ico"><Icon name="link" /></span><span class="eyebrow">App redirect URIs</span></div>
		<p class="hint">Where sign-ins may return to: provider sign-ins and magic links. Exact match, one per line.</p>
		<label class="field">
			<span class="visually-hidden">App redirect URIs</span>
			<textarea class="mono" rows="4" bind:value={redirects} placeholder="https://app.example.com/auth/done"></textarea>
		</label>
		<div><button type="button" class="btn" disabled={busy || redirects === savedRedirects} onclick={saveRedirects}>Save URIs</button></div>
	</section>
</div>

<dialog bind:this={dialog} aria-labelledby="provider-title">
	{#if draft}
		<form class="dialog" onsubmit={saveDraft}>
			<h2 id="provider-title">{draft.id ? `Edit ${draft.name}` : `Add ${presetOf(draft)?.label ?? 'a provider'}`}</h2>
			{#if draftError && Object.keys(draftError.errors ?? {}).length === 0}
				<p class="notice danger" role="alert"><Icon name="alert" /><span>{draftError.message}</span></p>
			{/if}
			<div class="grid">
				<label class="field">
					Name
					<input type="text" class="mono" required bind:value={draft.name} aria-invalid={derr('name') ? 'true' : undefined} />
					<span class={derr('name') ? 'error-text' : 'hint'}>{derr('name') ?? 'Part of the URLs. Lowercase.'}</span>
				</label>
				<label class="field">
					Collection
					<select bind:value={draft.collection} aria-invalid={derr('collection') ? 'true' : undefined}>
						{#each authCollections as c (c)}<option value={c}>{c}</option>{/each}
					</select>
					{#if derr('collection')}<span class="error-text">{derr('collection')}</span>{/if}
				</label>
				{#if draft.kind === 'oidc'}
					<label class="field wide">
						Issuer URL
						<span class="with-btn">
							<input type="url" class="mono" required bind:value={draft.issuer} aria-invalid={derr('issuer') ? 'true' : undefined} />
							<button type="button" class="btn sm" onclick={discover}>Check</button>
						</span>
						{#if derr('issuer')}
							<span class="error-text">{derr('issuer')}</span>
						{:else if discovery}
							<span class={discovery.ok ? 'ok-text' : 'error-text'}>{discovery.text}</span>
						{:else if presetOf(draft)?.placeholders}
							<span class="hint">Replace {presetOf(draft)?.placeholders?.map((p) => `{${p}}`).join(', ')}.</span>
						{/if}
					</label>
				{/if}
				<label class="field">
					Client id
					<input type="text" class="mono" required autocomplete="off" bind:value={draft.client_id} aria-invalid={derr('client_id') ? 'true' : undefined} />
				</label>
				<label class="field">
					Client secret
					<input
						type="password"
						autocomplete="new-password"
						value={draft.client_secret === MASK ? '' : draft.client_secret}
						placeholder={draft.client_secret === MASK ? 'stored; type to replace' : ''}
						oninput={(e) => draft && (draft.client_secret = e.currentTarget.value === '' && draft.id ? MASK : e.currentTarget.value)}
					/>
				</label>
				<label class="field wide">Scopes<input type="text" class="mono" bind:value={draft.scopes} /></label>
			</div>
			<div class="callback">
				<span class="eyebrow">Redirect URI to register at the provider</span>
				<code class="mono">{callbackUrl(draft)}</code>
				{#if presetOf(draft)?.docs}<a href={presetOf(draft)?.docs} target="_blank" rel="noreferrer noopener">Provider setup guide</a>{/if}
			</div>
			<label class="inline"><input type="checkbox" bind:checked={draft.enabled} />Enabled</label>
			<label class="inline"><input type="checkbox" bind:checked={draft.auto_link_verified_email} />Link to existing accounts by verified email</label>
			{#if draft.auto_link_verified_email}
				<p class="notice write">
					<Icon name="alert" /><span>Only for providers that verify every email they report. Unverified emails never link, whatever this says.</span>
				</p>
			{/if}
			<div class="actions">
				<button type="button" class="btn" onclick={() => dialog?.close()}>Cancel</button>
				<button type="submit" class="btn primary" disabled={busy}>Save</button>
			</div>
		</form>
	{/if}
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
		gap: 16px;
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
		font-size: 13px;
		overflow-wrap: anywhere;
	}
	.row-actions {
		display: flex;
		gap: 6px;
	}
	.presets {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(160px, 1fr));
		gap: 10px;
	}
	.preset {
		display: grid;
		gap: 2px;
		min-height: 64px;
		padding: 12px 14px;
		text-align: left;
		border: 1px solid var(--border);
		border-radius: var(--radius);
		background: transparent;
		color: var(--fg);
		font: inherit;
		cursor: pointer;
		transition: border-color 0.15s var(--ease-out), background-color 0.15s var(--ease-out);
	}
	.preset:hover {
		border-color: var(--border-strong);
		background: var(--surface);
	}
	.preset strong {
		font-size: 14px;
		font-weight: 500;
	}
	.preset .subtle {
		font-size: 12.5px;
	}
	dialog {
		width: min(640px, calc(100vw - 32px));
	}
	.dialog {
		display: grid;
		gap: 14px;
	}
	.grid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
		gap: 14px;
		align-items: start;
	}
	.wide {
		grid-column: 1 / -1;
	}
	.with-btn {
		display: flex;
		gap: 8px;
		align-items: center;
	}
	.callback {
		display: grid;
		gap: 6px;
		padding: 12px 14px;
		border-radius: var(--radius);
		background: var(--surface);
	}
	.callback code {
		font-size: 13px;
		overflow-wrap: anywhere;
	}
	.callback a {
		font-size: 13px;
	}
	.ok-text {
		font-size: 13px;
		color: var(--accent);
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
		margin-top: 4px;
	}
	.visually-hidden {
		position: absolute;
		width: 1px;
		height: 1px;
		overflow: hidden;
		clip: rect(0 0 0 0);
		white-space: nowrap;
	}
	@media (max-width: 480px) {
		.section {
			padding: 20px;
		}
	}
</style>
