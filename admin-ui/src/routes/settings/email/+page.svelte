<script lang="ts">
	// Settings: SMTP, a test send that saves nothing, and the email
	// templates. The SMTP password comes back masked; sending the mask keeps it.
	import { ApiError, get, message, patch, post } from '#lib/api.ts';
	import Icon from '#lib/components/Icon.svelte';

	const MASK = '••••';

	interface Smtp {
		host: string;
		port: number;
		username: string;
		password: string | null;
		tls: 'starttls' | 'tls' | 'none';
		from_name: string;
		from_address: string;
	}
	interface Template {
		subject: string;
		text: string;
		html: string;
	}
	type Templates = Record<string, Template>;

	const TEMPLATES: [string, string, string][] = [
		['verification', 'Email verification', '{{link}} opens the hosted confirmation page.'],
		['password_reset', 'Password reset', '{{link}} opens the hosted page to choose a new password.'],
		['email_change', 'Email change', 'Sent to the new address. {{link}} confirms the change.'],
		['otp', 'One-time code', '{{code}} is the six-digit code, valid for 10 minutes.'],
		['magic_link', 'Magic link', '{{link}} returns to your app with the sign-in token.']
	];

	let smtp = $state<Smtp | null>(null);
	let templates = $state<Templates | null>(null);
	let saved = $state('');
	let error = $state<ApiError | null>(null);
	let notice = $state('');
	let busy = $state(false);
	let testTo = $state('');
	let test = $state<{ ok: boolean; ms?: number; error?: string } | null>(null);
	let current = $state('verification');

	async function load() {
		try {
			const s = await get<{ smtp: Smtp; email_templates: Templates }>('/api/settings');
			smtp = s.smtp;
			templates = s.email_templates;
			saved = JSON.stringify([s.smtp, s.email_templates]);
		} catch (e) {
			error = e instanceof ApiError ? e : new ApiError(0, { detail: message(e) });
		}
	}
	load();

	const dirty = $derived(smtp !== null && JSON.stringify([smtp, templates]) !== saved);
	const passwordStored = $derived(smtp?.password === MASK);
	const configured = $derived(!!smtp?.host.trim());

	async function save() {
		if (!smtp || !templates) return;
		busy = true;
		error = null;
		notice = '';
		try {
			const s = await patch<{ smtp: Smtp; email_templates: Templates }>('/api/settings', {
				smtp: $state.snapshot(smtp),
				email_templates: $state.snapshot(templates)
			});
			smtp = s.smtp;
			templates = s.email_templates;
			saved = JSON.stringify([s.smtp, s.email_templates]);
			notice = configured ? 'Saved. New email uses these settings.' : 'Saved. Email is off until a host is set.';
		} catch (e) {
			error = e instanceof ApiError ? e : new ApiError(0, { detail: message(e) });
		} finally {
			busy = false;
		}
	}

	async function runTest(e: SubmitEvent) {
		e.preventDefault();
		if (!smtp) return;
		busy = true;
		test = null;
		error = null;
		try {
			test = await post('/api/settings/test-smtp', { to: testTo, smtp: $state.snapshot(smtp) });
		} catch (err) {
			error = err instanceof ApiError ? err : new ApiError(0, { detail: message(err) });
		} finally {
			busy = false;
		}
	}

	const field = (path: string) => error?.field(path);
	const tpl = $derived(templates?.[current]);
</script>

