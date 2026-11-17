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

pub async fn start_empty() -> (Arc<App>, Router, tempfile::TempDir) {
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
    let app = rackend::wiring::app(SqliteStore::new(db), "0.0.0-test", None)
        .await
        .unwrap();
    let router = rackend::wiring::router(Arc::clone(&app), false);
    (app, router, dir)
}

pub async fn start() -> TestApp {
    let (app, router, dir) = start_empty().await;
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
