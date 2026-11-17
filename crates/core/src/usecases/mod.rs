//! Application use cases. Every adapter (HTTP today, gRPC/MCP later) calls
//! these and nothing else (SPEC §4.2).

pub mod audit;
pub mod auth;
pub mod broker;
pub mod collections;
pub mod compiled;
pub mod ctx;
pub mod email_auth;
pub mod files;
pub mod first_boot;
pub mod jobs;
pub mod mail;
pub mod observe;
pub mod openapi;
pub mod passkeys;
pub mod realtime;
pub mod records;
pub mod secrets;
pub mod sessions;
pub mod settings;

use std::sync::{Arc, RwLock};

use tokio::sync::Semaphore;

use crate::Result;
use crate::domain::Schema;
use crate::ports::{
    Backups, BlobStore, BlobStoreFactory, Clock, HostMetrics, IdpClient, ImageProcessor, LogsStore,
    Mailer, MailerFactory, Spawner, Store, StoreExt,
};
use crate::secrets::SecretBox;
use crate::settings::Settings;

pub use compiled::{CompiledSchema, SchemaCache};
pub use ctx::{Principal, RequestCtx};

/// The application: ports plus in-memory caches.
#[derive(Debug)]
pub struct App {
    /// Database.
    pub store: Arc<dyn Store>,
    /// Background tasks and blocking work.
    pub spawner: Arc<dyn Spawner>,
    /// Time source.
    pub clock: Arc<dyn Clock>,
    /// Schema cache with compiled rules.
    pub schema: SchemaCache,
    settings: RwLock<Arc<Settings>>,
    /// Session cache, refresh grace window (SPEC §11.5).
    pub sessions: auth::SessionCache,
    /// Bounds concurrent password hashing (SPEC §11.2).
    pub hash_permits: Semaphore,
    /// Binary version.
    pub version: &'static str,
    /// `RACKEND_PUBLIC_URL`: overrides `app.public_url` (SPEC §5.2).
    pub public_url_override: Option<String>,
    /// Encrypts secrets at rest (SPEC §17.4).
    pub secrets: SecretBox,
    /// WebAuthn ceremony state (5 min TTL, SPEC §11.3).
    pub(crate) ceremonies: passkeys::Ceremonies,
    /// Builds blob stores from the storage settings.
    pub(crate) blob_factory: Arc<dyn BlobStoreFactory>,
    /// The current blob store (rebuilt when the storage settings change).
    pub(crate) blobs: RwLock<Arc<dyn BlobStore>>,
    /// Image probing, normalization and thumbnails.
    pub images: Arc<dyn ImageProcessor>,
    /// Thumbnail single flight and concurrency.
    pub(crate) thumbs: files::Thumbs,
    /// A cleanup of removed files is running.
    pub(crate) file_cleanup_running: std::sync::atomic::AtomicBool,
    /// The logs database.
    pub logs: Arc<dyn LogsStore>,
    /// Host and process metrics.
    pub host: Arc<dyn HostMetrics>,
    /// Backup archives.
    pub backups: Arc<dyn Backups>,
    /// Request-log buffer, counters, scheduler state.
    pub observe: observe::ObserveState,
    /// Realtime hub (SPEC §13).
    pub hub: crate::realtime::Hub,
    /// Enabled external providers.
    pub(crate) providers: broker::ProviderCache,
    /// Builds mailers from the `smtp` settings.
    pub(crate) mailer_factory: Arc<dyn MailerFactory>,
    /// The current mailer, `None` while SMTP is not configured.
    pub(crate) mailer: RwLock<Option<Arc<dyn Mailer>>>,
    /// External identity providers (SPEC §11.7).
    pub idp: Arc<dyn IdpClient>,
    /// Dev mode: `http://localhost:*` redirect URIs are allowed.
    pub dev_mode: bool,
    /// This app, for background tasks spawned from `&self` methods.
    pub(crate) me: std::sync::Weak<App>,
}

