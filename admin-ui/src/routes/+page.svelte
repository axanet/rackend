<script lang="ts">
	// Entry: setup on first boot, else the collections or the login screen.
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { get, message } from '#lib/api.ts';
	import { session } from '#lib/session.svelte.ts';

	let error = $state('');

	$effect(() => {
		get<{ needs_setup: boolean }>('/api/info')
			.then((info) => {
				if (info.needs_setup) goto(resolve('/setup') + location.search, { replaceState: true });
				else goto(resolve(session.user ? '/dashboard' : '/login'), { replaceState: true });
			})
			.catch((e) => (error = message(e)));
	});
</script>

<main class="center">
	{#if error}<p class="error-text" role="alert">Cannot reach the server: {error}</p>{:else}<p class="hint">Loading…</p>{/if}
</main>

<style>
	.center {
		display: grid;
		place-items: center;
		min-height: 100vh;
	}
</style>
