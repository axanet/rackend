<script lang="ts">
	import { page } from '$app/state';
	import { resolve } from '$app/paths';
	import CollectionEditor from '#lib/components/CollectionEditor.svelte';
	import Icon from '#lib/components/Icon.svelte';
	import { session } from '#lib/session.svelte.ts';

	const collection = $derived(session.collections.find((c) => c.name === page.params.name) ?? null);
</script>

<a class="back" href={resolve('/collections/[name]', { name: page.params.name ?? '' })}><Icon name="chevronLeft" />Records</a>
{#if collection}
	{#key collection.id + collection.updated}
		<CollectionEditor {collection} />
	{/key}
{:else}
	<p class="hint">Loading…</p>
{/if}

<style>
	.back {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		margin-bottom: 16px;
		font-size: 14px;
		color: var(--fg-muted);
		text-decoration: none;
	}
	.back:hover {
		color: var(--fg);
	}
</style>
