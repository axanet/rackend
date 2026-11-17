// Serves dist/ on http://localhost:5173 (any path falls back to index.html).
const dir = new URL('./dist/', import.meta.url).pathname;
const port = Number(process.env.PORT ?? 5173);
Bun.serve({
	port,
	async fetch(req) {
		const path = new URL(req.url).pathname;
		const file = Bun.file(dir + (path === '/' ? 'index.html' : path.slice(1)));
		return (await file.exists()) ? new Response(file) : new Response(Bun.file(dir + 'index.html'));
	}
});
console.log(`http://localhost:${port}/`);
