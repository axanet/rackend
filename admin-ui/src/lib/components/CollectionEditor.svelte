<script lang="ts">
	// Create or edit a collection: fields, rules (live-checked), indexes.
	// Changes to an existing collection are previewed with a dry run first.
	import { tick } from 'svelte';
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { ApiError, del, message, patch, post, type Collection, type Field, type FieldType, type IndexDef, type RuleOp } from '../api';
	import { session } from '../session.svelte';
	import Icon from './Icon.svelte';
	import RuleInput from './RuleInput.svelte';

	let { collection }: { collection: Collection | null } = $props();

	const TYPES: FieldType[] = ['text', 'email', 'url', 'number', 'bool', 'date', 'autodate', 'select', 'json', 'relation'];
	const OPS: RuleOp[] = ['list', 'view', 'create', 'update', 'delete'];

	function initial(c: Collection | null) {
		return {
			name: c?.name ?? '',
			// Snapshots: the source may be a reactive proxy, which structuredClone rejects.
			fields: $state.snapshot(c?.fields ?? []) as Field[],
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
		type: 'base',
		fields: draft.fields,
		indexes: draft.indexes,
		rules: draft.rules,
		options: { audit_records: draft.audit_records }
	});

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
			notice = updated.warnings?.length ? updated.warnings.join(' ') : 'Saved.';
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
		<h1>{collection ? `Schema of ${collection.name}` : 'New collection'}</h1>
		<div class="actions">
			{#if collection}
				<button type="button" class="btn danger" onclick={remove}><Icon name="trash" />Delete</button>
			{/if}
			<button class="btn primary" disabled={saving}>{collection ? 'Review changes' : 'Create collection'}</button>
		</div>
	</header>

	<div aria-live="polite" bind:this={banner}>
		{#if error}
			<div class="banner error" role="alert">
				<Icon name="alert" />
				<span>{error.message}{#if error.code === 'rule_compile_error' && error.position} (line {error.position.line}, column {error.position.column}){/if}</span>
			</div>
		{:else if notice}
			<div class="banner ok"><Icon name="check" /><span>{notice}</span></div>
		{/if}
	</div>

	<section class="card section">
		<label class="field">
			Name
			<input
				type="text"
				bind:value={draft.name}
				required
				pattern="[a-z][a-z0-9_]*"
				aria-invalid={fieldErr('name') ? 'true' : undefined}
				aria-describedby="name-hint"
			/>
			<span id="name-hint" class={fieldErr('name') ? 'error-text' : 'hint'}>{fieldErr('name') ?? 'Lowercase letters, digits and _. Also the table name and the URL segment.'}</span>
		</label>
		<label class="inline"><input type="checkbox" bind:checked={draft.audit_records} /> Audit record changes</label>
	</section>

	<section class="card section">
		<div class="section-head">
			<h2>Fields</h2>
			<button type="button" class="btn" onclick={addField}><Icon name="plus" />Add field</button>
		</div>
		<p class="hint">Every record also has <code>id</code>, <code>created</code> and <code>updated</code>. Removed fields go to the trash and can be restored.</p>
		{#each draft.fields as f, i (f.id ?? i)}
			<fieldset class="field-row">
				<legend class="sr-only">Field {i + 1}</legend>
				<div class="row-main">
					<label class="field grow">
						Name
						<input type="text" bind:value={f.name} required aria-invalid={fieldErr(`fields.${i}.name`) ? 'true' : undefined} />
						{#if fieldErr(`fields.${i}.name`)}<span class="error-text">{fieldErr(`fields.${i}.name`)}</span>{/if}
					</label>
					<label class="field">
						Type
						<select value={f.type} onchange={(e) => setType(f, e.currentTarget.value as FieldType)}>
							{#each TYPES as t (t)}<option value={t}>{t}</option>{/each}
						</select>
						{#if fieldErr(`fields.${i}.type`)}<span class="error-text">{fieldErr(`fields.${i}.type`)}</span>{/if}
					</label>
					{#if !['bool', 'autodate', 'json'].includes(f.type)}
						<label class="inline req"><input type="checkbox" bind:checked={f.required} /> Required</label>
					{/if}
					<button type="button" class="btn icon ghost" aria-label="Remove field {f.name || i + 1}" onclick={() => draft.fields.splice(i, 1)}>
						<Icon name="x" />
					</button>
				</div>
				<div class="options">
					{#if f.type === 'text'}
						<label class="field">Min chars<input type="number" min="0" value={f.min ?? ''} oninput={(e) => (f.min = num(e.currentTarget.value))} /></label>
						<label class="field">Max chars<input type="number" min="1" value={f.max ?? ''} placeholder="10000" oninput={(e) => (f.max = num(e.currentTarget.value))} /></label>
						<label class="field grow">Pattern (regex)<input type="text" class="mono" value={f.pattern ?? ''} oninput={(e) => (f.pattern = e.currentTarget.value || null)} /></label>
					{:else if f.type === 'email'}
						<label class="field grow">Only domains<input type="text" value={list(f.only_domains)} oninput={(e) => (f.only_domains = toList(e.currentTarget.value))} placeholder="example.com, …" /></label>
						<label class="field grow">Except domains<input type="text" value={list(f.except_domains)} oninput={(e) => (f.except_domains = toList(e.currentTarget.value))} /></label>
					{:else if f.type === 'url'}
						<label class="field grow">Schemes<input type="text" value={list(f.schemes)} oninput={(e) => (f.schemes = toList(e.currentTarget.value))} placeholder="https, http" /></label>
					{:else if f.type === 'number'}
						<label class="field">Min<input type="number" step="any" value={f.min ?? ''} oninput={(e) => (f.min = num(e.currentTarget.value))} /></label>
						<label class="field">Max<input type="number" step="any" value={f.max ?? ''} oninput={(e) => (f.max = num(e.currentTarget.value))} /></label>
						<label class="inline"><input type="checkbox" checked={!!f.integer_only} onchange={(e) => (f.integer_only = e.currentTarget.checked)} /> Whole numbers only</label>
					{:else if f.type === 'date'}
						<label class="field">Min<input type="text" class="mono" value={f.min ?? ''} placeholder="2026-01-01" oninput={(e) => (f.min = e.currentTarget.value || null)} /></label>
						<label class="field">Max<input type="text" class="mono" value={f.max ?? ''} oninput={(e) => (f.max = e.currentTarget.value || null)} /></label>
					{:else if f.type === 'autodate'}
						<label class="inline"><input type="checkbox" checked={f.on_create !== false} onchange={(e) => (f.on_create = e.currentTarget.checked)} /> On create</label>
						<label class="inline"><input type="checkbox" checked={!!f.on_update} onchange={(e) => (f.on_update = e.currentTarget.checked)} /> On update</label>
					{:else if f.type === 'select'}
						<label class="field grow">Values<input type="text" value={list(f.values)} oninput={(e) => (f.values = toList(e.currentTarget.value))} placeholder="draft, published" aria-invalid={fieldErr(`fields.${i}.values`) ? 'true' : undefined} /></label>
						<label class="field">Max selected<input type="number" min="1" value={f.max_select ?? 1} oninput={(e) => (f.max_select = num(e.currentTarget.value))} /></label>
					{:else if f.type === 'json'}
						<label class="field">Max bytes<input type="number" min="1" value={f.max_size ?? ''} placeholder="1048576" oninput={(e) => (f.max_size = num(e.currentTarget.value))} /></label>
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
			<p class="empty">No fields yet.</p>
		{/each}
	</section>

	<section class="card section">
		<h2>API rules</h2>
		<p class="hint">
			<strong>Superusers only</strong> is the default. Rules use the filter language, e.g. <code>owner = @request.auth.id</code>. On
			multi-valued fields, plain operators require <em>all</em> values to match and <code>?=</code>-style operators <em>at least one</em>.
		</p>
		<div class="rules">
			{#each OPS as op (op)}
				<RuleInput id="rule-{op}" label={op} operation={op} collection={payload} bind:value={draft.rules[op]} />
			{/each}
		</div>
	</section>

	<section class="card section">
		<div class="section-head">
			<h2>Indexes</h2>
			<button type="button" class="btn" onclick={() => draft.indexes.push({ name: '', fields: [], unique: false })}><Icon name="plus" />Add index</button>
		</div>
		{#each draft.indexes as idx, i (i)}
			<div class="row-main">
				<label class="field">Name<input type="text" bind:value={idx.name} required /></label>
				<label class="field grow">Fields<input type="text" value={list(idx.fields)} oninput={(e) => (idx.fields = toList(e.currentTarget.value))} placeholder="title, created" /></label>
				<label class="inline req"><input type="checkbox" bind:checked={idx.unique} /> Unique</label>
				<button type="button" class="btn icon ghost" aria-label="Remove index {idx.name || i + 1}" onclick={() => draft.indexes.splice(i, 1)}><Icon name="x" /></button>
			</div>
			{#if fieldErr(`indexes.${i}.fields`)}<p class="error-text">{fieldErr(`indexes.${i}.fields`)}</p>{/if}
		{:else}
			<p class="empty">No indexes.</p>
		{/each}
	</section>
</form>

<dialog bind:this={dialog} aria-labelledby="preview-title" class="card">
	<h2 id="preview-title">Apply these changes?</h2>
	{#if preview}
		{#if preview.warnings.length}
			<ul class="warnings">{#each preview.warnings as w (w)}<li>{w}</li>{/each}</ul>
		{/if}
		<p class="hint">SQL that will run in one transaction:</p>
		<pre class="ddl">{preview.ddl || '-- no table changes (rules or options only)'}</pre>
	{/if}
	<div class="actions">
		<button type="button" class="btn" onclick={() => dialog?.close()}>Cancel</button>
		<button type="button" class="btn primary" disabled={saving} onclick={apply}>Apply</button>
	</div>
</dialog>

<style>
	.editor {
		display: grid;
		gap: var(--space-4);
		max-width: 980px;
	}
	.bar {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--space-3);
	}
	.actions {
		display: flex;
		gap: var(--space-2);
		justify-content: flex-end;
	}
	.section {
		padding: var(--space-4);
		display: grid;
		gap: var(--space-3);
	}
	.section-head {
		display: flex;
		justify-content: space-between;
		align-items: center;
	}
	.hint {
		margin: 0;
	}
	.field-row {
		border: 1px solid var(--color-border);
		border-radius: var(--radius);
		padding: var(--space-3);
		margin: 0;
		display: grid;
		gap: var(--space-2);
	}
	.row-main,
	.options {
		display: flex;
		gap: var(--space-3);
		align-items: flex-end;
		flex-wrap: wrap;
	}
	.options:empty {
		display: none;
	}
	.grow {
		flex: 1 1 200px;
	}
	.inline {
		display: inline-flex;
		align-items: center;
		gap: var(--space-2);
		font-size: 0.875rem;
		min-height: 36px;
		cursor: pointer;
	}
	.rules {
		display: grid;
		gap: var(--space-4);
	}
	.empty {
		margin: 0;
		color: var(--color-muted-foreground);
	}
	.banner {
		display: flex;
		gap: var(--space-2);
		align-items: flex-start;
		padding: var(--space-3);
		border-radius: var(--radius);
		border: 1px solid;
	}
	.banner.error {
		color: var(--color-destructive);
		border-color: var(--color-destructive);
	}
	.banner.ok {
		color: var(--color-accent);
		border-color: var(--color-accent);
	}
	dialog {
		width: min(720px, calc(100vw - 32px));
		padding: var(--space-5);
		color: var(--color-foreground);
	}
	dialog::backdrop {
		background: rgb(0 0 0 / 0.55);
	}
	dialog h2 {
		margin-bottom: var(--space-3);
	}
	.ddl {
		background: var(--color-code-bg);
		padding: var(--space-3);
		border-radius: var(--radius-sm);
		overflow: auto;
		max-height: 320px;
		white-space: pre-wrap;
	}
	.warnings {
		color: var(--color-warning);
		padding-left: var(--space-5);
	}
</style>
