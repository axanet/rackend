//! Files (SPEC §12): storing uploads, serving with rules, file tokens,
//! thumbnails, cleanup of removed files and garbage collection.
//!
//! Write order (SPEC §12.3): blobs are stored **before** the database
//! transaction. If the transaction fails, the blobs are deleted right away;
//! the orphan GC is the safety net for crashes in between. Removed files are
//! marked `_files.deleted_at` inside the transaction and their blobs are
//! deleted after commit (the table is the outbox).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use time::OffsetDateTime;
use tokio::sync::Semaphore;

use super::records::ListParams;
use super::sessions::user_principal;
use super::{App, Principal, RequestCtx};
use crate::auth::token::{ct_eq, random_bytes};
use crate::domain::files::{
    self, DEFAULT_THUMB, FileRow, MAX_PIXELS, THUMBS_PREFIX, ThumbSpec, inline_ok, is_image,
    mime_allowed, sniff_mime, storage_key, stored_filename, thumb_key, thumbs_prefix_of,
};
use crate::domain::{Collection, FieldKind};
use crate::error::{FieldError, UnavailableReason, ValidationErrors};
use crate::ports::{
    BlobError, BlobReader, BlobStore, ByteRange, S3Config, StorageConfig, StoreExt,
};
use crate::settings::MASK;
use crate::time::format_timestamp;
use crate::{Error, Result};

/// File tokens live two minutes (SPEC §12.4).
pub const FILE_TOKEN_TTL_S: i64 = 120;
/// Presigned download URLs live 60 seconds.
const PRESIGN_TTL: Duration = Duration::from_secs(60);
/// Orphans younger than this are left alone: their transaction may still run.
pub const ORPHAN_MIN_AGE: Duration = Duration::from_secs(60 * 60);
/// Probe objects of the storage test live under this prefix.
const PROBE_PREFIX: &str = "_rackend-probe/";

impl From<BlobError> for Error {
    fn from(e: BlobError) -> Self {
        match e {
            BlobError::NotFound => Error::NotFound,
            BlobError::InvalidKey(k) => Error::internal(format!("invalid blob key {k}")),
            BlobError::RangeNotSatisfiable => Error::BadRequest("Range not satisfiable.".into()),
            BlobError::Backend(msg) => {
                tracing::error!(error = %msg, "file storage failed");
                Error::unavailable(UnavailableReason::Storage)
            }
        }
    }
}

/// One uploaded file, already streamed to a temporary file by the adapter.
#[derive(Debug, Clone)]
pub struct Upload {
    /// Field name (the multipart part name).
    pub field: String,
    /// The client's filename.
    pub original_name: String,
    /// Temporary file. The adapter deletes its directory afterwards; the
    /// core writes normalized copies next to it.
    pub path: PathBuf,
    /// Bytes.
    pub size: u64,
    /// Hex SHA-256.
    pub sha256: String,
    /// The first bytes, for type sniffing.
    pub head: Vec<u8>,
}

/// What the adapter needs to answer a download.
#[derive(Debug, Clone)]
pub struct FileServe {
    /// Blob key (the file or its thumbnail).
    pub key: String,
    /// Content type.
    pub mime: String,
    /// Bytes.
    pub size: u64,
    /// Strong ETag value (without quotes).
    pub etag: String,
    /// Name for `Content-Disposition`.
    pub download_name: String,
    /// `inline` (else `attachment`).
    pub inline: bool,
    /// Anyone may view the record: `Cache-Control: public`.
    pub public: bool,
    /// Redirect here instead of streaming (S3 `redirect_downloads`).
    pub redirect: Option<String>,
}

/// A download request (SPEC §12.4).
#[derive(Debug, Clone, Copy)]
pub struct FileRequest<'a> {
    /// Collection id or name.
    pub collection: &'a str,
    /// Record id.
    pub record_id: &'a str,
    /// Stored filename.
    pub filename: &'a str,
    /// `?token=` (file token).
    pub token: Option<&'a str>,
    /// `?thumb=WxH[mode]`.
    pub thumb: Option<&'a str>,
    /// `?download=1`.
    pub download: bool,
}

/// Upload limits of a collection.
#[derive(Debug, Clone, Default)]
pub struct UploadLimits {
    /// Per file field: max bytes per file and max files.
    pub fields: HashMap<String, (u64, u32)>,
    /// The largest multipart body.
    pub total: u64,
}

