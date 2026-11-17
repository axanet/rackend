//! M2 acceptance: auth collections, user sessions, rate limits, passkeys.

use std::net::SocketAddr;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode};
use serde_json::{Value, json};

use crate::common::*;
use crate::soft_authenticator::SoftAuthenticator;

const ORIGIN: &str = "https://rk.example.com";
const PW: &str = "a long user password";

async fn signup(t: &TestApp, email: &str) -> String {
    let (s, _, out) = call(
        &t.router,
        Method::POST,
        "/api/collections/users/records",
        None,
        Some(json!({"email": email, "password": PW, "name": "Ada"})),
    )
    .await;
    // Anonymous sign-up cannot view the new record (view rule), so no body.
    assert_eq!(s, StatusCode::NO_CONTENT, "{out}");
    let (s, out) = login(t, email, PW).await;
    assert_eq!(s, StatusCode::OK, "{out}");
    out["record"]["id"].as_str().unwrap().to_owned()
}

async fn login(t: &TestApp, email: &str, pw: &str) -> (StatusCode, Value) {
    let (s, _, out) = call(
        &t.router,
        Method::POST,
        "/api/collections/users/auth-with-password",
        None,
        Some(json!({"email": email, "password": pw})),
    )
    .await;
    (s, out)
}

async fn login_ok(t: &TestApp, email: &str) -> (String, String) {
    let (s, out) = login(t, email, PW).await;
    assert_eq!(s, StatusCode::OK, "{out}");
    (
        out["access_token"].as_str().unwrap().to_owned(),
        out["refresh_token"].as_str().unwrap().to_owned(),
    )
}

