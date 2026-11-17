<script lang="ts">
	// Backups (SPEC §18.4): schedule, retention, on-demand archives and
	// downloads. Restoring is done on the server with the CLI.
	import { ApiError, del, formatBytes, get, message, patch, post } from '#lib/api.ts';
	import Icon from '#lib/components/Icon.svelte';

	interface Backup {
		name: string;
		size: number;
		created: string;
		includes_storage: boolean;
	}
	interface Settings {
		schedule: string;
		keep: number;
		include_storage: boolean;
	}

	let items = $state<Backup[]>([]);
	let form = $state<Settings | null>(null);
	let saved = $state('');
	let error = $state<ApiError | null>(null);
	let notice = $state('');
	let busy = $state(false);

	async function load() {
		try {
			const s = await get<{ backups: Settings }>('/api/settings');
			form = s.backups;
			saved = JSON.stringify(s.backups);
			items = (await get<{ items: Backup[] }>('/api/backups')).items;
		} catch (e) {
			error = e instanceof ApiError ? e : new ApiError(0, { detail: message(e) });
		}
	}
	load();

	const dirty = $derived(form !== null && JSON.stringify(form) !== saved);

	async function save() {
		if (!form) return;
		busy = true;
		error = null;
		try {
			const s = await patch<{ backups: Settings }>('/api/settings', { backups: $state.snapshot(form) });
			form = s.backups;
			saved = JSON.stringify(s.backups);
			notice = form.schedule.trim() ? 'Saved. Backups run on this schedule (UTC).' : 'Saved. Scheduled backups are off.';
		} catch (e) {
			error = e instanceof ApiError ? e : new ApiError(0, { detail: message(e) });
		} finally {
			busy = false;
		}
	}

	async function now() {
		busy = true;
		error = null;
		notice = '';
		try {
			const b = await post<Backup>('/api/backups');
			notice = `Wrote ${b.name} (${formatBytes(b.size)}).`;
			items = (await get<{ items: Backup[] }>('/api/backups')).items;
		} catch (e) {
			error = e instanceof ApiError ? e : new ApiError(0, { detail: message(e) });
		} finally {
			busy = false;
		}
	}

	async function remove(b: Backup) {
		if (!confirm(`Delete ${b.name}? This cannot be undone.`)) return;
		try {
			await del(`/api/backups/${encodeURIComponent(b.name)}`);
			items = items.filter((x) => x.name !== b.name);
		} catch (e) {
			error = e instanceof ApiError ? e : new ApiError(0, { detail: message(e) });
		}
	}
</script>

<div class="page">
	<header class="bar">
		<div class="title">
			<span class="eyebrow">Settings</span>
			<h1>Backups</h1>
		</div>
		<div class="actions">
			<button type="button" class="btn" disabled={busy} onclick={now}><Icon name="download" />Back up now</button>
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

	{#if form}
		<section class="card section">
			<div class="card-head"><span class="ico"><Icon name="history" /></span><span class="eyebrow">Schedule</span></div>
			<div class="grid">
				<label class="field">
					Cron schedule (UTC)
					<input type="text" class="mono" bind:value={form.schedule} placeholder="0 3 * * *" aria-invalid={error?.field('backups.schedule') ? 'true' : undefined} />
					<span class={error?.field('backups.schedule') ? 'error-text' : 'hint'}>{error?.field('backups.schedule') ?? 'Minute, hour, day, month, weekday. 0 3 * * * is daily at 03:00. Empty turns it off.'}</span>
				</label>
				<label class="field">
					Keep
					<input type="number" min="1" bind:value={form.keep} />
					<span class="hint">The newest archives stay; older ones are deleted after each scheduled backup.</span>
				</label>
			</div>
			<label class="inline"><input type="checkbox" bind:checked={form.include_storage} />Include uploaded files (filesystem storage only; thumbnails are left out)</label>
		</section>
	{/if}

	<section class="card section" aria-labelledby="list-h">
		<h2 id="list-h">Archives</h2>
		{#if items.length}
			<ul class="list">
				{#each items as b (b.name)}
					<li>
						<span class="mono name">{b.name}</span>
						<span class="mono subtle">{formatBytes(b.size)}</span>
						<span class="subtle">{new Date(b.created).toLocaleString()}</span>
						{#if b.includes_storage}<span class="tag">files</span>{:else}<span class="tag">database</span>{/if}
						<span class="row-actions">
							<a class="btn sm icon ghost" href="/api/backups/{encodeURIComponent(b.name)}" download aria-label="Download {b.name}" title="Download"><Icon name="download" /></a>
							<button class="btn sm icon ghost" aria-label="Delete {b.name}" title="Delete" onclick={() => remove(b)}><Icon name="trash" /></button>
						</span>
					</li>
				{/each}
			</ul>
		{:else}
			<p class="hint">None yet.</p>
		{/if}
		<p class="hint">
			Archives live in <code>backups/</code> in the data directory. To restore one, stop the server and run
			<code>rackend backup restore &lt;name&gt;</code>; the replaced files are moved to <code>backups/replaced-&lt;time&gt;/</code>, never deleted.
		</p>
	</section>
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
	.grid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
		gap: 14px;
		align-items: start;
	}
	.inline {
		display: inline-flex;
		align-items: center;
		gap: 8px;
		font-size: 14px;
	}
	h2 {
		font-size: 15px;
	}
	.list {
		display: grid;
		margin: 0;
		padding: 0;
		list-style: none;
	}
	.list li {
		display: flex;
		align-items: center;
		gap: 12px;
		flex-wrap: wrap;
		padding: 8px 0;
		border-top: 1px solid var(--border);
		font-size: 13px;
	}
	.name {
		flex: 1 1 260px;
		overflow-wrap: anywhere;
	}
	.row-actions {
		display: flex;
		gap: 4px;
	}
</style>
