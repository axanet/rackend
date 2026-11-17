//! `rackend-http`: the axum HTTP adapter (SPEC §10).
//!
//! Every response that leaves this adapter with an error status is an RFC
//! 9457 problem (see [`problem`] and [`middleware::problem_mapper`]).

pub mod api;
pub mod ctx;
mod health;
pub mod html;
pub mod middleware;
pub mod net;
pub mod problem;
pub mod ratelimit;
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
    opts: HttpOptions,
    limits: ratelimit::RateLimits,
}

/// Request-handling options known at startup.
#[derive(Debug, Clone, Default)]
pub struct HttpOptions {
    /// Dev mode (`RACKEND_DEV`).
    pub dev: bool,
    /// `RACKEND_TRUSTED_PROXIES`; when empty, `security.trusted_proxies` applies.
    pub trusted_proxies: Vec<net::Cidr>,
    /// Where uploads are streamed before they are stored (`{data}/tmp`).
    /// Defaults to the system temp directory.
    pub tmp_dir: Option<std::path::PathBuf>,
}

impl AppState {
    /// Builds the state.
    pub fn new(app: Arc<App>, ui: Option<Arc<dyn UiAssets>>, opts: HttpOptions) -> Self {
        Self {
            inner: Arc::new(AppInner {
                app,
                ui,
                opts,
                limits: ratelimit::RateLimits::default(),
            }),
        }
    }

    /// Trusted proxy networks: the environment wins over the settings.
    pub fn trusted_proxies(&self) -> Vec<net::Cidr> {
        if !self.inner.opts.trusted_proxies.is_empty() {
            return self.inner.opts.trusted_proxies.clone();
        }
        self.inner
            .app
            .settings()
            .strings("security", "trusted_proxies")
            .iter()
            .filter_map(|c| net::Cidr::parse(c))
            .collect()
    }

    /// Directory for upload temp files.
    pub fn tmp_dir(&self) -> std::path::PathBuf {
        self.inner
            .opts
            .tmp_dir
            .clone()
            .unwrap_or_else(std::env::temp_dir)
    }

    /// `limits.max_json_body_kb` in bytes.
    pub fn max_json_body_bytes(&self) -> u64 {
        u64::try_from(
            self.inner
                .app
                .settings()
                .int("limits", "max_json_body_kb", 1024),
        )
        .unwrap_or(1024)
        .saturating_mul(1024)
    }

    /// The rate limiters.
    pub fn limits(&self) -> &ratelimit::RateLimits {
        &self.inner.limits
    }

