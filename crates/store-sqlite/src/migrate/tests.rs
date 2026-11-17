#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::conn::{DbOptions, Synchronous, open_writer};
use time::macros::datetime;

const NOW: OffsetDateTime = datetime!(2026-11-17 14:36:00 UTC);

struct Env {
    _dir: tempfile::TempDir,
    db: PathBuf,
    backups: PathBuf,
}

fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    Env {
        db: dir.path().join("data.db"),
        backups: dir.path().join("backups"),
        _dir: dir,
    }
}

fn opts<'a>(e: &'a Env, version: &'a str) -> MigrateOptions<'a> {
    MigrateOptions {
        backups_dir: Some(&e.backups),
        binary_version: version,
        now: NOW,
    }
}

fn open(e: &Env) -> Connection {
    open_writer(&e.db, &DbOptions::data(Synchronous::Full)).unwrap()
}

const M1: Migration = Migration {
    id: 1,
    name: "init",
    sql: include_str!("../../migrations/data/0001_init.sql"),
    rust: None,
    foreign_keys_off: false,
};
const M2_NOTES: Migration = Migration {
    id: 2,
    name: "notes",
    sql: "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL);",
    rust: None,
    foreign_keys_off: false,
};
const M3_GOOD: Migration = Migration {
    id: 3,
    name: "tags",
    sql: "CREATE TABLE tags (id INTEGER PRIMARY KEY);",
    rust: None,
    foreign_keys_off: false,
};
const M3_FAILING: Migration = Migration {
    id: 3,
    name: "tags",
    sql: "CREATE TABLE tags (id INTEGER PRIMARY KEY); THIS IS NOT SQL;",
    rust: None,
    foreign_keys_off: false,
};

fn meta(conn: &Connection, key: &str) -> Option<String> {
    meta_value(conn, key).unwrap()
}

#[test]
fn fresh_database_is_migrated_without_backup() {
    let e = env();
    let mut conn = open(&e);
    let report = run(&mut conn, &e.db, &[M1], &opts(&e, "0.1.0")).unwrap();
    assert_eq!(report.from, 0);
    assert_eq!(report.to, 1);
    assert_eq!(report.applied, vec![1]);
    assert_eq!(
        report.backup, None,
        "an empty database has nothing to back up"
    );
    assert_eq!(meta(&conn, "schema_version").as_deref(), Some("1"));
    assert_eq!(meta(&conn, "last_binary_version").as_deref(), Some("0.1.0"));
    assert!(meta(&conn, "created_at").is_some());

    let applied = applied_migrations(&conn).unwrap();
    assert_eq!(applied.len(), 1);
    assert_eq!(applied[0].checksum, M1.checksum());
    assert_eq!(applied[0].applied_at, "2026-11-17T14:36:00.000Z");
}

#[test]
fn rerun_is_a_no_op_but_records_new_binary_version() {
    let e = env();
    let mut conn = open(&e);
    run(&mut conn, &e.db, &[M1], &opts(&e, "0.1.0")).unwrap();
    let report = run(&mut conn, &e.db, &[M1], &opts(&e, "0.1.1")).unwrap();
    assert!(report.applied.is_empty());
    assert_eq!(meta(&conn, "last_binary_version").as_deref(), Some("0.1.1"));
    assert!(!e.backups.exists() || std::fs::read_dir(&e.backups).unwrap().next().is_none());
}

