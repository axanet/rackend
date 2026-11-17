# rackend — Technical Specification

> **Status:** Draft v0.2 (2026-11-17)
> **Owner:** axanet (axanet GmbH) · Repository: https://github.com/axanet/rackend · Images: `ghcr.io/axanet/rackend`
> **Audience:** coding agents and human contributors.
> **Scope:** v1 of rackend — a self-hosted, single-binary backend-as-a-service written in Rust, inspired by PocketBase.

---

## 0. How to use this document

### 0.1 Normative language
- **MUST / MUST NOT**: hard requirement. Violating it is a bug.
- **SHOULD / SHOULD NOT**: strong default. Deviating requires a written reason in an ADR (`docs/adr/`).
- **MAY**: optional.

### 0.2 Rules for agents working from this spec
1. Work **milestone by milestone** (§20). Do not start a milestone before the previous one meets its acceptance criteria, unless the task says otherwise.
2. When this spec is ambiguous or silent, **do not invent product behaviour**: choose the most conservative option (safer, smaller surface, reversible), implement it, and record the decision in `docs/adr/NNNN-title.md` (context → decision → consequences).
3. When implementation forces a change to this spec, update the spec **in the same change** and note it in §22 (Changelog).
4. **Data safety (§7) wins over everything else**, including performance, deadlines and convenience.
5. Adding a dependency not listed in §3.3 requires an ADR entry (one paragraph is fine).
6. Every change ships with tests (§19). No test, not done.

### 0.3 Glossary
| Term | Meaning |
|---|---|
| **Instance** | One running rackend binary with one data directory. One instance = one app. |
| **Collection** | A user-defined table, managed at runtime. Types: `base`, `auth`. |
| **Record** | A row in a collection. |
| **Field** | A typed column of a collection. |
| **Rule** | A filter expression that controls access to a collection operation (§9). |
| **Superuser** | Instance administrator. Stored separately from app users. |
| **User** | A record in an `auth` collection. |
| **Identity** | A login method linked to a user (password, passkey, external provider). |
| **Session** | A server-side login state; all tokens map to exactly one session. |
| **Provider** | An external OIDC/OAuth2 identity provider rackend logs users in *with*. |
| **Client** | An application that logs users in *through* rackend's OIDC provider. |
| **Channel** | An ephemeral realtime topic for broadcasts and presence (§13.3). |
| **Port / Adapter** | Hexagonal architecture terms (§4). |

---

## 1. Product overview

### 1.1 Vision
rackend lets a developer run one binary (or one Docker container) and immediately get: a database with an admin UI, a REST API with fine-grained access rules, authentication (including being an OIDC provider), file storage with image thumbnails, realtime subscriptions/presence/collaborative editing, and first-class observability. **Zero configuration is required to get started**; everything else is configured at runtime in the admin UI.

### 1.2 Goals (v1)
- G1: Single statically linked binary; Docker image is the primary distribution.
- G2: SQLite as the only database. Embedded, no external services required.
- G3: Dynamic collections with PocketBase-style access rules.
- G4: Auth: email/password, passkeys, email OTP/magic flows (with SMTP), external OIDC/OAuth2 providers (identity broker), and rackend acting as an **OIDC provider**.
- G5: Files on local filesystem by default, any S3-compatible store optionally (AWS S3, MinIO, Garage, RustFS).
- G6: Image optimization: EXIF stripping, orientation normalization, lazy thumbnails.
- G7: Realtime: record subscriptions (SSE + WebSocket), channels with presence, collaborative editing via Yjs-compatible CRDTs.
- G8: Observability: audit trail, request logs, app + host metrics, dashboard, Prometheus/OTLP export.
- G9: Upgrades never lose data (§7).
- G10: Hexagonal architecture; HTTP is the only adapter in v1, but gRPC/MCP adapters must be addable without touching the core.

### 1.3 Non-goals (v1)
- Custom server-side code (hooks, JS runtime, WASM plugins). Deferred until the core is solid.
- Horizontal scaling / multiple instances on one database.
- Multi-tenancy (multiple apps per instance).
- libSQL/Turso features (replication, sync).
- GraphQL.
- Storage backend migration (fs ↔ s3) with automatic file copying (deferred; see §12.6).

### 1.4 Target users
Developers building CRUD-style web and mobile apps, internal tools and prototypes who want to self-host with minimal operations.

---

## 2. Quality attributes

| Attribute | Requirement |
|---|---|
| Data safety | No released version may ever lose or corrupt data during upgrade (§7). Upgrade paths tested in CI. |
| Setup | `docker run -v rackend:/data -p 8080:8080 rackend/rackend` → working instance with admin setup link in logs. No env vars required. |
| Performance (reference: 4 vCPU, NVMe) | ≥ 5,000 simple authenticated record reads/s; ≥ 1,000 writes/s; p99 read latency < 20 ms at 50% of that load. |
| Realtime | ≥ 10,000 concurrent SSE/WS connections per instance on reference hardware. |
| Footprint | Idle RSS < 50 MB. Docker image < 40 MB compressed. |
| Security | OWASP ASVS L2 as guideline; secure defaults that cannot be turned off accidentally (§17). |
| Portability | Linux x86_64 and aarch64 (Docker + static binaries). macOS builds for development. |

---

## 3. Technology & tooling

### 3.1 Toolchain
- Rust: **stable** channel, version pinned in `mise.toml`, bumped deliberately (one PR per bump).
- Edition: 2024.
- Admin UI: SvelteKit (Svelte 5, runes) with `adapter-static`, built with **bun** (never npm/yarn/pnpm).
- All tools and tasks are managed via **mise**. A fresh clone MUST be buildable with `mise install && mise run build`.

### 3.2 `mise.toml` (reference)
```toml
[tools]
rust = { version = "stable", components = "clippy,rustfmt" } # pin to a concrete version, e.g. "1.9x.0"
bun = "latest"                                                 # pin in practice
"cargo:cargo-nextest" = "latest"
"cargo:cargo-deny" = "latest"

[env]
RACKEND_DATA_DIR = "{{config_root}}/.data"
RACKEND_DEV = "true"

[tasks.dev]
description = "Run server in dev mode (UI served from bun dev server via proxy)"
run = "cargo run -p rackend -- serve"

[tasks."ui:dev"]
dir = "admin-ui"
run = "bun install && bun run dev"

[tasks."ui:build"]
dir = "admin-ui"
run = "bun install --frozen-lockfile && bun run build"

[tasks.build]
depends = ["ui:build"]
run = "cargo build --release -p rackend"

[tasks.test]
run = "cargo nextest run --workspace"

[tasks."test:upgrade"]
run = "cargo nextest run -p rackend-store-sqlite --test upgrade_paths"

[tasks."test:s3"]
description = "S3 contract tests against MinIO and Garage (requires Docker)"
run = "cargo nextest run -p rackend-blob-s3 --features contract-tests"

[tasks.lint]
run = [
  "cargo fmt --all --check",
  "cargo clippy --workspace --all-targets -- -D warnings",
  "cargo deny check",
]

[tasks.fmt]
run = "cargo fmt --all"

[tasks."docker:build"]
run = "docker build -t rackend:dev ."
```

### 3.3 Approved dependencies

| Purpose | Crate | Notes |
|---|---|---|
| Async runtime | `tokio` | multi-thread runtime |
| HTTP | `axum`, `tower`, `tower-http` | compression, CORS, tracing, limits |
| SQLite | `rusqlite` (feature `bundled`) | pins SQLite version into the binary |
| Serialization | `serde`, `serde_json` | |
| Errors | `thiserror` (libs), `anyhow` (only in `rackend` binary crate) | |
| Logging/tracing | `tracing`, `tracing-subscriber` | JSON + pretty formatters |
| OTLP export | `opentelemetry`, `opentelemetry-otlp`, `tracing-opentelemetry` | behind cargo feature `otlp` (default on) |
| Metrics | `metrics`, `metrics-exporter-prometheus` | |
| Host metrics | `sysinfo` | plus manual cgroup v2 reads |
| IDs | `uuid` (feature `v7`) | |
| Time | `time` | RFC 3339 formatting |
| Password hashing | `argon2` | argon2id |
| Passkeys | `webauthn-rs` | |
| OIDC client (broker) | `openidconnect` | |
| JWT (provider) | `jsonwebtoken` | RS256 + ES256 |
| Key generation | `rsa`, `p256` | |
| Crypto primitives | `sha2`, `hmac`, `rand`, `chacha20poly1305`, `subtle`, `base64` | secret encryption, constant-time compare |
| SMTP | `lettre` | |
| Server-side HTML | `askama` | hosted login/consent pages |
| Embedded assets | `rust-embed` | admin UI bundle |
| Object storage | `object_store` (feature `aws`) | S3-compatible incl. custom endpoints |
| Images | `image`, `fast_image_resize`, `webp`, `kamadak-exif` | |
| CRDT | `yrs` | Yjs-compatible |
| Rate limiting | `governor` | |
| File locking | `fs4` | exclusive data dir lock |
| CLI | `clap` (derive) | |
| Allocator | `mimalloc` | musl's allocator is slow |
| Archives | `tar`, `zstd` | backups |
| Testing | `proptest`, `insta`, `tempfile`, `testcontainers`, `reqwest`, `tokio-tungstenite` | dev-deps |

The rule-language parser MUST be hand-written (no parser-combinator dependency), for good error messages and zero deps in `core`.

### 3.4 Code conventions
- Workspace lints: `unsafe_code = "forbid"`, clippy `-D warnings`. `unwrap()`/`expect()` forbidden outside tests and outside provably infallible cases (comment required).
- Every crate has a `README.md` with one paragraph stating its responsibility.
- Public items in `rackend-core` MUST have doc comments.
- Errors: each crate has its own `Error` enum (`thiserror`). The HTTP adapter maps all errors to RFC 9457 problem responses (§10.2), with no exceptions. No `anyhow` in libraries.
- Logging: structured `tracing` fields, never `format!` secrets. Fields named `password`, `token`, `secret`, `authorization`, `cookie` MUST be redacted by the log layer.
- Commits: Conventional Commits (`feat:`, `fix:`, `refactor:`, …).
- License: TBD (§21).

---

## 4. Architecture

### 4.1 Hexagonal layout

```
rackend/
├── Cargo.toml                 # workspace
├── mise.toml
├── Dockerfile
├── SPEC.md
├── docs/adr/
├── crates/
│   ├── core/                  # rackend-core: domain + application services + ports
│   │   ├── src/domain/        # collections, fields, records, values, validation
│   │   ├── src/rules/         # lexer, parser, AST, type-checker (NO SQL here)
│   │   ├── src/auth/          # identities, sessions, password, OTP, OIDC provider logic
│   │   ├── src/files/         # file metadata, naming, thumbnail spec parsing
│   │   ├── src/realtime/      # hub, subscriptions, channels, presence, collab sessions
│   │   ├── src/audit/
│   │   ├── src/settings/
│   │   ├── src/usecases/      # one module per use case (CreateRecord, AuthWithPassword, …)
│   │   └── src/ports/         # traits implemented by adapters
│   ├── store-sqlite/          # rackend-store-sqlite: Store port, migrations, rule→SQL compiler
│   ├── blob-fs/               # rackend-blob-fs: BlobStore on local filesystem
│   ├── blob-s3/               # rackend-blob-s3: BlobStore on S3-compatible storage
│   ├── imaging/               # rackend-imaging: ImageProcessor port impl
│   ├── mail-smtp/             # rackend-mail-smtp: Mailer port impl
│   ├── idp-client/            # rackend-idp-client: external OIDC/OAuth2 provider client port impl
│   ├── host-metrics/          # rackend-host-metrics: HostMetrics port impl (sysinfo + cgroups)
│   ├── http/                  # rackend-http: axum adapter (REST, SSE, WS, OIDC endpoints, login pages)
│   └── rackend/               # binary: CLI, bootstrap, composition root, embedded UI
├── admin-ui/                  # SvelteKit app → static bundle embedded by `crates/rackend`
└── tests/
    ├── e2e/                   # black-box tests against a spawned binary
    └── upgrade/fixtures/      # DB fixtures from every released version (§7.7)
```

### 4.2 Dependency rules (MUST, enforced by CI via `cargo deny` bans or a custom check)
- `core` depends on **no other workspace crate** and on no I/O crates (no `rusqlite`, `axum`, `object_store`, `reqwest`, `lettre`, `image`). Allowed: `serde`, `thiserror`, `tracing`, `uuid`, `time`, `argon2`, `sha2`/`hmac`/`rand`/`subtle`/`base64`, `jsonwebtoken`, `rsa`/`p256`, `yrs`, `webauthn-rs` (core types only), and `tokio` **sync primitives only** (`tokio::sync`, no I/O, no `spawn`). Task spawning goes through a `Spawner` port.
- Adapters depend on `core`, never on each other.
- `crates/rackend` (composition root) is the only crate that depends on all adapters.
- A future `grpc` or `mcp` crate MUST be able to implement all features by calling `core` use cases only.

### 4.3 Ports (traits in `core::ports`)

