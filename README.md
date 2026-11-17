# rackend

A self-hosted, single-binary backend-as-a-service written in Rust, inspired by PocketBase: one binary (or one container) gives you a database with an admin UI, a REST API with access rules, authentication (including an OIDC provider), file storage, realtime and observability. SQLite is the only database.

> Status: early development. Done: data safety, collections and access rules, auth (passwords, passkeys, email flows, external providers), files, realtime records, observability and backups, OpenAPI and client generation (milestones M0 to M7). Next: the OIDC provider (M8), then channels, presence and collaborative editing (M9).

## Run

```sh
docker run -d -v rackend_data:/data -p 8080:8080 ghcr.io/axanet/rackend:0.1
curl localhost:8080/api/health
```

Start with the [getting started guide](docs/guide/getting-started.md); all guides are in [docs/guide](docs/guide/README.md), deployment in [docs/deployment.md](docs/deployment.md).

## Develop

All tools and tasks are managed with [mise](https://mise.jdx.dev):

```sh
mise install          # Rust, bun, cargo-nextest, cargo-deny (pinned)
mise run build        # admin UI + release binary
mise run dev          # run the server (data in ./.data)
mise run test         # all tests
mise run test:upgrade # upgrade-path tests against committed fixtures
mise run test:clients # OpenAPI validation + generated TypeScript/Python clients (Docker, python3)
mise run test:broker  # sign-in against Keycloak (Docker)
mise run lint         # fmt, clippy -D warnings, cargo-deny, migration immutability
```

Layout: `crates/core` (domain, use cases, ports; no I/O), adapter crates (`store-sqlite`, `http`, `blob-*`, …), `crates/rackend` (binary and composition root), `admin-ui` (SvelteKit). Architecture decisions are recorded in [docs/adr](docs/adr); error responses are documented in [docs/errors.md](docs/errors.md).

## Contributing

Issues are welcome; pull requests from outside the axanet organization are closed automatically. See [CONTRIBUTING.md](CONTRIBUTING.md) and, for vulnerabilities, [SECURITY.md](SECURITY.md).

## License

Apache License 2.0, see [LICENSE](LICENSE) and [NOTICE](NOTICE). Bundled third-party material keeps its own license:

- The admin UI bundles the Redaction 20 font by MCKL and the Geist fonts by Vercel, all under the SIL Open Font License 1.1. Their license texts ship with the UI in [admin-ui/static/licenses](admin-ui/static/licenses) (served at `/_/licenses/`); the font files are used unmodified.
- The list of common passwords comes from SecLists by Daniel Miessler, under the MIT License ([crates/core/data/common-passwords.LICENSE](crates/core/data/common-passwords.LICENSE)).

The container image carries LICENSE and NOTICE in `/licenses/`.