#[test]
fn upgrade_takes_verified_backup_named_after_versions() {
    let e = env();
    let mut conn = open(&e);
    run(&mut conn, &e.db, &[M1], &opts(&e, "0.1.0")).unwrap();
    let report = run(&mut conn, &e.db, &[M1, M2_NOTES], &opts(&e, "0.2.0")).unwrap();
    assert_eq!(report.applied, vec![2]);
    let backup = report.backup.expect("backup taken");
    assert_eq!(
        backup.file_name().unwrap().to_str().unwrap(),
        "pre-upgrade-0.1.0-to-0.2.0-20261117T143600Z.db"
    );
    let b = Connection::open_with_flags(&backup, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(
        schema_version(&b).unwrap(),
        1,
        "backup is the pre-upgrade state"
    );
}

#[test]
fn newer_schema_is_refused_with_exit_78() {
    let e = env();
    let mut conn = open(&e);
    run(&mut conn, &e.db, &[M1, M2_NOTES], &opts(&e, "0.2.0")).unwrap();
    let err = run(&mut conn, &e.db, &[M1], &opts(&e, "0.1.0")).unwrap_err();
    assert_eq!(err.exit_code(), 78);
    let msg = err.to_string();
    assert!(
        msg.starts_with("database schema v2 is newer than this binary supports (v1). Downgrading is not supported"),
        "{msg}"
    );
}

#[test]
fn checksum_mismatch_is_refused_with_exit_78() {
    let e = env();
    let mut conn = open(&e);
    run(&mut conn, &e.db, &[M1], &opts(&e, "0.1.0")).unwrap();
    conn.execute(
        "UPDATE _migrations SET checksum = 'tampered' WHERE id = 1",
        [],
    )
    .unwrap();
    let err = run(&mut conn, &e.db, &[M1], &opts(&e, "0.1.0")).unwrap_err();
    assert!(
        matches!(err, MigrateError::ChecksumMismatch { id: 1, .. }),
        "{err}"
    );
    assert_eq!(err.exit_code(), 78);
}

#[test]
fn unknown_applied_migration_is_refused_with_exit_78() {
    let e = env();
    let mut conn = open(&e);
    run(&mut conn, &e.db, &[M1], &opts(&e, "0.1.0")).unwrap();
    conn.execute(
        "INSERT INTO _migrations VALUES (7, 'mystery', 'x', '2026-01-01T00:00:00.000Z', '9.9.9')",
        [],
    )
    .unwrap();
    let err = run(&mut conn, &e.db, &[M1], &opts(&e, "0.1.0")).unwrap_err();
    assert!(
        matches!(err, MigrateError::UnknownMigration { id: 7, .. }),
        "{err}"
    );
    assert_eq!(err.exit_code(), 78);
}

fn file_bytes(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap()
}

#[test]
fn failing_migration_leaves_data_byte_identical_and_backup_exists() {
    let e = env();
    {
        let mut conn = open(&e);
        run(&mut conn, &e.db, &[M1, M2_NOTES], &opts(&e, "0.2.0")).unwrap();
        conn.execute("INSERT INTO notes (id, body) VALUES (1, 'keep me')", [])
            .unwrap();
    } // closing the last connection checkpoints the WAL into data.db
    let before = file_bytes(&e.db);

    {
        let mut conn = open(&e);
        let err = run(
            &mut conn,
            &e.db,
            &[M1, M2_NOTES, M3_FAILING],
            &opts(&e, "0.3.0"),
        )
        .unwrap_err();
        assert!(
            matches!(err, MigrateError::MigrationFailed { id: 3, .. }),
            "{err}"
        );
        assert_eq!(err.exit_code(), 70);
    }
    assert_eq!(file_bytes(&e.db), before, "data.db must be byte-identical");

    let backups: Vec<_> = std::fs::read_dir(&e.backups).unwrap().flatten().collect();
    assert_eq!(backups.len(), 1);
    let b =
        Connection::open_with_flags(backups[0].path(), OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let body: String = b
        .query_row("SELECT body FROM notes WHERE id = 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(body, "keep me");
}

#[test]
fn earlier_migrations_of_a_failed_run_stay_applied_and_next_start_resumes() {
    let e = env();
    let mut conn = open(&e);
    run(&mut conn, &e.db, &[M1], &opts(&e, "0.1.0")).unwrap();

    let err = run(
        &mut conn,
        &e.db,
        &[M1, M2_NOTES, M3_FAILING],
        &opts(&e, "0.3.0"),
    )
    .unwrap_err();
    assert!(
        matches!(err, MigrateError::MigrationFailed { id: 3, .. }),
        "{err}"
    );
    assert_eq!(
        schema_version(&conn).unwrap(),
        2,
        "migration 2 stays applied"
    );
    assert!(
        !table_exists(&conn, "tags").unwrap(),
        "migration 3 rolled back"
    );

    let report = run(
        &mut conn,
        &e.db,
        &[M1, M2_NOTES, M3_GOOD],
        &opts(&e, "0.3.1"),
    )
    .unwrap();
    assert_eq!(report.applied, vec![3]);
    assert_eq!(report.from, 2);
}

#[test]
fn foreign_key_violations_roll_back_a_fk_off_migration() {
    let e = env();
    let mut conn = open(&e);
    run(&mut conn, &e.db, &[M1], &opts(&e, "0.1.0")).unwrap();
    let bad = Migration {
        id: 2,
        name: "orphans",
        sql: "CREATE TABLE parent (id INTEGER PRIMARY KEY);
              CREATE TABLE child (id INTEGER PRIMARY KEY, parent INTEGER REFERENCES parent (id));
              INSERT INTO child (id, parent) VALUES (1, 42);",
        rust: None,
        foreign_keys_off: true,
    };
    let err = run(&mut conn, &e.db, &[M1, bad], &opts(&e, "0.2.0")).unwrap_err();
    assert!(err.to_string().contains("foreign key violations"), "{err}");
    assert!(!table_exists(&conn, "child").unwrap(), "rolled back");
    let fk_on: i64 = conn
        .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
        .unwrap();
    assert_eq!(fk_on, 1, "foreign keys are re-enabled");
}

#[test]
fn keeps_only_the_newest_pre_upgrade_backups() {
    let dir = tempfile::tempdir().unwrap();
    for day in 10..18 {
        std::fs::write(
            dir.path()
                .join(format!("pre-upgrade-0.1.0-to-0.2.0-202611{day}T000000Z.db")),
            b"x",
        )
        .unwrap();
    }
    std::fs::write(dir.path().join("backup-20261101T000000Z.tar.zst"), b"x").unwrap();
    prune_pre_upgrade_backups(dir.path(), 5).unwrap();
    let mut names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names.len(),
        6,
        "5 pre-upgrade backups + the unrelated file: {names:?}"
    );
    assert!(names.contains(&"backup-20261101T000000Z.tar.zst".to_owned()));
    assert!(
        !names.iter().any(|n| n.contains("20261112")),
        "oldest are gone"
    );
}

#[test]
fn status_reports_pending_without_writing() {
    let e = env();
    let mut conn = open(&e);
    run(&mut conn, &e.db, &[M1], &opts(&e, "0.1.0")).unwrap();
    let s = status(&conn, &[M1, M2_NOTES]).unwrap();
    assert_eq!(s.schema_version, 1);
    assert_eq!(s.binary_max, 2);
    assert_eq!(s.pending.len(), 1);
    assert_eq!(s.pending[0].id, 2);
}
