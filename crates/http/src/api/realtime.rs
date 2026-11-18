//! Realtime transports (SPEC §13.2.1, §13.2.2): Server-Sent Events and
//! WebSocket. Authorization and dispatch live in the core.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::State;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::http::HeaderMap;
use axum::http::HeaderValue;
use axum::response::IntoResponse;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::{SinkExt as _, StreamExt as _};
use rackend_core::realtime::{Client, Out};
use rackend_core::usecases::realtime::SubRequest;
use rackend_core::usecases::{App, RequestCtx};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedReceiver;

use super::{ApiResult, no_content};
use crate::AppState;
use crate::ctx::{Ctx, SESSION_COOKIE, cookie, has_bearer, origin_allowed};
use crate::problem::ApiError;

/// SSE heartbeat and WebSocket ping interval.
const HEARTBEAT: Duration = Duration::from_secs(25);
/// WebSocket connections without a pong for this long are closed.
const PONG_TIMEOUT: Duration = Duration::from_secs(60);

/// Marks a client disconnected when its stream ends.
struct Detach {
    app: Arc<App>,
    id: String,
    keep: bool,
}

impl Drop for Detach {
    fn drop(&mut self) {
        self.app.hub.disconnect(&self.id, self.keep);
    }
}

fn last_event_id(headers: &HeaderMap) -> Option<(String, u64)> {
    let v = headers.get("last-event-id")?.to_str().ok()?;
    let (id, seq) = v.rsplit_once('.')?;
    Some((id.to_owned(), seq.parse().ok()?))
}

/// `GET /api/realtime`: an SSE stream. The first event is `connect` with
/// the client id; `Last-Event-ID` resumes a stream within two minutes.
pub async fn sse(State(state): State<AppState>, Ctx(ctx): Ctx, headers: HeaderMap) -> ApiResult {
    let app = Arc::clone(state.app());
    let resumed = last_event_id(&headers).and_then(|(id, seq)| app.hub.resume(&id, seq));
    let (client, rx, resumed, complete) = match resumed {
        Some((c, rx, complete)) => (c, rx, true, complete),
        None => {
            let (c, rx) = app.realtime_connect(ctx.ip.clone())?;
            (c, rx, false, true)
        }
    };
    let mut first = vec![
        Event::default()
            .event("connect")
            .data(json!({ "client_id": client.id, "resumed": resumed }).to_string()),
    ];
    if !complete {
        let topics: std::collections::BTreeSet<String> = client
            .subscriptions()
            .iter()
            .map(|s| s.topic.as_string())
            .collect();
        for topic in topics {
            first.push(
                Event::default()
                    .event("resync")
                    .data(json!({ "type": "resync", "topic": topic }).to_string()),
            );
        }
    }
    let guard = Detach {
        app: Arc::clone(&app),
        id: client.id.clone(),
        keep: true,
    };
    struct St {
        rx: UnboundedReceiver<Out>,
        client: Arc<Client>,
        done: bool,
        _guard: Detach,
    }
    let head = futures_util::stream::iter(first.into_iter().map(Ok::<_, Infallible>));
    let body = futures_util::stream::unfold(
        St {
            rx,
            client,
            done: false,
            _guard: guard,
        },
        |mut st| async move {
            if st.done {
                return None;
            }
            let out = st.rx.recv().await?;
            st.client.sent(&out);
            let ev = match &out {
                Out::Event { seq, frame } => Event::default()
                    .event(&frame.topic)
                    .id(format!("{}.{seq}", st.client.id))
                    .data(&frame.sse),
                Out::Resync { topic } => Event::default()
                    .event("resync")
                    .data(json!({ "type": "resync", "topic": topic }).to_string()),
                // Channels are WebSocket only.
                Out::Text(_) | Out::Binary(_) => Event::default().comment(""),
                Out::Close { reason } => {
                    st.done = true;
                    Event::default()
                        .event("close")
                        .data(json!({ "type": "close", "reason": reason }).to_string())
                }
            };
            Some((Ok::<_, Infallible>(ev), st))
        },
    );
    let mut res = Sse::new(head.chain(body))
        .keep_alive(KeepAlive::new().interval(HEARTBEAT))
        .into_response();
    let h = res.headers_mut();
    h.insert("cache-control", HeaderValue::from_static("no-cache"));
    // Proxies such as nginx must not buffer the stream.
    h.insert("x-accel-buffering", HeaderValue::from_static("no"));
    Ok(res)
}

/// Body of `POST /api/realtime`.
#[derive(Debug, Deserialize)]
pub struct Bind {
    client_id: String,
    #[serde(default)]
    subscriptions: Vec<SubIn>,
}

