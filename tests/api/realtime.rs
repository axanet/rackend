//! M4 acceptance: realtime record subscriptions over WebSocket and SSE
//! against a real TCP server (SPEC §13.2).

use std::time::Duration;

use axum::http::{Method, StatusCode};
use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;

use crate::common::*;

const PW: &str = "a long user password";

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn serve(t: &TestApp) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(rackend_http::serve(
        listener,
        t.router.clone(),
        std::future::pending(),
    ));
    format!("127.0.0.1:{port}")
}

struct Ws {
    s: WsStream,
}

impl Ws {
    async fn open(addr: &str, token: Option<&str>) -> Self {
        let (s, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/api/ws"))
            .await
            .unwrap();
        let mut ws = Self { s };
        let hello = ws.next().await.expect("connect message");
        assert_eq!(hello["t"], "connect");
        if let Some(tok) = token {
            ws.send(json!({"t": "auth", "token": tok})).await;
            let a = ws.next().await.unwrap();
            assert_eq!(a["t"], "ack", "{a}");
        }
        ws
    }

    async fn send(&mut self, v: Value) {
        self.s
            .send(Message::Text(v.to_string().into()))
            .await
            .unwrap();
    }

    /// The next JSON message within 5 s; `None` on close or timeout.
    async fn next(&mut self) -> Option<Value> {
        loop {
            let m = tokio::time::timeout(Duration::from_secs(5), self.s.next())
                .await
                .ok()??
                .ok()?;
            match m {
                Message::Text(t) => return serde_json::from_str(&t).ok(),
                Message::Close(f) => {
                    return Some(json!({"t": "closed", "reason": f.map(|f| f.reason.to_string())}));
                }
                _ => {}
            }
        }
    }

    async fn sub(&mut self, id: u64, topic: &str, extra: Value) -> Value {
        let mut m = json!({"t": "sub", "id": id, "topic": topic});
        if let Value::Object(e) = extra {
            m.as_object_mut().unwrap().extend(e);
        }
        self.send(m).await;
        self.next().await.unwrap()
    }

    /// Record events received before the next `pings` event (the sentinel).
    async fn until_sentinel(&mut self) -> Vec<Value> {
        let mut out = Vec::new();
        loop {
            let m = self.next().await.expect("sentinel did not arrive");
            if m["t"] == "record" && m["topic"] == "records:pings" {
                return out;
            }
            out.push(m);
        }
    }
}

async fn user(t: &TestApp, email: &str) -> (String, String) {
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

async fn setup(t: &TestApp) {
    for body in [
        json!({"name": "pings", "fields": [{"name": "n", "type": "number"}], "rules": {"list": "", "view": "", "create": null, "update": null, "delete": null}}),
        json!({"name": "secrets", "fields": [{"name": "label", "type": "text"}]}),
    ] {
        let (s, _, out) = call(
            &t.router,
            Method::POST,
            "/api/collections",
            Some(&t.token),
            Some(body),
        )
        .await;
        assert_eq!(s, StatusCode::CREATED, "{out}");
    }
    let secrets = t
        .app
        .schema
        .get()
        .schema
        .by_id_or_name("secrets")
        .unwrap()
        .id
        .to_string();
    let (s, _, out) = call(&t.router, Method::POST, "/api/collections", Some(&t.token), Some(json!({
        "name": "posts",
        "fields": [
            {"name": "title", "type": "text"},
            {"name": "owner", "type": "text"},
            {"name": "secret", "type": "relation", "collection_id": secrets},
        ],
        "rules": {"list": "owner = @request.auth.id", "view": "owner = @request.auth.id", "create": null, "update": null, "delete": null},
    }))).await;
    assert_eq!(s, StatusCode::CREATED, "{out}");
}

async fn su(t: &TestApp, method: Method, uri: &str, body: Option<Value>) -> Value {
    let (s, _, out) = call(&t.router, method, uri, Some(&t.token), body).await;
    assert!(s.is_success(), "{s} {out}");
    out
}

async fn sentinel(t: &TestApp) {
    su(
        t,
        Method::POST,
        "/api/collections/pings/records",
        Some(json!({"n": 1})),
    )
    .await;
}

#[tokio::test]
async fn events_follow_view_rules_filters_and_expand_rules() {
    let t = start().await;
    setup(&t).await;
    let addr = serve(&t).await;
    let (ada_tok, ada) = user(&t, "ada@example.com").await;
    let (bob_tok, bob) = user(&t, "bob@example.com").await;

    let mut wa = Ws::open(&addr, Some(&ada_tok)).await;
    let mut wb = Ws::open(&addr, Some(&bob_tok)).await;
    let mut wf = Ws::open(&addr, Some(&ada_tok)).await;
    let mut ws = Ws::open(&addr, Some(&t.token)).await;
    for (w, extra) in [
        (&mut wa, json!({"expand": "secret"})),
        (&mut wb, json!({})),
        (&mut wf, json!({"filter": "title = 'match'"})),
        (&mut ws, json!({"expand": "secret"})),
    ] {
        assert_eq!(w.sub(1, "records:posts", extra).await["t"], "ack");
        assert_eq!(w.sub(2, "records:pings", json!({})).await["t"], "ack");
    }

    let secret = su(
        &t,
        Method::POST,
        "/api/collections/secrets/records",
        Some(json!({"label": "hidden"})),
    )
    .await;
    // Bob's post: Ada sees nothing.
    let p1 = su(
        &t,
        Method::POST,
        "/api/collections/posts/records",
        Some(json!({"title": "b", "owner": bob})),
    )
    .await;
    sentinel(&t).await;
    assert!(wa.until_sentinel().await.is_empty());
    let got = wb.until_sentinel().await;
    assert_eq!(got.len(), 1);
    assert_eq!(
        (got[0]["action"].as_str(), got[0]["record"]["id"].as_str()),
        (Some("create"), p1["id"].as_str())
    );
    assert!(wf.until_sentinel().await.is_empty());
    assert_eq!(
        ws.until_sentinel().await.len(),
        1,
        "superusers see everything"
    );

    // Ada's post with a relation to a locked collection: no expansion leaks it.
    let p2 = su(
        &t,
        Method::POST,
        "/api/collections/posts/records",
        Some(json!({"title": "match", "owner": ada, "secret": secret["id"]})),
    )
    .await;
    sentinel(&t).await;
    let got = wa.until_sentinel().await;
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0]["action"], "create");
    assert!(
        got[0]["record"]
            .get("expand")
            .and_then(|e| e.get("secret"))
            .is_none(),
        "{}",
        got[0]
    );
    assert!(wb.until_sentinel().await.is_empty());
    assert_eq!(wf.until_sentinel().await.len(), 1, "the filter matches");
    let su_events = ws.until_sentinel().await;
    let su_p2 = su_events
        .iter()
        .find(|e| e["record"]["id"] == p2["id"])
        .unwrap();
    assert_eq!(
        su_p2["record"]["expand"]["secret"]["label"], "hidden",
        "superusers get the expansion"
    );