| Port | Responsibility | v1 adapters |
|---|---|---|
| `Store` | Transactions and repositories over the database | `store-sqlite` |
| `BlobStore` | Put/get/delete/list/head of file blobs, presigned GET (optional) | `blob-fs`, `blob-s3` |
| `ImageProcessor` | Probe, normalize (strip EXIF, rotate), thumbnail | `imaging` |
| `Mailer` | Send email | `mail-smtp`, `NoopMailer` (when SMTP unset) |
| `IdpClient` | OIDC discovery, auth URL, code exchange, ID-token validation, userinfo; OAuth2 presets | `idp-client` |
| `HostMetrics` | CPU, memory, disk, cgroup limits | `host-metrics` |
| `Clock` | Current time (mockable) | `SystemClock` in core |
| `IdGenerator` | UUIDv7 generation (mockable) | default in core |
| `SecretBox` | Encrypt/decrypt secrets at rest (§17.4) | in core, key injected |
| `Spawner` | Spawn background tasks | tokio impl in `rackend` |

### 4.4 Transactions: Unit of Work
- SQLite has a single writer. All writes go through a **dedicated writer thread** owning one connection. Reads use a pool of read-only connections.
- `Store` exposes:
  ```rust
  async fn read<R: Send + 'static>(&self, f: impl FnOnce(&mut dyn ReadTx) -> Result<R> + Send + 'static) -> Result<R>;
  async fn write<R: Send + 'static>(&self, f: impl FnOnce(&mut dyn WriteTx) -> Result<R> + Send + 'static) -> Result<R>;
  ```
  The closure runs synchronously inside one SQLite transaction (`BEGIN IMMEDIATE` for writes). Returning `Err` rolls back.
- `ReadTx` / `WriteTx` expose repository methods (records, collections, sessions, audit, …). Use cases MUST do the domain change **and** its audit entry in the same `write` closure (§14.1).
- The writer queue is bounded (default 1024 jobs). When full, the use case fails with `Unavailable` → HTTP 503 with `Retry-After: 1`.
- Side effects that must not happen on rollback (realtime events, emails, blob deletes) are collected in an **outbox** returned from the closure and executed only after commit.

### 4.5 Request flow (HTTP example)
```
axum handler
  → extract auth (session lookup via Store/read + cache)
  → build use-case input (DTO → domain)
  → core use case
      → Store.read/write(closure: authorize via compiled rule, mutate, audit)
      → after commit: publish realtime events, enqueue emails, delete orphaned blobs
  → map result/error → HTTP response
```

---

## 5. Runtime, bootstrap & CLI

### 5.1 CLI
```
rackend serve                      # default command
rackend healthcheck                # exits 0 if GET /api/health returns 200 (for Docker HEALTHCHECK)
rackend version
rackend superuser create <email>   # prompts for password or reads RACKEND_SUPERUSER_PASSWORD
rackend superuser reset-password <email>
rackend backup create
rackend backup list
rackend backup restore <file>      # instance MUST be stopped (lock check)
rackend check                      # integrity_check + foreign_key_check + blob consistency report
rackend migrate status             # shows applied/pending migrations, never applies
```

### 5.2 Bootstrap environment variables (the only env config)
| Variable | Default | Meaning |
|---|---|---|
| `RACKEND_DATA_DIR` | `/data` (Docker) / `./rackend_data` (bare) | Data directory |
| `RACKEND_HTTP_ADDR` | `0.0.0.0:8080` | Listen address |
| `RACKEND_PUBLIC_URL` | unset | Overrides the stored public URL (§5.5) |
| `RACKEND_ENCRYPTION_KEY` | unset | 32-byte base64 key for secrets at rest (§17.4) |
| `RACKEND_DEV` | `false` | Dev mode: pretty logs, permissive localhost redirects, CORS `*` |
| `RACKEND_LOG_LEVEL` | `info` | `tracing` env-filter syntax |
| `RACKEND_LOG_FORMAT` | `json` (`pretty` in dev) | |
| `RACKEND_TRUSTED_PROXIES` | unset | CIDR list whose `X-Forwarded-*` headers are trusted |
| `RACKEND_OTLP_ENDPOINT` | unset | Enables OTLP trace/metric export |
| `RACKEND_SUPERUSER_EMAIL` / `RACKEND_SUPERUSER_PASSWORD` | unset | Headless first-boot superuser creation (automation) |
| `RACKEND_SQLITE_SYNCHRONOUS` | `FULL` | `FULL` or `NORMAL` (§6.2) |

Everything else is runtime configuration in the database (§15).

### 5.3 Data directory layout
```
/data/
├── rackend.lock          # exclusive lock (fs4) held for process lifetime
├── data.db               # main database (+ -wal, -shm)
├── logs.db               # request logs + metric time series (+ -wal, -shm)
├── storage/              # blob-fs root (absent when S3 is used)
│   └── _thumbs/          # thumbnail cache
├── backups/              # pre-upgrade + manual + scheduled backups
└── tmp/                  # upload staging; wiped at startup
```
All paths are relative to `RACKEND_DATA_DIR`. rackend MUST NOT write outside it.

### 5.4 Startup sequence (MUST, in order)
1. Parse CLI/env. Init logging.
2. Create the data dir if missing. Check it is writable. Wipe `tmp/`.
3. Acquire an **exclusive lock** on `rackend.lock`. On failure: log `another rackend instance is using <dir>` and exit with code 75.
4. Warn (do not fail) if the data dir is on a network filesystem (detect via `statfs` magic numbers on Linux: NFS, CIFS/SMB, FUSE-sshfs). The message links to docs.
5. Open `data.db`, apply PRAGMAs (§6.2).
6. Run the **migration procedure** (§7.3). On any failure: exit non-zero. Data untouched.
7. Load settings and the collection schema cache. Compile all rules. A rule that fails to compile at startup (should be impossible) disables that operation (treated as superuser-only) and logs an error. It does not crash.
8. First boot only (§5.5).
9. Start background jobs (retention, metrics sampler, backups, key rotation, session GC, thumbnail cache GC).
10. Bind HTTP. Health reports `ok`.

During steps 5–9, if HTTP binding is needed for health in long migrations, a minimal server MAY be started that answers `/api/health` with `503 {"status":"migrating"}` and everything else with 503.

### 5.5 First boot (zero-config)
When `_meta.instance_id` is absent:
1. Generate `instance_id` (UUIDv7) and the initial signing keys (RS256 2048-bit + ES256) (§11.8).
2. Create the default auth collection `users` (§8.6) with secure default rules.
3. Create the default OIDC client `default` (§11.8.3).
4. If `RACKEND_SUPERUSER_EMAIL`/`PASSWORD` are set: create that superuser. Otherwise create a one-time **setup token** (32 random bytes, stored hashed, valid 24 h, regenerated on every restart until used) and log:
   `Open http://<listen-addr>/_/setup?token=<token> to create the first superuser.`
5. Public URL: unset. On the first superuser setup request, the UI proposes the URL derived from the request (`Host`, plus `X-Forwarded-Proto/Host` only from trusted proxies). The superuser confirms or edits it. It is stored in settings `app.public_url`.
6. Until `app.public_url` is set, features that need it (OIDC provider, external providers, passkeys, email links) are disabled with a visible admin-UI notice. Password auth works.

### 5.6 Shutdown
On SIGTERM/SIGINT: stop accepting connections → close realtime connections with a "server restarting" close frame/event → wait up to 10 s for in-flight requests → drain the writer queue → `PRAGMA wal_checkpoint(TRUNCATE)` on both DBs → release the lock → exit 0.

---

## 6. Database layer

### 6.1 Files
- `data.db`: everything that must be consistent (collections, records, auth, settings, audit).
- `logs.db`: request logs and metric points. Loss is acceptable; it is never in the same transaction as `data.db`.

### 6.2 PRAGMAs (every connection)
```
journal_mode = WAL
synchronous = FULL          -- RACKEND_SQLITE_SYNCHRONOUS may set NORMAL
foreign_keys = ON
busy_timeout = 5000
temp_store = MEMORY
cache_size = -32000         -- 32 MB per connection
```
Plus `PRAGMA optimize` on shutdown and every 6 h.
`synchronous=FULL` is the default because data safety outranks write throughput. `NORMAL` in WAL mode can lose the last commits on power loss (never corruption) and is offered as an opt-in.

### 6.3 Connections
- 1 writer connection on a dedicated OS thread (job channel).
- Reader pool: `max(4, num_cpus)` connections opened with `SQLITE_OPEN_READ_ONLY`, used via `spawn_blocking`.
- Same structure for `logs.db`, with 1 writer + 2 readers. Log writes are batched (flush every 1 s or 500 rows).

### 6.4 Conventions
- IDs: UUIDv7, stored as `TEXT` (36 chars, lowercase, hyphenated). Sortable by creation time.
- Timestamps: `TEXT`, RFC 3339 UTC with milliseconds, `Z` suffix (`2026-11-17T14:36:00.123Z`), lexicographically sortable.
- Booleans: `INTEGER` 0/1.
- JSON: `TEXT` validated with `json_valid()` CHECK constraints.
- System tables start with `_`. User collections MUST NOT.
- All user-facing SQL is generated with bound parameters. Identifiers are **never** taken from input without validation against the schema cache and quoting with `"..."`.

### 6.5 System tables (`data.db`)
Schemas are indicative. Exact DDL lives in migrations.

```sql
_meta(key TEXT PRIMARY KEY, value TEXT NOT NULL)
  -- instance_id, schema_version, created_at, last_binary_version

_migrations(id INTEGER PRIMARY KEY, name TEXT NOT NULL, checksum TEXT NOT NULL,
            applied_at TEXT NOT NULL, binary_version TEXT NOT NULL)

_settings(key TEXT PRIMARY KEY, value TEXT NOT NULL, is_secret INTEGER NOT NULL, updated TEXT NOT NULL)

_collections(id TEXT PRIMARY KEY, name TEXT UNIQUE NOT NULL, type TEXT NOT NULL,
             fields TEXT NOT NULL, indexes TEXT NOT NULL, rules TEXT NOT NULL,
             options TEXT NOT NULL, created TEXT NOT NULL, updated TEXT NOT NULL)

_schema_history(id TEXT PRIMARY KEY, collection_id TEXT, action TEXT NOT NULL,
                before TEXT, after TEXT, ddl TEXT NOT NULL, actor TEXT NOT NULL, ts TEXT NOT NULL)

_trash(id TEXT PRIMARY KEY, kind TEXT NOT NULL,          -- 'field' | 'collection'
       collection_id TEXT NOT NULL, original_name TEXT NOT NULL, storage_name TEXT NOT NULL,
       definition TEXT NOT NULL, deleted_at TEXT NOT NULL, purge_after TEXT NOT NULL, actor TEXT NOT NULL)

_superusers(id, email UNIQUE, password_hash, created, updated)

_identities(id, collection_id, user_id, provider_id, issuer, subject, email_at_link,
            created, last_login, UNIQUE(issuer, subject))

_passkeys(id, owner_type, collection_id, user_id, credential_id UNIQUE, credential TEXT,
          name, created, last_used)

_sessions(id, family_id, subject_type,           -- 'user' | 'superuser'
          collection_id, user_id, client_id,      -- client_id NULL for direct API
          auth_method, access_hash, access_expires,
          refresh_hash, prev_refresh_hash, refresh_expires,
          created, last_seen, revoked_at, revoked_reason, ip, user_agent)

_verification_tokens(hash PRIMARY KEY, purpose, collection_id, user_id, payload, expires, used_at)
  -- purpose: verify_email | reset_password | change_email | magic_link

_otps(id, collection_id, user_id, code_hash, purpose, expires, attempts, used_at)

_auth_providers(id, name UNIQUE, kind,            -- 'oidc' | 'oauth2_preset'
                preset, issuer, client_id, client_secret_enc, scopes, collection_id,
                auto_link_verified_email INTEGER, claim_mapping TEXT, enabled INTEGER, created, updated)

_oauth_clients(id, name, kind,                    -- 'public' | 'confidential'
               secret_hash, redirect_uris TEXT, post_logout_redirect_uris TEXT,
               scopes TEXT, collection_id, first_party INTEGER,
               access_ttl_s, refresh_ttl_s, enabled, created, updated)

_oauth_codes(hash PRIMARY KEY, client_id, session_id, redirect_uri, scopes,
             code_challenge, nonce, auth_time, expires, used_at)

_oauth_consents(user_id, collection_id, client_id, scopes, granted_at,
                PRIMARY KEY(collection_id, user_id, client_id))

_keys(kid PRIMARY KEY, alg, private_enc, public_jwk, created, activated_at, retired_at, delete_after)

_files(id, collection_id, record_id, field, storage_key UNIQUE, original_name, size, mime,
       width, height, sha256, created, deleted_at)

_channels(id, name UNIQUE, pattern, join_rule, publish_rule, presence INTEGER, options, created, updated)

_ydoc_updates(id INTEGER PRIMARY KEY, collection_id, record_id, field, update BLOB, created)

_audit(id, ts, actor_type, actor_id, actor_label, action, collection_id, record_id,
       diff TEXT, ip, user_agent, request_id, meta TEXT)

_setup_tokens(hash PRIMARY KEY, expires, used_at)
```
Appropriate indexes (e.g. `_audit(ts)`, `_audit(collection_id, record_id, ts)`, `_sessions(user_id)`, `_files(record_id)`) MUST be created.

### 6.6 `logs.db` tables
```sql
_requests(id INTEGER PRIMARY KEY, ts, method, route, path, status, duration_ms,
          ip, user_agent, actor_type, actor_id, request_id, bytes_in, bytes_out, error)
_metric_points(ts TEXT, series TEXT, value REAL, PRIMARY KEY(series, ts)) WITHOUT ROWID
```

