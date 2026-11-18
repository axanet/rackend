<script lang="ts">
	// Records browser: filter (rule language), sortable columns, pagination,
	// create / edit / delete.
	import { page } from '$app/state';
	import { resolve } from '$app/paths';
	import { ApiError, del, fileUrl, get, isImageName, message, type Field, type Page, type RecordRow } from '#lib/api.ts';
	import Icon from '#lib/components/Icon.svelte';
	import RecordForm from '#lib/components/RecordForm.svelte';
	import { live } from '#lib/realtime.svelte.ts';
	import { session } from '#lib/session.svelte.ts';

	const name = $derived(page.params.name ?? '');
	const collection = $derived(session.collections.find((c) => c.name === name) ?? null);

	let filterInput = $state('');
	let filter = $state('');
	let sort = $state('-created');
	let pageNo = $state(1);
	let perPage = $state(30);
	let data = $state<Page<RecordRow> | null>(null);
	let error = $state<ApiError | null>(null);
	let loading = $state(false);
	let editing = $state<RecordRow | null | undefined>(undefined);
	let status = $state('');

	// Reset paging and filters when switching collections.
	let lastName = '';
	$effect(() => {
		if (name !== lastName) {
			lastName = name;
			filterInput = '';
			filter = '';
			sort = '-created';
			pageNo = 1;
		}
	});

	async function load() {
		if (!collection) return;
		loading = true;
		const q = new URLSearchParams({ page: String(pageNo), per_page: String(perPage), sort });
		if (filter.trim()) q.set('filter', filter);
		try {
			data = await get<Page<RecordRow>>(`/api/collections/${collection.name}/records?${q}`);
			error = null;
			status = `${data.total_items} record${data.total_items === 1 ? '' : 's'}`;
		} catch (e) {
			error = e instanceof ApiError ? e : new ApiError(0, { detail: message(e) });
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		void [collection?.id, collection?.updated, filter, sort, pageNo, perPage];
		load();
	});

	// Live updates: any change in this collection reloads the current page
	// (debounced), so sorting, filters and paging stay server-side.
	let reloadTimer: ReturnType<typeof setTimeout> | undefined;
	$effect(() => {
		const c = collection;
		if (!c) return;
		live.watch([`records:${c.name}`], () => {
			clearTimeout(reloadTimer);
			reloadTimer = setTimeout(load, 250);
		});
		return () => {
			clearTimeout(reloadTimer);
			live.unwatch();
		};
	});

	// Collaborative documents are not part of the record JSON.
	const columns = $derived(collection ? ['id', ...collection.fields.filter((f) => f.type !== 'ydoc').map((f) => f.name), 'updated'] : []);

	function toggleSort(col: string) {
		const sortable = col === 'id' || col === 'updated' || collection?.fields.some((f) => f.name === col && !['json'].includes(f.type) && ((f.max_select as number) ?? 1) <= 1);
		if (!sortable) return;
		sort = sort === `-${col}` ? col : `-${col}`;
		pageNo = 1;
	}

	function sortState(col: string): 'ascending' | 'descending' | undefined {
		return sort === col ? 'ascending' : sort === `-${col}` ? 'descending' : undefined;
	}

	// Empty cells say "none" (Writing Style: no dashes as placeholders).
	function cell(v: unknown): string {
		if (v === null || v === undefined || v === '') return 'none';
		if (Array.isArray(v) && v.length === 0) return 'none';
		if (typeof v === 'object') return JSON.stringify(v);
		return String(v);
	}

	const fileField = (col: string): Field | undefined => collection?.fields.find((f) => f.name === col && f.type === 'file');
	const fileNames = (v: unknown): string[] => (Array.isArray(v) ? (v as string[]) : typeof v === 'string' && v ? [v] : []);

	const empty = (v: unknown) => v === null || v === undefined || v === '' || (Array.isArray(v) && v.length === 0);

	/** Mono for ids, timestamps, numbers and JSON. */
	function isMono(col: string): boolean {
		if (col === 'id' || col === 'updated' || col === 'created') return true;
		const f = collection?.fields.find((x) => x.name === col);
		return !!f && ['number', 'date', 'autodate', 'json', 'relation'].includes(f.type);
	}

	async function remove(r: RecordRow) {
		if (!collection || !confirm(`Delete record ${r.id}? This cannot be undone.`)) return;
		try {
			await del(`/api/collections/${collection.name}/records/${r.id}`, { 'If-Match': `"${r.updated}"` });
			await load();
		} catch (e) {
			error = e instanceof ApiError ? e : new ApiError(0, { detail: message(e) });
		}
	}

	function applyFilter(e: SubmitEvent) {
		e.preventDefault();
		filter = filterInput;
		pageNo = 1;
	}
