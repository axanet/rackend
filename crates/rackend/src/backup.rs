//! Backups (SPEC §18.4): `backup-<ts>.tar.zst` in `backups/` holding a
//! `VACUUM INTO` snapshot of `data.db`, `storage/` (filesystem backend,
//! thumbnails excluded) and `manifest.json` with a SHA-256 per file.
//!
//! The database is snapshotted first, then `storage/` is archived: files
//! uploaded in between are harmless extras, files deleted in between can be
//! missing (`rackend check` reports them). Restore is CLI only, with the
//! instance stopped; it never deletes anything, the replaced files move to
//! `backups/replaced-<ts>/`.

use std::fs::{self, File};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};

use rackend_core::ports::{BackupInfo, Backups, BoxFuture};
use rackend_core::time::{format_file_timestamp, format_timestamp};
use rackend_store_sqlite::SqliteDb;
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};
use time::OffsetDateTime;

/// zstd level: fast with a good ratio.
const LEVEL: i32 = 3;

fn io_err(e: impl std::fmt::Display) -> rackend_core::Error {
    rackend_core::Error::internal(format!("backup: {e}"))
}

/// Whether `name` is a backup archive name we created.
pub fn is_backup_name(name: &str) -> bool {
    name.starts_with("backup-")
        && name.ends_with(".tar.zst")
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.' || b == b'_')
}

fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut f = BufReader::new(File::open(path)?);
    let mut h = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Files under `root` (relative paths with `/`), skipping `_thumbs/`.
fn storage_files(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        for entry in entries {
            let entry = entry?;
            let p = entry.path();
            let rel = p.strip_prefix(root).unwrap_or(&p).to_path_buf();
            if rel.starts_with("_thumbs") {
                continue;
            }
            let ft = entry.file_type()?;
            if ft.is_dir() {
                stack.push(p);
            } else if ft.is_file() {
                out.push(rel);
            }
        }
    }
    out.sort();
    Ok(out)
}