---

## 7. Migrations & upgrade safety (CRITICAL)

> **Invariant:** upgrading rackend to any newer released version MUST NOT lose or corrupt data. When in doubt, refuse to start rather than risk data.

### 7.1 Kinds of schema change
1. **System migrations**: changes to `_`-tables, shipped inside the binary, applied at startup.
2. **Collection changes**: user-initiated via admin UI/API, applied at runtime (§8.5).

### 7.2 System migration rules (MUST)
- Migrations are numbered, append-only and forward-only. A released migration MUST NEVER be edited. CI compares checksums against the previous release tag.
- Each migration is SQL plus an optional Rust function, executed inside one transaction.
- Table restructuring uses SQLite's documented 12-step "generalized ALTER TABLE" procedure (create new → copy → drop old → rename). Where that requires `PRAGMA foreign_keys=OFF`, the migration runner disables it **outside** the transaction, runs the migration, runs `PRAGMA foreign_key_check` inside the transaction before commit (any violation → rollback), and then re-enables it.
- Migrations MUST NOT touch blob storage. A storage layout change (if ever needed) is copy → verify → switch, with cleanup only in a later release.
- Destructive changes to system data (dropping columns or tables with data) are done in two releases: release N stops using the data; release N+1 (or later) removes it.

### 7.3 Migration procedure at startup
1. Read `_meta.schema_version` (0 if new DB).
2. **Downgrade protection:** if `schema_version` > the highest migration known to this binary → exit with code 78 and the message: `database schema vX is newer than this binary supports (vY). Downgrading is not supported; restore a pre-upgrade backup from /data/backups to go back.`
3. Verify checksums of all applied migrations against the embedded ones. Mismatch → exit code 78.
4. If no pending migrations → continue.
5. **Pre-upgrade backup:**
   a. Check free space on the data dir ≥ 2 × size(`data.db`) + 100 MB. Otherwise exit with code 73 and a clear message.
   b. `VACUUM INTO 'backups/pre-upgrade-<fromVersion>-to-<toVersion>-<ts>.db'`.
   c. Open the backup read-only and run `PRAGMA integrity_check`. Failure → exit code 74, original untouched.
6. Apply pending migrations in order, each in its own transaction, and record each in `_migrations`. On any error → roll back the current migration, log the full error, exit code 70. Earlier migrations of this run stay applied. Each one is individually safe and recorded, so the next start resumes.
7. After all migrations: `PRAGMA integrity_check` and `PRAGMA foreign_key_check` on `data.db`. Failure → exit code 70 with instructions to restore the pre-upgrade backup.
8. Update `_meta.schema_version` and `_meta.last_binary_version`.
9. Keep the last **5** pre-upgrade backups and delete older ones (manual and scheduled backups have their own retention, §15).

`logs.db` has its own migration track. If a `logs.db` migration fails, the file MAY be moved aside to `logs.db.broken-<ts>` and recreated. Logs are non-critical. `data.db` is never treated this way.

### 7.4 Exit codes
| Code | Meaning |
|---|---|
| 0 | Normal |
| 70 | Migration or integrity failure |
| 73 | Insufficient disk space for safe upgrade |
| 74 | Backup verification failed |
| 75 | Data directory locked by another instance |
| 78 | Schema newer than binary / checksum mismatch |

### 7.5 Versioning policy
- SemVer. During `0.x`, a minor bump MAY break the HTTP API, but MUST NOT break data upgradability.
- Every release can upgrade from **every** earlier release directly (no mandatory stepping stones). If that ever becomes impossible, the binary MUST detect the gap and refuse with a message naming the intermediate version.

### 7.6 Docker specifics
- The container runs as a fixed UID/GID **10001:10001**, forever. Changing it is a breaking change that requires a major version and a migration note.
- One volume at `/data`.
- Docs MUST warn: do not put `/data` on NFS/SMB/network filesystems; do not run two containers on the same volume (the lock enforces this).

### 7.7 Upgrade-path testing (MUST in CI)
- `tests/upgrade/fixtures/<version>.db` (+ a matching `storage/` dir) is committed for **every released version**, generated by `tests/upgrade/generate.rs` against that release. Each fixture contains realistic data: several collections with all field types, relations, files, users with password + passkey + external identities, sessions, OIDC clients, audit entries, trashed fields.
- The CI test, for each fixture: copy → run the migration procedure with the HEAD binary → assert:
  - `integrity_check` = ok, `foreign_key_check` empty;
  - row counts per table equal the expected counts (stored next to the fixture as JSON);
  - a stable hash of every user record (canonical JSON) is unchanged, except where a migration documents an intended transformation;
  - existing passwords still verify, sessions still resolve, files still resolve to blobs;
  - the instance starts and serves `GET /api/collections/<c>/records` for each fixture collection.
- The release checklist includes generating the new fixture.

---

## 8. Collections & fields

### 8.1 Collection types
- `base`: plain data.
- `auth`: data + authentication (system auth fields, §8.6). Multiple auth collections are allowed (e.g. `users`, `staff`).
- (`view` collections, backed by SQL views, are deferred.)

### 8.2 Naming
- Collection name: `^[a-z][a-z0-9_]{0,62}$`, not starting with `_`, not a reserved word (`collections`, `settings`, `files`, `realtime`, `auth`, `batch`, `health`, `superusers`, SQLite keywords).
- Field name: same pattern. Reserved: `id`, `created`, `updated`, `expand`, `collection_id`, `collection_name`, plus auth system fields in auth collections.
- Each field has an immutable `id` (UUIDv7). Renames keep the id.

### 8.3 System fields (every collection)
| Field | SQL | Notes |
|---|---|---|
| `id` | `TEXT PRIMARY KEY` | UUIDv7, server-generated. A client MAY supply one on create if it is a valid UUID (useful for offline/optimistic UIs). |
| `created` | `TEXT NOT NULL` | set on insert |
| `updated` | `TEXT NOT NULL` | set on insert/update |

### 8.4 Field types

| Type | SQLite storage | Options | API JSON |
|---|---|---|---|
| `text` | TEXT | `min`, `max` (chars, default max 10,000), `pattern` (regex), `required` | string |
| `email` | TEXT | `required`, `only_domains`, `except_domains` | string |
| `url` | TEXT | `required`, `schemes` (default `["https","http"]`) | string |
| `number` | REAL | `min`, `max`, `integer_only`, `required` | number |
| `bool` | INTEGER | — | bool |
| `date` | TEXT | `min`, `max`, `required` | RFC 3339 string |
| `autodate` | TEXT | `on_create`, `on_update` | RFC 3339 string, read-only |
| `select` | TEXT (single) / TEXT JSON array (multi) | `values` (list), `max_select`, `required` | string / string[] |
| `json` | TEXT (`json_valid`) | `max_size` (bytes, default 1 MB) | any JSON |
| `relation` | TEXT (single id) / TEXT JSON array (multi) | `collection_id`, `max_select`, `min_select`, `on_delete` (`set_null`\|`cascade`\|`restrict`, default `set_null`), `required` | id / id[] (+ `expand`) |
| `file` | TEXT (single) / TEXT JSON array (multi) of stored filenames | `max_select`, `max_size` (default 10 MB), `mime_types`, `thumbs` (list of thumb specs), `protected` (bool) | filename / filename[] |
| `ydoc` | BLOB (compacted state) | — | omitted by default; see §13.4 |

Rules:
- `required` semantics: non-empty (non-null, non-empty string/array; for `number`, not null; `false` is a valid bool).
- Unknown fields in request bodies are rejected with a validation error (no silent ignore).
- Multi-valued fields are stored as JSON arrays. Queries use `json_each`.
- Relation integrity is enforced in the application layer (single and multi relations alike). Every write validates that the referenced ids exist. `on_delete` is applied in the same transaction as the delete. Cascades run as the system actor, are audited with `meta.cause = <original audit id>`, and are limited to depth 10 (deeper → `restrict` error).
- Collections MAY declare indexes: `{ "name", "fields": [...], "unique": bool }`. Created as `CREATE [UNIQUE] INDEX "idx_<collection>_<name>"`.

### 8.5 Collection schema changes (runtime)
Process for `POST/PATCH/DELETE /api/collections/...` (superuser only):
1. Validate the new definition (names, options, relation targets exist, no cycles of `cascade` deeper than 10).
2. Compile all five rules **against the new schema** (§9). Any error → 400 with position info.
3. Compute a diff and a DDL plan:
   - add field → `ALTER TABLE ADD COLUMN` (with default NULL; `required` is enforced on future writes only, and the UI warns about existing empty rows);
   - rename field → `ALTER TABLE RENAME COLUMN`;
   - change field type → allowed only between compatible types (`text↔email↔url`, `select single↔multi`, `relation single↔multi`, `file single↔multi`, `number` option changes). Single→multi wraps values in arrays; multi→single is allowed only if no row has > 1 value (checked first). Anything else → 400 "create a new field instead";
   - remove field → **soft delete**: rename the column to `_trash_<fieldid>`, add a `_trash` row with `purge_after = now + 30 days` (setting `schema.trash_retention_days`). The field disappears from the API and rules. It can be restored in the admin UI until purge;
   - delete collection → soft delete: rename the table to `_trash_<collectionid>` (+ `_trash` row). Delete is rejected if other collections have relations to it (the user must remove those first);
   - index changes → drop/create.
4. Execute everything in **one** write transaction: DDL + `_collections` update + `_schema_history` row (before, after, DDL text) + audit entry.
5. After commit: refresh the schema cache, recompile rules, notify realtime subscribers of the affected collection with a `schema` event, and invalidate relevant caches.
6. The purge job permanently drops trashed columns/tables after `purge_after`. Each purge is a separate audited transaction. Files of purged file fields are deleted from blob storage afterwards (§12.5).
7. `POST /api/collections/import` (superuser) accepts a full schema JSON and applies it as a single plan. A `dry_run=true` parameter returns the plan without applying.

### 8.6 Auth collections
System fields (in addition to §8.3):
| Field | SQL | API |
|---|---|---|
| `email` | TEXT, unique index (case-insensitive: stored lowercase) | visible to the owner and superusers; visible to others only if the collection option `email_visibility` is true |
| `email_verified` | INTEGER | read-only via the API |
| `password_hash` | TEXT NULL | never exposed |
| `name` | TEXT NULL | normal field, used for OIDC `name` claim by default |
| `avatar` | file NULL | normal field, OIDC `picture` |

Default `users` collection rules (first boot):
- list: `id = @request.auth.id`
- view: `id = @request.auth.id`
- create: `""` (public sign-up), with collection option `signup_enabled = true`
- update: `id = @request.auth.id`
- delete: `id = @request.auth.id`

Auth collection options:
```json
{
  "signup_enabled": true,
  "email_visibility": false,
  "methods": { "password": true, "passkey": true, "otp": true, "magic_link": true },
  "password": { "min_length": 10, "require_verified_email_to_login": false },
  "session": { "access_ttl_s": 900, "refresh_ttl_s": 2592000, "idle_timeout_s": 1209600 },
  "oidc_claims": { "name": "name", "picture": "avatar" }
}
```
Methods that need SMTP (`otp`, `magic_link`, verification, reset) are inactive while SMTP is unset, even when enabled.

---

## 9. Rule & filter language

The same language is used for (a) collection access rules, (b) the `filter` query parameter, (c) channel rules, and (d) realtime subscription filters.

### 9.1 Rule values per operation
Each collection has rules for: `list`, `view`, `create`, `update`, `delete`.
- `null` → **locked**: superusers only (the default for new collections).
- `""` (empty string) → **public**: anyone, including unauthenticated requests.
- expression → allowed iff the expression is true for the record and request.

Superusers bypass all rules.

### 9.2 Grammar (EBNF)
```
expr        = or_expr ;
or_expr     = and_expr { "||" and_expr } ;
and_expr    = unary { "&&" unary } ;
unary       = [ "!" ] primary ;
primary     = "(" expr ")" | comparison ;
comparison  = operand op operand ;
op          = "=" | "!=" | ">" | ">=" | "<" | "<=" | "~" | "!~"
            | "?=" | "?!=" | "?>" | "?>=" | "?<" | "?<=" | "?~" | "?!~" ;
operand     = literal | path | macro | function ;
literal     = string | number | "true" | "false" | "null" ;
string      = "'" { char } "'" | '"' { char } '"' ;      (* backslash escapes *)
number      = [ "-" ] digit { digit } [ "." digit { digit } ] ;
path        = ident { "." ident } [ modifier ] ;
macro       = "@request.auth." path
            | "@request.body." path
            | "@request.query." ident
            | "@request.headers." ident                 (* lowercase, '-' → '_' *)
            | "@request.method"
            | "@request.context"                         (* 'default'|'realtime'|'oidc' *)
            | "@collection." ident "." path              (* cross-collection join *)
            | "@channel.key"                             (* channel rules only *)
            | "@now" | "@today_start" | "@today_end" ;
function    = ident "(" [ operand { "," operand } ] ")" ;
modifier    = ":isset" | ":length" | ":lower" | ":each" ;
ident       = letter { letter | digit | "_" } ;
```
Whitespace is insignificant. `//` line comments are allowed in stored rules.

