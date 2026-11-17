//! Wires adapters to the core.

use std::sync::Arc;

use rackend_core::Result;
use rackend_core::ports::SystemClock;
use rackend_core::usecases::App;
use rackend_http::{AppState, HttpConfig};
use rackend_store_sqlite::SqliteStore;

use crate::spawner::TokioSpawner;
use crate::ui_assets::EmbeddedUi;

/// Loads the application on top of an opened store.
pub async fn app(
    store: SqliteStore,
    version: &'static str,
    public_url: Option<String>,
) -> Result<Arc<App>> {
    App::load(
        Arc::new(store),
        Arc::new(TokioSpawner),
        Arc::new(SystemClock),
        version,
        public_url,
    )
    .await
}

/// The HTTP router for `app`.
pub fn router(app: Arc<App>, dev: bool) -> axum::Router {
    let kb = u64::try_from(app.settings().int("limits", "max_json_body_kb", 1024)).unwrap_or(1024);
    rackend_http::router(
        AppState::new(app, Some(Arc::new(EmbeddedUi)), dev),
        HttpConfig {
            max_json_body_bytes: kb * 1024,
        },
    )
}