    // Moving it to Bob: Ada's view ends, Bob's begins.
    su(
        &t,
        Method::PATCH,
        &format!(
            "/api/collections/posts/records/{}",
            p2["id"].as_str().unwrap()
        ),
        Some(json!({"owner": bob, "title": "moved"})),
    )
    .await;
    sentinel(&t).await;
    let got = wa.until_sentinel().await;
    assert_eq!(got.len(), 1);
    assert_eq!(
        (got[0]["action"].as_str(), got[0]["reason"].as_str()),
        (Some("delete"), Some("no_longer_visible"))
    );
    let got = wb.until_sentinel().await;
    assert_eq!(got[0]["action"], "update");
    let got = wf.until_sentinel().await;
    assert_eq!(
        (got[0]["action"].as_str(), got[0]["reason"].as_str()),
        (Some("delete"), Some("no_longer_visible"))
    );

    // Deletes are judged by the record as it was.
    su(
        &t,
        Method::DELETE,
        &format!(
            "/api/collections/posts/records/{}",
            p2["id"].as_str().unwrap()
        ),
        None,
    )
    .await;
    sentinel(&t).await;
    assert!(wa.until_sentinel().await.is_empty());
    let got = wb.until_sentinel().await;
    assert_eq!(
        (got[0]["action"].as_str(), got[0]["record"]["id"].as_str()),
        (Some("delete"), p2["id"].as_str())
    );
}

