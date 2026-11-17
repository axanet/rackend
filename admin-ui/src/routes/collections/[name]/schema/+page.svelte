<script lang="ts">
	import { page } from '$app/state';
	import { resolve } from '$app/paths';
	import CollectionEditor from '#lib/components/CollectionEditor.svelte';
	import Icon from '#lib/components/Icon.svelte';
	import { session } from '#lib/session.svelte.ts';

	const collection = $derived(session.collections.find((c) => c.name === page.params.name) ?? null);
</script>

<a class="back" href={resolve('/collections/[name]', { name: page.params.name ?? '' })}><Icon name="chevronLeft" size={16} />Records</a>
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
		gap: var(--space-1);
		margin-bottom: var(--space-3);
		color: var(--color-muted-foreground);
		text-decoration: none;
	}
	.back:hover {
		color: var(--color-foreground);
	}
</style>
