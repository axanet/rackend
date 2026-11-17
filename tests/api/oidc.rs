//! M8: the OIDC provider end to end over HTTP (SPEC §11.8).

use std::collections::BTreeMap;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::common::*;

const ISS: &str = "https://id.example.com";
const APP: &str = "https://app.example.com/callback";
const PW: &str = "a long user password";

struct Oidc {
    t: TestApp,
    secret: String,
    client_id: String,
}

async fn setup() -> Oidc {
    let t = start().await;
    t.app.ensure_signing_keys().await.unwrap();
    let (s, _, out) = call(
        &t.router,
        Method::PATCH,
        "/api/settings",
        Some(&t.token),
        Some(json!({"app": {"public_url": ISS}})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{out}");
    let (s, _, c) = call(
        &t.router,
        Method::POST,
        "/api/oauth-clients",
        Some(&t.token),
        Some(json!({
            "name": "Server app", "kind": "confidential", "collection": "users",
            "redirect_uris": [APP], "post_logout_redirect_uris": ["https://app.example.com/bye"],
            "first_party": true,
        })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{c}");
    let secret = c["client_secret"].as_str().unwrap().to_owned();
    let (s, _, out) = call(
        &t.router,
        Method::POST,
        "/api/collections/users/records",
        None,
        Some(json!({"email": "ada@example.com", "password": PW, "name": "Ada"})),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "{out}");
    Oidc {
        t,
        secret,
        client_id: c["id"].as_str().unwrap().to_owned(),
    }
}

fn q(params: &[(&str, &str)]) -> String {
    params
        .iter()
        .map(|(k, v)| format!("{k}={}", rackend_core::usecases::oidc::urlencode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn location(r: &Reply) -> String {
    r.headers["location"].to_str().unwrap().to_owned()
}

fn query_of(url: &str) -> BTreeMap<String, String> {
    let qs = url.split_once('?').map_or("", |(_, q)| q);
    qs.split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| {
            let v = v.replace('+', " ");
            let mut out = Vec::new();
            let b = v.as_bytes();
            let mut i = 0;
            while i < b.len() {
                if b[i] == b'%' && i + 2 < b.len() + 1 {
                    out.push(u8::from_str_radix(&v[i + 1..i + 3], 16).unwrap());
                    i += 3;
                } else {
                    out.push(b[i]);
                    i += 1;
                }
            }
            (k.to_owned(), String::from_utf8(out).unwrap())
        })
        .collect()
}

async fn get(t: &TestApp, uri: &str, cookie: Option<&str>) -> Reply {
    let mut b = Request::builder().uri(uri);
    if let Some(c) = cookie {
        b = b.header("cookie", c);
    }
    send(&t.router, b.body(Body::empty()).unwrap()).await
}

async fn post_form(
    t: &TestApp,
    uri: &str,
    fields: &[(&str, &str)],
    cookie: Option<&str>,
    basic: Option<(&str, &str)>,
) -> Reply {
    let mut b = Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header("content-type", "application/x-www-form-urlencoded");
    if let Some(c) = cookie {
        b = b.header("cookie", c);
    }
    if let Some((id, s)) = basic {
        b = b.header(
            "authorization",
            format!("Basic {}", STANDARD.encode(format!("{id}:{s}"))),
        );
    }
    send(&t.router, b.body(Body::from(q(fields))).unwrap()).await
}

fn verifier_challenge() -> (String, String) {
    let v = "a-pkce-verifier-of-sufficient-length-1234567890".to_owned();
    let c = URL_SAFE_NO_PAD.encode(Sha256::digest(v.as_bytes()));
    (v, c)
}

/// Authorize, sign in on the hosted page; returns the code and the SSO cookie.
async fn sign_in(o: &Oidc, extra: &[(&str, &str)]) -> (BTreeMap<String, String>, String) {
    let (_, challenge) = verifier_challenge();
    let mut params = vec![
        ("response_type", "code"),
        ("client_id", o.client_id.as_str()),
        ("redirect_uri", APP),
        ("scope", "openid email profile offline_access"),
        ("state", "st4te"),
        ("nonce", "n0nce"),
        ("code_challenge", challenge.as_str()),
        ("code_challenge_method", "S256"),
    ];
    params.extend_from_slice(extra);
    let r = get(&o.t, &format!("/oauth2/authorize?{}", q(&params)), None).await;
    assert_eq!(
        r.status,
        StatusCode::FOUND,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let login = location(&r);
    assert!(login.starts_with("/auth/login?ar="), "{login}");
    let page = get(&o.t, &login, None).await;
    assert_eq!(page.status, StatusCode::OK);
    let html = String::from_utf8_lossy(&page.body).to_string();
    assert!(
        html.contains("Server app") && html.contains("name=\"password\""),
        "{html}"
    );
    let csp = page.headers["content-security-policy"].to_str().unwrap();
    assert!(
        csp.contains("form-action 'self' https://app.example.com")
            && csp.contains("script-src 'self'"),
        "{csp}"
    );
    let ar = query_of(&login)["ar"].clone();
    let r = post_form(
        &o.t,
        "/auth/login",
        &[("ar", &ar), ("email", "ada@example.com"), ("password", PW)],
        None,
        None,
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::SEE_OTHER,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let cookie = r.headers["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    assert!(cookie.starts_with("rk_sso="));
    let r = get(&o.t, &location(&r), Some(&cookie)).await;
    assert_eq!(r.status, StatusCode::FOUND);
    let back = location(&r);
    assert!(back.starts_with(APP), "{back}");
    (query_of(&back), cookie)
}

async fn exchange(o: &Oidc, code: &str) -> Reply {
    let (verifier, _) = verifier_challenge();
    post_form(
        &o.t,
        "/oauth2/token",
        &[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", APP),
            ("code_verifier", &verifier),
        ],
        None,
        Some((&o.client_id, &o.secret)),
    )
    .await
}

fn claims(jwt: &str) -> Value {
    let p = jwt.split('.').nth(1).unwrap();
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(p).unwrap()).unwrap()
}

#[tokio::test]
async fn discovery_and_jwks() {
    let o = setup().await;
    let (s, h, d) = call(
        &o.t.router,
        Method::GET,
        "/.well-known/openid-configuration",
        None,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(h["access-control-allow-origin"], "*");
    assert_eq!(d["issuer"], ISS);
    assert_eq!(d["token_endpoint"], format!("{ISS}/oauth2/token"));
    assert_eq!(d["code_challenge_methods_supported"], json!(["S256"]));
    let (s, _, k) = call(&o.t.router, Method::GET, "/oauth2/jwks", None, None).await;
    assert_eq!(s, StatusCode::OK);
    let algs: Vec<&str> = k["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k["alg"].as_str().unwrap())
        .collect();
    assert!(algs.contains(&"RS256") && algs.contains(&"ES256"), "{k}");
    assert!(!k.to_string().contains("\"d\""), "no private parts");
}

#[tokio::test]
async fn code_flow_tokens_userinfo_and_api_access() {
    let o = setup().await;
    let (params, _cookie) = sign_in(&o, &[]).await;
    assert_eq!(params["state"], "st4te");
    assert_eq!(params["iss"], ISS);
    let r = exchange(&o, &params["code"]).await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    assert_eq!(r.headers["cache-control"], "no-store");
    let tok = r.json();
    assert_eq!(tok["token_type"], "Bearer");
    let id = claims(tok["id_token"].as_str().unwrap());
    assert_eq!(id["iss"], ISS);
    assert_eq!(id["aud"], o.client_id.as_str());
    assert_eq!(id["nonce"], "n0nce");
    assert_eq!(id["email"], "ada@example.com");
    assert_eq!(id["name"], "Ada");
    assert!(id["auth_time"].is_i64() && id["sid"].is_string());
    let access = tok["access_token"].as_str().unwrap().to_owned();
    let at = claims(&access);
    assert_eq!(at["aud"], json!([o.client_id, format!("{ISS}/api")]));

    // UserInfo.
    let (s, _, ui) = call(
        &o.t.router,
        Method::GET,
        "/oauth2/userinfo",
        Some(&access),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{ui}");
    assert_eq!(ui["sub"], id["sub"]);
    assert_eq!(ui["email"], "ada@example.com");

    // rackend's own API accepts the access token.
    let uid = id["sub"].as_str().unwrap();
    let (s, _, rec) = call(
        &o.t.router,
        Method::GET,
        &format!("/api/collections/users/records/{uid}"),
        Some(&access),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{rec}");

    // The code is single use; reuse ends the session it created.
    let r = exchange(&o, &params["code"]).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.json()["error"], "invalid_grant");
    let (s, _, _) = call(
        &o.t.router,
        Method::GET,
        "/oauth2/userinfo",
        Some(&access),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "revoked at once");
    let (s, _, _) = call(
        &o.t.router,
        Method::GET,
        &format!("/api/collections/users/records/{uid}"),
        Some(&access),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn refresh_rotation_and_reuse() {
    let o = setup().await;
    let (params, _) = sign_in(&o, &[]).await;
    let tok = exchange(&o, &params["code"]).await.json();
    let rt1 = tok["refresh_token"].as_str().unwrap().to_owned();
    let auth = Some((o.client_id.as_str(), o.secret.as_str()));
    let r = post_form(
        &o.t,
        "/oauth2/token",
        &[("grant_type", "refresh_token"), ("refresh_token", &rt1)],
        None,
        auth,
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let tok2 = r.json();
    let rt2 = tok2["refresh_token"].as_str().unwrap().to_owned();
    assert_ne!(rt1, rt2);
    assert_eq!(
        claims(tok2["id_token"].as_str().unwrap())["sid"],
        claims(tok["id_token"].as_str().unwrap())["sid"]
    );
    // A refresh cannot widen the scope.
    let r = post_form(
        &o.t,
        "/oauth2/token",
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", &rt2),
            ("scope", "openid admin"),
        ],
        None,
        auth,
    )
    .await;
    assert_eq!(r.json()["error"], "invalid_scope");
    // Wrong client secret.
    let r = post_form(
        &o.t,
        "/oauth2/token",
        &[("grant_type", "refresh_token"), ("refresh_token", &rt2)],
        None,
        Some((&o.client_id, "nope")),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(r.json()["error"], "invalid_client");
    // Revocation (RFC 7009) ends the session.
    let r = post_form(&o.t, "/oauth2/revoke", &[("token", &rt2)], None, auth).await;
    assert_eq!(r.status, StatusCode::OK);
    let access = tok2["access_token"].as_str().unwrap();
    let (s, _, _) = call(
        &o.t.router,
        Method::GET,
        "/oauth2/userinfo",
        Some(access),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn sso_prompts_and_errors() {
    let o = setup().await;
    let (_, cookie) = sign_in(&o, &[]).await;
    let (_, challenge) = verifier_challenge();
    let base = |extra: &[(&str, &str)]| {
        let mut p = vec![
            ("response_type", "code"),
            ("client_id", o.client_id.as_str()),
            ("redirect_uri", APP),
            ("scope", "openid"),
            ("state", "s2"),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
        ];
        p.extend_from_slice(extra);
        format!("/oauth2/authorize?{}", q(&p))
    };
    // Signed in: straight back with a code.
    let r = get(&o.t, &base(&[]), Some(&cookie)).await;
    assert!(query_of(&location(&r)).contains_key("code"));
    // prompt=none without a session.
    let r = get(&o.t, &base(&[("prompt", "none")]), None).await;
    let p = query_of(&location(&r));
    assert_eq!(p["error"], "login_required");
    assert_eq!(p["state"], "s2");
    // prompt=login asks again even with a session; max_age=0 too.
    for extra in [("prompt", "login"), ("max_age", "0")] {
        let r = get(&o.t, &base(&[extra]), Some(&cookie)).await;
        assert!(location(&r).starts_with("/auth/login?ar="), "{extra:?}");
    }
    // Unregistered redirect URI: an error page, never a redirect.
    let r = get(
        &o.t,
        &format!(
            "/oauth2/authorize?{}",
            q(&[
                ("response_type", "code"),
                ("client_id", &o.client_id),
                ("redirect_uri", "https://evil.example.com/cb"),
                ("scope", "openid")
            ])
        ),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(r.headers.get("location").is_none());
    // Missing openid scope, wrong response type.
    let r = get(
        &o.t,
        &base(&[]).replace("scope=openid", "scope=email"),
        None,
    )
    .await;
    assert_eq!(query_of(&location(&r))["error"], "invalid_scope");
    let r = get(
        &o.t,
        &base(&[]).replace("response_type=code", "response_type=token"),
        None,
    )
    .await;
    assert_eq!(
        query_of(&location(&r))["error"],
        "unsupported_response_type"
    );
    // Login CSRF: a cross-site form post is refused.
    let mut req = Request::builder()
        .method(Method::POST)
        .uri("/auth/login")
        .header("content-type", "application/x-www-form-urlencoded")
        .header("origin", "https://evil.example.com");
    req = req.header("x", "y");
    let r = send(
        &o.t.router,
        req.body(Body::from("ar=x&email=a&password=b")).unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // Wrong password stays on the page.
    let r = get(&o.t, &base(&[("prompt", "login")]), Some(&cookie)).await;
    let ar = query_of(&location(&r))["ar"].clone();
    let r = post_form(
        &o.t,
        "/auth/login",
        &[
            ("ar", &ar),
            ("email", "ada@example.com"),
            ("password", "wrong password!"),
        ],
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert!(String::from_utf8_lossy(&r.body).contains("not correct"));
}

#[tokio::test]
async fn public_clients_need_pkce_and_third_parties_need_consent() {
    let o = setup().await;
    let (s, _, c) = call(
        &o.t.router,
        Method::POST,
        "/api/oauth-clients",
        Some(&o.t.token),
        Some(json!({"name": "Partner SPA", "kind": "public", "collection": "users", "redirect_uris": [APP], "require_pkce": false})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{c}");
    assert_eq!(c["require_pkce"], true, "public clients always need PKCE");
    assert!(c.get("client_secret").is_none());
    let pid = c["id"].as_str().unwrap().to_owned();
    let r = get(
        &o.t,
        &format!(
            "/oauth2/authorize?{}",
            q(&[
                ("response_type", "code"),
                ("client_id", &pid),
                ("redirect_uri", APP),
                ("scope", "openid email")
            ])
        ),
        None,
    )
    .await;
    assert_eq!(query_of(&location(&r))["error"], "invalid_request");

    let (_, cookie) = sign_in(&o, &[]).await;
    let (verifier, challenge) = verifier_challenge();
    let url = format!(
        "/oauth2/authorize?{}",
        q(&[
            ("response_type", "code"),
            ("client_id", &pid),
            ("redirect_uri", APP),
            ("scope", "openid email"),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256")
        ])
    );
    let r = get(&o.t, &url, Some(&cookie)).await;
    let consent = location(&r);
    assert!(consent.starts_with("/auth/consent?ar="), "{consent}");
    let page = get(&o.t, &consent, Some(&cookie)).await;
    let html = String::from_utf8_lossy(&page.body).to_string();
    assert!(
        html.contains("Partner SPA") && html.contains("See your email address"),
        "{html}"
    );
    let ar = query_of(&consent)["ar"].clone();
    // prompt=none before consent.
    let r = get(&o.t, &format!("{url}&prompt=none"), Some(&cookie)).await;
    assert_eq!(query_of(&location(&r))["error"], "consent_required");
    let r = post_form(
        &o.t,
        "/auth/consent",
        &[("ar", &ar), ("decision", "allow")],
        Some(&cookie),
        None,
    )
    .await;
    let back = query_of(&location(&r));
    let r = post_form(
        &o.t,
        "/oauth2/token",
        &[
            ("grant_type", "authorization_code"),
            ("code", &back["code"]),
            ("redirect_uri", APP),
            ("code_verifier", &verifier),
            ("client_id", &pid),
        ],
        None,
        None,
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    assert!(
        r.json().get("refresh_token").is_none(),
        "no offline_access, no refresh token"
    );
    // Consent is remembered.
    let r = get(&o.t, &url, Some(&cookie)).await;
    assert!(query_of(&location(&r)).contains_key("code"));
    // A wrong verifier fails.
    let code = query_of(&location(&get(&o.t, &url, Some(&cookie)).await))["code"].clone();
    let r = post_form(
        &o.t,
        "/oauth2/token",
        &[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("redirect_uri", APP),
            (
                "code_verifier",
                "wrong-verifier-wrong-verifier-wrong-verifier",
            ),
            ("client_id", &pid),
        ],
        None,
        None,
    )
    .await;
    assert_eq!(r.json()["error"], "invalid_grant");
}

#[tokio::test]
async fn logout_and_key_rotation() {
    let o = setup().await;
    let (params, cookie) = sign_in(&o, &[]).await;
    let tok = exchange(&o, &params["code"]).await.json();
    let id_token = tok["id_token"].as_str().unwrap().to_owned();
    let access = tok["access_token"].as_str().unwrap().to_owned();

    // Without a hint the user confirms first.
    let r = get(&o.t, "/oauth2/logout", Some(&cookie)).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(String::from_utf8_lossy(&r.body).contains("name=\"confirm\""));

    // Emergency rotation: old tokens still verify (old key stays in the JWKS).
    let (s, _, out) = call(
        &o.t.router,
        Method::POST,
        "/api/keys/rotate",
        Some(&o.t.token),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{out}");
    let (_, _, keys) = call(
        &o.t.router,
        Method::GET,
        "/api/keys",
        Some(&o.t.token),
        None,
    )
    .await;
    let states: Vec<&str> = keys["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k["state"].as_str().unwrap())
        .collect();
    assert_eq!(
        states.iter().filter(|s| **s == "active").count(),
        2,
        "{keys}"
    );
    assert_eq!(
        states.iter().filter(|s| **s == "retired").count(),
        2,
        "{keys}"
    );
    let (s, _, _) = call(
        &o.t.router,
        Method::GET,
        "/oauth2/userinfo",
        Some(&access),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);

    // With a hint: signed out at once, back to the registered URI.
    let url = format!(
        "/oauth2/logout?{}",
        q(&[
            ("id_token_hint", &id_token),
            ("post_logout_redirect_uri", "https://app.example.com/bye"),
            ("state", "x1")
        ])
    );
    let r = get(&o.t, &url, Some(&cookie)).await;
    assert_eq!(r.status, StatusCode::FOUND);
    assert_eq!(location(&r), "https://app.example.com/bye?state=x1");
    assert!(
        r.headers["set-cookie"]
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );
    // The browser session and the client session it started are gone.
    let (s, _, _) = call(
        &o.t.router,
        Method::GET,
        "/oauth2/userinfo",
        Some(&access),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (_, challenge) = verifier_challenge();
    let r = get(
        &o.t,
        &format!(
            "/oauth2/authorize?{}",
            q(&[
                ("response_type", "code"),
                ("client_id", &o.client_id),
                ("redirect_uri", APP),
                ("scope", "openid"),
                ("prompt", "none"),
                ("code_challenge", &challenge),
                ("code_challenge_method", "S256")
            ])
        ),
        Some(&cookie),
    )
    .await;
    assert_eq!(query_of(&location(&r))["error"], "login_required");
}

#[tokio::test]
async fn default_client_exists() {
    let o = setup().await;
    let (_, _, list) = call(
        &o.t.router,
        Method::GET,
        "/api/oauth-clients",
        Some(&o.t.token),
        None,
    )
    .await;
    let d = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "default")
        .cloned()
        .unwrap();
    assert_eq!(d["kind"], "public");
    assert_eq!(d["first_party"], true);
    assert_eq!(d["collection"], "users");
    assert_eq!(d["redirect_uris"], json!([]));
}

#[tokio::test]
async fn cors_for_protocol_endpoints_and_configured_api_origins() {
    let o = setup().await;
    let preflight = |uri: &str, origin: &str| {
        Request::builder()
            .method(Method::OPTIONS)
            .uri(uri)
            .header("origin", origin)
            .header("access-control-request-method", "POST")
            .header("access-control-request-headers", "authorization")
            .body(Body::empty())
            .unwrap()
    };
    let r = send(
        &o.t.router,
        preflight("/oauth2/token", "https://spa.example.com"),
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(r.headers["access-control-allow-origin"], "*");
    // /api: only configured origins.
    let r = send(
        &o.t.router,
        preflight("/api/collections/users/records", "https://spa.example.com"),
    )
    .await;
    assert!(r.headers.get("access-control-allow-origin").is_none());
    let (s, _, out) = call(
        &o.t.router,
        Method::PATCH,
        "/api/settings",
        Some(&o.t.token),
        Some(json!({"security": {"cors_origins": ["https://spa.example.com"]}})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{out}");
    let r = send(
        &o.t.router,
        preflight("/api/collections/users/records", "https://spa.example.com"),
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(
        r.headers["access-control-allow-origin"],
        "https://spa.example.com"
    );
    assert_eq!(r.headers["access-control-allow-credentials"], "true");
    let r = send(
        &o.t.router,
        preflight("/api/collections/users/records", "https://evil.example.com"),
    )
    .await;
    assert!(r.headers.get("access-control-allow-origin").is_none());
}
