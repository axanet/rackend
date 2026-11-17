<script lang="ts">
	// One time series as a 2px line over a faint area, with a crosshair and
	// tooltip on hover or arrow keys. Identity comes from the card title, so
	// there is no legend; a visually hidden table carries the numbers.
	let {
		label,
		points,
		format,
		height = 96
	}: { label: string; points: [string, number][]; format: (v: number) => string; height?: number } = $props();

	let width = $state(320);
	let active = $state<number | null>(null);
	const pad = { top: 8, right: 4, bottom: 18, left: 4 };

	const max = $derived(Math.max(...points.map((p) => p[1]), 0) || 1);
	const xs = $derived(points.map((_, i) => pad.left + (points.length < 2 ? 0 : (i / (points.length - 1)) * (width - pad.left - pad.right))));
	const ys = $derived(points.map((p) => pad.top + (1 - p[1] / max) * (height - pad.top - pad.bottom)));
	const line = $derived(xs.map((x, i) => `${i ? 'L' : 'M'}${x.toFixed(1)},${ys[i].toFixed(1)}`).join(''));
	const area = $derived(points.length ? `${line}L${xs[xs.length - 1].toFixed(1)},${height - pad.bottom}L${xs[0].toFixed(1)},${height - pad.bottom}Z` : '');
	const time = (ts: string) => new Date(ts).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });

	function pick(clientX: number, el: Element) {
		if (!points.length) return;
		const r = el.getBoundingClientRect();
		const x = clientX - r.left;
		let best = 0;
		for (let i = 1; i < xs.length; i++) if (Math.abs(xs[i] - x) < Math.abs(xs[best] - x)) best = i;
		active = best;
	}

	function key(e: KeyboardEvent) {
		if (!points.length) return;
		if (e.key === 'ArrowLeft') active = Math.max(0, (active ?? points.length) - 1);
		else if (e.key === 'ArrowRight') active = Math.min(points.length - 1, (active ?? -1) + 1);
		else if (e.key === 'Escape') active = null;
		else return;
		e.preventDefault();
	}
</script>

<div class="chart" bind:clientWidth={width}>
	{#if points.length < 2}
		<p class="empty">Collecting data. The first points appear after a minute or two.</p>
	{:else}
		<svg
			{width}
			{height}
			role="slider"
			aria-label="{label}, last hour"
			aria-valuemin={0}
			aria-valuemax={points.length - 1}
			aria-valuenow={active ?? points.length - 1}
			aria-valuetext="{time(points[active ?? points.length - 1][0])}: {format(points[active ?? points.length - 1][1])}"
			tabindex="0"
			onpointermove={(e) => pick(e.clientX, e.currentTarget)}
			onpointerleave={() => (active = null)}
			onkeydown={key}
			onblur={() => (active = null)}
		>
			<line class="base" x1={pad.left} x2={width - pad.right} y1={height - pad.bottom} y2={height - pad.bottom} />
			<path class="area" d={area} />
			<path class="line" d={line} />
			<text class="tick" x={pad.left} y={height - 4}>{time(points[0][0])}</text>
			<text class="tick" x={width - pad.right} y={height - 4} text-anchor="end">{time(points[points.length - 1][0])}</text>
			{#if active !== null}
				<line class="cross" x1={xs[active]} x2={xs[active]} y1={pad.top} y2={height - pad.bottom} />
				<circle class="dot" cx={xs[active]} cy={ys[active]} r="4" />
			{/if}
		</svg>
		{#if active !== null}
			<div class="tip" style:left="{Math.min(Math.max(xs[active], 60), width - 60)}px" aria-live="polite">
				<span class="mono">{time(points[active][0])}</span>
				<strong>{format(points[active][1])}</strong>
			</div>
		{/if}
		<table class="sr-only">
			<caption>{label}</caption>
			<thead><tr><th scope="col">Time</th><th scope="col">Value</th></tr></thead>
			<tbody>
				{#each points as p (p[0])}<tr><td>{time(p[0])}</td><td>{format(p[1])}</td></tr>{/each}
			</tbody>
		</table>
	{/if}
</div>

<style>
	.chart {
		position: relative;
		min-height: 60px;
	}
	svg {
		display: block;
		overflow: visible;
		outline: none;
		cursor: crosshair;
	}
	svg:focus-visible {
		outline: 2px solid var(--accent);
		outline-offset: 4px;
		border-radius: 4px;
	}
	.base {
		stroke: var(--border);
		stroke-width: 1;
	}
	.area {
		fill: color-mix(in srgb, var(--accent) 12%, transparent);
	}
	.line {
		fill: none;
		stroke: var(--accent);
		stroke-width: 2;
		stroke-linejoin: round;
		stroke-linecap: round;
	}
	.cross {
		stroke: var(--border-strong);
		stroke-width: 1;
	}
	.dot {
		fill: var(--accent);
		stroke: var(--bg-elev);
		stroke-width: 2;
	}
	.tick {
		font: 10px var(--font-mono);
		fill: var(--fg-subtle);
	}
	.tip {
		position: absolute;
		top: -6px;
		transform: translate(-50%, -100%);
		display: grid;
		gap: 2px;
		padding: 6px 10px;
		border: 1px solid var(--border);
		border-radius: var(--radius-sm);
		background: var(--bg-elev);
		box-shadow: var(--shadow-lg);
		font-size: 12px;
		white-space: nowrap;
		pointer-events: none;
		z-index: 2;
	}
	.tip span {
		color: var(--fg-subtle);
	}
	.tip strong {
		font-weight: 500;
		color: var(--fg);
	}
	.empty {
		margin: 0;
		padding: 18px 0;
		font-size: 13px;
		color: var(--fg-subtle);
	}
</style>
