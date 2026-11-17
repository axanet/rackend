//! Record CRUD (SPEC §9.4, §10.5). Writes run the domain change and its
//! audit entry in one transaction (SPEC §14.1).

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use serde_json::{Map, Value, json};
use time::OffsetDateTime;
use uuid::Uuid;

use super::audit::{entry, record_diff};
use super::compiled::CompiledSchema;
use super::{App, Principal, RequestCtx};
use crate::domain::field::{FieldKind, OnDelete};
use crate::domain::record::{
    normalize_auth_values, prepare_create, prepare_update, take_auth_input,
};
use crate::domain::{Actor, Collection, Field, Record, RuleOp};
use crate::error::{FieldError, ValidationErrors};
use crate::ports::{ReadTx, RecordQuery, Sort, StoreExt, WriteTx};
use crate::rules::eval::RequestValues;
use crate::rules::{Mode, RuleState, TypedRule, compile};
use crate::time::format_timestamp;
use crate::{Error, Result};

/// Max `per_page`.
pub const MAX_PER_PAGE: u32 = 500;
/// Max expand depth.
pub const MAX_EXPAND_DEPTH: usize = 3;
/// Max cascade depth on delete (SPEC §8.4).
pub const MAX_CASCADE_DEPTH: usize = 10;

/// List/view query parameters.
#[derive(Debug, Clone, Default)]
pub struct ListParams {
    /// 1-based page.
    pub page: Option<u32>,
    /// Page size.
    pub per_page: Option<u32>,
    /// `-created,title,@random`.
    pub sort: Option<String>,
    /// Rule-language filter.
    pub filter: Option<String>,
    /// `author,comments_via_post.author`.
    pub expand: Option<String>,
    /// `id,title,expand.author.name`.
    pub fields: Option<String>,
    /// Skip `COUNT(*)`.
    pub skip_total: bool,
}

/// One sub-request of a batch.
#[derive(Debug, Clone)]
pub struct BatchItem {
    /// `POST`, `PATCH` or `DELETE`.
    pub method: String,
    /// `/api/collections/{c}/records[/{id}]`.
    pub url: String,
    /// JSON body.
    pub body: Map<String, Value>,
}

/// Result of one batch sub-request.
#[derive(Debug, Clone)]
pub struct BatchResult {
    /// HTTP status of the sub-request.
    pub status: u16,
    /// Response body (record JSON), if any.
    pub body: Option<Value>,
}

/// A failed batch: the index of the failing request and its error.
#[derive(Debug)]
pub struct BatchFailure {
    /// Index of the sub-request that failed.
    pub index: usize,
    /// Why it failed.
    pub error: Error,
}

fn collection<'a>(compiled: &'a CompiledSchema, key: &str) -> Result<&'a Arc<Collection>> {
    compiled.schema.by_id_or_name(key).ok_or(Error::NotFound)
}

/// The rule to apply for `op`, or `None` for superusers / public rules.
/// Locked rules fail with `Forbidden`.
fn access<'a>(
    compiled: &'a CompiledSchema,
    c: &Collection,
    op: RuleOp,
    superuser: bool,
) -> Result<Option<&'a Arc<TypedRule>>> {
    if superuser {
        return Ok(None);
    }
    match compiled.rule(c, op) {
        RuleState::Locked => Err(Error::Forbidden(format!(
            "Only superusers can {} records of '{}'.",
            op.as_str(),
            c.name
        ))),
        RuleState::Public => Ok(None),
        RuleState::Expr(r) => Ok(Some(r)),
    }
}

/// Whether the view rule allows `id` (true for superusers/public).
fn viewable(
    tx: &mut dyn ReadTx,
    compiled: &CompiledSchema,
    c: &Collection,
    id: &str,
    req: &RequestValues,
    superuser: bool,
) -> Result<bool> {
    if superuser {
        return Ok(true);
    }
    match compiled.rule(c, RuleOp::View) {
        RuleState::Locked => Ok(false),
        RuleState::Public => Ok(true),
        RuleState::Expr(r) => tx.record_matches(c, id, r, req),
    }
}

fn parse_sort(c: &Collection, sort: Option<&str>) -> Result<Vec<Sort>> {
    let Some(sort) = sort.filter(|s| !s.trim().is_empty()) else {
        return Ok(vec![Sort::Desc("id".into())]);
    };
    sort.split(',')
        .map(str::trim)
        .map(|s| {
            if s == "@random" {
                return Ok(Sort::Random);
            }
            let (desc, name) = s
                .strip_prefix('-')
                .map_or((false, s.trim_start_matches('+')), |n| (true, n));
            let sortable = matches!(name, "id" | "created" | "updated")
                || c.field(name)
                    .is_some_and(|f| !f.is_multi() && !matches!(f.kind, FieldKind::Json { .. }));
            if !sortable {
                return Err(Error::BadRequest(format!("Cannot sort by '{name}'.")));
            }
            Ok(if desc {
                Sort::Desc(name.into())
            } else {
                Sort::Asc(name.into())
            })
        })
        .collect()
}

