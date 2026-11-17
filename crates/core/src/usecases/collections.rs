//! Collection management (SPEC §8.5, §10.4). Superuser only.
//!
//! Every change is planned and applied inside one write transaction against
//! the schema as stored in that transaction, so concurrent changes can't
//! interleave: DDL + `_collections` + `_schema_history` + audit commit
//! together or not at all.

use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use super::audit::entry;
use super::{App, RequestCtx};
use crate::domain::field::FieldKind;
use crate::domain::{
    Actor, Collection, DdlStep, Field, HistoryEntry, RuleOp, Schema, TrashEntry, TrashKind,
};
use crate::error::{FieldError, ValidationErrors};
use crate::ports::{StoreExt, WriteTx};
use crate::rules::compile_collection_rule;
use crate::time::format_timestamp;
use crate::{Error, Result};

/// Result of a schema change (or its dry run).
#[derive(Debug, Clone)]
pub struct Change {
    /// The collection after the change (`None` after a delete).
    pub collection: Option<Collection>,
    /// The SQL that was (or would be) executed.
    pub ddl: String,
    /// Warnings for the operator (e.g. a new required field with empty rows).
    pub warnings: Vec<String>,
}

fn storage_name(id: Uuid) -> String {
    format!("_trash_{}", id.simple())
}

/// Checks a collection against the schema it will live in: shape, relation
/// targets, and that **every** collection's rules still compile.
fn validate(c: &Collection, next: &Schema) -> Result<()> {
    let mut errs = c.validate_shape();
    for (i, f) in c.fields.iter().enumerate() {
        if let Some(target) = f.relation_target()
            && next.by_id(target).is_none()
        {
            errs.add(
                format!("fields.{i}.collection_id"),
                FieldError::new("not_found", "The target collection does not exist."),
            );
        }
    }
    if !errs.is_empty() {
        return Err(Error::Validation(errs));
    }
    for other in next.all() {
        for op in RuleOp::ALL {
            if let Err(e) = compile_collection_rule(next, other, op) {
                let prefix = if other.id == c.id {
                    format!("rules.{}: ", op.as_str())
                } else {
                    format!(
                        "this change breaks the {} rule of collection '{}': ",
                        op.as_str(),
                        other.name
                    )
                };
                return Err(Error::RuleCompile {
                    message: format!("{prefix}{}", e.message),
                    position: e.position,
                });
            }
        }
    }
    Ok(())
}

fn schema_in_tx(tx: &mut dyn WriteTx) -> Result<Schema> {
    Ok(Schema::new(tx.collections_list()?, 0))
}

fn find<'s>(schema: &'s Schema, id_or_name: &str) -> Result<&'s std::sync::Arc<Collection>> {
    schema.by_id_or_name(id_or_name).ok_or(Error::NotFound)
}

fn index_steps_create(c: &Collection) -> Vec<DdlStep> {
    c.indexes
        .iter()
        .map(|i| DdlStep::CreateIndex {
            table: c.name.clone(),
            name: c.index_sql_name(i),
            columns: i.fields.clone(),
            unique: i.unique,
        })
        .collect()
}

fn index_steps_drop(c: &Collection) -> Vec<DdlStep> {
    c.indexes
        .iter()
        .map(|i| DdlStep::DropIndex {
            name: c.index_sql_name(i),
        })
        .collect()
}

/// Whether a field may change from `old` to `new` in place, and how.
fn type_change(
    old: &Field,
    new: &Field,
    table: &str,
    column: &str,
) -> std::result::Result<Vec<DdlStep>, FieldError> {
    use FieldKind as K;
    let text_like = |k: &FieldKind| matches!(k, K::Text { .. } | K::Email { .. } | K::Url { .. });
    let cardinality = |steps: &mut Vec<DdlStep>| match (old.is_multi(), new.is_multi()) {
        (false, true) => steps.push(DdlStep::SingleToMulti {
            table: table.into(),
            column: column.into(),
        }),
        (true, false) => steps.push(DdlStep::MultiToSingle {
            table: table.into(),
            column: column.into(),
        }),
        _ => {}
    };
    let mut steps = Vec::new();
    match (&old.kind, &new.kind) {
        (a, b) if text_like(a) && text_like(b) => {}
        (K::Number { .. }, K::Number { .. })
        | (K::Bool, K::Bool)
        | (K::Date { .. }, K::Date { .. })
        | (K::Autodate { .. }, K::Autodate { .. })
        | (K::Json { .. }, K::Json { .. }) => {}
        (K::Select { .. }, K::Select { .. }) => cardinality(&mut steps),
        (
            K::Relation {
                collection_id: a, ..
            },
            K::Relation {
                collection_id: b, ..
            },
        ) => {
            if a != b {
                return Err(FieldError::new(
                    "incompatible_change",
                    "A relation cannot point to another collection; create a new field instead.",
                ));
            }
            cardinality(&mut steps);
        }
        _ => {
            return Err(FieldError::new(
                "incompatible_change",
                format!(
                    "Cannot change {} to {}; create a new field instead.",
                    old.type_name(),
                    new.type_name()
                ),
            ));
        }
    }
    Ok(steps)
}

