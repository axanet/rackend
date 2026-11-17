<script lang="ts">
	// App shell: collection navigation, settings, account and theme controls.
	// Signed-out visitors are sent to the login screen.
	import type { Snippet } from 'svelte';
	import { goto } from '$app/navigation';
	import { asset, resolve } from '$app/paths';
	import { page } from '$app/state';
	import Icon from './Icon.svelte';
	import Logo from './Logo.svelte';
	import ThemeToggle from './ThemeToggle.svelte';
	import { message } from '../api';
	import { session } from '../session.svelte';

	let { children }: { children: Snippet } = $props();
	let error = $state('');

	$effect(() => {
		if (!session.user) {
			goto(resolve('/login'), { replaceState: true });
			return;
		}
		session.loadCollections().catch((e) => (error = message(e)));
	});

	async function signOut() {
		await session.signOut();
		await goto(resolve('/login'));
	}

	const current = $derived(page.params.name);
	const inSettings = $derived(page.url.pathname.includes('/settings'));
</script>

<a class="skip" href="#main">Skip to content</a>
<div class="shell">
	<nav aria-label="Collections">
		<a class="home" href={resolve('/dashboard')} aria-label="rackend admin home"><Logo /></a>
		<div class="pages">
			{#each [['/dashboard', 'activity', 'Dashboard'], ['/logs', 'list', 'Logs'], ['/audit', 'history', 'Audit']] as const as [href, icon, label] (href)}
				<a class="settings" href={resolve(href)} aria-current={page.url.pathname.endsWith(href) ? 'page' : undefined}><Icon name={icon} />{label}</a>
			{/each}
		</div>
		<div class="nav-head">
			<span class="eyebrow">Collections</span>
			<a class="btn sm icon ghost" href={resolve('/collections/new')} aria-label="New collection" title="New collection"><Icon name="plus" /></a>
		</div>
		{#if error}<p class="error-text">{error}</p>{/if}
		<ul>
			{#each session.collections as c (c.id)}
				<li>
					<a href={resolve('/collections/[name]', { name: c.name })} aria-current={current === c.name ? 'page' : undefined}>
						<Icon name={c.type === 'auth' ? 'user' : 'table'} />{c.name}
					</a>
				</li>
			{:else}
				<li class="none">None yet</li>
			{/each}
		</ul>
		<a class="settings" href={resolve('/settings')} aria-current={inSettings ? 'page' : undefined}><Icon name="sliders" />Settings</a>
		<div class="foot">
			<span class="who mono" title={session.user?.email}>{session.user?.email}</span>
			<div class="row">
				<ThemeToggle />
				<button class="btn sm icon ghost" onclick={signOut} aria-label="Sign out" title="Sign out"><Icon name="logout" /></button>
				<a class="licenses" href={asset("licenses/redaction-OFL.txt")} title="Redaction 20 by MCKL and Geist by Vercel, SIL Open Font License 1.1">Font licenses</a>
			</div>
		</div>
	</nav>
	<main id="main" tabindex="-1">
		{@render children()}
	</main>
</div>

<style>
	.shell {
		display: grid;
		grid-template-columns: 248px minmax(0, 1fr);
		min-height: 100vh;
	}
	nav {
		position: sticky;
		top: 0;
		height: 100vh;
		display: flex;
		flex-direction: column;
		gap: 12px;
		padding: 16px 12px;
		border-right: 1px solid var(--border);
		background: var(--bg);
	}
	.home {
		display: inline-flex;
		padding: 6px 8px 14px;
		text-decoration: none;
	}
	.nav-head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding-left: 8px;
	}
	ul {
		display: grid;
		gap: 2px;
		align-content: start;
		flex: 1;
		margin: 0;
		padding: 0;
		list-style: none;
		overflow-y: auto;
	}
	li a {
		display: flex;
		align-items: center;
		gap: 10px;
		height: 36px;
		padding: 0 12px;
		border-radius: 999px;
		font-size: 14px;
		color: var(--fg-muted);
		text-decoration: none;
		transition:
			color 0.15s var(--ease-out),
			background-color 0.15s var(--ease-out);
	}
	li a:hover {
		color: var(--fg);
		background: var(--surface);
	}
	li a[aria-current='page'] {
		color: var(--fg);
		background: var(--surface-2);
		font-weight: 500;
	}
	li a[aria-current='page'] :global(svg) {
		color: var(--accent-text);
	}
	.none {
		padding: 0 12px;
		color: var(--fg-subtle);
		font-size: 14px;
	}
	.pages {
		display: grid;
		gap: 2px;
		padding-bottom: 12px;
		border-bottom: 1px solid var(--border);
	}
	.settings {
		display: flex;
		align-items: center;
		gap: 10px;
		height: 36px;
		padding: 0 12px;
		border-radius: 999px;
		font-size: 14px;
		color: var(--fg-muted);
		text-decoration: none;
	}
	.settings:hover {
		color: var(--fg);
		background: var(--surface);
	}
	.settings[aria-current='page'] {
		color: var(--fg);
		background: var(--surface-2);
		font-weight: 500;
	}
	.settings[aria-current='page'] :global(svg) {
		color: var(--accent-text);
	}
	.foot {
		display: grid;
		gap: 4px;
		padding-top: 12px;
		border-top: 1px solid var(--border);
	}
	.who {
		padding: 0 8px;
		font-size: 12px;
		color: var(--fg-subtle);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.row {
		display: flex;
		align-items: center;
		gap: 4px;
	}
	.licenses {
		margin-left: auto;
		padding: 0 8px;
		font-size: 12px;
		color: var(--fg-subtle);
		text-decoration: none;
	}
	.licenses:hover {
		color: var(--fg);
		text-decoration: underline;
	}
	main {
		min-width: 0;
		padding: 28px var(--gutter) 48px;
		outline: none;
	}
	.skip {
		position: absolute;
		left: 12px;
		top: -60px;
		z-index: 100;
		padding: 10px 14px;
		border-radius: var(--radius-sm);
		background: var(--fg);
		color: var(--bg);
		text-decoration: none;
		font-weight: 500;
	}
	.skip:focus {
		top: 12px;
	}
	@media (max-width: 760px) {
		.shell {
			grid-template-columns: minmax(0, 1fr);
		}
		nav {
			position: static;
			height: auto;
			border-right: 0;
			border-bottom: 1px solid var(--border);
		}
		ul {
			display: flex;
			flex-wrap: wrap;
			flex: none;
		}
		.foot {
			display: flex;
			justify-content: space-between;
			align-items: center;
		}
	}
</style>
