//! Error contract (SPEC §10.2.6): every failure is an RFC 9457 problem.

use std::collections::HashSet;
use std::path::PathBuf;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use axum::routing::{get, post};
use rackend_core::Error as CoreError;
use rackend_core::error::UnavailableReason;
use rackend_http::{ApiError, HttpConfig, routes, with_middleware};
use serde_json::Value;

use crate::common::{Reply, request, send, start};

const SECRET: &str = "SECRET-MARKER-7f3a";

#[derive(Debug)]
struct Leaky;
impl std::fmt::Display for Leaky {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "db password is {SECRET}")
    }
}
impl std::error::Error for Leaky {}

/// Handlers that trigger framework and handler failures, behind the
/// production middleware stack.
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

/// The `next` docs of the website (rackend-homepage), which describe this
/// branch: `RACKEND_DOCS_DIR`, else a rackend-homepage checkout next to rackend.
fn docs_dir() -> PathBuf {
    std::env::var_os("RACKEND_DOCS_DIR").map_or_else(
        || {
            PathBuf::from(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../../rackend-homepage/src/content/docs/docs/next"
            ))
        },
        PathBuf::from,
    )
}

fn documented_codes() -> HashSet<String> {
    let path = docs_dir().join("reference/errors.md");
    let doc = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{}: {e}. Problem codes are documented in rackend-homepage: clone it next to \
             rackend or set RACKEND_DOCS_DIR to its src/content/docs/docs/next",
            path.display()
        )
    });
    doc.lines()
        .filter_map(|l| l.strip_prefix("### "))
        .map(|s| s.trim().to_owned())
        .collect()
}

/// The checks every problem response must pass.
pub fn assert_problem(r: &Reply) -> Value {
    assert_eq!(
        r.headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/problem+json"),
        "content type for {}: {}",
        r.status,
        String::from_utf8_lossy(&r.body)
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
        format!("https://rackend.axanet.ch/docs/reference/errors/#{code}")
    );
    assert!(
        documented_codes().contains(code),
        "{code} must be documented in the docs (reference/errors.md)"
    );
    let rid = r
        .headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .unwrap();
    assert_eq!(p["request_id"].as_str(), Some(rid));
    assert_eq!(p["instance"].as_str().unwrap(), format!("urn:uuid:{rid}"));
    p
}

#[test]
fn every_registered_code_is_documented() {
    let documented = documented_codes();
    for code in rackend_http::ProblemCode::ALL {
        assert!(
            documented.contains(code.as_str()),
            "{} missing from the docs (reference/errors.md)",
            code.as_str()
        );
    }
}

