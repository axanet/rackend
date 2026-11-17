//! M5 acceptance: request logs, dashboard data, Prometheus, backups and
//! `rackend check` (SPEC §14, §18.4).

use std::sync::Arc;
use std::time::Duration;

use axum::http::{Method, StatusCode};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use crate::common::*;

async fn poll_logs(t: &TestApp, query: &str, want: u64) -> Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let (s, _, out) = call(
            &t.router,
            Method::GET,
            &format!("/api/logs?{query}"),
            Some(&t.token),
            None,
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{out}");
        if out["total_items"].as_u64().unwrap() >= want || std::time::Instant::now() > deadline {
            return out;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn requests_are_logged_with_route_actor_and_error() {
    let t = start().await;
    call(&t.router, Method::GET, "/api/health", None, None).await;
    let (s, h, _) = call(
        &t.router,
        Method::GET,
        "/api/collections/nope/records",
        Some(&t.token),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let rid = h["x-request-id"].to_str().unwrap().to_owned();
    let out = poll_logs(&t, &format!("request_id={rid}"), 1).await;
    let row = &out["items"][0];
    assert_eq!(row["route"], "/api/collections/{c}/records");
    assert_eq!(row["path"], "/api/collections/nope/records");
    assert_eq!(row["status"], 404);
    assert_eq!(row["error"], "not_found");
    assert_eq!(row["actor_type"], "superuser");
    assert!(row["duration_ms"].as_f64().unwrap() >= 0.0);
    let health = poll_logs(&t, "path=/api/health", 0).await;
    assert_eq!(health["total_items"], 0, "health checks are not logged");

    // min_status: only errors from now on.
    let (s, _, _) = call(
        &t.router,
        Method::PATCH,
        "/api/settings",
        Some(&t.token),
        Some(json!({"logs": {"min_status": 400}})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (_, h, _) = call(
        &t.router,
        Method::GET,
        "/api/collections",
        Some(&t.token),
        None,
    )
    .await;
    let ok_id = h["x-request-id"].to_str().unwrap().to_owned();
    let (_, h, _) = call(
        &t.router,
        Method::GET,
        "/api/collections/nope",
        Some(&t.token),
        None,
    )
    .await;
    let err_id = h["x-request-id"].to_str().unwrap().to_owned();
    assert_eq!(
        poll_logs(&t, &format!("request_id={err_id}"), 1).await["total_items"],
        1
    );
    assert_eq!(
        poll_logs(&t, &format!("request_id={ok_id}"), 0).await["total_items"],
        0
    );

    // Filters.
    let errors = poll_logs(&t, "min_status=400&method=get", 1).await;
    assert!(
        errors["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["status"].as_u64().unwrap() >= 400)
    );
    // Only superusers read logs.
    let (s, _, _) = call(&t.router, Method::GET, "/api/logs", None, None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn dashboard_summary_and_series() {
    let t = start().await;
    for _ in 0..5 {
        call(
            &t.router,
            Method::GET,
            "/api/collections",
            Some(&t.token),
            None,
        )
        .await;
    }
    poll_logs(&t, "path=/api/collections", 5).await;
    let (s, _, sum) = call(
        &t.router,
        Method::GET,
        "/api/metrics/summary",
        Some(&t.token),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{sum}");
    assert!(
        sum["requests"]["last_minute"].as_u64().unwrap() >= 5,
        "{sum}"
    );
    assert!(sum["requests"]["latency_ms"]["p95"].as_f64().unwrap() >= 0.0);
    assert!(
        sum["requests"]["top_routes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["route"] == "/api/collections")
    );
    assert!(sum["host"]["memory_total"].as_u64().unwrap() > 0);
    assert!(sum["host"]["disk_total"].as_u64().unwrap() > 0);
    assert!(sum["db"]["data_bytes"].as_u64().unwrap() > 0);
    assert_eq!(sum["realtime"]["connections"], 0);
    assert!(sum["disk_low"].is_boolean());

    let points = t.app.sample_metrics().await.unwrap();
    assert!(points.iter().any(|p| p.series == "http.rps"));
    let (s, _, series) = call(
        &t.router,
        Method::GET,
        "/api/metrics/series?series=http.rps,host.memory_total",
        Some(&t.token),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{series}");
    assert_eq!(series["series"]["http.rps"].as_array().unwrap().len(), 1);
    assert!(
        series["series"]["host.memory_total"][0][1]
            .as_f64()
            .unwrap()
            > 0.0
    );
    let (s, _, _) = call(
        &t.router,
        Method::GET,
        "/api/metrics/series",
        Some(&t.token),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _, _) = call(&t.router, Method::GET, "/api/metrics/summary", None, None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn prometheus_needs_a_token_or_public_flag() {
    let t = start().await;
    let router = rackend::wiring::router(
        Arc::clone(&t.app),
        rackend_http::HttpOptions {
            prometheus: rackend::metrics::recorder(),
            ..Default::default()
        },
    );
    call(
        &router,
        Method::GET,
        "/api/collections",
        Some(&t.token),
        None,
    )
    .await;
    let r = send(&router, request(Method::GET, "/metrics", None, None)).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let (s, _, _) = call(
        &router,
        Method::PATCH,
        "/api/settings",
        Some(&t.token),
        Some(json!({"metrics": {"prometheus_token": "scrape-secret-1234"}})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let r = send(
        &router,
        request(Method::GET, "/metrics", Some("wrong"), None),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = send(
        &router,
        request(Method::GET, "/metrics", Some("scrape-secret-1234"), None),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let text = String::from_utf8(r.body).unwrap();
    for name in [
        "rackend_http_requests_total",
        "rackend_http_request_duration_seconds_bucket",
        "rackend_db_size_bytes",
        "rackend_host_memory_total_bytes",
        "rackend_realtime_connections",
    ] {
        assert!(text.contains(name), "{name} missing");
    }
    let (_, _, settings) = call(&router, Method::GET, "/api/settings", Some(&t.token), None).await;
    assert_eq!(
        settings["metrics"]["prometheus_token"], "\u{2022}\u{2022}\u{2022}\u{2022}",
        "masked"
    );
}

/// SHA-256 over every user record of every collection, in a fixed order.
fn record_hash(db: &std::path::Path) -> String {
    let conn =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let mut tables: Vec<String> = conn
        .prepare("SELECT name FROM _collections ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    tables.sort();
    let mut h = Sha256::new();
    for t in tables {
        let sql = format!("SELECT * FROM \"{t}\" ORDER BY id");
        let mut stmt = conn.prepare(&sql).unwrap();
        let n = stmt.column_count();
        let rows = stmt
            .query_map([], |r| {
                Ok((0..n)
                    .map(|i| format!("{:?}", r.get::<_, rusqlite::types::Value>(i).unwrap()))
                    .collect::<Vec<_>>()
                    .join("|"))
            })
            .unwrap();
        h.update(t.as_bytes());
        for row in rows {
            h.update(row.unwrap().as_bytes());
        }
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

#[tokio::test]
async fn backup_and_restore_round_trip() {
    let t = start().await;
    let (s, _, out) = call(
        &t.router,
        Method::POST,
        "/api/collections",
        Some(&t.token),
        Some(json!({
            "name": "notes",
            "fields": [{"name": "text", "type": "text"}, {"name": "file", "type": "file"}],
        })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{out}");
    for i in 0..20 {
        let (s, _, out) = call_multipart(
            &t.router,
            Method::POST,
            "/api/collections/notes/records",
            Some(&t.token),
            Some(json!({"text": format!("note {i}")})),
            &[("file", "n.txt", format!("body {i}").as_bytes())],
        )
        .await;
        assert_eq!(s, StatusCode::CREATED, "{out}");
    }
    let before = record_hash(&t.dir.path().join("data.db"));

    let (s, _, info) = call(
        &t.router,
        Method::POST,
        "/api/backups",
        Some(&t.token),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{info}");
    assert_eq!(info["includes_storage"], true);
    let name = info["name"].as_str().unwrap().to_owned();
    let (_, _, list) = call(&t.router, Method::GET, "/api/backups", Some(&t.token), None).await;
    assert_eq!(list["items"][0]["name"], name.as_str());
    let r = send(
        &t.router,
        request(
            Method::GET,
            &format!("/api/backups/{name}"),
            Some(&t.token),
            None,
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(&r.body[..4], &[0x28, 0xB5, 0x2F, 0xFD], "zstd magic");
    let (_, _, audit) = call(
        &t.router,
        Method::GET,
        "/api/audit?action=backup.create",
        Some(&t.token),
        None,
    )
    .await;
    assert_eq!(audit["total_items"], 1);
    // NDJSON export: one JSON object per line.
    let r = send(
        &t.router,
        request(
            Method::GET,
            "/api/audit?format=ndjson&action=record.create",
            Some(&t.token),
            None,
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.headers["content-type"], "application/x-ndjson");
    let text = String::from_utf8(r.body).unwrap();
    let lines: Vec<Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 20);
    assert!(lines.iter().all(|l| l["action"] == "record.create"));

    // Change the live data afterwards; the backup must not see it.
    call(
        &t.router,
        Method::POST,
        "/api/collections/notes/records",
        Some(&t.token),
        Some(json!({"text": "after"})),
    )
    .await;
    assert_ne!(record_hash(&t.dir.path().join("data.db")), before);

    // Restore into another data directory with existing files.
    let target = tempfile::tempdir().unwrap();
    std::fs::write(target.path().join("data.db"), b"old").unwrap();
    std::fs::create_dir_all(target.path().join("storage")).unwrap();
    std::fs::create_dir_all(target.path().join("tmp")).unwrap();
    let archive = t.dir.path().join("backups").join(&name);
    let restored = rackend::backup::restore(target.path(), &archive).unwrap();
    assert_eq!(
        record_hash(&target.path().join("data.db")),
        before,
        "identical records"
    );
    assert_eq!(
        std::fs::read(restored.replaced.join("data.db")).unwrap(),
        b"old",
        "replaced files are kept"
    );
    // Every stored file came back with the same bytes.
    let conn = rusqlite::Connection::open(target.path().join("data.db")).unwrap();
    let rows: Vec<(String, String)> = conn
        .prepare("SELECT storage_key, sha256 FROM _files WHERE deleted_at IS NULL")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(rows.len(), 20);
    for (key, sum) in rows {
        let bytes = std::fs::read(target.path().join("storage").join(&key)).unwrap();
        let got: String = Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(got, sum, "{key}");
    }

    // A damaged archive is refused before anything moves.
    let bad = target.path().join("bad.tar.zst");
    let mut bytes = std::fs::read(&archive).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xFF;
    std::fs::write(&bad, bytes).unwrap();
    let hash_now = record_hash(&target.path().join("data.db"));
    assert!(rackend::backup::restore(target.path(), &bad).is_err());
    assert_eq!(record_hash(&target.path().join("data.db")), hash_now);

    // `rackend check` agrees, then notices a missing blob.
    let report = rackend::check::run(t.dir.path(), None).await.unwrap();
    assert!(report.ok(), "{report:?}");
    let key: String = rusqlite::Connection::open(t.dir.path().join("data.db"))
        .unwrap()
        .query_row("SELECT storage_key FROM _files LIMIT 1", [], |r| r.get(0))
        .unwrap();
    std::fs::remove_file(t.dir.path().join("storage").join(&key)).unwrap();
    let report = rackend::check::run(t.dir.path(), None).await.unwrap();
    assert_eq!(report.missing_blobs, vec![key]);
    assert!(!report.ok());

    let (s, _, _) = call(
        &t.router,
        Method::DELETE,
        &format!("/api/backups/{name}"),
        Some(&t.token),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _, _) = call(
        &t.router,
        Method::GET,
        "/api/backups/..%2Fdata.db",
        Some(&t.token),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn scheduled_backups_run_once_and_keep_the_newest() {
    let t = start().await;
    let (s, _, out) = call(
        &t.router,
        Method::PATCH,
        "/api/settings",
        Some(&t.token),
        Some(json!({"backups": {"schedule": "not cron"}})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{out}");
    // Every minute, keep one.
    let (s, _, _) = call(
        &t.router,
        Method::PATCH,
        "/api/settings",
        Some(&t.token),
        Some(json!({"backups": {"schedule": "* * * * *", "keep": 1}})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let first = t.app.run_scheduled_backup().await.unwrap().expect("due");
    assert!(
        t.app.run_scheduled_backup().await.unwrap().is_none(),
        "once per minute"
    );
    // Pretend the next minute came: the older archive is pruned.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    *t.app.observe_last_backup_for_tests() = None;
    let second = t
        .app
        .run_scheduled_backup()
        .await
        .unwrap()
        .expect("due again");
    let (_, _, list) = call(&t.router, Method::GET, "/api/backups", Some(&t.token), None).await;
    let names: Vec<&str> = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [second.name.as_str()],
        "keep = 1 (first was {})",
        first.name
    );
    // Disabled.
    call(
        &t.router,
        Method::PATCH,
        "/api/settings",
        Some(&t.token),
        Some(json!({"backups": {"schedule": ""}})),
    )
    .await;
    *t.app.observe_last_backup_for_tests() = None;
    assert!(t.app.run_scheduled_backup().await.unwrap().is_none());
}
