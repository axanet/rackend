<script lang="ts">
	import { resolve } from '$app/paths';
	import Icon from '#lib/components/Icon.svelte';
	import { session } from '#lib/session.svelte.ts';
</script>

<section class="card empty">
	<div class="card-head">
		<span class="ico"><Icon name="db" /></span>
		<span class="eyebrow">Collections</span>
	</div>
	<h1>{session.collections.length ? 'Pick a collection' : 'Create your first collection'}</h1>
	<p class="hint">
		A collection is a table with typed fields and access rules. Each one gets a REST API at
		<code>/api/collections/&lt;name&gt;/records</code>.
	</p>
	<a class="btn primary" href={resolve('/collections/new')}><Icon name="plus" />New collection</a>
</section>

<style>
	.empty {
		max-width: 560px;
		padding: 24px;
		display: grid;
		gap: 14px;
		justify-items: start;
	}
	.hint {
		font-size: 15px;
	}
</style>
