<script lang="ts">
	// API: the instance's OpenAPI document (downloads) and, per collection,
	// its endpoints and example requests, rendered from the document itself.
	import { ApiError, get, message } from '#lib/api.ts';
	import Icon from '#lib/components/Icon.svelte';

	type Schema = { type?: string | string[]; format?: string; enum?: string[]; items?: Schema; $ref?: string; anyOf?: Schema[]; properties?: Record<string, Schema>; required?: string[] };
	type Operation = { operationId: string; summary: string; tags: string[]; requestBody?: { content: Record<string, { schema: Schema }> } };
	type Doc = {
		info: { title: string; version: string };
		servers?: { url: string }[];
		tags: { name: string; description: string }[];
		paths: Record<string, Record<string, Operation>>;
		components: { schemas: Record<string, Schema> };
	};

	const ORDER = ['GET', 'POST', 'PATCH', 'PUT', 'DELETE'];

	let doc = $state<Doc | null>(null);
	let error = $state<ApiError | null>(null);
	let admin = $state(false);
	let current = $state('');
	let copied = $state('');

	async function load() {
		try {
			doc = await get<Doc>('/api/openapi.json');
			current ||= collections[0]?.name ?? '';
		} catch (e) {
			error = e instanceof ApiError ? e : new ApiError(0, { detail: message(e) });
		}
	}
	load();

	const base = $derived(doc?.servers?.[0]?.url ?? location.origin);
	const collections = $derived(doc?.tags.filter((t) => !['system', 'records', 'files', 'realtime', 'admin'].includes(t.name)) ?? []);
	const endpoints = $derived(
		doc
			? Object.entries(doc.paths).flatMap(([path, item]) =>
					Object.entries(item)
						.filter(([, op]) => op.tags?.includes(current))
						.map(([method, op]) => ({ method: method.toUpperCase(), path, op }))
				)
			.sort((a, b) => a.path.localeCompare(b.path) || ORDER.indexOf(a.method) - ORDER.indexOf(b.method))
			: []
	);
	const suffix = (version: string, format: string) => `/api/openapi.${format}?version=${version}${admin ? '&include=admin' : ''}`;

	function resolveRef(s: Schema | undefined): Schema | undefined {
		if (!s || !doc) return s;
		if (s.$ref) return resolveRef(doc.components.schemas[s.$ref.split('/').pop() ?? '']);
		if (s.anyOf) return resolveRef(s.anyOf.find((x) => x.type !== 'null'));
		return s;
	}

	/** A plausible example value for a schema. */
	function sample(s: Schema | undefined, name: string): unknown {
		const r = resolveRef(s);
		if (!r) return null;
		const type = Array.isArray(r.type) ? r.type.find((t) => t !== 'null') : r.type;
		if (r.enum) return r.enum[0];
		switch (type) {
			case 'array':
				return [sample(r.items, name)];
			case 'integer':
				return 1;
			case 'number':
				return 1.5;
			case 'boolean':
				return true;
			case 'object':
				return {};
			case 'string':
				if (r.format === 'uuid') return '01a1e000-0000-7000-8000-000000000000';
				if (r.format === 'date-time') return '2026-01-01T00:00:00Z';
				if (r.format === 'email') return 'ada@example.com';
				if (r.format === 'uri') return 'https://example.com';
				if (r.format === 'password') return 'a long password';
				return name === 'title' || name === 'name' ? 'Hello' : 'text';
			default:
				return { key: 'value' };
		}
	}

	function example(e: { method: string; path: string; op: Operation }): string {
		const path = e.path.replace('{id}', 'RECORD_ID');
		const lines = [`curl -X ${e.method} '${base}${path}' \\`, `  -H "Authorization: Bearer $TOKEN"`];
		const content = e.op.requestBody?.content;
		if (content?.['application/json']) {
			const schema = resolveRef(content['application/json'].schema);
			const body: Record<string, unknown> = {};
			for (const [k, v] of Object.entries(schema?.properties ?? {})) {
				if (k === 'id' || (schema?.required && !schema.required.includes(k) && Object.keys(body).length >= 3)) continue;
				body[k] = sample(v, k);
			}
			lines[lines.length - 1] += ' \\';
			lines.push(`  -H 'Content-Type: application/json' \\`, `  -d '${JSON.stringify(body)}'`);
		} else if (content?.['multipart/form-data']) {
			const schema = resolveRef(content['multipart/form-data'].schema);
			const parts = Object.entries(schema?.properties ?? {})
				.filter(([k]) => k !== '@json')
				.slice(0, 4)
				.map(([k, v]) => (resolveRef(v)?.format === 'binary' || resolveRef(resolveRef(v)?.items)?.format === 'binary' ? `  -F '${k}=@photo.jpg'` : `  -F '${k}=${String(sample(v, k))}'`));
			lines[lines.length - 1] += ' \\';
			lines.push(...parts.map((p, i) => (i < parts.length - 1 ? `${p} \\` : p)));
		}
		return lines.join('\n');
	}

	async function copy(text: string, key: string) {
		try {
			await navigator.clipboard.writeText(text);
			copied = key;
			setTimeout(() => (copied = ''), 1500);
		} catch {
			copied = '';
		}
	}

	const generate = $derived(
		`curl -H "Authorization: Bearer $TOKEN" '${base}/api/openapi.json' -o openapi.json\nnpx @openapitools/openapi-generator-cli generate -i openapi.json -g typescript-fetch -o rackend-client`
	);