/// Thumbnail generation state: single flight per key, bounded concurrency.
#[derive(Debug)]
pub struct Thumbs {
    flights: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    permits: Semaphore,
    generated: AtomicU64,
}

impl Default for Thumbs {
    fn default() -> Self {
        let cpus = std::thread::available_parallelism().map_or(4, |n| n.get());
        Self {
            flights: Mutex::new(HashMap::new()),
            permits: Semaphore::new(cpus),
            generated: AtomicU64::new(0),
        }
    }
}

impl Thumbs {
    fn flight(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut m = self
            .flights
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::clone(m.entry(key.to_owned()).or_default())
    }

    fn land(&self, key: &str) {
        let mut m = self
            .flights
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        m.remove(key);
    }
}

/// A blob store that fails every call, used when the configured backend
/// cannot be built (bad S3 settings). Files stay unavailable instead of
/// silently going to another backend.
#[derive(Debug)]
pub(crate) struct UnavailableBlobs(pub String);

impl BlobStore for UnavailableBlobs {
    fn name(&self) -> &'static str {
        "unavailable"
    }
    fn put_file<'a>(
        &'a self,
        _: &'a str,
        _: &'a std::path::Path,
        _: &'a str,
    ) -> crate::ports::BoxFuture<'a, std::result::Result<(), BlobError>> {
        Box::pin(async { Err(BlobError::Backend(self.0.clone())) })
    }
    fn put_bytes<'a>(
        &'a self,
        _: &'a str,
        _: Vec<u8>,
        _: &'a str,
    ) -> crate::ports::BoxFuture<'a, std::result::Result<(), BlobError>> {
        Box::pin(async { Err(BlobError::Backend(self.0.clone())) })
    }
    fn get<'a>(
        &'a self,
        _: &'a str,
        _: Option<ByteRange>,
    ) -> crate::ports::BoxFuture<'a, std::result::Result<BlobReader, BlobError>> {
        Box::pin(async { Err(BlobError::Backend(self.0.clone())) })
    }
    fn head<'a>(
        &'a self,
        _: &'a str,
    ) -> crate::ports::BoxFuture<'a, std::result::Result<Option<crate::ports::BlobMeta>, BlobError>>
    {
        Box::pin(async { Err(BlobError::Backend(self.0.clone())) })
    }
    fn delete<'a>(
        &'a self,
        _: &'a str,
    ) -> crate::ports::BoxFuture<'a, std::result::Result<(), BlobError>> {
        Box::pin(async { Err(BlobError::Backend(self.0.clone())) })
    }
    fn list<'a>(
        &'a self,
        _: &'a str,
    ) -> crate::ports::BoxFuture<'a, std::result::Result<Vec<crate::ports::BlobListItem>, BlobError>>
    {
        Box::pin(async { Err(BlobError::Backend(self.0.clone())) })
    }
    fn presign_get<'a>(
        &'a self,
        _: &'a str,
        _: Duration,
    ) -> crate::ports::BoxFuture<'a, std::result::Result<Option<String>, BlobError>> {
        Box::pin(async { Err(BlobError::Backend(self.0.clone())) })
    }
}

/// Eight random `[a-z0-9]` characters.
fn random_suffix() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    random_bytes::<8>()
        .iter()
        .map(|b| ALPHABET[usize::from(*b) % ALPHABET.len()] as char)
        .collect()
}

