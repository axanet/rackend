<script lang="ts">
	import type { Snippet } from 'svelte';
	let { title, subtitle, children }: { title: string; subtitle: string; children: Snippet } = $props();
</script>

<main>
	<section class="card">
		<div class="brand" aria-hidden="true"><span></span><span></span><span></span></div>
		<h1>{title}</h1>
		<p class="hint">{subtitle}</p>
		{@render children()}
	</section>
</main>

<style>
	main {
		min-height: 100vh;
		display: grid;
		place-items: center;
		padding: var(--space-4);
	}
	section {
		width: min(420px, 100%);
		padding: var(--space-6);
		display: grid;
		gap: var(--space-2);
	}
	section :global(form) {
		display: grid;
		gap: var(--space-4);
		margin-top: var(--space-4);
	}
	section :global(.btn.primary) {
		justify-content: center;
	}
	.brand {
		display: grid;
		gap: 3px;
		width: 28px;
		margin-bottom: var(--space-2);
	}
	.brand span {
		height: 6px;
		border-radius: 2px;
		background: var(--color-muted-foreground);
	}
	.brand span:first-child {
		background: var(--color-accent);
	}
	p {
		margin: 0;
	}
</style>
