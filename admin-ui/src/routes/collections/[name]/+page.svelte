<script lang="ts">
	// Records browser: filter (rule language), sortable columns, pagination,
	// create / edit / delete.
	import { page } from '$app/state';
	import { resolve } from '$app/paths';
	import { ApiError, del, get, message, type Page, type RecordRow } from '#lib/api.ts';
	import Icon from '#lib/components/Icon.svelte';
	import RecordForm from '#lib/components/RecordForm.svelte';
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

	const columns = $derived(collection ? ['id', ...collection.fields.map((f) => f.name), 'updated'] : []);

	function toggleSort(col: string) {
		const sortable = col === 'id' || col === 'updated' || collection?.fields.some((f) => f.name === col && !['json'].includes(f.type) && ((f.max_select as number) ?? 1) <= 1);
		if (!sortable) return;
		sort = sort === `-${col}` ? col : `-${col}`;
		pageNo = 1;
	}

	function sortState(col: string): 'ascending' | 'descending' | undefined {
		return sort === col ? 'ascending' : sort === `-${col}` ? 'descending' : undefined;
	}

	function cell(v: unknown): string {
		if (v === null || v === undefined || v === '') return '—';
		if (typeof v === 'object') return JSON.stringify(v);
		return String(v);
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
			<h1>{collection.name}</h1>
			<span class="hint" aria-live="polite">{status}</span>
		</div>
		<div class="actions">
			<a class="btn" href={resolve('/collections/[name]/schema', { name: collection.name })}><Icon name="settings" />Schema & rules</a>
			<button class="btn primary" onclick={() => (editing = null)}><Icon name="plus" />New record</button>
		</div>
	</header>

	<form class="filter" onsubmit={applyFilter} role="search">
		<label class="sr-only" for="filter">Filter records</label>
		<Icon name="search" />
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
		<button class="btn">Apply</button>
		<button type="button" class="btn icon ghost" aria-label="Reload" title="Reload" onclick={load}><Icon name="refresh" /></button>
	</form>
	<div id="filter-error" aria-live="polite">
		{#if error}
			<p class="error-text" role="alert">
				{error.position ? `Column ${error.position.column}: ` : ''}{error.message}
			</p>
		{/if}
	</div>

	<div class="table-wrap card" aria-busy={loading}>
		<table>
			<thead>
				<tr>
					{#each columns as col (col)}
						<th scope="col" aria-sort={sortState(col)}>
							<button class="th" onclick={() => toggleSort(col)}>
								{col}
								{#if sortState(col) === 'ascending'}<Icon name="arrowUp" size={14} />{:else if sortState(col) === 'descending'}<Icon name="arrowDown" size={14} />{/if}
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
							<td class:mono={col === 'id' || col === 'updated'} title={cell(r[col])}>{col === 'id' ? r.id.slice(-12) : cell(r[col])}</td>
						{/each}
						<td class="row-actions">
							<button class="btn icon ghost" aria-label="Edit record {r.id}" onclick={() => (editing = r)}><Icon name="pencil" size={16} /></button>
							<button class="btn icon ghost danger" aria-label="Delete record {r.id}" onclick={() => remove(r)}><Icon name="trash" size={16} /></button>
						</td>
					</tr>
				{:else}
					<tr><td colspan={columns.length + 1} class="empty">{loading ? 'Loading…' : filter ? 'No records match this filter.' : 'No records yet.'}</td></tr>
				{/each}
			</tbody>
		</table>
	</div>

	{#if data && data.total_pages > 1}
		<nav class="pager" aria-label="Pagination">
			<button class="btn icon" aria-label="Previous page" disabled={pageNo <= 1} onclick={() => pageNo--}><Icon name="chevronLeft" /></button>
			<span>Page {data.page} of {data.total_pages}</span>
			<button class="btn icon" aria-label="Next page" disabled={pageNo >= data.total_pages} onclick={() => pageNo++}><Icon name="chevronRight" /></button>
			<label class="per-page">Per page
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
		align-items: center;
		gap: var(--space-3);
		flex-wrap: wrap;
		margin-bottom: var(--space-4);
	}
	.title {
		display: flex;
		align-items: baseline;
		gap: var(--space-3);
	}
	.actions {
		display: flex;
		gap: var(--space-2);
	}
	.filter {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		color: var(--color-muted-foreground);
	}
	.filter input {
		flex: 1;
	}
	#filter-error {
		min-height: var(--space-4);
		margin: var(--space-1) 0 var(--space-2);
	}
	#filter-error p {
		margin: 0;
	}
	.table-wrap {
		overflow-x: auto;
	}
	table {
		width: 100%;
		border-collapse: collapse;
		font-size: 0.875rem;
	}
	th,
	td {
		text-align: left;
		padding: 0 var(--space-3);
		height: 36px;
		border-bottom: 1px solid var(--color-border);
		max-width: 280px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	thead th {
		background: var(--color-muted);
		position: sticky;
		top: 0;
		padding: 0;
	}
	.th {
		display: inline-flex;
		align-items: center;
		gap: var(--space-1);
		width: 100%;
		height: 36px;
		padding: 0 var(--space-3);
		background: none;
		border: 0;
		font: inherit;
		font-weight: 600;
		color: var(--color-muted-foreground);
		cursor: pointer;
	}
	tbody tr:hover {
		background: var(--color-muted);
	}
	.row-actions {
		width: 1%;
		text-align: right;
	}
	.empty {
		text-align: center;
		color: var(--color-muted-foreground);
		height: 96px;
	}
	.pager {
		display: flex;
		align-items: center;
		gap: var(--space-3);
		margin-top: var(--space-3);
	}
	.per-page {
		display: inline-flex;
		align-items: center;
		gap: var(--space-2);
		margin-left: auto;
		font-size: 0.875rem;
	}
	.per-page select {
		width: auto;
	}
</style>
