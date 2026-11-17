//! `rackend-http`: the axum HTTP adapter (SPEC §10).
//!
//! Every response that leaves this adapter with an error status is an RFC
//! 9457 problem (see [`problem`] and [`middleware::problem_mapper`]).

pub mod api;
pub mod ctx;
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
use axum::routing::{delete, get, patch, post};
use rackend_core::usecases::App;
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
    app: Arc<App>,
    ui: Option<Arc<dyn UiAssets>>,
    dev: bool,
}

impl AppState {
    /// Builds the state.
    pub fn new(app: Arc<App>, ui: Option<Arc<dyn UiAssets>>, dev: bool) -> Self {
        Self {
            inner: Arc::new(AppInner { app, ui, dev }),
        }
    }

    /// The application (use cases).
    pub fn app(&self) -> &Arc<App> {
        &self.inner.app
    }

    /// The binary version.
    pub fn version(&self) -> &'static str {
        self.inner.app.version
    }

    /// The admin UI bundle, if available.
    pub fn ui(&self) -> Option<&Arc<dyn UiAssets>> {
        self.inner.ui.as_ref()
    }

    /// Dev mode (`RACKEND_DEV`).
    pub fn dev(&self) -> bool {
        self.inner.dev
    }

    /// Whether cookies get `Secure`: always, except in dev mode when the
    /// public URL is plain http (or unset).
    pub fn secure_cookies(&self) -> bool {
        let https = self
            .inner
            .app
            .settings()
            .public_url()
            .is_some_and(|u| u.starts_with("https://"));
        !self.inner.dev || https
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
    let r = |method: Method, path: &'static str, example: &'static str| RouteSpec {
        method,
        path,
        example,
    };
    vec![
        r(Method::GET, "/api/health", "/api/health"),
        r(Method::GET, "/api/info", "/api/info"),
        r(Method::POST, "/api/setup", "/api/setup"),
        r(
            Method::POST,
            "/api/superusers/auth-with-password",
            "/api/superusers/auth-with-password",
        ),
        r(
            Method::POST,
            "/api/superusers/auth-refresh",
            "/api/superusers/auth-refresh",
        ),
        r(
            Method::POST,
            "/api/superusers/logout",
            "/api/superusers/logout",
        ),
        r(Method::GET, "/api/superusers", "/api/superusers"),
        r(Method::POST, "/api/superusers", "/api/superusers"),
        r(Method::PATCH, "/api/superusers/{id}", "/api/superusers/x"),
        r(Method::DELETE, "/api/superusers/{id}", "/api/superusers/x"),
        r(Method::GET, "/api/collections", "/api/collections"),
        r(Method::POST, "/api/collections", "/api/collections"),
        r(
            Method::GET,
            "/api/collections/export",
            "/api/collections/export",
        ),
        r(
            Method::POST,
            "/api/collections/import",
            "/api/collections/import",
        ),
        r(
            Method::GET,
            "/api/collections/{c}",
            "/api/collections/posts",
        ),
        r(
            Method::PATCH,
            "/api/collections/{c}",
            "/api/collections/posts",
        ),
        r(
            Method::DELETE,
            "/api/collections/{c}",
            "/api/collections/posts",
        ),
        r(
            Method::GET,
            "/api/collections/{c}/history",
            "/api/collections/posts/history",
        ),
        r(
            Method::GET,
            "/api/collections/{c}/records",
            "/api/collections/posts/records",
        ),
        r(
            Method::POST,
            "/api/collections/{c}/records",
            "/api/collections/posts/records",
        ),
        r(
            Method::GET,
            "/api/collections/{c}/records/{id}",
            "/api/collections/posts/records/x",
        ),
        r(
            Method::PATCH,
            "/api/collections/{c}/records/{id}",
            "/api/collections/posts/records/x",
        ),
        r(
            Method::DELETE,
            "/api/collections/{c}/records/{id}",
            "/api/collections/posts/records/x",
        ),
        r(Method::POST, "/api/batch", "/api/batch"),
        r(Method::GET, "/api/trash", "/api/trash"),
        r(
            Method::POST,
            "/api/trash/{id}/restore",
            "/api/trash/x/restore",
        ),
        r(Method::DELETE, "/api/trash/{id}", "/api/trash/x"),
        r(Method::POST, "/api/rules/check", "/api/rules/check"),
        r(Method::GET, "/api/settings", "/api/settings"),
        r(Method::PATCH, "/api/settings", "/api/settings"),
        r(Method::GET, "/api/audit", "/api/audit"),
        r(Method::GET, "/_", "/_"),
        r(Method::GET, "/_/", "/_/"),
        r(Method::GET, "/_/{*path}", "/_/collections"),
    ]
}

/// The full application router with all middleware.
pub fn router(state: AppState, cfg: HttpConfig) -> Router {
    use api::{admin, auth, collections, records};
    let app = Router::new()
        .route("/api/health", get(health::health))
        .route("/api/info", get(auth::info))
        .route("/api/setup", post(auth::setup))
        .route("/api/superusers/auth-with-password", post(auth::login))
        .route("/api/superusers/auth-refresh", post(auth::refresh))
        .route("/api/superusers/logout", post(auth::logout))
        .route("/api/superusers", get(auth::list).post(auth::create))
        .route(
            "/api/superusers/{id}",
            patch(auth::update).delete(auth::delete),
        )
        .route(
            "/api/collections",
            get(collections::list).post(collections::create),
        )
        .route("/api/collections/export", get(collections::export))
        .route("/api/collections/import", post(collections::import))
        .route(
            "/api/collections/{c}",
            get(collections::view)
                .patch(collections::update)
                .delete(collections::delete),
        )
        .route("/api/collections/{c}/history", get(collections::history))
        .route(
            "/api/collections/{c}/records",
            get(records::list).post(records::create),
        )
        .route(
            "/api/collections/{c}/records/{id}",
            get(records::view)
                .patch(records::update)
                .delete(records::delete),
        )
        .route("/api/batch", post(records::batch))
        .route("/api/trash", get(collections::trash_list))
        .route("/api/trash/{id}/restore", post(collections::trash_restore))
        .route("/api/trash/{id}", delete(collections::trash_purge))
        .route("/api/rules/check", post(collections::rules_check))
        .route(
            "/api/settings",
            get(admin::settings_get).patch(admin::settings_patch),
        )
        .route("/api/audit", get(admin::audit))
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
