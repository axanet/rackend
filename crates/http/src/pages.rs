//! Hosted pages for email links (SPEC §11.4): `/auth/verify`, `/auth/reset`
//! and `/auth/email-change`. Plain HTML forms: no scripts, styles from
//! `/auth/page.css`, a strict CSP. Opening a link never changes anything;
//! the form's POST does, so link scanners in mail clients cannot use it up.

use std::collections::BTreeMap;

use axum::Form;
use axum::extract::{Query, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, REFERRER_POLICY};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse as _, Response};
use rackend_core::Error as CoreError;
use rackend_core::auth::broker::TokenPurpose;

use crate::AppState;
use crate::ctx::AnonCtx;
use crate::html::{MARK_SVG, PAGE_CSS, escape};

/// The CSP of hosted pages.
pub const PAGE_CSP: &str = "default-src 'none'; style-src 'self'; img-src 'self' data:; \
form-action 'self'; frame-ancestors 'none'; base-uri 'none'";

/// Form styles on top of [`PAGE_CSS`].
const FORM_CSS: &str = "form{display:grid;gap:14px;margin-top:8px}\
label{display:grid;gap:6px;font:500 14px/1.3 var(--font-sans);color:var(--fg)}\
input{width:100%;min-height:44px;padding:8px 12px;border:1px solid var(--border-strong);border-radius:6px;\
background:var(--bg-elev);color:var(--fg);font:400 16px/1.4 var(--font-sans)}\
input:focus-visible,button:focus-visible,a:focus-visible{outline:2px solid var(--accent);outline-offset:2px}\
input[aria-invalid=true]{border-color:var(--danger)}\
button,a.button{display:inline-flex;align-items:center;justify-content:center;height:44px;padding:0 18px;\
border-radius:999px;border:1px solid var(--brand,var(--fg));background:var(--brand,var(--fg));\
color:var(--brand-fg,var(--bg));font:500 15px/1 var(--font-sans);text-decoration:none;cursor:pointer}\
button:hover,a.button:hover{background:color-mix(in srgb,var(--brand,var(--fg)) 85%,var(--bg))}\
button.secondary,a.button.secondary{background:transparent;color:var(--fg);border-color:var(--border-strong)}\
button.secondary:hover,a.button.secondary:hover{background:var(--surface)}\
button[disabled]{opacity:.5;cursor:not-allowed}\
.alt{display:grid;gap:10px}.alt form{margin:0}.alt button,.alt a.button{width:100%}\
.or{display:flex;align-items:center;gap:12px;margin:6px 0;font-size:13px;color:var(--fg-subtle)}\
.or::before,.or::after{content:\"\";flex:1;border-top:1px solid var(--border)}\
.links{display:flex;flex-wrap:wrap;justify-content:space-between;gap:8px;font-size:14px}\
.scopes{margin:0;padding-left:20px;display:grid;gap:4px;color:var(--fg)}\
.row{display:flex;gap:10px;justify-content:flex-end}.row button{flex:1}\
.mark .logo{max-height:28px;max-width:160px}\
.err{color:var(--danger)}.ok{color:var(--accent)}.hint{font-size:14px;color:var(--fg-subtle)}";

/// Marks a finished HTML page: the problem mapper keeps it as it is, even
/// with an error status.
#[derive(Debug, Clone, Copy)]
pub struct RenderedPage;

type Q = Query<BTreeMap<String, String>>;
type F = Form<BTreeMap<String, String>>;

/// `GET /auth/page.css`.
pub async fn css() -> Response {
    let mut res = format!("{PAGE_CSS}{FORM_CSS}").into_response();
    let h = res.headers_mut();
    h.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/css; charset=utf-8"),
    );
    h.insert(
        CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=3600"),
    );
    res
}

fn html(status: StatusCode, title: &str, body: &str) -> Response {
    let page = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
<meta name=\"robots\" content=\"noindex\">\
<title>{title} · rackend</title><link rel=\"stylesheet\" href=\"/auth/page.css\"></head><body>\
<main><div class=\"mark\">{MARK_SVG}rackend</div>{body}</main></body></html>",
        title = escape(title),
    );
    let mut res = (status, page).into_response();
    res.extensions_mut().insert(RenderedPage);
    let h = res.headers_mut();
    h.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    h.insert(CONTENT_SECURITY_POLICY, HeaderValue::from_static(PAGE_CSP));
    h.insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

