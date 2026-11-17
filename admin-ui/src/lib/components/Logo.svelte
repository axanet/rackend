<script lang="ts">
	// Placeholder mark from the homepage: three stacked rack units with one
	// status light in the signal colour. Not the final logo (Visual Identity: Logo).
	let { size = 22, wordmark = true }: { size?: number; wordmark?: boolean } = $props();
</script>

<span class="logo">
	<svg width={size} height={size} viewBox="0 0 32 32" aria-hidden="true" focusable="false">
		<rect x="4" y="4.5" width="24" height="6.5" rx="2" fill="currentColor" />
		<rect x="4" y="12.75" width="24" height="6.5" rx="2" fill="currentColor" opacity="0.72" />
		<rect x="4" y="21" width="24" height="6.5" rx="2" fill="currentColor" opacity="0.44" />
		<circle cx="23.5" cy="7.75" r="1.7" fill="var(--accent)" />
	</svg>
	{#if wordmark}<span class="wordmark">rackend</span>{/if}
</span>

<style>
	.logo {
		display: inline-flex;
		align-items: center;
		gap: 9px;
		color: var(--fg);
	}
	.wordmark {
		font-family: var(--font-mono);
		font-weight: 600;
		font-size: 17px;
		letter-spacing: -0.03em;
	}
</style>