/// Plans an update of `old` to `new`; also returns the fields moved to trash.
fn plan_update(old: &Collection, new: &Collection) -> Result<(Vec<DdlStep>, Vec<Field>)> {
    let mut errs = ValidationErrors::default();
    let mut steps = Vec::new();
    let renamed_table = old.name != new.name;

    // Indexes: drop changed/removed ones (all, if the table is renamed).
    let same_index =
        |i: &crate::domain::IndexDef| !renamed_table && new.indexes.iter().any(|n| n == i);
    for i in old.indexes.iter().filter(|i| !same_index(i)) {
        steps.push(DdlStep::DropIndex {
            name: old.index_sql_name(i),
        });
    }
    if renamed_table {
        steps.push(DdlStep::RenameTable {
            from: old.name.clone(),
            to: new.name.clone(),
        });
    }
    let table = new.name.clone();

    let new_by_id: HashMap<Uuid, &Field> = new.fields.iter().map(|f| (f.id, f)).collect();
    let old_ids: HashSet<Uuid> = old.fields.iter().map(|f| f.id).collect();

    // 1. Removed fields → trash (frees their names).
    let mut trashed = Vec::new();
    for f in old.fields.iter().filter(|f| !new_by_id.contains_key(&f.id)) {
        steps.push(DdlStep::RenameColumn {
            table: table.clone(),
            from: f.name.clone(),
            to: storage_name(f.id),
        });
        trashed.push(f.clone());
    }
    // 2. Renames in two phases, so swaps (a↔b) never collide.
    let renames: Vec<(&Field, &Field)> = old
        .fields
        .iter()
        .filter_map(|o| new_by_id.get(&o.id).map(|n| (o, *n)))
        .filter(|(o, n)| o.name != n.name)
        .collect();
    for (o, _) in &renames {
        steps.push(DdlStep::RenameColumn {
            table: table.clone(),
            from: o.name.clone(),
            to: format!("_tmp_{}", o.id.simple()),
        });
    }
    for (o, n) in &renames {
        steps.push(DdlStep::RenameColumn {
            table: table.clone(),
            from: format!("_tmp_{}", o.id.simple()),
            to: n.name.clone(),
        });
    }
    // 3. Type changes.
    for (i, n) in new.fields.iter().enumerate() {
        if let Some(o) = old.fields.iter().find(|o| o.id == n.id) {
            match type_change(o, n, &table, &n.name) {
                Ok(s) => steps.extend(s),
                Err(e) => errs.add(format!("fields.{i}.type"), e),
            }
        }
    }
    // 4. New fields.
    for f in new.fields.iter().filter(|f| !old_ids.contains(&f.id)) {
        steps.push(DdlStep::AddColumn {
            table: table.clone(),
            field: f.clone(),
        });
    }
    // 5. New/changed indexes.
    for i in new
        .indexes
        .iter()
        .filter(|i| renamed_table || !old.indexes.contains(i))
    {
        steps.push(DdlStep::CreateIndex {
            table: table.clone(),
            name: new.index_sql_name(i),
            columns: i.fields.clone(),
            unique: i.unique,
        });
    }
    if errs.is_empty() {
        Ok((steps, trashed))
    } else {
        Err(Error::Validation(errs))
    }
}

struct Applied {
    change: Change,
}

