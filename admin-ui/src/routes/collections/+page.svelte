<script lang="ts">
	import { resolve } from '$app/paths';
	import Icon from '#lib/components/Icon.svelte';
	import { session } from '#lib/session.svelte.ts';
</script>

<section class="empty card">
	<Icon name="database" size={28} />
	<h1>{session.collections.length ? 'Pick a collection' : 'Create your first collection'}</h1>
	<p class="hint">Collections are tables with typed fields and access rules. Each one gets a REST API at <code>/api/collections/&lt;name&gt;/records</code>.</p>
	<a class="btn primary" href={resolve('/collections/new')}><Icon name="plus" />New collection</a>
</section>

<style>
	.empty {
		max-width: 520px;
		padding: var(--space-6);
		display: grid;
		gap: var(--space-3);
		justify-items: start;
	}
	p {
		margin: 0;
	}
</style>
