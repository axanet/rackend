<script lang="ts">
	// Request logs (SPEC §14.2): newest first, with filters and details.
	import { ApiError, get, message } from '#lib/api.ts';
	import Icon from '#lib/components/Icon.svelte';

	interface Row {
		id: number;
		ts: string;
		method: string;
		route: string | null;
		path: string;
		status: number;
		duration_ms: number;
		ip: string | null;
		user_agent: string | null;
		actor_type: string | null;
		actor_id: string | null;
		request_id: string | null;
		bytes_in: number | null;
		bytes_out: number | null;
		error: string | null;
	}

	let method = $state('');
	let path = $state('');
	let status = $state('');
	let requestId = $state('');
	let page = $state(1);
	let data = $state<{ items: Row[]; total_items: number; total_pages: number } | null>(null);
	let error = $state('');
	let open = $state<number | null>(null);

	async function load() {
		const q = new URLSearchParams({ page: String(page), per_page: '50' });
		if (method) q.set('method', method);
		if (path.trim()) q.set('path', path.trim());
		if (status === 'errors') q.set('min_status', '400');
		if (status === 'server') q.set('min_status', '500');
		if (status === 'ok') q.set('max_status', '399');
		if (requestId.trim()) q.set('request_id', requestId.trim());
		try {
			data = await get(`/api/logs?${q}`);
			error = '';
		} catch (e) {
			error = e instanceof ApiError ? e.message : message(e);
		}
	}

	$effect(() => {
		void [page, method, status];
		load();
	});

	const tone = (s: number) => (s >= 500 ? 'bad' : s >= 400 ? 'warn' : 'ok');
</script>

<div class="page">
	<header class="bar">
		<div class="title">
			<span class="eyebrow">Observability</span>
			<h1>Request logs</h1>
		</div>
		<button class="btn" onclick={load}><Icon name="refresh" />Refresh</button>
	</header>

	<form
		class="filters"
		onsubmit={(e) => {
			e.preventDefault();
			page = 1;
			load();
		}}
	>
		<label class="field">Method
			<select bind:value={method}>
				<option value="">any</option>
				{#each ['GET', 'POST', 'PATCH', 'PUT', 'DELETE'] as m (m)}<option>{m}</option>{/each}
			</select>
		</label>
		<label class="field">Status
			<select bind:value={status}>
				<option value="">any</option>
				<option value="ok">success (below 400)</option>
				<option value="errors">errors (400 and up)</option>
				<option value="server">server errors (500 and up)</option>
			</select>
		</label>
		<label class="field grow">Path or route<input type="text" class="mono" bind:value={path} placeholder="/api/collections" /></label>
		<label class="field grow">Request id<input type="text" class="mono" bind:value={requestId} /></label>
		<button class="btn primary">Apply</button>
	</form>

	{#if error}<p class="notice danger" role="alert"><Icon name="alert" /><span>{error}</span></p>{/if}

	<div class="card table-wrap">
		<table class="table">
			<thead>
				<tr>
					<th scope="col">Time</th>
					<th scope="col">Request</th>
					<th scope="col">Status</th>
					<th scope="col" class="r">Duration</th>
					<th scope="col">Actor</th>
					<th scope="col"><span class="sr-only">Details</span></th>
				</tr>
			</thead>
			<tbody>
				{#each data?.items ?? [] as r (r.id)}
					<tr class:open={open === r.id}>
						<td class="mono time" title={new Date(r.ts).toLocaleString()}>{new Date(r.ts).toLocaleTimeString()}</td>
						<td class="mono req"><span class="method">{r.method}</span> {r.path}</td>
						<td><span class="status {tone(r.status)} mono">{r.status}</span>{#if r.error}<span class="code mono">{r.error}</span>{/if}</td>
						<td class="r mono">{r.duration_ms < 10 ? r.duration_ms.toFixed(1) : Math.round(r.duration_ms)} ms</td>
						<td class="mono" class:none={!r.actor_type || r.actor_type === 'anonymous'}>{r.actor_type && r.actor_type !== 'anonymous' ? r.actor_type : 'anonymous'}</td>
						<td class="r">
							<button class="btn sm icon ghost" aria-expanded={open === r.id} aria-label="Details of request {r.request_id}" onclick={() => (open = open === r.id ? null : r.id)}><Icon name="plus" /></button>
						</td>
					</tr>
					{#if open === r.id}
						<tr class="details">
							<td colspan="6">
								<dl>
									<dt>Route</dt><dd class="mono">{r.route ?? 'none'}</dd>
									<dt>Request id</dt><dd class="mono">{r.request_id ?? 'none'}</dd>
									<dt>Actor id</dt><dd class="mono">{r.actor_id ?? 'none'}</dd>
									<dt>IP</dt><dd class="mono">{r.ip ?? 'none'}</dd>
									<dt>User agent</dt><dd class="mono">{r.user_agent ?? 'none'}</dd>
									<dt>Bytes in / out</dt><dd class="mono">{r.bytes_in ?? 'none'} / {r.bytes_out ?? 'none'}</dd>
								</dl>
							</td>
						</tr>
					{/if}
				{:else}
					<tr><td colspan="6" class="empty">{data ? 'No requests match.' : 'Loading…'}</td></tr>
				{/each}
			</tbody>
		</table>
	</div>

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
		max-width: 1200px;
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
		flex: 1 1 200px;
	}
	.table-wrap {
		overflow-x: auto;
	}
	.table {
		width: 100%;
		border-collapse: collapse;
		font-size: 13px;
	}
	.table th {
		text-align: left;
		font: 500 11px/1 var(--font-mono);
		text-transform: uppercase;
		letter-spacing: 0.08em;
		color: var(--fg-subtle);
		padding: 14px 12px;
		border-bottom: 1px solid var(--border);
		background: var(--surface);
	}
	.table td {
		padding: 9px 12px;
		border-bottom: 1px solid var(--border);
		vertical-align: top;
	}
	.time {
		white-space: nowrap;
		color: var(--fg-muted);
	}
	.req {
		overflow-wrap: anywhere;
	}
	.method {
		color: var(--accent-text);
	}
	.status {
		display: inline-block;
		padding: 1px 8px;
		border-radius: 999px;
		border: 1px solid var(--border);
	}
	.status.bad {
		color: var(--danger);
		border-color: var(--danger);
	}
	.status.warn {
		color: var(--write);
		border-color: var(--write);
	}
	.code {
		margin-left: 8px;
		font-size: 12px;
		color: var(--fg-subtle);
	}
	.r {
		text-align: right;
	}
	.none {
		color: var(--fg-subtle);
	}
	tr.open td {
		background: var(--surface);
	}
	.details td {
		background: var(--surface);
	}
	dl {
		display: grid;
		grid-template-columns: max-content 1fr;
		gap: 6px 16px;
		margin: 0;
	}
	dt {
		font-size: 12px;
		color: var(--fg-subtle);
	}
	dd {
		margin: 0;
		overflow-wrap: anywhere;
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
</style>