<div class="page">
	<header class="bar">
		<div class="title">
			<span class="eyebrow">Settings</span>
			<h1>Email</h1>
		</div>
		<div class="actions">
			<button type="button" class="btn primary" disabled={busy || !dirty} onclick={save}>Save</button>
		</div>
	</header>

	<div aria-live="polite">
		{#if error}
			<p class="notice danger" role="alert"><Icon name="alert" /><span>{error.message}</span></p>
		{:else if notice}
			<p class="notice ok"><Icon name="check" /><span>{notice}</span></p>
		{/if}
	</div>

	{#if smtp && templates}
		<section class="card section">
			<div class="card-head">
				<span class="ico"><Icon name="mail" /></span><span class="eyebrow">SMTP server</span>
				<span class="tag">{configured ? 'on' : 'off'}</span>
			</div>
			<p class="hint">Verification, password reset, email change, one-time codes and magic links need this. Leave the host empty to turn email off.</p>
			<div class="grid">
				<label class="field">
					Host
					<input type="text" class="mono" bind:value={smtp.host} placeholder="smtp.example.com" autocomplete="off" />
				</label>
				<label class="field">
					Port
					<input type="number" min="1" max="65535" bind:value={smtp.port} aria-invalid={field('smtp.port') ? 'true' : undefined} />
					{#if field('smtp.port')}<span class="error-text">{field('smtp.port')}</span>{/if}
				</label>
				<label class="field">
					Encryption
					<select bind:value={smtp.tls}>
						<option value="starttls">STARTTLS (port 587)</option>
						<option value="tls">TLS (port 465)</option>
						<option value="none">None (local relays only)</option>
					</select>
				</label>
				<label class="field">Username<input type="text" class="mono" autocomplete="off" bind:value={smtp.username} /></label>
				<label class="field">
					Password
					<input
						type="password"
						autocomplete="new-password"
						value={passwordStored ? '' : (smtp.password ?? '')}
						placeholder={passwordStored ? 'stored; type to replace' : ''}
						oninput={(e) => smtp && (smtp.password = e.currentTarget.value === '' && passwordStored ? MASK : e.currentTarget.value)}
					/>
					<span class="hint">Encrypted at rest; never shown again.</span>
				</label>
				<label class="field">Sender name<input type="text" bind:value={smtp.from_name} placeholder="Acme" /></label>
				<label class="field">
					Sender address
					<input type="email" class="mono" bind:value={smtp.from_address} placeholder="noreply@example.com" aria-invalid={field('smtp.from_address') ? 'true' : undefined} />
					{#if field('smtp.from_address')}<span class="error-text">{field('smtp.from_address')}</span>{/if}
				</label>
			</div>
		</section>

		<section class="card section">
			<div class="card-head"><span class="ico"><Icon name="refresh" /></span><span class="eyebrow">Test</span></div>
			<form class="test" onsubmit={runTest}>
				<label class="field">
					Send a test email to
					<input type="email" class="mono" required bind:value={testTo} placeholder="you@example.com" />
				</label>
				<button type="submit" class="btn" disabled={busy || !configured}>Send test</button>
			</form>
			<div aria-live="polite">
				{#if test}
					{#if test.ok}
						<p class="notice ok"><Icon name="check" /><span>Sent in {test.ms} ms. Check the inbox. Nothing was saved.</span></p>
					{:else}
						<p class="notice danger"><Icon name="alert" /><span>{test.error}</span></p>
					{/if}
				{/if}
			</div>
			<p class="hint">Uses the values above, saved or not.</p>
		</section>

		<section class="card section">
			<div class="card-head"><span class="ico"><Icon name="pencil" /></span><span class="eyebrow">Templates</span></div>
			<div class="tpl-tabs" role="tablist" aria-label="Templates">
				{#each TEMPLATES as [key, label] (key)}
					<button type="button" role="tab" aria-selected={current === key} class:active={current === key} onclick={() => (current = key)}>{label}</button>
				{/each}
			</div>
			{#if tpl}
				<p class="hint">
					{TEMPLATES.find(([k]) => k === current)?.[2]} Also available: <code>{'{{app_name}}'}</code>, <code>{'{{email}}'}</code>. Values are HTML-escaped in the HTML body. An empty field uses the default.
				</p>
				<label class="field">Subject<input type="text" bind:value={tpl.subject} /></label>
				<label class="field">Plain text<textarea class="mono" rows="5" bind:value={tpl.text}></textarea></label>
				<label class="field">HTML<textarea class="mono" rows="7" bind:value={tpl.html}></textarea></label>
			{/if}
		</section>
	{/if}
</div>

<style>
	.page {
		display: grid;
		gap: 16px;
		max-width: 1000px;
	}
	.bar {
		display: flex;
		align-items: flex-end;
		justify-content: space-between;
		gap: 16px;
		flex-wrap: wrap;
		margin-bottom: 8px;
	}
	.title {
		display: grid;
		gap: 6px;
	}
	.actions {
		display: flex;
		gap: 8px;
	}
	.section {
		padding: 24px;
		display: grid;
		gap: 14px;
	}
	.card-head .tag {
		margin-left: auto;
	}
	.grid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
		gap: 14px;
		align-items: start;
	}
	.test {
		display: flex;
		gap: 12px;
		align-items: flex-end;
		flex-wrap: wrap;
	}
	.test .field {
		flex: 1 1 260px;
	}
	.tpl-tabs {
		display: flex;
		gap: 6px;
		flex-wrap: wrap;
	}
	.tpl-tabs button {
		min-height: 36px;
		padding: 0 14px;
		border: 1px solid var(--border);
		border-radius: 999px;
		background: transparent;
		color: var(--fg-muted);
		font: 500 13px/1 var(--font-sans);
		cursor: pointer;
	}
	.tpl-tabs button:hover {
		color: var(--fg);
		border-color: var(--border-strong);
	}
	.tpl-tabs button.active {
		color: var(--fg);
		border-color: var(--fg);
	}
	code {
		font-family: var(--font-mono);
		font-size: 12.5px;
	}
	@media (max-width: 480px) {
		.section {
			padding: 20px;
		}
	}
</style>
