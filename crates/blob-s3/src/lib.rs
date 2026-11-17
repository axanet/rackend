//! `rackend-blob-s3`: the `BlobStore` port on S3-compatible object storage
//! (SPEC §12.2) via `object_store`. Tested against MinIO and Garage.
//!
//! Path-style addressing is the default (MinIO, Garage, RustFS). Uploads
//! larger than 16 MB use multipart uploads.

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt as _;
use futures_util::stream::BoxStream;
use object_store::aws::{AmazonS3, AmazonS3Builder};
use object_store::buffered::BufWriter;
use object_store::path::Path as ObjectPath;
use object_store::signer::{Method, Signer as _};
use object_store::{GetOptions, GetRange, ObjectStore, ObjectStoreExt as _, PutPayload};
use rackend_core::domain::files::is_safe_key;
use rackend_core::ports::{
    BlobChunks, BlobError, BlobListItem, BlobMeta, BlobReader, BlobStore, BoxFuture, ByteRange,
    S3Config,
};
use time::OffsetDateTime;
use tokio::io::AsyncWriteExt as _;

/// Multipart threshold and part size.
const PART: usize = 16 * 1024 * 1024;

/// Blob storage in an S3 bucket.
#[derive(Debug, Clone)]
pub struct S3BlobStore {
    store: Arc<AmazonS3>,
    prefix: String,
}

fn err(e: object_store::Error) -> BlobError {
    match e {
        object_store::Error::NotFound { .. } => BlobError::NotFound,
        other => BlobError::Backend(other.to_string()),
    }
}

fn ts(dt: &object_store::ObjectMeta) -> Option<OffsetDateTime> {
    let t = dt.last_modified;
    OffsetDateTime::from_unix_timestamp(t.timestamp())
        .ok()
        .map(|o| o + time::Duration::nanoseconds(i64::from(t.timestamp_subsec_nanos())))
}

impl S3BlobStore {
    /// Connects lazily: nothing is sent until the first call.
    pub fn new(cfg: &S3Config) -> Result<Self, BlobError> {
        if cfg.bucket.is_empty() {
            return Err(BlobError::Backend("storage.s3.bucket is empty".into()));
        }
        if !cfg.prefix.is_empty() && !is_safe_key(&cfg.prefix) {
            return Err(BlobError::Backend(
                "storage.s3.prefix is not a relative path".into(),
            ));
        }
        let mut b = AmazonS3Builder::new()
            .with_bucket_name(&cfg.bucket)
            .with_region(if cfg.region.is_empty() {
                "us-east-1"
            } else {
                &cfg.region
            })
            .with_access_key_id(&cfg.access_key_id)
            .with_secret_access_key(&cfg.secret_access_key)
            .with_virtual_hosted_style_request(!cfg.force_path_style)
            .with_allow_http(cfg.allow_http)
            // Fail within seconds when the backend is down, not minutes.
            .with_retry(object_store::RetryConfig {
                max_retries: 3,
                retry_timeout: Duration::from_secs(15),
                ..object_store::RetryConfig::default()
            });
        if !cfg.endpoint.is_empty() {
            b = b.with_endpoint(&cfg.endpoint);
        }
        let store = b.build().map_err(|e| BlobError::Backend(e.to_string()))?;
        Ok(Self {
            store: Arc::new(store),
            prefix: cfg.prefix.trim_matches('/').to_owned(),
        })
    }

    fn path(&self, key: &str) -> Result<ObjectPath, BlobError> {
        if !is_safe_key(key) {
            return Err(BlobError::InvalidKey(key.to_owned()));
        }
        Ok(if self.prefix.is_empty() {
            ObjectPath::from(key)
        } else {
            ObjectPath::from(format!("{}/{key}", self.prefix))
        })
    }

    fn key_of(&self, p: &ObjectPath) -> String {
        let s = p.as_ref();
        if self.prefix.is_empty() {
            s.to_owned()
        } else {
            s.strip_prefix(&format!("{}/", self.prefix))
                .unwrap_or(s)
                .to_owned()
        }
    }
}

struct StreamChunks(BoxStream<'static, object_store::Result<bytes::Bytes>>);

impl BlobChunks for StreamChunks {
    fn next_chunk(&mut self) -> BoxFuture<'_, Option<Result<Vec<u8>, BlobError>>> {
        Box::pin(async move {
            self.0
                .next()
                .await
                .map(|r| r.map(|b| b.to_vec()).map_err(err))
        })
    }
}

