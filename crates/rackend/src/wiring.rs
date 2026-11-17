//! Wires adapters to the core.

use std::path::PathBuf;
use std::sync::Arc;

use rackend_blob_fs::FsBlobStore;
use rackend_blob_s3::S3BlobStore;
use rackend_core::ports::{
    BlobError, BlobStore, BlobStoreFactory, Clock, StorageConfig, SystemClock,
};
use rackend_core::usecases::{App, AppConfig, secrets};
use rackend_http::{AppState, HttpConfig, HttpOptions};
use rackend_imaging::Imaging;
use rackend_store_sqlite::{SqliteLogs, SqliteStore};

use crate::spawner::TokioSpawner;
use crate::ui_assets::EmbeddedUi;

/// Builds blob stores: `storage/` on disk, or an S3 bucket.
#[derive(Debug, Clone)]
pub struct BlobFactory {
    /// `{data}/storage`.
    pub root: PathBuf,
    /// `{data}/tmp` (same filesystem, for atomic renames).
    pub tmp: PathBuf,
}

impl BlobStoreFactory for BlobFactory {
    fn build(&self, config: &StorageConfig) -> Result<Arc<dyn BlobStore>, BlobError> {
        Ok(match config {
            StorageConfig::Fs => Arc::new(FsBlobStore::new(&self.root, self.tmp.join("blobs"))),
            StorageConfig::S3(s3) => Arc::new(S3BlobStore::new(s3)?),
        })
    }
}

/// Startup options of the application.
#[derive(Debug, Clone, Default)]
pub struct AppOptions {
    /// Binary version.
    pub version: &'static str,
    /// `RACKEND_PUBLIC_URL`.
    pub public_url: Option<String>,
    /// `RACKEND_ENCRYPTION_KEY`.
    pub encryption_key: Option<[u8; 32]>,
    /// `RACKEND_RESET_SECRETS`.
    pub reset_secrets: bool,
    /// The data directory (`storage/` and `tmp/` live there).
    pub data_dir: PathBuf,
}

/// Loads the application on top of an opened store: secrets, settings,
/// schema, then the first-boot steps (instance id, default `users`
/// collection). Signing keys are generated separately
/// ([`App::ensure_signing_keys`]) because RSA key generation is slow.
pub async fn app(
    store: SqliteStore,
    logs: SqliteLogs,
    opts: AppOptions,
) -> Result<Arc<App>, String> {
    let backups =
        crate::backup::FsBackups::new(&opts.data_dir, Some(store.db().clone()), opts.version);
    let store: Arc<dyn rackend_core::ports::Store> = Arc::new(store);
    let secrets = secrets::init(
        store.as_ref(),
        opts.encryption_key,
        opts.reset_secrets,
        SystemClock.now(),
    )
    .await
    .map_err(|e| e.to_string())?;
    let app = App::load(
        store,
        Arc::new(TokioSpawner),
        Arc::new(SystemClock),
        AppConfig {
            version: opts.version,
            public_url_override: opts.public_url,
            secrets,
            blob_factory: Arc::new(BlobFactory {
                root: opts.data_dir.join("storage"),
                tmp: opts.data_dir.join("tmp"),
            }),
            images: Arc::new(Imaging),
            logs: Arc::new(logs),
            host: Arc::new(rackend_host_metrics::Host::new(&opts.data_dir)),
            backups: Arc::new(backups),
        },
    )
    .await
    .map_err(|e| format!("cannot load settings and collections: {e}"))?;
    app.first_boot()
        .await
        .map_err(|e| format!("first boot failed: {e}"))?;
    Ok(app)
}

/// The HTTP router for `app`.
pub fn router(app: Arc<App>, opts: HttpOptions) -> axum::Router {
    let kb = u64::try_from(app.settings().int("limits", "max_json_body_kb", 1024)).unwrap_or(1024);
    rackend_http::router(
        AppState::new(app, Some(Arc::new(EmbeddedUi)), opts),
        HttpConfig {
            max_json_body_bytes: kb * 1024,
        },
    )
}