fn rel_name(p: &Path) -> String {
    p.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// The highest applied migration of a database file.
fn schema_version(db: &Path) -> Option<i64> {
    let conn =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .ok()?;
    conn.query_row("SELECT max(id) FROM _migrations", [], |r| r.get(0))
        .ok()
}

/// Writes the archive from an already taken database snapshot.
fn write_archive(
    snapshot: &Path,
    storage: Option<&Path>,
    dest: &Path,
    version: &str,
    created: OffsetDateTime,
) -> std::io::Result<Value> {
    let mut files = Map::new();
    files.insert("data.db".into(), Value::String(sha256_file(snapshot)?));
    let stored = match storage {
        Some(root) => storage_files(root)?,
        None => Vec::new(),
    };
    let partial = dest.with_extension("partial");
    let out = File::create(&partial)?;
    let enc = zstd::stream::write::Encoder::new(out, LEVEL)?;
    let mut tar = tar::Builder::new(enc);
    tar.mode(tar::HeaderMode::Deterministic);
    tar.append_path_with_name(snapshot, "data.db")?;
    if let Some(root) = storage {
        for rel in &stored {
            let full = root.join(rel);
            // Files deleted since the listing are skipped (SPEC §18.4).
            let Ok(sum) = sha256_file(&full) else {
                continue;
            };
            let name = format!("storage/{}", rel_name(rel));
            tar.append_path_with_name(&full, &name)?;
            files.insert(name, Value::String(sum));
        }
    }
    let manifest = json!({
        "format": 1,
        "version": version,
        "schema_version": schema_version(snapshot),
        "created": format_timestamp(created),
        "includes_storage": storage.is_some(),
        "files": files,
    });
    let bytes = serde_json::to_vec_pretty(&manifest).unwrap_or_default();
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(&mut header, "manifest.json", bytes.as_slice())?;
    let enc = tar.into_inner()?;
    let mut out = enc.finish()?;
    out.flush()?;
    out.sync_all()?;
    drop(out);
    fs::rename(&partial, dest)?;
    // A sidecar copy of the manifest lets `list` work without reading archives.
    fs::write(
        dest.with_extension("json"),
        serde_json::to_vec_pretty(&manifest).unwrap_or_default(),
    )?;
    Ok(manifest)
}

/// Backups of one data directory.
#[derive(Debug, Clone)]
pub struct FsBackups {
    dir: PathBuf,
    storage: PathBuf,
    tmp: PathBuf,
    data: Option<SqliteDb>,
    data_path: PathBuf,
    version: &'static str,
}

impl FsBackups {
    /// Backups of the data directory at `root`. With `data`, snapshots go
    /// through the running writer; without, a read-only connection is used
    /// (CLI).
    pub fn new(root: &Path, data: Option<SqliteDb>, version: &'static str) -> Self {
        Self {
            dir: root.join("backups"),
            storage: root.join("storage"),
            tmp: root.join("tmp"),
            data,
            data_path: root.join("data.db"),
            version,
        }
    }

    async fn snapshot(&self, dest: &Path) -> Result<(), rackend_core::Error> {
        match &self.data {
            Some(db) => db.snapshot_to(dest).await.map_err(io_err),
            None => {
                let (src, dest) = (self.data_path.clone(), dest.to_path_buf());
                tokio::task::spawn_blocking(move || -> rusqlite::Result<()> {
                    let conn = rusqlite::Connection::open_with_flags(
                        &src,
                        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
                    )?;
                    conn.execute("VACUUM INTO ?1", [dest.to_string_lossy()])
                        .map(drop)
                })
                .await
                .map_err(io_err)?
                .map_err(io_err)
            }
        }
    }

    fn info(&self, name: &str) -> Option<BackupInfo> {
        let path = self.dir.join(name);
        let meta = fs::metadata(&path).ok()?;
        let manifest: Value = fs::read(path.with_extension("json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or(Value::Null);
        let created = manifest["created"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| {
                meta.modified()
                    .map(|m| format_timestamp(OffsetDateTime::from(m)))
                    .unwrap_or_default()
            });
        Some(BackupInfo {
            name: name.to_owned(),
            size: meta.len(),
            created,
            includes_storage: manifest["includes_storage"].as_bool().unwrap_or(false),
        })
    }

    /// Creates an archive now.
    pub async fn create_now(
        &self,
        include_storage: bool,
    ) -> Result<BackupInfo, rackend_core::Error> {
        fs::create_dir_all(&self.dir).map_err(io_err)?;
        fs::create_dir_all(&self.tmp).map_err(io_err)?;
        let now = OffsetDateTime::now_utc();
        let name = format!("backup-{}.tar.zst", format_file_timestamp(now));
        let snap = self
            .tmp
            .join(format!("snapshot-{}.db", uuid::Uuid::now_v7()));
        self.snapshot(&snap).await?;
        let (dest, storage, version) = (self.dir.join(&name), self.storage.clone(), self.version);
        let snap2 = snap.clone();
        let res = tokio::task::spawn_blocking(move || {
            write_archive(
                &snap2,
                include_storage.then_some(storage.as_path()),
                &dest,
                version,
                now,
            )
        })
        .await
        .map_err(io_err)?;
        let _ = fs::remove_file(&snap);
        res.map_err(io_err)?;
        self.info(&name)
            .ok_or_else(|| io_err("the archive disappeared"))
    }
}

impl Backups for FsBackups {
    fn create(&self, include_storage: bool) -> BoxFuture<'_, rackend_core::Result<BackupInfo>> {
        Box::pin(self.create_now(include_storage))
    }

    fn list(&self) -> BoxFuture<'_, rackend_core::Result<Vec<BackupInfo>>> {
        Box::pin(async move {
            let mut out: Vec<BackupInfo> = match fs::read_dir(&self.dir) {
                Ok(entries) => entries
                    .filter_map(Result::ok)
                    .filter_map(|e| e.file_name().to_str().map(str::to_owned))
                    .filter(|n| is_backup_name(n))
                    .filter_map(|n| self.info(&n))
                    .collect(),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
                Err(e) => return Err(io_err(e)),
            };
            out.sort_by(|a, b| b.name.cmp(&a.name));
            Ok(out)
        })
    }

    fn delete(&self, name: String) -> BoxFuture<'_, rackend_core::Result<()>> {
        Box::pin(async move {
            let path = self.path(&name).ok_or(rackend_core::Error::NotFound)?;
            fs::remove_file(&path).map_err(io_err)?;
            let _ = fs::remove_file(path.with_extension("json"));
            Ok(())
        })
    }

    fn path(&self, name: &str) -> Option<PathBuf> {
        if !is_backup_name(name) {
            return None;
        }
        let p = self.dir.join(name);
        p.is_file().then_some(p)
    }
}

