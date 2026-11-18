<script lang="ts">
	// Create/edit one record in a modal dialog. Field inputs follow the field
	// type; server validation errors appear next to their field.
	import { ApiError, fileUrl, formatBytes, formBody, getFileToken, isImageName, message, patch, post, type Collection, type Field, type RecordRow } from '../api';
	import Icon from './Icon.svelte';

	let {
		collection,
		record,
		onclose,
		onsaved
	}: { collection: Collection; record: RecordRow | null; onclose: () => void; onsaved: () => void } = $props();

	const multi = (f: Field) => ((f.max_select as number | undefined) ?? 1) > 1;
	const editable = (f: Field) => f.type !== 'autodate' && f.type !== 'file' && f.type !== 'ydoc';
	const names = (v: unknown): string[] => (Array.isArray(v) ? (v as string[]) : typeof v === 'string' && v ? [v] : []);

	function toInput(f: Field, v: unknown): unknown {
		if (f.type === 'json') return v === null || v === undefined ? '' : JSON.stringify(v, null, 2);
		if (f.type === 'relation') return Array.isArray(v) ? v.join(', ') : (v ?? '');
		if (f.type === 'bool') return !!v;
		if (f.type === 'select' && multi(f)) return Array.isArray(v) ? [...v] : [];
		return v ?? '';
	}

	// svelte-ignore state_referenced_locally
	let values = $state<Record<string, unknown>>(
		Object.fromEntries(collection.fields.filter(editable).map((f) => [f.name, toInput(f, record?.[f.name])]))
	);
	const isAuth = $derived(collection.type === 'auth');
	let password = $state('');
	// Files: the stored ones the user keeps, and new ones to upload.
	// svelte-ignore state_referenced_locally
	let kept = $state<Record<string, string[]>>(Object.fromEntries(collection.fields.filter((f) => f.type === 'file').map((f) => [f.name, names(record?.[f.name])])));
	let added = $state<Record<string, File[]>>({});
	let error = $state<ApiError | null>(null);
	let saving = $state(false);
	let dialog = $state<HTMLDialogElement>();

	$effect(() => {
		dialog?.showModal();
	});

	function body(): Record<string, unknown> {
		const out: Record<string, unknown> = {};
		for (const f of collection.fields.filter(editable)) {
			const v = values[f.name];
			switch (f.type) {
				case 'number':
					out[f.name] = v === '' || v === null ? null : Number(v);
					break;
				case 'json':
					out[f.name] = (v as string).trim() === '' ? null : JSON.parse(v as string);
					break;
				case 'relation': {
					const ids = String(v).split(',').map((s) => s.trim()).filter(Boolean);
					out[f.name] = multi(f) ? ids : (ids[0] ?? null);
					break;
				}
				default:
					out[f.name] = v === '' && f.type !== 'text' ? null : v;
			}
		}
		if (isAuth && password !== '') out.password = password;
		return out;
	}

	function pick(f: Field, list: FileList | null) {
		const picked = Array.from(list ?? []);
		added[f.name] = multi(f) ? [...(added[f.name] ?? []), ...picked] : picked.slice(0, 1);
	}

	async function openFile(f: Field, name: string) {
		if (!record) return;
		const token = f.protected ? await getFileToken() : undefined;
		window.open(fileUrl(collection.name, record.id, name, { token }), '_blank', 'noopener');
	}

	async function save() {
		saving = true;
		error = null;
		try {
			const data = body();
			const files: [string, File][] = [];
			for (const f of collection.fields.filter((x) => x.type === 'file')) {
				const before = names(record?.[f.name]);
				const keep = kept[f.name] ?? [];
				if (record && keep.length !== before.length) data[f.name] = multi(f) ? keep : (keep[0] ?? null);
				for (const file of added[f.name] ?? []) files.push([f.name, file]);
			}
			const payload = files.length ? formBody(data, files) : data;
			if (record) {
				await patch(`/api/collections/${collection.name}/records/${record.id}`, payload, { 'If-Match': `"${record.updated}"` });
			} else {
				await post(`/api/collections/${collection.name}/records`, payload);
			}
			onsaved();
		} catch (e) {
			error = e instanceof ApiError ? e : new ApiError(0, { detail: e instanceof SyntaxError ? `Invalid JSON: ${e.message}` : message(e) });
		} finally {
			saving = false;
		}
	}
</script>