#[tokio::test]
async fn single_record_topics_unsubscribe_and_errors() {
    let t = start().await;
    setup(&t).await;
    let addr = serve(&t).await;
    let (tok, ada) = user(&t, "ada@example.com").await;
    let a = su(
        &t,
        Method::POST,
        "/api/collections/posts/records",
        Some(json!({"title": "a", "owner": ada})),
    )
    .await;
    let b = su(
        &t,
        Method::POST,
        "/api/collections/posts/records",
        Some(json!({"title": "b", "owner": ada})),
    )
    .await;
    let mut w = Ws::open(&addr, Some(&tok)).await;
    assert_eq!(
        w.sub(
            1,
            &format!("records:posts/{}", a["id"].as_str().unwrap()),
            json!({})
        )
        .await["t"],
        "ack"
    );
    assert_eq!(w.sub(2, "records:pings", json!({})).await["t"], "ack");
    for id in [&a["id"], &b["id"]] {
        su(
            &t,
            Method::PATCH,
            &format!("/api/collections/posts/records/{}", id.as_str().unwrap()),
            Some(json!({"title": "x"})),
        )
        .await;
    }
    sentinel(&t).await;
    let got = w.until_sentinel().await;
    assert_eq!(got.len(), 1);
    assert_eq!(got[0]["record"]["id"], a["id"]);
    assert_eq!(
        got[0]["topic"],
        format!("records:posts/{}", a["id"].as_str().unwrap())
    );

    w.send(json!({"t": "unsub", "id": 1})).await;
    assert_eq!(w.next().await.unwrap()["t"], "ack");
    su(
        &t,
        Method::PATCH,
        &format!(
            "/api/collections/posts/records/{}",
            a["id"].as_str().unwrap()
        ),
        Some(json!({"title": "y"})),
    )
    .await;
    sentinel(&t).await;
    assert!(w.until_sentinel().await.is_empty());

    // Errors: locked collections, bad filters, bad topics, channels.
    let e = w.sub(3, "records:secrets", json!({})).await;
    assert_eq!(
        (e["t"].as_str(), e["code"].as_str(), e["id"].as_u64()),
        (Some("err"), Some("forbidden"), Some(3))
    );
    let e = w
        .sub(4, "records:posts", json!({"filter": "title ="}))
        .await;
    assert_eq!(e["code"], "rule_compile_error");
    let e = w.sub(5, "posts", json!({})).await;
    assert_eq!(e["code"], "validation_failed");
    let e = w.sub(6, "records:nope", json!({})).await;
    assert_eq!(e["code"], "not_found");
    w.send(json!({"t": "join", "id": 7, "channel": "room:1"}))
        .await;
    assert_eq!(w.next().await.unwrap()["t"], "err");
    w.send(json!({"t": "ping"})).await;
    assert_eq!(w.next().await.unwrap()["t"], "pong");
    w.send(json!({"t": "auth", "token": "rk_at_bogus"})).await;
    assert_eq!(w.next().await.unwrap()["code"], "unauthorized");
}

#[tokio::test]
async fn revoked_sessions_are_disconnected() {
    let t = start().await;
    setup(&t).await;
    let addr = serve(&t).await;
    let (tok, _) = user(&t, "ada@example.com").await;
    let mut w = Ws::open(&addr, Some(&tok)).await;
    assert_eq!(w.sub(1, "records:pings", json!({})).await["t"], "ack");
    let started = std::time::Instant::now();
    let (s, _, _) = call(
        &t.router,
        Method::POST,
        "/api/collections/users/logout",
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let m = w.next().await.expect("closed");
    assert_eq!(m["t"], "closed", "{m}");
    assert_eq!(m["reason"], "session_revoked");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );

    // Another account cannot take over a bound connection.
    let (bob, _) = user(&t, "bob@example.com").await;
    let (other, _) = user(&t, "cy@example.com").await;
    let mut w = Ws::open(&addr, Some(&bob)).await;
    w.send(json!({"t": "auth", "token": other})).await;
    assert_eq!(w.next().await.unwrap()["code"], "forbidden");
}