/// A page with a heading and a message (results and errors).
pub fn message_page(status: StatusCode, eyebrow: &str, title: &str, message: &str) -> Response {
    let class = if status.is_success() {
        ""
    } else {
        " class=\"err\""
    };
    html(
        status,
        title,
        &format!(
            "<span class=\"eyebrow\">{}</span><h1>{}</h1><p{class}>{}</p>",
            escape(eyebrow),
            escape(title),
            escape(message)
        ),
    )
}

fn invalid_link() -> Response {
    message_page(
        StatusCode::BAD_REQUEST,
        "invalid_token",
        "This link does not work",
        "It was used already or has expired. Request a new one from the app.",
    )
}

/// A user-facing message for a failed confirmation.
fn failure(e: &CoreError) -> String {
    match e {
        CoreError::Validation(v) => {
            v.0.values()
                .flat_map(|errs| errs.iter().map(|e| e.message.clone()))
                .collect::<Vec<_>>()
                .join(" ")
        }
        CoreError::Conflict(_) => "This email address belongs to another account.".into(),
        CoreError::RateLimited { .. } => "Too many attempts. Try again later.".into(),
        _ => "Something went wrong. Try again later.".into(),
    }
}

fn hidden(c: &str, token: &str) -> String {
    format!(
        "<input type=\"hidden\" name=\"c\" value=\"{}\"><input type=\"hidden\" name=\"token\" value=\"{}\">",
        escape(c),
        escape(token)
    )
}

fn params(q: &BTreeMap<String, String>) -> Option<(&str, &str)> {
    Some((q.get("c")?.as_str(), q.get("token")?.as_str()))
}

async fn valid(state: &AppState, c: &str, token: &str, purpose: TokenPurpose) -> Option<bool> {
    state
        .app()
        .token_info(c, token, purpose)
        .await
        .ok()
        .filter(|i| i.valid)
        .map(|i| i.needs_password)
}

// Verification -------------------------------------------------------------

/// `GET /auth/verify?c=&token=`.
pub async fn verify_form(State(state): State<AppState>, Query(q): Q) -> Response {
    let Some((c, token)) = params(&q) else {
        return invalid_link();
    };
    if valid(&state, c, token, TokenPurpose::VerifyEmail)
        .await
        .is_none()
    {
        return invalid_link();
    }
    html(
        StatusCode::OK,
        "Verify your email",
        &format!(
            "<span class=\"eyebrow\">Email</span><h1>Verify your email</h1>\
<p>Confirm that this address belongs to you.</p>\
<form method=\"post\" action=\"/auth/verify\">{}<button type=\"submit\">Verify email</button></form>",
            hidden(c, token)
        ),
    )
}

/// `POST /auth/verify`.
pub async fn verify_submit(
    State(state): State<AppState>,
    AnonCtx(ctx): AnonCtx,
    Form(f): F,
) -> Response {
    let Some((c, token)) = params(&f) else {
        return invalid_link();
    };
    match state.app().confirm_verification(&ctx, c, token).await {
        Ok(()) => message_page(
            StatusCode::OK,
            "Email",
            "Email verified",
            "Thanks. You can close this page and return to the app.",
        ),
        Err(CoreError::Validation(_)) => invalid_link(),
        Err(e) => message_page(
            StatusCode::BAD_REQUEST,
            "Email",
            "Verification failed",
            &failure(&e),
        ),
    }
}

// Password reset -----------------------------------------------------------

fn reset_page(status: StatusCode, c: &str, token: &str, error: Option<&str>) -> Response {
    let err = error
        .map(|e| format!("<p class=\"err\" role=\"alert\">{}</p>", escape(e)))
        .unwrap_or_default();
    let invalid = if error.is_some() {
        " aria-invalid=\"true\""
    } else {
        ""
    };
    html(
        status,
        "Choose a new password",
        &format!(
            "<span class=\"eyebrow\">Password</span><h1>Choose a new password</h1>{err}\
<form method=\"post\" action=\"/auth/reset\">{}\
<label>New password<input type=\"password\" name=\"password\" autocomplete=\"new-password\" required maxlength=\"256\"{invalid}></label>\
<label>Repeat the password<input type=\"password\" name=\"password_confirm\" autocomplete=\"new-password\" required maxlength=\"256\"></label>\
<p class=\"hint\">You will be signed out on every device.</p>\
<button type=\"submit\">Save password</button></form>",
            hidden(c, token)
        ),
    )
}

