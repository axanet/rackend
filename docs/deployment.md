# Deploying rackend

## Quick start

```sh
docker run -d --name rackend -v rackend_data:/data -p 8080:8080 ghcr.io/axanet/rackend:0.1
```

No environment variables are required. Everything except the few bootstrap variables below is configured at runtime in the admin UI at `/_/`.

A reference `docker-compose.yml` is in the repository root. Pin the minor version (`x.y`) so upgrades are deliberate.

## Storage

rackend keeps everything in one data directory (`/data` in the container):

| Path | Contents |
|---|---|
| `rackend.lock` | Exclusive lock, held while the instance runs |
| `data.db` | Main SQLite database (+ `-wal`, `-shm`) |
| `logs.db` | Request logs and metrics; losing it is harmless |
| `storage/` | Uploaded files (local filesystem backend) |
| `backups/` | Pre-upgrade, manual and scheduled backups |
| `tmp/` | Upload staging, wiped at startup |

> [!WARNING]
> **Do not put the data directory on NFS, SMB/CIFS or another network filesystem.** SQLite's locking is unreliable there and the database can be corrupted. rackend logs a warning at startup when it detects one. Use a local disk or a block volume.
>
> **Do not run two instances on the same data directory.** The second one refuses to start (exit code 75).

The container runs as UID/GID `10001:10001`. Bind mounts must be writable by that user.

### File storage

