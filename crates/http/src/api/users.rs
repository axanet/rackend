//! Auth-collection endpoints: password login, refresh, logout, sessions and
//! passkeys (SPEC §10.6). Superuser passkeys live here too.

use std::net::IpAddr;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::http::header::SET_COOKIE;
use rackend_core::usecases::RequestCtx;
use rackend_core::usecases::auth::Tokens;
use rackend_core::usecases::passkeys::PasskeyScope;
use rackend_core::usecases::sessions::RefreshScope;
use serde_json::{Value, json};

use super::auth::SUPERUSER_REFRESH_PATH;
use super::{ApiResult, created, flag, no_content, ok, str_field};
use crate::AppState;
use crate::ctx::{Ctx, REFRESH_COOKIE, clear_cookies, cookie, session_cookies, wants_cookies};
use crate::problem::ApiError;
use crate::ratelimit::limited_error;

/// Refresh cookie path of collection `c` (as written in the URL).
pub fn refresh_path(c: &str) -> String {
    format!("/api/collections/{c}/auth-refresh")
}

/// Login response: tokens in the body, or cookies in cookie mode.
pub fn token_response(
    state: &AppState,
    headers: &HeaderMap,
    t: Tokens,
    refresh_path: &str,
) -> axum::response::Response {
    if !wants_cookies(headers) {
        return ok(json!({
            "access_token": t.access_token, "token_type": "Bearer", "expires_in": t.expires_in,
            "refresh_token": t.refresh_token, "record": t.record,
        }));
    }
    let mut res =
        ok(json!({ "token_type": "Cookie", "expires_in": t.expires_in, "record": t.record }));
    for c in session_cookies(
        &t.access_token,
        &t.refresh_token,
        t.expires_in,
        t.refresh_expires_in,
        refresh_path,
        state.secure_cookies(),
    ) {
        res.headers_mut().append(SET_COOKIE, c);
    }
    res
}

/// Checks the login limits for `account` (SPEC §11.10).
pub fn check_login_limit(state: &AppState, ctx: &RequestCtx, account: &str) -> ApiResult<()> {
    let ip = ctx.ip.as_deref().and_then(|i| i.parse::<IpAddr>().ok());
    state
        .limits()
        .check_login(state.limit_config(), ip, account)
        .map_err(limited_error)
}

