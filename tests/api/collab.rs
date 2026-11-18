//! M9: collaborative documents (SPEC §13.4) with a y-protocols client
//! built from yrs.

use std::time::Duration;

use axum::http::{Method, StatusCode};
use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use yrs::sync::{Message as YMessage, MessageReader, SyncMessage};
use yrs::updates::decoder::{Decode, DecoderV1};
use yrs::updates::encoder::Encode;
use yrs::{Doc, GetString, ReadTxn, StateVector, Text, Transact, Update};

use super::channels::{serve, user};
use crate::common::*;

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct Peer {
    s: WsStream,
    doc: Doc,
}

impl Peer {
    async fn open(url: &str, token: Option<&str>) -> Self {
        let mut req = url.into_client_request().unwrap();
        if let Some(t) = token {
            req.headers_mut()
                .insert("authorization", format!("Bearer {t}").parse().unwrap());
        }
        let (s, _) = tokio_tungstenite::connect_async(req).await.unwrap();
        Self { s, doc: Doc::new() }
    }

    async fn send(&mut self, m: YMessage) {
        self.s
            .send(Message::Binary(m.encode_v1().into()))
            .await
            .unwrap();
    }

    /// The next y-protocols messages (one frame) within `ms`.
    async fn recv(&mut self, ms: u64) -> Option<Vec<YMessage>> {
        loop {
            let m = tokio::time::timeout(Duration::from_millis(ms), self.s.next())
                .await
                .ok()??
                .ok()?;
            match m {
                Message::Binary(b) => {
                    let mut d = DecoderV1::from(b.as_ref());
                    return Some(MessageReader::new(&mut d).map(Result::unwrap).collect());
                }
                Message::Close(_) => return None,
                _ => {}
            }
        }
    }

    /// Applies document messages until one matching `f` arrives.
    async fn until(&mut self, f: impl Fn(&YMessage) -> bool) -> YMessage {
        loop {
            let msgs = self
                .recv(5000)
                .await
                .expect("expected message did not arrive");
            for m in msgs {
                if let YMessage::Sync(SyncMessage::SyncStep2(u) | SyncMessage::Update(u)) = &m {
                    self.doc
                        .transact_mut()
                        .apply_update(Update::decode_v1(u).unwrap())
                        .unwrap();
                }
                if f(&m) {
                    return m;
                }
            }
        }
    }

    /// Edits the local document and sends the change.
    async fn type_text(&mut self, at: u32, s: &str) {
        let sv = self.doc.transact().state_vector();
        let text = self.doc.get_or_insert_text("body");
        text.insert(&mut self.doc.transact_mut(), at, s);
        let update = self.doc.transact().encode_diff_v1(&sv);
        self.send(YMessage::Sync(SyncMessage::Update(update))).await;
    }

    fn text(&self) -> String {
        let text = self.doc.get_or_insert_text("body");
        text.get_string(&self.doc.transact())
    }
}

fn text_of(state: &[u8]) -> String {
    let doc = Doc::new();
    doc.transact_mut()
        .apply_update(Update::decode_v1(state).unwrap())
        .unwrap();
    let text = doc.get_or_insert_text("body");
    text.get_string(&doc.transact())
}

fn is_custom(m: &YMessage) -> bool {
    matches!(m, YMessage::Custom(100, _))
}

async fn state(t: &TestApp, path: &str, token: &str) -> (StatusCode, Vec<u8>) {
    let r = send(
        &t.router,
        request(Method::GET, &format!("{path}/state"), Some(token), None),
    )
    .await;
    (r.status, r.body)
}

