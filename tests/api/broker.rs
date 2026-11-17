//! M6 acceptance: external providers. The linking rules (SPEC §11.7.1)
//! against an in-process OpenID provider, including the auto-link attack
//! with an unverified email.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use serde_json::{Value, json};

use crate::common::*;
use crate::email_auth::{PUBLIC_URL, PW, SPA, email_app, signup};
use crate::mock_idp::MockIdp;

async fn add_provider(t: &TestApp, idp: &MockIdp, extra: Value) -> Value {
    let mut body = json!({
        "name": "mock", "kind": "oidc", "issuer": idp.base, "client_id": "rackend-test",
        "client_secret": "s3cret-client", "collection": "users",
    });
    for (k, v) in extra.as_object().cloned().unwrap_or_default() {
        body[k] = v;
    }
    let (s, _, out) = call(
        &t.router,
        Method::POST,
        "/api/auth-providers",
        Some(&t.token),
        Some(body),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{out}");
    out
}

async fn setup(extra: Value) -> (TestApp, MockIdp) {
    let (t, _) = email_app().await;
    let idp = MockIdp::start().await;
    add_provider(&t, &idp, extra).await;
    (t, idp)
}

/// The result of a provider sign-in: where the browser lands.
struct Landing {
    location: String,
}

impl Landing {
    fn fragment(&self, key: &str) -> Option<String> {
        let (_, frag) = self.location.split_once('#')?;
        frag.split('&')
            .find_map(|kv| kv.strip_prefix(&format!("{key}=")))
            .map(str::to_owned)
    }
}

/// Runs the browser part: start, the provider, the callback.
async fn sign_in(t: &TestApp, idp: &MockIdp, user: Value, bearer: Option<&str>) -> Landing {
    idp.next_user(user);
    let link = if bearer.is_some() { "&link=true" } else { "" };
    let start = send(
        &t.router,
        request(
            Method::GET,
            &format!("/api/collections/users/providers/mock/start?redirect_uri={SPA}{link}"),
            bearer,
            None,
        ),
    )
    .await;
    assert_eq!(
        start.status,
        StatusCode::FOUND,
        "{}",
        String::from_utf8_lossy(&start.body)
    );
    let to_idp = start.headers["location"].to_str().unwrap().to_owned();
    assert!(to_idp.starts_with(&idp.base), "{to_idp}");
    let cookie = start.headers["set-cookie"].to_str().unwrap().to_owned();
    assert!(
        cookie.contains("HttpOnly") && cookie.contains("SameSite=Lax"),
        "{cookie}"
    );
    let cookie = cookie.split(';').next().unwrap().to_owned();

    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let res = http.get(&to_idp).send().await.unwrap();
    let back = res.headers()["location"].to_str().unwrap().to_owned();
    let path = back
        .strip_prefix(PUBLIC_URL)
        .expect("callback on the public URL");
    assert!(
        path.starts_with("/api/collections/users/providers/mock/callback?"),
        "{path}"
    );

    let cb = Request::builder()
        .uri(path)
        .header("cookie", cookie)
        .body(Body::empty())
        .unwrap();
    let r = send(&t.router, cb).await;
    assert_eq!(
        r.status,
        StatusCode::SEE_OTHER,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    Landing {
        location: r.headers["location"].to_str().unwrap().to_owned(),
    }
}

async fn exchange(t: &TestApp, code: &str) -> (StatusCode, Value) {
    let (s, _, out) = call(
        &t.router,
        Method::POST,
        "/api/collections/users/auth-with-exchange-code",
        None,
        Some(json!({ "code": code })),
    )
    .await;
    (s, out)
}

async fn signed_in(t: &TestApp, landing: &Landing) -> Value {
    let code = landing
        .fragment("code")
        .unwrap_or_else(|| panic!("no code: {}", landing.location));
    assert!(landing.location.starts_with(SPA));
    let (s, out) = exchange(t, &code).await;
    assert_eq!(s, StatusCode::OK, "{out}");
    out
}

async fn audit_actions(t: &TestApp) -> Vec<String> {
    let (_, _, out) = call(
        &t.router,
        Method::GET,
        "/api/audit?per_page=200",
        Some(&t.token),
        None,
    )
    .await;
    out["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["action"].as_str().map(str::to_owned))
        .collect()
}

#[tokio::test]
async fn admin_api_masks_secret_and_lists_provider() {
    let (t, idp) = setup(json!({})).await;
    let (s, _, out) = call(
        &t.router,
        Method::GET,
        "/api/auth-providers",
        Some(&t.token),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let p = &out["items"][0];
    assert_eq!(p["client_secret"], "••••");
    assert_eq!(
        p["redirect_uri"],
        format!("{PUBLIC_URL}/api/collections/users/providers/mock/callback")
    );
    assert!(
        out["presets"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["preset"] == "github")
    );
    assert!(!out.to_string().contains("s3cret-client"));

    let (s, _, d) = call(
        &t.router,
        Method::POST,
        "/api/auth-providers/discover",
        Some(&t.token),
        Some(json!({"issuer": idp.base, "name": "mock"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{d}");
    assert_eq!(d["metadata"]["issuer"], idp.base.as_str());

    let (_, _, m) = call(
        &t.router,
        Method::GET,
        "/api/collections/users/auth-methods",
        None,
        None,
    )
    .await;
    assert_eq!(m["providers"][0]["name"], "mock");

    let (s, _, out) = call(
        &t.router,
        Method::POST,
        "/api/auth-providers",
        Some(&t.token),
        Some(json!({"name": "Bad Name", "kind": "oidc", "issuer": "http://idp.example.com", "client_id": "x", "collection": "users"})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(
        out["errors"]["name"].is_array() && out["errors"]["issuer"].is_array(),
        "{out}"
    );
    let (s, _, _) = call(&t.router, Method::GET, "/api/auth-providers", None, None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn sign_up_and_sign_in_with_a_provider() {
    let (t, idp) = setup(json!({})).await;
    let user =
        json!({"sub": "u-1", "email": "Pat@Example.com", "email_verified": true, "name": "Pat"});
    let landing = sign_in(&t, &idp, user.clone(), None).await;
    // PKCE S256, state and nonce went to the provider; scopes too.
    let q = idp.last_authorize();
    assert_eq!(q["code_challenge_method"], "S256");
    assert!(
        q["scope"].contains("openid") && q["scope"].contains("email"),
        "{q:?}"
    );
    assert!(!q["nonce"].is_empty() && !q["state"].is_empty());

    let code = landing.fragment("code").unwrap();
    let out = signed_in(&t, &landing).await;
    let id = out["record"]["id"].as_str().unwrap().to_owned();
    assert_eq!(out["record"]["email"], "pat@example.com");
    assert_eq!(out["record"]["email_verified"], true);
    assert_eq!(out["record"]["name"], "Pat");
    // Exchange codes are single use.
    assert_eq!(exchange(&t, &code).await.0, StatusCode::UNAUTHORIZED);

    // Rule 1: the same identity signs in to the same account.
    let again = signed_in(&t, &sign_in(&t, &idp, user, None).await).await;
    assert_eq!(again["record"]["id"], id.as_str());

    let access = again["access_token"].as_str().unwrap();
    let (s, _, ids) = call(
        &t.router,
        Method::GET,
        "/api/collections/users/identities",
        Some(access),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{ids}");
    assert_eq!(ids["items"].as_array().unwrap().len(), 1);
    assert_eq!(ids["items"][0]["provider"], "mock");
    assert_eq!(ids["items"][0]["issuer"], idp.base.as_str());

    // The only way in cannot be removed (OTP and magic links would be one).
    let (s, _, out) = call(
        &t.router,
        Method::PATCH,
        "/api/collections/users",
        Some(&t.token),
        Some(json!({"options": {"methods": {"password": true, "passkey": true, "otp": false, "magic_link": false}}})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{out}");
    let iid = ids["items"][0]["id"].as_str().unwrap();
    let (s, _, out) = call(
        &t.router,
        Method::DELETE,
        &format!("/api/collections/users/identities/{iid}"),
        Some(access),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{out}");
}

#[tokio::test]
async fn unverified_email_never_links_to_an_existing_account() {
    // The provider is trusted for verified emails, but this one is not verified.
    let (t, idp) = setup(json!({"auto_link_verified_email": true})).await;
    let victim = signup(&t, "victim@example.com").await;
    let attacker = json!({"sub": "evil-1", "email": "victim@example.com", "email_verified": false});
    let landing = sign_in(&t, &idp, attacker, None).await;
    assert_eq!(
        landing.fragment("error").as_deref(),
        Some("email_in_use"),
        "{}",
        landing.location
    );
    assert!(landing.fragment("code").is_none());

    // Nothing was linked to the victim, who still signs in as before.
    let (_, _, out) = call(
        &t.router,
        Method::POST,
        "/api/collections/users/auth-with-password",
        None,
        Some(json!({"email": "victim@example.com", "password": PW})),
    )
    .await;
    assert_eq!(out["record"]["id"], victim.as_str());
    let access = out["access_token"].as_str().unwrap();
    let (_, _, ids) = call(
        &t.router,
        Method::GET,
        "/api/collections/users/identities",
        Some(access),
        None,
    )
    .await;
    assert_eq!(ids["items"].as_array().unwrap().len(), 0);
    assert!(
        !audit_actions(&t)
            .await
            .iter()
            .any(|a| a.starts_with("identity."))
    );

    // Missing `email_verified` counts as unverified.
    let landing = sign_in(
        &t,
        &idp,
        json!({"sub": "evil-2", "email": "victim@example.com"}),
        None,
    )
    .await;
    assert_eq!(landing.fragment("error").as_deref(), Some("email_in_use"));

    // A verified email does not link to an account whose own email was never
    // verified: whoever registered it may not own the inbox.
    let landing = sign_in(
        &t,
        &idp,
        json!({"sub": "good-1", "email": "victim@example.com", "email_verified": true}),
        None,
    )
    .await;
    assert_eq!(landing.fragment("error").as_deref(), Some("email_in_use"));

    // Once the account's email is verified, a verified email links (rule 3), audited.
    let (s, _, out) = call(
        &t.router,
        Method::PATCH,
        &format!("/api/collections/users/records/{victim}"),
        Some(&t.token),
        Some(json!({"email_verified": true})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{out}");
    let landing = sign_in(
        &t,
        &idp,
        json!({"sub": "good-1", "email": "victim@example.com", "email_verified": true}),
        None,
    )
    .await;
    let out = signed_in(&t, &landing).await;
    assert_eq!(out["record"]["id"], victim.as_str());
    assert!(
        audit_actions(&t)
            .await
            .contains(&"identity.auto_linked".to_owned())
    );
}

#[tokio::test]
async fn verified_email_does_not_link_unless_the_provider_is_trusted() {
    let (t, idp) = setup(json!({})).await;
    signup(&t, "owner@example.com").await;
    let landing = sign_in(
        &t,
        &idp,
        json!({"sub": "x-1", "email": "owner@example.com", "email_verified": true}),
        None,
    )
    .await;
    assert_eq!(landing.fragment("error").as_deref(), Some("email_in_use"));
}

#[tokio::test]
async fn signed_in_users_link_a_provider() {
    let (t, idp) = setup(json!({})).await;
    let id = signup(&t, "linker@example.com").await;
    let (_, _, out) = call(
        &t.router,
        Method::POST,
        "/api/collections/users/auth-with-password",
        None,
        Some(json!({"email": "linker@example.com", "password": PW})),
    )
    .await;
    let access = out["access_token"].as_str().unwrap().to_owned();

    // Rule 2: any email at the provider; the session decides.
    let landing = sign_in(
        &t,
        &idp,
        json!({"sub": "l-1", "email": "other@example.com", "email_verified": false}),
        Some(&access),
    )
    .await;
    let out = signed_in(&t, &landing).await;
    assert_eq!(out["record"]["id"], id.as_str());
    assert_eq!(out["record"]["email"], "linker@example.com");
    assert!(
        audit_actions(&t)
            .await
            .contains(&"identity.link".to_owned())
    );

    // A password remains, so the identity can be removed.
    let (_, _, ids) = call(
        &t.router,
        Method::GET,
        "/api/collections/users/identities",
        Some(&access),
        None,
    )
    .await;
    let iid = ids["items"][0]["id"].as_str().unwrap().to_owned();
    let (s, _, _) = call(
        &t.router,
        Method::DELETE,
        &format!("/api/collections/users/identities/{iid}"),
        Some(&access),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    // An identity of another account cannot be linked.
    signed_in(
        &t,
        &sign_in(
            &t,
            &idp,
            json!({"sub": "taken-1", "email": "t@example.com", "email_verified": true}),
            None,
        )
        .await,
    )
    .await;
    let landing = sign_in(&t, &idp, json!({"sub": "taken-1"}), Some(&access)).await;
    assert_eq!(
        landing.fragment("error").as_deref(),
        Some("identity_in_use")
    );

    // Linking needs a session.
    let r = send(
        &t.router,
        request(
            Method::GET,
            &format!("/api/collections/users/providers/mock/start?redirect_uri={SPA}&link=true"),
            None,
            None,
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn sign_up_disabled_and_denied_consent() {
    let (t, idp) = setup(json!({})).await;
    let (s, _, out) = call(
        &t.router,
        Method::PATCH,
        "/api/collections/users",
        Some(&t.token),
        Some(json!({"options": {"signup_enabled": false}})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{out}");
    let landing = sign_in(
        &t,
        &idp,
        json!({"sub": "n-1", "email": "new@example.com", "email_verified": true}),
        None,
    )
    .await;
    assert_eq!(
        landing.fragment("error").as_deref(),
        Some("account_not_found")
    );

    let landing = sign_in(&t, &idp, json!({"deny": true}), None).await;
    assert_eq!(landing.fragment("error").as_deref(), Some("access_denied"));
}

#[tokio::test]
async fn state_nonce_and_redirect_checks() {
    let (t, idp) = setup(json!({})).await;
    // Redirect URIs must be allow-listed.
    let r = send(
        &t.router,
        request(
            Method::GET,
            "/api/collections/users/providers/mock/start?redirect_uri=https://evil.example.com/",
            None,
            None,
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        r.json()["errors"]["redirect_uri"][0]["code"],
        "redirect_not_allowed"
    );

    // A callback without the state cookie shows an error page.
    let r = send(
        &t.router,
        request(
            Method::GET,
            "/api/collections/users/providers/mock/callback?code=x&state=y",
            None,
            None,
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(String::from_utf8_lossy(&r.body).contains("invalid_state"));

    // A state that does not match the cookie.
    idp.next_user(json!({"sub": "s-1"}));
    let start = send(
        &t.router,
        request(
            Method::GET,
            &format!("/api/collections/users/providers/mock/start?redirect_uri={SPA}"),
            None,
            None,
        ),
    )
    .await;
    let cookie = start.headers["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let cb = Request::builder()
        .uri("/api/collections/users/providers/mock/callback?code=x&state=forged")
        .header("cookie", cookie)
        .body(Body::empty())
        .unwrap();
    let r = send(&t.router, cb).await;
    assert!(
        r.headers["location"]
            .to_str()
            .unwrap()
            .ends_with("#error=invalid_state")
    );

    // An ID token with another nonce is refused.
    idp.bad_nonce(true);
    let landing = sign_in(
        &t,
        &idp,
        json!({"sub": "s-2", "email": "n@example.com", "email_verified": true}),
        None,
    )
    .await;
    assert_eq!(landing.fragment("error").as_deref(), Some("provider_error"));
    idp.bad_nonce(false);
}

#[tokio::test]
async fn rotated_keys_are_fetched_again() {
    let (t, idp) = setup(json!({})).await;
    let user = json!({"sub": "r-1", "email": "rot@example.com", "email_verified": true});
    signed_in(&t, &sign_in(&t, &idp, user.clone(), None).await).await;
    let fetches = idp.jwks_fetches();
    idp.rotate_key(9);
    signed_in(&t, &sign_in(&t, &idp, user, None).await).await;
    assert_eq!(idp.jwks_fetches(), fetches + 1);
}