/// Expand paths as a tree: `a.b,a.c,d` → {a: {b, c}, d}.
#[derive(Debug, Default)]
struct ExpandTree(BTreeMap<String, ExpandTree>);

fn parse_expand(s: Option<&str>) -> Result<ExpandTree> {
    let mut root = ExpandTree::default();
    for path in s
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        let segs: Vec<&str> = path.split('.').collect();
        if segs.len() > MAX_EXPAND_DEPTH {
            return Err(Error::BadRequest(format!(
                "expand '{path}' is deeper than {MAX_EXPAND_DEPTH} levels"
            )));
        }
        let mut node = &mut root;
        for seg in segs {
            node = node.0.entry(seg.to_owned()).or_default();
        }
    }
    Ok(root)
}

/// Record JSON with an `expand` object filled per `tree`. Related records
/// are subject to their own collection's view rule; non-viewable ones are
/// omitted (the id stays in the field).
fn expand(
    tx: &mut dyn ReadTx,
    compiled: &CompiledSchema,
    c: &Collection,
    records: &[Record],
    tree: &ExpandTree,
    req: &RequestValues,
    superuser: bool,
) -> Result<Vec<Map<String, Value>>> {
    let viewer = req.auth.as_ref().map(|a| a.id.as_str());
    let mut out: Vec<Map<String, Value>> = records
        .iter()
        .map(|r| {
            let mut m = r.to_json(c);
            redact_email(c, &mut m, viewer, superuser);
            m
        })
        .collect();
    if tree.0.is_empty() || records.is_empty() {
        return Ok(out);
    }
    let mut expanded: Vec<Map<String, Value>> = vec![Map::new(); records.len()];
    for (seg, sub) in &tree.0 {
        // Forward relation.
        if let Some(f) = c.field(seg).filter(|f| f.relation_target().is_some()) {
            let target = compiled
                .schema
                .by_id(f.relation_target().unwrap_or_default())
                .ok_or(Error::NotFound)?
                .clone();
            let ids: Vec<String> = records.iter().flat_map(|r| ids_of(r.get(seg))).collect();
            let related = fetch_visible(tx, compiled, &target, Some(&ids), None, req, superuser)?;
            let related_json = expand(tx, compiled, &target, &related, sub, req, superuser)?;
            let by_id: HashMap<String, Map<String, Value>> = related
                .iter()
                .zip(related_json)
                .map(|(r, j)| (r.id.to_string(), j))
                .collect();
            for (i, r) in records.iter().enumerate() {
                let refs = ids_of(r.get(seg));
                if f.is_multi() {
                    let items: Vec<Value> = refs
                        .iter()
                        .filter_map(|id| by_id.get(id).cloned().map(Value::Object))
                        .collect();
                    expanded[i].insert(seg.clone(), Value::Array(items));
                } else if let Some(j) = refs.first().and_then(|id| by_id.get(id)) {
                    expanded[i].insert(seg.clone(), Value::Object(j.clone()));
                }
            }
            continue;
        }
        // Back-relation `<collection>_via_<field>`.
        let back = seg.split_once("_via_").and_then(|(cn, fname)| {
            let src = compiled.schema.by_name(cn)?.clone();
            let field = src
                .field(fname)
                .filter(|f| f.relation_target() == Some(c.id))?
                .clone();
            Some((src, field))
        });
        let Some((src, field)) = back else {
            return Err(Error::BadRequest(format!(
                "Cannot expand '{seg}': not a relation of '{}'.",
                c.name
            )));
        };
        let parent_ids: Vec<String> = records.iter().map(|r| r.id.to_string()).collect();
        let related = fetch_visible(
            tx,
            compiled,
            &src,
            None,
            Some((&field, &parent_ids)),
            req,
            superuser,
        )?;
        let related_json = expand(tx, compiled, &src, &related, sub, req, superuser)?;
        for (i, r) in records.iter().enumerate() {
            let me = r.id.to_string();
            let items: Vec<Value> = related
                .iter()
                .zip(&related_json)
                .filter(|(rr, _)| ids_of(rr.get(&field.name)).contains(&me))
                .map(|(_, j)| Value::Object(j.clone()))
                .collect();
            expanded[i].insert(seg.clone(), Value::Array(items));
        }
    }
    for (o, e) in out.iter_mut().zip(expanded) {
        o.insert("expand".into(), Value::Object(e));
    }
    Ok(out)
}

