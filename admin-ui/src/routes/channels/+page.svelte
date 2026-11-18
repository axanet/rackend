<script lang="ts">
	// Channels (SPEC §13.3): definitions with join and publish rules, and a
	// live view of the running instances and their members.
	import { onDestroy } from 'svelte';
	import { ApiError, del, get, message, patch, post } from '#lib/api.ts';
	import Icon from '#lib/components/Icon.svelte';
	import RuleInput from '#lib/components/RuleInput.svelte';

	interface Options {
		max_members: number;
		rate_per_s: number;
		echo: boolean;
		max_payload: number;
	}
	interface Channel {
		id: string;
		name: string;
		pattern: string;
		join_rule: string | null;
		publish_rule: string | null;
		presence: boolean;
		options: Options;
		instances: number;
		members: number;
	}
	interface Member {
		user_id: string | null;
		conn_id: string;
		collection: string | null;
		meta: unknown;
		joined_at: string;
	}
	interface Instance {
		channel: string;
		channel_id: string;
		messages: number;
		members: Member[];
	}
	interface Draft {
		id: string | null;
		name: string;
		pattern: string;
		join_rule: string | null;
		publish_rule: string | null;
		presence: boolean;
		options: Options;
	}

	const LIVE_EVERY_MS = 2000;

	let channels = $state<Channel[]>([]);
	let live = $state<Instance[]>([]);
	let published = $state(0);
	let error = $state<ApiError | null>(null);
	let notice = $state('');
	let busy = $state(false);
	let dialog = $state<HTMLDialogElement>();
	let draft = $state<Draft | null>(null);
	let draftError = $state<ApiError | null>(null);
	let timer: ReturnType<typeof setInterval> | undefined;

	const fail = (e: unknown) => (e instanceof ApiError ? e : new ApiError(0, { detail: message(e) }));

	async function load() {
		try {
			channels = (await get<{ items: Channel[] }>('/api/channels')).items;
		} catch (e) {
			error = fail(e);
		}
	}

	async function refreshLive() {
		if (document.hidden) return;
		try {
			const out = await get<{ items: Instance[]; published: number }>('/api/channels/live');
			live = out.items;
			published = out.published;
		} catch {
			// The next tick retries; the definitions list shows hard errors.
		}
	}

	load();
	refreshLive();
	timer = setInterval(refreshLive, LIVE_EVERY_MS);
	onDestroy(() => clearInterval(timer));

	function open(c: Channel | null) {
		draft = c
			? { id: c.id, name: c.name, pattern: c.pattern, join_rule: c.join_rule, publish_rule: c.publish_rule, presence: c.presence, options: { ...c.options } }
			: {
					id: null,
					name: '',
					pattern: '',
					join_rule: '@request.auth.id != ""',
					publish_rule: '',
					presence: true,
					options: { max_members: 1000, rate_per_s: 20, echo: false, max_payload: 16384 }
				};
		draftError = null;
		dialog?.showModal();
	}

	async function save(e: SubmitEvent) {
		e.preventDefault();
		if (!draft) return;
		busy = true;
		draftError = null;
		const { id, ...body } = $state.snapshot(draft);
		try {
			if (id) await patch(`/api/channels/${id}`, body);
			else await post('/api/channels', body);
			dialog?.close();
			notice = `Saved ${body.name}.`;
			await load();
		} catch (err) {
			draftError = fail(err);
		} finally {
			busy = false;
		}
	}

	async function remove(c: Channel) {
		if (!confirm(`Delete ${c.name}? Everyone in its channels is removed at once.`)) return;
		try {
			await del(`/api/channels/${c.id}`);
			notice = `Deleted ${c.name}.`;
			await load();
			await refreshLive();
		} catch (e) {
			error = fail(e);
		}
	}

	const derr = (k: string) => draftError?.field(k);
	const ruleText = (r: string | null) => (r === null ? 'superusers only' : r.trim() === '' ? 'anyone' : r);
	const time = (s: string) => new Date(s).toLocaleTimeString();
	const metaText = (m: unknown) => (m === null || m === undefined ? 'none' : JSON.stringify(m));
	const nameOf = (id: string) => channels.find((c) => c.id === id)?.name ?? 'deleted';
</script>