Functions (v1): `lower(x)`, `upper(x)`, `len(x)`, `date_add(date, 'P1D')` (ISO 8601 duration), `coalesce(a, b)`.

### 9.3 Semantics
- **Paths** resolve against the current record. `a.b.c` traverses relation fields (max **3** hops). Back-relations: `<collection>_via_<field>` (e.g. `comments_via_post.author`).
- **Multi-valued operands** (multi relation/select/file, `:each`, back-relations): plain operators mean **all** values must satisfy the comparison; `?`-prefixed operators mean **at least one**. Empty sets: `?op` → false, plain op → true (vacuous). This MUST be documented prominently, with examples.
- `~` is a case-insensitive "contains" (SQL `LIKE '%v%'` with escaping). If the value contains `%`, the user's wildcards are used as-is.
- `null` comparisons: `x = null` ↔ `x IS NULL OR x = ''` (empty string and NULL are both "empty").
- `@request.auth.*` with an unauthenticated request: every path resolves to null, so `@request.auth.id != ""` is the idiom for "logged in".
- `@request.auth.collection_name` lets rules distinguish auth collections.
- `@request.body.<field>`: the submitted value (create/update only). `@request.body.<field>:isset` is true if the field was present in the body. Typical use: prevent changing ownership: `@request.body.owner:isset = false || @request.body.owner = @request.auth.id`.
- `:length` → array length of multi-valued fields. `:lower` → lowercased string. `:each` → treat each element of a multi-valued field as a separate value.
- `@collection.<name>.<path>` adds a correlated `EXISTS` join against another collection (e.g. memberships). Access to other collections inside a rule **ignores their rules** (rules are written by the superuser). In the client `filter` parameter, `@collection` is **not allowed** (prevents data probing).
- **Type checking** happens at compile time: comparing incompatible types (e.g. bool with date) is a compile error. Unknown fields are a compile error with a "did you mean" hint.

### 9.4 Evaluation per operation
| Operation | How the rule is applied |
|---|---|
| list | Compiled to a `WHERE` fragment, ANDed with the user `filter`. Non-matching records are omitted. |
| view | Record fetched with `WHERE id = ? AND (rule)`. No match → **404** (no existence leak). |
| create | Inside the write transaction: validate → insert → `SELECT 1 FROM t WHERE id=? AND (rule)` (`@request.body` bound) → no match → rollback, **403**. |
| update | The record must satisfy the **view** rule (else 404) and the **update** rule evaluated against the *current* stored row with `@request.body` bound (else 403). Then validate and write. |
| delete | View rule (else 404), then delete rule (else 403). |
| realtime | View rule evaluated per event per subscriber (§13.2). |

### 9.5 Compilation (in `store-sqlite`)
- `core::rules` produces a typed AST (`TypedExpr`) referencing field ids, not names.
- `store-sqlite::rules_sql` compiles a `TypedExpr` to `(sql_fragment: String, params: Vec<Value>)` where every literal and macro value is a bound parameter.
- Relation traversals compile to correlated `EXISTS (SELECT 1 FROM ... )` subqueries (not JOINs) to avoid row multiplication. Multi-valued fields use `json_each`.
- Compiled forms are cached per (collection schema version, rule) and invalidated on schema change.
- Limits: expression length ≤ 4,096 chars; AST depth ≤ 32; relation hops ≤ 3; total joins/subqueries ≤ 16; `filter` param ≤ 2,048 chars. Exceeding → 400.
- Every rule-produced query MUST be run with an SQLite progress handler that aborts after 2 s (configurable) → 503 `query_timeout`.

### 9.6 Testing requirements
- The parser is fuzzed (`cargo-fuzz`) and property-tested: `parse(print(ast)) == ast`.
- Compiler snapshot tests (`insta`) for each operator × field type × cardinality.
- A semantic test suite runs the same expressions against a reference evaluator (in-memory, in `core`) and SQLite, and asserts identical results on randomized data (proptest). The in-memory evaluator is also used for realtime create-event checks where useful.

---

## 10. REST API

### 10.1 Conventions
- Base path `/api`. JSON in/out (`application/json`); `multipart/form-data` for requests with files (field `@json` holds the JSON part, file parts are named by field name; multiple files repeat the name).
- Authentication: `Authorization: Bearer <access_token>` or session cookie (§11.6).
- Request IDs: accept `X-Request-Id` (if it is a valid UUID) or generate one. Echo it in the response.
- Max JSON body: 1 MB (configurable). Max multipart: sum of per-field file limits + 1 MB.
- All list endpoints are paginated.
- Time values are RFC 3339 UTC.

### 10.2 Errors (RFC 9457 problem details)

**Every error response produced by rackend MUST be an RFC 9457 problem details object.** No endpoint, middleware, extractor or fallback may return a plain-text, HTML or ad-hoc JSON error body. The only deviations are the protocol-mandated cases in §10.2.5, and even those carry the RFC 9457 members.

#### 10.2.1 Shape
```json
{
  "type": "https://github.com/axanet/rackend/blob/main/docs/errors.md#validation_failed",
  "title": "Validation failed",
  "status": 400,
  "detail": "One or more fields are invalid.",
  "instance": "urn:uuid:01936b2e-7c4a-7d1e-9f3a-2b8c1d4e5f60",
  "code": "validation_failed",
  "request_id": "01936b2e-7c4a-7d1e-9f3a-2b8c1d4e5f60",
  "errors": {
    "title": [{ "code": "too_long", "message": "Must be at most 200 characters.", "params": { "max": 200 } }]
  }
}
```
- Standard members (RFC 9457 §3.1):
  - `type`: absolute URI identifying the problem type: `https://github.com/axanet/rackend/blob/main/docs/errors.md#<code>`. Each type MUST be documented in `docs/errors.md` (title, status, when it occurs, extension members). Once released, a type URI is permanent.
  - `title`: short, human-readable summary. Constant per `type` (never varies per occurrence).
  - `status`: MUST equal the HTTP status code of the response.
  - `detail`: occurrence-specific explanation for the client. Never contains internal details (SQL, stack traces, file paths, secrets).
  - `instance`: `urn:uuid:<request_id>`, identifying this occurrence.
- Extension members (always present): `code` (stable machine-readable snake_case identifier, equal to the `type` fragment) and `request_id` (same value as the `X-Request-Id` response header).
- Extension members (when applicable): `errors` (field-level validation errors, keyed by field name; nested paths use dot notation such as `expand.author`, `@json.title`), `position` (for rule/filter compile errors: `{ "line", "column", "offset" }`), `retry_after_s` (429/503), `max_bytes` (413), `allowed` (405: allowed methods).
- Content type: `application/problem+json` (charset UTF-8).
- Clients MUST be able to rely on `code`. `title`/`detail` text MAY change between versions.

#### 10.2.2 Problem type registry (initial)
| `code` | Status | Meaning |
|---|---|---|
| `bad_request` | 400 | Malformed request (invalid JSON, bad query parameter, bad multipart) |
| `validation_failed` | 400 | Body validation failed (`errors` present) |
| `rule_compile_error` | 400 | Filter/rule expression invalid (`position` present) |
| `unauthorized` | 401 | Missing/invalid/expired credentials (with `WWW-Authenticate`, §10.2.4) |
| `forbidden` | 403 | Authenticated but not allowed (rule, CSRF/origin check, superuser required) |
| `not_found` | 404 | Unknown route, collection, record, or record not visible (no existence leak) |
| `method_not_allowed` | 405 | Wrong method (`allowed` present, `Allow` header set) |
| `not_acceptable` | 406 | Unsupported `Accept` |
| `conflict` | 409 | Unique constraint, concurrent schema change, relation `restrict` |
| `precondition_failed` | 412 | `If-Match` mismatch |
| `payload_too_large` | 413 | Body or file exceeds limit (`max_bytes` present) |
| `unsupported_media_type` | 415 | Wrong `Content-Type` or file type not allowed for the field |
| `rate_limited` | 429 | Rate limit exceeded (`Retry-After` header + `retry_after_s`) |
| `smtp_not_configured` | 409 | Action needs SMTP |
| `public_url_not_configured` | 409 | Action needs `app.public_url` |
| `query_timeout` | 503 | Query exceeded the rule timeout |
| `unavailable` | 503 | Writer queue full, migrating, shutting down (`Retry-After`) |
| `internal` | 500 | Unexpected error. `detail` is generic ("An unexpected error occurred. Reference: <request_id>"); the full error is logged with the request id. |

New codes are added to this table and `docs/errors.md` in the same change that introduces them.

#### 10.2.3 Coverage (MUST)
- Domain/use-case errors: mapped centrally in `rackend-http` via one `impl IntoResponse for ApiError`. Handlers never build error bodies by hand.
- Framework-generated errors: axum extractor rejections (`Json`, `Query`, `Path`, `Multipart`), `tower-http` limit/timeout layers, unknown routes (404 fallback), wrong methods (405), CORS preflight failures, panics (caught by a `CatchPanic` layer → `internal`): all converted to problem details by a response-mapping layer.
- File endpoints, realtime HTTP endpoints (SSE handshake and subscription `POST`), WebSocket upgrade failures, collab endpoints, `/metrics` (401/404), and admin endpoints.
- `HEAD` requests: same status and headers, no body.

#### 10.2.4 Authentication challenges
401 responses on bearer-protected endpoints include `WWW-Authenticate: Bearer realm="rackend"` and, where relevant, `error="invalid_token"` (RFC 6750), alongside the problem body.

#### 10.2.5 Protocol-mandated formats
Some standards define their own error format. rackend follows those standards for compatibility and **adds the RFC 9457 members** to the same object wherever the format is a JSON body:
- **OAuth 2.0 / OIDC token, revocation and userinfo endpoints** (§11.8): body contains `error` and `error_description` exactly as RFC 6749 §5.2 / RFC 6750 require, plus `type`, `title`, `status`, `instance`, `code`, `request_id`. Content type stays `application/json`, as standard OAuth clients expect.
- **Authorization endpoint** (§11.8): errors after a valid redirect URI is established are returned as redirect query parameters (`error`, `error_description`, `state`) per RFC 6749 §4.1.2.1. Errors before that (unknown client, invalid redirect URI) render the hosted error page (below).
- **Hosted HTML pages** (`/auth/*`, `/_/` admin UI shell): browsers navigating to them get a human-readable HTML error page. If the request's `Accept` header prefers `application/json` or `application/problem+json`, the problem object is returned instead. The HTML page shows `title`, `detail` and `request_id`.
- **WebSocket** (§13.2.2, §13.4): after the upgrade, errors are sent as `{"t":"err","id":<n>,"problem":{...RFC 9457 object...}}` frames (`status` is the closest HTTP equivalent). Close frames use standard WebSocket close codes with the problem `code` as the reason text.
- **SSE** (§13.2.1): after the stream is open, errors are sent as `event: error` with an RFC 9457 object as `data`.

#### 10.2.6 Testing
- A contract test enumerates every registered route and method, triggers representative failures (bad JSON, missing auth, wrong method, oversized body, unknown id, rate limit), and asserts: `Content-Type: application/problem+json`, `status` member = HTTP status, `type` documented in `docs/errors.md`, `request_id` = `X-Request-Id`.
- A test asserts that `internal` errors never expose the underlying error message (an injected error with a secret marker must not appear in the response body).

### 10.3 Health & meta
| Method | Path | Auth | Description |
|---|---|---|---|
| GET | `/api/health` | none | `{"status":"ok"|"migrating","version":"x.y.z"}` |
| GET | `/api/info` | none | Public app info: name, enabled auth methods per auth collection, providers (name, display, icon), public URL |

### 10.4 Collections (superuser)
| Method | Path | Description |
|---|---|---|
| GET | `/api/collections` | List (paginated) |
| POST | `/api/collections` | Create |
| GET | `/api/collections/{idOrName}` | View |
| PATCH | `/api/collections/{idOrName}` | Update (schema change, §8.5); `?dry_run=true` returns the plan |
| DELETE | `/api/collections/{idOrName}` | Soft delete |
| GET | `/api/collections/{idOrName}/history` | Schema history |
| POST | `/api/collections/import` | Import a full schema (`?dry_run=true`) |
| GET | `/api/collections/export` | Export a full schema |
| GET | `/api/trash` / POST `/api/trash/{id}/restore` / DELETE `/api/trash/{id}` | Trash management (delete = purge now, requires confirmation param `?confirm=<original_name>`) |

### 10.5 Records
| Method | Path | Description |
|---|---|---|
| GET | `/api/collections/{c}/records` | List |
| GET | `/api/collections/{c}/records/{id}` | View |
| POST | `/api/collections/{c}/records` | Create |
| PATCH | `/api/collections/{c}/records/{id}` | Update (partial) |
| DELETE | `/api/collections/{c}/records/{id}` | Delete |
| POST | `/api/batch` | Transactional batch |

List/view query parameters:
- `page` (default 1), `per_page` (default 30, max 500).
- `sort`: comma-separated, `-` prefix for DESC, e.g. `-created,title`. Allowed: system and scalar fields; `@random`. Default: `-id` (UUIDv7 ≈ creation order, uses the primary key index).
- `filter`: rule-language expression (§9).
- `expand`: comma-separated relation paths, max depth 3, e.g. `author,comments_via_post.author`. Every expanded record is subject to **its own collection's view rule**. Non-viewable relations are silently omitted from `expand` (the id stays in the field).
- `fields`: comma-separated projection including `expand.*` paths, e.g. `id,title,expand.author.name`. `*` wildcard per level.
- `skip_total=true`: skip the `COUNT(*)` (response `total_items` / `total_pages` = -1).