/// Reads `storage` settings into a backend config. `secret` is the
/// decrypted secret access key.
fn storage_config_from(
    section: &serde_json::Map<String, Value>,
    secret: Option<String>,
) -> StorageConfig {
    if section.get("backend").and_then(Value::as_str) != Some("s3") {
        return StorageConfig::Fs;
    }
    let s3 = section.get("s3").and_then(Value::as_object);
    let s = |k: &str| {
        s3.and_then(|o| o.get(k))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let b = |k: &str, d: bool| {
        s3.and_then(|o| o.get(k))
            .and_then(Value::as_bool)
            .unwrap_or(d)
    };
    StorageConfig::S3(S3Config {
        endpoint: s("endpoint"),
        region: s("region"),
        bucket: s("bucket"),
        prefix: s("prefix"),
        access_key_id: s("access_key_id"),
        secret_access_key: secret.unwrap_or_default(),
        force_path_style: b("force_path_style", true),
        allow_http: b("allow_http", false),
    })
}

impl App {
    /// The current blob store.
    pub fn blobs(&self) -> Arc<dyn BlobStore> {
        self.blobs
            .read()
            .map_or_else(|p| Arc::clone(&p.into_inner()), |g| Arc::clone(&g))
    }

    /// Thumbnails generated since start (tests and metrics).
    pub fn thumb_generations(&self) -> u64 {
        self.thumbs.generated.load(Ordering::Relaxed)
    }

    /// The decrypted value of a secret setting (`storage.s3.secret_access_key`).
    pub(crate) fn secret_setting(&self, section: &str, path: &[&str]) -> Option<String> {
        let settings = self.settings();
        let mut v = settings
            .section(section)
            .map(|s| Value::Object(s.clone()))?;
        for p in path {
            v = v.get(*p)?.clone();
        }
        let s = v.as_str()?;
        if crate::secrets::is_encrypted(s) {
            match self.secrets.decrypt_str(s) {
                Ok(p) => Some(p),
                Err(e) => {
                    tracing::error!(error = %e, section, "cannot decrypt a secret setting");
                    None
                }
            }
        } else {
            Some(s.to_owned())
        }
    }

    /// The storage backend the settings ask for.
    pub fn storage_config(&self) -> StorageConfig {
        let settings = self.settings();
        let Some(section) = settings.section("storage") else {
            return StorageConfig::Fs;
        };
        storage_config_from(
            section,
            self.secret_setting("storage", &["s3", "secret_access_key"]),
        )
    }

    /// Rebuilds the blob store from the settings.
    pub(crate) fn rebuild_blobs(&self) {
        let cfg = self.storage_config();
        let store: Arc<dyn BlobStore> = match self.blob_factory.build(&cfg) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "cannot open the configured file storage; files are unavailable until the storage settings are fixed");
                Arc::new(UnavailableBlobs(e.to_string()))
            }
        };
        match self.blobs.write() {
            Ok(mut g) => *g = store,
            Err(p) => *p.into_inner() = store,
        }
    }

    /// Validates uploads, normalizes images and stores the blobs. Returns
    /// the `_files` rows to insert in the record transaction. On error, the
    /// blobs already stored are deleted again.
    pub(crate) async fn store_uploads(
        &self,
        c: &Collection,
        record_id: &str,
        uploads: Vec<Upload>,
        now: OffsetDateTime,
    ) -> Result<Vec<FileRow>> {
        let mut errs = ValidationErrors::default();
        let mut media = ValidationErrors::default();
        let mut per_field: BTreeMap<String, usize> = BTreeMap::new();
        for u in &uploads {
            *per_field.entry(u.field.clone()).or_default() += 1;
        }
        for (name, n) in &per_field {
            match c.field(name).map(|f| &f.kind) {
                Some(FieldKind::File { max_select, .. }) if *max_select == 1 && *n > 1 => {
                    errs.add(
                        name.clone(),
                        FieldError::new("too_many", "Upload one file.").param("max", 1),
                    );
                }
                Some(FieldKind::File { .. }) => {}
                Some(_) => errs.add(
                    name.clone(),
                    FieldError::new("not_a_file_field", "This field does not take files."),
                ),
                None => errs.add(
                    name.clone(),
                    FieldError::new("unknown_field", "Unknown field."),
                ),
            }
        }
        std::mem::take(&mut errs).into_result()?;

        let mut rows = Vec::with_capacity(uploads.len());
        let mut stored: Vec<String> = Vec::new();
        let blobs = self.blobs();
        let result: Result<()> = async {
            for u in uploads {
                let Some(field) = c.field(&u.field) else {
                    continue;
                };
                let FieldKind::File {
                    max_size,
                    mime_types,
                    strip_metadata,
                    ..
                } = &field.kind
                else {
                    continue;
                };
                if u.size > *max_size {
                    return Err(Error::PayloadTooLarge {
                        max_bytes: *max_size,
                    });
                }
                let mime = sniff_mime(&u.head, &u.original_name);
                if !mime_allowed(mime, mime_types) {
                    media.add(
                        u.field.clone(),
                        FieldError::new("type_not_allowed", "This file type is not allowed here.")
                            .param("type", mime)
                            .param("allowed", mime_types.join(", ")),
                    );
                    continue;
                }
                let (mut path, mut size, mut sha256) = (u.path.clone(), u.size, u.sha256.clone());
                let (mut width, mut height) = (None, None);
                if is_image(mime) {
                    let images = Arc::clone(&self.images);
                    let p = u.path.clone();
                    let info = self.blocking(move || images.probe(&p)).await?;
                    let info = match info {
                        Ok(Some(i)) => i,
                        _ => {
                            errs.add(
                                u.field.clone(),
                                FieldError::new("invalid_image", "The image cannot be read."),
                            );
                            continue;
                        }
                    };
                    if u64::from(info.width) * u64::from(info.height) > MAX_PIXELS {
                        errs.add(
                            u.field.clone(),
                            FieldError::new(
                                "too_many_pixels",
                                "The image is larger than 50 megapixels.",
                            )
                            .param("max", MAX_PIXELS),
                        );
                        continue;
                    }
                    (width, height) = (Some(info.width), Some(info.height));
                    if *strip_metadata && mime != "image/gif" {
                        let images = Arc::clone(&self.images);
                        let (input, output) = (
                            u.path.clone(),
                            PathBuf::from(format!("{}.norm", u.path.display())),
                        );
                        let out2 = output.clone();
                        let m = mime.to_owned();
                        match self
                            .blocking(move || images.normalize(&input, &out2, &m))
                            .await?
                        {
                            Ok(n) => {
                                (path, size, sha256) = (output, n.size, n.sha256);
                                (width, height) = (Some(n.info.width), Some(n.info.height));
                            }
                            Err(e) => {
                                tracing::debug!(error = %e, "image normalization failed");
                                errs.add(
                                    u.field.clone(),
                                    FieldError::new("invalid_image", "The image cannot be read."),
                                );
                                continue;
                            }
                        }
                    }
                }
                if !errs.is_empty() || !media.is_empty() {
                    continue;
                }
                let filename = stored_filename(&u.original_name, mime, &random_suffix());
                let key = storage_key(&c.id.to_string(), record_id, &filename);
                blobs.put_file(&key, &path, mime).await?;
                stored.push(key.clone());
                rows.push(FileRow {
                    id: files::new_file_id(),
                    collection_id: c.id.to_string(),
                    record_id: record_id.to_owned(),
                    field: field.id.to_string(),
                    storage_key: key,
                    original_name: u.original_name.chars().take(255).collect(),
                    size,
                    mime: mime.to_owned(),
                    width,
                    height,
                    sha256,
                    created: format_timestamp(now),
                    deleted_at: None,
                });
            }
            if !media.is_empty() {
                return Err(Error::UnsupportedMediaType(std::mem::take(&mut media)));
            }
            std::mem::take(&mut errs).into_result()
        }
        .await;
        if let Err(e) = result {
            self.delete_blobs(&stored).await;
            return Err(e);
        }
        Ok(rows)
    }

    /// Deletes blobs (and their thumbnails), logging failures. Used when a
    /// transaction failed after the blobs were stored.
    pub(crate) async fn delete_blobs(&self, keys: &[String]) {
        let blobs = self.blobs();
        for k in keys {
            if let Err(e) = blobs.delete(k).await {
                tracing::warn!(error = %e, key = %k, "cannot delete a stored file; the GC will retry");
            }
        }
    }

    /// Runs the cleanup of removed files in the background (after commit).
    pub(crate) fn after_file_changes(&self) {
        let Some(me) = self.me.upgrade() else { return };
        if self.file_cleanup_running.swap(true, Ordering::AcqRel) {
            return;
        }
        self.spawner.spawn(
            "files.cleanup",
            Box::pin(async move {
                if let Err(e) = me.files_cleanup().await {
                    tracing::warn!(error = %e, "file cleanup failed; the GC will retry");
                }
                me.file_cleanup_running.store(false, Ordering::Release);
            }),
        );
    }

    /// Deletes the blobs of files marked deleted, then their rows.
    pub async fn files_cleanup(&self) -> Result<u64> {
        let blobs = self.blobs();
        let mut done = 0;
        loop {
            let rows = self.store.read(|tx| tx.files_deleted(200)).await?;
            if rows.is_empty() {
                return Ok(done);
            }
            let mut gone = Vec::new();
            for r in &rows {
                if delete_with_thumbs(blobs.as_ref(), &r.storage_key)
                    .await
                    .is_ok()
                {
                    gone.push(r.id.clone());
                }
            }
            if gone.is_empty() {
                return Err(Error::unavailable(UnavailableReason::Storage));
            }
            done += gone.len() as u64;
            let n = gone.len();
            self.store.write(move |tx| tx.files_remove(&gone)).await?;
            if n < rows.len() {
                return Ok(done);
            }
        }
    }

    /// Garbage collection (SPEC §12.3): removed files, blobs without a
    /// `_files` row older than `min_age`, and thumbnails of missing files.
    pub async fn files_gc(&self, min_age: Duration) -> Result<Value> {
        let cleaned = self.files_cleanup().await?;
        let blobs = self.blobs();
        let all = blobs.list("").await?;
        let now = self.clock.now();
        let old_enough = |m: &crate::ports::BlobMeta| {
            m.last_modified.is_none_or(|t| {
                (now - t).whole_seconds() >= i64::try_from(min_age.as_secs()).unwrap_or(i64::MAX)
            })
        };
        let (thumbs, originals): (Vec<_>, Vec<_>) = all
            .into_iter()
            .filter(|b| !b.key.starts_with(PROBE_PREFIX))
            .partition(|b| b.key.starts_with(THUMBS_PREFIX));
        let keys: Vec<String> = originals.iter().map(|b| b.key.clone()).collect();
        let known: HashSet<String> = self
            .store
            .read(move |tx| tx.files_known_keys(&keys))
            .await?
            .into_iter()
            .collect();
        let mut orphans = 0u64;
        for b in &originals {
            if !known.contains(&b.key) && old_enough(&b.meta) {
                if let Err(e) = delete_with_thumbs(blobs.as_ref(), &b.key).await {
                    tracing::warn!(error = %e, key = %b.key, "cannot delete an orphaned file");
                } else {
                    orphans += 1;
                }
            }
        }
        // Thumbnails whose original has no live row.
        let thumb_sources: Vec<String> = thumbs
            .iter()
            .filter_map(|t| {
                let rest = t.key.strip_prefix(THUMBS_PREFIX)?;
                rest.rsplit_once('/').map(|(src, _)| src.to_owned())
            })
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let live: HashSet<String> = {
            let srcs = thumb_sources.clone();
            self.store
                .read(move |tx| {
                    let mut out = HashSet::new();
                    for k in &srcs {
                        if tx.file_by_key(k)?.is_some() {
                            out.insert(k.clone());
                        }
                    }
                    Ok(out)
                })
                .await?
        };
        let mut stale_thumbs = 0u64;
        for t in &thumbs {
            let src = t
                .key
                .strip_prefix(THUMBS_PREFIX)
                .and_then(|r| r.rsplit_once('/'))
                .map(|(s, _)| s);
            if src.is_some_and(|s| !live.contains(s))
                && old_enough(&t.meta)
                && blobs.delete(&t.key).await.is_ok()
            {
                stale_thumbs += 1;
            }
        }
        Ok(json!({ "removed": cleaned, "orphans": orphans, "stale_thumbs": stale_thumbs }))
    }

    /// Keeps the thumbnail cache under `storage.thumb_cache_max_mb`,
    /// deleting the oldest thumbnails first.
    pub async fn thumbs_gc(&self) -> Result<u64> {
        let cap = u64::try_from(self.settings().int("storage", "thumb_cache_max_mb", 2048))
            .unwrap_or(2048)
            * 1024
            * 1024;
        let blobs = self.blobs();
        let mut thumbs = blobs.list(THUMBS_PREFIX).await?;
        let mut total: u64 = thumbs.iter().map(|t| t.meta.size).sum();
        if total <= cap {
            return Ok(0);
        }
        thumbs.sort_by_key(|t| t.meta.last_modified);
        let mut deleted = 0;
        for t in thumbs {
            if total <= cap {
                break;
            }
            if blobs.delete(&t.key).await.is_ok() {
                total = total.saturating_sub(t.meta.size);
                deleted += 1;
            }
        }
        Ok(deleted)
    }

    /// `POST /api/files/token`: a short-lived token for protected files.
    pub fn file_token(&self, ctx: &RequestCtx) -> Result<Value> {
        let (kind, collection, id) = match &ctx.principal {
            Principal::Superuser { id, .. } => ("s", "-".to_owned(), id.clone()),
            Principal::User {
                id, collection_id, ..
            } => ("u", collection_id.clone(), id.clone()),
            Principal::Anonymous => return Err(Error::Unauthorized),
        };
        let exp = ctx.now.unix_timestamp() + FILE_TOKEN_TTL_S;
        let payload = format!("{kind}:{collection}:{id}:{exp}");
        let mac = self.secrets.mac("file-token", payload.as_bytes());
        let token = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(payload.as_bytes()),
            URL_SAFE_NO_PAD.encode(mac)
        );
        Ok(json!({ "token": token, "expires_in": FILE_TOKEN_TTL_S }))
    }

    /// The principal a valid file token stands for.
    async fn principal_from_file_token(
        &self,
        token: &str,
        now: OffsetDateTime,
    ) -> Result<Principal> {
        let bad = || Error::Forbidden("The file token is invalid or expired.".into());
        let (p, m) = token.split_once('.').ok_or_else(bad)?;
        let payload = URL_SAFE_NO_PAD.decode(p).map_err(|_| bad())?;
        let mac = URL_SAFE_NO_PAD.decode(m).map_err(|_| bad())?;
        let expected = self.secrets.mac("file-token", &payload);
        let enc = |b: &[u8]| URL_SAFE_NO_PAD.encode(b);
        if !ct_eq(&enc(&mac), &enc(&expected)) {
            return Err(bad());
        }
        let payload = String::from_utf8(payload).map_err(|_| bad())?;
        let parts: Vec<&str> = payload.split(':').collect();
        let [kind, collection, id, exp] = parts.as_slice() else {
            return Err(bad());
        };
        if exp.parse::<i64>().map_err(|_| bad())? < now.unix_timestamp() {
            return Err(bad());
        }
        let (kind, collection, id) = (
            (*kind).to_owned(),
            (*collection).to_owned(),
            (*id).to_owned(),
        );
        let compiled = self.schema.get();
        let principal = self
            .store
            .read(move |tx| -> Result<Option<Principal>> {
                Ok(match kind.as_str() {
                    "s" => tx.superuser_by_id(&id)?.map(|s| Principal::Superuser {
                        id: s.id,
                        email: s.email,
                        session_id: "file-token".into(),
                    }),
                    "u" => match compiled.schema.by_id_or_name(&collection) {
                        Some(c) => tx
                            .record_get(c, &id)?
                            .map(|r| user_principal(c, &r, "file-token")),
                        None => None,
                    },
                    _ => None,
                })
            })
            .await?;
        principal.ok_or_else(bad)
    }

    /// Authorizes a download (SPEC §12.4) and returns how to serve it.
    pub async fn file_serve(&self, ctx: &RequestCtx, req: FileRequest<'_>) -> Result<FileServe> {
        let FileRequest {
            collection: key,
            record_id,
            filename,
            token,
            thumb,
            download,
        } = req;
        if !files::is_stored_filename(filename) {
            return Err(Error::NotFound);
        }
        let mut ctx = ctx.clone();
        let token_ok = match token.filter(|t| !t.is_empty()) {
            Some(t) => {
                ctx.principal = self.principal_from_file_token(t, ctx.now).await?;
                true
            }
            None => false,
        };
        let compiled = self.schema.get();
        let c = compiled
            .schema
            .by_id_or_name(key)
            .ok_or(Error::NotFound)?
            .clone();
        // The view rule decides (404 when not viewable).
        let record = self
            .record_view(&ctx, key, record_id, ListParams::default())
            .await?;
        let field = c
            .fields
            .iter()
            .find(|f| {
                matches!(f.kind, FieldKind::File { .. })
                    && match record.get(&f.name) {
                        Some(Value::String(s)) => s == filename,
                        Some(Value::Array(a)) => a.iter().any(|v| v.as_str() == Some(filename)),
                        _ => false,
                    }
            })
            .ok_or(Error::NotFound)?;
        let FieldKind::File {
            thumbs, protected, ..
        } = &field.kind
        else {
            return Err(Error::NotFound);
        };
        if *protected && !token_ok {
            return Err(Error::Forbidden(
                "This file is protected. Request a file token (POST /api/files/token) and pass it as ?token=.".into(),
            ));
        }
        let skey = storage_key(&c.id.to_string(), record_id, filename);
        let lookup = skey.clone();
        let row = self
            .store
            .read(move |tx| tx.file_by_key(&lookup))
            .await?
            .ok_or(Error::NotFound)?;
        let public = !*protected && c.rules.view.as_deref().is_some_and(|v| v.trim().is_empty());
        let blobs = self.blobs();
        let mut serve = match thumb.filter(|t| !t.is_empty()) {
            None => FileServe {
                key: row.storage_key.clone(),
                mime: row.mime.clone(),
                size: row.size,
                etag: row.sha256.clone(),
                download_name: row.original_name.clone(),
                inline: inline_ok(&row.mime) && !download,
                public,
                redirect: None,
            },
            Some(t) => {
                let spec = ThumbSpec::parse(t)
                    .filter(|s| *s == DEFAULT_THUMB || thumbs.contains(s))
                    .ok_or_else(|| {
                        Error::BadRequest(format!(
                            "Thumb size {t} is not allowed for this field. Allowed: {}.",
                            std::iter::once(DEFAULT_THUMB)
                                .chain(thumbs.iter().copied())
                                .map(|s| s.to_string())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ))
                    })?;
                if !is_image(&row.mime) {
                    return Err(Error::NotFound);
                }
                let (key, size) = self.ensure_thumb(blobs.as_ref(), &row, spec).await?;
                let stem = row
                    .original_name
                    .rsplit_once('.')
                    .map_or(row.original_name.as_str(), |(s, _)| s);
                FileServe {
                    key,
                    mime: "image/webp".into(),
                    size,
                    etag: format!("{}-{spec}", row.sha256),
                    download_name: format!("{stem}_{spec}.webp"),
                    inline: !download,
                    public,
                    redirect: None,
                }
            }
        };
        let redirect = self
            .settings()
            .section("storage")
            .and_then(|s| s.get("s3"))
            .and_then(|s| s.get("redirect_downloads"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if redirect {
            serve.redirect = blobs.presign_get(&serve.key, PRESIGN_TTL).await?;
        }
        Ok(serve)
    }

    /// Opens the bytes of an authorized download.
    pub async fn file_read(
        &self,
        serve: &FileServe,
        range: Option<ByteRange>,
    ) -> Result<BlobReader> {
        Ok(self.blobs().get(&serve.key, range).await?)
    }

    /// Returns the thumbnail key and size, generating it at most once even
    /// under concurrent requests.
    async fn ensure_thumb(
        &self,
        blobs: &dyn BlobStore,
        row: &FileRow,
        spec: ThumbSpec,
    ) -> Result<(String, u64)> {
        let key = thumb_key(&row.storage_key, &spec);
        if let Some(m) = blobs.head(&key).await? {
            return Ok((key, m.size));
        }
        let flight = self.thumbs.flight(&key);
        let _guard = flight.lock().await;
        if let Some(m) = blobs.head(&key).await? {
            return Ok((key, m.size));
        }
        let _permit = self
            .thumbs
            .permits
            .acquire()
            .await
            .map_err(|_| Error::unavailable(UnavailableReason::ShuttingDown))?;
        let bytes = blobs
            .get(&row.storage_key, None)
            .await?
            .read_all(row.size.max(1) * 2)
            .await?;
        let images = Arc::clone(&self.images);
        let out = self
            .blocking(move || images.thumbnail(&bytes, &spec))
            .await?
            .map_err(|e| {
                tracing::warn!(error = %e, key = %row.storage_key, "cannot make a thumbnail");
                Error::NotFound
            })?;
        let size = out.len() as u64;
        blobs.put_bytes(&key, out, "image/webp").await?;
        self.thumbs.generated.fetch_add(1, Ordering::Relaxed);
        self.thumbs.land(&key);
        Ok((key, size))
    }

    /// Upload limits of collection `key`: per file field `(max bytes per
    /// file, max files)`, and the largest multipart body (all fields full
    /// plus 1 MB for the JSON part, SPEC §10.1).
    pub fn upload_limits(&self, key: &str) -> Result<UploadLimits> {
        let compiled = self.schema.get();
        let c = compiled.schema.by_id_or_name(key).ok_or(Error::NotFound)?;
        let mut fields = HashMap::new();
        let mut total: u64 = 1024 * 1024;
        for f in &c.fields {
            if let FieldKind::File {
                max_size,
                max_select,
                ..
            } = &f.kind
            {
                fields.insert(f.name.clone(), (*max_size, *max_select));
                total = total.saturating_add(max_size.saturating_mul(u64::from(*max_select)));
            }
        }
        Ok(UploadLimits { fields, total })
    }

    /// File count and bytes (admin UI usage stats).
    pub async fn files_usage(&self, ctx: &RequestCtx) -> Result<Value> {
        ctx.require_superuser()?;
        let (count, bytes) = self.store.read(|tx| tx.files_usage()).await?;
        Ok(json!({ "files": count, "bytes": bytes, "backend": self.blobs().name() }))
    }

    /// `POST /api/settings/test-storage`: put, head, get and delete a probe
    /// object, with `candidate` (a partial `storage` section) merged over the
    /// saved settings. Nothing is saved.
    pub async fn storage_test(&self, ctx: &RequestCtx, candidate: Option<Value>) -> Result<Value> {
        ctx.require_superuser()?;
        let settings = self.settings();
        let mut section = settings.section("storage").cloned().unwrap_or_default();
        if let Some(c) = candidate.filter(|c| !c.is_null()) {
            let merged = settings
                .validate_patch(&json!({ "storage": c }))
                .map_err(Error::Validation)?;
            if let Some((_, m)) = merged.into_iter().next() {
                section = m;
            }
        }
        let secret = match section
            .get("s3")
            .and_then(|s| s.get("secret_access_key"))
            .and_then(Value::as_str)
        {
            Some(s) if crate::secrets::is_encrypted(s) => self.secrets.decrypt_str(s).ok(),
            Some(s) if s != MASK => Some(s.to_owned()),
            _ => self.secret_setting("storage", &["s3", "secret_access_key"]),
        };
        let cfg = storage_config_from(&section, secret);
        let mut steps = Vec::new();
        let store = match self.blob_factory.build(&cfg) {
            Ok(s) => s,
            Err(e) => {
                steps.push(json!({ "step": "connect", "ok": false, "error": e.to_string() }));
                return Ok(json!({ "ok": false, "backend": backend_name(&cfg), "steps": steps }));
            }
        };
        let key = format!("{PROBE_PREFIX}{}", files::new_file_id());
        let body = b"rackend storage test".to_vec();
        let mut ok = true;
        macro_rules! step {
            ($name:literal, $fut:expr) => {{
                let start = std::time::Instant::now();
                let r = $fut.await;
                let ms = start.elapsed().as_millis() as u64;
                match r {
                    Ok(()) => steps.push(json!({ "step": $name, "ok": true, "ms": ms })),
                    Err(e) => {
                        ok = false;
                        steps.push(json!({ "step": $name, "ok": false, "ms": ms, "error": e }));
                    }
                }
            }};
        }
        step!("put", async {
            store
                .put_bytes(&key, body.clone(), "text/plain")
                .await
                .map_err(|e| e.to_string())
        });
        if ok {
            step!("head", async {
                match store.head(&key).await {
                    Ok(Some(m)) if m.size == body.len() as u64 => Ok(()),
                    Ok(Some(m)) => Err(format!("size {} instead of {}", m.size, body.len())),
                    Ok(None) => Err("the object is missing after put".to_owned()),
                    Err(e) => Err(e.to_string()),
                }
            });
            step!("get", async {
                match store.get(&key, None).await {
                    Ok(r) => match r.read_all(1024).await {
                        Ok(b) if b == body => Ok(()),
                        Ok(_) => Err("the content differs".to_owned()),
                        Err(e) => Err(e.to_string()),
                    },
                    Err(e) => Err(e.to_string()),
                }
            });
            step!("delete", async {
                store.delete(&key).await.map_err(|e| e.to_string())
            });
        }
        Ok(json!({ "ok": ok, "backend": backend_name(&cfg), "steps": steps }))
    }
}

fn backend_name(cfg: &StorageConfig) -> &'static str {
    match cfg {
        StorageConfig::Fs => "fs",
        StorageConfig::S3(_) => "s3",
    }
}

/// Deletes a blob and every thumbnail made from it.
async fn delete_with_thumbs(
    blobs: &dyn BlobStore,
    key: &str,
) -> std::result::Result<(), BlobError> {
    for t in blobs.list(&thumbs_prefix_of(key)).await? {
        blobs.delete(&t.key).await?;
    }
    blobs.delete(key).await
}
