# rackend

A self-hosted, single-binary backend-as-a-service written in Rust, inspired by PocketBase: one binary (or one container) gives you a database with an admin UI, a REST API with access rules, authentication (including an OIDC provider), file storage, realtime and observability. SQLite is the only database.

> Status: early development (milestones M0 and M1: data safety, collections, records, access rules, audit trail, superusers, minimal admin UI).

## Run

```sh
docker run -d -v rackend_data:/data -p 8080:8080 ghcr.io/axanet/rackend:0.1
curl localhost:8080/api/health
```

See [docs/deployment.md](docs/deployment.md).

## Develop

All tools and tasks are managed with [mise](https://mise.jdx.dev):

```sh
mise install          # Rust, bun, cargo-nextest, cargo-deny (pinned)
mise run build        # admin UI + release binary
mise run dev          # run the server (data in ./.data)
mise run test         # all tests
mise run test:upgrade # upgrade-path tests against committed fixtures
mise run lint         # fmt, clippy -D warnings, cargo-deny, migration immutability
```

Layout: `crates/core` (domain, use cases, ports; no I/O), adapter crates (`store-sqlite`, `http`, `blob-*`, …), `crates/rackend` (binary and composition root), `admin-ui` (SvelteKit). Architecture decisions are recorded in [docs/adr](docs/adr); error responses are documented in [docs/errors.md](docs/errors.md).

## License

To be decided before the first public release.
