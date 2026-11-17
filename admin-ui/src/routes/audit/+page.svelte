<script lang="ts">
	// Audit trail (SPEC §14.1): who changed what, newest first, with the
	// diff of each entry. Filters by action, actor, collection and record.
	import { ApiError, get, message } from '#lib/api.ts';
	import Icon from '#lib/components/Icon.svelte';
	import { session } from '#lib/session.svelte.ts';

	interface Entry {
		id: string;
		ts: string;
		actor_type: string;
		actor_id: string | null;
		actor_label: string | null;
		action: string;
		collection_id: string | null;
		record_id: string | null;
		diff: unknown;
		ip: string | null;
		request_id: string | null;
		meta: unknown;
	}

	let action = $state('');
	let actor = $state('');
	let collection = $state('');
	let record = $state('');
	let page = $state(1);
	let data = $state<{ items: Entry[]; total_items: number; total_pages: number } | null>(null);
	let error = $state('');
	let open = $state<string | null>(null);

	function query(): URLSearchParams {
		const q = new URLSearchParams();
		if (action.trim()) q.set('action', action.trim());
		if (actor.trim()) q.set('actor_id', actor.trim());
		if (collection) q.set('collection_id', collection);
		if (record.trim()) q.set('record_id', record.trim());
		return q;
	}

	async function load() {
		const q = query();
		q.set('page', String(page));
		q.set('per_page', '50');
		try {
			data = await get(`/api/audit?${q}`);
			error = '';
		} catch (e) {
			error = e instanceof ApiError ? e.message : message(e);
		}
	}

	$effect(() => {
		void [page, collection];
		load();
	});

	const exportHref = $derived.by(() => {
		void [action, actor, collection, record];
		const q = query();
		q.set('format', 'ndjson');
		return `/api/audit?${q}`;
	});
	const collectionName = (id: string | null) => session.collections.find((c) => c.id === id)?.name ?? id;
</script>

<div class="page">
	<header class="bar">
		<div class="title">
			<span class="eyebrow">Observability</span>
			<h1>Audit trail</h1>
		</div>
		<a class="btn" href={exportHref} download="audit.ndjson"><Icon name="download" />Export NDJSON</a>
	</header>

	<form
		class="filters"
		onsubmit={(e) => {
			e.preventDefault();
			page = 1;
			load();
		}}
	>
		<label class="field grow">Action<input type="text" class="mono" bind:value={action} placeholder="record. or auth.login.failure" /></label>
		<label class="field">Collection
			<select bind:value={collection}>
				<option value="">any</option>
				{#each session.collections as c (c.id)}<option value={c.id}>{c.name}</option>{/each}
			</select>
		</label>
		<label class="field grow">Record id<input type="text" class="mono" bind:value={record} /></label>
		<label class="field grow">Actor id<input type="text" class="mono" bind:value={actor} /></label>
		<button class="btn primary">Apply</button>
	</form>

	{#if error}<p class="notice danger" role="alert"><Icon name="alert" /><span>{error}</span></p>{/if}

	<ol class="timeline" aria-label="Audit entries">
		{#each data?.items ?? [] as e (e.id)}
			<li class="card entry">
				<div class="row">
					<div class="main">
						<span class="action mono">{e.action}</span>
						{#if e.collection_id}
							<span class="target mono">{collectionName(e.collection_id)}{#if e.record_id}/{e.record_id.slice(-12)}{/if}</span>
						{/if}
					</div>
					<div class="side">
						<span class="who">{e.actor_label ?? e.actor_type} <span class="kind">{e.actor_type}</span></span>
						<time class="mono" datetime={e.ts}>{new Date(e.ts).toLocaleString()}</time>
					</div>
					{#if e.diff || e.meta}
						<button class="btn sm icon ghost" aria-expanded={open === e.id} aria-label="Details of {e.action}" onclick={() => (open = open === e.id ? null : e.id)}><Icon name="plus" /></button>
					{:else}
						<span></span>
					{/if}
				</div>
				{#if open === e.id}
					{#if e.diff}<pre class="well">{JSON.stringify(e.diff, null, 2)}</pre>{/if}
					{#if e.meta}<pre class="well">{JSON.stringify(e.meta, null, 2)}</pre>{/if}
					<p class="hint mono">ip {e.ip ?? 'none'} · request {e.request_id ?? 'none'}</p>
				{/if}
			</li>
		{:else}
			<li class="empty">{data ? 'No entries match.' : 'Loading…'}</li>
		{/each}
	</ol>

	{#if data && data.total_pages > 1}
		<nav class="pager" aria-label="Pages">
			<button class="btn sm" disabled={page <= 1} onclick={() => page--}><Icon name="chevronLeft" />Newer</button>
			<span class="hint mono">page {page} of {data.total_pages}</span>
			<button class="btn sm" disabled={page >= data.total_pages} onclick={() => page++}>Older<Icon name="chevronRight" /></button>
		</nav>
	{/if}
</div>

<style>
	.page {
		display: grid;
		gap: 16px;
		max-width: 1100px;
	}
	.bar {
		display: flex;
		align-items: flex-end;
		justify-content: space-between;
		gap: 16px;
		margin-bottom: 8px;
	}
	.title {
		display: grid;
		gap: 6px;
	}
	.filters {
		display: flex;
		gap: 12px;
		align-items: flex-end;
		flex-wrap: wrap;
	}
	.grow {
		flex: 1 1 180px;
	}
	.timeline {
		display: grid;
		gap: 8px;
		margin: 0;
		padding: 0;
		list-style: none;
	}
	.entry {
		display: grid;
		gap: 10px;
		padding: 12px 16px;
	}
	.row {
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto 32px;
		gap: 12px;
		align-items: center;
		font-size: 13px;
	}
	.main,
	.side {
		display: grid;
		gap: 2px;
		min-width: 0;
	}
	.side {
		justify-items: end;
		text-align: right;
	}
	.action {
		color: var(--accent-text);
	}
	.kind {
		color: var(--fg-subtle);
		font-size: 12px;
	}
	.target {
		color: var(--fg-muted);
		overflow-wrap: anywhere;
	}
	time {
		font-size: 12px;
		color: var(--fg-subtle);
		white-space: nowrap;
	}
	pre {
		margin: 0;
		max-height: 320px;
		overflow: auto;
		font-size: 12px;
	}
	.empty {
		text-align: center;
		color: var(--fg-subtle);
		padding: 28px;
	}
	.pager {
		display: flex;
		align-items: center;
		justify-content: center;
		gap: 12px;
	}
	@media (max-width: 560px) {
		.row {
			grid-template-columns: minmax(0, 1fr) 32px;
		}
		.side {
			grid-row: 2;
			justify-items: start;
			text-align: left;
		}
	}
</style>
