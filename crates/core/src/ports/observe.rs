//! Observability and backup ports (SPEC §14, §18.4): the logs database,
//! host metrics and backup archives.

use super::BoxFuture;
use crate::Result;

/// One request log row (`logs.db._requests`, SPEC §14.2).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct RequestLog {
    /// Row id (0 before insert).
    pub id: i64,
    /// RFC 3339.
    pub ts: String,
    /// HTTP method.
    pub method: String,
    /// Matched route template (`/api/collections/{c}/records`).
    pub route: Option<String>,
    /// Raw path (no query string).
    pub path: String,
    /// Status code.
    pub status: u16,
    /// Duration in milliseconds.
    pub duration_ms: f64,
    /// Client IP.
    pub ip: Option<String>,
    /// User agent.
    pub user_agent: Option<String>,
    /// `superuser`, `user`, `anonymous`.
    pub actor_type: Option<String>,
    /// Actor id.
    pub actor_id: Option<String>,
    /// Request id.
    pub request_id: Option<String>,
    /// Request body bytes (from `Content-Length`).
    pub bytes_in: Option<u64>,
    /// Response body bytes, when known.
    pub bytes_out: Option<u64>,
    /// Problem code of error responses.
    pub error: Option<String>,
}

/// Filters for the log viewer.
#[derive(Debug, Clone, Default)]
pub struct RequestLogQuery {
    /// Exact method.
    pub method: Option<String>,
    /// Route template or path prefix.
    pub path: Option<String>,
    /// Lowest status (e.g. 400 for errors).
    pub min_status: Option<u16>,
    /// Highest status.
    pub max_status: Option<u16>,
    /// Actor id.
    pub actor_id: Option<String>,
    /// Request id.
    pub request_id: Option<String>,
    /// From (RFC 3339, inclusive).
    pub from: Option<String>,
    /// To (RFC 3339, exclusive).
    pub to: Option<String>,
    /// Page size.
    pub limit: u32,
    /// Offset.
    pub offset: u64,
}

/// Aggregates over a window of request logs.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct RequestStats {
    /// Requests.
    pub count: u64,
    /// Responses with status ≥ 500.
    pub server_errors: u64,
    /// Responses with status 400..500.
    pub client_errors: u64,
    /// Median duration (ms).
    pub p50_ms: f64,
    /// 95th percentile (ms).
    pub p95_ms: f64,
    /// 99th percentile (ms).
    pub p99_ms: f64,
    /// Busiest routes: (method, route, count, p95 ms).
    pub top_routes: Vec<RouteStat>,
}

/// One busy route.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct RouteStat {
    /// Method.
    pub method: String,
    /// Route template (or path).
    pub route: String,
    /// Requests.
    pub count: u64,
    /// 95th percentile (ms).
    pub p95_ms: f64,
    /// Error responses (≥ 400).
    pub errors: u64,
}

/// A point of a time series (`logs.db._metric_points`).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct MetricPoint {
    /// RFC 3339, truncated to the sampling minute.
    pub ts: String,
    /// Series name (`http.rps`).
    pub series: String,
    /// Value.
    pub value: f64,
}

/// The logs database (SPEC §6.6). Losing data here is acceptable; writes
/// are batched and never block requests.
pub trait LogsStore: Send + Sync + std::fmt::Debug {
    /// Inserts request rows in one transaction.
    fn append_requests(&self, rows: Vec<RequestLog>) -> BoxFuture<'_, Result<()>>;
    /// Filtered page of request rows, newest first, and the total.
    fn query_requests(&self, q: RequestLogQuery) -> BoxFuture<'_, Result<(Vec<RequestLog>, u64)>>;
    /// Aggregates since `since` (RFC 3339).
    fn request_stats(&self, since: String, top: u32) -> BoxFuture<'_, Result<RequestStats>>;
    /// Stores metric points (replacing same series and minute).
    fn put_points(&self, points: Vec<MetricPoint>) -> BoxFuture<'_, Result<()>>;
    /// Points of `series` in `[from, to)`, oldest first.
    fn series(
        &self,
        series: Vec<String>,
        from: String,
        to: String,
    ) -> BoxFuture<'_, Result<Vec<MetricPoint>>>;
    /// Deletes requests and points older than the cutoffs.
    fn prune(
        &self,
        requests_before: String,
        points_before: String,
    ) -> BoxFuture<'_, Result<(u64, u64)>>;
    /// Size of logs.db and its WAL in bytes.
    fn size(&self) -> (u64, u64);
}

/// Host and process numbers at one moment (SPEC §14.3). Inside a container
/// with cgroup v2 limits, memory and CPU describe the container.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct HostSample {
    /// Running in a container.
    pub container: bool,
    /// CPU busy, percent of the available CPUs (container quota if any).
    pub cpu_percent: f64,
    /// CPUs available (fractional with a CPU quota).
    pub cpus: f64,
    /// Load averages (1, 5, 15 min); zero where unsupported.
    pub load_avg: [f64; 3],
    /// Memory in use (container usage inside a container).
    pub memory_used: u64,
    /// Memory available in total (the container limit when set).
    pub memory_total: u64,
    /// Host RAM, for reference.
    pub host_memory_total: u64,
    /// Filesystem of the data directory: total bytes.
    pub disk_total: u64,
    /// Filesystem of the data directory: free bytes.
    pub disk_free: u64,
    /// Network bytes received since start.
    pub net_rx: u64,
    /// Network bytes sent since start.
    pub net_tx: u64,
    /// Process resident memory.
    pub process_rss: u64,
    /// Process CPU time (seconds).
    pub process_cpu_seconds: f64,
    /// Open file descriptors (Linux).
    pub open_fds: Option<u64>,
}

/// Reads host metrics.
pub trait HostMetrics: Send + Sync + std::fmt::Debug {
    /// A fresh sample. CPU usage is measured since the previous call.
    fn sample(&self) -> HostSample;
}

/// A backup archive.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BackupInfo {
    /// File name (`backup-20261117T030000Z.tar.zst`).
    pub name: String,
    /// Bytes.
    pub size: u64,
    /// RFC 3339.
    pub created: String,
    /// Whether `storage/` is included.
    pub includes_storage: bool,
}

/// Creates and lists backups (SPEC §18.4). Restores are CLI only.
pub trait Backups: Send + Sync + std::fmt::Debug {
    /// Writes a new archive: a snapshot of `data.db`, and `storage/` when
    /// `include_storage` (filesystem backend, thumbnails excluded).
    fn create(&self, include_storage: bool) -> BoxFuture<'_, Result<BackupInfo>>;
    /// Archives in `backups/`, newest first.
    fn list(&self) -> BoxFuture<'_, Result<Vec<BackupInfo>>>;
    /// Deletes an archive.
    fn delete(&self, name: String) -> BoxFuture<'_, Result<()>>;
    /// The path of an archive for download, if it exists.
    fn path(&self, name: &str) -> Option<std::path::PathBuf>;
}
