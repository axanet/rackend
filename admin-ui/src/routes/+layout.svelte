<script lang="ts">
	import '../app.css';
	import favicon from '#lib/assets/favicon.svg';
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { setUnauthorizedHandler } from '#lib/api.ts';
	import { session } from '#lib/session.svelte.ts';
	import type { LayoutProps } from './$types';

	let { children }: LayoutProps = $props();

	setUnauthorizedHandler(() => {
		session.clear();
		goto(resolve('/login'));
	});

	// Theme: system preference unless the user picked one.
	try {
		const t = localStorage.getItem('rackend.theme');
		if (t === 'light' || t === 'dark') document.documentElement.dataset.theme = t;
	} catch {
		/* storage unavailable */
	}
</script>

<svelte:head>
	<link rel="icon" href={favicon} />
	<title>rackend</title>
</svelte:head>

{@render children()}