    /// Current rate-limit values.
    pub fn limit_config(&self) -> ratelimit::LimitConfig {
        ratelimit::LimitConfig::from_settings(self.inner.app.settings().section("security"))
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
        self.inner.opts.dev
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
        !self.inner.opts.dev || https
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
        r(
            Method::GET,
            "/api/superusers/passkeys",
            "/api/superusers/passkeys",
        ),
        r(
            Method::DELETE,
            "/api/superusers/passkeys/{id}",
            "/api/superusers/passkeys/x",
        ),
        r(
            Method::POST,
            "/api/superusers/passkeys/register/start",
            "/api/superusers/passkeys/register/start",
        ),
        r(
            Method::POST,
            "/api/superusers/passkeys/register/finish",
            "/api/superusers/passkeys/register/finish",
        ),
        r(
            Method::POST,
            "/api/superusers/passkeys/login/start",
            "/api/superusers/passkeys/login/start",
        ),
        r(
            Method::POST,
            "/api/superusers/passkeys/login/finish",
            "/api/superusers/passkeys/login/finish",
        ),
        r(
            Method::GET,
            "/api/collections/{c}/auth-methods",
            "/api/collections/users/auth-methods",
        ),
        r(
            Method::POST,
            "/api/collections/{c}/auth-with-password",
            "/api/collections/users/auth-with-password",
        ),
        r(
            Method::POST,
            "/api/collections/{c}/auth-refresh",
            "/api/collections/users/auth-refresh",
        ),
        r(
            Method::POST,
            "/api/collections/{c}/logout",
            "/api/collections/users/logout",
        ),
        r(
            Method::GET,
            "/api/collections/{c}/sessions",
            "/api/collections/users/sessions",
        ),
        r(
            Method::DELETE,
            "/api/collections/{c}/sessions",
            "/api/collections/users/sessions",
        ),
        r(
            Method::DELETE,
            "/api/collections/{c}/sessions/{id}",
            "/api/collections/users/sessions/x",
        ),
        r(
            Method::GET,
            "/api/collections/{c}/passkeys",
            "/api/collections/users/passkeys",
        ),
        r(
            Method::DELETE,
            "/api/collections/{c}/passkeys/{id}",
            "/api/collections/users/passkeys/x",
        ),
        r(
            Method::POST,
            "/api/collections/{c}/passkeys/register/start",
            "/api/collections/users/passkeys/register/start",
        ),
        r(
            Method::POST,
            "/api/collections/{c}/passkeys/register/finish",
            "/api/collections/users/passkeys/register/finish",
        ),
        r(
            Method::POST,
            "/api/collections/{c}/passkeys/login/start",
            "/api/collections/users/passkeys/login/start",
        ),
        r(
            Method::POST,
            "/api/collections/{c}/passkeys/login/finish",
            "/api/collections/users/passkeys/login/finish",
        ),
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
        r(
            Method::GET,
            "/api/files/{c}/{id}/{filename}",
            "/api/files/posts/x/a_abcdefgh.png",
        ),
        r(Method::POST, "/api/files/token", "/api/files/token"),
        r(Method::GET, "/api/realtime", "/api/realtime"),
        r(Method::POST, "/api/realtime", "/api/realtime"),
        r(Method::GET, "/api/ws", "/api/ws"),
        r(Method::GET, "/api/files/usage", "/api/files/usage"),
        r(
            Method::POST,
            "/api/settings/test-storage",
            "/api/settings/test-storage",
        ),
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
    use api::{admin, auth, collections, files, realtime, records, users};
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
        .route("/api/superusers/passkeys", get(users::su_passkeys))
        .route(
            "/api/superusers/passkeys/{id}",
            delete(users::su_passkey_delete),
        )
        .route(
            "/api/superusers/passkeys/register/start",
            post(users::su_passkey_register_start),
        )
        .route(
            "/api/superusers/passkeys/register/finish",
            post(users::su_passkey_register_finish),
        )
        .route(
            "/api/superusers/passkeys/login/start",
            post(users::su_passkey_login_start),
        )
        .route(
            "/api/superusers/passkeys/login/finish",
            post(users::su_passkey_login_finish),
        )
        .route(
            "/api/collections/{c}/auth-methods",
            get(users::auth_methods),
        )
        .route(
            "/api/collections/{c}/auth-with-password",
            post(users::login),
        )
        .route("/api/collections/{c}/auth-refresh", post(users::refresh))
        .route("/api/collections/{c}/logout", post(users::logout))
        .route(
            "/api/collections/{c}/sessions",
            get(users::sessions).delete(users::sessions_revoke_all),
        )
        .route(
            "/api/collections/{c}/sessions/{id}",
            delete(users::session_revoke),
        )
        .route("/api/collections/{c}/passkeys", get(users::passkeys))
        .route(
            "/api/collections/{c}/passkeys/{id}",
            delete(users::passkey_delete),
        )
        .route(
            "/api/collections/{c}/passkeys/register/start",
            post(users::passkey_register_start),
        )
        .route(
            "/api/collections/{c}/passkeys/register/finish",
            post(users::passkey_register_finish),
        )
        .route(
            "/api/collections/{c}/passkeys/login/start",
            post(users::passkey_login_start),
        )
        .route(
            "/api/collections/{c}/passkeys/login/finish",
            post(users::passkey_login_finish),
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
        // Record writes read their body themselves: JSON (limit from the
        // settings) or multipart with per-field file limits (SPEC §10.1).
        .route(
            "/api/collections/{c}/records",
            get(records::list).merge(post(records::create).layer(DefaultBodyLimit::disable())),
        )
        .route(
            "/api/collections/{c}/records/{id}",
            get(records::view)
                .merge(patch(records::update).layer(DefaultBodyLimit::disable()))
                .delete(records::delete),
        )
        .route("/api/realtime", get(realtime::sse).post(realtime::bind))
        .route("/api/ws", get(realtime::ws))
        .route("/api/files/token", post(files::token))
        .route("/api/files/usage", get(files::usage))
        .route("/api/files/{c}/{id}/{filename}", get(files::download))
        .route("/api/settings/test-storage", post(files::test_storage))
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
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            ratelimit::global_limit,
        ))
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
