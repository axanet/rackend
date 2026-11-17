<script lang="ts">
	// Settings: file storage backend (filesystem or S3), a connection test
	// that saves nothing, and usage. Secrets come back masked; sending the
	// mask keeps the stored secret.
	import { ApiError, formatBytes, get, message, patch, post } from '#lib/api.ts';
	import Icon from '#lib/components/Icon.svelte';

	const MASK = '••••';

	interface S3 {
		endpoint: string;
		region: string;
		bucket: string;
		prefix: string;
		access_key_id: string;
		secret_access_key: string | null;
		force_path_style: boolean;
		allow_http: boolean;
		redirect_downloads: boolean;
	}
	interface Storage {
		backend: 'fs' | 's3';
		s3: S3;
		thumb_cache_max_mb: number;
	}
	interface Step {
		step: string;
		ok: boolean;
		ms?: number;
		error?: string;
	}

	let form = $state<Storage | null>(null);
	let saved = $state<string>('');
	let usage = $state<{ files: number; bytes: number; backend: string } | null>(null);
	let error = $state<ApiError | null>(null);
	let notice = $state('');
	let busy = $state(false);
	let test = $state<{ ok: boolean; backend: string; steps: Step[] } | null>(null);
	let confirmDialog = $state<HTMLDialogElement>();
	let conflict = $state('');

	async function load() {
		try {
			const s = await get<{ storage: Storage }>('/api/settings');
			form = s.storage;
			saved = JSON.stringify(s.storage);
			usage = await get('/api/files/usage');
		} catch (e) {
			error = e instanceof ApiError ? e : new ApiError(0, { detail: message(e) });
		}
	}
	load();

	const dirty = $derived(form !== null && JSON.stringify(form) !== saved);
	const secretStored = $derived(form?.s3.secret_access_key === MASK);

	async function runTest() {
		if (!form) return;
		busy = true;
		test = null;
		error = null;
		try {
			test = await post('/api/settings/test-storage', $state.snapshot(form));
		} catch (e) {
			error = e instanceof ApiError ? e : new ApiError(0, { detail: message(e) });
		} finally {
			busy = false;
		}
	}

	async function save(force = false) {
		if (!form) return;
		busy = true;
		error = null;
		notice = '';
		try {
			const s = await patch<{ storage: Storage }>(`/api/settings${force ? '?force=true' : ''}`, { storage: $state.snapshot(form) });
			form = s.storage;
			saved = JSON.stringify(s.storage);
			notice = 'Saved. New uploads use these settings.';
			confirmDialog?.close();
			usage = await get('/api/files/usage');
		} catch (e) {
			if (e instanceof ApiError && e.code === 'conflict' && !force) {
				conflict = e.message;
				confirmDialog?.showModal();
			} else {
				error = e instanceof ApiError ? e : new ApiError(0, { detail: message(e) });
			}
		} finally {
			busy = false;
		}
	}

	const field = (path: string) => error?.field(path);
</script>