</script>

<div class="page">
	<header class="bar">
		<div class="title">
			<span class="eyebrow">Developers</span>
			<h1>API</h1>
		</div>
	</header>

	{#if error}
		<p class="notice danger" role="alert"><Icon name="alert" /><span>{error.message}</span></p>
	{/if}

	{#if doc}
		<section class="card section">
			<div class="card-head"><span class="ico"><Icon name="download" /></span><span class="eyebrow">OpenAPI document</span><span class="tag mono">{doc.info.version}</span></div>
			<p class="hint">Generated from your collections; download it again after schema changes. Use 3.0 for generators with incomplete 3.1 support.</p>
			<div class="downloads">
				<a class="btn" href={suffix('3.1', 'json')} download="openapi-3.1.json"><Icon name="download" />3.1 JSON</a>
				<a class="btn" href={suffix('3.1', 'yaml')} download="openapi-3.1.yaml"><Icon name="download" />3.1 YAML</a>
				<a class="btn" href={suffix('3.0', 'json')} download="openapi-3.0.json"><Icon name="download" />3.0 JSON</a>
				<a class="btn" href={suffix('3.0', 'yaml')} download="openapi-3.0.yaml"><Icon name="download" />3.0 YAML</a>
			</div>
			<label class="inline"><input type="checkbox" bind:checked={admin} />Include superuser endpoints</label>
			<div class="snippet">
				<span class="eyebrow">Generate a TypeScript client</span>
				<pre class="well mono">{generate}</pre>
				<button type="button" class="btn sm" onclick={() => copy(generate, 'generate')}><Icon name={copied === 'generate' ? 'check' : 'copy'} />{copied === 'generate' ? 'Copied' : 'Copy'}</button>
			</div>
		</section>

		<section class="card section">
			<div class="card-head">
				<span class="ico"><Icon name="table" /></span><span class="eyebrow">Endpoints</span>
				<label class="pick">
					<span class="visually-hidden">Collection</span>
					<select bind:value={current}>
						{#each collections as c (c.name)}<option value={c.name}>{c.name}</option>{/each}
					</select>
				</label>
			</div>
			{#if endpoints.length === 0}
				<p class="hint">none</p>
			{/if}
			<ul class="eps">
				{#each endpoints as e (e.op.operationId)}
					<li>
						<details>
							<summary>
								<span class="verb mono" data-method={e.method}>{e.method}</span>
								<span class="path mono">{e.path}</span>
								<span class="sum">{e.op.summary}</span>
							</summary>
							<div class="detail">
								<span class="subtle mono">operationId: {e.op.operationId}</span>
								<pre class="well mono">{example(e)}</pre>
								<button type="button" class="btn sm" onclick={() => copy(example(e), e.op.operationId)}>
									<Icon name={copied === e.op.operationId ? 'check' : 'copy'} />{copied === e.op.operationId ? 'Copied' : 'Copy'}
								</button>
							</div>
						</details>
					</li>
				{/each}
			</ul>
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
	.card-head .tag {
		margin-left: auto;
	}
	.downloads {
		display: flex;
		flex-wrap: wrap;
		gap: 8px;
	}
	.inline {
		display: inline-flex;
		align-items: center;
		gap: 8px;
		min-height: 32px;
		font-size: 14px;
		cursor: pointer;
	}
	.snippet,
	.detail {
		display: grid;
		gap: 8px;
		justify-items: start;
		min-width: 0;
	}
	pre {
		width: 100%;
		margin: 0;
		padding: 12px 14px;
		overflow-x: auto;
		font-size: 12.5px;
		line-height: 1.55;
		white-space: pre;
	}
	.pick {
		margin-left: auto;
	}
	.pick select {
		min-height: 32px;
		width: auto;
	}
	.eps {
		display: grid;
		margin: 0;
		padding: 0;
		list-style: none;
	}
	.eps li {
		border-top: 1px solid var(--border);
	}
	.eps li:first-child {
		border-top: 0;
	}
	summary {
		display: flex;
		align-items: center;
		gap: 10px;
		min-height: 44px;
		cursor: pointer;
		flex-wrap: wrap;
	}
	.verb {
		width: 58px;
		font-size: 12px;
		font-weight: 600;
		color: var(--fg-muted);
	}
	.verb[data-method='GET'] {
		color: var(--accent);
	}
	.verb[data-method='DELETE'] {
		color: var(--danger);
	}
	.path {
		font-size: 13px;
		overflow-wrap: anywhere;
	}
	.sum {
		margin-left: auto;
		font-size: 13px;
		color: var(--fg-muted);
	}
	.detail {
		padding: 0 0 14px;
	}
	.detail .subtle {
		font-size: 12px;
	}
	.visually-hidden {
		position: absolute;
		width: 1px;
		height: 1px;
		overflow: hidden;
		clip: rect(0 0 0 0);
		white-space: nowrap;
	}
	@media (max-width: 480px) {
		.section {
			padding: 20px;
		}
		.sum {
			margin-left: 0;
			flex-basis: 100%;
		}
	}
</style>