/// Startup configuration of the app.
#[derive(Debug, Clone)]
pub struct AppConfig {
    /// Binary version.
    pub version: &'static str,
    /// `RACKEND_PUBLIC_URL`.
    pub public_url_override: Option<String>,
    /// The secrets box (see [`secrets::init`]).
    pub secrets: SecretBox,
    /// Builds blob stores (filesystem, S3).
    pub blob_factory: Arc<dyn BlobStoreFactory>,
    /// Image processing.
    pub images: Arc<dyn ImageProcessor>,
    /// The logs database.
    pub logs: Arc<dyn LogsStore>,
    /// Host metrics.
    pub host: Arc<dyn HostMetrics>,
    /// Backups.
    pub backups: Arc<dyn Backups>,
    /// Builds mailers (SMTP).
    pub mailer_factory: Arc<dyn MailerFactory>,
    /// External identity providers.
    pub idp: Arc<dyn IdpClient>,
    /// `RACKEND_DEV`.
    pub dev_mode: bool,
}

impl App {
    /// Builds the app and loads settings and the schema from the store.
    pub async fn load(
        store: Arc<dyn Store>,
        spawner: Arc<dyn Spawner>,
        clock: Arc<dyn Clock>,
        cfg: AppConfig,
    ) -> Result<Arc<Self>> {
        let cpus = std::thread::available_parallelism().map_or(4, |n| n.get());
        let unopened: Arc<dyn BlobStore> =
            Arc::new(files::UnavailableBlobs("storage not opened yet".into()));
        let app = Arc::new_cyclic(|me| Self {
            store,
            spawner,
            clock,
            schema: SchemaCache::default(),
            settings: RwLock::new(Arc::new(Settings::default())),
            sessions: auth::SessionCache::default(),
            hash_permits: Semaphore::new(cpus),
            version: cfg.version,
            public_url_override: cfg.public_url_override,
            secrets: cfg.secrets,
            ceremonies: passkeys::Ceremonies::default(),
            blob_factory: cfg.blob_factory,
            blobs: RwLock::new(unopened),
            images: cfg.images,
            thumbs: files::Thumbs::default(),
            file_cleanup_running: std::sync::atomic::AtomicBool::new(false),
            logs: cfg.logs,
            host: cfg.host,
            backups: cfg.backups,
            observe: observe::ObserveState::default(),
            hub: crate::realtime::Hub::default(),
            mailer_factory: cfg.mailer_factory,
            mailer: RwLock::new(None),
            providers: broker::ProviderCache::default(),
            idp: cfg.idp,
            dev_mode: cfg.dev_mode,
            me: me.clone(),
        });
        app.reload_settings().await?;
        app.rebuild_blobs();
        app.rebuild_mailer();
        app.reload_providers().await?;
        app.realtime_start();
        app.observe_start();
        app.reload_schema().await?;
        Ok(app)
    }

    /// Re-reads collections and recompiles all rules.
    pub async fn reload_schema(&self) -> Result<()> {
        let collections = self.store.read(|tx| tx.collections_list()).await?;
        let version = self.schema.get().schema.version + 1;
        self.schema.set(Schema::new(collections, version));
        Ok(())
    }

    /// Re-reads settings.
    pub async fn reload_settings(&self) -> Result<()> {
        let rows = self.store.read(|tx| tx.settings_list()).await?;
        let mut s = Settings::from_rows(&rows);
        if let Some(url) = &self.public_url_override {
            s.set("app", "public_url", serde_json::Value::String(url.clone()));
        }
        let timeout = u64::try_from(s.int("limits", "rule_timeout_ms", 2000)).unwrap_or(2000);
        self.store.set_query_timeout(timeout);
        let s = Arc::new(s);
        match self.settings.write() {
            Ok(mut g) => *g = s,
            Err(p) => *p.into_inner() = s,
        }
        Ok(())
    }

    /// Current settings.
    pub fn settings(&self) -> Arc<Settings> {
        self.settings
            .read()
            .map_or_else(|p| Arc::clone(&p.into_inner()), |g| Arc::clone(&g))
    }

    /// Runs CPU-heavy work off the async threads (password hashing).
    pub async fn blocking<T: Send + 'static>(
        &self,
        f: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T> {
        let _permit = self.hash_permits.acquire().await.map_err(|_| {
            crate::Error::unavailable(crate::error::UnavailableReason::ShuttingDown)
        })?;
        let out = self
            .spawner
            .run_blocking(Box::new(move || Box::new(f()) as crate::ports::AnyBox))
            .await;
        out.downcast::<T>()
            .map(|b| *b)
            .map_err(|_| crate::Error::internal("blocking task returned an unexpected type"))
    }
}
