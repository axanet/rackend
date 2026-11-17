#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Generates the upgrade fixture of a release (SPEC §7.7):
//!
//!     cargo run -p rackend-store-sqlite --example generate_fixture -- tests/upgrade/fixtures <version>
//!
//! Run it on the release commit, so the fixture holds exactly the schema that
//! release shipped. It writes `<version>.db`, `<version>.expected.json` and
//! (once files exist) `<version>.storage/`.
//!
//! Each milestone extends `populate` with realistic data for its features:
//! collections with every field type, relations, files, users with password +
//! passkey + external identities, sessions, OIDC clients, audit entries and
//! trashed fields.

#[path = "snapshot.rs"]
mod snapshot;

use std::path::PathBuf;

use rackend_store_sqlite::{DataDbPaths, DbOptions, Synchronous, open_data_db};
use rusqlite::Connection;
use snapshot::{Expected, snapshot};

fn populate(conn: &Connection) {
    // M0: runtime settings, including a secret-flagged row.
    let rows = [
        (
            "app",
            r#"{"name":"Fixture App","public_url":"https://fixture.example.com"}"#,
            0,
        ),
        (
            "limits",
            r#"{"max_json_body_kb":2048,"batch_max_requests":25}"#,
            0,
        ),
        (
            "smtp",
            r#"{"host":"smtp.example.com","port":587,"password":"enc:v1:AAAA"}"#,
            1,
        ),
        (
            "logs",
            r#"{"retention_days":14,"min_status":0,"sample_rate":0.5}"#,
            0,
        ),
    ];
    for (key, value, secret) in rows {
        conn.execute(
            "INSERT INTO _settings (key, value, is_secret, updated) VALUES (?1, ?2, ?3, '2026-11-17T12:00:00.000Z')",
            rusqlite::params![key, value, secret],
        )
        .unwrap();
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let out_dir = PathBuf::from(
        args.next()
            .expect("usage: generate_fixture <out_dir> <version>"),
    );
    let version = args
        .next()
        .expect("usage: generate_fixture <out_dir> <version>");
    std::fs::create_dir_all(&out_dir).unwrap();

    let work = tempfile::tempdir().unwrap();
    let paths = DataDbPaths {
        db: work.path().join("data.db"),
        backups_dir: work.path().join("backups"),
    };
    let (db, report) = open_data_db(
        &paths,
        DbOptions::data(Synchronous::Full),
        &version,
        time::OffsetDateTime::now_utc(),
    )
    .unwrap();
    drop(db); // the fixture is built with a plain connection below

    let conn = Connection::open(&paths.db).unwrap();
    populate(&conn);

    let target = out_dir.join(format!("{version}.db"));
    let _ = std::fs::remove_file(&target);
    conn.execute("VACUUM INTO ?1", [target.to_str().unwrap()])
        .unwrap();

    let fixture = Connection::open(&target).unwrap();
    // A committed fixture must be a single self-contained file.
    fixture
        .execute_batch("PRAGMA journal_mode = DELETE;")
        .unwrap();
    let (row_counts, record_hashes) = snapshot(&fixture);
    let expected = Expected {
        binary_version: version.clone(),
        schema_version: report.to,
        row_counts,
        record_hashes,
        intended_transformations: Default::default(),
    };
    std::fs::write(
        out_dir.join(format!("{version}.expected.json")),
        serde_json::to_string_pretty(&expected).unwrap() + "\n",
    )
    .unwrap();
    println!("wrote {} (schema v{})", target.display(), report.to);
}
