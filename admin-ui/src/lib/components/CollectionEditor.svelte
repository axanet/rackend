<script lang="ts">
	// Create or edit a collection: fields, rules (live-checked), indexes.
	// Changes to an existing collection are previewed with a dry run first.
	import { tick } from 'svelte';
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { ApiError, del, message, patch, post, type AuthOptions, type Collection, type Field, type FieldType, type IndexDef, type RuleOp } from '../api';
	import { session } from '../session.svelte';
	import Icon from './Icon.svelte';
	import RuleInput from './RuleInput.svelte';

	let { collection }: { collection: Collection | null } = $props();

	const TYPES: FieldType[] = ['text', 'email', 'url', 'number', 'bool', 'date', 'autodate', 'select', 'json', 'relation', 'file'];
	const MB = 1024 * 1024;
	const OPS: RuleOp[] = ['list', 'view', 'create', 'update', 'delete'];

	const AUTH_DEFAULTS: AuthOptions = {
		signup_enabled: true,
		email_visibility: false,
		methods: { password: true, passkey: true, otp: true, magic_link: true },
		password: { min_length: 10, require_verified_email_to_login: false },
		session: { access_ttl_s: 900, refresh_ttl_s: 2592000, idle_timeout_s: 1209600 }
	};
	// Shown for a new auth collection; the server adds them on create.
	const AUTH_SYSTEM: Field[] = [
		{ name: 'email', type: 'email', required: true, system: true },
		{ name: 'email_verified', type: 'bool', system: true }
	];

	function authOptions(c: Collection | null): AuthOptions {
		const o: Partial<AuthOptions> = c?.options ?? {};
		return {
			signup_enabled: o.signup_enabled ?? AUTH_DEFAULTS.signup_enabled,
			email_visibility: o.email_visibility ?? AUTH_DEFAULTS.email_visibility,
			methods: { ...AUTH_DEFAULTS.methods, ...$state.snapshot(o.methods ?? {}) },
			password: { ...AUTH_DEFAULTS.password, ...$state.snapshot(o.password ?? {}) },
			session: { ...AUTH_DEFAULTS.session, ...$state.snapshot(o.session ?? {}) }
		};
	}

	function initial(c: Collection | null) {
		return {
			name: c?.name ?? '',
			type: c?.type ?? 'base',
			// Snapshots: the source may be a reactive proxy, which structuredClone rejects.
			// System fields are fixed and shown apart; the server adds them back.
			fields: ($state.snapshot(c?.fields ?? []) as Field[]).filter((f) => !f.system),
			auth: authOptions(c),
			indexes: $state.snapshot(c?.indexes ?? []) as IndexDef[],
			rules: $state.snapshot(c?.rules ?? { list: null, view: null, create: null, update: null, delete: null }) as Record<RuleOp, string | null>,
			audit_records: c?.options.audit_records ?? true
		};
	}

	// svelte-ignore state_referenced_locally
	let draft = $state(initial(collection));
	let saving = $state(false);
	let error = $state<ApiError | null>(null);
	let notice = $state('');
	let noticeWrite = $state(false);
	let preview = $state<{ ddl: string; warnings: string[] } | null>(null);
	let dialog = $state<HTMLDialogElement>();
	let banner = $state<HTMLElement>();

	// Bring save errors into view: the form is long.
	$effect(() => {
		if (error) tick().then(() => banner?.scrollIntoView({ block: 'center', behavior: 'smooth' }));
	});

	let payload = $derived({
		...(collection ? { id: collection.id } : {}),
		name: draft.name,
		type: draft.type,
		fields: draft.fields,
		indexes: draft.indexes,
		rules: draft.rules,
		options: { audit_records: draft.audit_records, ...(draft.type === 'auth' ? draft.auth : {}) }
	});
	let systemFields = $derived(draft.type !== 'auth' ? [] : collection ? collection.fields.filter((f) => f.system) : AUTH_SYSTEM);

	function duration(s: number): string {
		if (!Number.isFinite(s) || s <= 0) return '';
		if (s % 86400 === 0) return `${s / 86400} d`;
		if (s % 3600 === 0) return `${s / 3600} h`;
		if (s % 60 === 0) return `${s / 60} min`;
		return `${s} s`;
	}

	function addField() {
		draft.fields.push({ name: '', type: 'text', required: false });
	}

	function setType(f: Field, type: FieldType) {
		for (const k of Object.keys(f)) {
			if (!['id', 'name'].includes(k)) delete f[k];
		}
		f.type = type;
		if (type === 'select') f.values = [];
		if (type === 'relation') f.collection_id = session.collections[0]?.id ?? '';
		if (type === 'autodate') {
			f.on_create = true;
			f.on_update = false;
		}
		if (type === 'file') {
			f.max_select = 1;
			f.max_size = 10 * MB;
			f.mime_types = [];
			f.thumbs = [];
			f.protected = false;
			f.strip_metadata = true;
		}
	}

	const list = (v: unknown) => (Array.isArray(v) ? v.join(', ') : '');
	const toList = (s: string) =>
		s
			.split(',')
			.map((x) => x.trim())
			.filter(Boolean);
	const num = (s: string) => (s.trim() === '' ? null : Number(s));
	const fieldErr = (path: string) => error?.field(path);

	async function save() {
		saving = true;
		error = null;
		notice = '';
		try {
			if (!collection) {
				const created = await post<Collection>('/api/collections', payload);
				await session.loadCollections();
				await goto(resolve('/collections/[name]', { name: created.name }));
				return;
			}
			const dry = await patch<Collection>(`/api/collections/${collection.id}?dry_run=true`, payload);
			preview = { ddl: dry.ddl ?? '', warnings: dry.warnings ?? [] };
			dialog?.showModal();
		} catch (e) {
			error = e instanceof ApiError ? e : new ApiError(0, { detail: message(e) });
		} finally {
			saving = false;
		}
	}

	async function apply() {
		if (!collection) return;
		saving = true;
		try {
			const updated = await patch<Collection>(`/api/collections/${collection.id}`, payload);
			dialog?.close();
			await session.loadCollections();
			noticeWrite = !!updated.warnings?.length;
			notice = noticeWrite ? (updated.warnings ?? []).join(' ') : 'Saved.';
			if (updated.name !== collection.name) {
				await goto(resolve('/collections/[name]/schema', { name: updated.name }));
			}
		} catch (e) {
			dialog?.close();
			error = e instanceof ApiError ? e : new ApiError(0, { detail: message(e) });
		} finally {
			saving = false;
		}
	}

	async function remove() {
		if (!collection || !confirm(`Move "${collection.name}" to the trash? It can be restored until it is purged.`)) return;
		try {
			await del(`/api/collections/${collection.id}`);
			await session.loadCollections();
			await goto(resolve('/collections'));
		} catch (e) {
			error = e instanceof ApiError ? e : new ApiError(0, { detail: message(e) });
		}
	}
