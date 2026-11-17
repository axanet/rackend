//! Observability and backups (SPEC §14, §18.4): request logs, the metrics
//! sampler, dashboard data, retention, and scheduled backups.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use time::OffsetDateTime;
use tokio::sync::mpsc;

use super::audit::entry;
use super::{App, RequestCtx};
use crate::domain::cron::Cron;
use crate::ports::{
    BackupInfo, HostSample, MetricPoint, RequestLog, RequestLogQuery, StoreExt as _,
};
use crate::time::format_timestamp;
use crate::{Error, Result};

/// Buffered request rows before they are dropped (losing logs is fine).
const LOG_BUFFER: usize = 10_000;
/// How often buffered rows are written.
const LOG_FLUSH: Duration = Duration::from_secs(1);
/// Sampling interval of the built-in dashboard series.
const SAMPLE_EVERY: Duration = Duration::from_secs(60);
/// Below this share of free disk space the dashboard warns.
pub const DISK_WARN_FREE: f64 = 0.10;

/// In-process counters for the dashboard and `/metrics`.
#[derive(Debug, Default)]
pub struct Counters {
    /// Logins by (method, result).
    logins: Mutex<HashMap<(&'static str, &'static str), u64>>,
    /// Request rows dropped because the buffer was full.
    pub logs_dropped: AtomicU64,
    /// Requests seen (health checks and UI assets excluded).
    pub http_requests: AtomicU64,
    /// Responses with status ≥ 500.
    pub http_server_errors: AtomicU64,
}

impl Counters {
    /// Counts a login attempt.
    pub fn login(&self, method: &'static str, ok: bool) {
        let mut m = self
            .logins
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *m.entry((method, if ok { "success" } else { "failure" }))
            .or_default() += 1;
    }

    /// Login counts.
    pub fn logins(&self) -> Vec<(&'static str, &'static str, u64)> {
        let m = self
            .logins
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        m.iter().map(|((a, b), n)| (*a, *b, *n)).collect()
    }
}

/// Request-log and sampler state.
#[derive(Debug)]
pub struct ObserveState {
    tx: mpsc::Sender<RequestLog>,
    rx: Mutex<Option<mpsc::Receiver<RequestLog>>>,
    /// Process start, for uptime.
    pub started: Instant,
    /// Last scheduled backup minute, so a schedule fires once.
    last_backup: Mutex<Option<OffsetDateTime>>,
    /// Counters.
    pub counters: Counters,
}

impl Default for ObserveState {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel(LOG_BUFFER);
        Self {
            tx,
            rx: Mutex::new(Some(rx)),
            started: Instant::now(),
            last_backup: Mutex::new(None),
            counters: Counters::default(),
        }
    }
}

/// Paths that are never logged: health checks and admin UI assets.
fn excluded(path: &str) -> bool {
    path == "/api/health" || path == "/_" || path.starts_with("/_/") || path == "/favicon.ico"
}

/// A cheap deterministic 0..1 value from the request id, for sampling.
fn fraction(id: Option<&str>) -> f64 {
    let h = id
        .unwrap_or_default()
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        });
    (h % 10_000) as f64 / 10_000.0
}

fn minute(t: OffsetDateTime) -> OffsetDateTime {
    t.replace_second(0)
        .and_then(|t| t.replace_nanosecond(0))
        .unwrap_or(t)
}

impl App {
    /// Starts the log writer, the sampler and the backup scheduler.
    pub(crate) fn observe_start(self: &Arc<Self>) {
        let rx = self
            .observe
            .rx
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(mut rx) = rx {
            let me = Arc::downgrade(self);
            let sleep = Arc::clone(&self.spawner);
            self.spawner.spawn(
                "logs.writer",
                Box::pin(async move {
                    loop {
                        sleep.sleep(LOG_FLUSH).await;
                        let mut rows = Vec::new();
                        while let Ok(r) = rx.try_recv() {
                            rows.push(r);
                        }
                        let Some(app) = me.upgrade() else { return };
                        if !rows.is_empty()
                            && let Err(e) = app.logs.append_requests(rows).await
                        {
                            tracing::warn!(error = %e, "cannot write request logs");
                        }
                    }
                }),
            );
        }
        let me = Arc::downgrade(self);
        let sleep = Arc::clone(&self.spawner);
        self.spawner.spawn(
            "metrics.sampler",
            Box::pin(async move {
                loop {
                    sleep.sleep(SAMPLE_EVERY).await;
                    let Some(app) = me.upgrade() else { return };
                    if let Err(e) = app.sample_metrics().await {
                        tracing::warn!(error = %e, "metrics sampling failed");
                    }
                    if let Err(e) = app.run_scheduled_backup().await {
                        tracing::error!(error = %e, "scheduled backup failed");
                    }
                }
            }),
        );
    }

