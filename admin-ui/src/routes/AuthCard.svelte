<script lang="ts">
	import type { Snippet } from 'svelte';
	import { asset } from '$app/paths';
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
	<p class="fonts">
		Set in Redaction 20 by MCKL (<a href={asset("licenses/redaction-OFL.txt")}>license</a>) and Geist by Vercel
		(<a href={asset("licenses/geist-OFL.txt")}>sans</a>, <a href={asset("licenses/geist-mono-OFL.txt")}>mono</a>), under the SIL Open Font
		License 1.1.
	</p>
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
	.fonts {
		max-width: 440px;
		text-align: center;
		font-size: 12px;
		color: var(--fg-subtle);
	}
	.fonts a {
		color: inherit;
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
