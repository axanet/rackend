//! `rackend check` (SPEC §5.1): database integrity, foreign keys and a blob
//! consistency report. Read only; safe next to a running instance.

use std::collections::HashSet;
use std::path::Path;

use rackend_core::ports::{BlobStoreFactory as _, StorageConfig};
use rackend_core::secrets::SecretBox;
use serde_json::Value;

use crate::wiring::BlobFactory;

/// The findings.
#[derive(Debug, Default)]
pub struct Report {
    /// `PRAGMA integrity_check` lines other than `ok`.
    pub integrity: Vec<String>,
    /// `PRAGMA foreign_key_check` rows.
    pub foreign_keys: Vec<String>,
    /// Files with a `_files` row but no blob.
    pub missing_blobs: Vec<String>,
    /// Blobs no record refers to (the GC removes them after an hour).
    pub orphan_blobs: Vec<String>,
    /// Problems reading the storage.
    pub storage_error: Option<String>,
}

impl Report {
    /// Whether anything needs attention (orphans alone do not).
    pub fn ok(&self) -> bool {
        self.integrity.is_empty()
            && self.foreign_keys.is_empty()
            && self.missing_blobs.is_empty()
            && self.storage_error.is_none()
    }
}

/// The key that decrypts settings secrets, without changing anything.
fn secrets_key(conn: &rusqlite::Connection, env_key: Option<[u8; 32]>) -> Option<SecretBox> {
    if let Some(k) = env_key {
        return Some(SecretBox::new(&k));
    }
    let local: String = conn
        .query_row(
            "SELECT value FROM _meta WHERE key = 'local_secret_key'",
            [],
            |r| r.get(0),
        )
        .ok()?;
    SecretBox::parse_key(&local)
        .ok()
        .map(|k| SecretBox::new(&k))
}

/// Runs every check on the data directory `root`.
pub async fn run(root: &Path, env_key: Option<[u8; 32]>) -> Result<Report, String> {
    let db = root.join("data.db");
    if !db.is_file() {
        return Err(format!("no database at {}", db.display()));
    }
    let conn =
        rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| format!("cannot open {}: {e}", db.display()))?;
    let mut report = Report::default();
    let q = |sql: &str| -> Result<Vec<String>, String> {
        let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
        let n = stmt.column_count();
        let rows = stmt
            .query_map([], |r| {
                Ok((0..n)
                    .map(|i| {
                        r.get::<_, rusqlite::types::Value>(i)
                            .map(|v| format!("{v:?}"))
                            .unwrap_or_default()
                    })
                    .collect::<Vec<_>>()
                    .join(" "))
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
    };
    report.integrity = q("PRAGMA integrity_check")?
        .into_iter()
        .filter(|l| l != "Text(\"ok\")")
        .collect();
    report.foreign_keys = q("PRAGMA foreign_key_check")?;

    // Blob consistency.
    let mut stmt = conn
        .prepare("SELECT storage_key FROM _files WHERE deleted_at IS NULL")
        .map_err(|e| e.to_string())?;
    let keys: HashSet<String> = stmt
        .query_map([], |r| r.get(0))
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .collect();
    let storage: Value = conn
        .query_row(
            "SELECT value FROM _settings WHERE key = 'storage'",
            [],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null);
    let cfg = if storage["backend"] == "s3" {
        let s3 = &storage["s3"];
        let secret = s3["secret_access_key"]
            .as_str()
            .and_then(|enc| secrets_key(&conn, env_key).and_then(|b| b.decrypt_str(enc).ok()));
        let s = |k: &str| s3[k].as_str().unwrap_or_default().to_owned();
        StorageConfig::S3(rackend_core::ports::S3Config {
            endpoint: s("endpoint"),
            region: s("region"),
            bucket: s("bucket"),
            prefix: s("prefix"),
            access_key_id: s("access_key_id"),
            secret_access_key: secret.unwrap_or_default(),
            force_path_style: s3["force_path_style"].as_bool().unwrap_or(true),
            allow_http: s3["allow_http"].as_bool().unwrap_or(false),
        })
    } else {
        StorageConfig::Fs
    };
    let factory = BlobFactory {
        root: root.join("storage"),
        tmp: root.join("tmp"),
    };
    match factory.build(&cfg) {
        Err(e) => report.storage_error = Some(e.to_string()),
        Ok(store) => match store.list("").await {
            Err(e) => report.storage_error = Some(e.to_string()),
            Ok(items) => {
                let blobs: HashSet<String> = items
                    .into_iter()
                    .map(|i| i.key)
                    .filter(|k| !k.starts_with("_thumbs/") && !k.starts_with("_rackend-probe/"))
                    .collect();
                report.missing_blobs = keys.difference(&blobs).cloned().collect();
                report.orphan_blobs = blobs.difference(&keys).cloned().collect();
                report.missing_blobs.sort();
                report.orphan_blobs.sort();
            }
        },
    }
    Ok(report)
}