/// A minimal SSE client: events as `(event, data, id)`.
struct Sse {
    res: reqwest::Response,
    buf: String,
}

impl Sse {
    async fn open(addr: &str, last_id: Option<&str>) -> Self {
        let mut req = reqwest::Client::new().get(format!("http://{addr}/api/realtime"));
        if let Some(id) = last_id {
            req = req.header("last-event-id", id);
        }
        let res = req.send().await.unwrap();
        assert_eq!(res.status(), 200);
        assert_eq!(res.headers()["content-type"], "text/event-stream");
        Self {
            res,
            buf: String::new(),
        }
    }

    async fn next(&mut self) -> Option<(String, Value, Option<String>)> {
        loop {
            if let Some(i) = self.buf.find("\n\n") {
                let block: String = self.buf.drain(..i + 2).collect();
                let (mut ev, mut data, mut id) = (String::from("message"), String::new(), None);
                for line in block.lines() {
                    if let Some(v) = line.strip_prefix("event:") {
                        ev = v.trim().to_owned();
                    } else if let Some(v) = line.strip_prefix("data:") {
                        data.push_str(v.trim());
                    } else if let Some(v) = line.strip_prefix("id:") {
                        id = Some(v.trim().to_owned());
                    }
                }
                if data.is_empty() {
                    continue; // keep-alive comment
                }
                return Some((ev, serde_json::from_str(&data).unwrap_or(Value::Null), id));
            }
            let chunk = tokio::time::timeout(Duration::from_secs(5), self.res.chunk())
                .await
                .ok()?
                .ok()??;
            self.buf.push_str(&String::from_utf8_lossy(&chunk));
        }
    }
}

async fn bind(addr: &str, token: &str, client: &str, subs: Value) -> StatusCode {
    let res = reqwest::Client::new()
        .post(format!("http://{addr}/api/realtime"))
        .bearer_auth(token)
        .json(&json!({"client_id": client, "subscriptions": subs}))
        .send()
        .await
        .unwrap();
    StatusCode::from_u16(res.status().as_u16()).unwrap()
}

#[tokio::test]
async fn sse_binds_resumes_and_cannot_be_hijacked() {
    let t = start().await;
    setup(&t).await;
    let addr = serve(&t).await;
    let (bob_tok, bob) = user(&t, "bob@example.com").await;
    let (ada_tok, _) = user(&t, "ada@example.com").await;

    let mut sse = Sse::open(&addr, None).await;
    let (ev, data, _) = sse.next().await.unwrap();
    assert_eq!(ev, "connect");
    let client = data["client_id"].as_str().unwrap().to_owned();
    assert_eq!(
        bind(
            &addr,
            &bob_tok,
            &client,
            json!([{"topic": "records:posts"}])
        )
        .await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        bind(&addr, &ada_tok, &client, json!([])).await,
        StatusCode::FORBIDDEN,
        "bound to Bob"
    );
    assert_eq!(
        bind(&addr, &bob_tok, "no-such-client", json!([])).await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        bind(
            &addr,
            &bob_tok,
            &client,
            json!([{"topic": "records:posts", "filter": "nope ="}])
        )
        .await,
        StatusCode::BAD_REQUEST
    );

    let p1 = su(
        &t,
        Method::POST,
        "/api/collections/posts/records",
        Some(json!({"title": "1", "owner": bob})),
    )
    .await;
    let (ev, data, id) = sse.next().await.unwrap();
    assert_eq!(ev, "records:posts");
    assert_eq!(
        (data["type"].as_str(), data["action"].as_str()),
        (Some("record"), Some("create"))
    );
    assert_eq!(data["record"]["id"], p1["id"]);
    let id = id.expect("events carry ids");
    assert!(id.starts_with(&format!("{client}.")));
    drop(sse);

    // Missed while disconnected, replayed on resume.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let p2 = su(
        &t,
        Method::POST,
        "/api/collections/posts/records",
        Some(json!({"title": "2", "owner": bob})),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut sse = Sse::open(&addr, Some(&id)).await;
    let (ev, data, _) = sse.next().await.unwrap();
    assert_eq!(
        (
            ev.as_str(),
            data["client_id"].as_str(),
            data["resumed"].as_bool()
        ),
        ("connect", Some(client.as_str()), Some(true))
    );
    let (_, data, _) = sse.next().await.unwrap();
    assert_eq!(data["record"]["id"], p2["id"], "replayed");
}

