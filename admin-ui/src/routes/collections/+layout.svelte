<script lang="ts">
	import Shell from '#lib/components/Shell.svelte';
	import type { LayoutProps } from './$types';

	let { children }: LayoutProps = $props();
</script>

<Shell>{@render children()}</Shell>
