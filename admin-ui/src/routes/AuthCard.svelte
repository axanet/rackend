<script lang="ts">
	import type { Snippet } from 'svelte';
	import Logo from '#lib/components/Logo.svelte';

	let { eyebrow, title, subtitle, children }: { eyebrow: string; title: string; subtitle: string; children: Snippet } = $props();
</script>

<main>
	<div class="top"><Logo /></div>
	<section class="card">
		<span class="eyebrow">{eyebrow}</span>
		<h1>{title}</h1>
		<p class="hint">{subtitle}</p>
		{@render children()}
	</section>
</main>

<style>
	main {
		min-height: 100vh;
		display: grid;
		align-content: center;
		justify-items: center;
		gap: 28px;
		padding: var(--gutter);
	}
	section {
		width: min(440px, 100%);
		padding: 32px;
		display: grid;
		gap: 10px;
	}
	h1 {
		font-size: 1.6rem;
	}
	section :global(form) {
		display: grid;
		gap: 18px;
		margin-top: 18px;
	}
	@media (max-width: 480px) {
		section {
			padding: 20px;
		}
	}
</style>
