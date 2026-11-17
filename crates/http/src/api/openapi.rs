//! `GET /api/openapi.json` and `/api/openapi.yaml` (SPEC §10.12).

use axum::extract::State;
use axum::http::HeaderValue;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::response::{IntoResponse as _, Response};
use rackend_core::openapi::{Version, to_json, to_yaml};

use super::ApiResult;
use crate::AppState;
use crate::ctx::Ctx;
use crate::problem::{ApiError, ProblemCode};

fn document(
    state: &AppState,
    ctx: &rackend_core::usecases::RequestCtx,
) -> ApiResult<serde_json::Value> {
    let version = match ctx.query.get("version") {
        None => Version::V31,
        Some(v) => Version::parse(v).ok_or_else(|| {
            ApiError::with_detail(ProblemCode::BadRequest, "version must be 3.1 or 3.0.")
        })?,
    };
    let include_admin = ctx.query.get("include").is_some_and(|i| i == "admin");
    Ok(state.app().openapi_document(ctx, version, include_admin)?)
}

fn with_type(body: String, ct: &'static str) -> Response {
    let mut res = body.into_response();
    res.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static(ct));
    res.headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    res
}

/// `GET /api/openapi.json?version=3.1|3.0&include=admin`.
pub async fn json(State(state): State<AppState>, Ctx(ctx): Ctx) -> ApiResult {
    let doc = document(&state, &ctx)?;
    Ok(with_type(to_json(&doc), "application/json"))
}

/// `GET /api/openapi.yaml`.
pub async fn yaml(State(state): State<AppState>, Ctx(ctx): Ctx) -> ApiResult {
    let doc = document(&state, &ctx)?;
    Ok(with_type(to_yaml(&doc), "application/yaml"))
}
