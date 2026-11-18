//! Collections, trash and rule checking (SPEC §10.4, §16).

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Path, Query, State};
use rackend_core::domain::{Collection, RuleOp};
use rackend_core::rules::{Mode, compile};
use rackend_core::usecases::collections::Change;
use serde_json::{Value, json};

use super::{ApiResult, created, flag, no_content, ok, str_field};
use crate::AppState;
use crate::ctx::Ctx;
use crate::problem::{ApiError, ProblemCode};

fn change_json(c: Change, dry_run: bool) -> Value {
    let mut v = c.collection.map_or(Value::Null, |c| c.to_json());
    if let Value::Object(m) = &mut v {
        if dry_run {
            m.insert("dry_run".into(), json!(true));
            m.insert("ddl".into(), json!(c.ddl));
        }
        if !c.warnings.is_empty() {
            m.insert("warnings".into(), json!(c.warnings));
        }
    }
    v
}

/// `GET /api/collections?page=&per_page=`.
pub async fn list(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Query(q): Query<BTreeMap<String, String>>,
) -> ApiResult {
    let all = state.app().collections_list(&ctx)?;
    let per_page: usize = q
        .get("per_page")
        .and_then(|v| v.parse().ok())
        .unwrap_or(500)
        .clamp(1, 500);
    let page: usize = q
        .get("page")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1)
        .max(1);
    let total = all.len();
    let items: Vec<Value> = all
        .iter()
        .skip((page - 1) * per_page)
        .take(per_page)
        .map(Collection::to_json)
        .collect();
    Ok(ok(json!({
        "page": page, "per_page": per_page, "total_items": total,
        "total_pages": total.div_ceil(per_page), "items": items,
    })))
}

/// `POST /api/collections[?dry_run=true]`.
pub async fn create(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Json(body): Json<Value>,
) -> ApiResult {
    let dry = flag(&ctx.query, "dry_run");
    let c = state.app().collection_create(&ctx, body, dry).await?;
    Ok(if dry {
        ok(change_json(c, true))
    } else {
        created(change_json(c, false))
    })
}

/// `GET /api/collections/{idOrName}`.
pub async fn view(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Path(key): Path<String>,
) -> ApiResult {
    Ok(ok(state.app().collection_get(&ctx, &key)?.to_json()))
}

/// `PATCH /api/collections/{idOrName}[?dry_run=true]`.
pub async fn update(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Path(key): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    let dry = flag(&ctx.query, "dry_run");
    let c = state.app().collection_update(&ctx, &key, body, dry).await?;
    Ok(ok(change_json(c, dry)))
}

/// `DELETE /api/collections/{idOrName}` (soft delete).
pub async fn delete(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Path(key): Path<String>,
) -> ApiResult {
    state.app().collection_delete(&ctx, &key).await?;
    Ok(no_content())
}

/// `GET /api/collections/{idOrName}/history`.
pub async fn history(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Path(key): Path<String>,
) -> ApiResult {
    let items = state.app().collection_history(&ctx, &key).await?;
    Ok(ok(json!({ "items": items })))
}

/// `GET /api/collections/export`.
pub async fn export(State(state): State<AppState>, Ctx(ctx): Ctx) -> ApiResult {
    Ok(ok(state.app().collections_export(&ctx)?))
}

/// `POST /api/collections/import[?dry_run=true]` with an array of collections.
pub async fn import(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Json(body): Json<Value>,
) -> ApiResult {
    let dry = flag(&ctx.query, "dry_run");
    let changes = state.app().collections_import(&ctx, body, dry).await?;
    let items: Vec<Value> = changes.into_iter().map(|c| change_json(c, dry)).collect();
    Ok(ok(json!({ "dry_run": dry, "items": items })))
}

/// `GET /api/trash`.
pub async fn trash_list(State(state): State<AppState>, Ctx(ctx): Ctx) -> ApiResult {
    let items = state.app().trash_list(&ctx).await?;
    Ok(ok(json!({ "items": items })))
}

/// `POST /api/trash/{id}/restore`.
pub async fn trash_restore(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Path(id): Path<String>,
) -> ApiResult {
    Ok(ok(state.app().trash_restore(&ctx, &id).await?.to_json()))
}

/// `DELETE /api/trash/{id}?confirm=<original_name>` (purge now).
pub async fn trash_purge(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Path(id): Path<String>,
) -> ApiResult {
    let confirm = ctx.query.get("confirm").cloned().unwrap_or_default();
    state.app().trash_purge(&ctx, &id, &confirm).await?;
    Ok(no_content())
}

/// `POST /api/rules/check {collection, rule, operation?}`: compiles a rule
/// (or `operation: "filter"`) against a stored collection (by id or name) or
/// an unsaved definition, for live checking in the admin UI.
pub async fn rules_check(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Json(body): Json<Value>,
) -> ApiResult {
    ctx.require_superuser()?;
    let rule = str_field(&body, "rule")?;
    let compiled = state.app().schema.get();
    // Channel join/publish rules have no collection.
    if body.get("operation").and_then(Value::as_str) == Some("channel") {
        if !rule.trim().is_empty() {
            compile(&compiled.schema, None, rule, Mode::Channel)
                .map_err(rackend_core::Error::from)?;
        }
        return Ok(ok(json!({ "ok": true, "public": rule.trim().is_empty() })));
    }
    let (schema, collection) = match body.get("collection") {
        Some(Value::String(key)) => {
            let c = compiled
                .schema
                .by_id_or_name(key)
                .ok_or(rackend_core::Error::NotFound)?;
            (compiled.schema.clone(), (**c).clone())
        }
        Some(def @ Value::Object(_)) => {
            let existing = def
                .get("id")
                .and_then(Value::as_str)
                .and_then(|id| compiled.schema.by_id_or_name(id));
            let c = Collection::from_input(def, existing.map(|c| &**c))
                .map_err(rackend_core::Error::Validation)?;
            (compiled.schema.with(c.clone()), c)
        }
        _ => {
            return Err(ApiError::with_detail(
                ProblemCode::BadRequest,
                "Give 'collection' as a name, id or definition.",
            ));
        }
    };
    let mode = match body
        .get("operation")
        .and_then(Value::as_str)
        .unwrap_or("list")
    {
        "filter" => Mode::Filter,
        op => Mode::Rule(
            RuleOp::ALL
                .into_iter()
                .find(|o| o.as_str() == op)
                .ok_or_else(|| {
                    ApiError::with_detail(ProblemCode::BadRequest, "Unknown operation.")
                })?,
        ),
    };
    if rule.trim().is_empty() {
        return Ok(ok(json!({ "ok": true, "public": true })));
    }
    compile(&schema, Some(&collection), rule, mode).map_err(rackend_core::Error::from)?;
    Ok(ok(json!({ "ok": true, "public": false })))
}