/// `GET /auth/reset?c=&token=`.
pub async fn reset_form(State(state): State<AppState>, Query(q): Q) -> Response {
    let Some((c, token)) = params(&q) else {
        return invalid_link();
    };
    if valid(&state, c, token, TokenPurpose::ResetPassword)
        .await
        .is_none()
    {
        return invalid_link();
    }
    reset_page(StatusCode::OK, c, token, None)
}

/// `POST /auth/reset`.
pub async fn reset_submit(
    State(state): State<AppState>,
    AnonCtx(ctx): AnonCtx,
    Form(f): F,
) -> Response {
    let Some((c, token)) = params(&f) else {
        return invalid_link();
    };
    let pw = f.get("password").map(String::as_str).unwrap_or_default();
    if f.get("password_confirm").map(String::as_str) != Some(pw) {
        return reset_page(
            StatusCode::BAD_REQUEST,
            c,
            token,
            Some("The passwords do not match."),
        );
    }
    match state.app().confirm_password_reset(&ctx, c, token, pw).await {
        Ok(()) => message_page(
            StatusCode::OK,
            "Password",
            "Password changed",
            "Sign in again with your new password.",
        ),
        Err(CoreError::Validation(v)) if v.0.contains_key("token") => invalid_link(),
        Err(e) => reset_page(StatusCode::BAD_REQUEST, c, token, Some(&failure(&e))),
    }
}

// Email change -------------------------------------------------------------

fn email_change_page(
    status: StatusCode,
    c: &str,
    token: &str,
    needs_password: bool,
    error: Option<&str>,
) -> Response {
    let err = error
        .map(|e| format!("<p class=\"err\" role=\"alert\">{}</p>", escape(e)))
        .unwrap_or_default();
    let pw = if needs_password {
        "<label>Current password<input type=\"password\" name=\"password\" autocomplete=\"current-password\" required maxlength=\"256\"></label>"
    } else {
        ""
    };
    html(
        status,
        "Confirm your new email",
        &format!(
            "<span class=\"eyebrow\">Email</span><h1>Confirm your new email</h1>{err}\
<p>Your account will use this address from now on.</p>\
<form method=\"post\" action=\"/auth/email-change\">{}{pw}\
<p class=\"hint\">You will be signed out on every device.</p>\
<button type=\"submit\">Change email</button></form>",
            hidden(c, token)
        ),
    )
}

/// `GET /auth/email-change?c=&token=`.
pub async fn email_change_form(State(state): State<AppState>, Query(q): Q) -> Response {
    let Some((c, token)) = params(&q) else {
        return invalid_link();
    };
    let Some(needs_password) = valid(&state, c, token, TokenPurpose::ChangeEmail).await else {
        return invalid_link();
    };
    email_change_page(StatusCode::OK, c, token, needs_password, None)
}

/// `POST /auth/email-change`.
pub async fn email_change_submit(
    State(state): State<AppState>,
    AnonCtx(ctx): AnonCtx,
    Form(f): F,
) -> Response {
    let Some((c, token)) = params(&f) else {
        return invalid_link();
    };
    let pw = f.get("password").map(String::as_str);
    match state.app().confirm_email_change(&ctx, c, token, pw).await {
        Ok(()) => message_page(
            StatusCode::OK,
            "Email",
            "Email changed",
            "Sign in again with your new address.",
        ),
        Err(CoreError::Validation(v)) if v.0.contains_key("token") => invalid_link(),
        Err(e) => {
            let needs = valid(&state, c, token, TokenPurpose::ChangeEmail)
                .await
                .unwrap_or(true);
            email_change_page(StatusCode::BAD_REQUEST, c, token, needs, Some(&failure(&e)))
        }
    }
}