/// One requested subscription.
#[derive(Debug, Deserialize)]
pub struct SubIn {
    topic: String,
    filter: Option<String>,
    expand: Option<String>,
    fields: Option<String>,
}

/// `POST /api/realtime {client_id, subscriptions}`: binds the caller's
/// identity (Authorization header or cookie) and replaces the subscriptions.
pub async fn bind(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Json(body): Json<Bind>,
) -> ApiResult {
    let reqs: Vec<SubRequest> = body
        .subscriptions
        .into_iter()
        .map(|s| SubRequest {
            id: None,
            topic: s.topic,
            filter: s.filter,
            expand: s.expand,
            fields: s.fields,
        })
        .collect();
    state
        .app()
        .realtime_set_subscriptions(&ctx, &body.client_id, &reqs)?;
    Ok(no_content())
}

/// `GET /api/ws`: the WebSocket transport.
pub async fn ws(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> ApiResult {
    // Browsers send cookies on cross-site WebSocket requests: a cookie
    // session only counts from an allowed origin.
    if !has_bearer(&headers) && cookie(&headers, SESSION_COOKIE).is_some_and(|c| !c.is_empty()) {
        origin_allowed(&state, &headers)?;
    }
    let app = Arc::clone(state.app());
    let (client, rx) = app.realtime_connect(ctx.ip.clone())?;
    app.realtime_auth(&client, &ctx)?;
    let limits = app.realtime_limits();
    Ok(upgrade
        .max_message_size(limits.message_bytes)
        .max_frame_size(limits.message_bytes)
        .read_buffer_size(4 * 1024)
        .write_buffer_size(4 * 1024)
        .on_upgrade(move |socket| run_ws(app, client, rx, socket, ctx)))
}

fn err(id: Option<&Value>, e: &ApiError) -> Value {
    let p = e.problem();
    let mut m = json!({ "t": "err", "code": p.code.as_str(), "message": p.detail.clone().unwrap_or_else(|| p.code.title().to_owned()) });
    if let Some(id) = id {
        m["id"] = id.clone();
    }
    if !p.errors.is_empty() {
        m["errors"] = json!(p.errors);
    }
    m
}

fn ack(id: Option<&Value>) -> Value {
    match id {
        Some(id) => json!({ "t": "ack", "id": id }),
        None => json!({ "t": "ack" }),
    }
}

/// Handles one client message and returns the reply.
async fn handle(app: &App, client: &Arc<Client>, ctx: &RequestCtx, text: &str) -> Value {
    let Ok(msg) = serde_json::from_str::<Value>(text) else {
        return err(
            None,
            &ApiError::with_detail(crate::ProblemCode::BadRequest, "Messages are JSON objects."),
        );
    };
    let id = msg.get("id");
    let s = |k: &str| msg.get(k).and_then(Value::as_str).map(str::to_owned);
    let result: Result<Value, ApiError> = match msg.get("t").and_then(Value::as_str) {
        Some("ping") => Ok(json!({ "t": "pong" })),
        Some("auth") => {
            async {
                let token =
                    s("token").ok_or_else(|| ApiError::from(rackend_core::Error::Unauthorized))?;
                let principal = app
                    .authenticate(&token)
                    .await
                    .map_err(|_| ApiError::from(rackend_core::Error::Unauthorized))?;
                let mut c2 = ctx.clone();
                c2.principal = principal;
                app.realtime_auth(client, &c2)?;
                Ok(ack(id))
            }
            .await
        }
        Some("sub") => {
            let req = SubRequest {
                id: id.and_then(Value::as_u64),
                topic: s("topic").unwrap_or_default(),
                filter: s("filter"),
                expand: s("expand"),
                fields: s("fields"),
            };
            app.realtime_subscribe(client, &req)
                .map(|()| ack(id))
                .map_err(ApiError::from)
        }
        Some("unsub") => {
            let n =
                app.realtime_unsubscribe(client, id.and_then(Value::as_u64), s("topic").as_deref());
            if n == 0 {
                Err(ApiError::with_detail(
                    crate::ProblemCode::NotFound,
                    "No such subscription.",
                ))
            } else {
                Ok(ack(id))
            }
        }
        Some("join") => {
            let channel = s("channel").unwrap_or_default();
            app.channel_join(client, &channel, msg.get("meta").cloned())
                .await
                .map(|()| ack(id))
                .map_err(ApiError::from)
        }
        Some("leave") => app
            .channel_leave(client, &s("channel").unwrap_or_default())
            .map(|()| ack(id))
            .map_err(ApiError::from),
        // Messages without an id are not acknowledged (errors still are).
        Some("pub") if id.is_none() => {
            return app
                .channel_publish(
                    client,
                    &s("channel").unwrap_or_default(),
                    &s("event").unwrap_or_default(),
                    msg.get("data").unwrap_or(&Value::Null),
                )
                .map_or_else(|e| err(None, &ApiError::from(e)), |()| Value::Null);
        }
        Some("pub") => app
            .channel_publish(
                client,
                &s("channel").unwrap_or_default(),
                &s("event").unwrap_or_default(),
                msg.get("data").unwrap_or(&Value::Null),
            )
            .map(|()| ack(id))
            .map_err(ApiError::from),
        Some("meta") => app
            .channel_meta(
                client,
                &s("channel").unwrap_or_default(),
                msg.get("meta").cloned(),
            )
            .map(|()| ack(id))
            .map_err(ApiError::from),
        _ => Err(ApiError::with_detail(
            crate::ProblemCode::BadRequest,
            "Unknown message type.",
        )),
    };
    result.unwrap_or_else(|e| err(id, &e))
}

async fn run_ws(
    app: Arc<App>,
    client: Arc<Client>,
    mut rx: UnboundedReceiver<Out>,
    socket: WebSocket,
    ctx: RequestCtx,
) {
    let _guard = Detach {
        app: Arc::clone(&app),
        id: client.id.clone(),
        keep: false,
    };
    let (mut sink, mut stream) = socket.split();
    let hello = json!({ "t": "connect", "client_id": client.id }).to_string();
    if sink.send(Message::Text(hello.into())).await.is_err() {
        return;
    }
    let mut ping = tokio::time::interval(HEARTBEAT);
    ping.tick().await;
    let mut last_pong = Instant::now();
    loop {
        tokio::select! {
            msg = stream.next() => match msg {
                Some(Ok(Message::Text(t))) => {
                    let reply = handle(&app, &client, &ctx, &t).await;
                    if !reply.is_null() && sink.send(Message::Text(reply.to_string().into())).await.is_err() {
                        break;
                    }
                }
                Some(Ok(Message::Pong(_))) => last_pong = Instant::now(),
                Some(Ok(Message::Ping(_))) => last_pong = Instant::now(),
                Some(Ok(Message::Binary(_))) => {
                    let e = ApiError::with_detail(crate::ProblemCode::BadRequest, "Send JSON text frames.");
                    if sink.send(Message::Text(err(None, &e).to_string().into())).await.is_err() {
                        break;
                    }
                }
                Some(Ok(Message::Close(_)) | Err(_)) | None => break,
            },
            out = rx.recv() => {
                let Some(out) = out else { break };
                client.sent(&out);
                let sent = match &out {
                    Out::Event { frame, .. } => sink.send(Message::Text(frame.ws.clone().into())).await,
                    Out::Resync { topic } => {
                        sink.send(Message::Text(json!({ "t": "resync", "topic": topic }).to_string().into())).await
                    }
                    Out::Text(t) => sink.send(Message::Text(t.as_ref().into())).await,
                    Out::Binary(_) => Ok(()),
                    Out::Close { reason } => {
                        let _ = sink
                            .send(Message::Close(Some(CloseFrame { code: 4000, reason: (*reason).into() })))
                            .await;
                        break;
                    }
                };
                if sent.is_err() {
                    break;
                }
            }
            _ = ping.tick() => {
                if last_pong.elapsed() > PONG_TIMEOUT {
                    let _ = sink.send(Message::Close(Some(CloseFrame { code: 4000, reason: "timeout".into() }))).await;
                    break;
                }
                if sink.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
        }
    }
}

/// `GET /api/channels`.
pub async fn channels(State(state): State<AppState>, Ctx(ctx): Ctx) -> ApiResult {
    Ok(super::ok(state.app().channels_list(&ctx)?))
}

/// `GET /api/channels/live`.
pub async fn channels_live(State(state): State<AppState>, Ctx(ctx): Ctx) -> ApiResult {
    Ok(super::ok(state.app().channels_live(&ctx)?))
}

/// `POST /api/channels`.
pub async fn channel_create(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Json(body): Json<Value>,
) -> ApiResult {
    Ok(super::created(
        state.app().channel_save(&ctx, None, body).await?,
    ))
}

/// `PATCH /api/channels/{id}`.
pub async fn channel_update(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    Ok(super::ok(
        state.app().channel_save(&ctx, Some(&id), body).await?,
    ))
}

/// `DELETE /api/channels/{id}`.
pub async fn channel_delete(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> ApiResult {
    state.app().channel_delete(&ctx, &id).await?;
    Ok(no_content())
}
