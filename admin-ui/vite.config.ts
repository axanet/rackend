import adapter from '@sveltejs/adapter-static';
import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

export default defineConfig({
	plugins: [
		sveltekit({
			compilerOptions: {
				// Force runes mode for the project, except for libraries. Can be removed in svelte 6.
				runes: ({ filename }) =>
					filename.split(/[/\\]/).includes('node_modules') ? undefined : true
			},
			// Static SPA bundle embedded into the rackend binary and served at /_/ (SPEC §16).
			adapter: adapter({ fallback: 'index.html', strict: false }),
			paths: { base: '/_' }
		})
	],
	server: {
		// `mise run dev` + `mise run ui:dev`: API calls go to the Rust server.
		proxy: { '/api': 'http://127.0.0.1:8080' }
	}
});