#[tokio::test]
async fn every_route_exists_and_rejects_other_methods() {
    let t = start().await;
    for route in routes() {
        let r = send(
            &t.router,
            request(Method::TRACE, route.example, Some(&t.token), None),
        )
        .await;
        assert_eq!(
            r.status,
            StatusCode::METHOD_NOT_ALLOWED,
            "{} {} is not registered",
            route.method,
            route.path
        );
        let p = assert_problem(&r);
        assert_eq!(p["code"], "method_not_allowed");
        let allowed: Vec<&str> = p["allowed"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(
            allowed.contains(&route.method.as_str()),
            "{}: {allowed:?}",
            route.path
        );
    }
}

type Case<'a> = (Method, &'a str, Option<&'a str>, Option<Value>, StatusCode);

#[tokio::test]
async fn representative_failures_on_real_routes_are_problems() {
    let t = start().await;
    let cases: Vec<Case<'_>> = vec![
        (
            Method::GET,
            "/api/does-not-exist",
            None,
            None,
            StatusCode::NOT_FOUND,
        ),
        (
            Method::GET,
            "/api/collections",
            None,
            None,
            StatusCode::UNAUTHORIZED,
        ),
        (
            Method::GET,
            "/api/collections",
            Some("rk_at_bad"),
            None,
            StatusCode::UNAUTHORIZED,
        ),
        (
            Method::GET,
            "/api/collections/nope",
            Some(&t.token),
            None,
            StatusCode::NOT_FOUND,
        ),
        (
            Method::GET,
            "/api/collections/nope/records",
            None,
            None,
            StatusCode::NOT_FOUND,
        ),
        (
            Method::POST,
            "/api/collections",
            Some(&t.token),
            Some(serde_json::json!({"name": "1bad"})),
            StatusCode::BAD_REQUEST,
        ),
        (
            Method::POST,
            "/api/superusers/auth-with-password",
            None,
            Some(serde_json::json!({"email": "x@y.z", "password": "nope"})),
            StatusCode::UNAUTHORIZED,
        ),
        (
            Method::POST,
            "/api/rules/check",
            Some(&t.token),
            Some(serde_json::json!({"collection": {"name": "x"}, "rule": "a = "})),
            StatusCode::BAD_REQUEST,
        ),
    ];
    for (method, uri, token, body, want) in cases {
        let r = send(&t.router, request(method.clone(), uri, token, body)).await;
        assert_eq!(
            r.status,
            want,
            "{method} {uri}: {}",
            String::from_utf8_lossy(&r.body)
        );
        let p = assert_problem(&r);
        if want == StatusCode::UNAUTHORIZED {
            assert!(r.headers.get(header::WWW_AUTHENTICATE).is_some());
        }
        if uri == "/api/rules/check" {
            assert_eq!(p["code"], "rule_compile_error");
            assert_eq!(p["position"]["column"], 5);
        }
    }
    // Bad JSON on a real endpoint.
    let r = send(
        &t.router,
        Request::builder()
            .method(Method::POST)
            .uri("/api/collections")
            .header("authorization", format!("Bearer {}", t.token))
            .header("content-type", "application/json")
            .body(Body::from("{nope"))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_problem(&r);
}

async fn probe(uri: &str, method: Method, ct: Option<&str>, body: &str) -> Reply {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(ct) = ct {
        b = b.header("content-type", ct);
    }
    send(&probe_app(), b.body(Body::from(body.to_owned())).unwrap()).await
}

#[tokio::test]
async fn framework_errors_are_mapped() {
    let r = probe(
        "/t/json",
        Method::POST,
        Some("application/json"),
        "{not json",
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(assert_problem(&r)["detail"].as_str().is_some());

    let r = probe(
        "/t/typed",
        Method::POST,
        Some("application/json"),
        r#"{"n":"x"}"#,
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "422 is rewritten to 400");
    assert_problem(&r);

    let r = probe("/t/json", Method::POST, Some("text/plain"), "{}").await;
    assert_eq!(r.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_problem(&r);

    let big = format!("{{\"a\":\"{}\"}}", "x".repeat(200));
    let r = probe("/t/json", Method::POST, Some("application/json"), &big).await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(assert_problem(&r)["max_bytes"], 64);

    for uri in ["/t/panic", "/t/internal", "/t/plain-error"] {
        let r = probe(uri, Method::GET, None, "").await;
        assert_eq!(r.status, StatusCode::INTERNAL_SERVER_ERROR, "{uri}");
        assert_eq!(assert_problem(&r)["code"], "internal");
        let text = String::from_utf8_lossy(&r.body);
        assert!(
            !text.contains(SECRET) && !text.contains("exploded"),
            "{uri} leaked: {text}"
        );
    }

    let r = probe("/t/busy", Method::GET, None, "").await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(assert_problem(&r)["retry_after_s"], 1);
    assert_eq!(r.headers.get(header::RETRY_AFTER).unwrap(), "1");
}

#[tokio::test]
async fn head_request_ids_headers_and_ui() {
    let t = start().await;
    let r = send(
        &t.router,
        request(Method::HEAD, "/api/does-not-exist", None, None),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert!(r.body.is_empty());

    let given = "01936b2e-7c4a-7d1e-9f3a-2b8c1d4e5f60";
    let r = send(
        &t.router,
        Request::builder()
            .uri("/api/nope")
            .header("x-request-id", given)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(assert_problem(&r)["request_id"], given);

    for uri in ["/api/health", "/api/nope", "/_/"] {
        let r = send(&t.router, request(Method::GET, uri, None, None)).await;
        assert_eq!(
            r.headers.get("x-content-type-options").unwrap(),
            "nosniff",
            "{uri}"
        );
    }
    let ui = send(&t.router, request(Method::GET, "/_/", None, None)).await;
    assert_eq!(ui.status, StatusCode::OK);
    assert_eq!(ui.headers.get("x-frame-options").unwrap(), "DENY");
    assert!(
        ui.headers
            .get("content-security-policy")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("script-src 'self'")
    );

    let r = send(
        &t.router,
        Request::builder()
            .uri("/_/missing.js")
            .header("accept", "text/html")
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
}

/// Once graceful shutdown starts, new requests get the `unavailable`
/// problem (SPEC §5.6): clients that reconnect at once, such as collab
/// editors whose sockets were just closed, must not get work from the
/// instance that is going away.
#[tokio::test]
async fn shutting_down_refuses_new_requests() {
    let t = start().await;
    let before = send(
        &t.router,
        request(Method::GET, "/api/collections", Some(&t.token), None),
    )
    .await;
    assert_eq!(before.status, StatusCode::OK);

    t.app.begin_shutdown();
    for uri in ["/api/collections", "/api/health"] {
        let r = send(&t.router, request(Method::GET, uri, Some(&t.token), None)).await;
        assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{uri}");
        let p = assert_problem(&r);
        assert_eq!(p["code"], "unavailable", "{uri}");
    }
}
