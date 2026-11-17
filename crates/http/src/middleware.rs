//! Request ids, problem mapping, security headers and panic handling.

use std::any::Any;
use std::time::Instant;

use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use axum::http::Method;
use axum::http::header::{self, HeaderName, HeaderValue};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use tracing::Instrument;
use uuid::Uuid;

use crate::problem::{PROBLEM_JSON, Problem, ProblemCode};

/// `X-Request-Id` header.
pub static X_REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

/// The id of the current request (request extension).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestId(pub Uuid);

/// Accepts a valid UUID from `X-Request-Id` or generates a UUIDv7, stores it
/// in the request extensions, runs the request inside an `http.request` span
/// carrying it, and echoes it in the response (SPEC §10.1, §14.5).
pub async fn request_id(mut req: Request, next: Next) -> Response {
    let id = req
        .headers()
        .get(&X_REQUEST_ID)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| Uuid::try_parse(v.trim()).ok())
        .unwrap_or_else(Uuid::now_v7);
    req.extensions_mut().insert(RequestId(id));

    let span = tracing::info_span!(
        "http.request",
        request_id = %id,
        method = %req.method(),
        path = %req.uri().path(),
    );
    let started = Instant::now();
    let mut res = next.run(req).instrument(span.clone()).await;
    let status = res.status().as_u16();
    span.in_scope(|| {
        tracing::debug!(
            status,
            duration_ms = started.elapsed().as_secs_f64() * 1000.0,
            "request finished"
        );
    });
    if let Ok(v) = HeaderValue::from_str(&id.to_string()) {
        res.headers_mut().insert(X_REQUEST_ID.clone(), v);
    }
    res
}

/// Settings the problem mapper needs.
#[derive(Debug, Clone, Copy)]
pub struct ProblemConfig {
    /// The JSON body limit, reported as `max_bytes` on 413.
    pub max_body_bytes: u64,
}

/// Converts every error response into RFC 9457 problem details (SPEC §10.2.3).
///
/// - Responses carrying a [`Problem`] extension (from `ApiError`) are rendered.
/// - Any other response with status >= 400 that is not already a problem is
///   converted from its status (extractor rejections, 404/405, limits).
/// - Browser navigations to hosted HTML pages get an HTML error page (§10.2.5).
/// - `HEAD` responses keep status and headers but drop the body.
pub async fn problem_mapper(
    State(cfg): State<ProblemConfig>,
    req: Request,
    next: Next,
) -> Response {
    let request_id = req
        .extensions()
        .get::<RequestId>()
        .map_or_else(|| Uuid::now_v7().to_string(), |r| r.0.to_string());
    let is_head = req.method() == Method::HEAD;
    let wants_html = wants_html(&req);

    let res = next.run(req).await;
    let res = if let Some(problem) = res.extensions().get::<Problem>().cloned() {
        render(problem, &request_id, res, wants_html)
    } else if res.status().as_u16() >= 400 && !is_problem(&res) {
        let mut problem = synthesize(&res, cfg);
        let text_body = is_text_plain(&res);
        let (parts, body) = res.into_parts();
        if problem.detail.is_none() && parts.status.is_client_error() && text_body {
            problem.detail = detail_from_body(body).await;
        }
        render(
            problem,
            &request_id,
            Response::from_parts(parts, Body::empty()),
            wants_html,
        )
    } else {
        res
    };
    if is_head { strip_body(res) } else { res }
}

fn is_problem(res: &Response) -> bool {
    res.headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with(PROBLEM_JSON))
}

/// Whether the client is a browser navigating to a hosted HTML page and does
/// not prefer JSON.
fn wants_html(req: &Request) -> bool {
    let path = req.uri().path();
    if !(path == "/_" || path.starts_with("/_/") || path.starts_with("/auth/")) {
        return false;
    }
    let accept = req
        .headers()
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if accept.contains("application/json") || accept.contains(PROBLEM_JSON) {
        return false;
    }
    accept.contains("text/html")
}