/// What a restore did.
#[derive(Debug)]
pub struct Restored {
    /// Where the replaced files went.
    pub replaced: PathBuf,
    /// Files restored.
    pub files: usize,
    /// The archive's manifest.
    pub manifest: Value,
}

/// Restores `archive` into the data directory `root`, which must be locked
/// by the caller (instance stopped). Checksums are verified before anything
/// is moved.
pub fn restore(root: &Path, archive: &Path) -> Result<Restored, String> {
    let now = OffsetDateTime::now_utc();
    let stage = root
        .join("tmp")
        .join(format!("restore-{}", format_file_timestamp(now)));
    fs::create_dir_all(&stage).map_err(|e| format!("cannot create {}: {e}", stage.display()))?;
    let file =
        File::open(archive).map_err(|e| format!("cannot open {}: {e}", archive.display()))?;
    let dec =
        zstd::stream::read::Decoder::new(file).map_err(|e| format!("not a zstd archive: {e}"))?;
    let mut tar = tar::Archive::new(dec);
    for entry in tar
        .entries()
        .map_err(|e| format!("not a tar archive: {e}"))?
    {
        let mut entry = entry.map_err(|e| format!("corrupt archive: {e}"))?;
        let path = entry
            .path()
            .map_err(|e| format!("corrupt archive: {e}"))?
            .into_owned();
        let safe = path
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)));
        if !safe {
            return Err(format!("refusing unsafe path {}", path.display()));
        }
        let dest = stage.join(&path);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        entry
            .unpack(&dest)
            .map_err(|e| format!("cannot extract {}: {e}", path.display()))?;
    }
    let manifest: Value = fs::read(stage.join("manifest.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .ok_or("the archive has no valid manifest.json")?;
    let files = manifest["files"]
        .as_object()
        .ok_or("the manifest lists no files")?;
    for (name, sum) in files {
        let got = sha256_file(&stage.join(name))
            .map_err(|e| format!("{name} is missing from the archive: {e}"))?;
        if Some(got.as_str()) != sum.as_str() {
            return Err(format!(
                "checksum mismatch for {name}; the archive is damaged"
            ));
        }
    }
    let replaced = root
        .join("backups")
        .join(format!("replaced-{}", format_file_timestamp(now)));
    fs::create_dir_all(&replaced)
        .map_err(|e| format!("cannot create {}: {e}", replaced.display()))?;
    for f in ["data.db", "data.db-wal", "data.db-shm"] {
        let p = root.join(f);
        if p.exists() {
            fs::rename(&p, replaced.join(f))
                .map_err(|e| format!("cannot move {}: {e}", p.display()))?;
        }
    }
    let includes_storage = manifest["includes_storage"].as_bool().unwrap_or(false);
    if includes_storage && root.join("storage").exists() {
        fs::rename(root.join("storage"), replaced.join("storage"))
            .map_err(|e| format!("cannot move storage/: {e}"))?;
    }
    fs::rename(stage.join("data.db"), root.join("data.db"))
        .map_err(|e| format!("cannot place data.db: {e}"))?;
    if includes_storage {
        let staged = stage.join("storage");
        if staged.exists() {
            fs::rename(&staged, root.join("storage"))
                .map_err(|e| format!("cannot place storage/: {e}"))?;
        } else {
            fs::create_dir_all(root.join("storage"))
                .map_err(|e| format!("cannot create storage/: {e}"))?;
        }
    }
    let _ = fs::remove_dir_all(&stage);
    Ok(Restored {
        replaced,
        files: files.len(),
        manifest,
    })
}