/// Runs `steps` (or renders them for a dry run) and records history + audit.
#[allow(clippy::too_many_arguments)]
fn commit_change(
    tx: &mut dyn WriteTx,
    steps: &[DdlStep],
    dry_run: bool,
    action: &str,
    before: Option<&Collection>,
    after: Option<&Collection>,
    actor: &Actor,
    now: OffsetDateTime,
) -> Result<String> {
    if dry_run {
        return tx.schema_render(steps);
    }
    let ddl = tx.schema_apply(steps)?;
    match after {
        Some(c) => tx.collection_put(c)?,
        None => {
            if let Some(b) = before {
                tx.collection_remove(&b.id.to_string())?;
            }
        }
    }
    let collection_id = after.or(before).map(|c| c.id.to_string());
    tx.schema_history_insert(&HistoryEntry {
        id: Uuid::now_v7().to_string(),
        collection_id: collection_id.clone(),
        action: action.to_owned(),
        before: before.map(Collection::to_json),
        after: after.map(Collection::to_json),
        ddl: ddl.clone(),
        actor: actor.describe(),
        ts: format_timestamp(now),
    })?;
    let mut e = entry(actor, &format!("collection.{action}"), now);
    e.collection_id = collection_id;
    e.diff = Some(
        json!({ "before": before.map(Collection::to_json), "after": after.map(Collection::to_json) }),
    );
    tx.audit_insert(&e)?;
    Ok(ddl)
}

fn required_warnings(old: Option<&Collection>, new: &Collection) -> Vec<String> {
    new.fields
        .iter()
        .filter(|f| f.required())
        .filter(|f| old.is_some_and(|o| o.field_by_id(f.id).is_none_or(|of| !of.required())))
        .map(|f| format!("'{}' is now required; existing records with an empty value keep it until they are next updated.", f.name))
        .collect()
}

impl App {
    /// All collections.
    pub fn collections_list(&self, ctx: &RequestCtx) -> Result<Vec<Collection>> {
        ctx.require_superuser()?;
        Ok(self
            .schema
            .get()
            .schema
            .all()
            .iter()
            .map(|c| (**c).clone())
            .collect())
    }

    /// One collection by id or name.
    pub fn collection_get(&self, ctx: &RequestCtx, id_or_name: &str) -> Result<Collection> {
        ctx.require_superuser()?;
        let compiled = self.schema.get();
        Ok((**find(&compiled.schema, id_or_name)?).clone())
    }

    /// Creates a collection.
    pub async fn collection_create(
        &self,
        ctx: &RequestCtx,
        input: Value,
        dry_run: bool,
    ) -> Result<Change> {
        ctx.require_superuser()?;
        let (actor, now) = (ctx.actor(), ctx.now);
        let applied = self
            .store
            .write(move |tx| {
                let schema = schema_in_tx(tx)?;
                let mut c = Collection::from_input(&input, None).map_err(Error::Validation)?;
                if schema.by_id(c.id).is_some() {
                    return Err(Error::Conflict(
                        "A collection with this id already exists.".into(),
                    ));
                }
                if schema.by_name(&c.name).is_some() {
                    return Err(Error::field(
                        "name",
                        FieldError::new("taken", "A collection with this name already exists."),
                    ));
                }
                c.created = format_timestamp(now);
                c.updated = c.created.clone();
                validate(&c, &schema.with(c.clone()))?;
                let mut steps = vec![DdlStep::CreateTable {
                    table: c.name.clone(),
                    fields: c.fields.clone(),
                }];
                steps.extend(index_steps_create(&c));
                let ddl =
                    commit_change(tx, &steps, dry_run, "create", None, Some(&c), &actor, now)?;
                Ok(Applied {
                    change: Change {
                        collection: Some(c),
                        ddl,
                        warnings: Vec::new(),
                    },
                })
            })
            .await?;
        if !dry_run {
            self.reload_schema().await?;
        }
        Ok(applied.change)
    }

