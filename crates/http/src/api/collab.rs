//! Collaborative documents (SPEC §13.4): the y-protocols WebSocket and the
//! state endpoints. The protocol logic lives in the core.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::IntoResponse;
use futures_util::{SinkExt as _, StreamExt as _};
use rackend_core::realtime::Out;
use rackend_core::usecases::App;
use rackend_core::usecases::collab::{CollabConn, error_message};
use serde::Deserialize;
use tokio::sync::mpsc::UnboundedReceiver;

use super::{ApiResult, no_content, ok};
use crate::AppState;
use crate::ctx::{Ctx, SESSION_COOKIE, cookie, has_bearer, origin_allowed};
use crate::problem::ApiError;

const HEARTBEAT: Duration = Duration::from_secs(25);
const PONG_TIMEOUT: Duration = Duration::from_secs(60);
/// Largest inbound frame: the initial sync of a full document.
const MAX_FRAME: usize = 16 * 1024 * 1024;

/// Query of the WebSocket URL.
#[derive(Debug, Deserialize)]
pub struct WsQuery {
    ticket: Option<String>,
}

/// Leaves the room and the hub when the connection ends (or never starts).
struct Leave {
    app: Arc<App>,
    conn: CollabConn,
}

impl Drop for Leave {
    fn drop(&mut self) {
        self.app.collab_close(&self.conn);
        self.app.hub.disconnect(&self.conn.client.id, false);
    }
}

/// `GET /api/collab/{collection}/{record_id}/{field}`: upgrades to a
/// WebSocket speaking y-protocols sync and awareness (binary).
pub async fn ws(
    State(state): State<AppState>,
    Ctx(mut ctx): Ctx,
    Path((collection, record_id, field)): Path<(String, String, String)>,
    Query(q): Query<WsQuery>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> ApiResult {
    let app = Arc::clone(state.app());
    if let Some(t) = &q.ticket {
        ctx.principal = app.collab_redeem(t, &collection, &record_id, &field)?;
    } else if !has_bearer(&headers)
        && cookie(&headers, SESSION_COOKIE).is_some_and(|c| !c.is_empty())
    {
        // Browsers send cookies on cross-site WebSocket requests.
        origin_allowed(&state, &headers)?;
    }
    let (conn, rx, first) = app
        .collab_open(&ctx, &collection, &record_id, &field)
        .await?;
    let leave = Leave {
        app: Arc::clone(&app),
        conn,
    };
    Ok(upgrade
        .max_message_size(MAX_FRAME)
        .max_frame_size(MAX_FRAME)
        .on_upgrade(move |socket| run(leave, rx, first, socket)))
}

async fn run(leave: Leave, mut rx: UnboundedReceiver<Out>, first: Vec<Vec<u8>>, socket: WebSocket) {
    let (app, conn) = (&leave.app, &leave.conn);
    let (mut sink, mut stream) = socket.split();
    for m in first {
        if sink.send(Message::Binary(m.into())).await.is_err() {
            return;
        }
    }
    let mut ping = tokio::time::interval(HEARTBEAT);
    ping.tick().await;
    let mut last_pong = Instant::now();
    loop {
        tokio::select! {
            msg = stream.next() => match msg {
                Some(Ok(Message::Binary(b))) => {
                    let replies = app.collab_message(conn, &b).unwrap_or_else(|e| {
                        let e = ApiError::from(e);
                        let p = e.problem();
                        vec![error_message(p.code.as_str(), &p.detail.clone().unwrap_or_default())]
                    });
                    for r in replies {
                        if sink.send(Message::Binary(r.into())).await.is_err() {
                            return;
                        }
                    }
                }
                Some(Ok(Message::Pong(_) | Message::Ping(_))) => last_pong = Instant::now(),
                Some(Ok(Message::Text(_))) => {
                    let m = error_message("bad_request", "Send binary y-protocols messages.");
                    if sink.send(Message::Binary(m.into())).await.is_err() {
                        return;
                    }
                }
                Some(Ok(Message::Close(_)) | Err(_)) | None => return,
            },
            out = rx.recv() => {
                let Some(out) = out else { return };
                conn.client.sent(&out);
                match out {
                    Out::Binary(b) => {
                        if sink.send(Message::Binary(b.to_vec().into())).await.is_err() {
                            return;
                        }
                    }
                    Out::Close { reason } => {
                        let _ = sink
                            .send(Message::Close(Some(CloseFrame { code: 4000, reason: reason.into() })))
                            .await;
                        return;
                    }
                    Out::Event { .. } | Out::Resync { .. } | Out::Text(_) => {}
                }
            }
            _ = ping.tick() => {
                if last_pong.elapsed() > PONG_TIMEOUT {
                    let _ = sink.send(Message::Close(Some(CloseFrame { code: 4000, reason: "timeout".into() }))).await;
                    return;
                }
                if sink.send(Message::Ping(Vec::new().into())).await.is_err() {
                    return;
                }
            }
        }
    }
}

/// `POST /api/collab/{collection}/{record_id}/{field}/ticket` →
/// `{ticket, expires_in}`.
pub async fn ticket(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Path((collection, record_id, field)): Path<(String, String, String)>,
) -> ApiResult {
    Ok(ok(state
        .app()
        .collab_ticket(&ctx, &collection, &record_id, &field)
        .await?))
}

/// `GET /api/collab/{collection}/{record_id}/{field}/state`: the document
/// as one Yjs update (v1).
pub async fn state_get(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Path((collection, record_id, field)): Path<(String, String, String)>,
) -> ApiResult {
    let bytes = state
        .app()
        .collab_state(&ctx, &collection, &record_id, &field)
        .await?;
    let mut res = Bytes::from(bytes).into_response();
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(res)
}

/// `PUT /api/collab/{collection}/{record_id}/{field}/state`: replaces the
/// document (update rule).
pub async fn state_put(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Path((collection, record_id, field)): Path<(String, String, String)>,
    body: Bytes,
) -> ApiResult {
    state
        .app()
        .collab_replace(&ctx, &collection, &record_id, &field, &body)
        .await?;
    Ok(no_content())
}