List response:
```json
{ "page": 1, "per_page": 30, "total_items": 120, "total_pages": 4, "items": [ { "id": "...", "collection_id": "...", "collection_name": "posts", "created": "...", "updated": "...", "title": "...", "expand": { "author": { ... } } } ] }
```

Update body modifiers for multi-valued fields: `"tags+": ["a"]` (append, deduplicated), `"tags-": ["b"]` (remove), `"+tags"` (prepend). Number fields: `"views+": 1` (atomic increment). Modifiers are evaluated inside the transaction.

Optimistic concurrency: `If-Match: "<updated>"` on PATCH/DELETE → 412 if the record changed. Records carry `ETag: "<updated>"`.

Batch: `POST /api/batch` with `{ "requests": [ { "method": "POST", "url": "/api/collections/posts/records", "body": {...} }, ... ] }`, max 50 requests (configurable). All run in **one** write transaction with full rule checks per sub-request. Any failure → rollback all, response lists per-request results. Files are not supported in batch v1.

### 10.6 Auth endpoints (per auth collection `{c}`)
| Method | Path | Description |
|---|---|---|
| GET | `/api/collections/{c}/auth-methods` | Enabled methods + providers |
| POST | `/api/collections/{c}/auth-with-password` | `{email, password}` → tokens |
| POST | `/api/collections/{c}/auth-refresh` | `{refresh_token}` (or cookie) → rotated tokens |
| POST | `/api/collections/{c}/logout` | Revoke current session |
| GET | `/api/collections/{c}/sessions` | Own sessions (device list) |
| DELETE | `/api/collections/{c}/sessions/{id}` | Revoke one (`/sessions` with `?all=true` → all but current) |
| POST | `/api/collections/{c}/request-verification` | `{email}` → 204 always |
| POST | `/api/collections/{c}/confirm-verification` | `{token}` |
| POST | `/api/collections/{c}/request-password-reset` | `{email}` → 204 always |
| POST | `/api/collections/{c}/confirm-password-reset` | `{token, password}` → revokes all sessions |
| POST | `/api/collections/{c}/request-email-change` | `{new_email}` (authenticated) |
| POST | `/api/collections/{c}/confirm-email-change` | `{token, password?}` |
| POST | `/api/collections/{c}/request-otp` | `{email}` → `{otp_id}` always (also for unknown emails) |
| POST | `/api/collections/{c}/auth-with-otp` | `{otp_id, code}` → tokens |
| POST | `/api/collections/{c}/request-magic-link` | `{email, redirect_uri}` → 204 always |
| POST | `/api/collections/{c}/auth-with-magic-link` | `{token}` → tokens |
| POST | `/api/collections/{c}/passkeys/register/start` / `finish` | Authenticated; add a passkey |
| POST | `/api/collections/{c}/passkeys/login/start` / `finish` | Discoverable-credential login |
| GET / DELETE | `/api/collections/{c}/passkeys[/{id}]` | Manage own passkeys |
| GET | `/api/collections/{c}/providers/{provider}/start` | Redirects to the external provider (`?redirect_uri=` for SPA return; must be allow-listed, §11.7) |
| GET | `/api/collections/{c}/providers/{provider}/callback` | Provider callback |
| POST | `/api/collections/{c}/auth-with-exchange-code` | `{code}` → tokens (after provider login with `redirect_uri`, §11.7) |
| GET / DELETE | `/api/collections/{c}/identities[/{id}]` | Linked external identities; unlinking the last login method is rejected |

Token response:
```json
{ "access_token": "...", "token_type": "Bearer", "expires_in": 900, "refresh_token": "...", "record": { ... } }
```

### 10.7 Superusers
`/api/superusers/auth-with-password`, `/auth-refresh`, `/logout`, `/passkeys/...`, plus CRUD `/api/superusers` (superuser only). `/_/setup` flow: `POST /api/setup {token, email, password, public_url}`.

### 10.8 OIDC provider endpoints (§11.8)
`/.well-known/openid-configuration`, `/oauth2/authorize`, `/oauth2/token`, `/oauth2/userinfo`, `/oauth2/jwks`, `/oauth2/revoke`, `/oauth2/logout`, plus hosted pages `/auth/login`, `/auth/consent`, `/auth/logout`, `/auth/verify`, `/auth/reset`. Admin: CRUD `/api/oauth-clients`.

### 10.9 Files (§12)
| Method | Path | Description |
|---|---|---|
| GET | `/api/files/{collection}/{record_id}/{filename}` | Download; `?thumb=WxH[mode]`, `?download=1`, `?token=` for protected files |
| POST | `/api/files/token` | Authenticated → short-lived file token |

### 10.10 Realtime (§13)
`GET /api/realtime` (SSE), `POST /api/realtime` (set SSE subscriptions), `GET /api/ws` (WebSocket), `GET /api/collab/{collection}/{record_id}/{field}` (WebSocket, y-sync). Admin: CRUD `/api/channels`.

### 10.11 Admin & observability (superuser)
`/api/settings` (GET/PATCH; secrets returned masked), `/api/settings/test-smtp`, `/api/settings/test-storage`, `/api/audit` (filter/sort/paginate), `/api/logs` (requests), `/api/metrics/summary`, `/api/metrics/series?series=...&from=...&to=...`, `/api/backups` (GET list, POST create, GET `/{name}` download), `/api/auth-providers` CRUD + `POST /api/auth-providers/discover {issuer}`, `/metrics` (Prometheus, §14.4).

### 10.12 OpenAPI
rackend generates an OpenAPI document describing its HTTP API **for the instance's current schema**, so users can generate typed API clients in any language with standard tools (openapi-generator, oapi-codegen, progenitor, …). The generated OpenAPI document is the supported integration path for API clients.

Content:
- All client-facing endpoints: records (per collection), auth (per auth collection, only the methods enabled for it), files, realtime SSE handshake/subscription endpoint, OIDC provider discovery link, health/info.
- **Per-collection component schemas**, derived from the live collection schema: `<Name>Record` (response), `<Name>Create` (request), `<Name>Update` (request, all optional + modifiers like `tags+`), `<Name>List` (paginated response). `<Name>` = PascalCase of the collection name.
- Field type mapping: `text`/`email`/`url` → `string` (with `format: email|uri`, `minLength`/`maxLength`/`pattern`), `number` → `number` or `integer`, `bool` → `boolean`, `date`/`autodate` → `string, format: date-time`, `select` → `enum` (array of enum for multi), `json` → `{}` (any), `relation` → `string, format: uuid` (array for multi) plus a typed `expand` object, `file` → `string` in records and `string, format: binary` in multipart request bodies, `ydoc` → omitted.
- Query parameters (`page`, `per_page`, `sort`, `filter`, `expand`, `fields`, `skip_total`) with descriptions; the rule-language grammar is linked, not modelled.
- Error responses as RFC 9457 problem schemas (§10.2).
- Security schemes: `bearerAuth` (HTTP bearer), `cookieAuth` (apiKey in cookie), and `oidc` (`openIdConnect`, pointing at the discovery document) once the OIDC provider is enabled.
- Superuser/admin endpoints (§10.4, §10.11) are **excluded by default**; included with `?include=admin` (superuser only).

Access & formats:
- `GET /api/openapi.json` (and `/api/openapi.yaml`). Default: superuser only, because the document reveals the schema. Setting `api.openapi_public` (default `false`) makes the client-facing document public.
- `?version=3.1` (default) or `?version=3.0` (3.0.3 output for generators with incomplete 3.1 support: `nullable` instead of type arrays, etc.).
- CLI: `rackend openapi export [--version 3.0|3.1] [--include-admin] [--format json|yaml]` writes the document to stdout. It opens the database **read-only**, so it also works while the server runs (for CI/codegen pipelines).
- Output is **deterministic** (stable key order, stable `operationId`s), so diffs between schema versions are meaningful. `operationId` format: `<collection>_<action>` in snake_case, e.g. `posts_list`, `posts_get`, `posts_create`, `posts_update`, `posts_delete`, `users_auth_with_password`. Static endpoints use fixed ids (`health_get`, `files_get`, …).
- The admin UI has an **API** page: download buttons for both versions and a per-collection overview of endpoints and example requests (rendered from the document, no third-party viewer required).
- WebSocket and collaborative-editing protocols (§13.2.2, §13.4) cannot be described in OpenAPI and are documented in `docs/realtime-protocol.md`.

---

## 11. Authentication & authorization

### 11.1 Model
```
            ┌───────────── identity sources ─────────────┐
password ─┐ passkey ─┐ OTP/magic link ─┐ external IdP ─┐
          └──────────┴─────────────────┴───────────────┴──▶ user (auth record)
                                                              │
                                                              ▼
                                                     session (_sessions)
                                                              │
                         ┌────────────────────────────────────┼─────────────────────┐
                         ▼                                    ▼                     ▼
              direct API tokens                      cookie session          OIDC provider tokens
            (opaque access+refresh)             (browser, same-site)       (JWT access, ID token,
                                                                             opaque refresh)
```
All login methods produce the same session. All token types resolve to exactly one session row. Revoking the session invalidates every token derived from it **immediately**.

### 11.2 Passwords
- argon2id, `m=19456 KiB, t=2, p=1`, 16-byte salt, PHC string format. Parameters are stored in the hash, so they can be raised later with rehash-on-login.
- Min length per collection (default 10), max 256. No composition rules. Check against a bundled list of the 10k most common passwords.
- Verification runs in `spawn_blocking` with a global semaphore (default `num_cpus`) to bound CPU usage.
- Unknown email: run a dummy hash verification to equalize timing.

### 11.3 Passkeys (WebAuthn)
- `webauthn-rs`, RP ID = host of `app.public_url`, origin = `app.public_url`. Disabled until the public URL is set.
- Discoverable credentials (usernameless login) and user verification `preferred`.
- Ceremony state is stored server-side (in-memory, keyed by a random id, 5 min TTL; single instance makes this safe).

### 11.4 OTP & magic links (require SMTP)
- OTP: 6 digits, 10 min TTL, max 5 attempts, stored as an HMAC-SHA256 hash. Requesting a new OTP invalidates the previous one.
- Magic link / verification / reset / email-change tokens: 32 random bytes, base64url, stored as SHA-256. TTL: magic link 15 min, verify 72 h, reset 1 h, email change 1 h. Single use.
- Email templates (subject + HTML + text) are editable in settings with variables `{{app_name}}`, `{{link}}`, `{{code}}`, `{{email}}`. Defaults are bundled. Templates are rendered with escaping.
- Request endpoints always return 204 / a dummy `otp_id` (no account enumeration).

### 11.5 Sessions & tokens (direct API)
- Access token: `rk_at_<session_id>_<32 random bytes base64url>`. Refresh token: `rk_rt_<session_id>_<32 random bytes>`. Stored as SHA-256 hashes; compared in constant time.
- Default TTLs: access 15 min, refresh 30 days, idle timeout 14 days (per collection, §8.6).
- **Refresh rotation:** each refresh issues a new access+refresh pair and moves the old refresh hash to `prev_refresh_hash`. Presenting `prev_refresh_hash` again (reuse) → revoke the session (`revoked_reason = refresh_reuse`), audit it, 401. A 10 s grace window allows the immediately preceding refresh token to be reused once (to absorb client races); in that case the same new pair is returned.
- Session lookup cache: in-memory LRU (10k entries, 30 s TTL), **explicitly invalidated** on revoke/password change/user delete.
- `last_seen` is updated at most once per minute per session (batched).
- Password change or reset revokes all other sessions of the user. Deleting a user revokes all sessions and identities.
- Session GC job deletes expired/revoked sessions after 30 days (kept for audit/device history).

### 11.6 Browser cookie mode
- Opt-in per request via `X-Rackend-Auth-Mode: cookie` on login/refresh, or automatic for the hosted login pages.
- Cookies: `rk_session` (access) and `rk_refresh` (refresh, `Path=/api/collections/{c}/auth-refresh`). Both `HttpOnly; Secure; SameSite=Lax` (`Secure` dropped only in dev mode on `http://localhost`).
- CSRF: for cookie-authenticated unsafe methods, the `Origin` (or `Referer`) header MUST match `app.public_url` or an entry of `security.cors_origins`. Otherwise 403.

### 11.7 External providers (identity broker)
- Provider kinds:
  - `oidc`: generic. Configuration = `issuer`, `client_id`, `client_secret`, optional `scopes` (default `openid email profile`). Endpoints come from discovery (`/.well-known/openid-configuration`), cached 24 h, with JWKS refresh on unknown `kid`.
  - `oauth2_preset`: built-in adapters for non-OIDC providers. v1: **GitHub** (`/user`, `/user/emails`). Presets for OIDC providers (Google, Microsoft, Apple, GitLab, Authentik, Pocket ID, Keycloak, Zitadel) are just pre-filled `oidc` configs (JSON files bundled in the binary).
