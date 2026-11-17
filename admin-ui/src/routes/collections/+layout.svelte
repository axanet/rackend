<script lang="ts">
	// App shell: collection navigation, account and theme controls.
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
	import Icon from '#lib/components/Icon.svelte';
	import { message } from '#lib/api.ts';
	import { session } from '#lib/session.svelte.ts';
	import type { LayoutProps } from './$types';

	let { children }: LayoutProps = $props();
	let error = $state('');
	let theme = $state(document.documentElement.dataset.theme ?? '');

	$effect(() => {
		if (!session.user) {
			goto(resolve('/login'), { replaceState: true });
			return;
		}
		session.loadCollections().catch((e) => (error = message(e)));
	});

	function toggleTheme() {
		const dark = theme ? theme === 'dark' : matchMedia('(prefers-color-scheme: dark)').matches;
		theme = dark ? 'light' : 'dark';
		document.documentElement.dataset.theme = theme;
		try {
			localStorage.setItem('rackend.theme', theme);
		} catch {
			/* ignore */
		}
	}

	async function signOut() {
		await session.signOut();
		await goto(resolve('/login'));
	}

	const current = $derived(page.params.name);
</script>

<a class="skip" href="#main">Skip to content</a>
<div class="shell">
	<nav aria-label="Collections">
		<div class="logo"><span class="mark" aria-hidden="true"></span>rackend</div>
		<div class="nav-head">
			<span>Collections</span>
			<a class="btn icon ghost" href={resolve('/collections/new')} aria-label="New collection" title="New collection"><Icon name="plus" /></a>
		</div>
		{#if error}<p class="error-text">{error}</p>{/if}
		<ul>
			{#each session.collections as c (c.id)}
				<li>
					<a href={resolve('/collections/[name]', { name: c.name })} aria-current={current === c.name ? 'page' : undefined}>
						<Icon name="database" size={16} />{c.name}
					</a>
				</li>
			{:else}
				<li class="hint">None yet</li>
			{/each}
		</ul>
		<div class="foot">
			<span class="hint who" title={session.user?.email}>{session.user?.email}</span>
			<div class="row">
				<button class="btn icon ghost" onclick={toggleTheme} aria-label="Switch color theme" title="Switch color theme">
					<Icon name={theme === 'light' ? 'moon' : 'sun'} />
				</button>
				<button class="btn icon ghost" onclick={signOut} aria-label="Sign out" title="Sign out"><Icon name="logout" /></button>
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
		grid-template-columns: 232px 1fr;
		min-height: 100vh;
	}
	nav {
		border-right: 1px solid var(--color-border);
		background: var(--color-card);
		display: flex;
		flex-direction: column;
		padding: var(--space-3);
		gap: var(--space-2);
		position: sticky;
		top: 0;
		height: 100vh;
	}
	.logo {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		font-weight: 700;
		padding: var(--space-2);
		font-family: var(--font-mono);
	}
	.mark {
		width: 14px;
		height: 14px;
		border-radius: 3px;
		background: var(--color-accent);
	}
	.nav-head {
		display: flex;
		justify-content: space-between;
		align-items: center;
		padding-left: var(--space-2);
		color: var(--color-muted-foreground);
		font-size: 0.75rem;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.04em;
	}
	ul {
		list-style: none;
		margin: 0;
		padding: 0;
		overflow-y: auto;
		flex: 1;
	}
	li a {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		padding: 6px var(--space-2);
		min-height: 36px;
		border-radius: var(--radius);
		text-decoration: none;
		color: var(--color-muted-foreground);
	}
	li a:hover {
		background: var(--color-muted);
		color: var(--color-foreground);
	}
	li a[aria-current='page'] {
		background: var(--color-muted);
		color: var(--color-foreground);
		font-weight: 600;
	}
	li.hint {
		padding: var(--space-2);
	}
	.foot {
		border-top: 1px solid var(--color-border);
		padding-top: var(--space-2);
		display: grid;
		gap: var(--space-1);
	}
	.who {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		padding: 0 var(--space-2);
	}
	.row {
		display: flex;
		gap: var(--space-1);
	}
	main {
		padding: var(--space-5);
		min-width: 0;
		outline: none;
	}
	.skip {
		position: absolute;
		left: -9999px;
	}
	.skip:focus {
		left: var(--space-2);
		top: var(--space-2);
		z-index: 10;
		background: var(--color-card);
		padding: var(--space-2);
	}
	@media (max-width: 760px) {
		.shell {
			grid-template-columns: 1fr;
		}
		nav {
			position: static;
			height: auto;
			border-right: 0;
			border-bottom: 1px solid var(--color-border);
		}
		ul {
			display: flex;
			flex-wrap: wrap;
			gap: var(--space-1);
			flex: none;
		}
		main {
			padding: var(--space-4);
		}
	}
</style>