Uploaded files go to `storage/` by default. To use an S3-compatible bucket instead (AWS S3, Garage, RustFS, MinIO, Cloudflare R2 and others), open **Settings** in the admin UI, choose S3-compatible, fill in the endpoint, bucket and keys, and use **Test connection** before saving. The secret key is encrypted at rest (see [Secrets at rest](#secrets-at-rest)). Path-style addressing is on by default because most self-hosted servers need it.

- Switching backends does not move existing files. rackend refuses the switch while files exist unless you confirm it (`PATCH /api/settings?force=true`); copy the files yourself first.
- Thumbnails are cached in the same backend under `_thumbs/`. They are made again on demand, so they need no backup; `storage.thumb_cache_max_mb` caps the cache.
- Removed files are deleted shortly after the change. A daily cleanup (also at start) removes blobs that no record refers to.

## Bootstrap environment variables

| Variable | Default | Meaning |
|---|---|---|
| `RACKEND_DATA_DIR` | `/data` (Docker) / `./rackend_data` | Data directory |
| `RACKEND_HTTP_ADDR` | `0.0.0.0:8080` | Listen address |
| `RACKEND_DEV` | `false` | Dev mode: pretty logs, permissive localhost handling. Never in production. |
| `RACKEND_LOG_LEVEL` | `info` | `tracing` filter, e.g. `info,rackend_store_sqlite=debug` |
| `RACKEND_LOG_FORMAT` | `json` (`pretty` in dev) | `json` or `pretty` |
| `RACKEND_SQLITE_SYNCHRONOUS` | `FULL` | `NORMAL` trades durability of the last commits on power loss for write speed (never corruption) |
| `RACKEND_PUBLIC_URL` | unset | Overrides the public URL stored in the settings |
| `RACKEND_SUPERUSER_EMAIL` / `RACKEND_SUPERUSER_PASSWORD` | unset | Create the first superuser without the setup screen (automation) |
| `RACKEND_ENCRYPTION_KEY` | unset | 32 bytes in base64 (`openssl rand -base64 32`). Encrypts secrets at rest; see below |
| `RACKEND_RESET_SECRETS` | `false` | Start even though the stored secrets cannot be decrypted, discarding them; see below |
| `RACKEND_TRUSTED_PROXIES` | unset | Comma-separated IPs or CIDRs (`10.0.0.0/8, ::1`) whose `X-Forwarded-For` is trusted. Overrides `security.trusted_proxies` |

| `RACKEND_OTLP_ENDPOINT` | unset | OpenTelemetry collector base URL (`http://otel-collector:4318`): traces and metrics are exported over OTLP/HTTP |

## Secrets at rest

SMTP and S3 passwords, client secrets of external providers and the signing keys are encrypted in the database. The key comes from `RACKEND_ENCRYPTION_KEY`, or, when it is not set, from a key rackend generates and stores in the database. The stored key only protects backups that you keep apart from the database, so set `RACKEND_ENCRYPTION_KEY` in production.

- **Adding the key later:** set `RACKEND_ENCRYPTION_KEY` and restart. rackend re-encrypts every secret with it and deletes the stored key (audited as `secrets.rekey`).
- **Keep the key safe.** Without it the secrets cannot be read and rackend refuses to start. Your collections and records are not affected.
- **Lost the key:** start once with `RACKEND_RESET_SECRETS=true` and a new key. rackend discards the secrets (audited as `secrets.reset`) and generates new signing keys; enter the SMTP, S3 and provider secrets again, then remove `RACKEND_RESET_SECRETS`.

## Behind a reverse proxy

Rate limits and audit entries use the client IP. Without trusted proxies that is the address of the TCP connection, which behind a proxy is the proxy itself. Set `RACKEND_TRUSTED_PROXIES` (or `security.trusted_proxies` in the settings) to the proxy's address or network; rackend then takes the right-most address in `X-Forwarded-For` that is not a trusted proxy. Never trust a network that clients can send from directly, or they can choose their own IP.

Realtime uses long-lived connections. The proxy must pass WebSocket upgrades on `/api/ws` and must not buffer `GET /api/realtime` (Server-Sent Events; rackend sends `X-Accel-Buffering: no` for nginx). Set read timeouts above 60 seconds: rackend sends a heartbeat every 25 seconds. Example for nginx:

```nginx
location /api/ws {
    proxy_pass http://rackend:8080;
    proxy_http_version 1.1;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection "upgrade";
    proxy_read_timeout 120s;
}
location / {
    proxy_pass http://rackend:8080;
    proxy_http_version 1.1;
    proxy_read_timeout 120s;
}
```

Each realtime connection holds a file descriptor. For many clients raise the limit (`docker run --ulimit nofile=65536:65536 ...`).

## First start

Without `RACKEND_SUPERUSER_EMAIL`/`_PASSWORD`, rackend logs a one-time link on every start until the first superuser exists:

```
Open http://localhost:8080/_/setup?token=… to create the first superuser.
```

The setup screen also asks for the public URL users reach the instance at.

## Email

Verification, password reset, email change, one-time codes and magic links need SMTP and a public URL (the links point at it). Set them in Settings → Email, or with `PATCH /api/settings`:

```json
{ "smtp": { "host": "smtp.example.com", "port": 587, "tls": "starttls", "username": "rackend", "password": "…", "from_name": "Acme", "from_address": "noreply@example.com" } }
```

- `tls`: `starttls` (port 587), `tls` (port 465) or `none` (a relay on the same host only). The password is stored encrypted.
- **Send test** in the admin UI (or `POST /api/settings/test-smtp {"to": "you@example.com"}`) checks the settings without saving them.
- Templates for every email are editable in the same place. `{{app_name}}`, `{{link}}`, `{{code}}` and `{{email}}` are replaced; values are escaped in the HTML body.
- Links in emails open pages served by rackend (`/auth/verify`, `/auth/reset`, `/auth/email-change`). If a reverse proxy only forwards `/api`, forward `/auth` too.

## Sign-in providers

Settings → Sign-in providers adds OpenID Connect providers (presets for Google, Microsoft Entra ID, Apple, GitLab, Authentik, Pocket ID, Keycloak, Zitadel, or any issuer) and GitHub.

1. Set the public URL first.
2. Register the redirect URI the dialog shows at the provider: `{public_url}/api/collections/{collection}/providers/{name}/callback`.
3. Add your app's return URL to **App redirect URIs** (exact match). Apps start a sign-in at `GET /api/collections/{c}/providers/{name}/start?redirect_uri=<that URL>`, receive `#code=…` (or `#error=…`) and exchange the code at `POST /api/collections/{c}/auth-with-exchange-code` within 60 seconds.
4. Leave **Link to existing accounts by verified email** off unless the provider verifies every address it reports.

Signed-in users add a provider to their account with `&link=true` on the start URL.

## API document

`GET /api/openapi.json` describes the API of your collections for client generators (see [Generating a client](guide/clients.md)). It is for superusers until you set `api.openapi_public: true`; the public document leaves out operations only superusers may call. `rackend openapi export` prints it from the command line, also while the server runs.

## Monitoring

- **Dashboard.** The admin UI shows requests, errors, latency, the busiest routes, recent server errors, database and file sizes, realtime connections, and CPU, memory and disk. Inside a container with memory or CPU limits it shows the container's limits, not the host's. It warns when the data directory's disk has less than 10% free.
- **Request logs.** Every request except health checks and admin UI assets goes to `logs.db`, kept for `logs.retention_days` (default 7). `logs.min_status` keeps only errors, `logs.sample_rate` samples successful requests. Losing `logs.db` loses no data.
- **Prometheus.** `GET /metrics` is off until you set `metrics.prometheus_token` in the settings (send it as `Authorization: Bearer <token>`) or `metrics.prometheus_public: true`.
- **OpenTelemetry.** Set `RACKEND_OTLP_ENDPOINT` to export traces (one span per request with database spans below it) and metrics over OTLP/HTTP.
- **Logs.** rackend logs JSON lines to stdout; every line inside a request carries its `request_id`, also returned as the `X-Request-Id` header.

## Backups

rackend writes backups to `backups/backup-<time>.tar.zst`: a consistent snapshot of `data.db`, the uploaded files in `storage/` (filesystem storage, thumbnails left out) and a manifest with a checksum for every file.

- **Scheduled.** Daily at 03:00 UTC by default (`backups.schedule`, a cron expression; empty turns it off). The newest `backups.keep` archives (default 7) are kept.
- **On demand.** Settings → Backups in the admin UI, `POST /api/backups`, or `rackend backup create` (works while the server runs).
- **Off site.** Copy the archives somewhere else, or download them from the admin UI. A backup on the same disk does not survive the disk.
- **S3 storage.** With S3 the archive holds the database only; turn on bucket versioning to keep older files.

To restore, stop the server and run:

```bash
rackend backup restore backup-20261117T030000Z.tar.zst
```

rackend checks every checksum first and changes nothing if the archive is damaged. The current `data.db` and `storage/` are moved to `backups/replaced-<time>/`, never deleted. An older database is migrated when the server starts.

`rackend check` reports database integrity problems and files whose blob is missing. It only reads and is safe next to a running server.

## Upgrades

Upgrading never loses data. On start, before any schema change, rackend:

1. refuses to run against a database written by a newer version (exit 78), because downgrades are not supported;
2. checks there is enough free space (2 × database size + 100 MB, else exit 73);
3. writes a verified snapshot to `backups/pre-upgrade-<from>-to-<to>-<timestamp>.db` (exit 74 if verification fails);
4. applies each migration in its own transaction (exit 70 on failure, data unchanged) and checks integrity afterwards.

The last 5 pre-upgrade backups are kept. To go back to an older version, stop rackend, replace `data.db` with the matching pre-upgrade backup, remove `data.db-wal` and `data.db-shm`, and start the older version.

`rackend migrate status` shows applied and pending migrations without changing anything; it is safe to run next to a live instance.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Normal shutdown |
| 1 | Invalid configuration or other startup failure |
| 70 | Migration or integrity failure |
| 73 | Not enough disk space for a safe upgrade |
| 74 | Pre-upgrade backup verification failed |
| 75 | Data directory locked by another instance |
| 78 | Database schema newer than this binary, or a migration checksum mismatch |

## Shutdown

On `SIGTERM`/`SIGINT` rackend stops accepting connections, waits up to 10 s for in-flight requests, finishes queued database writes, checkpoints the databases and exits 0. Give the container at least 30 s (`stop_grace_period`).
