# 0005. Admin UI CSP allows the SvelteKit boot script by hash

- Status: accepted
- Date: 2026-11-17
- Spec: §16, §17.3

## Context
The admin UI CSP in §17.3 has no `script-src`, so scripts fall back to `default-src 'self'`. A SvelteKit `adapter-static` SPA starts with one inline `<script>` in `index.html`, which that policy blocks: the UI would not load.

## Decision
HTML responses of the admin UI get the §17.3 policy plus `script-src 'self' 'sha256-<hash>'`, where the hashes are computed by the server from the inline scripts of the served page. Other assets get the §17.3 policy unchanged. `'unsafe-inline'` is never allowed for scripts.

## Consequences
- The UI works with no weakening beyond the exact boot script of the shipped build.
- Hashes are computed per response from the embedded file (a few microseconds).