fn is_text_plain(res: &Response) -> bool {
    res.headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("text/plain"))
}

fn synthesize(res: &Response, cfg: ProblemConfig) -> Problem {
    let status = res.status();
    let code = ProblemCode::for_status(status);
    let mut problem = Problem::new(code);
    match code {
        ProblemCode::MethodNotAllowed => {
            if let Some(allow) = res
                .headers()
                .get(header::ALLOW)
                .and_then(|v| v.to_str().ok())
            {
                problem.allowed = allow
                    .split(',')
                    .map(|m| m.trim().to_owned())
                    .filter(|m| !m.is_empty())
                    .collect();
            }
        }
        ProblemCode::PayloadTooLarge => problem.max_bytes = Some(cfg.max_body_bytes),
        ProblemCode::NotFound => {
            problem.detail = Some("The requested resource does not exist.".into())
        }
        ProblemCode::RateLimited | ProblemCode::Unavailable => {
            problem.retry_after_s = res
                .headers()
                .get(header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse().ok())
                .or(Some(1));
        }
        _ => {}
    }
    problem
}

/// Copies the text of a small plain-text rejection body into `detail` for
/// client errors. Framework rejections describe the client's input only.
async fn detail_from_body(body: Body) -> Option<String> {
    let bytes = to_bytes(body, 1024).await.ok()?;
    let text = String::from_utf8(bytes.to_vec()).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

fn render(problem: Problem, request_id: &str, res: Response, wants_html: bool) -> Response {
    let (mut parts, _) = res.into_parts();
    let status = problem.code.status();
    parts.status = status;
    parts.extensions.remove::<Problem>();
    parts.headers.remove(header::CONTENT_LENGTH);

    if let Some(retry) = problem.retry_after_s {
        parts
            .headers
            .insert(header::RETRY_AFTER, HeaderValue::from(retry));
    }
    if problem.code == ProblemCode::Unauthorized
        && !parts.headers.contains_key(header::WWW_AUTHENTICATE)
    {
        parts.headers.insert(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static("Bearer realm=\"rackend\""),
        );
    }
    if problem.code == ProblemCode::MethodNotAllowed
        && !problem.allowed.is_empty()
        && let Ok(v) = HeaderValue::from_str(&problem.allowed.join(", "))
    {
        parts.headers.insert(header::ALLOW, v);
    }
    if problem.code == ProblemCode::Internal || status.is_server_error() {
        tracing::warn!(code = problem.code.as_str(), %request_id, "request failed with a server error");
    }

    let body = problem.body(request_id);
    let (content_type, bytes) = if wants_html {
        (
            "text/html; charset=utf-8",
            crate::html::error_page(&body).into_bytes(),
        )
    } else {
        match serde_json::to_vec(&body) {
            Ok(json) => (PROBLEM_JSON, json),
            Err(_) => (PROBLEM_JSON, br#"{"code":"internal"}"#.to_vec()),
        }
    };
    parts
        .headers
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    Response::from_parts(parts, Body::from(bytes))
}

fn strip_body(res: Response) -> Response {
    let (parts, _) = res.into_parts();
    Response::from_parts(parts, Body::empty())
}

/// Security headers on every response (SPEC §17.2, §17.3).
pub async fn security_headers(req: Request, next: Next) -> Response {
    let hosted_page = {
        let p = req.uri().path();
        p == "/_" || p.starts_with("/_/") || p.starts_with("/auth/")
    };
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    if hosted_page {
        h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    }
    res
}

/// Panic handler for `CatchPanicLayer`: an `internal` problem. The panic
/// payload is logged, never returned.
pub fn panic_response(err: Box<dyn Any + Send + 'static>) -> Response {
    let msg = err
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| err.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic payload".into());
    tracing::error!(panic = %msg, "handler panicked");
    crate::problem::ApiError::new(ProblemCode::Internal).into_response()
}