/// The refresh token from the body or the refresh cookie.
pub fn refresh_token(headers: &HeaderMap, body: Option<&Value>) -> ApiResult<String> {
    body.and_then(|b| b.get("refresh_token"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| cookie(headers, REFRESH_COOKIE).filter(|t| !t.is_empty()))
        .ok_or_else(|| ApiError::from(rackend_core::Error::Unauthorized))
}

/// `GET /api/collections/{c}/auth-methods`.
pub async fn auth_methods(State(state): State<AppState>, Path(c): Path<String>) -> ApiResult {
    Ok(ok(state.app().auth_methods(&c)?))
}

/// `POST /api/collections/{c}/auth-with-password {email, password}`.
pub async fn login(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Path(c): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult {
    let email = str_field(&body, "email")?;
    let pw = str_field(&body, "password")?;
    let cid = state.app().auth_collection_id(&c)?;
    let account = format!("{cid}:{}", email.trim().to_lowercase());
    check_login_limit(&state, &ctx, &account)?;
    let t = state.app().user_login(&ctx, &c, email, pw).await?;
    state.limits().login_succeeded(&account);
    Ok(token_response(&state, &headers, t, &refresh_path(&c)))
}

/// `POST /api/collections/{c}/auth-refresh {refresh_token}` (or the cookie).
pub async fn refresh(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Path(c): Path<String>,
    headers: HeaderMap,
    body: Option<Json<Value>>,
) -> ApiResult {
    let token = refresh_token(&headers, body.as_ref().map(|b| &b.0))?;
    let cid = state.app().auth_collection_id(&c)?;
    let t = state
        .app()
        .refresh(&ctx, RefreshScope::Collection(&cid), &token)
        .await?;
    Ok(token_response(&state, &headers, t, &refresh_path(&c)))
}

/// `POST /api/collections/{c}/logout`.
pub async fn logout(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Path(c): Path<String>,
) -> ApiResult {
    state.app().logout(&ctx).await?;
    let mut res = no_content();
    for v in clear_cookies(&refresh_path(&c), state.secure_cookies()) {
        res.headers_mut().append(SET_COOKIE, v);
    }
    Ok(res)
}

/// `GET /api/collections/{c}/sessions`: the caller's devices.
pub async fn sessions(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Path(c): Path<String>,
) -> ApiResult {
    let items = state.app().user_sessions(&ctx, &c).await?;
    Ok(ok(json!({ "items": items })))
}

/// `DELETE /api/collections/{c}/sessions?all=true`: signs out every other device.
pub async fn sessions_revoke_all(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Path(c): Path<String>,
) -> ApiResult {
    if !flag(&ctx.query, "all") {
        return Err(ApiError::with_detail(
            crate::ProblemCode::BadRequest,
            "Pass ?all=true to sign out every other session.",
        ));
    }
    state.app().user_revoke_session(&ctx, &c, None).await?;
    Ok(no_content())
}

/// `DELETE /api/collections/{c}/sessions/{id}`.
pub async fn session_revoke(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Path((c, id)): Path<(String, String)>,
) -> ApiResult {
    state.app().user_revoke_session(&ctx, &c, Some(&id)).await?;
    Ok(no_content())
}

fn ceremony_id(body: &Value) -> ApiResult<&str> {
    str_field(body, "ceremony_id")
}

fn credential(body: &Value) -> ApiResult<Value> {
    body.get("credential").cloned().ok_or_else(|| {
        str_field(body, "credential")
            .err()
            .unwrap_or_else(|| ApiError::new(crate::ProblemCode::BadRequest))
    })
}

async fn pk_list(state: &AppState, ctx: &RequestCtx, scope: PasskeyScope) -> ApiResult {
    let items = state.app().passkeys_list(ctx, &scope).await?;
    Ok(ok(json!({ "items": items })))
}

async fn pk_delete(state: &AppState, ctx: &RequestCtx, scope: PasskeyScope, id: &str) -> ApiResult {
    state.app().passkey_delete(ctx, &scope, id).await?;
    Ok(no_content())
}

async fn pk_register_start(state: &AppState, ctx: &RequestCtx, scope: PasskeyScope) -> ApiResult {
    Ok(ok(state.app().passkey_register_start(ctx, &scope).await?))
}

async fn pk_register_finish(
    state: &AppState,
    ctx: &RequestCtx,
    scope: PasskeyScope,
    body: &Value,
) -> ApiResult {
    let name = body.get("name").and_then(Value::as_str).map(str::to_owned);
    let pk = state
        .app()
        .passkey_register_finish(ctx, &scope, ceremony_id(body)?, credential(body)?, name)
        .await?;
    Ok(created(pk))
}

async fn pk_login_finish(
    state: &AppState,
    ctx: &RequestCtx,
    scope: PasskeyScope,
    headers: &HeaderMap,
    body: &Value,
    refresh_path: &str,
) -> ApiResult {
    // Passkey logins share the per-IP login bucket; the account is unknown
    // until the assertion is verified, so the ceremony id stands in.
    let id = ceremony_id(body)?;
    check_login_limit(state, ctx, &format!("passkey:{id}"))?;
    let t = state
        .app()
        .passkey_login_finish(ctx, &scope, id, credential(body)?)
        .await?;
    Ok(token_response(state, headers, t, refresh_path))
}

/// `GET /api/collections/{c}/passkeys`.
pub async fn passkeys(
    State(s): State<AppState>,
    Ctx(ctx): Ctx,
    Path(c): Path<String>,
) -> ApiResult {
    pk_list(&s, &ctx, PasskeyScope::Collection(c)).await
}

/// `DELETE /api/collections/{c}/passkeys/{id}`.
pub async fn passkey_delete(
    State(s): State<AppState>,
    Ctx(ctx): Ctx,
    Path((c, id)): Path<(String, String)>,
) -> ApiResult {
    pk_delete(&s, &ctx, PasskeyScope::Collection(c), &id).await
}

/// `POST /api/collections/{c}/passkeys/register/start`.
pub async fn passkey_register_start(
    State(s): State<AppState>,
    Ctx(ctx): Ctx,
    Path(c): Path<String>,
) -> ApiResult {
    pk_register_start(&s, &ctx, PasskeyScope::Collection(c)).await
}

/// `POST /api/collections/{c}/passkeys/register/finish {ceremony_id, credential, name?}`.
pub async fn passkey_register_finish(
    State(s): State<AppState>,
    Ctx(ctx): Ctx,
    Path(c): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    pk_register_finish(&s, &ctx, PasskeyScope::Collection(c), &body).await
}

/// `POST /api/collections/{c}/passkeys/login/start`.
pub async fn passkey_login_start(State(s): State<AppState>, Path(c): Path<String>) -> ApiResult {
    Ok(ok(s
        .app()
        .passkey_login_start(&PasskeyScope::Collection(c))?))
}

/// `POST /api/collections/{c}/passkeys/login/finish {ceremony_id, credential}`.
pub async fn passkey_login_finish(
    State(s): State<AppState>,
    Ctx(ctx): Ctx,
    Path(c): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult {
    let path = refresh_path(&c);
    pk_login_finish(
        &s,
        &ctx,
        PasskeyScope::Collection(c),
        &headers,
        &body,
        &path,
    )
    .await
}

/// `GET /api/superusers/passkeys`.
pub async fn su_passkeys(State(s): State<AppState>, Ctx(ctx): Ctx) -> ApiResult {
    pk_list(&s, &ctx, PasskeyScope::Superuser).await
}

/// `DELETE /api/superusers/passkeys/{id}`.
pub async fn su_passkey_delete(
    State(s): State<AppState>,
    Ctx(ctx): Ctx,
    Path(id): Path<String>,
) -> ApiResult {
    pk_delete(&s, &ctx, PasskeyScope::Superuser, &id).await
}

/// `POST /api/superusers/passkeys/register/start`.
pub async fn su_passkey_register_start(State(s): State<AppState>, Ctx(ctx): Ctx) -> ApiResult {
    pk_register_start(&s, &ctx, PasskeyScope::Superuser).await
}

/// `POST /api/superusers/passkeys/register/finish`.
pub async fn su_passkey_register_finish(
    State(s): State<AppState>,
    Ctx(ctx): Ctx,
    Json(body): Json<Value>,
) -> ApiResult {
    pk_register_finish(&s, &ctx, PasskeyScope::Superuser, &body).await
}

/// `POST /api/superusers/passkeys/login/start`.
pub async fn su_passkey_login_start(State(s): State<AppState>) -> ApiResult {
    Ok(ok(s.app().passkey_login_start(&PasskeyScope::Superuser)?))
}

/// `POST /api/superusers/passkeys/login/finish`.
pub async fn su_passkey_login_finish(
    State(s): State<AppState>,
    Ctx(ctx): Ctx,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult {
    pk_login_finish(
        &s,
        &ctx,
        PasskeyScope::Superuser,
        &headers,
        &body,
        SUPERUSER_REFRESH_PATH,
    )
    .await
}