    /// Updates a collection (schema change).
    pub async fn collection_update(
        &self,
        ctx: &RequestCtx,
        id_or_name: &str,
        input: Value,
        dry_run: bool,
    ) -> Result<Change> {
        ctx.require_superuser()?;
        let (actor, now, key) = (ctx.actor(), ctx.now, id_or_name.to_owned());
        let retention = self.settings().int("schema", "trash_retention_days", 30);
        let applied = self
            .store
            .write(move |tx| {
                let schema = schema_in_tx(tx)?;
                let old = (**find(&schema, &key)?).clone();
                let mut new = Collection::from_input(&input, Some(&old)).map_err(Error::Validation)?;
                new.updated = format_timestamp(now);
                if new.name != old.name && schema.by_name(&new.name).is_some() {
                    return Err(Error::field("name", FieldError::new("taken", "A collection with this name already exists.")));
                }
                validate(&new, &schema.with(new.clone()))?;
                let (steps, trashed) = plan_update(&old, &new)?;
                for s in &steps {
                    if let DdlStep::MultiToSingle { table: _, column } = s {
                        let n = tx.count_multi_valued(&old.name, old.fields.iter().find(|f| {
                            new.field(column).is_some_and(|nf| nf.id == f.id)
                        }).map_or(column.as_str(), |f| f.name.as_str()))?;
                        if n > 0 && dry_run {
                            return Err(Error::Conflict(format!(
                                "{n} record(s) have more than one value in '{column}'; reduce them to one first"
                            )));
                        }
                    }
                }
                let ddl = commit_change(tx, &steps, dry_run, "update", Some(&old), Some(&new), &actor, now)?;
                if !dry_run {
                    for f in &trashed {
                        tx.trash_insert(&TrashEntry {
                            id: Uuid::now_v7().to_string(),
                            kind: TrashKind::Field,
                            collection_id: old.id.to_string(),
                            original_name: f.name.clone(),
                            storage_name: storage_name(f.id),
                            definition: f.to_json(),
                            deleted_at: format_timestamp(now),
                            purge_after: format_timestamp(now + Duration::days(retention)),
                            actor: actor.describe(),
                        })?;
                    }
                }
                let warnings = required_warnings(Some(&old), &new);
                Ok(Applied { change: Change { collection: Some(new), ddl, warnings } })
            })
            .await?;
        if !dry_run {
            self.reload_schema().await?;
        }
        Ok(applied.change)
    }

    /// Soft-deletes a collection (renamed to `_trash_<id>`).
    pub async fn collection_delete(&self, ctx: &RequestCtx, id_or_name: &str) -> Result<()> {
        ctx.require_superuser()?;
        let (actor, now, key) = (ctx.actor(), ctx.now, id_or_name.to_owned());
        let retention = self.settings().int("schema", "trash_retention_days", 30);
        self.store
            .write(move |tx| {
                let schema = schema_in_tx(tx)?;
                let c = (**find(&schema, &key)?).clone();
                let referrers: Vec<String> = schema
                    .all()
                    .iter()
                    .filter(|o| {
                        o.id != c.id
                            && o.relation_fields()
                                .any(|f| f.relation_target() == Some(c.id))
                    })
                    .map(|o| o.name.clone())
                    .collect();
                if !referrers.is_empty() {
                    return Err(Error::Conflict(format!(
                        "Collections {} have relations to '{}'; remove those fields first.",
                        referrers.join(", "),
                        c.name
                    )));
                }
                let next = schema.without(c.id);
                for other in next.all() {
                    for op in RuleOp::ALL {
                        if compile_collection_rule(&next, other, op).is_err() {
                            return Err(Error::Conflict(format!(
                                "The {} rule of '{}' references '{}'; change it first.",
                                op.as_str(),
                                other.name,
                                c.name
                            )));
                        }
                    }
                }
                let mut steps = index_steps_drop(&c);
                steps.push(DdlStep::RenameTable {
                    from: c.name.clone(),
                    to: storage_name(c.id),
                });
                commit_change(tx, &steps, false, "delete", Some(&c), None, &actor, now)?;
                tx.trash_insert(&TrashEntry {
                    id: Uuid::now_v7().to_string(),
                    kind: TrashKind::Collection,
                    collection_id: c.id.to_string(),
                    original_name: c.name.clone(),
                    storage_name: storage_name(c.id),
                    definition: c.to_json(),
                    deleted_at: format_timestamp(now),
                    purge_after: format_timestamp(now + Duration::days(retention)),
                    actor: actor.describe(),
                })?;
                Ok(())
            })
            .await?;
        self.reload_schema().await
    }

    /// Schema history of a collection.
    pub async fn collection_history(
        &self,
        ctx: &RequestCtx,
        id_or_name: &str,
    ) -> Result<Vec<HistoryEntry>> {
        ctx.require_superuser()?;
        let id = find(&self.schema.get().schema, id_or_name)?.id.to_string();
        self.store
            .read(move |tx| tx.schema_history(Some(&id), 500))
            .await
    }