fn ids_of(v: &Value) -> Vec<String> {
    match v {
        Value::String(s) if !s.is_empty() => vec![s.clone()],
        Value::Array(a) => a
            .iter()
            .filter_map(|x| x.as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    }
}

fn fetch_visible(
    tx: &mut dyn ReadTx,
    compiled: &CompiledSchema,
    c: &Collection,
    ids: Option<&[String]>,
    referencing: Option<(&Field, &[String])>,
    req: &RequestValues,
    superuser: bool,
) -> Result<Vec<Record>> {
    let rule = match (superuser, compiled.rule(c, RuleOp::View)) {
        (true, _) | (false, RuleState::Public) => None,
        (false, RuleState::Locked) => return Ok(Vec::new()),
        (false, RuleState::Expr(r)) => Some(r.as_ref()),
    };
    let page = tx.records_query(&RecordQuery {
        collection: c,
        conditions: rule.into_iter().collect(),
        req,
        ids,
        referencing,
        sort: vec![Sort::Asc("id".into())],
        limit: Some(10_000),
        offset: 0,
        count_total: false,
    })?;
    Ok(page.items)
}

/// Applies a `fields` projection (`id,title,expand.author.name`, `*` per level).
pub fn project(record: &Map<String, Value>, fields: &str) -> Map<String, Value> {
    let paths: Vec<Vec<&str>> = fields
        .split(',')
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .map(|f| f.split('.').collect())
        .collect();
    if paths.is_empty() {
        return record.clone();
    }
    project_paths(record, &paths)
}

fn project_paths(obj: &Map<String, Value>, paths: &[Vec<&str>]) -> Map<String, Value> {
    let mut out = Map::new();
    let mut nested: BTreeMap<&str, Vec<Vec<&str>>> = BTreeMap::new();
    for p in paths {
        match p.as_slice() {
            ["*"] => {
                for (k, v) in obj {
                    if k != "expand" {
                        out.insert(k.clone(), v.clone());
                    }
                }
            }
            [k] => {
                if let Some(v) = obj.get(*k) {
                    out.insert((*k).to_owned(), v.clone());
                }
            }
            [k, rest @ ..] => nested.entry(*k).or_default().push(rest.to_vec()),
            [] => {}
        }
    }
    for (k, sub) in nested {
        match obj.get(k) {
            Some(Value::Object(o)) => {
                out.insert(k.to_owned(), Value::Object(project_nested(o, &sub)));
            }
            Some(Value::Array(items)) => {
                let projected = items
                    .iter()
                    .map(|i| {
                        i.as_object()
                            .map_or(i.clone(), |o| Value::Object(project_paths(o, &sub)))
                    })
                    .collect();
                out.insert(k.to_owned(), Value::Array(projected));
            }
            _ => {}
        }
    }
    out
}

/// Inside `expand`, the next segment names a relation, not a record field.
fn project_nested(o: &Map<String, Value>, sub: &[Vec<&str>]) -> Map<String, Value> {
    project_paths(o, sub)
}

fn body_values(req: &RequestValues, body: &Map<String, Value>) -> RequestValues {
    let mut r = req.clone();
    // Modifier keys (`tags+`) are exposed under the field name.
    let mut plain = Map::new();
    for (k, v) in body {
        let name = k.trim_start_matches('+').trim_end_matches(['+', '-']);
        plain.insert(name.to_owned(), v.clone());
    }
    r.body = Some(plain);
    r
}

/// Checks that every relation id in `values` exists.
fn check_relations(
    tx: &mut dyn WriteTx,
    compiled: &CompiledSchema,
    c: &Collection,
    values: &Map<String, Value>,
) -> Result<()> {
    let mut errs = ValidationErrors::default();
    for f in c.relation_fields() {
        let Some(v) = values.get(&f.name) else {
            continue;
        };
        let ids = ids_of(v);
        if ids.is_empty() {
            continue;
        }
        let target = compiled
            .schema
            .by_id(f.relation_target().unwrap_or_default())
            .ok_or(Error::NotFound)?;
        let missing = tx.records_missing(target, &ids)?;
        if !missing.is_empty() {
            errs.add(
                f.name.clone(),
                FieldError::new("not_found", "Related record(s) not found.").param("ids", missing),
            );
        }
    }
    errs.into_result()
}

fn if_match(expected: Option<&str>, current: &Record) -> Result<()> {
    match expected {
        Some(tag) if tag.trim() != "*" && tag.trim().trim_matches('"') != current.updated => {
            Err(Error::PreconditionFailed)
        }
        _ => Ok(()),
    }
}

/// Everything a write needs, owned so it can move into the write closure.
#[derive(Clone)]
struct WriteCtx {
    compiled: Arc<CompiledSchema>,
    req: RequestValues,
    actor: Actor,
    superuser: bool,
    now: OffsetDateTime,
}

/// Outcome of a write: the record JSON if the caller may view it.
pub type WriteOutcome = Option<Map<String, Value>>;

/// Hides `email` of auth records from everyone but the owner and superusers,
/// unless the collection makes emails visible (SPEC §8.6).
fn redact_email(c: &Collection, m: &mut Map<String, Value>, viewer: Option<&str>, superuser: bool) {
    if let Some(o) = c.auth()
        && !superuser
        && !o.email_visibility
        && viewer != m.get("id").and_then(Value::as_str)
    {
        m.remove("email");
    }
}

fn outcome(w: &WriteCtx, c: &Collection, r: &Record) -> Map<String, Value> {
    let mut m = r.to_json(c);
    redact_email(
        c,
        &mut m,
        w.req.auth.as_ref().map(|a| a.id.as_str()),
        w.superuser,
    );
    m
}

fn create_in_tx(
    tx: &mut dyn WriteTx,
    w: &WriteCtx,
    key: &str,
    body: &Map<String, Value>,
    password_hash: Option<&str>,
) -> Result<WriteOutcome> {
    let c = collection(&w.compiled, key)?.clone();
    let rule = access(&w.compiled, &c, RuleOp::Create, w.superuser)?;
    let now = format_timestamp(w.now);
    let mut plan = prepare_create(&c, body, &now).map_err(Error::Validation)?;
    normalize_auth_values(&c, &mut plan.values);
    check_relations(tx, &w.compiled, &c, &plan.values)?;
    let record = Record {
        id: plan.id.unwrap_or_else(Uuid::now_v7),
        created: now.clone(),
        updated: now,
        data: plan.values,
    };
    tx.record_insert(&c, &record)?;
    let id = record.id.to_string();
    if password_hash.is_some() {
        tx.auth_password_set(&c, &id, password_hash)?;
    }
    if let Some(rule) = rule
        && !tx.record_matches(&c, &id, rule, &body_values(&w.req, body))?
    {
        return Err(Error::Forbidden(
            "The create rule does not allow this record.".into(),
        ));
    }
    if c.options.audit_records {
        let mut e = entry(&w.actor, "record.create", w.now);
        e.collection_id = Some(c.id.to_string());
        e.record_id = Some(id.clone());
        e.diff = Some(record_diff(&c, None, Some(&record.data)));
        tx.audit_insert(&e)?;
    }
    Ok(viewable(tx, &w.compiled, &c, &id, &w.req, w.superuser)?.then(|| outcome(w, &c, &record)))
}

fn update_in_tx(
    tx: &mut dyn WriteTx,
    w: &WriteCtx,
    key: &str,
    id: &str,
    body: &Map<String, Value>,
    expected: Option<&str>,
    password_hash: Option<&str>,
) -> Result<WriteOutcome> {
    let c = collection(&w.compiled, key)?.clone();
    let current = tx.record_get(&c, id)?.ok_or(Error::NotFound)?;
    if !viewable(tx, &w.compiled, &c, id, &w.req, w.superuser)? {
        return Err(Error::NotFound);
    }
    if let Some(rule) = access(&w.compiled, &c, RuleOp::Update, w.superuser)?
        && !tx.record_matches(&c, id, rule, &body_values(&w.req, body))?
    {
        return Err(Error::Forbidden(
            "The update rule does not allow this change.".into(),
        ));
    }
    if_match(expected, &current)?;
    let now = format_timestamp(w.now);
    let mut plan = prepare_update(&c, &current, body, &now).map_err(Error::Validation)?;
    normalize_auth_values(&c, &mut plan.changes);
    check_relations(tx, &w.compiled, &c, &plan.changes)?;
    tx.record_update(&c, id, &now, &plan.changes)?;
    if password_hash.is_some() {
        tx.auth_password_set(&c, id, password_hash)?;
        let mut e = entry(&w.actor, "auth.password.change", w.now);
        e.collection_id = Some(c.id.to_string());
        e.record_id = Some(id.to_owned());
        tx.audit_insert(&e)?;
    }
    let mut after = current.clone();
    after.updated = now;
    for (k, v) in &plan.changes {
        after.data.insert(k.clone(), v.clone());
    }
    if c.options.audit_records {
        let mut e = entry(&w.actor, "record.update", w.now);
        e.collection_id = Some(c.id.to_string());
        e.record_id = Some(id.to_owned());
        e.diff = Some(record_diff(&c, Some(&current.data), Some(&after.data)));
        tx.audit_insert(&e)?;
    }
    Ok(viewable(tx, &w.compiled, &c, id, &w.req, w.superuser)?.then(|| outcome(w, &c, &after)))
}

fn delete_in_tx(
    tx: &mut dyn WriteTx,
    w: &WriteCtx,
    key: &str,
    id: &str,
    expected: Option<&str>,
) -> Result<()> {
    let c = collection(&w.compiled, key)?.clone();
    let current = tx.record_get(&c, id)?.ok_or(Error::NotFound)?;
    if !viewable(tx, &w.compiled, &c, id, &w.req, w.superuser)? {
        return Err(Error::NotFound);
    }
    if let Some(rule) = access(&w.compiled, &c, RuleOp::Delete, w.superuser)?
        && !tx.record_matches(&c, id, rule, &w.req)?
    {
        return Err(Error::Forbidden(
            "The delete rule does not allow this.".into(),
        ));
    }
    if_match(expected, &current)?;
    let audit_id = delete_record(tx, w, &c, &current, &w.actor, None)?;
    cascade(tx, w, &c, &current, audit_id.as_deref(), 1)
}

/// Deletes one record and audits it; returns the audit id.
fn delete_record(
    tx: &mut dyn WriteTx,
    w: &WriteCtx,
    c: &Collection,
    r: &Record,
    actor: &Actor,
    cause: Option<&str>,
) -> Result<Option<String>> {
    tx.record_delete(c, &r.id.to_string())?;
    if c.auth().is_some() {
        // A deleted user loses every session and passkey (SPEC §11.5).
        tx.sessions_revoke_for(
            &r.id.to_string(),
            None,
            "user_deleted",
            &format_timestamp(w.now),
        )?;
        tx.passkeys_delete_for(&r.id.to_string())?;
    }
    if !c.options.audit_records {
        return Ok(None);
    }
    let mut e = entry(actor, "record.delete", w.now);
    e.collection_id = Some(c.id.to_string());
    e.record_id = Some(r.id.to_string());
    e.diff = Some(record_diff(c, Some(&r.data), None));
    e.meta = cause.map(|c| json!({ "cause": c }));
    let id = e.id.clone();
    tx.audit_insert(&e)?;
    Ok(Some(id))
}

/// Applies `on_delete` of every relation pointing at `deleted` (SPEC §8.4):
/// restrict → conflict, set_null → reference removed, cascade → delete
/// (system actor, `meta.cause`), at most 10 levels deep.
fn cascade(
    tx: &mut dyn WriteTx,
    w: &WriteCtx,
    c: &Collection,
    deleted: &Record,
    cause: Option<&str>,
    depth: usize,
) -> Result<()> {
    let system = Actor {
        request_id: w.actor.request_id.clone(),
        ..Actor::system()
    };
    let id = deleted.id.to_string();
    for src in w.compiled.schema.all() {
        for f in src
            .relation_fields()
            .filter(|f| f.relation_target() == Some(c.id))
        {
            let FieldKind::Relation { on_delete, .. } = f.kind else {
                continue;
            };
            let ids = [id.clone()];
            let refs = tx.records_query(&RecordQuery {
                collection: src,
                conditions: Vec::new(),
                req: &w.req,
                ids: None,
                referencing: Some((f, &ids)),
                sort: vec![Sort::Asc("id".into())],
                limit: None,
                offset: 0,
                count_total: false,
            })?;
            for r in refs.items {
                if src.id == c.id && r.id == deleted.id {
                    continue;
                }
                match on_delete {
                    OnDelete::Restrict => {
                        return Err(Error::Conflict(format!(
                            "Record {} of '{}' still references this record ({}.{} is on_delete: restrict).",
                            r.id, src.name, src.name, f.name
                        )));
                    }
                    OnDelete::SetNull => {
                        let new_value = if f.is_multi() {
                            Value::Array(
                                ids_of(r.get(&f.name))
                                    .into_iter()
                                    .filter(|x| x != &id)
                                    .map(Value::String)
                                    .collect(),
                            )
                        } else {
                            Value::Null
                        };
                        let now = format_timestamp(w.now);
                        let mut changes = Map::new();
                        changes.insert(f.name.clone(), new_value);
                        tx.record_update(src, &r.id.to_string(), &now, &changes)?;
                        if src.options.audit_records {
                            let mut after = r.data.clone();
                            after.extend(changes);
                            let mut e = entry(&system, "record.update", w.now);
                            e.collection_id = Some(src.id.to_string());
                            e.record_id = Some(r.id.to_string());
                            e.diff = Some(record_diff(src, Some(&r.data), Some(&after)));
                            e.meta = cause.map(|c| json!({ "cause": c }));
                            tx.audit_insert(&e)?;
                        }
                    }
                    OnDelete::Cascade => {
                        if depth >= MAX_CASCADE_DEPTH {
                            return Err(Error::Conflict(format!(
                                "Cascading delete is deeper than {MAX_CASCADE_DEPTH} levels; delete the dependent records first."
                            )));
                        }
                        // Skip records already removed earlier in this cascade.
                        if tx.record_get(src, &r.id.to_string())?.is_none() {
                            continue;
                        }
                        let audit = delete_record(tx, w, src, &r, &system, cause)?;
                        cascade(tx, w, src, &r, audit.as_deref().or(cause), depth + 1)?;
                    }
                }
            }
        }
    }
    Ok(())
}

impl App {
    fn write_ctx(&self, ctx: &RequestCtx) -> WriteCtx {
        WriteCtx {
            compiled: self.schema.get(),
            req: ctx.request_values(),
            actor: ctx.actor(),
            superuser: ctx.is_superuser(),
            now: ctx.now,
        }
    }

    /// Lists records (SPEC §10.5).
    pub async fn records_list(&self, ctx: &RequestCtx, key: &str, p: ListParams) -> Result<Value> {
        let compiled = self.schema.get();
        let c = collection(&compiled, key)?.clone();
        let superuser = ctx.is_superuser();
        let rule = access(&compiled, &c, RuleOp::List, superuser)?.cloned();
        let filter = match p.filter.as_deref().filter(|f| !f.trim().is_empty()) {
            Some(f) => Some(Arc::new(compile(
                &compiled.schema,
                Some(&c),
                f,
                Mode::Filter,
            )?)),
            None => None,
        };
        let sort = parse_sort(&c, p.sort.as_deref())?;
        if !superuser {
            let hidden = |cid: Uuid, col: &str| {
                col == "email"
                    && compiled
                        .schema
                        .by_id(cid)
                        .and_then(|x| x.auth())
                        .is_some_and(|o| !o.email_visibility)
            };
            let filters_email = filter
                .as_ref()
                .is_some_and(|f| f.reads_column(c.id, &hidden));
            let sorts_email = sort
                .iter()
                .any(|s| matches!(s, Sort::Asc(x) | Sort::Desc(x) if hidden(c.id, x)));
            if filters_email || sorts_email {
                return Err(Error::BadRequest(
                    "Emails of this collection are not visible; you cannot filter or sort by them."
                        .into(),
                ));
            }
        }
        let tree = parse_expand(p.expand.as_deref())?;
        let per_page = p.per_page.unwrap_or(30).clamp(1, MAX_PER_PAGE);
        let page = p.page.unwrap_or(1).max(1);
        let req = ctx.request_values();
        let fields = p.fields.clone();
        let skip_total = p.skip_total;
        self.store
            .read(move |tx| {
                let mut conditions: Vec<&TypedRule> = Vec::new();
                if let Some(r) = &rule {
                    conditions.push(r);
                }
                if let Some(f) = &filter {
                    conditions.push(f);
                }
                let result = tx.records_query(&RecordQuery {
                    collection: &c,
                    conditions,
                    req: &req,
                    ids: None,
                    referencing: None,
                    sort,
                    limit: Some(per_page),
                    offset: u64::from(page - 1) * u64::from(per_page),
                    count_total: !skip_total,
                })?;
                let items = expand(tx, &compiled, &c, &result.items, &tree, &req, superuser)?;
                let items: Vec<Value> = items
                    .into_iter()
                    .map(|m| Value::Object(fields.as_deref().map_or(m.clone(), |f| project(&m, f))))
                    .collect();
                let (total_items, total_pages) = match result.total {
                    Some(t) => (
                        i64::try_from(t).unwrap_or(i64::MAX),
                        i64::try_from(t.div_ceil(u64::from(per_page))).unwrap_or(i64::MAX),
                    ),
                    None => (-1, -1),
                };
                Ok(json!({
                    "page": page, "per_page": per_page,
                    "total_items": total_items, "total_pages": total_pages,
                    "items": items,
                }))
            })
            .await
    }

    /// Views one record (SPEC §9.4: not viewable → 404).
    pub async fn record_view(
        &self,
        ctx: &RequestCtx,
        key: &str,
        id: &str,
        p: ListParams,
    ) -> Result<Map<String, Value>> {
        let compiled = self.schema.get();
        let c = collection(&compiled, key)?.clone();
        let superuser = ctx.is_superuser();
        let rule = match access(&compiled, &c, RuleOp::View, superuser) {
            Ok(r) => r.cloned(),
            Err(Error::Forbidden(_)) => return Err(Error::NotFound),
            Err(e) => return Err(e),
        };
        let tree = parse_expand(p.expand.as_deref())?;
        let req = ctx.request_values();
        let id = id.to_owned();
        self.store
            .read(move |tx| {
                let ids = [id];
                let page = tx.records_query(&RecordQuery {
                    collection: &c,
                    conditions: rule.as_deref().into_iter().collect(),
                    req: &req,
                    ids: Some(&ids),
                    referencing: None,
                    sort: Vec::new(),
                    limit: Some(1),
                    offset: 0,
                    count_total: false,
                })?;
                let mut items = expand(tx, &compiled, &c, &page.items, &tree, &req, superuser)?;
                let item = items.pop().ok_or(Error::NotFound)?;
                Ok(p.fields
                    .as_deref()
                    .map_or(item.clone(), |f| project(&item, f)))
            })
            .await
    }

    /// Creates a record.
    pub async fn record_create(
        &self,
        ctx: &RequestCtx,
        key: &str,
        body: Map<String, Value>,
    ) -> Result<WriteOutcome> {
        let mut body = body;
        let compiled = self.schema.get();
        let c = collection(&compiled, key)?.clone();
        let mut hash = None;
        if let Some(opts) = c.auth() {
            let input = take_auth_input(&c, &mut body).map_err(Error::Validation)?;
            if !ctx.is_superuser() {
                if !opts.signup_enabled {
                    return Err(Error::Forbidden(
                        "Sign-up is disabled for this collection.".into(),
                    ));
                }
                if body.contains_key("email_verified") {
                    return Err(Error::field(
                        "email_verified",
                        FieldError::new("read_only", "Only superusers can set this."),
                    ));
                }
                if input.password.is_none() {
                    return Err(Error::field(
                        "password",
                        FieldError::new("required", "Required."),
                    ));
                }
            }
            if let Some(pw) = input.password {
                if body
                    .get("email")
                    .and_then(Value::as_str)
                    .is_none_or(|e| e.trim().is_empty())
                {
                    return Err(Error::field(
                        "email",
                        FieldError::new("required", "Required for password sign-in."),
                    ));
                }
                hash = Some(
                    self.hash_new_password(&pw, opts.password_min_length)
                        .await?,
                );
            }
        }
        let w = self.write_ctx(ctx);
        let key = key.to_owned();
        self.store
            .write(move |tx| create_in_tx(tx, &w, &key, &body, hash.as_deref()))
            .await
    }

    /// Checks the password policy and hashes it off the async threads.
    pub(crate) async fn hash_new_password(&self, pw: &str, min_len: u32) -> Result<String> {
        crate::auth::password::check_policy(pw, min_len as usize)
            .map_err(|e| Error::field("password", e))?;
        let pw = pw.to_owned();
        self.blocking(move || crate::auth::password::hash(&pw))
            .await?
    }

    /// Updates a record (partial, with modifiers).
    pub async fn record_update(
        &self,
        ctx: &RequestCtx,
        key: &str,
        id: &str,
        body: Map<String, Value>,
        if_match: Option<String>,
    ) -> Result<WriteOutcome> {
        let mut body = body;
        let compiled = self.schema.get();
        let c = collection(&compiled, key)?.clone();
        let mut hash = None;
        if let Some(opts) = c.auth() {
            let input = take_auth_input(&c, &mut body).map_err(Error::Validation)?;
            if !ctx.is_superuser() {
                for k in ["email", "email_verified"] {
                    if body.contains_key(k) {
                        return Err(Error::field(
                            k,
                            FieldError::new("read_only", "Only superusers can change this."),
                        ));
                    }
                }
            }
            if let Some(pw) = input.password {
                if !ctx.is_superuser() {
                    let (c2, id2) = (c.clone(), id.to_owned());
                    let current = self
                        .store
                        .read(move |tx| tx.auth_password_hash(&c2, &id2))
                        .await?;
                    if current.is_some() {
                        let old = input.old_password.unwrap_or_default();
                        if !self.verify_password(current, &old).await? {
                            return Err(Error::field(
                                "old_password",
                                FieldError::new("invalid", "The current password is wrong."),
                            ));
                        }
                    }
                }
                hash = Some(
                    self.hash_new_password(&pw, opts.password_min_length)
                        .await?,
                );
            }
        }
        let w = self.write_ctx(ctx);
        let (key2, id2) = (key.to_owned(), id.to_owned());
        let changed_password = hash.is_some();
        let out = self
            .store
            .write(move |tx| {
                update_in_tx(
                    tx,
                    &w,
                    &key2,
                    &id2,
                    &body,
                    if_match.as_deref(),
                    hash.as_deref(),
                )
            })
            .await?;
        if changed_password {
            // Other sessions end; the caller's own session (if it is this user) stays.
            let keep = match &ctx.principal {
                Principal::User {
                    id: uid,
                    session_id,
                    ..
                } if uid == id => Some(session_id.clone()),
                _ => None,
            };
            self.revoke_all_for(id, keep.as_deref(), "password_change", ctx.now)
                .await?;
        }
        Ok(out)
    }

    /// Deletes a record, applying `on_delete` of incoming relations.
    pub async fn record_delete(
        &self,
        ctx: &RequestCtx,
        key: &str,
        id: &str,
        if_match: Option<String>,
    ) -> Result<()> {
        let w = self.write_ctx(ctx);
        let (key, id) = (key.to_owned(), id.to_owned());
        self.store
            .write(move |tx| delete_in_tx(tx, &w, &key, &id, if_match.as_deref()))
            .await?;
        // Sessions of deleted users (also via cascades) are revoked in the
        // transaction; drop the cache so their tokens fail immediately.
        self.sessions.invalidate_all();
        Ok(())
    }

    /// Runs sub-requests in one write transaction (SPEC §10.5). Any failure
    /// rolls everything back.
    pub async fn records_batch(
        &self,
        ctx: &RequestCtx,
        items: Vec<BatchItem>,
    ) -> Result<std::result::Result<Vec<BatchResult>, BatchFailure>> {
        let max =
            usize::try_from(self.settings().int("limits", "batch_max_requests", 50)).unwrap_or(50);
        if items.is_empty() {
            return Err(Error::BadRequest("The batch is empty.".into()));
        }
        if items.len() > max {
            return Err(Error::BadRequest(format!(
                "A batch may contain at most {max} requests."
            )));
        }
        let w = self.write_ctx(ctx);
        // The batch outcome travels out of the closure; the closure fails
        // (and rolls back) when a sub-request fails.
        let outcome = self
            .store
            .write(move |tx| {
                let mut results = Vec::with_capacity(items.len());
                for (index, item) in items.iter().enumerate() {
                    let r = run_batch_item(tx, &w, item);
                    match r {
                        Ok(res) => results.push(res),
                        Err(error) => {
                            return Err(Error::internal(BatchAbort(Some(BatchFailure {
                                index,
                                error,
                            }))));
                        }
                    }
                }
                Ok(results)
            })
            .await;
        match outcome {
            Ok(results) => Ok(Ok(results)),
            Err(Error::Internal(e)) => match e.downcast::<BatchAbort>() {
                Ok(mut abort) => match abort.0.take() {
                    Some(failure) => Ok(Err(failure)),
                    None => Err(Error::internal("batch aborted")),
                },
                Err(other) => Err(Error::Internal(other)),
            },
            Err(other) => Err(other),
        }
    }
}

/// Carries a batch failure out of the write closure.
#[derive(Debug)]
struct BatchAbort(Option<BatchFailure>);

impl std::fmt::Display for BatchAbort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("batch sub-request failed")
    }
}

