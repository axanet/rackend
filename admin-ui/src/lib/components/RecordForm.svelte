<script lang="ts">
	// Create/edit one record in a modal dialog. Field inputs follow the field
	// type; server validation errors appear next to their field.
	import { ApiError, message, patch, post, type Collection, type Field, type RecordRow } from '../api';
	import Icon from './Icon.svelte';

	let {
		collection,
		record,
		onclose,
		onsaved
	}: { collection: Collection; record: RecordRow | null; onclose: () => void; onsaved: () => void } = $props();

	const multi = (f: Field) => ((f.max_select as number | undefined) ?? 1) > 1;
	const editable = (f: Field) => f.type !== 'autodate';

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

	async function save() {
		saving = true;
		error = null;
		try {
			const data = body();
			if (record) {
				await patch(`/api/collections/${collection.name}/records/${record.id}`, data, { 'If-Match': `"${record.updated}"` });
			} else {
				await post(`/api/collections/${collection.name}/records`, data);
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
				{#if f.type === 'autodate'}
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
	.choices {
		display: flex;
		flex-wrap: wrap;
		gap: 4px 16px;
	}
</style>
