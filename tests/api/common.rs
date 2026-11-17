//! Test helpers: an app on a temp data dir, a superuser, request helpers.

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Method, Request, StatusCode};
use rackend_core::usecases::{App, Principal, RequestCtx};
use rackend_store_sqlite::{DataDbPaths, DbOptions, SqliteStore, Synchronous, open_data_db};
use serde_json::Value;
use tower::ServiceExt;

pub const ADMIN_EMAIL: &str = "admin@example.com";
pub const ADMIN_PASSWORD: &str = "correct horse battery";

pub struct TestApp {
    pub app: Arc<App>,
    pub router: Router,
    pub token: String,
    pub dir: tempfile::TempDir,
}

/// Adapters a test replaces.
#[derive(Default)]
pub struct Overrides {
    pub mailer_factory: Option<Arc<dyn rackend_core::ports::MailerFactory>>,
    pub idp: Option<Arc<dyn rackend_core::ports::IdpClient>>,
    pub dev: bool,
}

/// Captures email instead of sending it.
#[derive(Debug, Clone, Default)]
pub struct Outbox(pub Arc<std::sync::Mutex<Vec<rackend_core::ports::Email>>>);

impl rackend_core::ports::Mailer for Outbox {
    fn send(
        &self,
        email: rackend_core::ports::Email,
    ) -> rackend_core::ports::BoxFuture<'_, Result<(), rackend_core::ports::MailError>> {
        self.0.lock().unwrap().push(email);
        Box::pin(async { Ok(()) })
    }
}

impl rackend_core::ports::MailerFactory for Outbox {
    fn build(
        &self,
        _: &rackend_core::ports::SmtpConfig,
    ) -> Result<Arc<dyn rackend_core::ports::Mailer>, rackend_core::ports::MailError> {
        Ok(Arc::new(self.clone()))
    }
}

impl Outbox {
    /// Emails sent to `to` so far.
    pub fn to(&self, to: &str) -> Vec<rackend_core::ports::Email> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.to == to)
            .cloned()
            .collect()
    }

    /// Waits (mail goes out in the background) for the `n`th email to `to`.
    pub async fn wait(&self, to: &str, n: usize) -> rackend_core::ports::Email {
        for _ in 0..200 {
            if let Some(e) = self.to(to).get(n - 1) {
                return e.clone();
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("no email #{n} to {to}");
    }
}

pub async fn start_empty() -> (Arc<App>, Router, tempfile::TempDir) {
    start_empty_with(Overrides::default()).await
}

pub async fn start_empty_with(o: Overrides) -> (Arc<App>, Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let paths = DataDbPaths {
        db: dir.path().join("data.db"),
        backups_dir: dir.path().join("backups"),
    };
    let (db, _) = open_data_db(
        &paths,
        DbOptions::data(Synchronous::Full),
        "0.0.0-test",
        time::OffsetDateTime::now_utc(),
    )
    .unwrap();
    let logs = rackend_store_sqlite::open_logs_db(
        &dir.path().join("logs.db"),
        DbOptions::logs(Synchronous::Full),
        "0.0.0-test",
        time::OffsetDateTime::now_utc(),
    )
    .unwrap();
    let app = rackend::wiring::app(
        SqliteStore::new(db),
        rackend_store_sqlite::SqliteLogs::new(&logs),
        rackend::wiring::AppOptions {
            version: "0.0.0-test",
            data_dir: dir.path().to_path_buf(),
            mailer_factory: o.mailer_factory,
            idp: o.idp,
            dev: o.dev,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let router = rackend::wiring::router(
        Arc::clone(&app),
        rackend_http::HttpOptions {
            tmp_dir: Some(dir.path().join("tmp")),
            ..Default::default()
        },
    );
    (app, router, dir)
}

pub async fn start() -> TestApp {
    start_with(Overrides::default()).await
}

pub async fn start_with(o: Overrides) -> TestApp {
    let (app, router, dir) = start_empty_with(o).await;
    assert!(
        app.ensure_superuser(ADMIN_EMAIL, ADMIN_PASSWORD)
            .await
            .unwrap()
    );
    let (status, _, body) = call(
        &router,
        Method::POST,
        "/api/superusers/auth-with-password",
        None,
        Some(serde_json::json!({"email": ADMIN_EMAIL, "password": ADMIN_PASSWORD})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let token = body["access_token"].as_str().unwrap().to_owned();
    TestApp {
        app,
        router,
        token,
        dir,
    }
}

pub struct Reply {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

pub async fn send(router: &Router, req: Request<Body>) -> Reply {
    let res = router.clone().oneshot(req).await.unwrap();
    let (parts, body) = res.into_parts();
    Reply {
        status: parts.status,
        headers: parts.headers,
        body: to_bytes(body, usize::MAX).await.unwrap().to_vec(),
    }
}

pub fn request(
    method: Method,
    uri: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        b = b.header("authorization", format!("Bearer {t}"));
    }
    match body {
        Some(v) => b
            .header("content-type", "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    }
}

pub async fn call(
    router: &Router,
    method: Method,
    uri: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, HeaderMap, Value) {
    let r = send(router, request(method, uri, token, body)).await;
    let json = r.json();
    (r.status, r.headers, json)
}

/// A context for a synthetic auth-collection user (HTTP user auth is M2).
pub fn user_ctx(id: &str) -> RequestCtx {
    RequestCtx::system(
        Principal::User {
            id: id.into(),
            collection_id: "users".into(),
            collection_name: "users".into(),
            fields: serde_json::Map::new(),
            session_id: "s".into(),
        },
        time::OffsetDateTime::now_utc(),
    )
}

pub fn anon_ctx() -> RequestCtx {
    RequestCtx::system(Principal::Anonymous, time::OffsetDateTime::now_utc())
}

pub fn superuser_ctx() -> RequestCtx {
    RequestCtx::system(
        Principal::Superuser {
            id: "su".into(),
            email: ADMIN_EMAIL.into(),
            session_id: "s".into(),
        },
        time::OffsetDateTime::now_utc(),
    )
}

/// A `multipart/form-data` request: an optional `@json` part plus files
/// given as `(field, filename, bytes)`.
pub fn multipart_request(
    method: Method,
    uri: &str,
    token: Option<&str>,
    json: Option<Value>,
    files: &[(&str, &str, &[u8])],
) -> Request<Body> {
    let boundary = "rackend-test-boundary-7d3f";
    let mut body = Vec::new();
    if let Some(j) = json {
        body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"@json\"\r\nContent-Type: application/json\r\n\r\n{j}\r\n").as_bytes());
    }
    for (field, name, bytes) in files {
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{field}\"; filename=\"{name}\"\r\nContent-Type: application/octet-stream\r\n\r\n").as_bytes(),
        );
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    let mut b = Request::builder().method(method).uri(uri).header(
        "content-type",
        format!("multipart/form-data; boundary={boundary}"),
    );
    if let Some(t) = token {
        b = b.header("authorization", format!("Bearer {t}"));
    }
    b.body(Body::from(body)).unwrap()
}

/// Sends a multipart request and parses the JSON answer.
pub async fn call_multipart(
    router: &Router,
    method: Method,
    uri: &str,
    token: Option<&str>,
    json: Option<Value>,
    files: &[(&str, &str, &[u8])],
) -> (StatusCode, HeaderMap, Value) {
    let r = send(router, multipart_request(method, uri, token, json, files)).await;
    let v = r.json();
    (r.status, r.headers, v)
}
