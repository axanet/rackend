//! Dashboard data, request logs, backups and Prometheus (SPEC §14, §18.4).

use std::collections::BTreeMap;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::header::{AUTHORIZATION, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use rackend_core::ports::{RequestLogQuery, StoreExt as _};
use serde_json::{Value, json};

use super::{ApiResult, created, no_content, ok};
use crate::AppState;
use crate::ctx::Ctx;
use crate::problem::{ApiError, ProblemCode};

fn param<T: std::str::FromStr>(q: &BTreeMap<String, String>, k: &str) -> Option<T> {
    q.get(k).and_then(|v| v.parse().ok())
}

/// `GET /api/metrics/summary`.
pub async fn summary(State(state): State<AppState>, Ctx(ctx): Ctx) -> ApiResult {
    Ok(ok(state.app().metrics_summary(&ctx).await?))
}

/// `GET /api/metrics/series?series=a,b&from=&to=`.
pub async fn series(State(state): State<AppState>, Ctx(ctx): Ctx) -> ApiResult {
    let names: Vec<String> = ctx
        .query
        .get("series")
        .map(|s| {
            s.split(',')
                .map(|x| x.trim().to_owned())
                .filter(|x| !x.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let (from, to) = (ctx.query.get("from").cloned(), ctx.query.get("to").cloned());
    Ok(ok(state
        .app()
        .metrics_series(&ctx, names, from, to)
        .await?))
}

/// `GET /api/logs?method=&path=&min_status=&max_status=&actor_id=&request_id=&from=&to=&page=&per_page=`.
pub async fn logs(State(state): State<AppState>, Ctx(ctx): Ctx) -> ApiResult {
    let q = &ctx.query;
    let per_page: u32 = param(q, "per_page").unwrap_or(50).clamp(1, 500);
    let page: u32 = param(q, "page").unwrap_or(1).max(1);
    let query = RequestLogQuery {
        method: q.get("method").cloned().filter(|s| !s.is_empty()),
        path: q.get("path").cloned().filter(|s| !s.is_empty()),
        min_status: param(q, "min_status"),
        max_status: param(q, "max_status"),
        actor_id: q.get("actor_id").cloned().filter(|s| !s.is_empty()),
        request_id: q.get("request_id").cloned().filter(|s| !s.is_empty()),
        from: q.get("from").cloned(),
        to: q.get("to").cloned(),
        limit: per_page,
        offset: u64::from(page - 1) * u64::from(per_page),
    };
    let (items, total) = state.app().request_logs(&ctx, query).await?;
    Ok(ok(json!({
        "page": page, "per_page": per_page, "total_items": total,
        "total_pages": total.div_ceil(u64::from(per_page)), "items": items,
    })))
}

/// `GET /api/backups`.
pub async fn backups(State(state): State<AppState>, Ctx(ctx): Ctx) -> ApiResult {
    Ok(ok(
        json!({ "items": state.app().backups_list(&ctx).await? }),
    ))
}

/// `POST /api/backups`: a backup now.
pub async fn backup_create(State(state): State<AppState>, Ctx(ctx): Ctx) -> ApiResult {
    let info = state.app().backup_create(&ctx).await?;
    Ok(created(json!(info)))
}

/// `DELETE /api/backups/{name}`.
pub async fn backup_delete(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Path(name): Path<String>,
) -> ApiResult {
    state.app().backup_delete(&ctx, &name).await?;
    Ok(no_content())
}

/// `GET /api/backups/{name}`: download.
pub async fn backup_download(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Path(name): Path<String>,
) -> ApiResult {
    let path = state.app().backup_path(&ctx, &name)?;
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| ApiError::new(ProblemCode::NotFound))?;
    let len = file.metadata().await.map(|m| m.len()).ok();
    let stream = futures_util::stream::unfold(file, |mut f| async move {
        use tokio::io::AsyncReadExt as _;
        let mut buf = vec![0u8; 64 * 1024];
        match f.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok::<_, std::io::Error>(bytes::Bytes::from(buf)), f))
            }
            Err(e) => Some((Err(e), f)),
        }
    });
    let mut res = Response::new(Body::from_stream(stream));
    let h = res.headers_mut();
    h.insert(CONTENT_TYPE, HeaderValue::from_static("application/zstd"));
    if let Some(len) = len {
        h.insert(CONTENT_LENGTH, HeaderValue::from(len));
    }
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{name}\"")) {
        h.insert(CONTENT_DISPOSITION, v);
    }
    Ok(res)
}

