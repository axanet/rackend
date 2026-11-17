# 0016. M7 OpenAPI and documentation details not fixed by the spec

- Status: accepted
- Date: 2026-11-17
- Spec: §10.12, §20 M7

## Context

M7 adds the generated OpenAPI document (HTTP, CLI, admin UI API page), the realtime protocol document and the guides. The acceptance asks for valid 3.1 and 3.0 documents, generated TypeScript and Python clients that work against a server, and a getting-started guide that runs verbatim. Building that pipeline surfaced several choices.

## Decision

### Generator

- `rackend_core::openapi` builds the document from the schema snapshot as plain JSON (no new dependency). It produces OpenAPI 3.1; 3.0.3 is a rewrite pass (`type: [X, "null"]` becomes `nullable: true`, `anyOf [X, null]` becomes `allOf [X]` + `nullable`).
- Writers for JSON and YAML live in the same module. YAML is block style with every string and key double-quoted with JSON escapes, which YAML 1.2 reads identically; no YAML crate is needed. A test in `scripts/openapi-clients.sh` checks that the YAML parses to the JSON document.
- Key order is deterministic: OpenAPI's usual top-level order (`openapi`, `info`, `servers`, `security`, `tags`, `paths`, `components`), `operationId`, `summary`, `parameters`, `requestBody`, `responses` first in operations, then alphabetical.
- Record responses: every field is present and nullable except `bool` (always a boolean) and `json` (any value); an auth collection's `email` is optional because it is hidden from other users.
- Select fields get a named enum schema (`PostsStatus`). Inline enums of a multipart parameter and of the matching model property make TypeScript generators emit two exports with the same name.
- Update modifiers (`tags+`, `+tags`, `tags-`, `views+`, `views-`) are listed in the description of `<Name>Update`, which allows extra properties, instead of as properties: generators map `tags+`, `+tags` and `tags-` to the same identifier and fail to compile.
- Collections with file fields describe create and update bodies as `multipart/form-data` only (`<Name>Multipart`, with an optional `@json` part). Generators pick one media type per operation, and the Python generator prefers a JSON one whenever it is listed, then sends form fields under a JSON content type. The server still accepts JSON bodies; the request body description says so.
- Multipart text parts are now typed by their field: numbers, booleans (`true`, `True`, `1`, `on`), JSON, and lists (repeated parts, or a JSON array) for multi-value select and relation fields. Form-based clients send everything as text.
- Record creates are documented as `201` (as served) with `204` when the caller may not view the result; record id path parameters are `format: uuid`, matching the record schema.
- The public document (`api.openapi_public`) leaves out operations whose rule is locked. Superusers and the CLI see all of them. `?include=admin` (superusers only) adds the admin endpoints with generic object schemas.
- Auth operations follow the collection's enabled methods; provider operations appear when the collection has an enabled provider. The OIDC security scheme arrives with the OIDC provider (M8).

### CLI

- `rackend openapi export` reads `_collections`, `_settings` and `_auth_providers` through a read-only SQLite connection, so it works next to a running server. `RACKEND_PUBLIC_URL` sets the server URL like for `serve`. Its output equals the HTTP document byte for byte (checked in the client pipeline).

### Acceptance pipeline

- `scripts/openapi-clients.sh` (`mise run test:clients`, CI job `clients`): starts the binary with a fixture covering every field type, validates the 3.1 and 3.0 documents in JSON and YAML, with and without admin endpoints (`openapi-spec-validator` 0.9.0), generates `typescript-fetch` and `python` clients with openapi-generator 7.17.0 (Docker image), compiles the TypeScript client with `tsc --strict`, installs the Python client, and runs `scripts/clients/smoke.ts` and `smoke.py`: sign up, password sign-in, create with a file upload, list, update, download the file, delete. Clients generated from the 3.0 document are compiled too.
- `scripts/getting-started.sh` runs every `bash` block of `docs/guide/getting-started.md` in order in an empty directory and checks what a reader should see. CI tags the image it built with the guide's image name first.
- `tests/api/docs_examples.rs` runs the list-semantics table of the rules guide.

### Documentation

- `docs/realtime-protocol.md` documents the SSE and WebSocket protocols as implemented (including `connect`, `resync`, `close`, close code 4000) and marks channels and collaborative editing as coming with M9.
- `docs/errors.md` had the WebSocket and SSE error formats wrong (a nested `problem` object, an `error` SSE event); it now describes what the server sends.
- Guides live in `docs/guide/` (getting started, generating a client, rules). Deployment, upgrades, backup and restore and the NFS warning stay in `docs/deployment.md`.
- The admin UI gets an API page: downloads of both versions in JSON and YAML (optionally with admin endpoints) and, per collection, the endpoints with example `curl` requests built from the document.

## Consequences

- Clients generated for collections with files use multipart for every create and update. Modifiers from generated clients go into `@json` or extra properties.
- The CI `clients` job needs Docker, Python and bun, and downloads the generator image and Python packages.
