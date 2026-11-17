# admin-ui

The rackend admin UI: a SvelteKit (Svelte 5) single-page app built with bun and `adapter-static`, embedded into the `rackend` binary and served at `/_/` (SPEC §16).

```sh
mise run ui:dev    # dev server; /api is proxied to http://127.0.0.1:8080 (mise run dev)
mise run ui:build  # static bundle in admin-ui/build, picked up by cargo build
```

Use the bun runtime (`bun --bun run …`); node is not required.
