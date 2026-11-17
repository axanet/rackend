//! M6 acceptance: broker sign-in against a real Keycloak (Docker), including
//! the auto-link attack with an unverified email. Run with
//! `mise run test:broker` (feature `keycloak-tests`).

use std::collections::HashMap;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use serde_json::{Value, json};
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner as _;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};

use crate::common::*;
use crate::email_auth::{PUBLIC_URL, PW, SPA, email_app, signup};

const IMAGE: (&str, &str) = ("quay.io/keycloak/keycloak", "26.5.7");

fn realm() -> Value {
    let user = |name: &str, email: &str, verified: bool| {
        json!({
            "username": name, "email": email, "emailVerified": verified, "enabled": true,
            "firstName": name, "lastName": "Test",
            "credentials": [{ "type": "password", "value": format!("{name}-password"), "temporary": false }],
        })
    };
    json!({
        "realm": "rackend",
        "enabled": true,
        "users": [
            user("alice", "alice@example.com", true),
            // Claims the victim's address without verifying it.
            user("mallory", "victim@example.com", false),
        ],
        "clients": [{
            "clientId": "rackend",
            "secret": "kc-client-secret",
            "publicClient": false,
            "standardFlowEnabled": true,
            "directAccessGrantsEnabled": false,
            "redirectUris": [format!("{PUBLIC_URL}/*")],
            "attributes": { "pkce.code.challenge.method": "S256" },
        }],
    })
}

async fn keycloak() -> (ContainerAsync<GenericImage>, String) {
    let c = GenericImage::new(IMAGE.0, IMAGE.1)
        .with_exposed_port(8080.tcp())
        .with_wait_for(WaitFor::message_on_stdout("started in"))
        .with_env_var("KC_BOOTSTRAP_ADMIN_USERNAME", "admin")
        .with_env_var("KC_BOOTSTRAP_ADMIN_PASSWORD", "admin")
        .with_copy_to(
            "/opt/keycloak/data/import/rackend.json",
            realm().to_string().into_bytes(),
        )
        .with_cmd(["start-dev", "--import-realm"])
        .with_startup_timeout(std::time::Duration::from_secs(180))
        .start()
        .await
        .unwrap();
    let port = c.get_host_port_ipv4(8080).await.unwrap();
    (c, format!("http://127.0.0.1:{port}/realms/rackend"))
}

fn cookies_of(res: &reqwest::Response, jar: &mut HashMap<String, String>) {
    for v in res.headers().get_all("set-cookie") {
        if let Some((k, rest)) = v.to_str().unwrap().split_once('=') {
            jar.insert(
                k.to_owned(),
                rest.split(';').next().unwrap_or_default().to_owned(),
            );
        }
    }
}

fn cookie_header(jar: &HashMap<String, String>) -> String {
    jar.iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Signs in at Keycloak as `user` and returns where rackend sends the browser.
async fn sign_in(t: &TestApp, user: &str) -> String {
    let start = send(
        &t.router,
        request(
            Method::GET,
            &format!("/api/collections/users/providers/keycloak/start?redirect_uri={SPA}"),
            None,
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
    let authorize = start.headers["location"].to_str().unwrap().to_owned();
    let state_cookie = start.headers["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();

    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let mut jar = HashMap::new();
    let page = http.get(&authorize).send().await.unwrap();
    cookies_of(&page, &mut jar);
    let html = page.text().await.unwrap();
    let action = html
        .split("id=\"kc-form-login\"")
        .nth(1)
        .and_then(|s| s.split("action=\"").nth(1))
        .and_then(|s| s.split('"').next())
        .unwrap_or_else(|| panic!("no login form: {html}"))
        .replace("&amp;", "&");
    let res = http
        .post(&action)
        .header("cookie", cookie_header(&jar))
        .form(&[
            ("username", user),
            ("password", &format!("{user}-password")),
            ("credentialId", ""),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        reqwest::StatusCode::FOUND,
        "{}",
        res.text().await.unwrap_or_default()
    );
    let back = res.headers()["location"].to_str().unwrap().to_owned();
    let path = back
        .strip_prefix(PUBLIC_URL)
        .unwrap_or_else(|| panic!("unexpected redirect {back}"));

    let cb = Request::builder()
        .uri(path)
        .header("cookie", state_cookie)
        .body(Body::empty())
        .unwrap();
    let r = send(&t.router, cb).await;
    assert_eq!(
        r.status,
        StatusCode::SEE_OTHER,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    r.headers["location"].to_str().unwrap().to_owned()
}

#[tokio::test]
async fn keycloak_sign_in_and_auto_link_attack() {
    let (_kc, issuer) = keycloak().await;
    let (t, _) = email_app().await;
    let victim = signup(&t, "victim@example.com").await;
    let (s, _, out) = call(
        &t.router,
        Method::POST,
        "/api/auth-providers",
        Some(&t.token),
        Some(json!({
            "name": "keycloak", "kind": "oidc", "preset": "keycloak", "issuer": issuer,
            "client_id": "rackend", "client_secret": "kc-client-secret", "collection": "users",
            "auto_link_verified_email": true,
        })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{out}");

    // A verified Keycloak user signs up and in.
    let landing = sign_in(&t, "alice").await;
    let code = landing
        .split("#code=")
        .nth(1)
        .unwrap_or_else(|| panic!("no code: {landing}"));
    let (s, _, out) = call(
        &t.router,
        Method::POST,
        "/api/collections/users/auth-with-exchange-code",
        None,
        Some(json!({ "code": code })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{out}");
    assert_eq!(out["record"]["email"], "alice@example.com");
    assert_eq!(out["record"]["email_verified"], true);

    // Mallory set the victim's address at Keycloak without verifying it.
    let landing = sign_in(&t, "mallory").await;
    assert!(landing.ends_with("#error=email_in_use"), "{landing}");
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
    assert_eq!(ids["items"].as_array().unwrap().len(), 0, "{ids}");
}
