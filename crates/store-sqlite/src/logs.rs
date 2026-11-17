//! The `LogsStore` port on `logs.db` (SPEC §6.6, §14.2): request rows and
//! metric points. Writes are batched by the caller.

use rackend_core::ports::{
    BoxFuture, LogsStore, MetricPoint, RequestLog, RequestLogQuery, RequestStats, RouteStat,
};
use rackend_core::{Error as CoreError, Result as CoreResult};
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, Row, params, params_from_iter};

use crate::db::SqliteDb;
use crate::open::LogsDb;

fn err(e: impl std::fmt::Display) -> CoreError {
    CoreError::internal(format!("logs.db: {e}"))
}

/// Request logs and metric points in `logs.db`.
#[derive(Debug, Clone)]
pub struct SqliteLogs {
    db: SqliteDb,
}

impl SqliteLogs {
    /// Wraps an opened `logs.db`.
    pub fn new(logs: &LogsDb) -> Self {
        Self {
            db: logs.db().clone(),
        }
    }
}

const COLS: &str = "id, ts, method, route, path, status, duration_ms, ip, user_agent, actor_type, actor_id, request_id, bytes_in, bytes_out, error";

fn row(r: &Row<'_>) -> rusqlite::Result<RequestLog> {
    Ok(RequestLog {
        id: r.get(0)?,
        ts: r.get(1)?,
        method: r.get(2)?,
        route: r.get(3)?,
        path: r.get(4)?,
        status: u16::try_from(r.get::<_, i64>(5)?).unwrap_or(0),
        duration_ms: r.get(6)?,
        ip: r.get(7)?,
        user_agent: r.get(8)?,
        actor_type: r.get(9)?,
        actor_id: r.get(10)?,
        request_id: r.get(11)?,
        bytes_in: r
            .get::<_, Option<i64>>(12)?
            .and_then(|n| u64::try_from(n).ok()),
        bytes_out: r
            .get::<_, Option<i64>>(13)?
            .and_then(|n| u64::try_from(n).ok()),
        error: r.get(14)?,
    })
}

/// The `q`-quantile of the durations matching `cond` (nearest rank).
fn quantile(
    conn: &Connection,
    cond: &str,
    args: &[SqlValue],
    count: u64,
    q: f64,
) -> rusqlite::Result<f64> {
    if count == 0 {
        return Ok(0.0);
    }
    let offset = ((count as f64 * q).ceil() as u64).clamp(1, count) - 1;
    let mut vals = args.to_vec();
    vals.push(SqlValue::Integer(i64::try_from(offset).unwrap_or(0)));
    let n = vals.len();
    conn.query_row(
        &format!("SELECT duration_ms FROM _requests WHERE {cond} ORDER BY duration_ms LIMIT 1 OFFSET ?{n}"),
        params_from_iter(vals.iter()),
        |r| r.get(0),
    )
}