    /// Queues a request row (SPEC §14.2): health checks and UI assets are
    /// skipped, `logs.min_status` and `logs.sample_rate` (2xx only) apply.
    pub fn record_request(&self, row: RequestLog) {
        if excluded(&row.path) {
            return;
        }
        self.observe
            .counters
            .http_requests
            .fetch_add(1, Ordering::Relaxed);
        if row.status >= 500 {
            self.observe
                .counters
                .http_server_errors
                .fetch_add(1, Ordering::Relaxed);
        }
        let s = self.settings();
        let min = u16::try_from(s.int("logs", "min_status", 0)).unwrap_or(0);
        if row.status < min {
            return;
        }
        if (200..300).contains(&row.status) {
            let rate = s
                .section("logs")
                .and_then(|l| l.get("sample_rate"))
                .and_then(Value::as_f64)
                .unwrap_or(1.0);
            if rate < 1.0 && fraction(row.request_id.as_deref()) >= rate {
                return;
            }
        }
        if self.observe.tx.try_send(row).is_err() {
            self.observe
                .counters
                .logs_dropped
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Writes buffered request rows now (tests, shutdown).
    pub async fn flush_request_logs(&self) -> Result<()> {
        // The writer drains on its own; in tests no writer may run yet, so
        // drain here when the receiver is still available.
        let rows: Vec<RequestLog> = {
            let mut guard = self
                .observe
                .rx
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match guard.as_mut() {
                Some(rx) => std::iter::from_fn(|| rx.try_recv().ok()).collect(),
                None => Vec::new(),
            }
        };
        if !rows.is_empty() {
            self.logs.append_requests(rows).await?;
        }
        Ok(())
    }

    /// The last scheduled-backup minute (tests reset it).
    #[doc(hidden)]
    pub fn observe_last_backup_for_tests(
        &self,
    ) -> std::sync::MutexGuard<'_, Option<OffsetDateTime>> {
        self.observe
            .last_backup
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Counters for logins etc.
    pub fn counters(&self) -> &Counters {
        &self.observe.counters
    }

    /// A host sample (CPU since the previous sample).
    pub fn host_sample(&self) -> HostSample {
        self.host.sample()
    }

    /// Samples the dashboard series once (every 60 s).
    pub async fn sample_metrics(&self) -> Result<Vec<MetricPoint>> {
        let now = self.clock.now();
        let ts = format_timestamp(minute(now));
        let since = format_timestamp(now - time::Duration::seconds(60));
        let stats = self.logs.request_stats(since, 0).await?;
        let host = self.host.sample();
        let store = self.store.stats();
        let (logs_db, logs_wal) = self.logs.size();
        let (files, file_bytes) = self
            .store
            .read(|tx| tx.files_usage())
            .await
            .unwrap_or((0, 0));
        let errors = stats.server_errors + stats.client_errors;
        let p = |series: &str, value: f64| MetricPoint {
            ts: ts.clone(),
            series: series.to_owned(),
            value,
        };
        let points = vec![
            p("http.rps", stats.count as f64 / 60.0),
            p(
                "http.error_rate",
                if stats.count == 0 {
                    0.0
                } else {
                    errors as f64 / stats.count as f64
                },
            ),
            p("http.server_errors", stats.server_errors as f64),
            p("http.p50_ms", stats.p50_ms),
            p("http.p95_ms", stats.p95_ms),
            p("http.p99_ms", stats.p99_ms),
            p("db.write_queue", store.write_queue_depth as f64),
            p("db.size", (store.db_size + store.wal_size) as f64),
            p("logs.size", (logs_db + logs_wal) as f64),
            p("realtime.connections", self.hub.connections() as f64),
            p("host.cpu_percent", host.cpu_percent),
            p("host.memory_used", host.memory_used as f64),
            p("host.memory_total", host.memory_total as f64),
            p("host.disk_free", host.disk_free as f64),
            p("host.disk_total", host.disk_total as f64),
            p("host.net_rx", host.net_rx as f64),
            p("host.net_tx", host.net_tx as f64),
            p("process.rss", host.process_rss as f64),
            p("storage.files", files as f64),
            p("storage.bytes", file_bytes as f64),
        ];
        if host.disk_total > 0 && (host.disk_free as f64) < host.disk_total as f64 * DISK_WARN_FREE
        {
            tracing::warn!(
                free = host.disk_free,
                total = host.disk_total,
                "less than 10% disk space left on the data directory's filesystem"
            );
        }
        self.logs.put_points(points.clone()).await?;
        Ok(points)
    }

    /// `GET /api/metrics/summary`: everything the dashboard shows now.
    pub async fn metrics_summary(&self, ctx: &RequestCtx) -> Result<Value> {
        ctx.require_superuser()?;
        let now = self.clock.now();
        let minute_stats = self
            .logs
            .request_stats(format_timestamp(now - time::Duration::seconds(60)), 0)
            .await?;
        let hour_stats = self
            .logs
            .request_stats(format_timestamp(now - time::Duration::hours(1)), 8)
            .await?;
        let (recent_errors, _) = self
            .logs
            .query_requests(RequestLogQuery {
                min_status: Some(500),
                limit: 10,
                ..RequestLogQuery::default()
            })
            .await?;
        let host = self.host.sample();
        let store = self.store.stats();
        let (logs_db, logs_wal) = self.logs.size();
        let (files, file_bytes) = self.store.read(|tx| tx.files_usage()).await?;
        let disk_low = host.disk_total > 0
            && (host.disk_free as f64) < host.disk_total as f64 * DISK_WARN_FREE;
        let errors = minute_stats.server_errors + minute_stats.client_errors;
        Ok(json!({
            "version": self.version,
            "uptime_s": self.observe.started.elapsed().as_secs(),
            "requests": {
                "per_second": minute_stats.count as f64 / 60.0,
                "last_minute": minute_stats.count,
                "error_rate": if minute_stats.count == 0 { 0.0 } else { errors as f64 / minute_stats.count as f64 },
                "server_errors_last_minute": minute_stats.server_errors,
                "last_hour": hour_stats.count,
                "latency_ms": { "p50": hour_stats.p50_ms, "p95": hour_stats.p95_ms, "p99": hour_stats.p99_ms },
                "top_routes": hour_stats.top_routes,
                "recent_errors": recent_errors,
            },
            "db": {
                "write_queue": store.write_queue_depth,
                "data_bytes": store.db_size + store.wal_size,
                "wal_bytes": store.wal_size,
                "logs_bytes": logs_db + logs_wal,
            },
            "realtime": { "connections": self.hub.connections(), "events": self.hub.dispatched.load(Ordering::Relaxed) },
            "storage": { "files": files, "bytes": file_bytes, "backend": self.blobs().name() },
            "host": host,
            "disk_low": disk_low,
            "logs_dropped": self.observe.counters.logs_dropped.load(Ordering::Relaxed),
        }))
    }

    /// `GET /api/metrics/series?series=a,b&from=&to=`.
    pub async fn metrics_series(
        &self,
        ctx: &RequestCtx,
        series: Vec<String>,
        from: Option<String>,
        to: Option<String>,
    ) -> Result<Value> {
        ctx.require_superuser()?;
        if series.is_empty() || series.len() > 20 {
            return Err(Error::BadRequest(
                "Ask for 1 to 20 series, e.g. ?series=http.rps,host.cpu_percent.".into(),
            ));
        }
        let now = self.clock.now();
        let from = from.unwrap_or_else(|| format_timestamp(now - time::Duration::hours(1)));
        let to = to.unwrap_or_else(|| format_timestamp(now + time::Duration::minutes(1)));
        let points = self
            .logs
            .series(series.clone(), from.clone(), to.clone())
            .await?;
        let mut by: HashMap<String, Vec<Value>> =
            series.iter().map(|s| (s.clone(), Vec::new())).collect();
        for p in points {
            by.entry(p.series).or_default().push(json!([p.ts, p.value]));
        }
        Ok(json!({ "from": from, "to": to, "series": by }))
    }

    /// `GET /api/logs`: request logs, newest first.
    pub async fn request_logs(
        &self,
        ctx: &RequestCtx,
        q: RequestLogQuery,
    ) -> Result<(Vec<RequestLog>, u64)> {
        ctx.require_superuser()?;
        self.logs.query_requests(q).await
    }

    /// Deletes request logs and metric points past their retention.
    pub async fn job_logs_retention(&self) -> Result<(u64, u64)> {
        let s = self.settings();
        let now = self.clock.now();
        let days = |k: &str, d: i64| s.int(k, "retention_days", d).max(1);
        let req = format_timestamp(now - time::Duration::days(days("logs", 7)));
        let pts = format_timestamp(now - time::Duration::days(days("metrics", 7)));
        self.logs.prune(req, pts).await
    }

    /// Whether a `/metrics` scrape is allowed: `metrics.prometheus_public`,
    /// or a bearer token equal to `metrics.prometheus_token`.
    pub fn prometheus_allowed(&self, bearer: Option<&str>) -> bool {
        let s = self.settings();
        let public = s
            .section("metrics")
            .and_then(|m| m.get("prometheus_public"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if public {
            return true;
        }
        match (
            bearer,
            self.secret_setting("metrics", &["prometheus_token"]),
        ) {
            (Some(b), Some(t)) if !t.is_empty() => crate::auth::token::ct_eq(b, &t),
            _ => false,
        }
    }

    /// `GET /api/backups`.
    pub async fn backups_list(&self, ctx: &RequestCtx) -> Result<Vec<BackupInfo>> {
        ctx.require_superuser()?;
        self.backups.list().await
    }

    fn include_storage(&self) -> bool {
        let s = self.settings();
        let include = s
            .section("backups")
            .and_then(|b| b.get("include_storage"))
            .and_then(Value::as_bool)
            .unwrap_or(true);
        include && self.blobs().name() == "fs"
    }

    /// `POST /api/backups`: a backup now (audited).
    pub async fn backup_create(&self, ctx: &RequestCtx) -> Result<BackupInfo> {
        ctx.require_superuser()?;
        let info = self.backups.create(self.include_storage()).await?;
        self.audit_backup(ctx.actor(), ctx.now, &info).await?;
        Ok(info)
    }

    /// Deletes a backup archive (audited).
    pub async fn backup_delete(&self, ctx: &RequestCtx, name: &str) -> Result<()> {
        ctx.require_superuser()?;
        if self.backups.path(name).is_none() {
            return Err(Error::NotFound);
        }
        self.backups.delete(name.to_owned()).await?;
        let (actor, now, name) = (ctx.actor(), ctx.now, name.to_owned());
        self.store
            .write(move |tx| {
                let mut e = entry(&actor, "backup.delete", now);
                e.meta = Some(json!({ "name": name }));
                tx.audit_insert(&e)
            })
            .await
    }

    /// The file of a backup for download.
    pub fn backup_path(&self, ctx: &RequestCtx, name: &str) -> Result<std::path::PathBuf> {
        ctx.require_superuser()?;
        self.backups.path(name).ok_or(Error::NotFound)
    }

    async fn audit_backup(
        &self,
        actor: crate::domain::Actor,
        now: OffsetDateTime,
        info: &BackupInfo,
    ) -> Result<()> {
        let meta = json!({ "name": info.name, "size": info.size, "includes_storage": info.includes_storage });
        self.store
            .write(move |tx| {
                let mut e = entry(&actor, "backup.create", now);
                e.meta = Some(meta);
                tx.audit_insert(&e)
            })
            .await
    }

    /// Runs the `backups.schedule` backup when its minute is due, then keeps
    /// the newest `backups.keep` archives.
    pub async fn run_scheduled_backup(&self) -> Result<Option<BackupInfo>> {
        let s = self.settings();
        let Some(schedule) = s
            .string("backups", "schedule")
            .filter(|x| !x.trim().is_empty())
        else {
            return Ok(None);
        };
        let Some(cron) = Cron::parse(&schedule) else {
            return Ok(None);
        };
        let now = minute(self.clock.now());
        {
            let mut last = self
                .observe
                .last_backup
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // The sampler wakes every 60 s; catch a minute it slept through.
            let window = [now, now - time::Duration::minutes(1)];
            let Some(due) = window.into_iter().find(|t| cron.matches(*t)) else {
                return Ok(None);
            };
            if last.is_some_and(|l| l >= due) {
                return Ok(None);
            }
            *last = Some(due);
        }
        let info = self.backups.create(self.include_storage()).await?;
        self.audit_backup(crate::domain::Actor::system(), self.clock.now(), &info)
            .await?;
        let keep = usize::try_from(s.int("backups", "keep", 7).max(1)).unwrap_or(7);
        let all = self.backups.list().await?;
        for old in all.into_iter().skip(keep) {
            if let Err(e) = self.backups.delete(old.name.clone()).await {
                tracing::warn!(error = %e, name = %old.name, "cannot delete an old backup");
            }
        }
        tracing::info!(name = %info.name, size = info.size, "scheduled backup written");
        Ok(Some(info))
    }
}
