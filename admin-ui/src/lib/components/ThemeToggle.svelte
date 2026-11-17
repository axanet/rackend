<script lang="ts">
	// Cycles system → light → dark (Visual Identity: Implementation). The head
	// script in app.html applies the stored choice before first paint.
	import Icon from './Icon.svelte';

	type Mode = 'system' | 'light' | 'dark';
	const ORDER: Mode[] = ['system', 'light', 'dark'];
	const mq = matchMedia('(prefers-color-scheme: dark)');

	function read(): Mode {
		try {
			const v = localStorage.getItem('theme');
			return v === 'light' || v === 'dark' ? v : 'system';
		} catch {
			return 'system';
		}
	}

	let mode = $state<Mode>(read());

	function apply(m: Mode) {
		try {
			if (m === 'system') localStorage.removeItem('theme');
			else localStorage.setItem('theme', m);
		} catch {
			/* storage unavailable: the choice lasts for this page */
		}
		document.documentElement.dataset.theme = m === 'system' ? (mq.matches ? 'dark' : 'light') : m;
		mode = m;
	}

	$effect(() => {
		const follow = () => mode === 'system' && apply('system');
		mq.addEventListener('change', follow);
		return () => mq.removeEventListener('change', follow);
	});
</script>

<button
	type="button"
	class="btn sm icon ghost"
	aria-label="Theme: {mode}. Switch theme"
	title="Theme: {mode}"
	onclick={() => apply(ORDER[(ORDER.indexOf(mode) + 1) % ORDER.length])}
>
	<Icon name={mode === 'light' ? 'sun' : mode === 'dark' ? 'moon' : 'system'} />
</button>