<div class="page">
	<header class="bar">
		<div class="title">
			<span class="eyebrow">Settings</span>
			<h1>File storage</h1>
		</div>
		<div class="actions">
			<button type="button" class="btn" disabled={busy || !form} onclick={runTest}><Icon name="refresh" />Test connection</button>
			<button type="button" class="btn primary" disabled={busy || !dirty} onclick={() => save()}>Save</button>
		</div>
	</header>

	<div aria-live="polite">
		{#if error}
			<p class="notice danger" role="alert"><Icon name="alert" /><span>{error.message}</span></p>
		{:else if notice}
			<p class="notice ok"><Icon name="check" /><span>{notice}</span></p>
		{/if}
	</div>

	{#if usage}
		<section class="card stats" aria-label="Usage">
			<div><span class="eyebrow">Files</span><span class="num mono">{usage.files}</span></div>
			<div><span class="eyebrow">Stored</span><span class="num mono">{formatBytes(usage.bytes)}</span></div>
			<div><span class="eyebrow">Backend in use</span><span class="num mono">{usage.backend}</span></div>
		</section>
	{/if}

	{#if form}
		<section class="card section">
			<div class="card-head"><span class="ico"><Icon name="db" /></span><span class="eyebrow">Backend</span></div>
			<div class="kinds" role="radiogroup" aria-label="Backend">
				{#each [['fs', 'Filesystem', 'Files live in storage/ inside the data directory. Back it up with the database.'], ['s3', 'S3-compatible', 'AWS S3, RustFS, Garage, Cloudflare R2 and others.']] as const as [value, label, text] (value)}
					<label class="kind-option" class:active={form.backend === value}>
						<input type="radio" name="backend" {value} aria-label={label} bind:group={form.backend} />
						<span><strong>{label}</strong><span class="hint">{text}</span></span>
					</label>
				{/each}
			</div>
			{#if usage && usage.files > 0 && form.backend !== usage.backend}
				<p class="notice write"><Icon name="alert" /><span>{usage.files === 1 ? '1 file is' : `${usage.files} files are`} stored in {usage.backend}. Switching does not move them; copy them to the new backend yourself first.</span></p>
			{/if}
		</section>

		{#if form.backend === 's3'}
			<section class="card section">
				<div class="card-head"><span class="ico"><Icon name="lock" /></span><span class="eyebrow">S3 connection</span></div>
				<div class="grid">
					<label class="field wide">
						Endpoint
						<input type="url" class="mono" bind:value={form.s3.endpoint} placeholder="https://s3.eu-central-1.amazonaws.com" aria-invalid={field('storage.s3.endpoint') ? 'true' : undefined} />
						<span class={field('storage.s3.endpoint') ? 'error-text' : 'hint'}>{field('storage.s3.endpoint') ?? 'Leave empty for AWS S3 in the region below.'}</span>
					</label>
					<label class="field">Region<input type="text" class="mono" bind:value={form.s3.region} /></label>
					<label class="field">Bucket<input type="text" class="mono" bind:value={form.s3.bucket} /></label>
					<label class="field">
						Key prefix
						<input type="text" class="mono" bind:value={form.s3.prefix} placeholder="optional, e.g. rackend/prod" aria-invalid={field('storage.s3.prefix') ? 'true' : undefined} />
						{#if field('storage.s3.prefix')}<span class="error-text">{field('storage.s3.prefix')}</span>{/if}
					</label>
					<label class="field">Access key id<input type="text" class="mono" autocomplete="off" bind:value={form.s3.access_key_id} /></label>
					<label class="field">
						Secret access key
						<input
							type="password"
							autocomplete="new-password"
							value={secretStored ? '' : (form.s3.secret_access_key ?? '')}
							placeholder={secretStored ? 'stored; type to replace' : ''}
							oninput={(e) => form && (form.s3.secret_access_key = e.currentTarget.value === '' && secretStored ? MASK : e.currentTarget.value)}
						/>
						<span class="hint">Encrypted at rest; never shown again.</span>
					</label>
				</div>
				<div class="checks">
					<label class="inline"><input type="checkbox" bind:checked={form.s3.force_path_style} />Path-style addressing (RustFS, Garage)</label>
					<label class="inline"><input type="checkbox" bind:checked={form.s3.redirect_downloads} />Redirect downloads to signed URLs</label>
					<label class="inline"><input type="checkbox" bind:checked={form.s3.allow_http} />Allow plain http (development only)</label>
				</div>
			</section>
		{/if}

		<section class="card section">
			<div class="card-head"><span class="ico"><Icon name="layout" /></span><span class="eyebrow">Thumbnails</span></div>
			<label class="field narrow">
				Cache limit (MB)
				<input type="number" min="0" bind:value={form.thumb_cache_max_mb} />
				<span class="hint">The oldest thumbnails are removed daily above this size. They are made again on demand.</span>
			</label>
		</section>

		{#if test}
			<section class="card section" aria-live="polite">
				<div class="card-head">
					<span class="ico"><Icon name={test.ok ? 'check' : 'alert'} /></span><span class="eyebrow">Connection test, {test.backend}</span>
				</div>
				<ol class="steps">
					{#each test.steps as s (s.step)}
						<li class:fail={!s.ok}>
							<span class="mono step">{s.step}</span>
							<span class="tag">{s.ok ? 'ok' : 'failed'}</span>
							{#if s.ms !== undefined}<span class="subtle mono">{s.ms} ms</span>{/if}
							{#if s.error}<span class="error-text">{s.error}</span>{/if}
						</li>
					{/each}
				</ol>
				<p class="hint">{test.ok ? 'Put, head, get and delete worked. Nothing was saved.' : 'Fix the settings and test again. Nothing was saved.'}</p>
			</section>
		{/if}
	{/if}
</div>

<dialog bind:this={confirmDialog} aria-labelledby="switch-title">
	<div class="dialog">
		<h2 id="switch-title">Switch the storage backend?</h2>
		<p>{conflict}</p>
		<p class="hint">Records keep pointing at their files; downloads fail until the files exist in the new backend.</p>
		<div class="actions">
			<button type="button" class="btn" onclick={() => confirmDialog?.close()}>Cancel</button>
			<button type="button" class="btn danger" disabled={busy} onclick={() => save(true)}>Switch anyway</button>
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
	.actions {
		display: flex;
		gap: 8px;
		flex-wrap: wrap;
		justify-content: flex-end;
	}
	.section {
		padding: 24px;
		display: grid;
		gap: 14px;
	}
	.stats {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(160px, 1fr));
		gap: 16px;
		padding: 20px 24px;
	}
	.stats div {
		display: grid;
		gap: 6px;
	}
	.num {
		font-size: 20px;
		font-weight: 500;
		letter-spacing: -0.02em;
	}
	.kinds {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
		gap: 12px;
	}
	.kind-option {
		position: relative;
		display: flex;
		gap: 12px;
		padding: 14px;
		border: 1px solid var(--border);
		border-radius: var(--radius);
		cursor: pointer;
		transition: border-color 0.15s var(--ease-out), background-color 0.15s var(--ease-out);
	}
	.kind-option:hover {
		border-color: var(--border-strong);
	}
	.kind-option.active {
		border-color: var(--accent);
		background: var(--surface);
	}
	.kind-option:has(input:focus-visible) {
		outline: 2px solid var(--accent);
		outline-offset: 2px;
	}
	.kind-option input {
		position: absolute;
		opacity: 0;
		pointer-events: none;
	}
	.kind-option strong {
		display: block;
		font-size: 14px;
		font-weight: 500;
	}
	.kind-option .hint {
		font-size: 13px;
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
	.narrow {
		max-width: 320px;
	}
	.checks {
		display: grid;
		gap: 4px;
	}
	.inline {
		display: inline-flex;
		align-items: center;
		gap: 8px;
		min-height: 32px;
		font-size: 14px;
		cursor: pointer;
	}
	.steps {
		display: grid;
		gap: 6px;
		margin: 0;
		padding: 0;
		list-style: none;
	}
	.steps li {
		display: flex;
		align-items: center;
		gap: 10px;
		flex-wrap: wrap;
		font-size: 14px;
	}
	.step {
		width: 56px;
	}
	.steps li.fail .tag {
		color: var(--danger);
		border-color: var(--danger);
	}
	dialog {
		width: min(520px, calc(100vw - 32px));
	}
	.dialog {
		display: grid;
		gap: 12px;
	}
	.dialog .actions {
		margin-top: 8px;
	}
	@media (max-width: 480px) {
		.section {
			padding: 20px;
		}
	}
</style>
