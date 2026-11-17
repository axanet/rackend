//! The OIDC provider over HTTP (SPEC §11.8): discovery, JWKS, the
//! `/oauth2/*` protocol endpoints, the hosted login, consent and logout
//! pages (askama, strings from `i18n/en.json`), and the admin endpoints for
//! clients and signing keys.

use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;

use askama::Template;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::header::{
    AUTHORIZATION, CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, LOCATION, ORIGIN, PRAGMA,
    SET_COOKIE, WWW_AUTHENTICATE,
};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse as _, Response};
use base64::Engine as _;
use rackend_core::Error as CoreError;
use rackend_core::usecases::oidc::{
    AuthRequest, Authorize, ClientInfo, Logout, OAuthError, SSO_COOKIE, SsoSession, TokenRequest,
};
use rackend_core::usecases::passkeys::PasskeyScope;
use rackend_core::usecases::{App, RequestCtx};
use serde_json::{Value, json};

use crate::AppState;
use crate::api::broker::email_limit;
use crate::api::users::check_login_limit;
use crate::api::{ApiResult, created, no_content, ok};
use crate::ctx::{AnonCtx, Ctx, cookie};
use crate::html::MARK_SVG;
use crate::pages::RenderedPage;

// Strings -------------------------------------------------------------------

/// The string catalog of the hosted pages (English; ready for more).
#[derive(Debug)]
pub struct I18n(HashMap<String, String>);

impl I18n {
    /// The string for `key` (the key itself if missing).
    pub fn get<'a>(&'a self, key: &'a str) -> &'a str {
        self.0.get(key).map_or(key, String::as_str)
    }

    /// The string with `{name}` placeholders filled.
    pub fn fill(&self, key: &str, vars: &[(&str, &str)]) -> String {
        let mut s = self.get(key).to_owned();
        for (k, v) in vars {
            s = s.replace(&format!("{{{k}}}"), v);
        }
        s
    }
}

fn t() -> &'static I18n {
    static EN: OnceLock<I18n> = OnceLock::new();
    EN.get_or_init(|| {
        I18n(serde_json::from_str(include_str!("../i18n/en.json")).unwrap_or_default())
    })
}

// Page chrome -----------------------------------------------------------------

/// App name and logo on every hosted page (settings `app.name`, `app.logo`).
#[derive(Debug, Clone)]
pub struct Chrome {
    /// App name.
    pub app_name: String,
    /// A `data:image/…` logo.
    pub logo: Option<String>,
    /// The default mark.
    pub mark: &'static str,
}

fn chrome(state: &AppState) -> Chrome {
    let s = state.app().settings();
    Chrome {
        app_name: s.string("app", "name").unwrap_or_else(|| "rackend".into()),
        logo: s
            .string("app", "logo")
            .filter(|l| l.starts_with("data:image/") && l.len() < 200_000),
        mark: MARK_SVG,
    }
}

#[derive(Template)]
#[template(path = "login.html")]
struct LoginPage<'a> {
    t: &'a I18n,
    chrome: Chrome,
    title: String,
    subtitle: String,
    error: Option<String>,
    ar: String,
    email: String,
    collection: String,
    password: bool,
    passkey: bool,
    otp: bool,
    forgot: bool,
    signup: bool,
    providers: Vec<(String, String)>,
}

#[derive(Template)]
#[template(path = "code.html")]
struct CodePage<'a> {
    t: &'a I18n,
    chrome: Chrome,
    title: String,
    error: Option<String>,
    ar: String,
    email: String,
    otp_id: String,
    sent: String,
}

#[derive(Template)]
#[template(path = "signup.html")]
struct SignupPage<'a> {
    t: &'a I18n,
    chrome: Chrome,
    title: String,
    error: Option<String>,
    ar: String,
    email: String,
}

#[derive(Template)]
#[template(path = "forgot.html")]
struct ForgotPage<'a> {
    t: &'a I18n,
    chrome: Chrome,
    title: String,
    error: Option<String>,
    sent: Option<String>,
    ar: String,
}

#[derive(Template)]
#[template(path = "consent.html")]
struct ConsentPage<'a> {
    t: &'a I18n,
    chrome: Chrome,
    title: String,
    asks: String,
    scopes: Vec<String>,
    signed_in_as: String,
    ar: String,
}

#[derive(Template)]
#[template(path = "logout.html")]
struct LogoutPage<'a> {
    t: &'a I18n,
    chrome: Chrome,
    title: String,
    question: String,
    fields: Vec<(String, String)>,
}

#[derive(Template)]
#[template(path = "message.html")]
struct MessagePage<'a> {
    #[allow(dead_code)]
    t: &'a I18n,
    chrome: Chrome,
    title: String,
    message: String,
    is_error: bool,
    link: Option<(String, String)>,
}

