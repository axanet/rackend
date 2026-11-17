//! Email flows, external providers, identities (SPEC §10.6, §11.4, §11.7)
//! and their admin endpoints.

use std::net::IpAddr;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::header::{CACHE_CONTROL, LOCATION, SET_COOKIE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse as _, Response};
use rackend_core::usecases::RequestCtx;
use rackend_core::usecases::broker::{CallbackOutcome, CallbackParams, STATE_COOKIE};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::users::{check_login_limit, refresh_path, token_response};
use super::{ApiResult, created, flag, no_content, ok, str_field};
use crate::AppState;
use crate::ctx::{Ctx, cookie};
use crate::ratelimit::limited_error;

type Q = Query<std::collections::BTreeMap<String, String>>;

/// The email limits (SPEC §11.10): per IP and per target address.
fn email_limit(state: &AppState, ctx: &RequestCtx, address: &str) -> ApiResult<()> {
    let ip = ctx.ip.as_deref().and_then(|i| i.parse::<IpAddr>().ok());
    state
        .limits()
        .check_email(state.limit_config(), ip, address)
        .map_err(limited_error)
}

fn opt_str<'a>(body: &'a Value, key: &str) -> Option<&'a str> {
    body.get(key).and_then(Value::as_str)
}

/// `POST /api/collections/{c}/request-verification {email}`: always 204.
pub async fn request_verification(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Path(c): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    let email = str_field(&body, "email")?;
    email_limit(&state, &ctx, email)?;
    state.app().request_verification(&ctx, &c, email).await?;
    Ok(no_content())
}

/// `POST /api/collections/{c}/confirm-verification {token}`.
pub async fn confirm_verification(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Path(c): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    let token = str_field(&body, "token")?;
    state.app().confirm_verification(&ctx, &c, token).await?;
    Ok(no_content())
}

/// `POST /api/collections/{c}/request-password-reset {email}`: always 204.
pub async fn request_password_reset(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Path(c): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    let email = str_field(&body, "email")?;
    email_limit(&state, &ctx, email)?;
    state.app().request_password_reset(&ctx, &c, email).await?;
    Ok(no_content())
}

/// `POST /api/collections/{c}/confirm-password-reset {token, password}`.
pub async fn confirm_password_reset(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Path(c): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    let token = str_field(&body, "token")?;
    let password = str_field(&body, "password")?;
    state
        .app()
        .confirm_password_reset(&ctx, &c, token, password)
        .await?;
    Ok(no_content())
}

/// `POST /api/collections/{c}/request-email-change {new_email}` (signed in).
pub async fn request_email_change(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Path(c): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    let email = str_field(&body, "new_email")?;
    email_limit(&state, &ctx, email)?;
    state.app().request_email_change(&ctx, &c, email).await?;
    Ok(no_content())
}

/// `POST /api/collections/{c}/confirm-email-change {token, password?}`.
pub async fn confirm_email_change(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Path(c): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    let token = str_field(&body, "token")?;
    state
        .app()
        .confirm_email_change(&ctx, &c, token, opt_str(&body, "password"))
        .await?;
    Ok(no_content())
}

/// `POST /api/collections/{c}/request-otp {email}` → `{otp_id}`, always.
pub async fn request_otp(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Path(c): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    let email = str_field(&body, "email")?;
    email_limit(&state, &ctx, email)?;
    let otp_id = state.app().request_otp(&ctx, &c, email).await?;
    Ok(ok(json!({ "otp_id": otp_id })))
}