#[tokio::test]
async fn collaborative_documents_sync_persist_and_enforce_read_only() {
    let t = start().await;
    let su = Some(t.token.as_str());
    let (tw, idw) = user(&t, "writer@example.com").await;
    let (tr, _) = user(&t, "reader@example.com").await;
    let (s, _, out) = call(
        &t.router,
        Method::POST,
        "/api/collections",
        su,
        Some(json!({
            "name": "notes",
            "fields": [{"name": "title", "type": "text"}, {"name": "body", "type": "ydoc"}],
            "rules": {
                "list": "@request.auth.id != ''", "view": "@request.auth.id != ''",
                "create": "", "update": format!("@request.auth.id = '{idw}'"), "delete": null,
            },
        })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{out}");
    // Rules cannot use ydoc fields.
    let (s, _, out) = call(
        &t.router,
        Method::PATCH,
        "/api/collections/notes",
        su,
        Some(json!({"rules": {"list": "body != ''"}})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{out}");

    // REST writes cannot set the document; records omit it.
    let (s, _, out) = call(
        &t.router,
        Method::POST,
        "/api/collections/notes/records",
        Some(&tw),
        Some(json!({"title": "x", "body": "hello"})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{out}");
    let (s, _, rec) = call(
        &t.router,
        Method::POST,
        "/api/collections/notes/records",
        Some(&tw),
        Some(json!({"title": "Plan"})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{rec}");
    assert!(rec.get("body").is_none(), "{rec}");
    let id = rec["id"].as_str().unwrap().to_owned();
    let path = format!("/api/collab/notes/{id}/body");

    let addr = serve(&t).await;
    // Not a ydoc field, unknown record, no access.
    for (p, tok, code) in [
        (format!("/api/collab/notes/{id}/title"), Some(&tw), 400),
        (
            format!("/api/collab/notes/{}/body", uuid::Uuid::now_v7()),
            Some(&tw),
            404,
        ),
        (path.clone(), None, 404),
    ] {
        let mut req = format!("ws://{addr}{p}").into_client_request().unwrap();
        if let Some(t) = tok {
            req.headers_mut()
                .insert("authorization", format!("Bearer {t}").parse().unwrap());
        }
        match tokio_tungstenite::connect_async(req).await {
            Err(tokio_tungstenite::tungstenite::Error::Http(res)) => {
                assert_eq!(res.status(), code, "{p}");
            }
            other => panic!("{p}: expected {code}, got {:?}", other.map(|_| ())),
        }
    }

    // The writer connects with a header, the reader with a ticket.
    let mut w = Peer::open(&format!("ws://{addr}{path}"), Some(&tw)).await;
    w.until(|m| matches!(m, YMessage::Sync(SyncMessage::SyncStep1(_))))
        .await;
    let (s, _, ticket) = call(
        &t.router,
        Method::POST,
        &format!("{path}/ticket"),
        Some(&tr),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{ticket}");
    let url = format!(
        "ws://{addr}{path}?ticket={}",
        ticket["ticket"].as_str().unwrap()
    );
    let mut r = Peer::open(&url, None).await;
    r.until(|m| matches!(m, YMessage::Sync(SyncMessage::SyncStep1(_))))
        .await;
    match tokio_tungstenite::connect_async(url.as_str()).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(res)) => assert_eq!(res.status(), 401),
        other => panic!("tickets are single use, got {:?}", other.map(|_| ())),
    }

    // Writer edits reach the reader.
    w.type_text(0, "hello").await;
    r.until(|m| matches!(m, YMessage::Sync(SyncMessage::Update(_))))
        .await;
    assert_eq!(r.text(), "hello");

    // Awareness is relayed.
    let aw = Doc::new();
    let mut awareness = yrs::sync::Awareness::new(aw);
    awareness.set_local_state_raw(r#"{"user":"w"}"#);
    w.send(YMessage::Awareness(awareness.update().unwrap()))
        .await;
    r.until(|m| matches!(m, YMessage::Awareness(_))).await;

    // Reader updates are dropped with the custom error message.
    r.type_text(5, " from the reader").await;
    let YMessage::Custom(_, body) = r.until(is_custom).await else {
        unreachable!()
    };
    let err: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(err["code"], "read_only", "{err}");
    assert!(
        w.recv(500).await.is_none_or(|m| !m
            .iter()
            .any(|m| matches!(m, YMessage::Sync(SyncMessage::Update(_))))),
        "the reader's update is not relayed"
    );

    // Sync step 1 from a fresh client returns the document.
    let mut late = Peer::open(&format!("ws://{addr}{path}"), Some(&tw)).await;
    late.send(YMessage::Sync(SyncMessage::SyncStep1(
        StateVector::default(),
    )))
    .await;
    late.until(|m| matches!(m, YMessage::Sync(SyncMessage::SyncStep2(_))))
        .await;
    assert_eq!(late.text(), "hello");
    w.type_text(5, " world").await;

    // State over REST (in memory), then after shutdown persistence (from
    // the database).
    tokio::time::sleep(Duration::from_millis(600)).await;
    let (s, st) = state(&t, &path, &tr).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(text_of(&st), "hello world");
    drop((w, r, late));
    tokio::time::sleep(Duration::from_millis(200)).await;
    t.app.collab_shutdown().await;
    assert_eq!(t.app.collab.open_documents(), 0);
    let (_, st) = state(&t, &path, &tr).await;
    assert_eq!(
        text_of(&st),
        "hello world",
        "persisted; the reader's text is not"
    );

    // The record's `updated` was touched.
    let (_, _, after) = call(
        &t.router,
        Method::GET,
        &format!("/api/collections/notes/records/{id}"),
        Some(&tw),
        None,
    )
    .await;
    assert_ne!(after["updated"], rec["updated"]);
    assert!(after.get("body").is_none());

    // Replacing the state needs the update rule and a valid update.
    let fresh = Doc::new();
    fresh
        .get_or_insert_text("body")
        .insert(&mut fresh.transact_mut(), 0, "replaced");
    let new_state = fresh
        .transact()
        .encode_state_as_update_v1(&StateVector::default());
    let put = |tok: &str, body: Vec<u8>| {
        let mut req = request(Method::PUT, &format!("{path}/state"), Some(tok), None);
        *req.body_mut() = axum::body::Body::from(body);
        req.headers_mut()
            .insert("content-type", "application/octet-stream".parse().unwrap());
        send(&t.router, req)
    };
    assert_eq!(
        put(&tr, new_state.clone()).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        put(&tw, b"junk".to_vec()).await.status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(put(&tw, new_state).await.status, StatusCode::NO_CONTENT);
    assert_eq!(text_of(&state(&t, &path, &tw).await.1), "replaced");

    // Deleting the record removes its document.
    let (s, _, _) = call(
        &t.router,
        Method::DELETE,
        &format!("/api/collections/notes/records/{id}"),
        su,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert_eq!(state(&t, &path, &tw).await.0, StatusCode::NOT_FOUND);
}
