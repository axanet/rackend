//! M6 acceptance: email flows (verification, reset, email change, OTP,
//! magic links) and no account enumeration on the email endpoints.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use serde_json::{Value, json};

use crate::common::*;

pub const PW: &str = "a long user password";
pub const PUBLIC_URL: &str = "https://rk.example.com";
pub const SPA: &str = "https://spa.example.com/auth/done";

/// An app with captured email, a public URL, SMTP and an allowed SPA URI.
pub async fn email_app() -> (TestApp, Outbox) {
    let outbox = Outbox::default();
    let t = start_with(Overrides {
        mailer_factory: Some(Arc::new(outbox.clone())),
        ..Default::default()
    })
    .await;
    let (s, _, out) = call(
        &t.router,
        Method::PATCH,
        "/api/settings",
        Some(&t.token),
        Some(json!({
            "app": { "public_url": PUBLIC_URL },
            "smtp": { "host": "smtp.example.com", "from_address": "noreply@example.com" },
            "security": { "app_redirect_uris": [SPA] },
        })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{out}");
    (t, outbox)
}

pub async fn signup(t: &TestApp, email: &str) -> String {
    let (s, _, out) = call(
        &t.router,
        Method::POST,
        "/api/collections/users/records",
        None,
        Some(json!({"email": email, "password": PW})),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "{out}");
    let (s, out) = login(t, email, PW).await;
    assert_eq!(s, StatusCode::OK, "{out}");
    out["record"]["id"].as_str().unwrap().to_owned()
}

pub async fn login(t: &TestApp, email: &str, pw: &str) -> (StatusCode, Value) {
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

async fn post(t: &TestApp, path: &str, token: Option<&str>, body: Value) -> (StatusCode, Value) {
    let (s, _, out) = call(
        &t.router,
        Method::POST,
        &format!("/api/collections/users/{path}"),
        token,
        Some(body),
    )
    .await;
    (s, out)
}

/// The value of `name` in a link's query or fragment.
fn param(link: &str, name: &str) -> String {
    link.split(['?', '&', '#'])
        .find_map(|kv| kv.strip_prefix(&format!("{name}=")))
        .unwrap_or_else(|| panic!("no {name} in {link}"))
        .to_owned()
}

fn link_in(text: &str) -> String {
    text.split_whitespace()
        .find(|w| w.starts_with("http"))
        .unwrap_or_else(|| panic!("no link in {text}"))
        .to_owned()
}

async fn user(t: &TestApp, id: &str) -> Value {
    let (s, _, out) = call(
        &t.router,
        Method::GET,
        &format!("/api/collections/users/records/{id}"),
        Some(&t.token),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{out}");
    out
}

fn form(uri: &str, fields: &[(&str, &str)]) -> Request<Body> {
    let body: Vec<String> = fields
        .iter()
        .map(|(k, v)| {
            format!(
                "{k}={}",
                v.replace('+', "%2B").replace('@', "%40").replace(' ', "+")
            )
        })
        .collect();
    Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from(body.join("&")))
        .unwrap()
}

#[tokio::test]
async fn email_methods_need_smtp() {
    let t = start().await;
    let (s, _, out) = call(
        &t.router,
        Method::POST,
        "/api/collections/users/request-otp",
        None,
        Some(json!({"email": "a@example.com"})),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{out}");
    assert_eq!(out["code"], "smtp_not_configured");
    let (_, _, m) = call(
        &t.router,
        Method::GET,
        "/api/collections/users/auth-methods",
        None,
        None,
    )
    .await;
    assert_eq!(m["otp"]["enabled"], false);
    assert_eq!(m["magic_link"]["enabled"], false);
}

#[tokio::test]
async fn verification_through_the_hosted_page() {
    let (t, outbox) = email_app().await;
    let (_, _, m) = call(
        &t.router,
        Method::GET,
        "/api/collections/users/auth-methods",
        None,
        None,
    )
    .await;
    assert_eq!(m["otp"]["enabled"], true);
    let id = signup(&t, "vera@example.com").await;
    assert_eq!(user(&t, &id).await["email_verified"], false);

    let (s, _) = post(
        &t,
        "request-verification",
        None,
        json!({"email": "vera@example.com"}),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let mail = outbox.wait("vera@example.com", 1).await;
    assert!(
        mail.subject.contains("Confirm your email"),
        "{}",
        mail.subject
    );
    let link = link_in(&mail.text);
    assert!(
        link.starts_with(&format!("{PUBLIC_URL}/auth/verify?c=users&token=")),
        "{link}"
    );
    assert!(
        mail.html.contains(&link.replace('&', "&amp;")),
        "HTML escapes the link"
    );

    // Opening the link changes nothing; the form does.
    let path = link.trim_start_matches(PUBLIC_URL);
    let page = send(&t.router, request(Method::GET, path, None, None)).await;
    assert_eq!(page.status, StatusCode::OK);
    let csp = page.headers["content-security-policy"].to_str().unwrap();
    assert!(
        csp.contains("default-src 'none'") && !csp.contains("unsafe-inline"),
        "{csp}"
    );
    assert_eq!(page.headers["referrer-policy"], "no-referrer");
    let html = String::from_utf8_lossy(&page.body);
    assert!(
        html.contains("<form method=\"post\" action=\"/auth/verify\">"),
        "{html}"
    );
    assert_eq!(user(&t, &id).await["email_verified"], false);

    let token = param(&link, "token");
    // A browser signed in to the admin UI sends its session cookie along, and
    // `Origin: null` under `Referrer-Policy: no-referrer`; the page ignores both.
    let mut req = form("/auth/verify", &[("c", "users"), ("token", &token)]);
    req.headers_mut()
        .insert("cookie", "rk_session=whatever".parse().unwrap());
    req.headers_mut().insert("origin", "null".parse().unwrap());
    let done = send(&t.router, req).await;
    assert_eq!(
        done.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&done.body)
    );
    assert_eq!(user(&t, &id).await["email_verified"], true);

    // Single use.
    let again = send(
        &t.router,
        form("/auth/verify", &[("c", "users"), ("token", &token)]),
    )
    .await;
    assert_eq!(again.status, StatusCode::BAD_REQUEST);
    let (s, out) = post(&t, "confirm-verification", None, json!({"token": token})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(out["errors"]["token"][0]["code"], "invalid_token");

    // Verified accounts get no more verification mail.
    post(
        &t,
        "request-verification",
        None,
        json!({"email": "vera@example.com"}),
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(outbox.to("vera@example.com").len(), 1);
}

#[tokio::test]
async fn password_reset_ends_sessions() {
    let (t, outbox) = email_app().await;
    let id = signup(&t, "rita@example.com").await;
    let (_, out) = login(&t, "rita@example.com", PW).await;
    let refresh = out["refresh_token"].as_str().unwrap().to_owned();

    let (s, _) = post(
        &t,
        "request-password-reset",
        None,
        json!({"email": "rita@example.com"}),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let link = link_in(&outbox.wait("rita@example.com", 1).await.text);
    assert!(link.contains("/auth/reset?c=users&token="), "{link}");
    let token = param(&link, "token");

    // A weak password does not use up the link.
    let (s, out) = post(
        &t,
        "confirm-password-reset",
        None,
        json!({"token": token, "password": "short"}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{out}");
    // The hosted form checks the repeat field.
    let page = send(
        &t.router,
        form(
            "/auth/reset",
            &[
                ("c", "users"),
                ("token", &token),
                ("password", "another long password"),
                ("password_confirm", "different"),
            ],
        ),
    )
    .await;
    assert_eq!(page.status, StatusCode::BAD_REQUEST);
    assert!(
        String::from_utf8_lossy(&page.body).contains("do not match"),
        "{}",
        String::from_utf8_lossy(&page.body)
    );

    let new_pw = "a brand new password";
    let page = send(
        &t.router,
        form(
            "/auth/reset",
            &[
                ("c", "users"),
                ("token", &token),
                ("password", new_pw),
                ("password_confirm", new_pw),
            ],
        ),
    )
    .await;
    assert_eq!(
        page.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&page.body)
    );

    // Old sessions are gone, the old password too; the email is verified.
    let (s, _, _) = call(
        &t.router,
        Method::POST,
        "/api/collections/users/auth-refresh",
        None,
        Some(json!({"refresh_token": refresh})),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(
        login(&t, "rita@example.com", PW).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        login(&t, "rita@example.com", new_pw).await.0,
        StatusCode::OK
    );
    assert_eq!(user(&t, &id).await["email_verified"], true);

    let (s, _) = post(
        &t,
        "confirm-password-reset",
        None,
        json!({"token": token, "password": "yet another password"}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "single use");
}

#[tokio::test]
async fn email_change_needs_the_password() {
    let (t, outbox) = email_app().await;
    let id = signup(&t, "old@example.com").await;
    let (_, out) = login(&t, "old@example.com", PW).await;
    let access = out["access_token"].as_str().unwrap().to_owned();

    let (s, _) = post(
        &t,
        "request-email-change",
        None,
        json!({"new_email": "new@example.com"}),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, out) = post(
        &t,
        "request-email-change",
        Some(&access),
        json!({"new_email": "New@Example.com"}),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "{out}");
    let mail = outbox.wait("new@example.com", 1).await;
    let link = link_in(&mail.text);
    let token = param(&link, "token");

    // The hosted page asks for the password.
    let page = send(
        &t.router,
        request(Method::GET, link.trim_start_matches(PUBLIC_URL), None, None),
    )
    .await;
    assert!(String::from_utf8_lossy(&page.body).contains("name=\"password\""));

    let (s, out) = post(&t, "confirm-email-change", None, json!({"token": token})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(out["errors"]["password"][0]["code"], "invalid_password");
    let (s, out) = post(
        &t,
        "confirm-email-change",
        None,
        json!({"token": token, "password": PW}),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "{out}");

    let u = user(&t, &id).await;
    assert_eq!(u["email"], "new@example.com");
    assert_eq!(u["email_verified"], true);
    assert_eq!(
        login(&t, "old@example.com", PW).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(login(&t, "new@example.com", PW).await.0, StatusCode::OK);
    // The old session ended.
    let (s, _, _) = call(
        &t.router,
        Method::GET,
        "/api/collections/users/sessions",
        Some(&access),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    // Taken addresses get the same answer and no mail.
    signup(&t, "taken@example.com").await;
    let (_, out) = login(&t, "new@example.com", PW).await;
    let access = out["access_token"].as_str().unwrap().to_owned();
    let (s, _) = post(
        &t,
        "request-email-change",
        Some(&access),
        json!({"new_email": "taken@example.com"}),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(outbox.to("taken@example.com").is_empty());
}

fn code_in(text: &str) -> String {
    text.split(|c: char| !c.is_ascii_digit())
        .find(|w| w.len() == 6)
        .unwrap_or_else(|| panic!("no code in {text}"))
        .to_owned()
}

#[tokio::test]
async fn otp_login_and_attempt_limit() {
    let (t, outbox) = email_app().await;
    let id = signup(&t, "otto@example.com").await;

    let (s, out) = post(
        &t,
        "request-otp",
        None,
        json!({"email": "otto@example.com"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{out}");
    let otp_id = out["otp_id"].as_str().unwrap().to_owned();
    let code = code_in(&outbox.wait("otto@example.com", 1).await.text);
    let wrong = if code == "000000" { "111111" } else { "000000" };

    let (s, _) = post(
        &t,
        "auth-with-otp",
        None,
        json!({"otp_id": otp_id, "code": wrong}),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, out) = post(
        &t,
        "auth-with-otp",
        None,
        json!({"otp_id": otp_id, "code": code}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{out}");
    assert_eq!(out["record"]["id"], id.as_str());
    assert_eq!(out["record"]["email_verified"], true);
    let (s, _) = post(
        &t,
        "auth-with-otp",
        None,
        json!({"otp_id": otp_id, "code": code}),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "single use");

    // A new code replaces the old one; five wrong codes void it.
    let (_, a) = post(
        &t,
        "request-otp",
        None,
        json!({"email": "otto@example.com"}),
    )
    .await;
    let first = code_in(&outbox.wait("otto@example.com", 2).await.text);
    let (_, b) = post(
        &t,
        "request-otp",
        None,
        json!({"email": "otto@example.com"}),
    )
    .await;
    let second = code_in(&outbox.wait("otto@example.com", 3).await.text);
    let (s, _) = post(
        &t,
        "auth-with-otp",
        None,
        json!({"otp_id": a["otp_id"], "code": first}),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "replaced");
    let wrong = if second == "000000" {
        "111111"
    } else {
        "000000"
    };
    for _ in 0..5 {
        let (s, _) = post(
            &t,
            "auth-with-otp",
            None,
            json!({"otp_id": b["otp_id"], "code": wrong}),
        )
        .await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
    }
    let (s, _) = post(
        &t,
        "auth-with-otp",
        None,
        json!({"otp_id": b["otp_id"], "code": second}),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "void after five attempts");
}

#[tokio::test]
async fn magic_link_login() {
    let (t, outbox) = email_app().await;
    let id = signup(&t, "maggie@example.com").await;
    let (s, out) = post(
        &t,
        "request-magic-link",
        None,
        json!({"email": "maggie@example.com", "redirect_uri": "https://evil.example.com/"}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(
        out["errors"]["redirect_uri"][0]["code"],
        "redirect_not_allowed"
    );

    let (s, _) = post(
        &t,
        "request-magic-link",
        None,
        json!({"email": "maggie@example.com", "redirect_uri": SPA}),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let link = link_in(&outbox.wait("maggie@example.com", 1).await.text);
    assert!(link.starts_with(&format!("{SPA}#token=")), "{link}");
    let token = param(&link, "token");
    let (s, out) = post(&t, "auth-with-magic-link", None, json!({"token": token})).await;
    assert_eq!(s, StatusCode::OK, "{out}");
    assert_eq!(out["record"]["id"], id.as_str());
    let (s, _) = post(&t, "auth-with-magic-link", None, json!({"token": token})).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "single use");
}

/// Builds a request body for an email address.
type BodyFor = Box<dyn Fn(&str) -> Value>;

/// Status, body and headers, minus what differs per request.
fn shape(r: &Reply) -> (StatusCode, Value, Vec<(String, String)>) {
    let mut body = r.json();
    if let Some(id) = body.get("otp_id").and_then(Value::as_str) {
        assert!(uuid::Uuid::try_parse(id).is_ok(), "{id}");
        body["otp_id"] = json!("<uuid>");
    }
    let mut headers: Vec<(String, String)> = r
        .headers
        .iter()
        .filter(|(k, _)| !["date", "x-request-id"].contains(&k.as_str()))
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or_default().to_owned()))
        .collect();
    headers.sort();
    (r.status, body, headers)
}

#[tokio::test]
async fn email_endpoints_do_not_reveal_accounts() {
    let (t, outbox) = email_app().await;
    let limits = |n: u32| json!({"security": {"rate_limits": {"email_address_per_hour": n}}});
    let (s, _, out) = call(
        &t.router,
        Method::PATCH,
        "/api/settings",
        Some(&t.token),
        Some(limits(20)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{out}");
    signup(&t, "known@example.com").await;
    let (_, out) = login(&t, "known@example.com", PW).await;
    let access = out["access_token"].as_str().unwrap().to_owned();

    let cases: Vec<(&str, Option<&str>, BodyFor)> = vec![
        (
            "request-verification",
            None,
            Box::new(|e| json!({"email": e})),
        ),
        (
            "request-password-reset",
            None,
            Box::new(|e| json!({"email": e})),
        ),
        ("request-otp", None, Box::new(|e| json!({"email": e}))),
        (
            "request-magic-link",
            None,
            Box::new(|e| json!({"email": e, "redirect_uri": SPA})),
        ),
        (
            "request-email-change",
            Some(access.as_str()),
            Box::new(|e| json!({"new_email": e})),
        ),
    ];
    for (path, token, body) in &cases {
        // For an email change, "known" is the taken address.
        let known = send(
            &t.router,
            request(
                Method::POST,
                &format!("/api/collections/users/{path}"),
                *token,
                Some(body("known@example.com")),
            ),
        )
        .await;
        let unknown = send(
            &t.router,
            request(
                Method::POST,
                &format!("/api/collections/users/{path}"),
                *token,
                Some(body("nobody@example.com")),
            ),
        )
        .await;
        assert!(known.status.is_success(), "{path}: {}", known.status);
        assert_eq!(shape(&known), shape(&unknown), "{path} answers differ");
    }
    // Mail went only to the account (and to the free address for the change).
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(outbox.to("known@example.com").len(), 4);
    assert_eq!(outbox.to("nobody@example.com").len(), 1);

    // An unknown OTP id fails like a wrong code.
    let (_, out) = post(
        &t,
        "request-otp",
        None,
        json!({"email": "ghost@example.com"}),
    )
    .await;
    let (s, _) = post(
        &t,
        "auth-with-otp",
        None,
        json!({"otp_id": out["otp_id"], "code": "123456"}),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    // The per-address limit applies to unknown addresses as well (3 per hour).
    call(
        &t.router,
        Method::PATCH,
        "/api/settings",
        Some(&t.token),
        Some(limits(3)),
    )
    .await;
    for i in 0..4 {
        let (s, _) = post(
            &t,
            "request-password-reset",
            None,
            json!({"email": "limit@example.com"}),
        )
        .await;
        let expected = if i < 3 {
            StatusCode::NO_CONTENT
        } else {
            StatusCode::TOO_MANY_REQUESTS
        };
        assert_eq!(s, expected, "request {i}");
    }
}

#[tokio::test]
async fn smtp_test_send_and_secret() {
    let (t, outbox) = email_app().await;
    let (s, _, out) = call(
        &t.router,
        Method::POST,
        "/api/settings/test-smtp",
        Some(&t.token),
        Some(json!({"to": "ops@example.com"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{out}");
    assert_eq!(out["ok"], true);
    assert_eq!(
        outbox.wait("ops@example.com", 1).await.subject,
        "Test email from rackend"
    );

    // The password is stored encrypted and masked.
    let (s, _, out) = call(
        &t.router,
        Method::PATCH,
        "/api/settings",
        Some(&t.token),
        Some(json!({"smtp": {"password": "hunter2-smtp"}})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{out}");
    assert_ne!(out["smtp"]["password"], "hunter2-smtp");
    assert_eq!(t.app.smtp_config().unwrap().password, "hunter2-smtp");

    let (s, _, _) = call(
        &t.router,
        Method::POST,
        "/api/settings/test-smtp",
        None,
        Some(json!({"to": "x@example.com"})),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn templates_are_editable_and_escaped() {
    let (t, outbox) = email_app().await;
    let (s, _, out) = call(
        &t.router,
        Method::PATCH,
        "/api/settings",
        Some(&t.token),
        Some(json!({
            "app": { "name": "Acme <Shop>" },
            "email_templates": { "otp": { "subject": "{{app_name}} code", "text": "Code {{code}}", "html": "<b>{{app_name}}</b> {{code}}" } },
        })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{out}");
    signup(&t, "tpl@example.com").await;
    post(&t, "request-otp", None, json!({"email": "tpl@example.com"})).await;
    let mail = outbox.wait("tpl@example.com", 1).await;
    assert_eq!(mail.subject, "Acme <Shop> code");
    assert!(
        mail.html.starts_with("<b>Acme &lt;Shop&gt;</b> "),
        "{}",
        mail.html
    );
    assert!(mail.text.starts_with("Code "));
}
