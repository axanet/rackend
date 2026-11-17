//! `rackend-blob-fs`: the `BlobStore` port on the local filesystem
//! (SPEC §12.2).
//!
//! Keys map to paths under the root. Every write goes to a temporary file
//! first, is fsynced and then atomically renamed into place, so readers
//! never see partial files.

use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use rackend_core::domain::files::is_safe_key;
use rackend_core::ports::{
    BlobChunks, BlobError, BlobListItem, BlobMeta, BlobReader, BlobStore, BoxFuture, ByteRange,
};
use time::OffsetDateTime;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

const CHUNK: usize = 64 * 1024;

/// Blob storage in a directory.
#[derive(Debug, Clone)]
pub struct FsBlobStore {
    root: PathBuf,
    tmp: PathBuf,
}

fn io(e: &std::io::Error) -> BlobError {
    if e.kind() == std::io::ErrorKind::NotFound {
        BlobError::NotFound
    } else {
        BlobError::Backend(e.to_string())
    }
}

fn modified(m: &std::fs::Metadata) -> Option<OffsetDateTime> {
    m.modified().ok().map(OffsetDateTime::from)
}

impl FsBlobStore {
    /// A store under `root`; `tmp` must be on the same filesystem (renames).
    pub fn new(root: impl Into<PathBuf>, tmp: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            tmp: tmp.into(),
        }
    }

    fn path(&self, key: &str) -> Result<PathBuf, BlobError> {
        if is_safe_key(key) {
            Ok(self.root.join(key))
        } else {
            Err(BlobError::InvalidKey(key.to_owned()))
        }
    }

    /// Writes `src` (a file or bytes) to a temp file, fsyncs it, renames it
    /// to `key` and fsyncs the directory.
    async fn write_atomic(&self, key: &str, src: Source<'_>) -> Result<(), BlobError> {
        let dest = self.path(key)?;
        let parent = dest
            .parent()
            .ok_or_else(|| BlobError::InvalidKey(key.to_owned()))?
            .to_path_buf();
        tokio::fs::create_dir_all(&parent)
            .await
            .map_err(|e| io(&e))?;
        tokio::fs::create_dir_all(&self.tmp)
            .await
            .map_err(|e| io(&e))?;
        let tmp = self.tmp.join(format!("blob-{}", uuid::Uuid::now_v7()));
        let result = async {
            let mut out = tokio::fs::File::create(&tmp).await.map_err(|e| io(&e))?;
            match src {
                Source::File(p) => {
                    let mut input = tokio::fs::File::open(p).await.map_err(|e| io(&e))?;
                    tokio::io::copy(&mut input, &mut out)
                        .await
                        .map_err(|e| io(&e))?;
                }
                Source::Bytes(b) => out.write_all(&b).await.map_err(|e| io(&e))?,
            }
            out.flush().await.map_err(|e| io(&e))?;
            out.sync_all().await.map_err(|e| io(&e))?;
            drop(out);
            tokio::fs::rename(&tmp, &dest).await.map_err(|e| io(&e))?;
            sync_dir(&parent).await;
            Ok(())
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(&tmp).await;
        }
        result
    }
}

enum Source<'a> {
    File(&'a Path),
    Bytes(Vec<u8>),
}

/// Makes a rename durable (best effort; not all platforms allow it).
async fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(d) = tokio::fs::File::open(dir).await {
        let _ = d.sync_all().await;
    }
    #[cfg(not(unix))]
    let _ = dir;
}

struct FileChunks {
    file: tokio::fs::File,
    remaining: u64,
}

impl BlobChunks for FileChunks {
    fn next_chunk(&mut self) -> BoxFuture<'_, Option<Result<Vec<u8>, BlobError>>> {
        Box::pin(async move {
            if self.remaining == 0 {
                return None;
            }
            let want = usize::try_from(self.remaining.min(CHUNK as u64)).unwrap_or(CHUNK);
            let mut buf = vec![0u8; want];
            match self.file.read(&mut buf).await {
                Ok(0) => None,
                Ok(n) => {
                    buf.truncate(n);
                    self.remaining -= n as u64;
                    Some(Ok(buf))
                }
                Err(e) => Some(Err(io(&e))),
            }
        })
    }
}

