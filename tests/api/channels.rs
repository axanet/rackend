//! M9: channels and presence over the WebSocket (SPEC §13.3).

use std::time::Duration;

use axum::http::{Method, StatusCode};
use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

use crate::common::*;

const PW: &str = "a long user password";

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

pub async fn serve(t: &TestApp) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(rackend_http::serve(
        listener,
        t.router.clone(),
        std::future::pending(),
    ));
    format!("127.0.0.1:{port}")
}

pub async fn user(t: &TestApp, email: &str) -> (String, String) {
    let (s, _, _) = call(
        &t.router,
        Method::POST,
        "/api/collections/users/records",
        None,
        Some(json!({"email": email, "password": PW})),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, _, out) = call(
        &t.router,
        Method::POST,
        "/api/collections/users/auth-with-password",
        None,
        Some(json!({"email": email, "password": PW})),
    )
    .await;
    (
        out["access_token"].as_str().unwrap().to_owned(),
        out["record"]["id"].as_str().unwrap().to_owned(),
    )
}

struct Ws {
    s: WsStream,
    seen: Vec<Value>,
}

impl Ws {
    async fn open(addr: &str, token: Option<&str>) -> Self {
        let (s, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/api/ws"))
            .await
            .unwrap();
        let mut ws = Self {
            s,
            seen: Vec::new(),
        };
        assert_eq!(ws.recv().await.unwrap()["t"], "connect");
        if let Some(tok) = token {
            ws.send(json!({"t": "auth", "token": tok})).await;
            assert_eq!(ws.recv().await.unwrap()["t"], "ack");
        }
        ws
    }

    async fn send(&mut self, v: Value) {
        self.s
            .send(Message::Text(v.to_string().into()))
            .await
            .unwrap();
    }

    async fn recv(&mut self) -> Option<Value> {
        loop {
            let m = tokio::time::timeout(Duration::from_secs(6), self.s.next())
                .await
                .ok()??
                .ok()?;
            if let Message::Text(t) = m {
                return serde_json::from_str(&t).ok();
            }
        }
    }

    /// The first message (seen earlier or arriving within 6 s) matching `f`.
    async fn wait(&mut self, f: impl Fn(&Value) -> bool) -> Value {
        if let Some(i) = self.seen.iter().position(&f) {
            return self.seen.remove(i);
        }
        loop {
            let m = self.recv().await.expect("expected message did not arrive");
            if f(&m) {
                return m;
            }
            self.seen.push(m);
        }
    }

    /// Whether a message matching `f` arrives within `ms`.
    async fn gets_within(&mut self, ms: u64, f: impl Fn(&Value) -> bool) -> bool {
        if self.seen.iter().any(&f) {
            return true;
        }
        let deadline = tokio::time::Instant::now() + Duration::from_millis(ms);
        while let Ok(Some(Ok(m))) = tokio::time::timeout_at(deadline, self.s.next()).await {
            if let Message::Text(t) = m
                && let Ok(v) = serde_json::from_str::<Value>(&t)
            {
                if f(&v) {
                    return true;
                }
                self.seen.push(v);
            }
        }
        false
    }

    async fn request(&mut self, mut m: Value, id: u64) -> Value {
        m["id"] = json!(id);
        self.send(m).await;
        self.wait(|v| v["id"] == id && (v["t"] == "ack" || v["t"] == "err"))
            .await
    }
}

fn presence(kind: &'static str) -> impl Fn(&Value) -> bool {
    move |v| v["t"] == "presence" && v["kind"] == kind
}

#[tokio::test]
async fn channels_carry_messages_and_presence_under_rules() {
    let t = start().await;
    let su = Some(t.token.as_str());

    // Definitions: validation and rule compile errors.
    let (s, _, out) = call(
        &t.router,
        Method::POST,
        "/api/channels",
        su,
        Some(json!({"name": "Rooms", "pattern": "Room:*"})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{out}");
    assert!(out["errors"]["pattern"].is_array(), "{out}");
    let (s, _, out) = call(
        &t.router,
        Method::POST,
        "/api/channels",
        su,
        Some(json!({"name": "Rooms", "pattern": "room:*", "join_rule": "@channel.nope = 1"})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{out}");
    assert!(
        out["detail"].as_str().unwrap().contains("join_rule"),
        "{out}"
    );
    let (s, _, def) = call(
        &t.router,
        Method::POST,
        "/api/channels",
        su,
        Some(json!({
            "name": "Rooms", "pattern": "room:*",
            "join_rule": "@request.auth.id != ''",
            "publish_rule": "@channel.key != 'quiet'",
            "options": {"max_members": 3},
        })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{def}");
    let def_id = def["id"].as_str().unwrap().to_owned();
    let (s, _, out) = call(
        &t.router,
        Method::POST,
        "/api/channels",
        su,
        Some(json!({"name": "Rooms again", "pattern": "room:*"})),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{out}");
    let (s, _, _) = call(&t.router, Method::GET, "/api/channels", None, None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    let addr = serve(&t).await;
    let (ta, ida) = user(&t, "a@example.com").await;
    let (tb, idb) = user(&t, "b@example.com").await;
    let mut a = Ws::open(&addr, Some(&ta)).await;
    let mut b = Ws::open(&addr, Some(&tb)).await;
    let mut anon = Ws::open(&addr, None).await;

    // Rules and unknown channels.
    let r = anon
        .request(json!({"t": "join", "channel": "room:1"}), 1)
        .await;
    assert_eq!(r["code"], "forbidden", "{r}");
    let r = a
        .request(json!({"t": "join", "channel": "nowhere"}), 1)
        .await;
    assert_eq!(r["t"], "err", "{r}");
    let r = a
        .request(json!({"t": "join", "channel": "room:1", "meta": "x"}), 2)
        .await;
    assert_eq!(r["t"], "err", "{r}");

    // Join: sync for the joiner, join for the others.
    let r = a
        .request(
            json!({"t": "join", "channel": "room:1", "meta": {"status": "here"}}),
            3,
        )
        .await;
    assert_eq!(r["t"], "ack", "{r}");
    let sync = a.wait(presence("sync")).await;
    assert_eq!(sync["members"].as_array().unwrap().len(), 1);
    assert_eq!(sync["members"][0]["user_id"], ida.as_str());
    assert_eq!(sync["members"][0]["meta"]["status"], "here");
    let r = b
        .request(json!({"t": "join", "channel": "room:1"}), 1)
        .await;
    assert_eq!(r["t"], "ack", "{r}");
    assert_eq!(
        b.wait(presence("sync")).await["members"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let joined = a.wait(presence("join")).await;
    assert_eq!(joined["members"][0]["user_id"], idb.as_str());
    assert_eq!(joined["members"][0]["collection"], "users");
    let b_conn = joined["members"][0]["conn_id"].as_str().unwrap().to_owned();

    // Messages reach the others, not the sender; no ack without an id.
    b.send(json!({"t": "pub", "channel": "room:1", "event": "cursor", "data": {"x": 3}}))
        .await;
    let m = a.wait(|v| v["t"] == "msg").await;
    assert_eq!(m["event"], "cursor");
    assert_eq!(m["data"]["x"], 3);
    assert_eq!(m["from"]["user_id"], idb.as_str());
    assert_eq!(m["from"]["conn_id"], b_conn.as_str());
    assert!(
        !b.gets_within(300, |v| v["t"] == "msg" || v["t"] == "ack")
            .await
    );
    let r = b
        .request(
            json!({"t": "pub", "channel": "room:1", "event": "big", "data": "x".repeat(20_000)}),
            5,
        )
        .await;
    assert_eq!(r["code"], "payload_too_large", "{r}");
    let r = anon
        .request(json!({"t": "pub", "channel": "room:1", "event": "x"}), 2)
        .await;
    assert_eq!(r["t"], "err", "not joined: {r}");

    // Meta updates.
    let r = b
        .request(
            json!({"t": "meta", "channel": "room:1", "meta": {"status": "typing"}}),
            6,
        )
        .await;
    assert_eq!(r["t"], "ack");
    let m = a.wait(presence("meta")).await;
    assert_eq!(m["members"][0]["meta"]["status"], "typing");

    // The publish rule: members of room:quiet cannot publish.
    let r = a
        .request(json!({"t": "join", "channel": "room:quiet"}), 7)
        .await;
    assert_eq!(r["t"], "ack");
    let r = a
        .request(
            json!({"t": "pub", "channel": "room:quiet", "event": "x"}),
            8,
        )
        .await;
    assert_eq!(r["code"], "forbidden", "{r}");

    // Live view.
    let (s, _, live) = call(&t.router, Method::GET, "/api/channels/live", su, None).await;
    assert_eq!(s, StatusCode::OK);
    let room1 = live["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["channel"] == "room:1")
        .unwrap();
    assert_eq!(room1["members"].as_array().unwrap().len(), 2);
    assert_eq!(room1["messages"], 1);
    let (_, _, list) = call(&t.router, Method::GET, "/api/channels", su, None).await;
    assert_eq!(list["items"][0]["members"], 3);

    // A disconnect is debounced, then everyone sees `leave`.
    drop(b);
    assert!(
        !a.gets_within(2000, presence("leave")).await,
        "leave is debounced"
    );
    let left = a.wait(presence("leave")).await;
    assert_eq!(left["members"][0]["conn_id"], b_conn.as_str());

    // A reconnect within the window replaces the member in one message.
    let mut b2 = Ws::open(&addr, Some(&tb)).await;
    assert_eq!(
        b2.request(json!({"t": "join", "channel": "room:1"}), 1)
            .await["t"],
        "ack"
    );
    let first = a.wait(presence("join")).await;
    let old = first["members"][0]["conn_id"].as_str().unwrap().to_owned();
    drop(b2);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let mut b3 = Ws::open(&addr, Some(&tb)).await;
    assert_eq!(
        b3.request(json!({"t": "join", "channel": "room:1"}), 1)
            .await["t"],
        "ack"
    );
    let again = a.wait(presence("join")).await;
    assert_eq!(again["replaced"], old.as_str(), "{again}");
    assert!(!a.gets_within(4500, presence("leave")).await, "no flap");

    // Explicit leave.
    assert_eq!(
        b3.request(json!({"t": "leave", "channel": "room:1"}), 2)
            .await["t"],
        "ack"
    );
    a.wait(presence("leave")).await;

    // Deleting the definition removes the members.
    let (s, _, _) = call(
        &t.router,
        Method::DELETE,
        &format!("/api/channels/{def_id}"),
        su,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let kicked = a
        .wait(|v| v["t"] == "presence" && v["kind"] == "leave" && v["reason"] == "forbidden")
        .await;
    assert_eq!(kicked["members"][0]["user_id"], ida.as_str());
    let r = a
        .request(json!({"t": "join", "channel": "room:1"}), 9)
        .await;
    assert_eq!(r["t"], "err");
}