impl BlobStore for S3BlobStore {
    fn name(&self) -> &'static str {
        "s3"
    }

    fn put_file<'a>(
        &'a self,
        key: &'a str,
        path: &'a std::path::Path,
        _content_type: &'a str,
    ) -> BoxFuture<'a, Result<(), BlobError>> {
        Box::pin(async move {
            let location = self.path(key)?;
            let mut file = tokio::fs::File::open(path)
                .await
                .map_err(|e| BlobError::Backend(e.to_string()))?;
            let store: Arc<dyn ObjectStore> = self.store.clone();
            let mut w = BufWriter::with_capacity(store, location, PART);
            let copied = tokio::io::copy(&mut file, &mut w).await;
            let done = match copied {
                Ok(_) => w.shutdown().await,
                Err(e) => Err(e),
            };
            if let Err(e) = done {
                let _ = w.abort().await;
                return Err(BlobError::Backend(e.to_string()));
            }
            Ok(())
        })
    }

    fn put_bytes<'a>(
        &'a self,
        key: &'a str,
        bytes: Vec<u8>,
        _content_type: &'a str,
    ) -> BoxFuture<'a, Result<(), BlobError>> {
        Box::pin(async move {
            let location = self.path(key)?;
            self.store
                .put(&location, PutPayload::from(bytes))
                .await
                .map(drop)
                .map_err(err)
        })
    }

    fn get<'a>(
        &'a self,
        key: &'a str,
        range: Option<ByteRange>,
    ) -> BoxFuture<'a, Result<BlobReader, BlobError>> {
        Box::pin(async move {
            let location = self.path(key)?;
            let opts = GetOptions {
                range: range.map(|r| GetRange::Bounded(r.start..r.end + 1)),
                ..GetOptions::default()
            };
            let res = self
                .store
                .get_opts(&location, opts)
                .await
                .map_err(|e| match e {
                    object_store::Error::NotFound { .. } => BlobError::NotFound,
                    other
                        if range.is_some()
                            && other.to_string().to_ascii_lowercase().contains("range") =>
                    {
                        BlobError::RangeNotSatisfiable
                    }
                    other => BlobError::Backend(other.to_string()),
                })?;
            let size = res.meta.size;
            if let Some(r) = range
                && (r.start > r.end || r.end >= size)
            {
                return Err(BlobError::RangeNotSatisfiable);
            }
            Ok(BlobReader {
                size,
                range,
                chunks: Box::new(StreamChunks(res.into_stream())),
            })
        })
    }

    fn head<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<BlobMeta>, BlobError>> {
        Box::pin(async move {
            let location = self.path(key)?;
            match self.store.head(&location).await {
                Ok(m) => Ok(Some(BlobMeta {
                    size: m.size,
                    last_modified: ts(&m),
                })),
                Err(object_store::Error::NotFound { .. }) => Ok(None),
                Err(e) => Err(err(e)),
            }
        })
    }

    fn delete<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<(), BlobError>> {
        Box::pin(async move {
            let location = self.path(key)?;
            match self.store.delete(&location).await {
                Ok(()) | Err(object_store::Error::NotFound { .. }) => Ok(()),
                Err(e) => Err(err(e)),
            }
        })
    }

    fn list<'a>(&'a self, prefix: &'a str) -> BoxFuture<'a, Result<Vec<BlobListItem>, BlobError>> {
        Box::pin(async move {
            // object_store lists by path segments: list the deepest
            // directory of the prefix, then filter by the full prefix.
            let dir = prefix.rsplit_once('/').map_or("", |(d, _)| d);
            let base = match (self.prefix.is_empty(), dir.is_empty()) {
                (true, true) => None,
                (true, false) => Some(ObjectPath::from(dir)),
                (false, true) => Some(ObjectPath::from(self.prefix.as_str())),
                (false, false) => Some(ObjectPath::from(format!("{}/{dir}", self.prefix))),
            };
            let mut stream = self.store.list(base.as_ref());
            let mut out = Vec::new();
            while let Some(item) = stream.next().await {
                let m = item.map_err(err)?;
                let key = self.key_of(&m.location);
                if key.starts_with(prefix) {
                    out.push(BlobListItem {
                        key,
                        meta: BlobMeta {
                            size: m.size,
                            last_modified: ts(&m),
                        },
                    });
                }
            }
            out.sort_by(|a, b| a.key.cmp(&b.key));
            Ok(out)
        })
    }

    fn presign_get<'a>(
        &'a self,
        key: &'a str,
        ttl: Duration,
    ) -> BoxFuture<'a, Result<Option<String>, BlobError>> {
        Box::pin(async move {
            let location = self.path(key)?;
            self.store
                .signed_url(Method::GET, &location, ttl)
                .await
                .map(|u| Some(u.to_string()))
                .map_err(err)
        })
    }
}
