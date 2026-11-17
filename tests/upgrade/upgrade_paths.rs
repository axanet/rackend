#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Upgrade-path test (SPEC §7.7): every committed fixture of a released
//! version must migrate with the HEAD binary without losing or changing data.

#[path = "snapshot.rs"]
mod snapshot;

use std::path::{Path, PathBuf};

use rackend_core::ports::StoreExt;
use rackend_store_sqlite::migrate::DATA_MIGRATIONS;
use rackend_store_sqlite::{DataDbPaths, DbOptions, SqliteStore, Synchronous, open_data_db};
use rusqlite::Connection;
use snapshot::{Expected, snapshot};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/upgrade/fixtures")
}

fn fixtures() -> Vec<(String, PathBuf, Expected)> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(fixtures_dir()).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(version) = name.strip_suffix(".db") else {
            continue;
        };
        let expected_path = fixtures_dir().join(format!("{version}.expected.json"));
        let expected: Expected = serde_json::from_str(
            &std::fs::read_to_string(&expected_path)
                .unwrap_or_else(|_| panic!("{version}: missing {}", expected_path.display())),
        )
        .unwrap();
        out.push((version.to_owned(), entry.path(), expected));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let dest = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &dest);
        } else {
            std::fs::copy(e.path(), dest).unwrap();
        }
    }
}

#[test]
fn there_is_at_least_one_fixture() {
    assert!(
        !fixtures().is_empty(),
        "tests/upgrade/fixtures must contain fixtures"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn every_fixture_upgrades_to_head_without_data_loss() {
    let head = DATA_MIGRATIONS.last().map_or(0, |m| m.id);
    for (version, path, expected) in fixtures() {
        let work = tempfile::tempdir().unwrap();
        let db_path = work.path().join("data.db");
        std::fs::copy(&path, &db_path).unwrap();
        let storage = fixtures_dir().join(format!("{version}.storage"));
        if storage.is_dir() {
            copy_dir(&storage, &work.path().join("storage"));
        }

        let (db, report) = open_data_db(
            &DataDbPaths {
                db: db_path.clone(),
                backups_dir: work.path().join("backups"),
            },
            DbOptions::data(Synchronous::Full),
            "0.0.0-head",
            time::OffsetDateTime::now_utc(),
        )
        .unwrap_or_else(|e| panic!("{version}: migration failed: {e}"));
        assert_eq!(report.from, expected.schema_version, "{version}");
        assert_eq!(report.to, head, "{version}: migrated to HEAD");
        if report.from < head {
            assert!(
                report.backup.is_some(),
                "{version}: a pre-upgrade backup was taken"
            );
        }

        // The instance starts: the store answers queries on the migrated DB.
        let store = SqliteStore::new(db.clone());
        let settings = store.read(|tx| tx.settings_list()).await.unwrap();
        assert_eq!(
            settings.len() as u64,
            expected.row_counts.get("_settings").copied().unwrap_or(0),
            "{version}: settings readable through the store"
        );
        db.shutdown().await.unwrap();

        let conn = Connection::open(&db_path).unwrap();
        let ic: String = conn
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ic, "ok", "{version}: integrity_check");
        let fk: i64 = conn
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(fk, 0, "{version}: foreign_key_check");

        let (counts, hashes) = snapshot(&conn);
        for (table, n) in &expected.row_counts {
            assert_eq!(
                counts.get(table),
                Some(n),
                "{version}: row count of {table}"
            );
        }
        for (table, hash) in &expected.record_hashes {
            if let Some(migration) = expected.intended_transformations.get(table) {
                eprintln!("{version}: {table} intentionally transformed by {migration}");
                continue;
            }
            assert_eq!(
                hashes.get(table),
                Some(hash),
                "{version}: records of {table} changed"
            );
        }
        // Later milestones add: passwords still verify, sessions resolve,
        // files resolve to blobs, and GET /api/collections/<c>/records works.
    }
}
