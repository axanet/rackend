# 0014. M5 observability and backup details not fixed by the spec

- Status: accepted
- Date: 2026-11-17
- Spec: §14, §18.4, §20 M5

## Context

M5 adds request logs, metrics, the dashboard, Prometheus and OTLP export, host and container metrics, and backups with restore and `rackend check`. This record covers the choices the spec leaves open.

## Decision

### Ports

- New core ports: `LogsStore` (request rows, metric points, aggregates, retention), `HostMetrics` and `Backups`. `Store` gains `stats()` (writer queue depth, file sizes).
- The core does not depend on the `metrics` facade. The HTTP adapter records HTTP metrics; the other Prometheus series are gauges set from the app's state when `/metrics` is scraped.

### Request logs

- An access-log layer wraps the whole stack, so it sees the final status, the request id, the matched route template and the problem code. Rows go into a bounded buffer (10,000) and are written once a second in one transaction; a full buffer drops rows and counts them (`rackend_request_logs_dropped_total`).
- `logs.sample_rate` samples 2xx responses deterministically by request id.
- `GET /api/logs` filters by method, route or path prefix, status range, actor, request id and time.

### Dashboard

- `GET /api/metrics/summary`: requests per second and error rate over the last minute, latency percentiles and busiest routes over the last hour, recent server errors, database and storage sizes, realtime connections, host numbers and a `disk_low` flag below 10% free.
- The sampler writes 20 series every 60 s to `_metric_points` (`http.rps`, `http.p95_ms`, `host.cpu_percent`, `host.memory_used`, …); `GET /api/metrics/series` reads them (default: the last hour).
- Percentiles are computed in SQL over the logged requests (nearest rank), so they follow the log sampling settings.
- The dashboard charts are plain SVG, one series per chart, keyboard explorable, with a hidden data table. The vgpu/3D direction (Dashboard Vision) stays a later step.

### Host and container metrics

- `sysinfo` for host numbers, opened lazily on the first sample. With cgroup v2 limits, memory (`memory.max`, `memory.current`) and CPU (`cpu.max`, `cpu.stat`) describe the container; `host_memory_total` keeps the host's RAM for reference. Verified with `docker run --memory=256m --cpus=1.5`.

### Prometheus and OTLP

- `/metrics` answers 401 until `metrics.prometheus_token` is set (a secret setting, compared in constant time) or `metrics.prometheus_public` is true.
- OTLP uses HTTP/protobuf (`{endpoint}/v1/traces`, `{endpoint}/v1/metrics`), behind the default-on cargo feature `otlp`. Spans: `http.request` with `db.read`/`db.write` below. Metrics: the dashboard numbers as observable instruments, exported every 60 s. `RACKEND_OTLP_ENDPOINT` must be an http(s) URL.

### Audit

- `GET /api/audit?format=ndjson` streams all matching entries, 1,000 per page.
- The admin UI gets a timeline with filters and diffs.

### Backups

- Archives: `backups/backup-<ts>.tar.zst` with `data.db`, `storage/` (filesystem backend, `_thumbs` left out) and `manifest.json` last. A copy of the manifest sits next to the archive (`.json`) so listing needs no decompression.
- The database snapshot is `VACUUM INTO` on the writer when the server runs (writes wait for it), or on a read-only connection from the CLI.
- `backups.schedule` is a five-field cron expression in UTC (validated); the sampler loop checks it every minute and runs a due minute once. After each scheduled backup only the newest `backups.keep` archives stay. An empty schedule turns it off.
- `POST /api/backups`, `GET /api/backups`, `GET/DELETE /api/backups/{name}`; creating and deleting are audited (`backup.create`, `backup.delete`).
- Restore (`rackend backup restore`) takes the data-directory lock, so it refuses to run next to a server (exit 75). It verifies every checksum before moving anything, then moves `data.db` (and WAL files) and `storage/` to `backups/replaced-<ts>/`.
- `rackend check` opens the database read only: `integrity_check`, `foreign_key_check`, files without blobs (a failure) and blobs without files (reported; the GC removes them).

### Dependencies

- New: `metrics`, `metrics-exporter-prometheus`, `sysinfo`, `tar`, `zstd`, `opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp`, `tracing-opentelemetry` (all approved in SPEC §3.3). zstd adds BSD-2-Clause to the license allowlist.
- The Docker image grows to 31.4 MB uncompressed (budget: 40 MB compressed).

## Consequences

- Backups of large databases pause writes while `VACUUM INTO` runs. Online backup through the SQLite backup API is a possible improvement.
- Percentiles from sampled logs are approximate when `logs.sample_rate` < 1.
