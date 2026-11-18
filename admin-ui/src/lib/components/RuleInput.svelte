<script lang="ts">
	// A rule/filter editor with live compile checks against the (possibly
	// unsaved) collection definition. Errors show the exact position.
	import { ApiError, post } from '../api';
	import Icon from './Icon.svelte';

	let {
		value = $bindable(),
		label,
		operation,
		collection,
		id
	}: {
		value: string | null;
		label: string;
		operation: string;
		collection?: unknown;
		id: string;
	} = $props();

	type Mode = 'locked' | 'public' | 'expr';
	let mode = $derived<Mode>(value === null ? 'locked' : value.trim() === '' ? 'public' : 'expr');
	let error = $state<{ message: string; line: number; column: number } | null>(null);
	let checking = $state(false);
	let timer: ReturnType<typeof setTimeout> | undefined;

	function setMode(m: Mode) {
		value = m === 'locked' ? null : m === 'public' ? '' : value && value.trim() ? value : '@request.auth.id != ""';
	}

	$effect(() => {
		const rule = value;
		const def = $state.snapshot(collection);
		clearTimeout(timer);
		if (rule === null || rule.trim() === '') {
			error = null;
			return;
		}
		timer = setTimeout(async () => {
			checking = true;
			try {
				// `channel-join` / `channel-publish`: channel rules, no collection.
				const op = operation.startsWith('channel') ? 'channel' : operation;
				await post('/api/rules/check', { collection: def, rule, operation: op });
				error = null;
			} catch (e) {
				if (e instanceof ApiError && e.code === 'rule_compile_error' && e.position) {
					error = { message: e.detail ?? e.message, line: e.position.line, column: e.position.column };
				} else if (e instanceof ApiError && e.code === 'validation_failed') {
					// The collection itself is invalid; the editor shows those errors.
					error = null;
				} else {
					error = { message: e instanceof Error ? e.message : String(e), line: 0, column: 0 };
				}
			} finally {
				checking = false;
			}
		}, 300);
		return () => clearTimeout(timer);
	});

	let errorLine = $derived(error && value && error.line > 0 ? value.split('\n')[error.line - 1] ?? '' : '');
</script>

<div class="rule">
	<div class="head">
		<span class="op mono" id="{id}-label">{label}</span>
		<div class="modes" role="radiogroup" aria-labelledby="{id}-label">
			{#each [['locked', 'Superusers only'], ['public', 'Anyone'], ['expr', 'Rule']] as [m, text] (m)}
				<button
					type="button"
					role="radio"
					aria-checked={mode === m}
					class:active={mode === m}
					onclick={() => setMode(m as Mode)}>{text}</button
				>
			{/each}
		</div>
	</div>
	{#if mode === 'expr'}
		<textarea
			{id}
			class="mono"
			rows="2"
			spellcheck="false"
			aria-labelledby="{id}-label"
			aria-invalid={error ? 'true' : undefined}
			aria-describedby={error ? `${id}-error` : undefined}
			bind:value
		></textarea>
		<div aria-live="polite" id="{id}-error">
			{#if error}
				<p class="error-text">{error.line ? `Line ${error.line}, column ${error.column}: ` : ''}{error.message}</p>
				{#if errorLine}
					<pre class="well caret" aria-hidden="true">{errorLine}
{' '.repeat(Math.max(0, error.column - 1))}^</pre>
				{/if}
			{:else if checking}
				<p class="hint">Checking…</p>
			{/if}
		</div>
	{:else if mode === 'locked'}
		<p class="state subtle"><Icon name="lock" />Locked: superusers only.</p>
	{:else if operation === 'channel-publish'}
		<p class="state subtle">Every member of the channel.</p>
	{:else if operation === 'update' || operation === 'delete'}
		<p class="state write"><Icon name="alert" />Anyone, including signed-out visitors, can {operation} every record.</p>
	{:else}
		<p class="state subtle">Anyone, including signed-out visitors.</p>
	{/if}
</div>

<style>
	.rule {
		display: grid;
		gap: 8px;
	}
	.rule + :global(.rule) {
		padding-top: 16px;
		border-top: 1px solid var(--border);
	}
	.head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 12px;
		flex-wrap: wrap;
	}
	.op {
		font-size: 13px;
		font-weight: 500;
		color: var(--accent-text);
	}
	.modes {
		display: inline-flex;
		padding: 3px;
		gap: 2px;
		border: 1px solid var(--border);
		border-radius: 999px;
		background: var(--surface);
	}
	.modes button {
		height: 28px;
		padding: 0 12px;
		border: 0;
		border-radius: 999px;
		background: transparent;
		color: var(--fg-muted);
		font: 500 13px/1 var(--font-sans);
		cursor: pointer;
		transition:
			color 0.15s var(--ease-out),
			background-color 0.15s var(--ease-out);
	}
	.modes button:hover {
		color: var(--fg);
	}
	.modes button.active {
		background: var(--bg-elev);
		color: var(--fg);
		box-shadow: 0 0 0 1px var(--border-strong);
	}
	.caret {
		color: var(--danger);
		margin-top: 6px;
	}
	.state {
		display: inline-flex;
		align-items: center;
		gap: 8px;
		font-size: 13px;
	}
	.write {
		color: var(--write);
	}
</style>