- `POST /api/auth-providers/discover {issuer}` validates discovery and returns metadata and the **redirect URI to register at the provider**: `{public_url}/api/collections/{c}/providers/{name}/callback`.
- Flow: authorization code + **PKCE S256 always**, `state` (random 32 bytes, bound to an encrypted, HttpOnly 10-min cookie that also holds the PKCE verifier and nonce), `nonce` check, full ID token validation (`iss`, `aud`, `exp`, `iat`, signature, `azp` when there are multiple audiences).
- Linking (§11.7.1) → create a session → redirect:
  - when started with `redirect_uri` (SPA/mobile): redirect to `redirect_uri#code=<one-time exchange code>`. The client exchanges it via `POST /api/collections/{c}/auth-with-exchange-code {code}` (60 s TTL, single use) for tokens. `redirect_uri` MUST match `security.app_redirect_uris` (exact match, or `http://localhost:*` in dev mode);
  - when started from the hosted login page: continue the OIDC provider flow (§11.8).

#### 11.7.1 Identity linking rules (MUST)
1. Look up `_identities` by `(issuer, sub)`. Found → that user.
2. Not found and the request carries an authenticated session of the same collection with `link=true` → link to that user.
3. Not found, provider `auto_link_verified_email = true`, the ID token says `email_verified = true`, and a user with that email exists → link to that user (audited as `identity.auto_linked`).
4. Not found and `signup_enabled` → create a user (email from the claim if verified, otherwise `email_verified=false`; `name`/avatar not auto-fetched, §17.6), then link.
5. Otherwise → error page `account_not_found`.
- Linking never matches on unverified email. `auto_link_verified_email` defaults to **false**.
- Unlinking: allowed only if the user keeps at least one other login method.

### 11.8 rackend as OIDC provider
#### 11.8.1 Scope (v1 subset)
Supported: Discovery, JWKS, Authorization Code flow with **PKCE S256 required for all clients**, `response_type=code` only, `response_mode=query`, refresh tokens (`offline_access` scope), UserInfo, token revocation (RFC 7009), RP-initiated logout, `prompt=none|login|consent`, `max_age`, `login_hint`, `nonce`, `state`.
Not supported in v1: implicit/hybrid flows, dynamic client registration, request objects, front/back-channel logout, `client_secret_jwt`/`private_key_jwt`, pairwise subjects, token introspection (planned for v1.x), device flow.

#### 11.8.2 Discovery document
`issuer` = `app.public_url` (no trailing slash). `id_token_signing_alg_values_supported: ["RS256","ES256"]`, `token_endpoint_auth_methods_supported: ["client_secret_basic","client_secret_post","none"]`, `code_challenge_methods_supported: ["S256"]`, `scopes_supported: ["openid","profile","email","offline_access"]`, `claims_supported: ["sub","iss","aud","exp","iat","auth_time","nonce","sid","email","email_verified","name","picture","updated_at"]`.

#### 11.8.3 Clients
- `_oauth_clients`: `public` (SPA/mobile, no secret) or `confidential` (secret shown once at creation, stored as a SHA-256 hash).
- `redirect_uris`: exact string match (no wildcards), except a client flagged `dev` may use `http://localhost:*` / `http://127.0.0.1:*` (port wildcard only).
- Each client is bound to **one auth collection**.
- `first_party = true` skips the consent screen.
- First boot creates client `default`: public, first party, bound to `users`, redirect URIs empty (the admin UI shows a "add your app's redirect URI" task). In dev mode it accepts localhost.

#### 11.8.4 Tokens
- **ID token**: JWT signed with the active RS256 key (or ES256 if the client is configured for it). Claims: `iss, sub (user id), aud (client_id), exp (5 min), iat, auth_time, nonce, sid (session id), azp`, plus `email, email_verified` (scope `email`) and `name, picture, updated_at` (scope `profile`, mapped via `oidc_claims`).
- **Access token**: JWT (RS256), `iss, sub, aud = [client_id, "{issuer}/api"], exp (client access_ttl, default 15 min), iat, sid, scope, client_id, jti`. Accepted by rackend's own API: valid signature + `iss` + `aud` + the **session `sid` must be active** (cache lookup), so revocation is instant for rackend itself. External resource servers that only verify signatures see revocation after expiry (documented).
- **Refresh token**: opaque, same rotation and reuse detection as §11.5. Only issued with the `offline_access` scope (first-party clients get it by default).
- Authorization codes: 32 random bytes, stored hashed, 60 s TTL, single use. Reuse → revoke the session created from it.

#### 11.8.5 Hosted pages
- Server-rendered with `askama`, no JS framework. Small vanilla JS only for WebAuthn and progressive enhancement. MUST work without JS for password/OTP.
- `/auth/login`: shows methods enabled for the client's collection: password, passkey button, OTP/magic link, external provider buttons, sign-up link (if `signup_enabled`), forgot password.
- `/auth/consent`: client name, requested scopes, allow/deny. Remembered in `_oauth_consents`.
- Theming via settings: app name, logo (uploaded file), primary color, light/dark auto. Strict CSP (§17.3).
- i18n: English in v1, with strings in a catalog file ready for translation.

#### 11.8.6 Signing keys
- Generated on first boot. Stored in `_keys` with private keys encrypted via `SecretBox` (§17.4).
- Rotation: a new key is generated every 90 days, **published in JWKS 7 days before activation**, and the old key is kept in JWKS for `max(refresh_ttl, 7 days)` after retirement, then deleted.
- `POST /api/keys/rotate` (superuser) forces rotation (emergency). The audit entry is mandatory.

#### 11.8.7 Conformance
The "Basic OP" and "Config OP" profiles of the OpenID Foundation conformance suite MUST pass before the provider is marked stable (M8 acceptance criteria). The suite runs in CI nightly via Docker.

### 11.9 Superusers
- Stored in `_superusers`, separate from auth collections. Login via password and passkey (direct API only in v1; admin UI). Separate sessions (`subject_type = superuser`), TTL access 15 min, refresh 7 days.
- Superuser tokens are rejected on the OIDC provider endpoints, and user tokens are rejected on admin endpoints.
- MFA: passkey as a second factor for superusers (setting `security.superuser_require_passkey`, default false but strongly recommended in the UI).

### 11.10 Rate limits (defaults, configurable in settings)
| Bucket | Limit |
|---|---|
| Login attempts (password/OTP) per IP | 20 / min |
| Login attempts per account | 10 / 15 min (then 15-min exponential backoff) |
| Email-sending endpoints per IP | 5 / 10 min |
| Email-sending endpoints per target email | 3 / hour |
| Token endpoint per client | 60 / min |
| Global per IP (all API) | 600 / min |
Exceeding → 429 + `Retry-After`. IPs come from the socket, or from `X-Forwarded-For` only for trusted proxies.

---

## 12. File storage

### 12.1 BlobStore port
```rust
trait BlobStore: Send + Sync {
    async fn put(&self, key: &str, body: BlobBody, meta: BlobMeta) -> Result<()>;  // streaming
    async fn get(&self, key: &str, range: Option<ByteRange>) -> Result<BlobStream>;
    async fn head(&self, key: &str) -> Result<Option<BlobMeta>>;
    async fn delete(&self, key: &str) -> Result<()>;                               // idempotent
    async fn list(&self, prefix: &str) -> Result<BoxStream<Result<BlobListItem>>>;
    async fn presign_get(&self, key: &str, ttl: Duration) -> Result<Option<Url>>;  // fs → None
}
```
Only these operations are allowed, so every S3-compatible backend behaves the same.

### 12.2 Backends
- `blob-fs` (default): root `/data/storage`. Writes go to `tmp/` first, then `fsync` + atomic `rename`. Keys map to paths; keys are validated to never contain `..` or absolute paths.
- `blob-s3`: `object_store` with `endpoint`, `region`, `bucket`, `access_key_id`, `secret_access_key` (encrypted), `force_path_style` (default **true**, needed for MinIO/Garage/RustFS), optional `prefix`, `allow_http` (dev only). Multipart uploads above 16 MB.
- `POST /api/settings/test-storage` performs put/head/get/delete on a probe key and reports each step.
- Contract tests (M3) MUST pass against **MinIO and Garage** (and RustFS when a stable image is available) via `testcontainers`.

### 12.3 Upload pipeline
1. Stream each multipart file to `tmp/` while computing SHA-256 and enforcing `max_size` (abort early → 413).
2. Sniff the MIME type from content (magic bytes), not from the client header. Check against the field's `mime_types`.
3. Images (`image/jpeg`, `image/png`, `image/webp`, `image/gif`): probe dimensions; reject when width × height > 50 MP (decompression-bomb guard). **Normalize**: apply EXIF orientation and strip all EXIF/XMP/IPTC metadata (privacy: GPS) by re-encoding (JPEG quality 90; PNG lossless; WebP lossless; GIF untouched except metadata where possible). Configurable per field: `strip_metadata` (default true).
4. Storage key: `{collection_id}/{record_id}/{sanitized_basename}_{8 random chars}.{ext}`. Sanitize: lowercase, `[a-z0-9_-]`, max 100 chars.
5. Write: blob `put` **before** the DB transaction. The DB transaction inserts `_files` rows and the record. If the transaction fails, an outbox action deletes the blobs. A startup + daily GC job deletes blobs with no `_files` row older than 1 hour (orphan protection).
6. Replacing/removing files in an update: `_files.deleted_at` is set in the transaction, and the blob is deleted by the outbox after commit (plus the GC job as a safety net).

### 12.4 Serving
- `GET /api/files/{collection}/{record_id}/{filename}`: authorizes via the record's **view rule** (404 if not viewable). Protected fields (`protected=true`) additionally require `?token=` (file token: HMAC-SHA256 signed, 2 min TTL, bound to the user and collection) so plain `<img src>` URLs can't be shared indefinitely. Non-protected files of viewable records need no token. (Rule checks with cookie/bearer auth still apply when present.)
- Headers: `Content-Type` from stored mime, `Content-Disposition` (`inline` for images/PDF/video/audio, `attachment` otherwise, or forced by `?download=1`), `ETag` = sha256, `Cache-Control: private, max-age=3600` (public files: `public, max-age=86400`), `X-Content-Type-Options: nosniff`. SVG and HTML are always served as `attachment` (XSS).
- Range requests are supported (video).
- S3: by default rackend proxies the stream. Setting `storage.s3.redirect_downloads=true` → 302 to a presigned URL (60 s) after the authorization check.

### 12.5 Thumbnails
- Spec syntax `WxH[mode]`: `100x100` (center crop), `100x100t` (crop top), `100x100b` (crop bottom), `100x100f` (fit inside, no crop), `0x200` (height 200, keep ratio), `300x0` (width 300, keep ratio).
- Only sizes listed in the field's `thumbs` option are allowed (plus a global default `100x100`). Others → 400. This prevents CPU/storage DoS.
- Generated **lazily** on first request, with `fast_image_resize` (Lanczos3). Output format: **WebP** (lossy, quality 80, via `webp`/libwebp, statically linked) for photos; PNG input with alpha → WebP with alpha. GIF → first frame. Non-image files → 404 for thumb requests.
- Concurrency: a semaphore (default `num_cpus`) plus **single-flight** per (key, spec), so concurrent requests for the same thumb generate it once.
- Cache: stored in BlobStore under `_thumbs/{storage_key}/{spec}.webp`. Deleted together with the original. A size-capped LRU GC (setting `storage.thumb_cache_max_mb`, default 2,048) runs daily.

### 12.6 Backend switching
Changing `storage.backend` while `_files` has rows is rejected in v1 unless `?force=true`. The admin UI explains that existing files must be copied manually (a `rackend storage migrate` CLI is planned for v1.x).

---

## 13. Realtime

### 13.1 Hub
- In-process hub in `core::realtime`. One instance only, so no external broker.
- Events originate **only** from the post-commit outbox of use cases (never from SQLite hooks), so they carry the full before/after records and the actor.
- Per collection, a `tokio::sync::broadcast` channel (capacity 1,024). A subscriber that lags beyond the capacity receives a `resync` event (it should refetch) instead of silently missing events.
- Connection limits (settings): max connections per IP (default 100), max subscriptions per connection (default 100), max message size 64 KB in, max outbound queue per connection 1 MB (exceeded → disconnect with reason `slow_consumer`).

### 13.2 Record subscriptions
Topics:
- `records:{collection}`: all records of a collection (optionally with `filter`).
- `records:{collection}/{record_id}`: a single record.
- Options per subscription: `filter` (rule language, no `@collection`), `expand`, `fields`.

Authorization per event, per subscriber (MUST):
- `create`/`update`: the subscriber must satisfy the collection's **view rule** for the new record **and** match the subscription filter. If an update makes a record leave the subscriber's visible set, the subscriber receives `delete` (`reason: "no_longer_visible"`).
- `delete`: evaluated against the record as it was before deletion.
- Evaluation is done in SQLite via the compiled rule on the reader pool, batched per event across subscribers that share the same auth subject and filter (dedupe). The `core` reference evaluator MAY be used for rules with no relation traversal (optimization; must give identical results, §9.6).
- Expanded relations in events also apply the view rules of the related collections.
- When a subscriber's session is revoked, its connection is closed within 5 s (session revocation publishes an internal event).

Event payload:
```json
{ "type": "record", "topic": "records:messages", "action": "create", "record": { ... }, "ts": "..." }
```

#### 13.2.1 SSE transport
1. `GET /api/realtime` → `event: connect` with `{"client_id":"..."}`. Heartbeat comment every 25 s.
2. `POST /api/realtime {client_id, subscriptions: [ {topic, filter?, expand?, fields?} ]}` with the normal Authorization header **binds the auth subject** to that client id and replaces its subscription set. Tokens are never put in the URL.
3. Events: `event: <topic>`, `data: <json>`, `id: <monotonic seq>`. On reconnect with `Last-Event-ID`, rackend replays from a per-connection ring buffer (last 256 events, 2 min) when available, else sends `resync`.