#[tokio::test]
async fn limits_apply() {
    let t = start().await;
    setup(&t).await;
    let addr = serve(&t).await;
    su(&t, Method::PATCH, "/api/settings", Some(json!({"realtime": {"max_subscriptions_per_connection": 1, "max_connections_per_ip": 2}}))).await;
    let mut a = Ws::open(&addr, None).await;
    assert_eq!(a.sub(1, "records:pings", json!({})).await["t"], "ack");
    let e = a.sub(2, "records:pings", json!({"filter": "n > 1"})).await;
    assert_eq!(e["errors"]["topic"][0]["code"], "too_many", "{e}");
    let _b = Ws::open(&addr, None).await;
    let third = tokio_tungstenite::connect_async(format!("ws://{addr}/api/ws")).await;
    match third {
        Err(tokio_tungstenite::tungstenite::Error::Http(res)) => assert_eq!(res.status(), 429),
        other => panic!("expected 429, got {other:?}"),
    }
}

#[tokio::test]
async fn cookie_websockets_need_an_allowed_origin() {
    let t = start().await;
    let addr = serve(&t).await;
    let r = send(&t.router, {
        let mut req = request(
            Method::POST,
            "/api/superusers/auth-with-password",
            None,
            Some(json!({"email": ADMIN_EMAIL, "password": ADMIN_PASSWORD})),
        );
        req.headers_mut()
            .insert("x-rackend-auth-mode", "cookie".parse().unwrap());
        req
    })
    .await;
    let cookie = r
        .headers
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|c| c.starts_with("rk_session="))
        .and_then(|c| c.split(';').next())
        .unwrap()
        .to_owned();
    let attempt = |origin: &'static str| {
        let addr = addr.clone();
        let cookie = cookie.clone();
        async move {
            let mut req = format!("ws://{addr}/api/ws").into_client_request().unwrap();
            req.headers_mut().insert("cookie", cookie.parse().unwrap());
            req.headers_mut().insert("origin", origin.parse().unwrap());
            tokio_tungstenite::connect_async(req).await
        }
    };
    match attempt("https://evil.example").await {
        Err(tokio_tungstenite::tungstenite::Error::Http(res)) => assert_eq!(res.status(), 403),
        other => panic!("expected 403, got {:?}", other.map(|_| ())),
    }
    let own = Box::leak(format!("http://{addr}").into_boxed_str());
    assert!(attempt(own).await.is_ok(), "same origin works");
}

#[tokio::test]
async fn closed_websockets_leave_no_state_behind() {
    let t = start().await;
    setup(&t).await;
    let addr = serve(&t).await;
    let mut conns = Vec::new();
    for _ in 0..50 {
        let mut w = Ws::open(&addr, None).await;
        assert_eq!(w.sub(1, "records:pings", json!({})).await["t"], "ack");
        conns.push(w);
    }
    assert_eq!(t.app.hub.len(), 50);
    for mut w in conns {
        w.s.close(None).await.unwrap();
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !t.app.hub.is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "{} clients left",
            t.app.hub.len()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Abrupt drops (no close frame) are cleaned up too.
    let w = Ws::open(&addr, None).await;
    drop(w);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !t.app.hub.is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "dropped socket not cleaned up"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
