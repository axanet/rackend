<script lang="ts">
	// First boot: create the first superuser and confirm the public URL.
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
	import { ApiError, message, post } from '#lib/api.ts';
	import { session, type Superuser } from '#lib/session.svelte.ts';
	import AuthCard from '../AuthCard.svelte';

	let token = $state(page.url.searchParams.get('token') ?? '');
	let email = $state('');
	let password = $state('');
	let publicUrl = $state(location.origin);
	let error = $state<ApiError | null>(null);
	let busy = $state(false);

	async function submit(e: SubmitEvent) {
		e.preventDefault();
		busy = true;
		error = null;
		try {
			const res = await post<{ record: Superuser }>(
				'/api/setup',
				{ token, email, password, public_url: publicUrl },
				{ 'X-Rackend-Auth-Mode': 'cookie' }
			);
			session.signIn(res.record);
			await goto(resolve('/collections'), { replaceState: true });
		} catch (err) {
			error = err instanceof ApiError ? err : new ApiError(0, { detail: message(err) });
		} finally {
			busy = false;
		}
	}
</script>

<AuthCard title="Set up rackend" subtitle="Create the first superuser. You can add more later.">
	<form onsubmit={submit}>
		{#if error && !Object.keys(error.errors).length}<p class="error-text" role="alert">{error.message}</p>{/if}
		{#if !page.url.searchParams.get('token')}
			<label class="field">
				Setup token
				<input type="text" class="mono" bind:value={token} required autocomplete="off" />
				<span class="hint">Printed in the server log on startup.</span>
			</label>
		{/if}
		<label class="field">
			Email
			<input type="email" bind:value={email} required autocomplete="username" aria-invalid={error?.field('email') ? 'true' : undefined} />
			{#if error?.field('email')}<span class="error-text">{error.field('email')}</span>{/if}
		</label>
		<label class="field">
			Password
			<input type="password" bind:value={password} required minlength="10" autocomplete="new-password" aria-invalid={error?.field('password') ? 'true' : undefined} aria-describedby="pw-hint" />
			<span id="pw-hint" class={error?.field('password') ? 'error-text' : 'hint'}>{error?.field('password') ?? 'At least 10 characters.'}</span>
		</label>
		<label class="field">
			Public URL
			<input type="url" bind:value={publicUrl} required aria-invalid={error?.field('public_url') ? 'true' : undefined} aria-describedby="url-hint" />
			<span id="url-hint" class={error?.field('public_url') ? 'error-text' : 'hint'}>
				{error?.field('public_url') ?? 'The address users reach this instance at. Needed for OIDC, passkeys and email links.'}
			</span>
		</label>
		<button class="btn primary" disabled={busy}>{busy ? 'Creating…' : 'Create superuser'}</button>
	</form>
</AuthCard>
