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
</script>

<svelte:head>
	<link rel="icon" href={favicon} type="image/svg+xml" />
	<title>rackend admin</title>
</svelte:head>

{@render children()}
