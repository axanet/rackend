//! Blob storage and image processing ports (SPEC §12.1, §4.3).
//!
//! Only these operations exist, so every S3-compatible backend behaves the
//! same as the filesystem one. The methods return boxed futures so the
//! traits stay object safe (ADR 0001); streams are chunk readers, which keeps
//! `futures` out of the core.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use time::OffsetDateTime;

use super::BoxFuture;
use crate::domain::files::ThumbSpec;

/// A blob storage failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BlobError {
    /// No blob under this key.
    #[error("blob not found")]
    NotFound,
    /// The key is not a safe relative path.
    #[error("invalid blob key: {0}")]
    InvalidKey(String),
    /// The requested range lies outside the blob.
    #[error("range not satisfiable")]
    RangeNotSatisfiable,
    /// The backend failed (network, permissions, disk).
    #[error("storage backend: {0}")]
    Backend(String),
}

/// What `head` and `list` know about a blob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobMeta {
    /// Bytes.
    pub size: u64,
    /// Last modification.
    pub last_modified: Option<OffsetDateTime>,
}

/// One entry of `list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobListItem {
    /// Full key.
    pub key: String,
    /// Size and age.
    pub meta: BlobMeta,
}

/// An inclusive byte range, already resolved against the blob size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    /// First byte.
    pub start: u64,
    /// Last byte (inclusive).
    pub end: u64,
}

impl ByteRange {
    /// Bytes in the range.
    pub fn len(&self) -> u64 {
        self.end - self.start + 1
    }

    /// Always false: a range holds at least one byte.
    pub fn is_empty(&self) -> bool {
        false
    }
}

/// Reads a blob in chunks.
pub trait BlobChunks: Send {
    /// The next chunk, `None` at the end.
    fn next_chunk(&mut self) -> BoxFuture<'_, Option<Result<Vec<u8>, BlobError>>>;
}

/// An opened blob.
pub struct BlobReader {
    /// Size of the whole blob.
    pub size: u64,
    /// The range being read, if any.
    pub range: Option<ByteRange>,
    /// The bytes.
    pub chunks: Box<dyn BlobChunks>,
}

impl std::fmt::Debug for BlobReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlobReader")
            .field("size", &self.size)
            .field("range", &self.range)
            .finish_non_exhaustive()
    }
}

impl BlobReader {
    /// Reads everything into memory, failing above `limit` bytes.
    pub async fn read_all(mut self, limit: u64) -> Result<Vec<u8>, BlobError> {
        let mut out = Vec::new();
        while let Some(chunk) = self.chunks.next_chunk().await {
            out.extend_from_slice(&chunk?);
            if out.len() as u64 > limit {
                return Err(BlobError::Backend(format!(
                    "blob larger than {limit} bytes"
                )));
            }
        }
        Ok(out)
    }
}

/// Object storage (SPEC §12.1).
pub trait BlobStore: Send + Sync + std::fmt::Debug {
    /// Backend name for logs and the settings test (`fs`, `s3`).
    fn name(&self) -> &'static str;

    /// Stores the file at `path` (streaming) under `key`, replacing any blob.
    fn put_file<'a>(
        &'a self,
        key: &'a str,
        path: &'a Path,
        content_type: &'a str,
    ) -> BoxFuture<'a, Result<(), BlobError>>;

    /// Stores `bytes` under `key`, replacing any blob.
    fn put_bytes<'a>(
        &'a self,
        key: &'a str,
        bytes: Vec<u8>,
        content_type: &'a str,
    ) -> BoxFuture<'a, Result<(), BlobError>>;

    /// Opens a blob, optionally a byte range of it.
    fn get<'a>(
        &'a self,
        key: &'a str,
        range: Option<ByteRange>,
    ) -> BoxFuture<'a, Result<BlobReader, BlobError>>;

    /// Size and age of a blob, `None` when it does not exist.
    fn head<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<BlobMeta>, BlobError>>;

    /// Deletes a blob. Deleting a missing blob is not an error.
    fn delete<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<(), BlobError>>;

    /// All blobs whose key starts with `prefix` (`""` for all).
    fn list<'a>(&'a self, prefix: &'a str) -> BoxFuture<'a, Result<Vec<BlobListItem>, BlobError>>;

    /// A URL that downloads the blob directly for `ttl`; `None` when the
    /// backend cannot sign URLs (filesystem).
    fn presign_get<'a>(
        &'a self,
        key: &'a str,
        ttl: Duration,
    ) -> BoxFuture<'a, Result<Option<String>, BlobError>>;
}

/// S3 connection settings, with the secret decrypted.
#[derive(Clone, PartialEq, Eq)]
pub struct S3Config {
    /// Endpoint URL (`https://s3.eu-central-1.amazonaws.com`, `http://minio:9000`).
    pub endpoint: String,
    /// Region.
    pub region: String,
    /// Bucket.
    pub bucket: String,
    /// Key prefix inside the bucket (no leading or trailing slash).
    pub prefix: String,
    /// Access key id.
    pub access_key_id: String,
    /// Secret access key.
    pub secret_access_key: String,
    /// Path-style addressing (MinIO, Garage, RustFS).
    pub force_path_style: bool,
    /// Allow plain http endpoints.
    pub allow_http: bool,
}

impl std::fmt::Debug for S3Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Config")
            .field("endpoint", &self.endpoint)
            .field("region", &self.region)
            .field("bucket", &self.bucket)
            .field("prefix", &self.prefix)
            .field("force_path_style", &self.force_path_style)
            .field("allow_http", &self.allow_http)
            .finish_non_exhaustive()
    }
}

/// Which backend to build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageConfig {
    /// `{data}/storage`.
    Fs,
    /// An S3-compatible bucket.
    S3(S3Config),
}

/// Builds blob stores from the settings (the backend can change at runtime).
pub trait BlobStoreFactory: Send + Sync + std::fmt::Debug {
    /// A store for `config`.
    fn build(&self, config: &StorageConfig) -> Result<Arc<dyn BlobStore>, BlobError>;
}

/// Size of an image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageInfo {
    /// Pixels.
    pub width: u32,
    /// Pixels.
    pub height: u32,
}

/// The result of normalizing an upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedImage {
    /// Size after rotation.
    pub info: ImageInfo,
    /// Bytes written.
    pub size: u64,
    /// Hex SHA-256 of the written file.
    pub sha256: String,
}

/// An image processing failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ImageError {
    /// The bytes are not a decodable image of the claimed type.
    #[error("invalid image: {0}")]
    Invalid(String),
    /// Reading or writing failed.
    #[error("image I/O: {0}")]
    Io(String),
}

/// Image processing (SPEC §12.3, §12.5). Synchronous and CPU bound: the
/// core calls it on the blocking pool.
pub trait ImageProcessor: Send + Sync + std::fmt::Debug {
    /// Reads only the header: dimensions, or `None` if the format is unknown.
    fn probe(&self, path: &Path) -> Result<Option<ImageInfo>, ImageError>;

    /// Applies the EXIF orientation and writes a copy without metadata
    /// (JPEG quality 90, PNG and WebP lossless). GIFs are copied as they are.
    fn normalize(
        &self,
        input: &Path,
        output: &Path,
        mime: &str,
    ) -> Result<NormalizedImage, ImageError>;

    /// A WebP thumbnail (quality 80, alpha kept) of the image in `input`.
    fn thumbnail(&self, input: &[u8], spec: &ThumbSpec) -> Result<Vec<u8>, ImageError>;
}
