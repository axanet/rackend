<script lang="ts" module>
	// 16px grid, 1.4px stroke, round caps and joins, drawn in-house (Visual
	// Identity: Icons). Paths shared with the homepage use the same drawings.
	const paths = {
		db: 'M2.5 4c0-1.1 2.5-2 5.5-2s5.5.9 5.5 2-2.5 2-5.5 2-5.5-.9-5.5-2zM2.5 4v8c0 1.1 2.5 2 5.5 2s5.5-.9 5.5-2V4M2.5 8c0 1.1 2.5 2 5.5 2s5.5-.9 5.5-2',
		table: 'M2.5 3.5h11v9h-11zM2.5 6.5h11M6.5 6.5v6',
		lock: 'M4 7V5a4 4 0 018 0v2M3 7h10v7H3zM8 10v1.5',
		mail: 'M2 3.5h12v9H2zM2.5 4.5L8 9l5.5-4.5',
		key: 'M10 2a4 4 0 00-3.8 5.2L2 11.5V14h2.5v-1.5H6V11h1.5l1.3-1.3A4 4 0 1010 2zM11 5h.01',
		link: 'M6.5 9.5l3-3M7 4.5l1-1a2.8 2.8 0 014 4l-1 1M9 11.5l-1 1a2.8 2.8 0 01-4-4l1-1',
		activity: 'M1.5 8.5h3l2-5 3 9 2-4h3',
		list: 'M5.5 4h8M5.5 8h8M5.5 12h8M2.5 4h.01M2.5 8h.01M2.5 12h.01',
		download: 'M8 2.5v8M4.5 7L8 10.5 11.5 7M2.5 13.5h11',
		file: 'M4 1.5h5l3.5 3.5v9.5h-8.5zM9 1.5V5h3.5',
		upload: 'M8 10.5V2.5M4.5 6L8 2.5 11.5 6M2.5 10.5v3h11v-3',
		user: 'M8 8a2.75 2.75 0 100-5.5A2.75 2.75 0 008 8zM2.5 14c.6-2.8 2.8-4.5 5.5-4.5s4.9 1.7 5.5 4.5',
		history: 'M2.5 8a5.5 5.5 0 101.7-4M2 2v3h3M8 5v3l2 1.5',
		trash: 'M2.5 4h11M6 4V2.5h4V4M4 4l.7 9.5h6.6L12 4',
		check: 'M3 8.5l3 3 7-7',
		alert: 'M8 2L1.8 13h12.4zM8 6.5v3M8 11.3h.01',
		layout: 'M2.5 2.5h11v11h-11zM2.5 6h11M6.5 6v7.5',
		plus: 'M8 3v10M3 8h10',
		x: 'M4 4l8 8M12 4l-8 8',
		pencil: 'M10.5 2.5l3 3-8 8h-3v-3z',
		chevronLeft: 'M10 3.5L5.5 8l4.5 4.5',
		chevronRight: 'M6 3.5L10.5 8 6 12.5',
		search: 'M7 2.5a4.5 4.5 0 100 9 4.5 4.5 0 000-9zM13.5 13.5l-3.3-3.3',
		logout: 'M6 2.5H3.5a1 1 0 00-1 1v9a1 1 0 001 1H6M10.5 11l3-3-3-3M13.5 8h-7',
		sliders: 'M2.5 4.5h6M11.5 4.5h2M2.5 11.5h2M7.5 11.5h6M10 3v3M6 10v3',
		arrowUp: 'M8 13V3M4 7l4-4 4 4',
		arrowDown: 'M8 3v10M4 9l4 4 4-4',
		refresh: 'M13.5 8a5.5 5.5 0 11-1.6-3.9M13.5 2v3h-3',
		sun: 'M8 5a3 3 0 100 6 3 3 0 000-6zM8 1.5V3M8 13v1.5M1.5 8H3M13 8h1.5M3.4 3.4l1 1M11.6 11.6l1 1M3.4 12.6l1-1M11.6 4.4l1-1',
		moon: 'M13.5 9.6A5.6 5.6 0 016.4 2.5a5.6 5.6 0 107.1 7.1z',
		system: 'M3.5 2.5h9a1.5 1.5 0 011.5 1.5v5.5a1.5 1.5 0 01-1.5 1.5h-9A1.5 1.5 0 012 9.5V4a1.5 1.5 0 011.5-1.5zM6 14h4M8 11v3'
	} as const;
	export type IconName = keyof typeof paths;
</script>

<script lang="ts">
	let { name, size = 16, label }: { name: IconName; size?: number; label?: string } = $props();
</script>

<svg
	width={size}
	height={size}
	viewBox="0 0 16 16"
	fill="none"
	stroke="currentColor"
	stroke-width="1.4"
	stroke-linecap="round"
	stroke-linejoin="round"
	role={label ? 'img' : undefined}
	aria-label={label}
	aria-hidden={label ? undefined : 'true'}
	focusable="false"><path d={paths[name]} /></svg
>
