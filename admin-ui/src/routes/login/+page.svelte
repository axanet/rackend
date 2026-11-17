<script lang="ts">
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { message, post } from '#lib/api.ts';
	import { session, type Superuser } from '#lib/session.svelte.ts';
	import AuthCard from '../AuthCard.svelte';

	let email = $state('');
	let password = $state('');
	let error = $state('');
	let busy = $state(false);

	async function submit(e: SubmitEvent) {
		e.preventDefault();
		busy = true;
		error = '';
		try {
			const res = await post<{ record: Superuser }>(
				'/api/superusers/auth-with-password',
				{ email, password },
				{ 'X-Rackend-Auth-Mode': 'cookie' }
			);
			session.signIn(res.record);
			await goto(resolve('/collections'), { replaceState: true });
		} catch (err) {
			error = message(err).startsWith('Missing, invalid') ? 'Wrong email or password.' : message(err);
		} finally {
			busy = false;
		}
	}
</script>

<AuthCard eyebrow="Admin" title="Sign in" subtitle="Superuser access to this rackend instance.">
	<form onsubmit={submit}>
		<div aria-live="polite">{#if error}<p class="error-text" role="alert">{error}</p>{/if}</div>
		<label class="field">Email<input type="email" bind:value={email} required autocomplete="username" /></label>
		<label class="field">Password<input type="password" bind:value={password} required autocomplete="current-password" /></label>
		<button class="btn primary" disabled={busy}>{busy ? 'Signing in…' : 'Sign in'}</button>
	</form>
</AuthCard>
