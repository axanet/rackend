<script lang="ts">
	// Dashboard (SPEC §14.4): live numbers every 10 s, last-hour series every
	// 60 s. Inside a container, memory and CPU are the container's.
	import { ApiError, formatBytes, get, message } from '#lib/api.ts';
	import Icon from '#lib/components/Icon.svelte';
	import LineChart from '#lib/components/LineChart.svelte';

	interface Route {
		method: string;
		route: string;
		count: number;
		p95_ms: number;
		errors: number;
	}
	interface Row {
		ts: string;
		method: string;
		route: string | null;
		path: string;
		status: number;
		error: string | null;
		request_id: string | null;
	}
	interface Summary {
		version: string;
		uptime_s: number;
		requests: {
			per_second: number;
			last_minute: number;
			error_rate: number;
			last_hour: number;
			latency_ms: { p50: number; p95: number; p99: number };
			top_routes: Route[];
			recent_errors: Row[];
		};
		db: { write_queue: number; data_bytes: number; wal_bytes: number; logs_bytes: number };
		realtime: { connections: number; events: number };
		storage: { files: number; bytes: number; backend: string };
		host: {
			container: boolean;
			cpu_percent: number;
			cpus: number;
			load_avg: [number, number, number];
			memory_used: number;
			memory_total: number;
			host_memory_total: number;
			disk_total: number;
			disk_free: number;
			process_rss: number;
		};
		disk_low: boolean;
	}

	const SERIES = ['http.rps', 'http.p95_ms', 'host.cpu_percent', 'host.memory_used'] as const;

	let summary = $state<Summary | null>(null);
	let series = $state<Record<string, [string, number][]>>({});
	let error = $state('');

	async function loadSummary() {
		try {
			summary = await get<Summary>('/api/metrics/summary');
			error = '';
		} catch (e) {
			error = e instanceof ApiError ? e.message : message(e);
		}
	}
	async function loadSeries() {
		try {
			const r = await get<{ series: Record<string, [string, number][]> }>(`/api/metrics/series?series=${SERIES.join(',')}`);
			series = r.series;
		} catch {
			/* the summary shows errors */
		}
	}

	$effect(() => {
		loadSummary();
		loadSeries();
		const a = setInterval(loadSummary, 10_000);
		const b = setInterval(loadSeries, 60_000);
		return () => {
			clearInterval(a);
			clearInterval(b);
		};
	});

	const pct = (v: number) => `${(v * 100).toFixed(v < 0.1 ? 1 : 0)}%`;
	const ms = (v: number) => (v >= 100 ? `${Math.round(v)} ms` : `${v.toFixed(1)} ms`);
	const rate = (v: number) => (v >= 10 ? v.toFixed(0) : v.toFixed(2));
	const share = (used: number, total: number) => (total > 0 ? used / total : 0);
	function uptime(s: number) {
		const d = Math.floor(s / 86400);
		const h = Math.floor((s % 86400) / 3600);
		const m = Math.floor((s % 3600) / 60);
		return d ? `${d} d ${h} h` : h ? `${h} h ${m} min` : `${m} min`;
	}
</script>