    /// The full schema as JSON (export).
    pub fn collections_export(&self, ctx: &RequestCtx) -> Result<Value> {
        ctx.require_superuser()?;
        Ok(Value::Array(
            self.schema
                .get()
                .schema
                .all()
                .iter()
                .map(|c| c.to_json())
                .collect(),
        ))
    }

    /// Applies a full schema (create or update each listed collection) as
    /// one plan in one transaction. Collections not listed are kept.
    pub async fn collections_import(
        &self,
        ctx: &RequestCtx,
        input: Value,
        dry_run: bool,
    ) -> Result<Vec<Change>> {
        ctx.require_superuser()?;
        let (actor, now) = (ctx.actor(), ctx.now);
        let Value::Array(items) = input else {
            return Err(Error::BadRequest(
                "Expected an array of collections.".into(),
            ));
        };
        let changes = self
            .store
            .write(move |tx| {
                let mut schema = schema_in_tx(tx)?;
                // Parse everything first, matching existing collections by id, then name.
                let mut planned = Vec::new();
                for (i, item) in items.iter().enumerate() {
                    let existing = item
                        .get("id")
                        .and_then(Value::as_str)
                        .and_then(|id| schema.by_id_or_name(id))
                        .or_else(|| {
                            item.get("name")
                                .and_then(Value::as_str)
                                .and_then(|n| schema.by_name(n))
                        })
                        .map(|c| (**c).clone());
                    let mut c = Collection::from_input(item, existing.as_ref()).map_err(|e| {
                        let mut prefixed = ValidationErrors::default();
                        for (k, v) in e.0 {
                            for fe in v {
                                prefixed.add(format!("{i}.{k}"), fe);
                            }
                        }
                        Error::Validation(prefixed)
                    })?;
                    c.updated = format_timestamp(now);
                    if existing.is_none() {
                        c.created = c.updated.clone();
                    }
                    schema = schema.with(c.clone());
                    planned.push((existing, c));
                }
                let mut out = Vec::new();
                for (existing, c) in planned {
                    validate(&c, &schema)?;
                    let change = match existing {
                        None => {
                            let mut steps = vec![DdlStep::CreateTable {
                                table: c.name.clone(),
                                fields: c.fields.clone(),
                            }];
                            steps.extend(index_steps_create(&c));
                            let ddl = commit_change(
                                tx,
                                &steps,
                                dry_run,
                                "import",
                                None,
                                Some(&c),
                                &actor,
                                now,
                            )?;
                            Change {
                                collection: Some(c),
                                ddl,
                                warnings: Vec::new(),
                            }
                        }
                        Some(old) => {
                            let (steps, trashed) = plan_update(&old, &c)?;
                            let ddl = commit_change(
                                tx,
                                &steps,
                                dry_run,
                                "import",
                                Some(&old),
                                Some(&c),
                                &actor,
                                now,
                            )?;
                            if !dry_run {
                                for f in &trashed {
                                    tx.trash_insert(&TrashEntry {
                                        id: Uuid::now_v7().to_string(),
                                        kind: TrashKind::Field,
                                        collection_id: old.id.to_string(),
                                        original_name: f.name.clone(),
                                        storage_name: storage_name(f.id),
                                        definition: f.to_json(),
                                        deleted_at: format_timestamp(now),
                                        purge_after: format_timestamp(now + Duration::days(30)),
                                        actor: actor.describe(),
                                    })?;
                                }
                            }
                            let warnings = required_warnings(Some(&old), &c);
                            Change {
                                collection: Some(c),
                                ddl,
                                warnings,
                            }
                        }
                    };
                    out.push(change);
                }
                Ok(out)
            })
            .await?;
        if !dry_run {
            self.reload_schema().await?;
        }
        Ok(changes)
    }

    /// Trash contents.
    pub async fn trash_list(&self, ctx: &RequestCtx) -> Result<Vec<TrashEntry>> {
        ctx.require_superuser()?;
        self.store.read(|tx| tx.trash_list()).await
    }

