<script lang="ts">
	// Settings: the app's name, logo and brand color (shown on the hosted
	// sign-in pages and in emails), and who may read the API document.
	import { ApiError, get, message, patch } from '#lib/api.ts';
	import Icon from '#lib/components/Icon.svelte';

	interface General {
		name: string;
		logo: string | null;
		primary_color: string | null;
		public_url: string | null;
		openapi_public: boolean;
	}

	const MAX_LOGO = 100 * 1024;

	let form = $state<General | null>(null);
	let saved = $state('');
	let error = $state<ApiError | null>(null);
	let notice = $state('');
	let busy = $state(false);
	let logoError = $state('');

	async function load() {
		try {
			const s = await get<{ app: Omit<General, 'openapi_public'>; api: { openapi_public: boolean } }>('/api/settings');
			form = { ...s.app, openapi_public: s.api.openapi_public };
			saved = JSON.stringify(form);
		} catch (e) {
			error = e instanceof ApiError ? e : new ApiError(0, { detail: message(e) });
		}
	}
	load();

	const dirty = $derived(form !== null && JSON.stringify(form) !== saved);

	function pickLogo(e: Event) {
		const file = (e.currentTarget as HTMLInputElement).files?.[0];
		logoError = '';
		if (!file || !form) return;
		if (!/^image\/(png|jpeg|webp|svg\+xml|gif)$/.test(file.type)) {
			logoError = 'Use a PNG, JPEG, WebP, SVG or GIF image.';
			return;
		}
		if (file.size > MAX_LOGO) {
			logoError = 'Use an image of at most 100 KB.';
			return;
		}
		const reader = new FileReader();
		reader.onload = () => form && (form.logo = String(reader.result));
		reader.readAsDataURL(file);
	}

	async function save() {
		if (!form) return;
		busy = true;
		error = null;
		notice = '';
		const f = $state.snapshot(form);
		try {
			const s = await patch<{ app: Omit<General, 'openapi_public'>; api: { openapi_public: boolean } }>('/api/settings', {
				app: { name: f.name, logo: f.logo, primary_color: f.primary_color, public_url: f.public_url },
				api: { openapi_public: f.openapi_public }
			});
			form = { ...s.app, openapi_public: s.api.openapi_public };
			saved = JSON.stringify(form);
			notice = 'Saved.';
		} catch (e) {
			error = e instanceof ApiError ? e : new ApiError(0, { detail: message(e) });
		} finally {
			busy = false;
		}
	}

	const field = (path: string) => error?.field(path);
</script>

<div class="page">
	<header class="bar">
		<div class="title">
			<span class="eyebrow">Settings</span>
			<h1>General</h1>
		</div>
		<button type="button" class="btn primary" disabled={busy || !dirty} onclick={save}>Save</button>
	</header>

	<div aria-live="polite">
		{#if error}
			<p class="notice danger" role="alert"><Icon name="alert" /><span>{error.message}</span></p>
		{:else if notice}
			<p class="notice ok"><Icon name="check" /><span>{notice}</span></p>
		{/if}
	</div>

	{#if form}
		<section class="card section">
			<div class="card-head"><span class="ico"><Icon name="layout" /></span><span class="eyebrow">Your app</span></div>
			<div class="grid">
				<label class="field">
					Name
					<input type="text" bind:value={form.name} />
					<span class="hint">On sign-in pages and in emails.</span>
				</label>
				<label class="field">
					Public URL
					<input type="url" class="mono" bind:value={form.public_url} placeholder="https://api.example.com" aria-invalid={field('app.public_url') ? 'true' : undefined} />
					<span class={field('app.public_url') ? 'error-text' : 'hint'}>{field('app.public_url') ?? 'The issuer of tokens and the base of links in emails.'}</span>
				</label>
			</div>
			<div class="brand">
				<div class="field">
					<span>Logo</span>
					<div class="logo-row">
						<div class="preview" aria-hidden="true">{#if form.logo}<img src={form.logo} alt="" />{:else}<span class="subtle">none</span>{/if}</div>
						<label class="btn sm">Choose image<input class="file" type="file" accept="image/png,image/jpeg,image/webp,image/svg+xml,image/gif" onchange={pickLogo} /></label>
						{#if form.logo}<button type="button" class="btn sm ghost" onclick={() => form && (form.logo = null)}>Remove</button>{/if}
					</div>
					<span class={logoError ? 'error-text' : 'hint'}>{logoError || 'Up to 100 KB, stored in the settings.'}</span>
				</div>
				<label class="field color">
					Brand color
					<span class="color-row">
						<input type="color" value={form.primary_color ?? '#0b0c0e'} oninput={(e) => form && (form.primary_color = e.currentTarget.value)} />
						<input type="text" class="mono" bind:value={form.primary_color} placeholder="#0a8f5f" />
					</span>
					<span class="hint">Buttons on the hosted sign-in pages.</span>
				</label>
			</div>
		</section>

		<section class="card section">
			<div class="card-head"><span class="ico"><Icon name="code" /></span><span class="eyebrow">API document</span></div>
			<label class="inline"><input type="checkbox" bind:checked={form.openapi_public} />Anyone may read the OpenAPI document</label>
			<p class="hint">It describes your collections and fields. Operations only superusers may call are left out of the public document.</p>
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
	.section {
		padding: 24px;
		display: grid;
		gap: 14px;
	}
	.grid,
	.brand {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
		gap: 14px;
		align-items: start;
	}
	.field {
		display: grid;
		gap: 6px;
		font-size: 14px;
		font-weight: 500;
	}
	.logo-row,
	.color-row {
		display: flex;
		align-items: center;
		gap: 10px;
		flex-wrap: wrap;
	}
	.preview {
		display: grid;
		place-items: center;
		width: 120px;
		height: 44px;
		border: 1px dashed var(--border-strong);
		border-radius: var(--radius);
		font-size: 13px;
	}
	.preview img {
		max-width: 110px;
		max-height: 36px;
	}
	.file {
		position: absolute;
		width: 1px;
		height: 1px;
		opacity: 0;
	}
	.btn:has(.file:focus-visible) {
		outline: 2px solid var(--accent);
		outline-offset: 2px;
	}
	input[type='color'] {
		width: 44px;
		height: 40px;
		padding: 2px;
		border: 1px solid var(--border-strong);
		border-radius: var(--radius);
		background: var(--bg-elev);
	}
	.color-row input[type='text'] {
		max-width: 140px;
	}
	.inline {
		display: inline-flex;
		align-items: center;
		gap: 8px;
		min-height: 32px;
		font-size: 14px;
		cursor: pointer;
	}
	@media (max-width: 480px) {
		.section {
			padding: 20px;
		}
	}
</style>