fn csp(form_origin: Option<&str>) -> String {
    let extra = form_origin
        .filter(|o| o.starts_with("https://") || o.starts_with("http://"))
        .map(|o| format!(" {o}"))
        .unwrap_or_default();
    format!(
        "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; \
form-action 'self'{extra}; frame-ancestors 'none'; base-uri 'none'"
    )
}

fn html(
    status: StatusCode,
    body: Result<String, askama::Error>,
    form_origin: Option<&str>,
) -> Response {
    let body = body.unwrap_or_else(|e| {
        tracing::error!(error = %e, "cannot render a hosted page");
        "<!doctype html><title>Error</title><p>Something went wrong.</p>".into()
    });
    let mut res = (status, body).into_response();
    res.extensions_mut().insert(RenderedPage);
    let h = res.headers_mut();
    h.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    if let Ok(v) = HeaderValue::from_str(&csp(form_origin)) {
        h.insert(CONTENT_SECURITY_POLICY, v);
    }
    h.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

fn message(
    state: &AppState,
    status: StatusCode,
    title: &str,
    text: &str,
    link: Option<(String, String)>,
) -> Response {
    let page = MessagePage {
        t: t(),
        chrome: chrome(state),
        title: title.to_owned(),
        message: text.to_owned(),
        is_error: !status.is_success(),
        link,
    };
    html(status, page.render(), None)
}

fn expired(state: &AppState) -> Response {
    message(
        state,
        StatusCode::BAD_REQUEST,
        t().get("error.title"),
        t().get("login.expired"),
        None,
    )
}

fn redirect(status: StatusCode, to: &str) -> Response {
    let mut res = status.into_response();
    if let Ok(v) = HeaderValue::from_str(to) {
        res.headers_mut().insert(LOCATION, v);
    }
    res.headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res.extensions_mut().insert(RenderedPage);
    res
}

fn sso_cookie(state: &AppState, value: &str, max_age: i64) -> Option<HeaderValue> {
    let sec = if state.secure_cookies() {
        "; Secure"
    } else {
        ""
    };
    HeaderValue::from_str(&format!(
        "{SSO_COOKIE}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{sec}"
    ))
    .ok()
}

async fn sso(state: &AppState, headers: &HeaderMap) -> Option<SsoSession> {
    let token = cookie(headers, SSO_COOKIE).filter(|t| !t.is_empty())?;
    state.app().sso_session(&token).await
}

/// Hosted forms only accept same-origin posts (login CSRF).
fn same_origin(state: &AppState, headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(ORIGIN).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    let origin = origin.to_ascii_lowercase();
    if let Some(u) = state.app().settings().public_url() {
        let expected = u.to_ascii_lowercase();
        let expected = expected.split('/').take(3).collect::<Vec<_>>().join("/");
        if origin == expected {
            return true;
        }
    }
    state.dev()
        && (origin.starts_with("http://localhost") || origin.starts_with("http://127.0.0.1"))
}

fn forbidden_origin(state: &AppState) -> Response {
    message(
        state,
        StatusCode::FORBIDDEN,
        t().get("error.title"),
        "Cross-site form submission rejected.",
        None,
    )
}

/// Sends the browser on with the request after a sign-in.
fn signed_in(
    state: &AppState,
    tokens: &rackend_core::usecases::auth::Tokens,
    mut req: AuthRequest,
) -> Response {
    req.logged_in = true;
    let ar = state.app().seal(&req);
    let mut res = redirect(
        StatusCode::SEE_OTHER,
        &format!("/oauth2/authorize?ar={}", urlencode(&ar)),
    );
    if let Some(c) = sso_cookie(
        state,
        &App::sso_cookie_value(tokens),
        tokens.refresh_expires_in,
    ) {
        res.headers_mut().append(SET_COOKIE, c);
    }
    res
}

fn urlencode(s: &str) -> String {
    rackend_core::usecases::oidc::urlencode(s)
}

fn authorize_response(state: &AppState, outcome: Authorize, after_post: bool) -> Response {
    let status = if after_post {
        StatusCode::SEE_OTHER
    } else {
        StatusCode::FOUND
    };
    match outcome {
        Authorize::Redirect(url) => redirect(status, &url),
        Authorize::Login { ar, .. } => {
            redirect(status, &format!("/auth/login?ar={}", urlencode(&ar)))
        }
        Authorize::Consent { ar, .. } => {
            redirect(status, &format!("/auth/consent?ar={}", urlencode(&ar)))
        }
        Authorize::Error { code, message: m } => message(
            state,
            StatusCode::BAD_REQUEST,
            t().get("error.title"),
            &format!("{m} ({code})"),
            None,
        ),
    }
}

type Form = axum::Form<BTreeMap<String, String>>;
type Q = Query<BTreeMap<String, String>>;

fn field(f: &BTreeMap<String, String>, k: &str) -> String {
    f.get(k).cloned().unwrap_or_default()
}

// Protocol endpoints ----------------------------------------------------------

/// `GET /.well-known/openid-configuration`.
pub async fn discovery(State(state): State<AppState>) -> ApiResult {
    let mut res = ok(state.app().oidc_discovery()?);
    res.headers_mut().insert(
        CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=300"),
    );
    Ok(res)
}

/// `GET /oauth2/jwks`.
pub async fn jwks(State(state): State<AppState>) -> ApiResult {
    let set = state.app().oidc_jwks();
    if set["keys"].as_array().is_none_or(Vec::is_empty) {
        return Err(
            CoreError::unavailable(rackend_core::error::UnavailableReason::Starting).into(),
        );
    }
    let mut res = ok(set);
    res.headers_mut().insert(
        CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=300"),
    );
    Ok(res)
}

async fn authorize_any(
    state: &AppState,
    ctx: &RequestCtx,
    headers: &HeaderMap,
    params: &BTreeMap<String, String>,
    after_post: bool,
) -> Response {
    let app = state.app();
    let req = match params.get("ar") {
        Some(ar) => match app.unseal(ar, ctx.now) {
            Some(r) => r,
            None => return expired(state),
        },
        None => match app.authorize_request(params, ctx.now).await {
            Ok(r) => r,
            Err(outcome) => return authorize_response(state, outcome, after_post),
        },
    };
    let s = sso(state, headers).await;
    authorize_response(state, app.authorize(ctx, req, s).await, after_post)
}

/// `GET /oauth2/authorize`.
pub async fn authorize_get(
    State(state): State<AppState>,
    AnonCtx(ctx): AnonCtx,
    headers: HeaderMap,
    Query(q): Q,
) -> Response {
    authorize_any(&state, &ctx, &headers, &q, false).await
}

/// `POST /oauth2/authorize` (form-encoded parameters, OIDC Core §3.1.2.1).
pub async fn authorize_post(
    State(state): State<AppState>,
    AnonCtx(ctx): AnonCtx,
    headers: HeaderMap,
    axum::Form(f): Form,
) -> Response {
    authorize_any(&state, &ctx, &headers, &f, true).await
}

fn oauth_error(e: &OAuthError) -> Response {
    let mut body = json!({ "error": e.error, "error_description": e.description });
    // The RFC 9457 members too (docs/errors.md, protocol-mandated formats).
    body["status"] = json!(e.status);
    body["title"] = json!(e.error);
    body["detail"] = json!(e.description);
    body["code"] = json!(e.error);
    let mut res = (
        StatusCode::from_u16(e.status).unwrap_or(StatusCode::BAD_REQUEST),
        Json(body),
    )
        .into_response();
    res.extensions_mut().insert(RenderedPage);
    res.headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if e.status == 401 {
        let v = if e.error == "invalid_token" {
            format!(
                "Bearer error=\"invalid_token\", error_description=\"{}\"",
                e.description.replace('"', "'")
            )
        } else {
            "Basic realm=\"rackend\"".into()
        };
        if let Ok(v) = HeaderValue::from_str(&v) {
            res.headers_mut().insert(WWW_AUTHENTICATE, v);
        }
    }
    res
}

fn basic_auth(headers: &HeaderMap) -> Option<(String, String)> {
    let v = headers.get(AUTHORIZATION)?.to_str().ok()?;
    let (scheme, b64) = v.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("basic") {
        return None;
    }
    let raw = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .ok()?;
    let s = String::from_utf8(raw).ok()?;
    let (id, secret) = s.split_once(':')?;
    // RFC 6749 §2.3.1: both parts are form-urlencoded.
    Some((form_decode(id), form_decode(secret)))
}

fn form_decode(x: &str) -> String {
    let bytes = x.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() + 1
                && x.get(i + 1..i + 3)
                    .is_some_and(|h| u8::from_str_radix(h, 16).is_ok()) =>
            {
                out.push(u8::from_str_radix(&x[i + 1..i + 3], 16).unwrap_or(b'%'));
                i += 2;
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn no_store(mut res: Response) -> Response {
    res.headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res.headers_mut()
        .insert(PRAGMA, HeaderValue::from_static("no-cache"));
    res
}

/// `POST /oauth2/token`.
pub async fn token(
    State(state): State<AppState>,
    AnonCtx(ctx): AnonCtx,
    headers: HeaderMap,
    axum::Form(form): Form,
) -> Response {
    let r = TokenRequest {
        form,
        basic: basic_auth(&headers),
    };
    match state.app().oauth_token(&ctx, r).await {
        Ok(v) => no_store(Json(v).into_response()),
        Err(e) => oauth_error(&e),
    }
}

fn bearer(headers: &HeaderMap) -> Option<String> {
    let v = headers.get(AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = v.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| token.trim().to_owned())
}

/// `GET /oauth2/userinfo`.
pub async fn userinfo_get(State(state): State<AppState>, headers: HeaderMap) -> Response {
    userinfo(&state, bearer(&headers)).await
}

/// `POST /oauth2/userinfo` (token in the header or the form).
pub async fn userinfo_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::Form(f): Form,
) -> Response {
    userinfo(
        &state,
        bearer(&headers).or_else(|| f.get("access_token").cloned()),
    )
    .await
}

async fn userinfo(state: &AppState, token: Option<String>) -> Response {
    let Some(token) = token else {
        let mut res = StatusCode::UNAUTHORIZED.into_response();
        res.headers_mut()
            .insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        res.extensions_mut().insert(RenderedPage);
        return res;
    };
    match state.app().oauth_userinfo(&token).await {
        Ok(v) => no_store(Json(v).into_response()),
        Err(e) => oauth_error(&e),
    }
}

/// `POST /oauth2/revoke` (RFC 7009).
pub async fn revoke(
    State(state): State<AppState>,
    AnonCtx(ctx): AnonCtx,
    headers: HeaderMap,
    axum::Form(form): Form,
) -> Response {
    let r = TokenRequest {
        form,
        basic: basic_auth(&headers),
    };
    match state.app().oauth_revoke(&ctx, r).await {
        Ok(()) => no_store(StatusCode::OK.into_response()),
        Err(e) => oauth_error(&e),
    }
}

async fn logout_any(
    state: &AppState,
    ctx: &RequestCtx,
    headers: &HeaderMap,
    params: BTreeMap<String, String>,
    post: bool,
) -> Response {
    if post && !same_origin(state, headers) {
        return forbidden_origin(state);
    }
    let confirmed = post && params.get("confirm").map(String::as_str) == Some("yes");
    let s = sso(state, headers).await;
    let clear = sso_cookie(state, "", 0);
    let mut res = match state.app().oauth_logout(ctx, &params, s, confirmed).await {
        Logout::Redirect(url) => redirect(
            if post {
                StatusCode::SEE_OTHER
            } else {
                StatusCode::FOUND
            },
            &url,
        ),
        Logout::Confirm => {
            let c = chrome(state);
            let fields = params
                .iter()
                .filter(|(k, _)| {
                    [
                        "post_logout_redirect_uri",
                        "state",
                        "client_id",
                        "ui_locales",
                    ]
                    .contains(&k.as_str())
                })
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            let page = LogoutPage {
                t: t(),
                title: t().get("logout.title").to_owned(),
                question: t().fill("logout.confirm", &[("app", &c.app_name)]),
                chrome: c,
                fields,
            };
            return html(StatusCode::OK, page.render(), None);
        }
        Logout::Done => message(
            state,
            StatusCode::OK,
            t().get("logout.title"),
            t().get("logout.done"),
            None,
        ),
    };
    if let Some(c) = clear {
        res.headers_mut().append(SET_COOKIE, c);
    }
    res
}

/// `GET /oauth2/logout`.
pub async fn logout_get(
    State(state): State<AppState>,
    AnonCtx(ctx): AnonCtx,
    headers: HeaderMap,
    Query(q): Q,
) -> Response {
    logout_any(&state, &ctx, &headers, q, false).await
}

/// `POST /oauth2/logout`.
pub async fn logout_post(
    State(state): State<AppState>,
    AnonCtx(ctx): AnonCtx,
    headers: HeaderMap,
    axum::Form(f): Form,
) -> Response {
    logout_any(&state, &ctx, &headers, f, true).await
}

// Hosted pages ------------------------------------------------------------------

struct Hosted {
    req: AuthRequest,
    client: ClientInfo,
}

async fn hosted(state: &AppState, ctx: &RequestCtx, ar: &str) -> Option<Hosted> {
    let req = state.app().unseal(ar, ctx.now)?;
    let client = state.app().request_client(&req).await?;
    Some(Hosted { req, client })
}

async fn login_page(
    state: &AppState,
    h: &Hosted,
    ar: &str,
    email: &str,
    error: Option<String>,
    status: StatusCode,
) -> Response {
    let app = state.app();
    let methods = app
        .auth_methods(&h.client.collection)
        .unwrap_or(Value::Null);
    let on = |k: &str| methods[k]["enabled"] == true;
    let providers: Vec<(String, String)> = methods["providers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| {
            let name = p["name"].as_str()?;
            let label = t().fill("login.provider", &[("provider", &provider_label(p))]);
            Some((
                label,
                format!(
                    "/api/collections/{}/providers/{name}/start?continue={}",
                    h.client.collection,
                    urlencode(ar)
                ),
            ))
        })
        .collect();
    let page = LoginPage {
        t: t(),
        chrome: chrome(state),
        title: t().get("login.title").to_owned(),
        subtitle: t().fill("login.to", &[("client", &h.client.name)]),
        error,
        ar: ar.to_owned(),
        email: if email.is_empty() {
            h.req.login_hint.clone().unwrap_or_default()
        } else {
            email.to_owned()
        },
        collection: h.client.collection.clone(),
        password: on("password"),
        passkey: on("passkey"),
        otp: on("otp"),
        forgot: on("password") && app.smtp_configured(),
        signup: methods["signup_enabled"] == true && on("password"),
        providers,
    };
    html(status, page.render(), Some(&h.client.redirect_origin))
}

fn provider_label(p: &Value) -> String {
    let name = p["name"].as_str().unwrap_or_default();
    match p["preset"].as_str() {
        Some("github") => "GitHub".into(),
        Some("google") => "Google".into(),
        Some("microsoft") => "Microsoft".into(),
        Some("apple") => "Apple".into(),
        Some("gitlab") => "GitLab".into(),
        _ => {
            let mut c = name.chars();
            c.next()
                .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                .unwrap_or_default()
        }
    }
}

/// `GET /auth/login?ar=&error=`.
pub async fn login_get(
    State(state): State<AppState>,
    AnonCtx(ctx): AnonCtx,
    Query(q): Q,
) -> Response {
    let ar = field(&q, "ar");
    let Some(h) = hosted(&state, &ctx, &ar).await else {
        return expired(&state);
    };
    let error = q.get("error").map(|code| match code.as_str() {
        "email_in_use" => t().get("login.email_in_use").to_owned(),
        _ => t().get("login.provider_failed").to_owned(),
    });
    login_page(&state, &h, &ar, "", error, StatusCode::OK).await
}

fn login_error(e: &CoreError) -> String {
    match e {
        CoreError::Forbidden(m) if m.contains("Verify") => t().get("login.not_verified").to_owned(),
        CoreError::Forbidden(m) => m.clone(),
        _ => t().get("login.bad_credentials").to_owned(),
    }
}

/// `POST /auth/login {ar, email, password}`.
pub async fn login_post(
    State(state): State<AppState>,
    AnonCtx(ctx): AnonCtx,
    headers: HeaderMap,
    axum::Form(f): Form,
) -> Response {
    if !same_origin(&state, &headers) {
        return forbidden_origin(&state);
    }
    let ar = field(&f, "ar");
    let Some(h) = hosted(&state, &ctx, &ar).await else {
        return expired(&state);
    };
    let (email, pw) = (field(&f, "email"), field(&f, "password"));
    let app = state.app();
    let Ok(cid) = app.auth_collection_id(&h.client.collection) else {
        return expired(&state);
    };
    let account = format!("{cid}:{}", email.trim().to_lowercase());
    if check_login_limit(&state, &ctx, &account).is_err() {
        return login_page(
            &state,
            &h,
            &ar,
            &email,
            Some(t().get("login.rate_limited").into()),
            StatusCode::TOO_MANY_REQUESTS,
        )
        .await;
    }
    match app
        .user_login(&ctx, &h.client.collection, &email, &pw)
        .await
    {
        Ok(tokens) => {
            state.limits().login_succeeded(&account);
            signed_in(&state, &tokens, h.req)
        }
        Err(e) => {
            login_page(
                &state,
                &h,
                &ar,
                &email,
                Some(login_error(&e)),
                StatusCode::UNAUTHORIZED,
            )
            .await
        }
    }
}

/// `POST /auth/login/code {ar, email}`: emails a code, or asks for the email.
pub async fn code_request(
    State(state): State<AppState>,
    AnonCtx(ctx): AnonCtx,
    headers: HeaderMap,
    axum::Form(f): Form,
) -> Response {
    if !same_origin(&state, &headers) {
        return forbidden_origin(&state);
    }
    let ar = field(&f, "ar");
    let Some(h) = hosted(&state, &ctx, &ar).await else {
        return expired(&state);
    };
    let email = field(&f, "email").trim().to_owned();
    let mut page = CodePage {
        t: t(),
        chrome: chrome(&state),
        title: t().get("otp.title").to_owned(),
        error: None,
        ar: ar.clone(),
        email: email.clone(),
        otp_id: String::new(),
        sent: String::new(),
    };
    if !email.is_empty() {
        if email_limit(&state, &ctx, &email).is_err() {
            page.error = Some(t().get("login.rate_limited").into());
        } else {
            match state
                .app()
                .request_otp(&ctx, &h.client.collection, &email)
                .await
            {
                Ok(id) => {
                    page.otp_id = id;
                    page.sent = t().fill("otp.sent", &[("email", &email)]);
                }
                Err(_) => page.error = Some(t().get("login.no_methods").into()),
            }
        }
    }
    html(
        StatusCode::OK,
        page.render(),
        Some(&h.client.redirect_origin),
    )
}

/// `POST /auth/login/code/verify {ar, otp_id, code, email}`.
pub async fn code_verify(
    State(state): State<AppState>,
    AnonCtx(ctx): AnonCtx,
    headers: HeaderMap,
    axum::Form(f): Form,
) -> Response {
    if !same_origin(&state, &headers) {
        return forbidden_origin(&state);
    }
    let ar = field(&f, "ar");
    let Some(h) = hosted(&state, &ctx, &ar).await else {
        return expired(&state);
    };
    let (otp_id, code, email) = (
        field(&f, "otp_id"),
        field(&f, "code").replace(' ', ""),
        field(&f, "email"),
    );
    let app = state.app();
    let cid = app
        .auth_collection_id(&h.client.collection)
        .unwrap_or_default();
    let account = format!("{cid}:otp:{otp_id}");
    let result = match check_login_limit(&state, &ctx, &account) {
        Ok(()) => app
            .auth_with_otp(&ctx, &h.client.collection, &otp_id, &code)
            .await
            .ok(),
        Err(_) => None,
    };
    match result {
        Some(tokens) => signed_in(&state, &tokens, h.req),
        None => {
            let page = CodePage {
                t: t(),
                chrome: chrome(&state),
                title: t().get("otp.title").to_owned(),
                error: Some(t().get("otp.wrong").into()),
                sent: t().fill("otp.sent", &[("email", &email)]),
                ar,
                email,
                otp_id,
            };
            html(
                StatusCode::UNAUTHORIZED,
                page.render(),
                Some(&h.client.redirect_origin),
            )
        }
    }
}

/// `POST /auth/login/passkey {ar, ceremony_id, credential}` (from page.js):
/// answers `{redirect}`.
pub async fn passkey_finish(
    State(state): State<AppState>,
    AnonCtx(ctx): AnonCtx,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult {
    if !same_origin(&state, &headers) {
        return Err(CoreError::Forbidden("Cross-site request rejected.".into()).into());
    }
    let ar = body["ar"].as_str().unwrap_or_default().to_owned();
    let Some(h) = hosted(&state, &ctx, &ar).await else {
        return Err(CoreError::field(
            "ar",
            rackend_core::error::FieldError::new("expired", t().get("login.expired")),
        )
        .into());
    };
    let tokens = state
        .app()
        .passkey_login_finish(
            &ctx,
            &PasskeyScope::Collection(h.client.collection.clone()),
            body["ceremony_id"].as_str().unwrap_or_default(),
            body["credential"].clone(),
        )
        .await?;
    let res = signed_in(&state, &tokens, h.req);
    let location = res
        .headers()
        .get(LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("/")
        .to_owned();
    let mut out = ok(json!({ "redirect": location }));
    for c in res.headers().get_all(SET_COOKIE) {
        out.headers_mut().append(SET_COOKIE, c.clone());
    }
    Ok(out)
}

/// `GET /auth/signup?ar=`.
pub async fn signup_get(
    State(state): State<AppState>,
    AnonCtx(ctx): AnonCtx,
    Query(q): Q,
) -> Response {
    let ar = field(&q, "ar");
    let Some(h) = hosted(&state, &ctx, &ar).await else {
        return expired(&state);
    };
    let page = SignupPage {
        t: t(),
        chrome: chrome(&state),
        title: t().get("signup.title").into(),
        error: None,
        ar,
        email: String::new(),
    };
    html(
        StatusCode::OK,
        page.render(),
        Some(&h.client.redirect_origin),
    )
}

/// `POST /auth/signup {ar, email, password, password_confirm}`.
pub async fn signup_post(
    State(state): State<AppState>,
    AnonCtx(ctx): AnonCtx,
    headers: HeaderMap,
    axum::Form(f): Form,
) -> Response {
    if !same_origin(&state, &headers) {
        return forbidden_origin(&state);
    }
    let ar = field(&f, "ar");
    let Some(h) = hosted(&state, &ctx, &ar).await else {
        return expired(&state);
    };
    let (email, pw) = (field(&f, "email").trim().to_owned(), field(&f, "password"));
    let page = |error: String| SignupPage {
        t: t(),
        chrome: chrome(&state),
        title: t().get("signup.title").into(),
        error: Some(error),
        ar: ar.clone(),
        email: email.clone(),
    };
    if f.get("password_confirm") != Some(&pw) {
        return html(
            StatusCode::BAD_REQUEST,
            page(t().get("signup.mismatch").into()).render(),
            None,
        );
    }
    let app = state.app();
    let mut body = serde_json::Map::new();
    body.insert("email".into(), json!(email));
    body.insert("password".into(), json!(pw));
    if let Err(e) = app.record_create(&ctx, &h.client.collection, body).await {
        let msg = match &e {
            CoreError::Validation(v) => {
                v.0.values()
                    .flat_map(|x| x.iter().map(|e| e.message.clone()))
                    .collect::<Vec<_>>()
                    .join(" ")
            }
            CoreError::Conflict(_) => t().get("login.email_in_use").into(),
            CoreError::Forbidden(m) => m.clone(),
            _ => t().get("error.back").into(),
        };
        return html(StatusCode::BAD_REQUEST, page(msg).render(), None);
    }
    match app
        .user_login(&ctx, &h.client.collection, &email, &pw)
        .await
    {
        Ok(tokens) => signed_in(&state, &tokens, h.req),
        Err(_) => {
            let _ = app
                .request_verification(&ctx, &h.client.collection, &email)
                .await;
            message(
                &state,
                StatusCode::OK,
                t().get("signup.title"),
                t().get("signup.check_email"),
                Some((
                    t().get("forgot.back").into(),
                    format!("/auth/login?ar={}", urlencode(&ar)),
                )),
            )
        }
    }
}

/// `GET /auth/forgot?ar=`.
pub async fn forgot_get(
    State(state): State<AppState>,
    AnonCtx(ctx): AnonCtx,
    Query(q): Q,
) -> Response {
    let ar = field(&q, "ar");
    if hosted(&state, &ctx, &ar).await.is_none() {
        return expired(&state);
    }
    let page = ForgotPage {
        t: t(),
        chrome: chrome(&state),
        title: t().get("forgot.title").into(),
        error: None,
        sent: None,
        ar,
    };
    html(StatusCode::OK, page.render(), None)
}

/// `POST /auth/forgot {ar, email}`.
pub async fn forgot_post(
    State(state): State<AppState>,
    AnonCtx(ctx): AnonCtx,
    headers: HeaderMap,
    axum::Form(f): Form,
) -> Response {
    if !same_origin(&state, &headers) {
        return forbidden_origin(&state);
    }
    let ar = field(&f, "ar");
    let Some(h) = hosted(&state, &ctx, &ar).await else {
        return expired(&state);
    };
    let email = field(&f, "email").trim().to_owned();
    let mut page = ForgotPage {
        t: t(),
        chrome: chrome(&state),
        title: t().get("forgot.title").into(),
        error: None,
        sent: None,
        ar,
    };
    if email_limit(&state, &ctx, &email).is_err() {
        page.error = Some(t().get("login.rate_limited").into());
    } else {
        let _ = state
            .app()
            .request_password_reset(&ctx, &h.client.collection, &email)
            .await;
        page.sent = Some(t().fill("forgot.sent", &[("email", &email)]));
    }
    html(StatusCode::OK, page.render(), None)
}

/// `GET /auth/consent?ar=`.
pub async fn consent_get(
    State(state): State<AppState>,
    AnonCtx(ctx): AnonCtx,
    headers: HeaderMap,
    Query(q): Q,
) -> Response {
    let ar = field(&q, "ar");
    let Some(h) = hosted(&state, &ctx, &ar).await else {
        return expired(&state);
    };
    let Some(s) = sso(&state, &headers).await else {
        return redirect(
            StatusCode::FOUND,
            &format!("/auth/login?ar={}", urlencode(&ar)),
        );
    };
    let email = user_email(&state, &s).await.unwrap_or_default();
    let page = ConsentPage {
        t: t(),
        chrome: chrome(&state),
        title: t().get("consent.title").into(),
        asks: t().fill("consent.asks", &[("client", &h.client.name)]),
        scopes: h
            .req
            .scopes
            .iter()
            .map(|sc| t().get(&format!("scope.{sc}")).to_owned())
            .collect(),
        signed_in_as: t().fill("consent.as", &[("email", &email)]),
        ar,
    };
    html(
        StatusCode::OK,
        page.render(),
        Some(&h.client.redirect_origin),
    )
}

async fn user_email(state: &AppState, s: &SsoSession) -> Option<String> {
    use rackend_core::ports::StoreExt as _;
    let app = state.app();
    let c = app
        .schema
        .get()
        .schema
        .by_id_or_name(&s.collection_id)
        .cloned()?;
    let id = s.user_id.clone();
    let r = app
        .store
        .read(move |tx| tx.record_get(&c, &id))
        .await
        .ok()??;
    r.get("email").as_str().map(str::to_owned)
}

/// `POST /auth/consent {ar, decision}`.
pub async fn consent_post(
    State(state): State<AppState>,
    AnonCtx(ctx): AnonCtx,
    headers: HeaderMap,
    axum::Form(f): Form,
) -> Response {
    if !same_origin(&state, &headers) {
        return forbidden_origin(&state);
    }
    let ar = field(&f, "ar");
    let Some(h) = hosted(&state, &ctx, &ar).await else {
        return expired(&state);
    };
    let s = sso(&state, &headers).await;
    let allow = f.get("decision").map(String::as_str) == Some("allow");
    authorize_response(
        &state,
        state.app().consent(&ctx, h.req, s, allow).await,
        true,
    )
}

/// `GET /auth/page.js`: passkey sign-in and small enhancements.
pub async fn page_js() -> Response {
    let mut res = include_str!("../assets/page.js").into_response();
    res.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/javascript; charset=utf-8"),
    );
    res.headers_mut().insert(
        CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=3600"),
    );
    res
}

/// `GET /auth/theme.css`: the primary color from `app.primary_color`.
pub async fn theme_css(State(state): State<AppState>) -> Response {
    let color = state
        .app()
        .settings()
        .string("app", "primary_color")
        .filter(|c| {
            let hex = c.strip_prefix('#').unwrap_or_default();
            matches!(hex.len(), 3 | 6) && hex.bytes().all(|b| b.is_ascii_hexdigit())
        });
    let css = match color {
        Some(c) => format!(":root{{--brand:{c};--brand-fg:#fff}}\n"),
        None => String::new(),
    };
    let mut res = css.into_response();
    res.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/css; charset=utf-8"),
    );
    res.headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    res
}

// Admin -------------------------------------------------------------------------

/// `GET /api/oauth-clients`.
pub async fn clients(State(state): State<AppState>, Ctx(ctx): Ctx) -> ApiResult {
    Ok(ok(state.app().oauth_clients(&ctx).await?))
}

/// `POST /api/oauth-clients`.
pub async fn client_create(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Json(body): Json<Value>,
) -> ApiResult {
    Ok(created(
        state.app().oauth_client_save(&ctx, None, body).await?,
    ))
}

/// `PATCH /api/oauth-clients/{id}`.
pub async fn client_update(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    Ok(ok(state
        .app()
        .oauth_client_save(&ctx, Some(&id), body)
        .await?))
}

/// `DELETE /api/oauth-clients/{id}`.
pub async fn client_delete(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Path(id): Path<String>,
) -> ApiResult {
    state.app().oauth_client_delete(&ctx, &id).await?;
    Ok(no_content())
}

/// `GET /api/keys`.
pub async fn keys(State(state): State<AppState>, Ctx(ctx): Ctx) -> ApiResult {
    Ok(ok(state.app().keys_overview(&ctx).await?))
}

/// `POST /api/keys/rotate`.
pub async fn keys_rotate(State(state): State<AppState>, Ctx(ctx): Ctx) -> ApiResult {
    Ok(ok(state.app().keys_rotate(&ctx).await?))
}

/// CORS (SPEC §17.3). The OIDC protocol endpoints answer any origin
/// without credentials (the tokens are the credentials). `/api` answers the
/// origins in `security.cors_origins` (and `http://localhost:*` in dev mode),
/// with credentials for cookie mode. Preflights are answered here.
pub async fn cors(
    State(state): State<AppState>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let path = req.uri().path().to_owned();
    let open =
        path.starts_with("/oauth2/") && path != "/oauth2/authorize" && path != "/oauth2/logout"
            || path.starts_with("/.well-known/");
    let origin = req
        .headers()
        .get(ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let api_origin = (path.starts_with("/api/") && !open)
        .then_some(())
        .and(origin.clone())
        .filter(|o| {
            let o = o.to_ascii_lowercase();
            state
                .app()
                .settings()
                .strings("security", "cors_origins")
                .iter()
                .any(|a| a.trim_end_matches('/').eq_ignore_ascii_case(&o))
                || (state.dev()
                    && (o.starts_with("http://localhost:") || o.starts_with("http://127.0.0.1:")))
        });
    if !open && api_origin.is_none() {
        return next.run(req).await;
    }
    let preflight = req.method() == Method::OPTIONS
        && req.headers().contains_key("access-control-request-method");
    let mut res = if preflight {
        StatusCode::NO_CONTENT.into_response()
    } else {
        next.run(req).await
    };
    let h = res.headers_mut();
    match &api_origin {
        Some(o) => {
            if let Ok(v) = HeaderValue::from_str(o) {
                h.insert("access-control-allow-origin", v);
            }
            h.insert(
                "access-control-allow-credentials",
                HeaderValue::from_static("true"),
            );
            h.insert(
                "access-control-expose-headers",
                HeaderValue::from_static("etag, x-request-id, retry-after"),
            );
            h.append("vary", HeaderValue::from_static("Origin"));
        }
        None => {
            h.insert("access-control-allow-origin", HeaderValue::from_static("*"));
        }
    }
    if preflight {
        h.insert(
            "access-control-allow-methods",
            HeaderValue::from_static("GET, POST, PATCH, PUT, DELETE"),
        );
        h.insert(
            "access-control-allow-headers",
            HeaderValue::from_static("authorization, content-type, if-match, x-rackend-auth-mode"),
        );
        h.insert("access-control-max-age", HeaderValue::from_static("600"));
        res.extensions_mut().insert(RenderedPage);
    }
    res
}
