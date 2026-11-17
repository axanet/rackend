//! Application use cases. Every adapter (HTTP today, gRPC/MCP later) calls
//! these and nothing else (SPEC §4.2).

pub mod audit;
pub mod auth;
pub mod collections;
pub mod compiled;
pub mod ctx;
pub mod first_boot;
pub mod jobs;
pub mod passkeys;
pub mod records;
pub mod secrets;
pub mod sessions;
pub mod settings;

use std::sync::{Arc, RwLock};

use tokio::sync::Semaphore;

use crate::Result;
use crate::domain::Schema;
use crate::ports::{Clock, Spawner, Store, StoreExt};
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
        let app = Arc::new(Self {
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
        });
        app.reload_settings().await?;
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