#### 13.2.2 WebSocket transport
`GET /api/ws` (auth via `Authorization` header, cookie, or a first `auth` message within 5 s; browsers can't set headers on WS, so the `auth` message is the primary path). JSON text frames:
```
client → server
{ "t": "auth",   "token": "..." }
{ "t": "sub",    "id": 1, "topic": "records:messages", "filter": "room = 'x'", "expand": "author" }
{ "t": "unsub",  "id": 2, "topic": "records:messages" }
{ "t": "join",   "id": 3, "channel": "room:42", "meta": { "status": "typing" } }   // §13.3
{ "t": "leave",  "id": 4, "channel": "room:42" }
{ "t": "pub",    "id": 5, "channel": "room:42", "event": "cursor", "data": { ... } }
{ "t": "meta",   "id": 6, "channel": "room:42", "meta": { ... } }                  // update presence meta
{ "t": "ping" }

server → client
{ "t": "ack",    "id": 1 }
{ "t": "err",    "id": 1, "code": "forbidden", "message": "..." }
{ "t": "record", "topic": "...", "action": "...", "record": { ... } }
{ "t": "msg",    "channel": "room:42", "event": "cursor", "from": { "user_id": "...", "conn_id": "..." }, "data": { ... } }
{ "t": "presence", "channel": "room:42", "kind": "sync" | "join" | "leave" | "meta", "members": [ ... ] }
{ "t": "resync", "topic": "..." }
{ "t": "pong" }
```
The server sends WS pings every 25 s and closes after 60 s without a pong.

### 13.3 Channels & presence
- Channels are defined by superusers in `_channels`: `name` (e.g. `room`), `pattern` (`room:*`; the part after `:` is available as `@channel.key`, max 128 chars), `join_rule`, `publish_rule` (rule language with `@request.auth.*`, `@channel.key`, `@collection.*`), `presence` (bool), `options` (max members, message rate).
- Example join rule: `@collection.room_members.room = @channel.key && @collection.room_members.user = @request.auth.id`.
- Messages are **ephemeral**: not stored, delivered at most once to currently joined members (including the sender only if `echo=true`). Max payload 16 KB. Default rate 20 msgs/s per connection per channel.
- Presence: on join, the member gets `presence.sync` (the full member list); others get `presence.join`. Member = `{ user_id, conn_id, collection, meta (≤ 1 KB JSON), joined_at }`. Leave on explicit leave or disconnect. Disconnects are debounced 3 s, so brief reconnects don't flap.
- Use cases covered: chat typing indicators, online lists, live cursors, ephemeral notifications.
- Chat messages themselves are normal records (persistent), delivered via record subscriptions.

### 13.4 Collaborative editing (`ydoc` fields)
- A `ydoc` field holds a Yjs document. The endpoint `GET /api/collab/{collection}/{record_id}/{field}` upgrades to a WebSocket speaking the **y-protocols sync + awareness protocol (binary)**, compatible with `y-websocket` clients (Tiptap, BlockNote, CodeMirror, Monaco bindings).
- Auth: first message or cookie (same as §13.2.2), then:
  - connect requires the **view rule**;
  - sync step 2 / update messages are applied only if the subscriber satisfies the **update rule** (re-evaluated every 30 s and on schema/session change); otherwise the connection is read-only and incoming updates are dropped with an `err` (custom message type 100, documented).
- Server state: one in-memory `yrs::Doc` per open (record, field), loaded from the compacted BLOB + pending `_ydoc_updates`, and unloaded 60 s after the last client leaves.
- Persistence: each received update is appended to `_ydoc_updates` (writer queue, batched every 250 ms). Compaction (encode full state → write BLOB to the record column → delete merged updates, in one transaction) runs when > 500 pending updates or after 30 s idle.
- `updated` of the record is touched at most every 30 s during editing, and a throttled `record update` event is emitted, with the `ydoc` field omitted.
- REST: `ydoc` fields are omitted from record JSON by default. `GET /api/collab/{c}/{id}/{field}/state` returns the binary state (`application/octet-stream`) for SSR/backup. `PUT .../state` (update rule) replaces it.
- Awareness (cursors/selections) is relayed in memory only.
- Size limit per document: 10 MB compacted (configurable). Exceeding → updates rejected.

---

## 14. Observability

### 14.1 Audit trail (`_audit` in `data.db`)
- Written **in the same transaction** as the change it describes (Unit of Work, §4.4). If the audit insert fails, the change fails.
- Actor: `actor_type` ∈ `superuser | user | client | system | anonymous`, `actor_id`, `actor_label` (email/name at the time), `ip`, `user_agent`, `request_id`.
- Actions (non-exhaustive, dot-namespaced):
  - `record.create|update|delete` (diff: changed fields only, before/after),
  - `collection.create|update|delete|restore|purge`,
  - `auth.login.success|failure`, `auth.logout`, `auth.refresh_reuse`, `auth.password.change|reset`, `auth.email.change`, `auth.session.revoke`, `identity.link|unlink|auto_linked`, `passkey.add|remove`,
  - `oauth.authorize`, `oauth.token`, `oauth.consent.grant|revoke`, `oauth.client.create|update|delete`,
  - `settings.update`, `superuser.create|update|delete`, `keys.rotate`, `backup.create|restore`, `provider.create|update|delete`.
- Diffs: field types `password`/secrets → `"[redacted]"`. `ydoc` and file contents are never included (file names are). `json` fields are included up to 8 KB, then truncated with a marker.
- Per-collection option `audit_records` (default **true**) can disable record-level audit for high-volume collections (e.g. telemetry). Auth, schema and settings audit cannot be disabled.
- Failed logins are audited even though they change no data (written as their own small transaction).
- Retention: setting `audit.retention_days` (default 365, `0` = forever). Deletion by a daily job, in batches of 10k.
- Admin UI: global timeline with filters (actor, action, collection, record, time range), plus a per-record history tab.
- Export: `GET /api/audit?format=ndjson` streaming.

### 14.2 Request logs (`logs.db`)
- One row per HTTP request (excluding `/api/health` and static assets): method, matched route template, status, duration, IP, actor, request id, bytes, error code.
- Setting `logs.retention_days` (default 7), `logs.min_status` (default 0 = all; e.g. 400 = errors only), `logs.sample_rate` for 2xx (default 1.0).
- Batched inserts. Losing logs on crash is acceptable.

### 14.3 Metrics
Collected via the `metrics` facade:
- HTTP: `rackend_http_requests_total{route,method,status}`, `rackend_http_request_duration_seconds` (histogram).
- DB: `rackend_db_write_queue_depth`, `rackend_db_write_duration_seconds`, `rackend_db_read_duration_seconds`, `rackend_db_size_bytes{db}`, `rackend_db_wal_size_bytes{db}`, `rackend_db_query_timeouts_total`.
- Realtime: `rackend_realtime_connections{transport}`, `rackend_realtime_subscriptions`, `rackend_realtime_events_total{kind}`, `rackend_realtime_dropped_total{reason}`, `rackend_channels_members`, `rackend_collab_docs_loaded`.
- Auth: `rackend_auth_logins_total{method,result}`, `rackend_sessions_active`.
- Files: `rackend_storage_bytes`, `rackend_thumbnails_generated_total`, `rackend_thumbnail_cache_hits_total`.
- Process: RSS, CPU seconds, open FDs, tokio tasks.
- Host (via `HostMetrics`): CPU usage %, load average, memory used/total, disk used/free **of the data dir's filesystem**, network rx/tx. Inside containers, read cgroup v2 (`/sys/fs/cgroup/memory.max`, `memory.current`, `cpu.max`, `cpu.stat`) and report container limits/usage; also report `container=true`.

### 14.4 Exposure
- Built-in dashboard (zero config): a sampler job writes selected series every 60 s to `logs.db._metric_points` (retention setting `metrics.retention_days`, default 7). The admin UI dashboard shows: requests/s, error rate, p50/p95/p99 latency, write queue depth, DB size, realtime connections, CPU/memory/disk (host + container), top routes, recent errors, disk-space warning when < 10% free.
- Prometheus: `GET /metrics`, disabled until `metrics.prometheus_token` is set (bearer token) or `metrics.prometheus_public=true`.
- OTLP: when `RACKEND_OTLP_ENDPOINT` is set, export traces (one span per request + DB spans) and metrics.
- Alert hooks (v1.x): out of scope; documented as future work.

### 14.5 Tracing & logs
- Structured JSON logs to stdout (Docker-friendly). Every log line inside a request carries `request_id`.
- Spans: `http.request` → `usecase.<name>` → `db.read|write`, `blob.*`, `mail.send`, `idp.*`.

---

## 15. Settings (runtime configuration)

Stored in `_settings` as JSON per key. Secrets (`is_secret=1`) are encrypted (§17.4) and returned as `"••••"` with a `set: true|false` flag. Updating a secret requires sending the new value; omitting it keeps the old one.

| Key | Contents (defaults) |
|---|---|
| `app` | `name` ("rackend"), `public_url` (unset), `logo`, `primary_color`, `support_email` |
| `smtp` | `host`, `port`, `username`, `password`*, `tls` (`starttls`\|`tls`\|`none`), `from_name`, `from_address` |
| `email_templates` | per template: subject, html, text |
| `storage` | `backend` (`fs`\|`s3`), `s3.{endpoint, region, bucket, prefix, access_key_id, secret_access_key*, force_path_style, redirect_downloads}`, `thumb_cache_max_mb` |
| `security` | `cors_origins` ([]), `app_redirect_uris` ([]), `trusted_proxies` (env takes precedence), `superuser_require_passkey` (false), `rate_limits{...}` |
| `audit` | `retention_days` (365) |
| `logs` | `retention_days` (7), `min_status` (0), `sample_rate` (1.0) |
| `metrics` | `retention_days` (7), `prometheus_token`*, `prometheus_public` (false) |
| `backups` | `schedule` (cron, default `0 3 * * *`), `keep` (7), `include_storage` (true for fs) |
| `realtime` | limits (§13.1) |
| `schema` | `trash_retention_days` (30) |
| `limits` | `max_json_body_kb` (1024), `batch_max_requests` (50), `rule_timeout_ms` (2000) |

`*` = secret. Every settings change is audited (the diff shows secret keys as changed/unchanged only).

CORS default: if `security.cors_origins` is empty → `Access-Control-Allow-Origin: *` **without** credentials (bearer-token SPAs work out of the box). Cookie auth from other origins requires listing them explicitly. Dev mode: localhost origins with credentials.

---

## 16. Admin UI

- SvelteKit (Svelte 5 runes) with `adapter-static`, served at `/_/` from the embedded bundle (`rust-embed`), SPA fallback to `index.html`. Built with bun. Uses the public REST API only (no private endpoints except `/api/setup`).
- Auth: superuser direct API with cookie mode.
- Screens (v1):
  1. **Setup** (first boot): create the superuser, confirm the public URL.
  2. **Collections**: list, create/edit with a field editor, rules editor with **live compile and error highlighting** (`POST /api/rules/check {collection, rule}` → errors with position), schema diff preview before applying (dry run), indexes, history, trash.
  3. **Records browser**: table per collection with filter bar (rule language), sort, expand, inline edit, file previews/thumbnails, relation pickers, record audit history tab, live updates via realtime.
  4. **Auth**: auth collection options, providers (add via issuer + discover, copyable redirect URI, test login button), OIDC clients (create, redirect URIs, show secret once), sessions overview with revoke, signing keys and rotation.
  5. **Channels**: CRUD + live presence view.
  6. **Files**: storage settings + test, usage stats.
  7. **Observability**: dashboard (§14.4), audit timeline, request logs.
  8. **Settings**: app, SMTP (+ test send), security, retention, backups (list/create/download).
  9. **Notices**: a persistent checklist of incomplete setup (public URL, SMTP, encryption key, superuser passkey, default client redirect URIs, low disk space, data dir on network FS).
- Accessibility: keyboard navigable, labels, contrast WCAG AA. Light/dark mode.

---

## 17. Security requirements

### 17.1 General
- All randomness from the OS CSPRNG (`rand::rngs::OsRng`).
- All secret comparisons constant-time (`subtle`).
- No secret, token, password or cookie value is ever logged (§3.4) or included in error responses.
- Dependencies are checked with `cargo deny` (advisories, licenses, bans) in CI.

### 17.2 HTTP
- Security headers on all responses: `X-Content-Type-Options: nosniff`, `Referrer-Policy: strict-origin-when-cross-origin`, `X-Frame-Options: DENY` (admin UI + hosted auth pages), `Strict-Transport-Security` when `public_url` is https.
- Request timeouts: header read 10 s, body 60 s (uploads: per-field size / 100 KB/s, min 60 s), idle keep-alive 60 s.
- Path normalization: reject `..`, encoded slashes in path params.

### 17.3 CSP
- Admin UI: `default-src 'self'; img-src 'self' data: blob:; style-src 'self' 'unsafe-inline'; connect-src 'self'; frame-ancestors 'none'`.
- Hosted auth pages: `default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; form-action 'self' <allowed redirect origins>; frame-ancestors 'none'`.

### 17.4 Secrets at rest
- `SecretBox` encrypts with XChaCha20-Poly1305 using a 32-byte key:
  - from `RACKEND_ENCRYPTION_KEY` if set;
  - otherwise from an auto-generated key stored in `_meta.local_secret_key` (zero-config). This protects against leaked backups only if they're stored separately, so the admin UI shows a notice recommending the env key.
- Switching from the stored key to an env key: on startup, if the env key is set and secrets are encrypted with the stored key, re-encrypt all secrets in one transaction, then delete the stored key. Audited.
- Losing the env key = losing encrypted secrets (SMTP password, S3 keys, client secrets of external providers, signing keys). User data is unaffected; signing keys are regenerated (all OIDC tokens become invalid). This is documented clearly.

### 17.5 Input handling
- Rule/filter injection impossible by construction (§9.5).
- Uploaded files: content sniffing, SVG/HTML served as attachment, metadata stripping (§12).
- JSON depth limit 64, string length limits via field options.

### 17.6 SSRF
- rackend makes outbound requests only to: superuser-configured OIDC issuers/JWKS/token endpoints, S3 endpoints, SMTP. It does **not** fetch user-supplied URLs (e.g. avatar URLs from provider claims are not downloaded in v1).
- Outbound HTTP client: 10 s timeout, max 5 redirects, max response 1 MB for discovery/JWKS.

### 17.7 Threat model notes
A short `docs/threat-model.md` MUST exist by M3, covering: account takeover via linking, token theft, rule misconfiguration (mitigated by the locked default, rule compile checks, and an admin UI warning for `""` public rules on update/delete), DoS via expensive filters (timeouts, limits), and file-based XSS.

---

## 18. Packaging & deployment

### 18.1 Builds
- Release binaries: `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl` (static, `mimalloc`), plus `aarch64-apple-darwin` and `x86_64-apple-darwin` (dev use).
- The admin UI bundle is built first and embedded. `cargo build` without a built UI MUST still work (it embeds a placeholder page that says "UI not built; run `mise run ui:build`").
- `rackend version` prints version, git commit, build date, SQLite version, enabled features.

### 18.2 Dockerfile (reference)
```dockerfile
# --- UI ---
FROM oven/bun:1 AS ui
WORKDIR /src/admin-ui
COPY admin-ui/package.json admin-ui/bun.lock ./
RUN bun install --frozen-lockfile
COPY admin-ui/ ./
RUN bun run build

# --- Rust ---
FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev cmake make perl clang
WORKDIR /src
COPY . .
COPY --from=ui /src/admin-ui/build ./admin-ui/build
RUN cargo build --release -p rackend --locked

# --- Runtime ---
FROM scratch
COPY --from=build /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/
COPY --from=build /src/target/release/rackend /rackend
USER 10001:10001
ENV RACKEND_DATA_DIR=/data
VOLUME ["/data"]
EXPOSE 8080
HEALTHCHECK --interval=15s --timeout=3s --start-period=30s CMD ["/rackend", "healthcheck"]
ENTRYPOINT ["/rackend"]
CMD ["serve"]
```
Notes: `scratch` has no `/data` directory with the right owner. The build stage MUST create `/data` owned by 10001 and copy it in (`COPY --from=build --chown=10001:10001 /data-skel /data`), so named volumes inherit the correct ownership. The image is published multi-arch (amd64, arm64) with tags `x.y.z`, `x.y`, `x`, and `latest`. The docs recommend pinning `x.y`.

### 18.3 Reference `docker-compose.yml`
```yaml
services:
  rackend:
    image: ghcr.io/axanet/rackend:0.1
    restart: unless-stopped
    ports: ["8080:8080"]
    volumes: ["rackend_data:/data"]
    stop_grace_period: 30s
volumes:
  rackend_data:
```

### 18.4 Backups
- `rackend backup create` / scheduled job / admin UI: `VACUUM INTO` a snapshot of `data.db`, plus (fs backend, `include_storage=true`) a `tar.zst` of `storage/` excluding `_thumbs`, written as `backups/backup-<ts>.tar.zst` containing `data.db`, `storage/`, and `manifest.json` (version, schema_version, sha256 per file).
- Storage is archived **after** the DB snapshot. Files uploaded between the two steps are harmless extras. Files deleted in between can be missing (rare). `rackend check` reports missing blobs.
- Restore: CLI only, with the instance stopped (lock). It verifies the manifest checksums, moves the current `data.db` and `storage/` to `backups/replaced-<ts>/` (never deletes), and extracts. A restored DB that is older than the binary is migrated on next start (normal path).
- S3 backend: DB snapshot only. Docs recommend bucket versioning.

---

## 19. Testing strategy

| Layer | What | Tools |
|---|---|---|
| Unit (core) | Domain validation, rule parser/type-checker/evaluator, token logic, linking rules, thumb spec parsing | `cargo test`, `proptest` |
| Rule engine | Parser fuzzing, round-trip, SQL snapshots, evaluator-vs-SQLite equivalence | `cargo-fuzz`, `insta`, `proptest` |
| Adapter | `store-sqlite` against temp DBs; `blob-fs` on tempdir; `blob-s3` contract tests vs MinIO + Garage; `imaging` golden images; `mail-smtp` vs a test SMTP container | `tempfile`, `testcontainers` |
| HTTP integration | In-process server per test (temp data dir), full API flows, rule enforcement matrix (every operation × public/locked/expression × anon/user/other user/superuser) | `reqwest`, `tokio-tungstenite` |
| Realtime | Event delivery + authorization (incl. leave-visibility), lag/resync, presence join/leave/debounce, y-sync with a reference Yjs client | — |
| Upgrade | §7.7 | — |
| OIDC | Unit tests for every validation; nightly OpenID conformance suite (Basic OP, Config OP); broker flow vs a containerized IdP (e.g. Keycloak or Authentik) | Docker |
| E2E | Spawn the release binary, run a scripted scenario (setup → collection → user signup → upload → subscribe → collab) | — |
| Load | Reference scenarios for §2 targets; run before releases | `oha` / `goose` |
| Admin UI | Component tests + a small Playwright smoke suite | bun |

CI (Forgejo Actions or GitHub Actions, both supported by a shared script): `mise run lint`, `mise run test`, `mise run test:upgrade`, `mise run test:s3` (Docker runners), build both arches, build and smoke-test the Docker image (`docker run` → healthcheck → setup via env → simple API call).

---

## 20. Milestones & acceptance criteria

Each milestone ends with: all tests green, docs updated (`docs/`), the spec changelog updated, and a tagged pre-release `0.<milestone>.0-alpha`.

### M0 — Skeleton & safety net
- Workspace layout (§4.1), dependency rules enforced in CI, mise tasks, lint config.
- `rackend serve|version|healthcheck`, bootstrap env (§5.2), data dir layout, lock file, NFS warning.
- `store-sqlite`: connection model (§6.3), PRAGMAs, system migration runner with full §7.3 procedure (backup, downgrade protection, checksums, exit codes), `_meta`, `_migrations`, `_settings`.
- Graceful shutdown (§5.6). RFC 9457 error handling with full coverage (§10.2.3), `docs/errors.md`, error contract test (§10.2.6). Request IDs. Structured logging.
- Upgrade test harness (§7.7) working with a synthetic "v0" fixture.
- Dockerfile + compose, multi-arch build in CI.
**Accept:** `docker run` starts, `/api/health` ok; every error path hit by the contract test returns `application/problem+json`; a second container on the same volume exits 75; a DB with a too-new schema exits 78; a failing test migration leaves data byte-identical and the pre-upgrade backup exists.

### M1 — Collections, records & rules
- Collections CRUD with DDL plans, all field types except `file` and `ydoc`, indexes, soft delete + trash + purge job, schema history, import/export, dry run.
- Rule language: lexer, parser, type checker, reference evaluator, SQL compiler, limits, timeouts, `POST /api/rules/check`.
- Records CRUD, list params (filter/sort/expand incl. back-relations/fields/skip_total), modifiers, `If-Match`, batch.
- Audit trail (records + schema + settings), retention job.
- Minimal admin UI: setup, collections editor with rule checking, records browser.
**Accept:** rule enforcement matrix passes; evaluator-vs-SQL equivalence property tests pass (≥ 10k cases); every write produces exactly one audit row in the same transaction (verified by a fault-injection test that fails the audit insert → no data change).

### M2 — Auth core
- Superusers (password, passkey), first-boot setup flow, public URL confirmation.
- Auth collections with system fields, password auth, sessions with rotation + reuse detection, session cache, cookie mode + CSRF, sessions API, rate limits, `@request.auth` in rules.
- Passkeys for users.
- Secrets at rest (`SecretBox`), signing key generation.
- `docs/threat-model.md`.
**Accept:** refresh reuse revokes the session and is audited; revoked tokens fail within the same request cycle; login timing for unknown vs known email within 10% (statistical test); no secrets in logs (log capture test greps for test secrets).

### M3 — Files & images
- `file` field type, upload pipeline, `_files`, blob-fs, blob-s3, storage settings + test endpoint, serving with rules, protected files + file tokens, range requests, thumbnails (lazy, single-flight, WebP), orphan GC, thumb cache GC.
**Accept:** S3 contract tests pass on MinIO and Garage; EXIF GPS stripped (golden test); a 60 MP image rejected; 100 concurrent requests for the same new thumb generate it exactly once; a failed DB transaction leaves no orphan blob after GC.

### M4 — Realtime I (records)
- Hub, SSE + WS transports, record subscriptions with per-subscriber rule checks, visibility-leave deletes, resync, limits, session-revocation disconnect.
- Admin UI records browser uses realtime.
**Accept:** authorization tests (a user never receives events for records they can't view, including via expand); 10k idle connections on reference hardware under the §2 memory budget (documented measurement).

### M5 — Observability
- Request logs, metrics (§14.3), host + cgroup metrics, sampler, dashboard, `/metrics`, OTLP export, audit UI, log viewer, disk-space warnings.
- Scheduled backups + backup CLI/restore + `rackend check`.
**Accept:** inside a memory-limited container, the dashboard shows the container limit, not host RAM; backup → restore round-trip on a fixture yields identical record hashes.

### M6 — Auth II (broker & email)
- SMTP mailer + templates + test send, verification, password reset, email change, OTP, magic links.
- External providers: generic OIDC with discovery, GitHub preset, bundled OIDC presets, linking rules (§11.7.1), identities API, exchange-code flow for SPAs.
**Accept:** broker login works against a containerized Keycloak (or Authentik) in CI; the auto-link-by-email attack (unverified email at the provider) is blocked (test); no account enumeration on email endpoints (response-equivalence test).

### M7 — Developer experience
- OpenAPI generation (§10.12): per-collection schemas, 3.1 + 3.0 output, admin UI API page, `rackend openapi export` CLI.
- `docs/realtime-protocol.md` (SSE, WebSocket, channels, collab).
- Docs site content: getting started, generating a client from the OpenAPI document, rules guide with multi-value semantics, deployment, upgrades, backup/restore, NFS warning.
**Accept:** the generated documents (3.1 and 3.0, for a fixture schema covering all field types) pass an OpenAPI validator in CI; clients generated with openapi-generator (`typescript-fetch` and `python`) compile and pass a smoke test against a running instance (auth with password, create/list/update/delete a record, upload and download a file); the getting-started guide works verbatim on a clean machine (scripted in CI).

### M8 — OIDC provider
- Discovery, JWKS, authorize (hosted login + consent), token, userinfo, revoke, logout, clients CRUD + UI, JWT access tokens accepted by rackend's API with session check, key rotation job + manual rotation, theming.
**Accept:** OpenID conformance suite Basic OP + Config OP pass; a sample SPA (`examples/oidc-spa`) logs in via the `default` client using a standard OIDC library.

### M9 — Realtime II (channels, presence, collab)
- Channels with rules, broadcast, presence (debounced), admin UI live view.
- `ydoc` field type, y-sync endpoint, persistence/compaction, read-only enforcement, state REST endpoints.
**Accept:** two browser clients with Tiptap + `y-websocket` co-edit a document, restart the server mid-session, and no edits are lost; a read-only user's updates are rejected and never persisted.

**v1.0.0** = M0–M9 complete + load targets (§2) met + full upgrade test coverage from the first public alpha.

---

## 21. Open questions & deferred items
| Topic | Status |
|---|---|
| License (MIT vs Apache-2.0 vs AGPL) | Open, decide before first public release |
| Extensibility (hooks, JS runtime, WASM) | Deferred post-v1 |
| `view` collections (SQL views) | Deferred |
| Storage backend migration CLI | v1.x |
| Token introspection endpoint | v1.x |
| Superuser login via external OIDC (company SSO for admins) | v1.x |
| Webhooks / alert hooks | v1.x |
| Cursor pagination | v1.x |
| Full-text search (FTS5) | v1.x candidate |
| AsyncAPI document for the WebSocket/realtime protocols | v1.x |
| gRPC / MCP adapters | Post-v1; architecture already allows them (§4.2) |
| i18n of hosted pages beyond English | v1.x |

---

## 22. Changelog
- **0.1 (2026-11-17)**: initial draft.
- **0.2 (2026-11-17)**: owner axanet / repo and image locations; expanded OpenAPI generation (§10.12) and M7; all errors follow RFC 9457 with full coverage and documented protocol-mandated cases (§10.2).