</script>

<form
	class="editor"
	onsubmit={(e) => {
		e.preventDefault();
		save();
	}}
>
	<header class="bar">
		<div class="title">
			<span class="eyebrow">{collection ? 'Schema' : 'New collection'}</span>
			<h1>{collection ? collection.name : 'Create a collection'}</h1>
		</div>
		<div class="actions">
			{#if collection}
				<button type="button" class="btn danger" onclick={remove}><Icon name="trash" />Delete</button>
			{/if}
			<button class="btn primary" disabled={saving}>{collection ? 'Review changes' : 'Create collection'}</button>
		</div>
	</header>

	<div aria-live="polite" bind:this={banner}>
		{#if error}
			<div class="notice danger" role="alert">
				<Icon name="alert" />
				<span>{error.message}{error.code === 'rule_compile_error' && error.position ? ` (line ${error.position.line}, column ${error.position.column})` : ''}</span>
			</div>
		{:else if notice}
			<div class="notice" class:ok={!noticeWrite} class:write={noticeWrite}><Icon name={noticeWrite ? 'alert' : 'check'} /><span>{notice}</span></div>
		{/if}
	</div>

	<section class="card section">
		<div class="card-head"><span class="ico"><Icon name="db" /></span><span class="eyebrow">Collection</span></div>
		<label class="field">
			Name
			<input
				type="text"
				class="mono"
				bind:value={draft.name}
				required
				pattern="[a-z][a-z0-9_]*"
				aria-invalid={fieldErr('name') ? 'true' : undefined}
				aria-describedby="name-hint"
			/>
			<span id="name-hint" class={fieldErr('name') ? 'error-text' : 'hint'}>{fieldErr('name') ?? 'Lowercase letters, digits and _. Also the table name and the URL segment.'}</span>
		</label>
		<fieldset class="kind">
			<legend>Type</legend>
			{#if collection}
				<p class="hint"><span class="tag">{draft.type}</span> The type cannot change after creation.</p>
			{:else}
				<div class="kinds">
					{#each [['base', 'table', 'Base', 'Records with your fields.'], ['auth', 'user', 'Auth', 'Accounts that can sign in: email, password, passkeys, sessions.']] as const as [value, icon, label, text] (value)}
						<label class="kind-option" class:active={draft.type === value}>
							<input type="radio" name="type" {value} aria-label={label} bind:group={draft.type} />
							<span class="ico"><Icon name={icon} /></span>
							<span><strong>{label}</strong><span class="hint">{text}</span></span>
						</label>
					{/each}
				</div>
			{/if}
		</fieldset>
		<label class="inline"><input type="checkbox" bind:checked={draft.audit_records} />Audit record changes</label>
	</section>

	{#if draft.type === 'auth'}
		<section class="card section">
			<div class="card-head"><span class="ico"><Icon name="user" /></span><span class="eyebrow">Sign-in</span></div>
			<div class="auth-grid">
				<fieldset class="group">
					<legend>Methods</legend>
					<label class="inline"><input type="checkbox" bind:checked={draft.auth.methods.password} />Email and password</label>
					<label class="inline"><input type="checkbox" bind:checked={draft.auth.methods.passkey} />Passkeys</label>
					<label class="inline"><input type="checkbox" bind:checked={draft.auth.methods.otp} />Email codes <span class="tag">needs SMTP</span></label>
					<label class="inline"><input type="checkbox" bind:checked={draft.auth.methods.magic_link} />Magic links <span class="tag">needs SMTP</span></label>
				</fieldset>
				<fieldset class="group">
					<legend>Accounts</legend>
					<label class="inline"><input type="checkbox" bind:checked={draft.auth.signup_enabled} />Public sign-up</label>
					<label class="inline"><input type="checkbox" bind:checked={draft.auth.email_visibility} />Others can see emails</label>
					<label class="inline"><input type="checkbox" bind:checked={draft.auth.password.require_verified_email_to_login} />Verified email required to sign in</label>
				</fieldset>
			</div>
			<div class="limits">
				<label class="field">
					Min password length
					<input type="number" min="8" max="256" bind:value={draft.auth.password.min_length} aria-invalid={fieldErr('options.password.min_length') ? 'true' : undefined} />
					<span class={fieldErr('options.password.min_length') ? 'error-text' : 'hint mono'}>{fieldErr('options.password.min_length') ?? '8 to 256'}</span>
				</label>
				{#each [['access_ttl_s', 'Access token'], ['refresh_ttl_s', 'Refresh token'], ['idle_timeout_s', 'Idle timeout']] as [key, label] (key)}
					{@const k = key as keyof AuthOptions['session']}
					<label class="field">
						{label} (s)
						<input type="number" min="60" bind:value={draft.auth.session[k]} aria-invalid={fieldErr(`options.session.${k}`) ? 'true' : undefined} />
						<span class={fieldErr(`options.session.${k}`) ? 'error-text' : 'hint mono'}>{fieldErr(`options.session.${k}`) ?? duration(draft.auth.session[k])}</span>
					</label>
				{/each}
			</div>
			{#each Object.entries(error?.errors ?? {}).filter(([k]) => k.startsWith('options.') && !k.startsWith('options.session.') && k !== 'options.password.min_length') as [k, errs] (k)}
				<p class="error-text">{k.slice('options.'.length)}: {errs[0].message}</p>
			{/each}
		</section>
	{/if}

	<section class="card section">
		<div class="section-head">
			<div class="card-head"><span class="ico"><Icon name="table" /></span><span class="eyebrow">Fields</span></div>
			<button type="button" class="btn sm" onclick={addField}><Icon name="plus" />Add field</button>
		</div>
		<p class="hint">
			Every record also has <code>id</code>, <code>created</code> and <code>updated</code>. Removed fields go to the trash and can be restored.
		</p>
		{#each systemFields as f (f.name)}
			<div class="field-row system">
				<span class="mono">{f.name}</span>
				<span class="tag">{f.type}</span>
				<span class="tag">system</span>
				<span class="hint">{f.name === 'email' ? 'Unique, lowercase. Users cannot change it yet.' : 'Set by superusers; users cannot change it.'}</span>
			</div>
		{/each}
		{#if systemFields.length}
			<p class="hint">The password is stored as an Argon2id hash in a hidden column and never returned.</p>
		{/if}
		{#each draft.fields as f, i (f.id ?? i)}
			<fieldset class="field-row">
				<legend class="sr-only">Field {i + 1}</legend>
				<div class="row-main">
					<label class="field grow">
						Name
						<input type="text" class="mono" bind:value={f.name} required aria-invalid={fieldErr(`fields.${i}.name`) ? 'true' : undefined} />
						{#if fieldErr(`fields.${i}.name`)}<span class="error-text">{fieldErr(`fields.${i}.name`)}</span>{/if}
					</label>
					<label class="field type">
						Type
						<select value={f.type} onchange={(e) => setType(f, e.currentTarget.value as FieldType)}>
							{#each TYPES as t (t)}<option value={t}>{t}</option>{/each}
						</select>
						{#if fieldErr(`fields.${i}.type`)}<span class="error-text">{fieldErr(`fields.${i}.type`)}</span>{/if}
					</label>
					{#if !['bool', 'autodate', 'json'].includes(f.type)}
						<label class="inline"><input type="checkbox" bind:checked={f.required} />Required</label>
					{/if}
					<button type="button" class="btn sm icon ghost" aria-label="Remove field {f.name || i + 1}" onclick={() => draft.fields.splice(i, 1)}>
						<Icon name="x" />
					</button>
				</div>
				<div class="options">
					{#if f.type === 'text'}
						<label class="field">Min chars<input type="number" min="0" value={f.min ?? ''} oninput={(e) => (f.min = num(e.currentTarget.value))} /></label>
						<label class="field">Max chars<input type="number" min="1" value={f.max ?? ''} placeholder="10000" oninput={(e) => (f.max = num(e.currentTarget.value))} /></label>
						<label class="field grow">Pattern (regex)<input type="text" class="mono" value={f.pattern ?? ''} oninput={(e) => (f.pattern = e.currentTarget.value || null)} /></label>
					{:else if f.type === 'email'}
						<label class="field grow">Only domains<input type="text" class="mono" value={list(f.only_domains)} oninput={(e) => (f.only_domains = toList(e.currentTarget.value))} placeholder="example.com" /></label>
						<label class="field grow">Except domains<input type="text" class="mono" value={list(f.except_domains)} oninput={(e) => (f.except_domains = toList(e.currentTarget.value))} /></label>
					{:else if f.type === 'url'}
						<label class="field grow">Schemes<input type="text" class="mono" value={list(f.schemes)} oninput={(e) => (f.schemes = toList(e.currentTarget.value))} placeholder="https, http" /></label>
					{:else if f.type === 'number'}
						<label class="field">Min<input type="number" step="any" value={f.min ?? ''} oninput={(e) => (f.min = num(e.currentTarget.value))} /></label>
						<label class="field">Max<input type="number" step="any" value={f.max ?? ''} oninput={(e) => (f.max = num(e.currentTarget.value))} /></label>
						<label class="inline"><input type="checkbox" checked={!!f.integer_only} onchange={(e) => (f.integer_only = e.currentTarget.checked)} />Whole numbers only</label>
					{:else if f.type === 'date'}
						<label class="field">Min<input type="text" class="mono" value={f.min ?? ''} placeholder="2026-01-01" oninput={(e) => (f.min = e.currentTarget.value || null)} /></label>
						<label class="field">Max<input type="text" class="mono" value={f.max ?? ''} oninput={(e) => (f.max = e.currentTarget.value || null)} /></label>
					{:else if f.type === 'autodate'}
						<label class="inline"><input type="checkbox" checked={f.on_create !== false} onchange={(e) => (f.on_create = e.currentTarget.checked)} />On create</label>
						<label class="inline"><input type="checkbox" checked={!!f.on_update} onchange={(e) => (f.on_update = e.currentTarget.checked)} />On update</label>
					{:else if f.type === 'select'}
						<label class="field grow">Values<input type="text" class="mono" value={list(f.values)} oninput={(e) => (f.values = toList(e.currentTarget.value))} placeholder="draft, published" aria-invalid={fieldErr(`fields.${i}.values`) ? 'true' : undefined} /></label>
						<label class="field">Max selected<input type="number" min="1" value={f.max_select ?? 1} oninput={(e) => (f.max_select = num(e.currentTarget.value))} /></label>
					{:else if f.type === 'json'}
						<label class="field">Max bytes<input type="number" min="1" value={f.max_size ?? ''} placeholder="1048576" oninput={(e) => (f.max_size = num(e.currentTarget.value))} /></label>
					{:else if f.type === 'file'}
						<label class="field">Max files<input type="number" min="1" value={f.max_select ?? 1} oninput={(e) => (f.max_select = num(e.currentTarget.value))} /></label>
						<label class="field">
							Max size (MB)
							<input
								type="number"
								min="0.01"
								step="any"
								value={typeof f.max_size === 'number' ? Math.round((f.max_size / MB) * 100) / 100 : 10}
								oninput={(e) => {
									const v = num(e.currentTarget.value);
									f.max_size = v === null ? null : Math.max(1, Math.round(v * MB));
								}}
							/>
						</label>
						<label class="field grow">Allowed types<input type="text" class="mono" value={list(f.mime_types)} oninput={(e) => (f.mime_types = toList(e.currentTarget.value))} placeholder="any; e.g. image/*" /></label>
						<label class="field grow" title="100x100 is always allowed">Thumb sizes<input type="text" class="mono" value={list(f.thumbs)} oninput={(e) => (f.thumbs = toList(e.currentTarget.value))} placeholder="e.g. 300x0, 64x64f" /></label>
						<label class="inline" title="Downloads need a short-lived file token, so links cannot be shared"><input type="checkbox" checked={!!f.protected} onchange={(e) => (f.protected = e.currentTarget.checked)} />Protected</label>
						<label class="inline" title="Removes EXIF, GPS and other metadata from JPEG, PNG and WebP uploads"><input type="checkbox" checked={f.strip_metadata !== false} onchange={(e) => (f.strip_metadata = e.currentTarget.checked)} />Strip metadata</label>
					{:else if f.type === 'relation'}
						<label class="field grow">
							Collection
							<select bind:value={f.collection_id}>
								{#each session.collections as c (c.id)}<option value={c.id}>{c.name}</option>{/each}
								{#if collection === null || !session.collections.some((c) => c.id === collection?.id)}<option value="" disabled>(save first to relate to itself)</option>{/if}
							</select>
						</label>
						<label class="field">Max selected<input type="number" min="1" value={f.max_select ?? 1} oninput={(e) => (f.max_select = num(e.currentTarget.value))} /></label>
						<label class="field">
							On delete
							<select value={f.on_delete ?? 'set_null'} onchange={(e) => (f.on_delete = e.currentTarget.value)}>
								<option value="set_null">Set null</option>
								<option value="cascade">Cascade</option>
								<option value="restrict">Restrict</option>
							</select>
						</label>
					{/if}
				</div>
				{#each Object.entries(error?.errors ?? {}).filter(([k]) => k.startsWith(`fields.${i}.`) && !k.endsWith('.name') && !k.endsWith('.type')) as [k, errs] (k)}
					<p class="error-text">{k.split('.').pop()}: {errs[0].message}</p>
				{/each}
			</fieldset>
		{:else}
			{#if !systemFields.length}
				<p class="placeholder">No fields yet. Records will only have <code>id</code>, <code>created</code> and <code>updated</code>.</p>
			{/if}
		{/each}
	</section>

	<section class="card section">
		<div class="card-head"><span class="ico"><Icon name="lock" /></span><span class="eyebrow">API rules</span></div>
		<p class="hint">
			Locked by default; only superusers can read or write. Rules use the filter language, e.g. <code>owner = @request.auth.id</code>. On multi-valued
			fields, plain operators require <em>all</em> values to match and <code>?=</code>-style operators <em>at least one</em>.
		</p>
		<div class="rules">
			{#each OPS as op (op)}
				<RuleInput id="rule-{op}" label={op} operation={op} collection={payload} bind:value={draft.rules[op]} />
			{/each}
		</div>
	</section>

	<section class="card section">
		<div class="section-head">
			<div class="card-head"><span class="ico"><Icon name="layout" /></span><span class="eyebrow">Indexes</span></div>
			<button type="button" class="btn sm" onclick={() => draft.indexes.push({ name: '', fields: [], unique: false })}><Icon name="plus" />Add index</button>
		</div>
		{#each draft.indexes as idx, i (i)}
			<div class="row-main">
				<label class="field">Name<input type="text" class="mono" bind:value={idx.name} required /></label>
				<label class="field grow">Fields<input type="text" class="mono" value={list(idx.fields)} oninput={(e) => (idx.fields = toList(e.currentTarget.value))} placeholder="title, created" /></label>
				<label class="inline"><input type="checkbox" bind:checked={idx.unique} />Unique</label>
				<button type="button" class="btn sm icon ghost" aria-label="Remove index {idx.name || i + 1}" onclick={() => draft.indexes.splice(i, 1)}><Icon name="x" /></button>
			</div>
			{#if fieldErr(`indexes.${i}.fields`)}<p class="error-text">{fieldErr(`indexes.${i}.fields`)}</p>{/if}
		{:else}
			<p class="placeholder">No indexes. The primary key on <code>id</code> always exists.</p>
		{/each}
	</section>
</form>

<dialog bind:this={dialog} aria-labelledby="preview-title">
	<div class="dialog">
		<div class="dialog-head">
			<h2 id="preview-title">Apply these changes?</h2>
			<span class="tag">dry run</span>
		</div>
		{#if preview}
			{#each preview.warnings as w (w)}
				<p class="notice write"><Icon name="alert" /><span>{w}</span></p>
			{/each}
			<p class="hint">SQL that will run in one transaction, together with the schema history and audit entries:</p>
			<pre class="well ddl">{preview.ddl || '-- no table changes (rules or options only)'}</pre>
		{/if}
		<div class="actions">
			<button type="button" class="btn" onclick={() => dialog?.close()}>Cancel</button>
			<button type="button" class="btn primary" disabled={saving} onclick={apply}>Apply</button>
		</div>
	</div>
</dialog>

<style>
	.editor {
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
		justify-content: flex-end;
		flex-wrap: wrap;
	}
	.section {
		padding: 24px;
		display: grid;
		gap: 14px;
	}
	.section:hover {
		border-color: var(--border-strong);
	}
	.section-head {
		display: flex;
		justify-content: space-between;
		align-items: center;
		gap: 12px;
	}
	.field-row {
		margin: 0;
		padding: 16px;
		display: grid;
		gap: 12px;
		border: 1px solid var(--border);
		border-radius: var(--radius);
		background: var(--surface);
	}
	.field-row input,
	.field-row select {
		background: var(--bg-elev);
	}
	.row-main,
	.options {
		display: flex;
		gap: 12px;
		align-items: flex-end;
		flex-wrap: wrap;
	}
	.options:empty {
		display: none;
	}
	.grow {
		flex: 1 1 200px;
	}
	.type {
		flex: 0 0 150px;
	}
	.inline {
		display: inline-flex;
		align-items: center;
		gap: 8px;
		height: 40px;
		font-size: 14px;
		cursor: pointer;
	}
	.rules {
		display: grid;
		gap: 16px;
	}
	.kind,
	.group {
		margin: 0;
		padding: 0;
		border: 0;
		display: grid;
		gap: 8px;
		min-width: 0;
	}
	.kind legend,
	.group legend {
		padding: 0;
		margin-bottom: 8px;
		font-size: 14px;
		font-weight: 500;
	}
	.kinds {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
		gap: 12px;
	}
	.kind-option {
		position: relative;
		display: flex;
		gap: 12px;
		align-items: flex-start;
		padding: 14px;
		border: 1px solid var(--border);
		border-radius: var(--radius);
		background: transparent;
		cursor: pointer;
		transition:
			border-color 0.15s var(--ease-out),
			background-color 0.15s var(--ease-out);
	}
	.kind-option:hover {
		border-color: var(--border-strong);
	}
	.kind-option.active {
		border-color: var(--accent);
		background: var(--surface);
	}
	.kind-option.active .ico {
		color: var(--accent-text);
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
	.auth-grid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(260px, 1fr));
		gap: 16px;
	}
	.limits {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(170px, 1fr));
		gap: 12px;
		align-items: start;
	}
	.auth-grid .inline {
		height: 32px;
	}
	.field-row.system {
		display: flex;
		align-items: center;
		gap: 10px;
		flex-wrap: wrap;
		padding: 10px 16px;
	}
	.field-row.system .mono {
		font-size: 14px;
	}
	dialog {
		width: min(720px, calc(100vw - 32px));
	}
	.dialog {
		display: grid;
		gap: 14px;
	}
	.dialog-head {
		display: flex;
		align-items: center;
		gap: 10px;
	}
	.ddl {
		max-height: 320px;
		white-space: pre-wrap;
	}
	@media (max-width: 480px) {
		.section {
			padding: 20px;
		}
		.type {
			flex: 1 1 140px;
		}
	}
</style>