    /// Restores a trashed field or collection.
    pub async fn trash_restore(&self, ctx: &RequestCtx, id: &str) -> Result<Collection> {
        ctx.require_superuser()?;
        let (actor, now, id) = (ctx.actor(), ctx.now, id.to_owned());
        let restored = self
            .store
            .write(move |tx| {
                let schema = schema_in_tx(tx)?;
                let t = tx.trash_get(&id)?.ok_or(Error::NotFound)?;
                let (before, mut after, steps) = match t.kind {
                    TrashKind::Field => {
                        let c = schema.by_id_or_name(&t.collection_id).ok_or_else(|| {
                            Error::Conflict("The collection of this field no longer exists.".into())
                        })?;
                        if c.field(&t.original_name).is_some() {
                            return Err(Error::Conflict(format!(
                                "'{}' already has a field named '{}'; rename it first.",
                                c.name, t.original_name
                            )));
                        }
                        let mut errs = ValidationErrors::default();
                        let field = Field::from_json(&t.definition, "definition", &mut errs)
                            .ok_or_else(|| {
                                Error::internal("trashed field definition is invalid")
                            })?;
                        let mut after = (**c).clone();
                        after.fields.push(field);
                        let steps = vec![DdlStep::RenameColumn {
                            table: c.name.clone(),
                            from: t.storage_name.clone(),
                            to: t.original_name.clone(),
                        }];
                        (Some((**c).clone()), after, steps)
                    }
                    TrashKind::Collection => {
                        if schema.by_name(&t.original_name).is_some() {
                            return Err(Error::Conflict(format!(
                                "A collection named '{}' exists; rename it first.",
                                t.original_name
                            )));
                        }
                        let c = Collection::from_input(&t.definition, None).map_err(|_| {
                            Error::internal("trashed collection definition is invalid")
                        })?;
                        let mut steps = vec![DdlStep::RenameTable {
                            from: t.storage_name.clone(),
                            to: c.name.clone(),
                        }];
                        steps.extend(index_steps_create(&c));
                        (None, c, steps)
                    }
                };
                after.updated = format_timestamp(now);
                validate(&after, &schema.with(after.clone()))?;
                commit_change(
                    tx,
                    &steps,
                    false,
                    "restore",
                    before.as_ref(),
                    Some(&after),
                    &actor,
                    now,
                )?;
                tx.trash_remove(&t.id)?;
                Ok(after)
            })
            .await?;
        self.reload_schema().await?;
        Ok(restored)
    }

    /// Purges a trash entry now. `confirm` must equal its original name.
    pub async fn trash_purge(&self, ctx: &RequestCtx, id: &str, confirm: &str) -> Result<()> {
        ctx.require_superuser()?;
        let (actor, now, id, confirm) = (ctx.actor(), ctx.now, id.to_owned(), confirm.to_owned());
        self.store
            .write(move |tx| {
                let t = tx.trash_get(&id)?.ok_or(Error::NotFound)?;
                if t.original_name != confirm {
                    return Err(Error::BadRequest(format!(
                        "Confirm the permanent deletion with ?confirm={}",
                        t.original_name
                    )));
                }
                purge_in_tx(tx, &t, &actor, now)
            })
            .await
    }
}

/// Permanently drops a trashed column/table (one audited transaction).
pub(crate) fn purge_in_tx(
    tx: &mut dyn WriteTx,
    t: &TrashEntry,
    actor: &Actor,
    now: OffsetDateTime,
) -> Result<()> {
    let schema = schema_in_tx(tx)?;
    let step = match t.kind {
        TrashKind::Field => {
            let Some(c) = schema.by_id_or_name(&t.collection_id) else {
                // The whole collection is gone; its table carries the column.
                tx.trash_remove(&t.id)?;
                return Ok(());
            };
            DdlStep::DropColumn {
                table: c.name.clone(),
                column: t.storage_name.clone(),
            }
        }
        TrashKind::Collection => DdlStep::DropTable {
            table: t.storage_name.clone(),
        },
    };
    let ddl = tx.schema_apply(&[step])?;
    tx.trash_remove(&t.id)?;
    tx.schema_history_insert(&HistoryEntry {
        id: Uuid::now_v7().to_string(),
        collection_id: Some(t.collection_id.clone()),
        action: "purge".into(),
        before: Some(t.definition.clone()),
        after: None,
        ddl,
        actor: actor.describe(),
        ts: format_timestamp(now),
    })?;
    let mut e = entry(actor, "collection.purge", now);
    e.collection_id = Some(t.collection_id.clone());
    e.meta = Some(json!({ "kind": t.kind, "name": t.original_name }));
    tx.audit_insert(&e)
}