fn stats(conn: &Connection, since: &str, top: u32) -> rusqlite::Result<RequestStats> {
    let args = [SqlValue::Text(since.to_owned())];
    let (count, server, client): (i64, i64, i64) = conn.query_row(
        "SELECT count(*), coalesce(sum(status >= 500), 0), coalesce(sum(status >= 400 AND status < 500), 0)
         FROM _requests WHERE ts >= ?1",
        params![since],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let count = u64::try_from(count).unwrap_or(0);
    let cond = "ts >= ?1";
    let mut out = RequestStats {
        count,
        server_errors: u64::try_from(server).unwrap_or(0),
        client_errors: u64::try_from(client).unwrap_or(0),
        p50_ms: quantile(conn, cond, &args, count, 0.50)?,
        p95_ms: quantile(conn, cond, &args, count, 0.95)?,
        p99_ms: quantile(conn, cond, &args, count, 0.99)?,
        top_routes: Vec::new(),
    };
    if top > 0 {
        let mut stmt = conn.prepare_cached(
            "SELECT method, coalesce(route, path) AS r, count(*) AS n, coalesce(sum(status >= 400), 0)
             FROM _requests WHERE ts >= ?1 GROUP BY method, r ORDER BY n DESC LIMIT ?2",
        )?;
        let routes: Vec<(String, String, i64, i64)> = stmt
            .query_map(params![since, top], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        for (method, route, n, errors) in routes {
            let n = u64::try_from(n).unwrap_or(0);
            let rargs = [
                SqlValue::Text(since.to_owned()),
                SqlValue::Text(method.clone()),
                SqlValue::Text(route.clone()),
            ];
            let p95 = quantile(
                conn,
                "ts >= ?1 AND method = ?2 AND coalesce(route, path) = ?3",
                &rargs,
                n,
                0.95,
            )?;
            out.top_routes.push(RouteStat {
                method,
                route,
                count: n,
                p95_ms: p95,
                errors: u64::try_from(errors).unwrap_or(0),
            });
        }
    }
    Ok(out)
}

impl LogsStore for SqliteLogs {
    fn append_requests(&self, rows: Vec<RequestLog>) -> BoxFuture<'_, CoreResult<()>> {
        Box::pin(async move {
            self.db
                .with_writer(move |conn| -> rusqlite::Result<()> {
                    let tx = conn.transaction()?;
                    {
                        let mut stmt = tx.prepare_cached(
                            "INSERT INTO _requests (ts, method, route, path, status, duration_ms, ip, user_agent, actor_type, actor_id, request_id, bytes_in, bytes_out, error)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                        )?;
                        for r in &rows {
                            stmt.execute(params![
                                r.ts,
                                r.method,
                                r.route,
                                r.path,
                                r.status,
                                r.duration_ms,
                                r.ip,
                                r.user_agent,
                                r.actor_type,
                                r.actor_id,
                                r.request_id,
                                r.bytes_in.and_then(|n| i64::try_from(n).ok()),
                                r.bytes_out.and_then(|n| i64::try_from(n).ok()),
                                r.error
                            ])?;
                        }
                    }
                    tx.commit()
                })
                .await
                .map_err(err)?
                .map_err(err)
        })
    }

    fn query_requests(
        &self,
        q: RequestLogQuery,
    ) -> BoxFuture<'_, CoreResult<(Vec<RequestLog>, u64)>> {
        Box::pin(async move {
            self.db
                .with_reader(move |conn| -> rusqlite::Result<(Vec<RequestLog>, u64)> {
                    let mut conds: Vec<String> = Vec::new();
                    let mut vals: Vec<SqlValue> = Vec::new();
                    fn bind(vals: &mut Vec<SqlValue>, v: SqlValue) -> String {
                        vals.push(v);
                        format!("?{}", vals.len())
                    }
                    if let Some(m) = &q.method {
                        let a = bind(&mut vals, SqlValue::Text(m.to_ascii_uppercase()));
                        conds.push(format!("method = {a}"));
                    }
                    if let Some(p) = &q.path {
                        let escaped = p
                            .replace('\\', "\\\\")
                            .replace('%', "\\%")
                            .replace('_', "\\_");
                        let a = bind(&mut vals, SqlValue::Text(p.clone()));
                        let b = bind(&mut vals, SqlValue::Text(format!("{escaped}%")));
                        conds.push(format!("(route = {a} OR path LIKE {b} ESCAPE '\\')"));
                    }
                    if let Some(s) = q.min_status {
                        let a = bind(&mut vals, SqlValue::Integer(i64::from(s)));
                        conds.push(format!("status >= {a}"));
                    }
                    if let Some(s) = q.max_status {
                        let a = bind(&mut vals, SqlValue::Integer(i64::from(s)));
                        conds.push(format!("status <= {a}"));
                    }
                    if let Some(x) = &q.actor_id {
                        let a = bind(&mut vals, SqlValue::Text(x.clone()));
                        conds.push(format!("actor_id = {a}"));
                    }
                    if let Some(x) = &q.request_id {
                        let a = bind(&mut vals, SqlValue::Text(x.clone()));
                        conds.push(format!("request_id = {a}"));
                    }
                    if let Some(x) = &q.from {
                        let a = bind(&mut vals, SqlValue::Text(x.clone()));
                        conds.push(format!("ts >= {a}"));
                    }
                    if let Some(x) = &q.to {
                        let a = bind(&mut vals, SqlValue::Text(x.clone()));
                        conds.push(format!("ts < {a}"));
                    }
                    let wh = if conds.is_empty() {
                        String::new()
                    } else {
                        format!("WHERE {}", conds.join(" AND "))
                    };
                    let total: i64 = conn.query_row(
                        &format!("SELECT count(*) FROM _requests {wh}"),
                        params_from_iter(vals.iter()),
                        |r| r.get(0),
                    )?;
                    let n = vals.len();
                    let mut page_vals = vals.clone();
                    page_vals.push(SqlValue::Integer(i64::from(q.limit.max(1))));
                    page_vals.push(SqlValue::Integer(i64::try_from(q.offset).unwrap_or(0)));
                    let mut stmt = conn.prepare(&format!(
                        "SELECT {COLS} FROM _requests {wh} ORDER BY id DESC LIMIT ?{} OFFSET ?{}",
                        n + 1,
                        n + 2
                    ))?;
                    let items = stmt
                        .query_map(params_from_iter(page_vals.iter()), row)?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    Ok((items, u64::try_from(total).unwrap_or(0)))
                })
                .await
                .map_err(err)?
                .map_err(err)
        })
    }

    fn request_stats(&self, since: String, top: u32) -> BoxFuture<'_, CoreResult<RequestStats>> {
        Box::pin(async move {
            self.db
                .with_reader(move |conn| stats(conn, &since, top))
                .await
                .map_err(err)?
                .map_err(err)
        })
    }

    fn put_points(&self, points: Vec<MetricPoint>) -> BoxFuture<'_, CoreResult<()>> {
        Box::pin(async move {
            self.db
                .with_writer(move |conn| -> rusqlite::Result<()> {
                    let tx = conn.transaction()?;
                    {
                        let mut stmt = tx.prepare_cached(
                            "INSERT OR REPLACE INTO _metric_points (ts, series, value) VALUES (?1, ?2, ?3)",
                        )?;
                        for p in &points {
                            stmt.execute(params![p.ts, p.series, p.value])?;
                        }
                    }
                    tx.commit()
                })
                .await
                .map_err(err)?
                .map_err(err)
        })
    }

    fn series(
        &self,
        series: Vec<String>,
        from: String,
        to: String,
    ) -> BoxFuture<'_, CoreResult<Vec<MetricPoint>>> {
        Box::pin(async move {
            self.db
                .with_reader(move |conn| -> rusqlite::Result<Vec<MetricPoint>> {
                    let json = serde_json::to_string(&series).unwrap_or_else(|_| "[]".into());
                    let mut stmt = conn.prepare_cached(
                        "SELECT ts, series, value FROM _metric_points
                         WHERE series IN (SELECT value FROM json_each(?1)) AND ts >= ?2 AND ts < ?3
                         ORDER BY ts",
                    )?;
                    stmt.query_map(params![json, from, to], |r| {
                        Ok(MetricPoint {
                            ts: r.get(0)?,
                            series: r.get(1)?,
                            value: r.get(2)?,
                        })
                    })?
                    .collect()
                })
                .await
                .map_err(err)?
                .map_err(err)
        })
    }

    fn prune(
        &self,
        requests_before: String,
        points_before: String,
    ) -> BoxFuture<'_, CoreResult<(u64, u64)>> {
        Box::pin(async move {
            self.db
                .with_writer(move |conn| -> rusqlite::Result<(u64, u64)> {
                    let a =
                        conn.execute("DELETE FROM _requests WHERE ts < ?1", [&requests_before])?;
                    let b =
                        conn.execute("DELETE FROM _metric_points WHERE ts < ?1", [&points_before])?;
                    Ok((a as u64, b as u64))
                })
                .await
                .map_err(err)?
                .map_err(err)
        })
    }

    fn size(&self) -> (u64, u64) {
        self.db.file_sizes()
    }
}