async fn set_settings(t: &TestApp, patch: Value) {
    let (s, _, out) = call(
        &t.router,
        Method::PATCH,
        "/api/settings",
        Some(&t.token),
        Some(patch),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{out}");
}

#[tokio::test]
async fn first_boot_creates_the_users_auth_collection() {
    let t = start().await;
    let (s, _, c) = call(
        &t.router,
        Method::GET,
        "/api/collections/users",
        Some(&t.token),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{c}");
    assert_eq!(c["type"], "auth");
    let names: Vec<&str> = c["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap())
        .collect();
    assert_eq!(&names[..2], ["email", "email_verified"]);
    assert_eq!(c["fields"][0]["system"], true);

    let (_, _, info) = call(&t.router, Method::GET, "/api/info", None, None).await;
    assert_eq!(info["auth_collections"][0]["name"], "users");
    let (s, _, m) = call(
        &t.router,
        Method::GET,
        "/api/collections/users/auth-methods",
        None,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(m["password"]["enabled"], true);
    assert_eq!(m["passkey"]["enabled"], false, "passkeys need a public URL");
}

#[tokio::test]
async fn signup_login_and_owner_rules() {
    let t = start().await;
    let ada = signup(&t, "Ada@Example.com").await;
    let bob = signup(&t, "bob@example.com").await;

    let (s, out) = login(&t, "ada@example.com", "wrong password!").await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{out}");
    let (s, out) = login(&t, "nobody@example.com", PW).await;
    assert_eq!(
        s,
        StatusCode::UNAUTHORIZED,
        "same answer for unknown emails: {out}"
    );

    let (access, _) = login_ok(&t, "ADA@example.com").await;
    let (s, _, me) = call(
        &t.router,
        Method::GET,
        &format!("/api/collections/users/records/{ada}"),
        Some(&access),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{me}");
    assert_eq!(me["email"], "ada@example.com", "the owner sees the email");
    assert!(me.get("password_hash").is_none());
    assert!(me.get("password").is_none());

    let (s, _, _) = call(
        &t.router,
        Method::GET,
        &format!("/api/collections/users/records/{bob}"),
        Some(&access),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "view rule hides other users");

    let (s, _, out) = call(
        &t.router,
        Method::GET,
        "/api/collections/users/records?filter=email%20%3D%20%22bob%40example.com%22",
        Some(&access),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::BAD_REQUEST,
        "no probing hidden emails: {out}"
    );

    // A user cannot verify their own email or change it directly.
    let (s, _, out) = call(
        &t.router,
        Method::PATCH,
        &format!("/api/collections/users/records/{ada}"),
        Some(&access),
        Some(json!({"email_verified": true})),
    )
    .await;
    assert!(s.is_client_error(), "{s} {out}");

    // Superusers see every email.
    let (_, _, list) = call(
        &t.router,
        Method::GET,
        "/api/collections/users/records?sort=email",
        Some(&t.token),
        None,
    )
    .await;
    assert_eq!(list["items"][1]["email"], "bob@example.com");
}

#[tokio::test]
async fn batch_cannot_write_auth_collections() {
    let t = start().await;
    let (s, _, out) = call(
        &t.router,
        Method::POST,
        "/api/batch",
        None,
        Some(json!({"requests": [{
            "method": "POST",
            "url": "/api/collections/users/records",
            "body": {"email": "mallory@example.com", "email_verified": true},
        }]})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{out}");
    let (_, _, list) = call(
        &t.router,
        Method::GET,
        "/api/collections/users/records",
        Some(&t.token),
        None,
    )
    .await;
    assert_eq!(list["total_items"], 0, "nothing was created: {list}");
}

#[tokio::test]
async fn weak_and_common_passwords_are_refused() {
    let t = start().await;
    for pw in ["short", "QWERTYUIOP"] {
        let (s, _, out) = call(
            &t.router,
            Method::POST,
            "/api/collections/users/records",
            None,
            Some(json!({"email": "x@example.com", "password": pw})),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{pw}: {out}");
        assert!(out["errors"]["password"].is_array(), "{out}");
    }
}

#[tokio::test]
async fn user_refresh_reuse_revokes_and_is_audited() {
    let t = start().await;
    signup(&t, "ada@example.com").await;
    let (access, rt1) = login_ok(&t, "ada@example.com").await;
    let refresh = |rt: &str| {
        call(
            &t.router,
            Method::POST,
            "/api/collections/users/auth-refresh",
            None,
            Some(json!({"refresh_token": rt})),
        )
    };
    let (s, _, second) = refresh(&rt1).await;
    assert_eq!(s, StatusCode::OK, "{second}");
    let rt2 = second["refresh_token"].as_str().unwrap().to_owned();
    assert_ne!(rt1, rt2);
    let (s, _, _) = call(
        &t.router,
        Method::POST,
        "/api/superusers/auth-refresh",
        None,
        Some(json!({"refresh_token": rt2})),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNAUTHORIZED,
        "user tokens do not refresh superuser sessions"
    );

    // Within the grace window the old token returns the same pair once.
    let (s, _, again) = refresh(&rt1).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(again["refresh_token"], second["refresh_token"]);
    // A second reuse is theft: revoke the session.
    let (s, _, _) = refresh(&rt1).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _, _) = refresh(&rt2).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "the whole session is gone");
    let new_access = second["access_token"].as_str().unwrap();
    for a in [access.as_str(), new_access] {
        let (s, _, _) = call(
            &t.router,
            Method::GET,
            "/api/collections/users/sessions",
            Some(a),
            None,
        )
        .await;
        assert_eq!(
            s,
            StatusCode::UNAUTHORIZED,
            "revoked access tokens fail at once"
        );
    }
    let (_, _, audit) = call(
        &t.router,
        Method::GET,
        "/api/audit?action=auth.refresh_reuse",
        Some(&t.token),
        None,
    )
    .await;
    assert_eq!(audit["total_items"], 1, "{audit}");
}

#[tokio::test]
async fn sessions_list_and_revoke() {
    let t = start().await;
    signup(&t, "ada@example.com").await;
    let (a1, _) = login_ok(&t, "ada@example.com").await;
    let (a2, _) = login_ok(&t, "ada@example.com").await;
    let (a3, _) = login_ok(&t, "ada@example.com").await;
    let (_, _, list) = call(
        &t.router,
        Method::GET,
        "/api/collections/users/sessions",
        Some(&a1),
        None,
    )
    .await;
    let items = list["items"].as_array().unwrap();
    assert_eq!(items.len(), 4, "three logins plus the one in signup()");
    assert_eq!(items.iter().filter(|i| i["current"] == true).count(), 1);
    let other = items.iter().find(|i| i["current"] == false).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let (s, _, _) = call(
        &t.router,
        Method::DELETE,
        &format!("/api/collections/users/sessions/{other}"),
        Some(&a1),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _, _) = call(
        &t.router,
        Method::DELETE,
        "/api/collections/users/sessions?all=true",
        Some(&a1),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let alive = [&a1, &a2, &a3].into_iter().map(|a| async {
        call(
            &t.router,
            Method::GET,
            "/api/collections/users/sessions",
            Some(a),
            None,
        )
        .await
        .0
    });
    let mut statuses = Vec::new();
    for f in alive {
        statuses.push(f.await);
    }
    assert_eq!(
        statuses,
        [
            StatusCode::OK,
            StatusCode::UNAUTHORIZED,
            StatusCode::UNAUTHORIZED
        ]
    );

    // Logout ends the current session too.
    let (s, _, _) = call(
        &t.router,
        Method::POST,
        "/api/collections/users/logout",
        Some(&a1),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _, _) = call(
        &t.router,
        Method::GET,
        "/api/collections/users/sessions",
        Some(&a1),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn password_change_needs_the_old_password_and_signs_out_elsewhere() {
    let t = start().await;
    let id = signup(&t, "ada@example.com").await;
    let (a1, _) = login_ok(&t, "ada@example.com").await;
    let (a2, _) = login_ok(&t, "ada@example.com").await;
    let url = format!("/api/collections/users/records/{id}");
    let (s, _, out) = call(
        &t.router,
        Method::PATCH,
        &url,
        Some(&a1),
        Some(json!({"password": "another long password"})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "old_password required: {out}");
    let (s, _, out) = call(
        &t.router,
        Method::PATCH,
        &url,
        Some(&a1),
        Some(json!({"password": "another long password", "old_password": PW})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{out}");
    let (s, _, _) = call(&t.router, Method::GET, &url, Some(&a1), None).await;
    assert_eq!(s, StatusCode::OK, "the caller stays signed in");
    let (s, _, _) = call(&t.router, Method::GET, &url, Some(&a2), None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "other sessions end");
    let (s, _) = login(&t, "ada@example.com", PW).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = login(&t, "ada@example.com", "another long password").await;
    assert_eq!(s, StatusCode::OK);

    // Deleting the user ends every session.
    let (s, _, _) = call(&t.router, Method::DELETE, &url, Some(&t.token), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _, _) = call(
        &t.router,
        Method::GET,
        "/api/collections/users/sessions",
        Some(&a1),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

fn from_ip(mut req: Request<Body>, ip: [u8; 4]) -> Request<Body> {
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from((ip, 40000))));
    req
}

#[tokio::test]
async fn login_rate_limits_and_lockout() {
    let t = start().await;
    signup(&t, "ada@example.com").await;
    set_settings(
        &t,
        json!({"security": {"rate_limits": {"login_account_per_15min": 3, "login_ip_per_min": 100}}}),
    )
    .await;
    for _ in 0..3 {
        let (s, _) = login(&t, "ada@example.com", "wrong password!").await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
    }
    let (s, out) = login(&t, "ada@example.com", PW).await;
    assert_eq!(
        s,
        StatusCode::TOO_MANY_REQUESTS,
        "locked even with the right password: {out}"
    );
    assert_eq!(out["code"], "rate_limited");
    assert_eq!(out["retry_after_s"], 900);

    // The per-IP global bucket, with a trusted-proxy-free direct peer.
    set_settings(
        &t,
        json!({"security": {"rate_limits": {"global_ip_per_min": 2}}}),
    )
    .await;
    let get = || request(Method::GET, "/api/info", None, None);
    assert_eq!(
        send(&t.router, from_ip(get(), [192, 0, 2, 1])).await.status,
        StatusCode::OK
    );
    assert_eq!(
        send(&t.router, from_ip(get(), [192, 0, 2, 1])).await.status,
        StatusCode::OK
    );
    let r = send(&t.router, from_ip(get(), [192, 0, 2, 1])).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(r.headers.get("retry-after").is_some());
    assert_eq!(
        send(&t.router, from_ip(get(), [192, 0, 2, 9])).await.status,
        StatusCode::OK
    );
    // X-Forwarded-For is ignored unless the peer is a trusted proxy.
    let mut spoof = from_ip(get(), [192, 0, 2, 1]);
    spoof
        .headers_mut()
        .insert("x-forwarded-for", "198.51.100.7".parse().unwrap());
    assert_eq!(
        send(&t.router, spoof).await.status,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn login_timing_does_not_reveal_accounts() {
    let t = start().await;
    signup(&t, "ada@example.com").await;
    set_settings(
        &t,
        json!({"security": {"rate_limits": {"login_account_per_15min": 0, "login_ip_per_min": 0}}}),
    )
    .await;
    // Warm up, then alternate to spread out noise; compare medians.
    login(&t, "ada@example.com", "wrong password!").await;
    let (mut known, mut unknown) = (Vec::new(), Vec::new());
    for i in 0..9 {
        for (email, out) in [
            ("ada@example.com", &mut known),
            ("nobody@example.com", &mut unknown),
        ] {
            let start = std::time::Instant::now();
            let (s, _) = login(&t, email, &format!("wrong password {i}")).await;
            out.push(start.elapsed().as_secs_f64());
            assert_eq!(s, StatusCode::UNAUTHORIZED);
        }
    }
    let median = |v: &mut Vec<f64>| {
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    };
    let (k, u) = (median(&mut known), median(&mut unknown));
    let diff = (k - u).abs() / k.max(u);
    assert!(
        diff < 0.10,
        "known {k:.4}s vs unknown {u:.4}s differ by {:.1}%",
        diff * 100.0
    );
}

#[tokio::test]
async fn superuser_passkey_only_mode() {
    let t = start().await;
    set_settings(&t, json!({"security": {"superuser_require_passkey": true}})).await;
    let (s, _, out) = call(
        &t.router,
        Method::POST,
        "/api/superusers/auth-with-password",
        None,
        Some(json!({"email": ADMIN_EMAIL, "password": ADMIN_PASSWORD})),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{out}");
}

#[tokio::test]
async fn passkey_register_login_and_delete() {
    let t = start().await;
    let id = signup(&t, "ada@example.com").await;
    let (access, _) = login_ok(&t, "ada@example.com").await;

    let (s, _, out) = call(
        &t.router,
        Method::POST,
        "/api/collections/users/passkeys/register/start",
        Some(&access),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "needs a public URL: {out}");
    assert_eq!(out["code"], "public_url_not_configured");
    set_settings(&t, json!({"app": {"public_url": ORIGIN}})).await;

    let mut auth = SoftAuthenticator::new(ORIGIN, 7);
    let (s, _, start) = call(
        &t.router,
        Method::POST,
        "/api/collections/users/passkeys/register/start",
        Some(&access),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{start}");
    let credential = auth.register(&start["options"]);
    let (s, _, pk) = call(
        &t.router,
        Method::POST,
        "/api/collections/users/passkeys/register/finish",
        Some(&access),
        Some(json!({"ceremony_id": start["ceremony_id"], "credential": credential, "name": "Laptop"})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{pk}");
    assert_eq!(pk["name"], "Laptop");

    // Sign in with the passkey, no email needed.
    let (s, _, start) = call(
        &t.router,
        Method::POST,
        "/api/collections/users/passkeys/login/start",
        None,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{start}");
    let assertion = auth.login(&start["options"]);
    let (s, _, out) = call(
        &t.router,
        Method::POST,
        "/api/collections/users/passkeys/login/finish",
        None,
        Some(json!({"ceremony_id": start["ceremony_id"], "credential": assertion.clone()})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{out}");
    assert_eq!(out["record"]["id"], id.as_str());
    let pk_access = out["access_token"].as_str().unwrap().to_owned();

    // A replayed assertion fails: the ceremony is single use.
    let (s, _, _) = call(
        &t.router,
        Method::POST,
        "/api/collections/users/passkeys/login/finish",
        None,
        Some(json!({"ceremony_id": start["ceremony_id"], "credential": assertion})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // A different (unregistered) authenticator cannot sign in.
    let mut stranger = SoftAuthenticator::new(ORIGIN, 9);
    stranger.user_handle = auth.user_handle.clone();
    let (_, _, start) = call(
        &t.router,
        Method::POST,
        "/api/collections/users/passkeys/login/start",
        None,
        None,
    )
    .await;
    let (s, _, _) = call(
        &t.router,
        Method::POST,
        "/api/collections/users/passkeys/login/finish",
        None,
        Some(json!({"ceremony_id": start["ceremony_id"], "credential": stranger.login(&start["options"])})),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    let (_, _, list) = call(
        &t.router,
        Method::GET,
        "/api/collections/users/passkeys",
        Some(&pk_access),
        None,
    )
    .await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1, "{list}");
    let pk_id = list["items"][0]["id"].as_str().unwrap();
    let (s, _, _) = call(
        &t.router,
        Method::DELETE,
        &format!("/api/collections/users/passkeys/{pk_id}"),
        Some(&pk_access),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::NO_CONTENT,
        "the password is still a login method"
    );
}

#[tokio::test]
async fn superuser_passkey_login() {
    let t = start().await;
    set_settings(&t, json!({"app": {"public_url": ORIGIN}})).await;
    let mut auth = SoftAuthenticator::new(ORIGIN, 3);
    let (_, _, start) = call(
        &t.router,
        Method::POST,
        "/api/superusers/passkeys/register/start",
        Some(&t.token),
        None,
    )
    .await;
    let (s, _, out) = call(
        &t.router,
        Method::POST,
        "/api/superusers/passkeys/register/finish",
        Some(&t.token),
        Some(json!({"ceremony_id": start["ceremony_id"], "credential": auth.register(&start["options"])})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{out}");
    set_settings(&t, json!({"security": {"superuser_require_passkey": true}})).await;

    // A superuser passkey does not open a user session and vice versa.
    let (_, _, start) = call(
        &t.router,
        Method::POST,
        "/api/collections/users/passkeys/login/start",
        None,
        None,
    )
    .await;
    let (s, _, _) = call(
        &t.router,
        Method::POST,
        "/api/collections/users/passkeys/login/finish",
        None,
        Some(json!({"ceremony_id": start["ceremony_id"], "credential": auth.login(&start["options"])})),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    let (_, _, start) = call(
        &t.router,
        Method::POST,
        "/api/superusers/passkeys/login/start",
        None,
        None,
    )
    .await;
    let (s, _, out) = call(
        &t.router,
        Method::POST,
        "/api/superusers/passkeys/login/finish",
        None,
        Some(json!({"ceremony_id": start["ceremony_id"], "credential": auth.login(&start["options"])})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{out}");
    assert_eq!(out["record"]["email"], ADMIN_EMAIL);
}