</script>

{#if !collection}
	<p class="hint">{session.collections.length ? `No collection named “${name}”.` : 'Loading…'}</p>
{:else}
	<header class="bar">
		<div class="title">
			<span class="eyebrow">Records</span>
			<div class="name-row">
				<h1>{collection.name}</h1>
				<span class="chip" aria-live="polite">{status || 'loading…'}</span>
				<span class="live" class:on={live.connected} title={live.connected ? 'Updates appear as they happen' : 'Not receiving live updates'}>
					<span class="dot" aria-hidden="true"></span>{live.connected ? 'live' : 'offline'}
				</span>
			</div>
		</div>
		<div class="actions">
			<a class="btn" href={resolve('/collections/[name]/schema', { name: collection.name })}><Icon name="sliders" />Schema and rules</a>
			<button class="btn primary" onclick={() => (editing = null)}><Icon name="plus" />New record</button>
		</div>
	</header>

	<form class="filter" onsubmit={applyFilter} role="search">
		<label class="sr-only" for="filter">Filter records</label>
		<span class="filter-icon"><Icon name="search" /></span>
		<input
			id="filter"
			type="search"
			class="mono"
			placeholder={"Filter, e.g. title ~ 'hello' && created > '2026-01-01'"}
			bind:value={filterInput}
			aria-invalid={error?.code === 'rule_compile_error' ? 'true' : undefined}
			aria-describedby="filter-error"
			spellcheck="false"
		/>
		<button class="btn sm">Apply</button>
		<button type="button" class="btn sm icon ghost" aria-label="Reload" title="Reload" onclick={load}><Icon name="refresh" /></button>
	</form>
	<div id="filter-error" aria-live="polite">
		{#if error}
			<p class="error-text" role="alert">{error.position ? `Column ${error.position.column}: ` : ''}{error.message}</p>
		{/if}
	</div>

	<div class="table-wrap" aria-busy={loading}>
		<table>
			<thead>
				<tr>
					{#each columns as col (col)}
						<th scope="col" aria-sort={sortState(col)}>
							<button class="th" onclick={() => toggleSort(col)}>
								{col}
								{#if sortState(col) === 'ascending'}<Icon name="arrowUp" size={12} />{:else if sortState(col) === 'descending'}<Icon name="arrowDown" size={12} />{/if}
							</button>
						</th>
					{/each}
					<th scope="col"><span class="sr-only">Actions</span></th>
				</tr>
			</thead>
			<tbody>
				{#each data?.items ?? [] as r (r.id)}
					<tr>
						{#each columns as col (col)}
							{@const ff = fileField(col)}
							{#if ff && !empty(r[col])}
								<td class="files-cell" title={fileNames(r[col]).join(', ')}>
									{#each fileNames(r[col]).slice(0, 3) as n (n)}
										{#if isImageName(n) && !ff.protected}
											<img src={fileUrl(collection?.name ?? '', r.id, n, { thumb: '100x100' })} alt={n} width="24" height="24" loading="lazy" />
										{:else}
											<span class="file-name mono">{n}</span>
										{/if}
									{/each}
									{#if fileNames(r[col]).length > 3}<span class="subtle">+{fileNames(r[col]).length - 3}</span>{/if}
								</td>
							{:else}
								<td class:mono={isMono(col)} class:none={empty(r[col])} title={cell(r[col])}>{col === 'id' ? r.id.slice(-12) : cell(r[col])}</td>
							{/if}
						{/each}
						<td class="row-actions">
							<button class="btn sm icon ghost" aria-label="Edit record {r.id}" onclick={() => (editing = r)}><Icon name="pencil" /></button>
							<button class="btn sm icon ghost del" aria-label="Delete record {r.id}" onclick={() => remove(r)}><Icon name="trash" /></button>
						</td>
					</tr>
				{:else}
					<tr><td colspan={columns.length + 1} class="empty-row">{loading ? 'Loading…' : filter ? 'No records match this filter.' : 'No records yet.'}</td></tr>
				{/each}
			</tbody>
		</table>
	</div>

	{#if data && data.total_pages > 1}
		<nav class="pager" aria-label="Pagination">
			<button class="btn sm icon" aria-label="Previous page" disabled={pageNo <= 1} onclick={() => pageNo--}><Icon name="chevronLeft" /></button>
			<span class="mono pages">{data.page} / {data.total_pages}</span>
			<button class="btn sm icon" aria-label="Next page" disabled={pageNo >= data.total_pages} onclick={() => pageNo++}><Icon name="chevronRight" /></button>
			<label class="per-page"
				><span class="eyebrow">Per page</span>
				<select bind:value={perPage} onchange={() => (pageNo = 1)}>
					{#each [30, 100, 500] as n (n)}<option value={n}>{n}</option>{/each}
				</select>
			</label>
		</nav>
	{/if}

	{#if editing !== undefined}
		<RecordForm
			{collection}
			record={editing}
			onclose={() => (editing = undefined)}
			onsaved={() => {
				editing = undefined;
				load();
			}}
		/>
	{/if}
{/if}

<style>
	.bar {
		display: flex;
		justify-content: space-between;
		align-items: flex-end;
		gap: 16px;
		flex-wrap: wrap;
		margin-bottom: 20px;
	}
	.title {
		display: grid;
		gap: 6px;
	}
	.name-row {
		display: flex;
		align-items: center;
		gap: 12px;
	}
	.actions {
		display: flex;
		gap: 8px;
		flex-wrap: wrap;
	}
	.filter {
		display: flex;
		align-items: center;
		gap: 8px;
		position: relative;
	}
	.filter-icon {
		position: absolute;
		left: 14px;
		color: var(--fg-subtle);
		pointer-events: none;
	}
	.filter input {
		flex: 1;
		padding-left: 38px;
	}
	#filter-error {
		min-height: 22px;
		margin: 6px 0 8px;
	}
	.table-wrap {
		overflow-x: auto;
		border: 1px solid var(--border);
		border-radius: var(--radius);
		background: var(--bg-elev);
	}
	table {
		width: 100%;
		border-collapse: collapse;
		font-size: 14px;
	}
	th,
	td {
		height: 40px;
		padding: 0 14px;
		max-width: 300px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		text-align: left;
		border-bottom: 1px solid var(--border);
	}
	td.mono {
		font-size: 12.5px;
	}
	td.none {
		color: var(--fg-subtle);
	}
	tbody tr:last-child td {
		border-bottom: 0;
	}
	thead th {
		padding: 0;
		background: var(--surface);
	}
	.th {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		width: 100%;
		height: 38px;
		padding: 0 14px;
		border: 0;
		background: none;
		font: 500 11px/1 var(--font-mono);
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: var(--fg-subtle);
		cursor: pointer;
	}
	.th:hover,
	th[aria-sort] .th {
		color: var(--fg);
	}
	tbody tr {
		transition: background-color 0.15s var(--ease-out);
	}
	tbody tr:hover {
		background: var(--surface);
	}
	.row-actions {
		width: 1%;
		text-align: right;
		padding-right: 8px;
	}
	.row-actions :global(.btn) {
		display: inline-flex;
	}
	.del:hover {
		color: var(--danger);
	}
	.empty-row {
		height: 120px;
		text-align: center;
		color: var(--fg-subtle);
	}
	.pager {
		display: flex;
		align-items: center;
		gap: 10px;
		margin-top: 14px;
	}
	.pages {
		font-size: 13px;
		color: var(--fg-muted);
	}
	.per-page {
		display: inline-flex;
		align-items: center;
		gap: 10px;
		margin-left: auto;
	}
	.per-page select {
		width: auto;
		min-height: 34px;
		padding: 4px 10px;
	}
	.files-cell {
		white-space: nowrap;
	}
	.files-cell img {
		vertical-align: middle;
		margin-right: 4px;
		border-radius: 4px;
		object-fit: cover;
		border: 1px solid var(--border);
	}
	.file-name {
		display: inline-block;
		max-width: 160px;
		overflow: hidden;
		text-overflow: ellipsis;
		vertical-align: middle;
		margin-right: 6px;
	}
	.live {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		font: 500 11px/1 var(--font-mono);
		text-transform: uppercase;
		letter-spacing: 0.08em;
		color: var(--fg-subtle);
	}
	.live .dot {
		width: 7px;
		height: 7px;
		border-radius: 50%;
		background: var(--border-strong);
	}
	.live.on {
		color: var(--accent-text);
	}
	.live.on .dot {
		background: var(--accent);
		box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 20%, transparent);
	}
	@media (prefers-reduced-motion: no-preference) {
		.live.on .dot {
			animation: pulse 2.4s var(--ease-out) infinite;
		}
	}
	@keyframes pulse {
		50% {
			box-shadow: 0 0 0 5px color-mix(in srgb, var(--accent) 0%, transparent);
		}
	}
</style>
