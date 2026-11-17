//! Records (SPEC §10.5).

use axum::Json;
use axum::extract::{Path, State};
use axum::http::header::{ETAG, IF_MATCH};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use rackend_core::usecases::records::{BatchItem, ListParams};
use serde_json::{Map, Value, json};

use super::{ApiResult, flag, no_content, ok};
use crate::AppState;
use crate::ctx::Ctx;
use crate::problem::{ApiError, ProblemCode};

fn params(q: &std::collections::BTreeMap<String, String>) -> ListParams {
    ListParams {
        page: q.get("page").and_then(|v| v.parse().ok()),
        per_page: q.get("per_page").and_then(|v| v.parse().ok()),
        sort: q.get("sort").cloned(),
        filter: q.get("filter").cloned(),
        expand: q.get("expand").cloned(),
        fields: q.get("fields").cloned(),
        skip_total: flag(q, "skip_total"),
    }
}

fn with_etag(mut res: Response, record: &Map<String, Value>) -> Response {
    if let Some(Ok(v)) = record
        .get("updated")
        .and_then(Value::as_str)
        .map(|u| HeaderValue::from_str(&format!("\"{u}\"")))
    {
        res.headers_mut().insert(ETAG, v);
    }
    res
}

fn if_match(h: &HeaderMap) -> Option<String> {
    h.get(IF_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

/// `GET /api/collections/{c}/records`.
pub async fn list(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Path(c): Path<String>,
) -> ApiResult {
    let p = params(&ctx.query);
    Ok(ok(state.app().records_list(&ctx, &c, p).await?))
}

/// `GET /api/collections/{c}/records/{id}`.
pub async fn view(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Path((c, id)): Path<(String, String)>,
) -> ApiResult {
    let p = params(&ctx.query);
    let r = state.app().record_view(&ctx, &c, &id, p).await?;
    Ok(with_etag(ok(Value::Object(r.clone())), &r))
}

/// `POST /api/collections/{c}/records`. Returns 204 when the caller may not
/// view the created record.
pub async fn create(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Path(c): Path<String>,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    Ok(match state.app().record_create(&ctx, &c, body).await? {
        Some(r) => with_etag(
            (StatusCode::CREATED, Json(Value::Object(r.clone()))).into_response(),
            &r,
        ),
        None => no_content(),
    })
}

/// `PATCH /api/collections/{c}/records/{id}` (`If-Match` supported).
pub async fn update(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    headers: HeaderMap,
    Path((c, id)): Path<(String, String)>,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    Ok(
        match state
            .app()
            .record_update(&ctx, &c, &id, body, if_match(&headers))
            .await?
        {
            Some(r) => with_etag(ok(Value::Object(r.clone())), &r),
            None => no_content(),
        },
    )
}

/// `DELETE /api/collections/{c}/records/{id}` (`If-Match` supported).
pub async fn delete(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    headers: HeaderMap,
    Path((c, id)): Path<(String, String)>,
) -> ApiResult {
    state
        .app()
        .record_delete(&ctx, &c, &id, if_match(&headers))
        .await?;
    Ok(no_content())
}

/// `POST /api/batch {requests: [{method, url, body}]}`. On failure the
/// response is the failing sub-request's problem, with `failed_index` and
/// the per-request `results`; nothing was committed.
pub async fn batch(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Json(body): Json<Value>,
) -> ApiResult {
    let Some(requests) = body.get("requests").and_then(Value::as_array) else {
        return Err(ApiError::with_detail(
            ProblemCode::BadRequest,
            "Expected {\"requests\": [...]}.",
        ));
    };
    let mut items = Vec::with_capacity(requests.len());
    for (i, r) in requests.iter().enumerate() {
        let method = r.get("method").and_then(Value::as_str);
        let url = r.get("url").and_then(Value::as_str);
        let (Some(method), Some(url)) = (method, url) else {
            return Err(ApiError::with_detail(
                ProblemCode::BadRequest,
                format!("requests[{i}] needs method and url."),
            ));
        };
        let body = match r.get("body") {
            None | Some(Value::Null) => Map::new(),
            Some(Value::Object(m)) => m.clone(),
            Some(_) => {
                return Err(ApiError::with_detail(
                    ProblemCode::BadRequest,
                    format!("requests[{i}].body must be an object."),
                ));
            }
        };
        items.push(BatchItem {
            method: method.to_owned(),
            url: url.to_owned(),
            body,
        });
    }
    let n = items.len();
    match state.app().records_batch(&ctx, items).await? {
        Ok(results) => {
            let out: Vec<Value> = results
                .into_iter()
                .map(|r| json!({ "status": r.status, "body": r.body }))
                .collect();
            Ok(ok(Value::Array(out)))
        }
        Err(failure) => {
            let mut err = ApiError::from(failure.error);
            let status = err.problem().code.status().as_u16();
            let results: Vec<Value> = (0..n)
                .map(|i| match i.cmp(&failure.index) {
                    std::cmp::Ordering::Less => json!({ "status": "rolled_back" }),
                    std::cmp::Ordering::Equal => json!({ "status": status }),
                    std::cmp::Ordering::Greater => json!({ "status": "skipped" }),
                })
                .collect();
            let p = err.problem_mut();
            p.extra.insert("failed_index".into(), json!(failure.index));
            p.extra.insert("results".into(), Value::Array(results));
            Err(err)
        }
    }
}