/// Walks `dir` recursively, collecting files as keys relative to `root`.
fn walk(root: &Path, dir: &Path, prefix: &str, out: &mut Vec<BlobListItem>) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let ft = entry.file_type()?;
        if ft.is_dir() {
            walk(root, &path, prefix, out)?;
        } else if ft.is_file() {
            let Ok(rel) = path.strip_prefix(root) else {
                continue;
            };
            let key = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            if key.starts_with(prefix) {
                let m = entry.metadata()?;
                out.push(BlobListItem {
                    key,
                    meta: BlobMeta {
                        size: m.len(),
                        last_modified: modified(&m),
                    },
                });
            }
        }
    }
    Ok(())
}

impl BlobStore for FsBlobStore {
    fn name(&self) -> &'static str {
        "fs"
    }

    fn put_file<'a>(
        &'a self,
        key: &'a str,
        path: &'a Path,
        _content_type: &'a str,
    ) -> BoxFuture<'a, Result<(), BlobError>> {
        Box::pin(self.write_atomic(key, Source::File(path)))
    }

    fn put_bytes<'a>(
        &'a self,
        key: &'a str,
        bytes: Vec<u8>,
        _content_type: &'a str,
    ) -> BoxFuture<'a, Result<(), BlobError>> {
        Box::pin(self.write_atomic(key, Source::Bytes(bytes)))
    }

    fn get<'a>(
        &'a self,
        key: &'a str,
        range: Option<ByteRange>,
    ) -> BoxFuture<'a, Result<BlobReader, BlobError>> {
        Box::pin(async move {
            let path = self.path(key)?;
            let mut file = tokio::fs::File::open(&path).await.map_err(|e| io(&e))?;
            let size = file.metadata().await.map_err(|e| io(&e))?.len();
            let remaining = match range {
                Some(r) => {
                    if r.start > r.end || r.end >= size {
                        return Err(BlobError::RangeNotSatisfiable);
                    }
                    file.seek(SeekFrom::Start(r.start))
                        .await
                        .map_err(|e| io(&e))?;
                    r.len()
                }
                None => size,
            };
            Ok(BlobReader {
                size,
                range,
                chunks: Box::new(FileChunks { file, remaining }),
            })
        })
    }

    fn head<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<BlobMeta>, BlobError>> {
        Box::pin(async move {
            let path = self.path(key)?;
            match tokio::fs::metadata(&path).await {
                Ok(m) if m.is_file() => Ok(Some(BlobMeta {
                    size: m.len(),
                    last_modified: modified(&m),
                })),
                Ok(_) => Ok(None),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(io(&e)),
            }
        })
    }

    fn delete<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<(), BlobError>> {
        Box::pin(async move {
            let path = self.path(key)?;
            match tokio::fs::remove_file(&path).await {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(e) => return Err(io(&e)),
            }
            // Remove now-empty parent directories up to the root.
            let mut dir = path.parent().map(Path::to_path_buf);
            while let Some(d) = dir {
                if d == self.root || tokio::fs::remove_dir(&d).await.is_err() {
                    break;
                }
                dir = d.parent().map(Path::to_path_buf);
            }
            Ok(())
        })
    }

    fn list<'a>(&'a self, prefix: &'a str) -> BoxFuture<'a, Result<Vec<BlobListItem>, BlobError>> {
        Box::pin(async move {
            let root = self.root.clone();
            // Start at the deepest directory the prefix names.
            let start = match prefix.rsplit_once('/') {
                Some((dir, _)) if is_safe_key(dir) => root.join(dir),
                Some(_) => return Err(BlobError::InvalidKey(prefix.to_owned())),
                None => root.clone(),
            };
            let prefix = prefix.to_owned();
            tokio::task::spawn_blocking(move || {
                let mut out = Vec::new();
                walk(&root, &start, &prefix, &mut out).map_err(|e| io(&e))?;
                out.sort_by(|a, b| a.key.cmp(&b.key));
                Ok(out)
            })
            .await
            .map_err(|e| BlobError::Backend(e.to_string()))?
        })
    }

    fn presign_get<'a>(
        &'a self,
        _key: &'a str,
        _ttl: Duration,
    ) -> BoxFuture<'a, Result<Option<String>, BlobError>> {
        Box::pin(async { Ok(None) })
    }
}

/// Seconds since a blob was modified, for tests and diagnostics.
pub fn age(meta: &BlobMeta) -> Option<Duration> {
    let m = meta.last_modified?;
    SystemTime::from(m).elapsed().ok()
}