<div class="page">
	<header class="bar">
		<div class="title">
			<span class="eyebrow">Realtime</span>
			<h1>Channels</h1>
		</div>
		<div class="actions">
			<button type="button" class="btn primary" onclick={() => open(null)}><Icon name="plus" />Add channel</button>
		</div>
	</header>

	<div aria-live="polite">
		{#if error}
			<p class="notice danger" role="alert"><Icon name="alert" /><span>{error.message}</span></p>
		{:else if notice}
			<p class="notice ok"><Icon name="check" /><span>{notice}</span></p>
		{/if}
	</div>

	<section class="card section">
		<div class="card-head"><span class="ico"><Icon name="radio" /></span><span class="eyebrow">Definitions</span></div>
		<p class="hint">Clients join channels such as <code class="mono">room:42</code> over <code class="mono">/api/ws</code> to send ephemeral messages and see who is present. A pattern ending in <code class="mono">:*</code> defines one channel per key, available to the rules as <code class="mono">@channel.key</code>.</p>
		<div class="table-wrap">
			<table class="table">
				<thead>
					<tr><th scope="col">Name</th><th scope="col">Pattern</th><th scope="col">Join</th><th scope="col">Publish</th><th scope="col">Presence</th><th scope="col" class="num">Members</th><th scope="col"><span class="sr-only">Actions</span></th></tr>
				</thead>
				<tbody>
					{#each channels as c (c.id)}
						<tr>
							<td><strong>{c.name}</strong></td>
							<td class="mono">{c.pattern}</td>
							<td class="mono rule" title={ruleText(c.join_rule)}>{ruleText(c.join_rule)}</td>
							<td class="mono rule" title={ruleText(c.publish_rule)}>{ruleText(c.publish_rule)}</td>
							<td>{c.presence ? 'on' : 'off'}</td>
							<td class="num mono">{c.members}</td>
							<td class="row-actions">
								<button type="button" class="btn sm icon ghost" aria-label={`Edit ${c.name}`} title="Edit" onclick={() => open(c)}><Icon name="pencil" /></button>
								<button type="button" class="btn sm icon ghost danger" aria-label={`Delete ${c.name}`} title="Delete" onclick={() => remove(c)}><Icon name="trash" /></button>
							</td>
						</tr>
					{:else}
						<tr><td colspan="7" class="hint">none</td></tr>
					{/each}
				</tbody>
			</table>
		</div>
	</section>

	<section class="card section" aria-labelledby="live-title">
		<div class="card-head">
			<span class="ico"><Icon name="activity" /></span><span class="eyebrow" id="live-title">Live</span>
			<span class="subtle head-action mono">{published} messages since start</span>
		</div>
		{#each live as i (i.channel)}
			<div class="instance">
				<div class="instance-head">
					<strong class="mono">{i.channel}</strong>
					<span class="tag">{nameOf(i.channel_id)}</span>
					<span class="subtle">{i.members.length} {i.members.length === 1 ? 'member' : 'members'}, {i.messages} messages</span>
				</div>
				<div class="table-wrap">
					<table class="table">
						<thead><tr><th scope="col">User</th><th scope="col">Collection</th><th scope="col">Connection</th><th scope="col">Meta</th><th scope="col">Joined</th></tr></thead>
						<tbody>
							{#each i.members as m (m.conn_id)}
								<tr>
									<td class="mono">{m.user_id ?? 'anonymous'}</td>
									<td>{m.collection ?? 'none'}</td>
									<td class="mono">{m.conn_id}</td>
									<td class="mono meta" title={metaText(m.meta)}>{metaText(m.meta)}</td>
									<td>{time(m.joined_at)}</td>
								</tr>
							{/each}
						</tbody>
					</table>
				</div>
			</div>
		{:else}
			<p class="hint">No one is in a channel right now. This view refreshes every 2 seconds.</p>
		{/each}
	</section>
</div>

<dialog bind:this={dialog} aria-labelledby="channel-title">
	{#if draft}
		<form class="dialog" onsubmit={save}>
			<h2 id="channel-title">{draft.id ? `Edit ${draft.name}` : 'Add a channel'}</h2>
			{#if draftError && Object.keys(draftError.errors ?? {}).length === 0}
				<p class="notice danger" role="alert"><Icon name="alert" /><span>{draftError.message}</span></p>
			{/if}
			<div class="grid">
				<label class="field">
					Name
					<input type="text" required bind:value={draft.name} aria-invalid={derr('name') ? 'true' : undefined} />
					<span class={derr('name') ? 'error-text' : 'hint'}>{derr('name') ?? 'For you; clients use the pattern.'}</span>
				</label>
				<label class="field">
					Pattern
					<input type="text" class="mono" required bind:value={draft.pattern} placeholder="room:*" aria-invalid={derr('pattern') ? 'true' : undefined} />
					<span class={derr('pattern') ? 'error-text' : 'hint'}>{derr('pattern') ?? 'lobby, or room:* for one channel per key.'}</span>
				</label>
			</div>
			<RuleInput id="join-rule" label="join" operation="channel-join" bind:value={draft.join_rule} />
			<RuleInput id="publish-rule" label="publish" operation="channel-publish" bind:value={draft.publish_rule} />
			<div class="grid">
				<label class="field">
					Max members
					<input type="number" min="1" max="100000" bind:value={draft.options.max_members} aria-invalid={derr('options.max_members') ? 'true' : undefined} />
					{#if derr('options.max_members')}<span class="error-text">{derr('options.max_members')}</span>{/if}
				</label>
				<label class="field">
					Messages per second
					<input type="number" min="1" max="1000" bind:value={draft.options.rate_per_s} aria-invalid={derr('options.rate_per_s') ? 'true' : undefined} />
					<span class={derr('options.rate_per_s') ? 'error-text' : 'hint'}>{derr('options.rate_per_s') ?? 'Per connection.'}</span>
				</label>
				<label class="field">
					Max message bytes
					<input type="number" min="1" max="65536" bind:value={draft.options.max_payload} aria-invalid={derr('options.max_payload') ? 'true' : undefined} />
					{#if derr('options.max_payload')}<span class="error-text">{derr('options.max_payload')}</span>{/if}
				</label>
			</div>
			<div class="checks">
				<label class="inline"><input type="checkbox" bind:checked={draft.presence} />Presence (member lists, join and leave)</label>
				<label class="inline"><input type="checkbox" bind:checked={draft.options.echo} />Senders receive their own messages</label>
			</div>
			<div class="actions">
				<button type="button" class="btn" onclick={() => dialog?.close()}>Cancel</button>
				<button type="submit" class="btn primary" disabled={busy}>Save</button>
			</div>
		</form>
	{/if}
</dialog>

<style>
	.page {
		display: grid;
		gap: 16px;
		max-width: 1100px;
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
		min-width: 0;
	}
	.head-action {
		margin-left: auto;
		font-size: 13px;
	}
	.table-wrap {
		overflow-x: auto;
	}
	.table {
		width: 100%;
		border-collapse: collapse;
		font-size: 13.5px;
	}
	.table th {
		text-align: left;
		font: 500 11px/1 var(--font-mono);
		text-transform: uppercase;
		letter-spacing: 0.08em;
		color: var(--fg-subtle);
		padding: 12px;
		border-bottom: 1px solid var(--border);
		background: var(--surface);
		white-space: nowrap;
	}
	.table td {
		padding: 9px 12px;
		border-bottom: 1px solid var(--border);
		vertical-align: middle;
	}
	.table td.mono {
		font-size: 12.5px;
		white-space: nowrap;
	}
	.table .num {
		text-align: right;
	}
	.rule,
	.meta {
		max-width: 260px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-size: 13px;
	}
	.row-actions {
		display: flex;
		gap: 6px;
		justify-content: flex-end;
	}
	.instance {
		display: grid;
		gap: 8px;
		padding-top: 14px;
		border-top: 1px solid var(--border);
	}
	.instance:first-of-type {
		padding-top: 0;
		border-top: 0;
	}
	.instance-head {
		display: flex;
		align-items: center;
		gap: 10px;
		flex-wrap: wrap;
	}
	.instance-head .subtle {
		font-size: 13px;
	}
	dialog {
		width: min(680px, calc(100vw - 32px));
	}
	.dialog {
		display: grid;
		gap: 14px;
	}
	.grid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
		align-items: start;
		gap: 14px;
	}
	.checks {
		display: grid;
		gap: 2px;
	}
	.inline {
		display: inline-flex;
		align-items: center;
		gap: 8px;
		min-height: 32px;
		font-size: 14px;
		cursor: pointer;
	}
	.actions {
		display: flex;
		gap: 8px;
		justify-content: flex-end;
	}
	@media (max-width: 480px) {
		.section {
			padding: 20px;
		}
	}
</style>