/// `GET /metrics`: Prometheus text format. Disabled until
/// `metrics.prometheus_token` is set (bearer) or `metrics.prometheus_public`.
pub async fn prometheus(State(state): State<AppState>, headers: HeaderMap) -> ApiResult {
    let bearer = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.strip_prefix("Bearer ")
                .or_else(|| v.strip_prefix("bearer "))
        })
        .map(str::trim);
    if !state.app().prometheus_allowed(bearer) {
        return Err(ApiError::with_detail(
            ProblemCode::Unauthorized,
            "Set metrics.prometheus_token and send it as a bearer token, or set metrics.prometheus_public.",
        ));
    }
    let Some(handle) = state.prometheus() else {
        return Err(ApiError::with_detail(
            ProblemCode::NotFound,
            "Prometheus metrics are not enabled in this build.",
        ));
    };
    update_gauges(&state).await;
    let mut res = (StatusCode::OK, handle.render()).into_response();
    res.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
    );
    Ok(res)
}

/// Sets the gauges that are read on demand (SPEC §14.3).
async fn update_gauges(state: &AppState) {
    use metrics::{counter, gauge};
    let app = state.app();
    let host = app.host_sample();
    let store = app.store.stats();
    let (logs_db, logs_wal) = app.logs.size();
    gauge!("rackend_db_write_queue_depth").set(store.write_queue_depth as f64);
    gauge!("rackend_db_size_bytes", "db" => "data").set(store.db_size as f64);
    gauge!("rackend_db_wal_size_bytes", "db" => "data").set(store.wal_size as f64);
    gauge!("rackend_db_size_bytes", "db" => "logs").set(logs_db as f64);
    gauge!("rackend_db_wal_size_bytes", "db" => "logs").set(logs_wal as f64);
    gauge!("rackend_realtime_connections").set(app.hub.connections() as f64);
    counter!("rackend_realtime_events_total").absolute(
        app.hub
            .dispatched
            .load(std::sync::atomic::Ordering::Relaxed),
    );
    counter!("rackend_thumbnails_generated_total").absolute(app.thumb_generations());
    counter!("rackend_request_logs_dropped_total").absolute(
        app.counters()
            .logs_dropped
            .load(std::sync::atomic::Ordering::Relaxed),
    );
    for (method, result, n) in app.counters().logins() {
        counter!("rackend_auth_logins_total", "method" => method, "result" => result).absolute(n);
    }
    if let Ok((files, bytes)) = app.store.read(|tx| tx.files_usage()).await {
        gauge!("rackend_storage_bytes").set(bytes as f64);
        gauge!("rackend_storage_files").set(files as f64);
    }
    gauge!("rackend_process_resident_memory_bytes").set(host.process_rss as f64);
    counter!("rackend_process_cpu_seconds_total").absolute(host.process_cpu_seconds as u64);
    if let Some(fds) = host.open_fds {
        gauge!("rackend_process_open_fds").set(fds as f64);
    }
    gauge!("rackend_host_cpu_usage_percent").set(host.cpu_percent);
    gauge!("rackend_host_cpus").set(host.cpus);
    gauge!("rackend_host_load1").set(host.load_avg[0]);
    gauge!("rackend_host_memory_used_bytes").set(host.memory_used as f64);
    gauge!("rackend_host_memory_total_bytes").set(host.memory_total as f64);
    gauge!("rackend_host_disk_total_bytes").set(host.disk_total as f64);
    gauge!("rackend_host_disk_free_bytes").set(host.disk_free as f64);
    gauge!("rackend_host_network_received_bytes").set(host.net_rx as f64);
    gauge!("rackend_host_network_transmitted_bytes").set(host.net_tx as f64);
    gauge!("rackend_container").set(if host.container { 1.0 } else { 0.0 });
    let _ = Value::Null;
}