<dialog bind:this={dialog} aria-labelledby="record-title" onclose={onclose}>
	<form
		onsubmit={(e) => {
			e.preventDefault();
			save();
		}}
	>
		<header>
			<div class="title">
				<span class="eyebrow">{collection.name}</span>
				<h2 id="record-title">{record ? 'Edit record' : 'New record'}</h2>
			</div>
			<button type="button" class="btn sm icon ghost" aria-label="Close" onclick={() => dialog?.close()}><Icon name="x" /></button>
		</header>
		{#if record}<p class="id mono">{record.id}</p>{/if}
		<div aria-live="polite">
			{#if error}
				<p class="notice danger" role="alert">
					{error.code === 'precondition_failed' ? 'Someone changed this record since you opened it. Close and reopen it to see the latest version.' : error.message}
				</p>
			{/if}
		</div>
		<div class="fields">
			{#each collection.fields as f (f.name)}
				{@const err = error?.field(f.name)}
				{#if f.type === 'file'}
					<fieldset class="field">
						<legend class="name">
							{f.name}{f.required ? ' *' : ''} <span class="chip">file</span>{#if f.protected}<span class="chip">protected</span>{/if}
						</legend>
						{#if (kept[f.name] ?? []).length || (added[f.name] ?? []).length}
							<ul class="files">
								{#each kept[f.name] ?? [] as n (n)}
									<li>
										{#if record && isImageName(n) && !f.protected}
											<img src={fileUrl(collection.name, record.id, n, { thumb: '100x100' })} alt="" width="32" height="32" loading="lazy" />
										{:else}
											<span class="file-ico"><Icon name="file" /></span>
										{/if}
										<button type="button" class="link mono" onclick={() => openFile(f, n)}>{n}</button>
										<button type="button" class="btn sm icon ghost" aria-label="Remove {n}" title="Remove" onclick={() => (kept[f.name] = kept[f.name].filter((x) => x !== n))}><Icon name="x" /></button>
									</li>
								{/each}
								{#each added[f.name] ?? [] as file, i (i)}
									<li class="new">
										<span class="file-ico"><Icon name="upload" /></span>
										<span class="mono">{file.name}</span>
										<span class="subtle">{formatBytes(file.size)}</span>
										<button type="button" class="btn sm icon ghost" aria-label="Do not upload {file.name}" title="Remove" onclick={() => (added[f.name] = added[f.name].filter((_, j) => j !== i))}><Icon name="x" /></button>
									</li>
								{/each}
							</ul>
						{/if}
						<label class="btn sm pick">
							<Icon name="upload" />{multi(f) ? 'Add files' : (kept[f.name] ?? []).length ? 'Replace file' : 'Choose file'}
							<input
								class="sr-only"
								type="file"
								multiple={multi(f)}
								accept={((f.mime_types as string[]) ?? []).join(',') || undefined}
								onchange={(e) => {
									pick(f, e.currentTarget.files);
									e.currentTarget.value = '';
								}}
								aria-invalid={err ? 'true' : undefined}
							/>
						</label>
						<span class={err ? 'error-text' : 'hint'}>
							{err ?? `Up to ${formatBytes((f.max_size as number) ?? 10485760)} per file${multi(f) ? `, ${f.max_select} files` : ''}${((f.mime_types as string[]) ?? []).length ? `; ${(f.mime_types as string[]).join(', ')}` : ''}.${!multi(f) && (kept[f.name] ?? []).length ? ' A new file replaces the current one.' : ''}`}
						</span>
					</fieldset>
				{:else if f.type === 'ydoc'}
					<div class="field">
						<span class="name">{f.name} <span class="chip">ydoc</span></span>
						<span class="subtle value">Collaborative document: clients edit it live through <span class="mono">/api/collab/{collection.name}/{record?.id ?? '{id}'}/{f.name}</span>.</span>
					</div>
				{:else if f.type === 'autodate'}
					<div class="field">
						<span class="name">{f.name} <span class="chip">autodate</span></span>
						<span class="mono subtle value">{(record?.[f.name] as string) ?? 'set by the server'}</span>
					</div>
				{:else if f.type === 'bool'}
					<label class="check"><input type="checkbox" bind:checked={values[f.name] as boolean} /><span class="name">{f.name}</span> <span class="chip">bool</span></label>
				{:else if f.type === 'select' && multi(f)}
					<fieldset class="field">
						<legend class="name">{f.name}{f.required ? ' *' : ''} <span class="chip">select</span></legend>
						<div class="choices">
							{#each (f.values as string[]) ?? [] as v (v)}
								<label class="check"><input type="checkbox" value={v} bind:group={values[f.name] as string[]} /><span class="mono">{v}</span></label>
							{/each}
						</div>
						{#if err}<span class="error-text">{err}</span>{/if}
					</fieldset>
				{:else}
					<label class="field">
						<span class="name">{f.name}{f.required ? ' *' : ''} <span class="chip">{f.type}</span></span>
						{#if f.type === 'select'}
							<select bind:value={values[f.name]} aria-invalid={err ? 'true' : undefined}>
								<option value="">none</option>
								{#each (f.values as string[]) ?? [] as v (v)}<option value={v}>{v}</option>{/each}
							</select>
						{:else if f.type === 'json' || f.type === 'text'}
							<textarea class:mono={f.type === 'json'} rows={f.type === 'json' ? 5 : 2} bind:value={values[f.name]} aria-invalid={err ? 'true' : undefined}></textarea>
						{:else if f.type === 'number'}
							<input type="number" step="any" bind:value={values[f.name]} aria-invalid={err ? 'true' : undefined} />
						{:else}
							<input
								type={f.type === 'email' ? 'email' : f.type === 'url' ? 'url' : 'text'}
								class:mono={f.type === 'relation' || f.type === 'date'}
								placeholder={f.type === 'relation' ? (multi(f) ? 'record ids, comma separated' : 'record id') : f.type === 'date' ? '2026-11-17T12:00:00Z' : ''}
								bind:value={values[f.name]}
								aria-invalid={err ? 'true' : undefined}
							/>
						{/if}
						{#if err}<span class="error-text">{err}</span>{/if}
					</label>
				{/if}
			{/each}
			{#if isAuth}
				{@const err = error?.field('password')}
				<label class="field">
					<span class="name">password{record ? '' : ' *'} <span class="chip">hidden</span></span>
					<input
						type="password"
						autocomplete="new-password"
						bind:value={password}
						required={!record}
						placeholder={record ? 'leave empty to keep the current password' : ''}
						aria-invalid={err ? 'true' : undefined}
						aria-describedby="password-hint"
					/>
					<span id="password-hint" class={err ? 'error-text' : 'hint'}>
						{err ?? (record ? 'Setting a new password signs the user out everywhere.' : 'Stored as an Argon2id hash; never shown again.')}
					</span>
				</label>
			{/if}
		</div>
		<footer>
			<button type="button" class="btn" onclick={() => dialog?.close()}>Cancel</button>
			<button class="btn primary" disabled={saving}>{record ? 'Save' : 'Create'}</button>
		</footer>
	</form>
</dialog>

<style>
	dialog {
		width: min(640px, calc(100vw - 32px));
		max-height: calc(100vh - 48px);
	}
	form {
		display: grid;
		gap: 16px;
	}
	header,
	footer {
		display: flex;
		justify-content: space-between;
		align-items: flex-start;
		gap: 8px;
	}
	footer {
		justify-content: flex-end;
		padding-top: 8px;
	}
	.title {
		display: grid;
		gap: 6px;
	}
	.id {
		margin-top: -8px;
		font-size: 12px;
		color: var(--fg-subtle);
	}
	.fields {
		display: grid;
		gap: 16px;
	}
	.field {
		display: grid;
		gap: 6px;
		margin: 0;
		padding: 0;
		border: 0;
		font-size: 14px;
		font-weight: 500;
	}
	.name {
		display: inline-flex;
		align-items: center;
		gap: 8px;
		font-family: var(--font-mono);
		font-size: 13px;
	}
	.value {
		font-size: 13px;
	}
	.check {
		display: inline-flex;
		align-items: center;
		gap: 8px;
		min-height: 32px;
		font-size: 14px;
	}
	.pick {
		justify-self: start;
		cursor: pointer;
	}
	.pick:has(input:focus-visible) {
		outline: 2px solid var(--accent);
		outline-offset: 2px;
	}
	.files {
		display: grid;
		gap: 6px;
		margin: 0;
		padding: 0;
		list-style: none;
	}
	.files li {
		display: flex;
		align-items: center;
		gap: 10px;
		min-height: 40px;
		padding: 4px 4px 4px 8px;
		border: 1px solid var(--border);
		border-radius: var(--radius-sm);
		background: var(--surface);
		font-size: 13px;
		font-weight: 400;
	}
	.files li.new {
		border-style: dashed;
	}
	.files img {
		border-radius: 4px;
		object-fit: cover;
	}
	.files .link {
		flex: 1;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		padding: 0;
		border: 0;
		background: none;
		color: var(--fg);
		text-align: left;
		font-size: 13px;
		cursor: pointer;
		text-decoration: underline;
		text-decoration-color: var(--border-strong);
	}
	.files .mono:not(.link) {
		flex: 1;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.file-ico {
		display: inline-flex;
		width: 32px;
		justify-content: center;
		color: var(--fg-subtle);
	}
	.choices {
		display: flex;
		flex-wrap: wrap;
		gap: 4px 16px;
	}
</style>
