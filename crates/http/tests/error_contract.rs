#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Error contract (SPEC §10.2.6): every failure is an RFC 9457 problem.

use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use axum::response::Response;
use axum::routing::{get, post};
use rackend_core::Error as CoreError;
use rackend_core::error::UnavailableReason;
use rackend_core::ports::{AnyBox, ReadJob, Store, StoreFuture, WriteJob};
use rackend_http::{
    ApiError, AppState, HttpConfig, UiAssets, UiFile, router, routes, with_middleware,
};
use serde_json::Value;
use tower::ServiceExt;

#[derive(Debug)]
struct NoStore;
impl Store for NoStore {
    fn read_job(&self, _: ReadJob) -> StoreFuture<'_, AnyBox> {
        Box::pin(async { Err(CoreError::NotFound) })
    }
    fn write_job(&self, _: WriteJob) -> StoreFuture<'_, AnyBox> {
        Box::pin(async { Err(CoreError::NotFound) })
    }
}

#[derive(Debug)]
struct FakeUi;
impl UiAssets for FakeUi {
    fn get(&self, path: &str) -> Option<UiFile> {
        (path == "index.html").then_some(UiFile {
            data: Cow::Borrowed(b"<!doctype html><title>ui</title>"),
            mime: "text/html; charset=utf-8",
        })
    }
}

fn app() -> Router {
    let state = AppState::new("0.0.0-test", Arc::new(NoStore), Some(Arc::new(FakeUi)));
    router(state, HttpConfig::default())
}

const SECRET: &str = "SECRET-MARKER-7f3a";

#[derive(Debug)]
struct Leaky;

impl std::fmt::Display for Leaky {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "db password is {SECRET}")
    }
}
impl std::error::Error for Leaky {}

/// A router with handlers that trigger framework and handler failures, using
/// the production middleware stack.
fn probe_app() -> Router {
    #[derive(serde::Deserialize)]
    #[allow(dead_code)]
    struct Typed {
        n: u32,
    }
    let app = Router::new()
        .route(
            "/t/json",
            post(|axum::Json(_): axum::Json<Value>| async { "ok" }),
        )
        .route(
            "/t/typed",
            post(|axum::Json(_): axum::Json<Typed>| async { "ok" }),
        )
        .route(
            "/t/panic",
            get(|| async {
                if true {
                    panic!("boom {SECRET}")
                } else {
                    "unreachable"
                }
            }),
        )
        .route(
            "/t/internal",
            get(|| async { Err::<(), _>(ApiError::from(CoreError::internal(Leaky))) }),
        )
        .route(
            "/t/busy",
            get(|| async {
                Err::<(), _>(ApiError::from(CoreError::unavailable(
                    UnavailableReason::WriterQueueFull,
                )))
            }),
        )
        .route(
            "/t/plain-error",
            get(|| async { (StatusCode::BAD_GATEWAY, "upstream exploded") }),
        );
    with_middleware(
        app,
        HttpConfig {
            max_json_body_bytes: 64,
        },
    )
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("not JSON ({e}): {}", String::from_utf8_lossy(&self.body)))
    }
}

async fn send(app: &Router, req: Request<Body>) -> Reply {
    let res: Response = app.clone().oneshot(req).await.unwrap();
    let (parts, body) = res.into_parts();
    Reply {
        status: parts.status,
        headers: parts.headers,
        body: to_bytes(body, usize::MAX).await.unwrap().to_vec(),
    }
}