<div class="page">
	<header class="bar">
		<div class="title">
			<span class="eyebrow">Overview</span>
			<h1>Dashboard</h1>
		</div>
		{#if summary}
			<div class="meta">
				<span class="chip mono">v{summary.version}</span>
				<span class="chip">up {uptime(summary.uptime_s)}</span>
				{#if summary.host.container}<span class="chip">container</span>{/if}
			</div>
		{/if}
	</header>

	{#if error}<p class="notice danger" role="alert"><Icon name="alert" /><span>{error}</span></p>{/if}
	{#if summary?.disk_low}
		<p class="notice write" role="alert">
			<Icon name="alert" /><span>Less than 10% disk space left on the data directory ({formatBytes(summary.host.disk_free)} of {formatBytes(summary.host.disk_total)} free). SQLite needs room for its write-ahead log and upgrades need room for a backup.</span>
		</p>
	{/if}

	{#if summary}
		{@const s = summary}
		<section class="tiles" aria-label="Now">
			<div class="card tile">
				<span class="eyebrow">Requests / s</span>
				<span class="num">{rate(s.requests.per_second)}</span>
				<span class="sub">{s.requests.last_hour} in the last hour</span>
			</div>
			<div class="card tile">
				<span class="eyebrow">Error rate</span>
				<span class="num" class:bad={s.requests.error_rate > 0.05}>{pct(s.requests.error_rate)}</span>
				<span class="sub">last minute, 4xx and 5xx</span>
			</div>
			<div class="card tile">
				<span class="eyebrow">Latency p95</span>
				<span class="num">{ms(s.requests.latency_ms.p95)}</span>
				<span class="sub mono">p50 {ms(s.requests.latency_ms.p50)} · p99 {ms(s.requests.latency_ms.p99)}</span>
			</div>
			<div class="card tile">
				<span class="eyebrow">Realtime</span>
				<span class="num">{s.realtime.connections}</span>
				<span class="sub">connections, {s.realtime.events} events sent</span>
			</div>
			<div class="card tile">
				<span class="eyebrow">Database</span>
				<span class="num">{formatBytes(s.db.data_bytes)}</span>
				<span class="sub">write queue {s.db.write_queue}, logs {formatBytes(s.db.logs_bytes)}</span>
			</div>
			<div class="card tile">
				<span class="eyebrow">Files</span>
				<span class="num">{formatBytes(s.storage.bytes)}</span>
				<span class="sub">{s.storage.files} files on {s.storage.backend}</span>
			</div>
		</section>

		<section class="card section" aria-label="Machine">
			<div class="card-head">
				<span class="ico"><Icon name="layout" /></span><span class="eyebrow">{s.host.container ? 'Container' : 'Host'}</span>
			</div>
			<div class="meters">
				{#each [
					['CPU', share(s.host.cpu_percent, 100), `${s.host.cpu_percent.toFixed(0)}% of ${s.host.cpus % 1 ? s.host.cpus.toFixed(1) : s.host.cpus} CPU${s.host.cpus === 1 ? '' : 's'}`],
					['Memory', share(s.host.memory_used, s.host.memory_total), `${formatBytes(s.host.memory_used)} of ${formatBytes(s.host.memory_total)}${s.host.container && s.host.memory_total < s.host.host_memory_total ? ' (container limit)' : ''}`],
					['Disk', share(s.host.disk_total - s.host.disk_free, s.host.disk_total), `${formatBytes(s.host.disk_total - s.host.disk_free)} of ${formatBytes(s.host.disk_total)} used`]
				] as const as [label, value, text] (label)}
					<div class="meter">
						<div class="meter-head"><span>{label}</span><span class="mono">{text}</span></div>
						<div class="track" role="meter" aria-label={label} aria-valuemin="0" aria-valuemax="100" aria-valuenow={Math.round(value * 100)} aria-valuetext={text}>
							<div class="fill" class:warn={value > 0.85} style:width="{Math.min(100, value * 100)}%"></div>
						</div>
					</div>
				{/each}
			</div>
			<p class="hint mono">load {s.host.load_avg.map((l) => l.toFixed(2)).join(' / ')} · process {formatBytes(s.host.process_rss)}</p>
		</section>

		<section class="charts" aria-label="Last hour">
			{#each [
				['http.rps', 'Requests / s', rate],
				['http.p95_ms', 'Latency p95', ms],
				['host.cpu_percent', 'CPU', (v: number) => `${v.toFixed(0)}%`],
				['host.memory_used', 'Memory used', formatBytes]
			] as const as [key, label, fmt] (key)}
				<div class="card chart-card">
					<span class="eyebrow">{label}</span>
					<LineChart {label} points={series[key] ?? []} format={fmt} />
				</div>
			{/each}
		</section>

		<div class="split">
			<section class="card section" aria-labelledby="routes-h">
				<h2 id="routes-h">Busiest routes</h2>
				{#if s.requests.top_routes.length}
					<table class="table">
						<thead><tr><th scope="col">Route</th><th scope="col" class="r">Requests</th><th scope="col" class="r">p95</th><th scope="col" class="r">Errors</th></tr></thead>
						<tbody>
							{#each s.requests.top_routes as r (r.method + r.route)}
								<tr>
									<td class="mono"><span class="method">{r.method}</span> {r.route}</td>
									<td class="r mono">{r.count}</td>
									<td class="r mono">{ms(r.p95_ms)}</td>
									<td class="r mono" class:none={!r.errors}>{r.errors || 'none'}</td>
								</tr>
							{/each}
						</tbody>
					</table>
				{:else}
					<p class="hint">No requests in the last hour.</p>
				{/if}
			</section>
			<section class="card section" aria-labelledby="errors-h">
				<h2 id="errors-h">Recent server errors</h2>
				{#if s.requests.recent_errors.length}
					<ul class="errors">
						{#each s.requests.recent_errors as e (e.request_id ?? e.ts)}
							<li>
								<span class="mono status">{e.status}</span>
								<span class="mono path">{e.method} {e.path}</span>
								<span class="mono when">{new Date(e.ts).toLocaleString()}</span>
							</li>
						{/each}
					</ul>
				{:else}
					<p class="hint"><Icon name="check" /> None recently.</p>
				{/if}
			</section>
		</div>
	{:else if !error}
		<p class="hint">Loading…</p>
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
		flex-wrap: wrap;
		margin-bottom: 8px;
	}
	.title {
		display: grid;
		gap: 6px;
	}
	.meta {
		display: flex;
		gap: 6px;
		flex-wrap: wrap;
	}
	.tiles {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(170px, 1fr));
		gap: 12px;
	}
	.tile {
		display: grid;
		gap: 6px;
		padding: 18px 20px;
	}
	.num {
		font: 500 26px/1.1 var(--font-sans);
		letter-spacing: -0.02em;
		font-variant-numeric: tabular-nums;
	}
	.num.bad {
		color: var(--danger);
	}
	.sub {
		font-size: 12.5px;
		color: var(--fg-subtle);
	}
	.section {
		padding: 22px 24px;
		display: grid;
		gap: 14px;
	}
	.meters {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
		gap: 18px;
	}
	.meter {
		display: grid;
		gap: 8px;
	}
	.meter-head {
		display: flex;
		justify-content: space-between;
		gap: 8px;
		font-size: 13px;
	}
	.meter-head .mono {
		color: var(--fg-muted);
		font-size: 12px;
	}
	.track {
		height: 8px;
		border-radius: 999px;
		background: var(--surface-2);
		overflow: hidden;
	}
	.fill {
		height: 100%;
		border-radius: 999px;
		background: var(--accent);
		transition: width 0.4s var(--ease-out);
	}
	.fill.warn {
		background: var(--write);
	}
	.charts {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(260px, 1fr));
		gap: 12px;
	}
	.chart-card {
		display: grid;
		gap: 14px;
		padding: 18px 20px 14px;
	}
	.split {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(380px, 1fr));
		gap: 12px;
		align-items: start;
	}
	h2 {
		font-size: 15px;
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
		padding: 0 8px 10px 0;
	}
	.table td {
		padding: 8px 8px 8px 0;
		border-top: 1px solid var(--border);
		overflow-wrap: anywhere;
	}
	.r {
		text-align: right;
	}
	.method {
		color: var(--accent-text);
	}
	.none {
		color: var(--fg-subtle);
	}
	.errors {
		display: grid;
		margin: 0;
		padding: 0;
		list-style: none;
	}
	.errors li {
		display: grid;
		grid-template-columns: auto 1fr;
		gap: 2px 10px;
		padding: 8px 0;
		border-top: 1px solid var(--border);
		font-size: 13px;
	}
	.status {
		color: var(--danger);
	}
	.path {
		overflow-wrap: anywhere;
	}
	.when {
		grid-column: 2;
		font-size: 11.5px;
		color: var(--fg-subtle);
	}
	@media (max-width: 480px) {
		.split {
			grid-template-columns: minmax(0, 1fr);
		}
	}
</style>
