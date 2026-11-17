//! Settings and audit endpoints (SPEC §10.11).

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Query, State};
use rackend_core::ports::AuditQuery;
use serde_json::{Value, json};

use super::{ApiResult, ok};
use crate::AppState;
use crate::ctx::Ctx;

/// `GET /api/settings`.
pub async fn settings_get(State(state): State<AppState>, Ctx(ctx): Ctx) -> ApiResult {
    Ok(ok(state.app().settings_get(&ctx)?))
}

/// `PATCH /api/settings` (`?force=true` allows switching the storage
/// backend while files exist).
pub async fn settings_patch(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Json(body): Json<Value>,
) -> ApiResult {
    let force = super::flag(&ctx.query, "force");
    Ok(ok(state
        .app()
        .settings_patch_with(&ctx, body, force)
        .await?))
}

/// `GET /api/audit?actor_type=&actor_id=&action=&collection_id=&record_id=&from=&to=&page=&per_page=&sort=`.
pub async fn audit(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Query(q): Query<BTreeMap<String, String>>,
) -> ApiResult {
    let per_page: u32 = q
        .get("per_page")
        .and_then(|v| v.parse().ok())
        .unwrap_or(50)
        .clamp(1, 500);
    let page: u32 = q
        .get("page")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1)
        .max(1);
    let query = AuditQuery {
        actor_type: q.get("actor_type").cloned(),
        actor_id: q.get("actor_id").cloned(),
        action: q.get("action").cloned(),
        collection_id: q.get("collection_id").cloned(),
        record_id: q.get("record_id").cloned(),
        from: q.get("from").cloned(),
        to: q.get("to").cloned(),
        newest_first: q.get("sort").is_none_or(|s| s != "ts"),
        limit: per_page,
        offset: u64::from(page - 1) * u64::from(per_page),
    };
    if q.get("format").is_some_and(|f| f == "ndjson") {
        return ndjson(state, ctx, query).await;
    }
    let (items, total) = state.app().audit_list(&ctx, query).await?;
    Ok(ok(json!({
        "page": page, "per_page": per_page, "total_items": total,
        "total_pages": total.div_ceil(u64::from(per_page)), "items": items,
    })))
}

/// Streams every matching audit entry as NDJSON, page by page.
async fn ndjson(
    state: AppState,
    ctx: rackend_core::usecases::RequestCtx,
    mut query: AuditQuery,
) -> ApiResult {
    // Fail early (e.g. not a superuser) before the stream starts.
    query.limit = 1000;
    query.offset = 0;
    let (first, _) = state.app().audit_list(&ctx, query.clone()).await?;
    let stream = futures_util::stream::unfold(
        (state, ctx, query, Some(first)),
        |(state, ctx, mut query, page)| async move {
            let items = match page {
                Some(p) => p,
                None => {
                    query.offset += u64::from(query.limit);
                    match state.app().audit_list(&ctx, query.clone()).await {
                        Ok((p, _)) => p,
                        Err(_) => return None,
                    }
                }
            };
            if items.is_empty() {
                return None;
            }
            let mut buf = String::new();
            for e in &items {
                buf.push_str(&serde_json::to_string(e).unwrap_or_default());
                buf.push('\n');
            }
            Some((
                Ok::<_, std::io::Error>(bytes::Bytes::from(buf)),
                (state, ctx, query, None),
            ))
        },
    );
    let mut res = axum::response::Response::new(axum::body::Body::from_stream(stream));
    res.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/x-ndjson"),
    );
    Ok(res)
}
