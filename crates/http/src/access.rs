//! Access logging and HTTP metrics (SPEC §14.2, §14.3). Wraps the whole
//! stack, so it sees the final status, the request id and the problem code.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::extract::{ConnectInfo, MatchedPath, Request, State};
use axum::http::header;
use axum::middleware::Next;
use axum::response::Response;
use rackend_core::ports::RequestLog;
use rackend_core::time::format_timestamp;

use crate::AppState;

/// Who made the request, filled in by the `Ctx` extractor.
#[derive(Debug, Clone, Default)]
pub struct ActorSlot(pub Arc<Mutex<Option<Actor>>>);

/// Actor kind and id.
type Actor = (String, Option<String>);

impl ActorSlot {
    /// Records the actor.
    pub fn set(&self, kind: &str, id: Option<String>) {
        if let Ok(mut g) = self.0.lock() {
            *g = Some((kind.to_owned(), id));
        }
    }
}

/// The matched route template, copied to the response by [`tag_route`].
#[derive(Debug, Clone)]
pub struct RouteTag(pub String);

/// The problem code of an error response, set when the problem is rendered.
#[derive(Debug, Clone, Copy)]
pub struct ProblemTag(pub &'static str);

/// Route layer: remembers which route template matched.
pub async fn tag_route(req: Request, next: Next) -> Response {
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_owned());
    let mut res = next.run(req).await;
    if let Some(r) = route {
        res.extensions_mut().insert(RouteTag(r));
    }
    res
}

/// Outermost layer: one log row and the HTTP metrics per request.
pub async fn access_log(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let started = Instant::now();
    let ts = format_timestamp(time::OffsetDateTime::now_utc());
    let method = req.method().as_str().to_owned();
    let path = req.uri().path().to_owned();
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip());
    let ip =
        crate::net::client_ip(peer, req.headers(), &state.trusted_proxies()).map(|i| i.to_string());
    let user_agent = req
        .headers()
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.chars().take(512).collect());
    let bytes_in = req
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());
    let slot = ActorSlot::default();
    req.extensions_mut().insert(slot.clone());

    let res = next.run(req).await;

    let duration = started.elapsed();
    let status = res.status().as_u16();
    let route = res.extensions().get::<RouteTag>().map(|r| r.0.clone());
    let label = route.clone().unwrap_or_else(|| "unmatched".into());
    metrics::counter!(
        "rackend_http_requests_total",
        "route" => label.clone(),
        "method" => method.clone(),
        "status" => status.to_string()
    )
    .increment(1);
    metrics::histogram!("rackend_http_request_duration_seconds", "route" => label, "method" => method.clone())
        .record(duration.as_secs_f64());

    let (actor_type, actor_id) = slot
        .0
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .map_or((None, None), |(k, id)| (Some(k), id));
    state.app().record_request(RequestLog {
        id: 0,
        ts,
        method,
        route,
        path,
        status,
        duration_ms: duration.as_secs_f64() * 1000.0,
        ip,
        user_agent,
        actor_type,
        actor_id,
        request_id: res
            .headers()
            .get(&crate::middleware::X_REQUEST_ID)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
        bytes_in,
        bytes_out: res
            .headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok()),
        error: res.extensions().get::<ProblemTag>().map(|p| p.0.to_owned()),
    });
    res
}
