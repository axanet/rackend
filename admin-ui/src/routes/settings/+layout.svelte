<script lang="ts">
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
	import Shell from '#lib/components/Shell.svelte';
	import type { LayoutProps } from './$types';

	let { children }: LayoutProps = $props();
	const tabs = [
		['/settings', 'File storage'],
		['/settings/email', 'Email'],
		['/settings/providers', 'Sign-in providers'],
		['/settings/backups', 'Backups']
	] as const;
</script>

<Shell>
	<nav class="tabs" aria-label="Settings sections">
		{#each tabs as [href, label] (href)}
			<a href={resolve(href)} aria-current={page.url.pathname.replace(/\/$/, '').endsWith(href) ? 'page' : undefined}>{label}</a>
		{/each}
	</nav>
	{@render children()}
</Shell>

<style>
	.tabs {
		display: inline-flex;
		gap: 2px;
		padding: 3px;
		margin-bottom: 20px;
		border: 1px solid var(--border);
		border-radius: 999px;
		background: var(--surface);
	}
	.tabs a {
		padding: 6px 14px;
		border-radius: 999px;
		font-size: 13px;
		font-weight: 500;
		color: var(--fg-muted);
		text-decoration: none;
	}
	.tabs a:hover {
		color: var(--fg);
	}
	.tabs a[aria-current='page'] {
		background: var(--bg-elev);
		color: var(--fg);
		box-shadow: 0 0 0 1px var(--border-strong);
	}
</style>
