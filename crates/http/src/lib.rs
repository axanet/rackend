//! `rackend-http`: the axum HTTP adapter (SPEC §10).
//!
//! Every response that leaves this adapter with an error status is an RFC
//! 9457 problem (see [`problem`] and [`middleware::problem_mapper`]).

mod health;
pub mod html;
pub mod middleware;
pub mod problem;
pub mod ui;

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::http::Method;
use axum::routing::get;
use rackend_core::ports::Store;
use tokio::net::TcpListener;
use tower_http::catch_panic::CatchPanicLayer;

pub use problem::{ApiError, Problem, ProblemCode};
pub use ui::{UiAssets, UiFile};

/// HTTP settings known at startup.
#[derive(Debug, Clone, Copy)]
pub struct HttpConfig {
    /// Max JSON body size in bytes (SPEC §10.1, setting `limits.max_json_body_kb`).
    pub max_json_body_bytes: u64,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            max_json_body_bytes: 1024 * 1024,
        }
    }
}

/// Shared state of all handlers.
#[derive(Debug, Clone)]
pub struct AppState {
    inner: Arc<AppInner>,
}

#[derive(Debug)]
struct AppInner {
    version: &'static str,
    store: Arc<dyn Store>,
    ui: Option<Arc<dyn UiAssets>>,
}

impl AppState {
    /// Builds the state.
    pub fn new(
        version: &'static str,
        store: Arc<dyn Store>,
        ui: Option<Arc<dyn UiAssets>>,
    ) -> Self {
        Self {
            inner: Arc::new(AppInner { version, store, ui }),
        }
    }

    /// The binary version.
    pub fn version(&self) -> &'static str {
        self.inner.version
    }

    /// The store.
    pub fn store(&self) -> &Arc<dyn Store> {
        &self.inner.store
    }

    /// The admin UI bundle, if available.
    pub fn ui(&self) -> Option<&Arc<dyn UiAssets>> {
        self.inner.ui.as_ref()
    }
}

/// One registered route, for the error contract test (SPEC §10.2.6).
#[derive(Debug, Clone)]
pub struct RouteSpec {
    /// HTTP method.
    pub method: Method,
    /// Path template in axum syntax.
    pub path: &'static str,
    /// A concrete path that matches the template.
    pub example: &'static str,
}

/// Every route this adapter registers. Kept next to [`router`]; the contract
/// test fails if a route here is not served.
pub fn routes() -> Vec<RouteSpec> {
    vec![
        RouteSpec {
            method: Method::GET,
            path: "/api/health",
            example: "/api/health",
        },
        RouteSpec {
            method: Method::GET,
            path: "/_",
            example: "/_",
        },
        RouteSpec {
            method: Method::GET,
            path: "/_/",
            example: "/_/",
        },
        RouteSpec {
            method: Method::GET,
            path: "/_/{*path}",
            example: "/_/collections",
        },
    ]
}

/// The full application router with all middleware.
pub fn router(state: AppState, cfg: HttpConfig) -> Router {
    let app = Router::new()
        .route("/api/health", get(health::health))
        .route("/_", get(ui::redirect_to_ui))
        .route("/_/", get(ui::index))
        .route("/_/{*path}", get(ui::asset))
        .fallback(not_found)
        .with_state(state);
    with_middleware(app, cfg)
}

/// Applies the standard middleware stack to `app`. Public so tests can check
/// that framework errors of any handler are mapped to problems.
pub fn with_middleware(app: Router, cfg: HttpConfig) -> Router {
    let max = usize::try_from(cfg.max_json_body_bytes).unwrap_or(usize::MAX);
    // `Router::layer` wraps each endpoint *inside* its method router, so a 405
    // would reach the mapper before axum adds the `Allow` header. Wrapping the
    // whole router as a fallback service puts the middleware outside it.
    let inner = app.layer(DefaultBodyLimit::max(max));
    // Layers run outside-in in the reverse order they are added.
    Router::new()
        .fallback_service(inner)
        .layer(CatchPanicLayer::custom(middleware::panic_response))
        .layer(axum::middleware::from_fn_with_state(
            middleware::ProblemConfig {
                max_body_bytes: cfg.max_json_body_bytes,
            },
            middleware::problem_mapper,
        ))
        .layer(axum::middleware::from_fn(middleware::security_headers))
        .layer(axum::middleware::from_fn(middleware::request_id))
}

async fn not_found() -> ApiError {
    ApiError::new(ProblemCode::NotFound)
}

/// Serves `app` on `listener` until `shutdown` resolves, then stops accepting
/// connections and waits for in-flight requests (the caller bounds the wait,
/// SPEC §5.6).
pub async fn serve(
    listener: TcpListener,
    app: Router,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
}