fn req(method: Method, uri: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

fn documented_codes() -> HashSet<String> {
    let doc = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/errors.md"))
        .unwrap();
    doc.lines()
        .filter_map(|l| l.strip_prefix("### "))
        .map(|s| s.trim().to_owned())
        .collect()
}

/// The checks every problem response must pass.
fn assert_problem(r: &Reply) -> Value {
    assert_eq!(
        r.headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/problem+json"),
        "content type for {}",
        r.status
    );
    let p = r.json();
    assert_eq!(
        p["status"].as_u64(),
        Some(u64::from(r.status.as_u16())),
        "status member: {p}"
    );
    let code = p["code"].as_str().unwrap();
    assert_eq!(
        p["type"].as_str().unwrap(),
        format!("https://github.com/axanet/rackend/blob/main/docs/errors.md#{code}")
    );
    assert!(
        documented_codes().contains(code),
        "{code} must be documented in docs/errors.md"
    );
    let rid = r
        .headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .unwrap();
    assert_eq!(p["request_id"].as_str(), Some(rid));
    assert_eq!(p["instance"].as_str().unwrap(), format!("urn:uuid:{rid}"));
    assert!(p["title"].as_str().is_some_and(|t| !t.is_empty()));
    p
}

#[test]
fn every_registered_code_is_documented() {
    let documented = documented_codes();
    for code in rackend_http::ProblemCode::ALL {
        assert!(
            documented.contains(code.as_str()),
            "{} missing from docs/errors.md",
            code.as_str()
        );
    }
}

#[tokio::test]
async fn every_route_is_served_and_rejects_wrong_methods() {
    let app = app();
    for route in routes() {
        let ok = send(&app, req(route.method.clone(), route.example)).await;
        assert_ne!(
            ok.status,
            StatusCode::NOT_FOUND,
            "{} {} is not served",
            route.method,
            route.path
        );

        let wrong = send(&app, req(Method::DELETE, route.example)).await;
        assert_eq!(
            wrong.status,
            StatusCode::METHOD_NOT_ALLOWED,
            "{}",
            route.path
        );
        let p = assert_problem(&wrong);
        assert_eq!(p["code"], "method_not_allowed");
        let allowed = p["allowed"].as_array().unwrap_or_else(|| {
            panic!(
                "{}: no allowed in {p}, headers {:?}",
                route.path, wrong.headers
            )
        });
        assert!(allowed.iter().any(|m| m == "GET"), "{}", route.path);
        assert!(wrong.headers.contains_key(header::ALLOW));
    }
}

#[tokio::test]
async fn unknown_route_is_not_found() {
    let r = send(&app(), req(Method::GET, "/api/does-not-exist")).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(assert_problem(&r)["code"], "not_found");
}

#[tokio::test]
async fn head_keeps_status_and_headers_without_body() {
    let r = send(&app(), req(Method::HEAD, "/api/does-not-exist")).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(
        r.headers.get(header::CONTENT_TYPE).unwrap(),
        "application/problem+json"
    );
    assert!(r.body.is_empty());
}

fn post_json(uri: &str, content_type: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header(header::CONTENT_TYPE, content_type)
        .body(Body::from(body.to_owned()))
        .unwrap()
}

#[tokio::test]
async fn bad_json_is_bad_request() {
    let r = send(
        &probe_app(),
        post_json("/t/json", "application/json", "{not json"),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let p = assert_problem(&r);
    assert_eq!(p["code"], "bad_request");
    assert!(
        p["detail"].as_str().is_some(),
        "rejection text becomes the detail"
    );
}

#[tokio::test]
async fn json_of_wrong_shape_is_bad_request_not_422() {
    let r = send(
        &probe_app(),
        post_json("/t/typed", "application/json", r#"{"n":"x"}"#),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(assert_problem(&r)["code"], "bad_request");
}

#[tokio::test]
async fn wrong_content_type_is_unsupported_media_type() {
    let r = send(&probe_app(), post_json("/t/json", "text/plain", "{}")).await;
    assert_eq!(r.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(assert_problem(&r)["code"], "unsupported_media_type");
}

#[tokio::test]
async fn oversized_body_is_payload_too_large_with_max_bytes() {
    let big = format!("{{\"a\":\"{}\"}}", "x".repeat(200));
    let r = send(&probe_app(), post_json("/t/json", "application/json", &big)).await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE);
    let p = assert_problem(&r);
    assert_eq!(p["code"], "payload_too_large");
    assert_eq!(p["max_bytes"], 64);
}

#[tokio::test]
async fn panic_is_internal_and_leaks_nothing() {
    let r = send(&probe_app(), req(Method::GET, "/t/panic")).await;
    assert_eq!(r.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(assert_problem(&r)["code"], "internal");
    assert!(!String::from_utf8_lossy(&r.body).contains(SECRET));
}

#[tokio::test]
async fn internal_error_never_exposes_its_message() {
    let r = send(&probe_app(), req(Method::GET, "/t/internal")).await;
    assert_eq!(r.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(assert_problem(&r)["code"], "internal");
    assert!(!String::from_utf8_lossy(&r.body).contains(SECRET));
}

#[tokio::test]
async fn non_problem_server_errors_are_mapped_to_internal_without_body_text() {
    let r = send(&probe_app(), req(Method::GET, "/t/plain-error")).await;
    assert_eq!(r.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(assert_problem(&r)["code"], "internal");
    assert!(!String::from_utf8_lossy(&r.body).contains("exploded"));
}

#[tokio::test]
async fn unavailable_has_retry_after() {
    let r = send(&probe_app(), req(Method::GET, "/t/busy")).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    let p = assert_problem(&r);
    assert_eq!(p["code"], "unavailable");
    assert_eq!(p["retry_after_s"], 1);
    assert_eq!(r.headers.get(header::RETRY_AFTER).unwrap(), "1");
}

#[tokio::test]
async fn request_id_is_accepted_or_generated() {
    let app = app();
    let given = "01936b2e-7c4a-7d1e-9f3a-2b8c1d4e5f60";
    let r = send(
        &app,
        Request::builder()
            .uri("/api/nope")
            .header("x-request-id", given)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.headers.get("x-request-id").unwrap(), given);
    assert_eq!(assert_problem(&r)["request_id"], given);

    let r = send(
        &app,
        Request::builder()
            .uri("/api/health")
            .header("x-request-id", "not-a-uuid")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let rid = r.headers.get("x-request-id").unwrap().to_str().unwrap();
    assert_ne!(rid, "not-a-uuid");
    assert!(uuid::Uuid::try_parse(rid).is_ok());
}

#[tokio::test]
async fn health_is_ok_with_version() {
    let r = send(&app(), req(Method::GET, "/api/health")).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        r.json(),
        serde_json::json!({"status": "ok", "version": "0.0.0-test"})
    );
}

#[tokio::test]
async fn security_headers_on_every_response() {
    let app = app();
    for uri in ["/api/health", "/api/nope", "/_/"] {
        let r = send(&app, req(Method::GET, uri)).await;
        assert_eq!(
            r.headers.get("x-content-type-options").unwrap(),
            "nosniff",
            "{uri}"
        );
        assert_eq!(
            r.headers.get("referrer-policy").unwrap(),
            "strict-origin-when-cross-origin",
            "{uri}"
        );
    }
    let ui = send(&app, req(Method::GET, "/_/")).await;
    assert_eq!(ui.headers.get("x-frame-options").unwrap(), "DENY");
    assert!(ui.headers.get("content-security-policy").is_some());
}

#[tokio::test]
async fn ui_spa_fallback_and_missing_assets() {
    let app = app();
    let r = send(&app, req(Method::GET, "/_/collections/posts")).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(String::from_utf8_lossy(&r.body).contains("<title>ui</title>"));

    let r = send(&app, req(Method::GET, "/_/missing.js")).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_problem(&r);

    let r = send(&app, req(Method::GET, "/_/../../etc/passwd")).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn browsers_get_html_error_pages_on_hosted_pages() {
    let r = send(
        &app(),
        Request::builder()
            .uri("/_/missing.js")
            .header(header::ACCEPT, "text/html,application/xhtml+xml,*/*;q=0.8")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert!(
        r.headers
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("text/html")
    );
    let rid = r
        .headers
        .get("x-request-id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let body = String::from_utf8_lossy(&r.body);
    assert!(body.contains(&rid) && body.contains("Not found"), "{body}");

    // JSON wins when the client prefers it.
    let r = send(
        &app(),
        Request::builder()
            .uri("/_/missing.js")
            .header(header::ACCEPT, "application/problem+json")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_problem(&r);
}