/// `POST /api/collections/{c}/auth-with-otp {otp_id, code}`.
pub async fn auth_with_otp(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Path(c): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult {
    let otp_id = str_field(&body, "otp_id")?;
    let code = str_field(&body, "code")?;
    let cid = state.app().auth_collection_id(&c)?;
    let account = format!("{cid}:otp:{otp_id}");
    check_login_limit(&state, &ctx, &account)?;
    let t = state.app().auth_with_otp(&ctx, &c, otp_id, code).await?;
    state.limits().login_succeeded(&account);
    Ok(token_response(&state, &headers, t, &refresh_path(&c)))
}

/// `POST /api/collections/{c}/request-magic-link {email, redirect_uri}`.
pub async fn request_magic_link(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Path(c): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    let email = str_field(&body, "email")?;
    let redirect_uri = str_field(&body, "redirect_uri")?;
    email_limit(&state, &ctx, email)?;
    state
        .app()
        .request_magic_link(&ctx, &c, email, redirect_uri)
        .await?;
    Ok(no_content())
}

fn token_key(cid: &str, kind: &str, token: &str) -> String {
    let d = Sha256::digest(token.as_bytes());
    let hex: String = d.iter().take(8).map(|b| format!("{b:02x}")).collect();
    format!("{cid}:{kind}:{hex}")
}

/// `POST /api/collections/{c}/auth-with-magic-link {token}`.
pub async fn auth_with_magic_link(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Path(c): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult {
    let token = str_field(&body, "token")?;
    let cid = state.app().auth_collection_id(&c)?;
    check_login_limit(&state, &ctx, &token_key(&cid, "magic", token))?;
    let t = state.app().auth_with_magic_link(&ctx, &c, token).await?;
    Ok(token_response(&state, &headers, t, &refresh_path(&c)))
}

/// `POST /api/collections/{c}/auth-with-exchange-code {code}`.
pub async fn auth_with_exchange_code(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Path(c): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult {
    let code = str_field(&body, "code")?;
    let cid = state.app().auth_collection_id(&c)?;
    check_login_limit(&state, &ctx, &token_key(&cid, "exchange", code))?;
    let t = state.app().auth_with_exchange_code(&ctx, &c, code).await?;
    Ok(token_response(&state, &headers, t, &refresh_path(&c)))
}

fn state_cookie(value: &str, path: &str, max_age: i64, secure: bool) -> Option<HeaderValue> {
    let sec = if secure { "; Secure" } else { "" };
    HeaderValue::from_str(&format!(
        "{STATE_COOKIE}={value}; Path={path}; HttpOnly; SameSite=Lax; Max-Age={max_age}{sec}"
    ))
    .ok()
}

/// `GET /api/collections/{c}/providers/{name}/start?redirect_uri=&link=&format=json`:
/// redirects to the provider (or answers `{url}` with `format=json`) and
/// sets the encrypted state cookie.
pub async fn provider_start(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Path((c, name)): Path<(String, String)>,
    Query(q): Q,
) -> ApiResult {
    let link = flag(&q, "link");
    let started = state
        .app()
        .provider_start(
            &ctx,
            &c,
            &name,
            q.get("redirect_uri").map(String::as_str),
            link,
        )
        .await?;
    let secure = started.secure || state.secure_cookies();
    let mut res = if q.get("format").map(String::as_str) == Some("json") {
        ok(json!({ "url": started.url }))
    } else {
        let mut r = StatusCode::FOUND.into_response();
        if let Ok(v) = HeaderValue::from_str(&started.url) {
            r.headers_mut().insert(LOCATION, v);
        }
        r
    };
    let max_age = rackend_core::usecases::broker::STATE_TTL.whole_seconds();
    if let Some(v) = state_cookie(&started.cookie, &started.cookie_path, max_age, secure) {
        res.headers_mut().append(SET_COOKIE, v);
    }
    res.headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(res)
}

/// `GET /api/collections/{c}/providers/{name}/callback`.
pub async fn provider_callback(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Path((c, name)): Path<(String, String)>,
    Query(q): Q,
    headers: HeaderMap,
) -> Response {
    let params = CallbackParams {
        code: q.get("code").cloned(),
        state: q.get("state").cloned(),
        error: q.get("error").cloned(),
        cookie: cookie(&headers, STATE_COOKIE),
    };
    let outcome = state.app().provider_callback(&ctx, &c, &name, params).await;
    let mut res = match outcome {
        CallbackOutcome::Redirect(url) => {
            let mut r = StatusCode::SEE_OTHER.into_response();
            if let Ok(v) = HeaderValue::from_str(&url) {
                r.headers_mut().insert(LOCATION, v);
            }
            r
        }
        CallbackOutcome::Error { code, message } => {
            crate::pages::message_page(StatusCode::BAD_REQUEST, code, "Sign-in failed", &message)
        }
    };
    let path = format!("/api/collections/{c}/providers/{name}/callback");
    if let Some(v) = state_cookie("", &path, 0, state.secure_cookies()) {
        res.headers_mut().append(SET_COOKIE, v);
    }
    res.headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

/// `GET /api/collections/{c}/identities`.
pub async fn identities(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Path(c): Path<String>,
) -> ApiResult {
    let items = state.app().identities_list(&ctx, &c).await?;
    Ok(ok(json!({ "items": items })))
}

/// `DELETE /api/collections/{c}/identities/{id}`.
pub async fn identity_delete(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Path((c, id)): Path<(String, String)>,
) -> ApiResult {
    state.app().identity_unlink(&ctx, &c, &id).await?;
    Ok(no_content())
}

// Admin --------------------------------------------------------------------

/// `GET /api/auth-providers` → `{items, presets}`.
pub async fn providers(State(state): State<AppState>, Ctx(ctx): Ctx) -> ApiResult {
    Ok(ok(state.app().providers_list(&ctx).await?))
}

/// `POST /api/auth-providers`.
pub async fn provider_create(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Json(body): Json<Value>,
) -> ApiResult {
    Ok(created(state.app().provider_save(&ctx, None, body).await?))
}

/// `PATCH /api/auth-providers/{id}`.
pub async fn provider_update(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    Ok(ok(state.app().provider_save(&ctx, Some(&id), body).await?))
}

/// `DELETE /api/auth-providers/{id}`.
pub async fn provider_delete(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Path(id): Path<String>,
) -> ApiResult {
    state.app().provider_delete(&ctx, &id).await?;
    Ok(no_content())
}

/// `POST /api/auth-providers/discover {issuer, collection?, name?}`.
pub async fn provider_discover(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Json(body): Json<Value>,
) -> ApiResult {
    Ok(ok(state.app().provider_discover(&ctx, &body).await?))
}

/// `POST /api/settings/test-smtp {to, smtp?}`.
pub async fn test_smtp(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Json(body): Json<Value>,
) -> ApiResult {
    let to = str_field(&body, "to")?;
    Ok(ok(state
        .app()
        .smtp_test(&ctx, to, body.get("smtp").cloned())
        .await?))
}
