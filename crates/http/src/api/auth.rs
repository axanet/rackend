//! Setup, superuser authentication and superuser management (SPEC §10.7).

use axum::Json;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::http::header::SET_COOKIE;
use axum::response::{IntoResponse, Response};
use rackend_core::usecases::auth::Tokens;
use serde_json::{Value, json};

use super::{ApiResult, created, no_content, ok, str_field};
use crate::AppState;
use crate::ctx::{Ctx, REFRESH_COOKIE, clear_cookies, cookie, session_cookies, wants_cookies};

/// Path of the superuser refresh endpoint (refresh cookie scope).
pub const SUPERUSER_REFRESH_PATH: &str = "/api/superusers/auth-refresh";

fn token_response(state: &AppState, headers: &HeaderMap, t: Tokens) -> Response {
    let body = json!({
        "access_token": t.access_token, "token_type": "Bearer", "expires_in": t.expires_in,
        "refresh_token": t.refresh_token, "record": t.superuser,
    });
    if !wants_cookies(headers) {
        return ok(body);
    }
    let mut res =
        ok(json!({ "token_type": "Cookie", "expires_in": t.expires_in, "record": t.superuser }));
    for c in session_cookies(
        &t.access_token,
        &t.refresh_token,
        t.expires_in,
        t.refresh_expires_in,
        SUPERUSER_REFRESH_PATH,
        state.secure_cookies(),
    ) {
        res.headers_mut().append(SET_COOKIE, c);
    }
    res
}

/// `GET /api/info`: public instance information.
pub async fn info(State(state): State<AppState>) -> ApiResult {
    let app = state.app();
    let settings = app.settings();
    let needs_setup = app.store_superusers_count().await? == 0;
    Ok(ok(json!({
        "name": settings.string("app", "name").unwrap_or_else(|| "rackend".into()),
        "version": app.version,
        "public_url": settings.public_url(),
        "needs_setup": needs_setup,
        "auth_collections": [],
    })))
}

/// `POST /api/setup {token, email, password, public_url}`.
pub async fn setup(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult {
    let t = state
        .app()
        .setup(
            &ctx,
            str_field(&body, "token")?,
            str_field(&body, "email")?,
            str_field(&body, "password")?,
            str_field(&body, "public_url")?,
        )
        .await?;
    Ok(token_response(&state, &headers, t))
}

/// `POST /api/superusers/auth-with-password {email, password}`.
pub async fn login(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult {
    let t = state
        .app()
        .superuser_login(
            &ctx,
            str_field(&body, "email")?,
            str_field(&body, "password")?,
        )
        .await?;
    Ok(token_response(&state, &headers, t))
}

/// `POST /api/superusers/auth-refresh {refresh_token}` (or the refresh cookie).
pub async fn refresh(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    headers: HeaderMap,
    body: Option<Json<Value>>,
) -> ApiResult {
    let from_body = body
        .as_ref()
        .and_then(|b| b.get("refresh_token"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let token = from_body
        .or_else(|| cookie(&headers, REFRESH_COOKIE))
        .ok_or_else(|| crate::problem::ApiError::from(rackend_core::Error::Unauthorized))?;
    let t = state.app().superuser_refresh(&ctx, &token).await?;
    Ok(token_response(&state, &headers, t))
}

/// `POST /api/superusers/logout`.
pub async fn logout(State(state): State<AppState>, Ctx(ctx): Ctx) -> ApiResult {
    state.app().logout(&ctx).await?;
    let mut res = no_content();
    for c in clear_cookies(SUPERUSER_REFRESH_PATH, state.secure_cookies()) {
        res.headers_mut().append(SET_COOKIE, c);
    }
    Ok(res)
}

/// `GET /api/superusers`.
pub async fn list(State(state): State<AppState>, Ctx(ctx): Ctx) -> ApiResult {
    let items = state.app().superusers_list(&ctx).await?;
    Ok(ok(json!({ "items": items })))
}

/// `POST /api/superusers {email, password}`.
pub async fn create(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Json(body): Json<Value>,
) -> ApiResult {
    let s = state
        .app()
        .superuser_create(
            &ctx,
            str_field(&body, "email")?,
            str_field(&body, "password")?,
        )
        .await?;
    Ok(created(s))
}

/// `PATCH /api/superusers/{id} {email?, password?}`.
pub async fn update(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    let email = body.get("email").and_then(Value::as_str);
    let password = body.get("password").and_then(Value::as_str);
    Ok(ok(state
        .app()
        .superuser_update(&ctx, &id, email, password)
        .await?))
}

/// `DELETE /api/superusers/{id}`.
pub async fn delete(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Path(id): Path<String>,
) -> ApiResult {
    state.app().superuser_delete(&ctx, &id).await?;
    Ok(no_content().into_response())
}
