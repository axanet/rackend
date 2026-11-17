//! Wires adapters to the core.

use std::sync::Arc;

use rackend_core::ports::{Clock, SystemClock};
use rackend_core::usecases::{App, AppConfig, secrets};
use rackend_http::{AppState, HttpConfig, HttpOptions};
use rackend_store_sqlite::SqliteStore;

use crate::spawner::TokioSpawner;
use crate::ui_assets::EmbeddedUi;

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
}

/// Loads the application on top of an opened store: secrets, settings,
/// schema, then the first-boot steps (instance id, default `users`
/// collection). Signing keys are generated separately
/// ([`App::ensure_signing_keys`]) because RSA key generation is slow.
pub async fn app(store: SqliteStore, opts: AppOptions) -> Result<Arc<App>, String> {
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
