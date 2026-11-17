<script lang="ts">
	// A rule/filter editor with live compile checks against the (possibly
	// unsaved) collection definition. Errors show the exact position.
	import { ApiError, post } from '../api';

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
		collection: unknown;
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
				await post('/api/rules/check', { collection: def, rule, operation });
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
		<span class="label" id="{id}-label">{label}</span>
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
				<p class="error-text">
					{error.line ? `Line ${error.line}, column ${error.column}: ` : ''}{error.message}
				</p>
				{#if errorLine}
					<pre class="caret" aria-hidden="true">{errorLine}
{' '.repeat(Math.max(0, error.column - 1))}^</pre>
				{/if}
			{:else if checking}
				<p class="hint">Checking…</p>
			{/if}
		</div>
	{:else if mode === 'public' && (operation === 'update' || operation === 'delete')}
		<p class="warn"><strong>Anyone</strong>, including signed-out visitors, can {operation} every record.</p>
	{/if}
</div>

<style>
	.rule {
		display: grid;
		gap: var(--space-2);
	}
	.head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--space-3);
		flex-wrap: wrap;
	}
	.label {
		font-weight: 600;
		font-size: 0.875rem;
		text-transform: capitalize;
	}
	.modes {
		display: inline-flex;
		border: 1px solid var(--color-border);
		border-radius: var(--radius);
		overflow: hidden;
	}
	.modes button {
		border: 0;
		background: transparent;
		color: var(--color-muted-foreground);
		font: inherit;
		font-size: 0.8125rem;
		padding: 4px 10px;
		min-height: 30px;
		cursor: pointer;
	}
	.modes button + button {
		border-left: 1px solid var(--color-border);
	}
	.modes button.active {
		background: var(--color-muted);
		color: var(--color-foreground);
		font-weight: 600;
	}
	.caret {
		margin: 0;
		padding: var(--space-2);
		background: var(--color-code-bg);
		border-radius: var(--radius-sm);
		color: var(--color-destructive);
		overflow-x: auto;
	}
	.warn {
		margin: 0;
		color: var(--color-warning);
		font-size: 0.8125rem;
	}
</style>
