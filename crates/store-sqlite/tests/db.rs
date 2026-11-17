#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Connection model, Store port and logs.db recovery.

use std::sync::mpsc;

use rackend_core::Error as CoreError;
use rackend_core::error::UnavailableReason;
use rackend_core::ports::{SettingRow, StoreExt};
use rackend_store_sqlite::{
    DataDbPaths, DbOptions, Error, SqliteStore, Synchronous, open_data_db, open_logs_db,
};
use time::OffsetDateTime;

fn open(dir: &tempfile::TempDir, opts: DbOptions) -> SqliteStore {
    let paths = DataDbPaths {
        db: dir.path().join("data.db"),
        backups_dir: dir.path().join("backups"),
    };
    let (db, _) = open_data_db(&paths, opts, "0.0.0-test", OffsetDateTime::now_utc()).unwrap();
    SqliteStore::new(db)
}

fn row(key: &str, value: &str) -> SettingRow {
    SettingRow {
        key: key.into(),
        value: value.into(),
        is_secret: false,
        updated: "2026-11-17T00:00:00.000Z".into(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn write_commits_and_read_sees_it() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(&dir, DbOptions::data(Synchronous::Full));
    store
        .write(|tx| tx.setting_put(&row("app", r#"{"name":"x"}"#)))
        .await
        .unwrap();
    let rows = store.read(|tx| tx.settings_list()).await.unwrap();
    assert_eq!(rows, vec![row("app", r#"{"name":"x"}"#)]);
    let v = store
        .read(|tx| tx.meta_get("schema_version"))
        .await
        .unwrap();
    let head = rackend_store_sqlite::migrate::DATA_MIGRATIONS
        .len()
        .to_string();
    assert_eq!(v.as_deref(), Some(head.as_str()));
}

#[tokio::test(flavor = "multi_thread")]
async fn returning_err_rolls_back() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(&dir, DbOptions::data(Synchronous::Full));
    let res: Result<(), _> = store
        .write(|tx| {
            tx.setting_put(&row("app", "{}"))?;
            Err(CoreError::Conflict("abort".into()))
        })
        .await;
    assert!(matches!(res, Err(CoreError::Conflict(_))));
    assert!(
        store
            .read(|tx| tx.settings_list())
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn full_writer_queue_is_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let mut opts = DbOptions::data(Synchronous::Full);
    opts.writer_queue = 1;
    let store = open(&dir, opts);
    let db = store.db().clone();

    let (started_tx, started_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let busy = {
        let db = db.clone();
        tokio::spawn(async move {
            db.with_writer(move |_| {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            })
            .await
        })
    };
    started_rx.recv().unwrap(); // the writer is now executing the busy job
    let queued = {
        let db = db.clone();
        tokio::spawn(async move { db.with_writer(|_| 1).await })
    };
    // Give the queued job time to occupy the single queue slot.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let err = store.write(|_| Ok(())).await.unwrap_err();
    assert!(
        matches!(
            err,
            CoreError::Unavailable {
                reason: UnavailableReason::WriterQueueFull,
                retry_after_s: 1
            }
        ),
        "{err:?}"
    );

    release_tx.send(()).unwrap();
    busy.await.unwrap().unwrap();
    assert_eq!(queued.await.unwrap().unwrap(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_drains_queue_checkpoints_and_rejects_new_writes() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(&dir, DbOptions::data(Synchronous::Full));
    let mut pending = Vec::new();
    for i in 0..50 {
        let s = store.clone();
        pending.push(tokio::spawn(async move {
            s.write(move |tx| tx.setting_put(&row(&format!("k{i:02}"), "1")))
                .await
        }));
    }
    // Let the spawned writes enqueue before shutting down.
    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    store.db().shutdown().await.unwrap();

    let mut ok = 0;
    for p in pending {
        if p.await.unwrap().is_ok() {
            ok += 1;
        }
    }
    let err = store.write(|_| Ok(())).await.unwrap_err();
    assert!(matches!(
        err,
        CoreError::Unavailable {
            reason: UnavailableReason::ShuttingDown,
            ..
        }
    ));

    let wal = dir.path().join("data.db-wal");
    let wal_len = std::fs::metadata(&wal).map_or(0, |m| m.len());
    assert_eq!(wal_len, 0, "WAL truncated by the shutdown checkpoint");

    // Everything that was acknowledged is durable.
    let conn = rusqlite_count(&dir.path().join("data.db"));
    assert_eq!(conn, ok);
}

fn rusqlite_count(path: &std::path::Path) -> usize {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.query_row("SELECT count(*) FROM _settings", [], |r| r.get::<_, i64>(0))
        .map(|n| usize::try_from(n).unwrap())
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn broken_logs_db_is_moved_aside_and_recreated() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("logs.db");
    std::fs::write(
        &path,
        b"this is not a sqlite database at all, just garbage bytes......",
    )
    .unwrap();
    let logs = open_logs_db(
        &path,
        DbOptions::logs(Synchronous::Full),
        "0.0.0-test",
        OffsetDateTime::now_utc(),
    )
    .unwrap();
    let n: i64 = logs
        .db()
        .with_reader(|c| c.query_row("SELECT count(*) FROM _requests", [], |r| r.get(0)))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(n, 0);
    let moved: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("logs.db.broken-")
        })
        .collect();
    assert_eq!(moved.len(), 1);
}

#[test]
fn error_maps_to_core_unavailable() {
    let e: CoreError = Error::QueueFull.into();
    assert!(matches!(e, CoreError::Unavailable { .. }));
}