impl std::error::Error for BatchAbort {}

fn run_batch_item(tx: &mut dyn WriteTx, w: &WriteCtx, item: &BatchItem) -> Result<BatchResult> {
    let path = item.url.split('?').next().unwrap_or_default();
    let segs: Vec<&str> = path.trim_matches('/').split('/').collect();
    let (key, id) = match segs.as_slice() {
        ["api", "collections", c, "records"] => (*c, None),
        ["api", "collections", c, "records", id] => (*c, Some(*id)),
        _ => {
            return Err(Error::BadRequest(format!(
                "Unsupported batch URL '{}'.",
                item.url
            )));
        }
    };
    // Auth records carry passwords, verification state and sessions; their
    // writes go through the record endpoints only (ADR 0011).
    if collection(&w.compiled, key)?.auth().is_some() {
        return Err(Error::BadRequest(
            "Batch requests cannot write auth collections; use the record endpoints.".into(),
        ));
    }
    match (item.method.to_ascii_uppercase().as_str(), id) {
        ("POST", None) => {
            let out = create_in_tx(tx, w, key, &item.body, None)?;
            Ok(BatchResult {
                status: if out.is_some() { 201 } else { 204 },
                body: out.map(Value::Object),
            })
        }
        ("PATCH", Some(id)) => {
            let out = update_in_tx(tx, w, key, id, &item.body, None, None)?;
            Ok(BatchResult {
                status: if out.is_some() { 200 } else { 204 },
                body: out.map(Value::Object),
            })
        }
        ("DELETE", Some(id)) => {
            delete_in_tx(tx, w, key, id, None)?;
            Ok(BatchResult {
                status: 204,
                body: None,
            })
        }
        (m, _) => Err(Error::BadRequest(format!(
            "Unsupported batch method {m} for '{}'.",
            item.url
        ))),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn projection() {
        let rec = json!({"id": "1", "title": "t", "x": 1, "expand": {"author": {"id": "a", "name": "n", "age": 3}, "tags": [{"id": "t1", "name": "x"}]}});
        let m = rec.as_object().unwrap();
        assert_eq!(
            Value::Object(project(m, "id,expand.author.name")),
            json!({"id": "1", "expand": {"author": {"name": "n"}}})
        );
        assert_eq!(
            Value::Object(project(m, "*")),
            json!({"id": "1", "title": "t", "x": 1})
        );
        assert_eq!(
            Value::Object(project(m, "expand.tags.name")),
            json!({"expand": {"tags": [{"name": "x"}]}})
        );
    }

    #[test]
    fn expand_tree_depth() {
        assert!(parse_expand(Some("a.b.c")).is_ok());
        assert!(parse_expand(Some("a.b.c.d")).is_err());
    }
}
