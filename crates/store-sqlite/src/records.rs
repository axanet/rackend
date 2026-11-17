//! Record storage: value conversion, queries with compiled rules, timeouts.

use std::cell::Cell;
use std::time::{Duration, Instant};

use rackend_core::domain::field::FieldKind;
use rackend_core::domain::value::number_json;
use rackend_core::domain::{Collection, Field, Record};
use rackend_core::ports::{RecordPage, RecordQuery, Sort};
use rackend_core::rules::TypedRule;
use rackend_core::rules::eval::RequestValues;
use rackend_core::{Error, Result};
use rusqlite::types::{Value as SqlValue, ValueRef};
use rusqlite::{Connection, ErrorCode, params_from_iter};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::rules_sql::{Fragment, compile, q};

/// JSON value of a field → SQLite value.
pub fn to_sql(f: &Field, v: &Value) -> SqlValue {
    match v {
        Value::Null => SqlValue::Null,
        Value::Bool(b) => SqlValue::Integer(i64::from(*b)),
        Value::Number(n) if matches!(f.kind, FieldKind::Number { .. }) => {
            SqlValue::Real(n.as_f64().unwrap_or(0.0))
        }
        Value::String(s) if !f.is_multi() && !matches!(f.kind, FieldKind::Json { .. }) => {
            SqlValue::Text(s.clone())
        }
        other => SqlValue::Text(other.to_string()),
    }
}

/// SQLite value of a field → JSON value.
pub fn from_sql(f: &Field, v: ValueRef<'_>) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) if matches!(f.kind, FieldKind::Bool) => Value::Bool(i != 0),
        ValueRef::Integer(i) => Value::from(i),
        ValueRef::Real(r) => number_json(r),
        ValueRef::Text(t) => {
            let s = String::from_utf8_lossy(t).into_owned();
            if f.is_multi() || matches!(f.kind, FieldKind::Json { .. }) {
                serde_json::from_str(&s).unwrap_or(Value::String(s))
            } else {
                Value::String(s)
            }
        }
        ValueRef::Blob(_) => Value::Null,
    }
}

fn select_columns(c: &Collection) -> String {
    let mut cols = vec![
        "t.\"id\"".to_owned(),
        "t.\"created\"".to_owned(),
        "t.\"updated\"".to_owned(),
    ];
    cols.extend(c.fields.iter().map(|f| format!("t.{}", q(&f.name))));
    cols.join(", ")
}

fn read_row(c: &Collection, row: &rusqlite::Row<'_>) -> rusqlite::Result<Record> {
    let id: String = row.get(0)?;
    let mut data = Map::new();
    for (i, f) in c.fields.iter().enumerate() {
        data.insert(f.name.clone(), from_sql(f, row.get_ref(i + 3)?));
    }
    Ok(Record {
        id: Uuid::try_parse(&id).unwrap_or_default(),
        created: row.get(1)?,
        updated: row.get(2)?,
        data,
    })
}

/// Maps SQLite errors: interrupts are query timeouts, unique violations conflicts.
pub fn map_err(e: rusqlite::Error) -> Error {
    if let rusqlite::Error::SqliteFailure(f, msg) = &e {
        match f.code {
            ErrorCode::OperationInterrupted => return Error::QueryTimeout,
            ErrorCode::ConstraintViolation
                if f.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
                    || f.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY =>
            {
                let detail = msg.as_deref().map_or_else(
                    || "A record with this value already exists.".to_owned(),
                    |m| {
                        format!(
                            "Unique constraint failed: {}",
                            m.trim_start_matches("UNIQUE constraint failed: ")
                        )
                    },
                );
                return Error::Conflict(detail);
            }
            _ => {}
        }
    }
    Error::internal(e)
}

thread_local! {
    /// Deadline of the rule query running on this thread. Each connection
    /// is used by one thread at a time, and the progress handler runs on the
    /// thread executing the statement.
    static DEADLINE: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// Installs the progress handler that enforces [`with_timeout`] deadlines.
/// Called once per connection when it is opened.
pub fn install_timeout_handler(conn: &mut Connection) -> rusqlite::Result<()> {
    conn.progress_handler(
        1000,
        Some(|| DEADLINE.with(|d| d.get().is_some_and(|deadline| Instant::now() > deadline))),
    )
}

/// Runs `f` so that SQLite aborts it after `timeout` (SPEC §9.5); the abort
/// surfaces as [`Error::QueryTimeout`].
pub fn with_timeout<T>(
    _conn: &Connection,
    timeout: Duration,
    f: impl FnOnce() -> rusqlite::Result<T>,
) -> Result<T> {
    let previous = DEADLINE.with(|d| d.replace(Some(Instant::now() + timeout)));
    let out = f();
    DEADLINE.with(|d| d.set(previous));
    out.map_err(map_err)
}

pub fn get(conn: &Connection, c: &Collection, id: &str) -> Result<Option<Record>> {
    let sql = format!(
        "SELECT {} FROM {} t WHERE t.\"id\" = ?1",
        select_columns(c),
        q(&c.name)
    );
    let mut stmt = conn.prepare_cached(&sql).map_err(map_err)?;
    let mut rows = stmt.query([id]).map_err(map_err)?;
    match rows.next().map_err(map_err)? {
        Some(row) => Ok(Some(read_row(c, row).map_err(map_err)?)),
        None => Ok(None),
    }
}

/// WHERE clause and its values for a query.
fn conditions(qr: &RecordQuery<'_>) -> (String, Vec<SqlValue>) {
    let mut parts = Vec::new();
    let mut values = Vec::new();
    for rule in &qr.conditions {
        let frag: Fragment = compile(rule, values.len());
        values.extend(frag.values(qr.req));
        parts.push(format!("({})", frag.sql));
    }
    if let Some(ids) = qr.ids {
        values.push(SqlValue::Text(
            serde_json::to_string(ids).unwrap_or_else(|_| "[]".into()),
        ));
        parts.push(format!(
            "t.\"id\" IN (SELECT value FROM json_each(?{}))",
            values.len()
        ));
    }
    if let Some((field, ids)) = qr.referencing {
        values.push(SqlValue::Text(
            serde_json::to_string(ids).unwrap_or_else(|_| "[]".into()),
        ));
        let n = values.len();
        if field.is_multi() {
            parts.push(format!(
                "EXISTS (SELECT 1 FROM json_each(t.{}) je WHERE je.value IN (SELECT value FROM json_each(?{n})))",
                q(&field.name)
            ));
        } else {
            parts.push(format!(
                "t.{} IN (SELECT value FROM json_each(?{n}))",
                q(&field.name)
            ));
        }
    }
    let clause = if parts.is_empty() {
        "1".to_owned()
    } else {
        parts.join(" AND ")
    };
    (clause, values)
}

pub fn query(conn: &Connection, qr: &RecordQuery<'_>, timeout: Duration) -> Result<RecordPage> {
    let c = qr.collection;
    let (clause, mut values) = conditions(qr);
    let table = q(&c.name);

    let total = if qr.count_total {
        let sql = format!("SELECT count(*) FROM {table} t WHERE {clause}");
        let n: i64 = with_timeout(conn, timeout, || {
            conn.prepare_cached(&sql)?
                .query_row(params_from_iter(values.iter()), |r| r.get(0))
        })?;
        Some(u64::try_from(n).unwrap_or(0))
    } else {
        None
    };

    let mut order: Vec<String> = qr
        .sort
        .iter()
        .map(|s| match s {
            Sort::Asc(col) => format!("t.{} ASC", q(col)),
            Sort::Desc(col) => format!("t.{} DESC", q(col)),
            Sort::Random => "random()".to_owned(),
        })
        .collect();
    if !qr
        .sort
        .iter()
        .any(|s| matches!(s, Sort::Asc(c) | Sort::Desc(c) if c == "id"))
    {
        order.push("t.\"id\" DESC".into());
    }
    let mut sql = format!(
        "SELECT {} FROM {table} t WHERE {clause} ORDER BY {}",
        select_columns(c),
        order.join(", ")
    );
    if let Some(limit) = qr.limit {
        values.push(SqlValue::Integer(i64::from(limit)));
        values.push(SqlValue::Integer(
            i64::try_from(qr.offset).unwrap_or(i64::MAX),
        ));
        sql.push_str(&format!(
            " LIMIT ?{} OFFSET ?{}",
            values.len() - 1,
            values.len()
        ));
    }
    let items = with_timeout(conn, timeout, || {
        let mut stmt = conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(params_from_iter(values.iter()), |row| read_row(c, row))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
    })?;
    Ok(RecordPage { items, total })
}

pub fn matches(
    conn: &Connection,
    c: &Collection,
    id: &str,
    rule: &TypedRule,
    req: &RequestValues,
    timeout: Duration,
) -> Result<bool> {
    let frag = compile(rule, 1);
    let mut values = vec![SqlValue::Text(id.to_owned())];
    values.extend(frag.values(req));
    let sql = format!(
        "SELECT EXISTS (SELECT 1 FROM {} t WHERE t.\"id\" = ?1 AND ({}))",
        q(&c.name),
        frag.sql
    );
    with_timeout(conn, timeout, || {
        conn.prepare_cached(&sql)?
            .query_row(params_from_iter(values.iter()), |r| r.get::<_, bool>(0))
    })
}

/// Like [`matches`], against a row given by its values: the base table is
/// replaced by a one-row `SELECT` with the same column names.
pub fn matches_values(
    conn: &Connection,
    c: &Collection,
    r: &Record,
    rule: &TypedRule,
    req: &RequestValues,
    timeout: Duration,
) -> Result<bool> {
    let mut values = vec![
        SqlValue::Text(r.id.to_string()),
        SqlValue::Text(r.created.clone()),
        SqlValue::Text(r.updated.clone()),
    ];
    let mut cols = vec![
        "?1 AS \"id\"".to_owned(),
        "?2 AS \"created\"".to_owned(),
        "?3 AS \"updated\"".to_owned(),
    ];
    for f in &c.fields {
        values.push(to_sql(f, r.get(&f.name)));
        cols.push(format!("?{} AS {}", values.len(), q(&f.name)));
    }
    let frag = compile(rule, values.len());
    values.extend(frag.values(req));
    let sql = format!(
        "SELECT EXISTS (SELECT 1 FROM (SELECT {}) t WHERE ({}))",
        cols.join(", "),
        frag.sql
    );
    with_timeout(conn, timeout, || {
        conn.prepare_cached(&sql)?
            .query_row(params_from_iter(values.iter()), |row| row.get::<_, bool>(0))
    })
}

pub fn missing(conn: &Connection, c: &Collection, ids: &[String]) -> Result<Vec<String>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let sql = format!(
        "SELECT value FROM json_each(?1) WHERE value NOT IN (SELECT \"id\" FROM {})",
        q(&c.name)
    );
    let json = serde_json::to_string(ids).unwrap_or_else(|_| "[]".into());
    let mut stmt = conn.prepare_cached(&sql).map_err(map_err)?;
    let rows = stmt
        .query_map([json], |r| r.get::<_, String>(0))
        .map_err(map_err)?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(map_err)
}

pub fn insert(conn: &Connection, c: &Collection, r: &Record) -> Result<()> {
    let mut cols = vec![
        "\"id\"".to_owned(),
        "\"created\"".to_owned(),
        "\"updated\"".to_owned(),
    ];
    let mut values = vec![
        SqlValue::Text(r.id.to_string()),
        SqlValue::Text(r.created.clone()),
        SqlValue::Text(r.updated.clone()),
    ];
    for f in &c.fields {
        cols.push(q(&f.name));
        values.push(to_sql(f, r.get(&f.name)));
    }
    let placeholders: Vec<String> = (1..=values.len()).map(|i| format!("?{i}")).collect();
    let sql = format!(
        "INSERT INTO {} ({}) VALUES ({})",
        q(&c.name),
        cols.join(", "),
        placeholders.join(", ")
    );
    conn.prepare_cached(&sql)
        .and_then(|mut s| s.execute(params_from_iter(values.iter())))
        .map(drop)
        .map_err(map_err)
}

pub fn update(
    conn: &Connection,
    c: &Collection,
    id: &str,
    updated: &str,
    changes: &Map<String, Value>,
) -> Result<()> {
    let mut sets = vec!["\"updated\" = ?1".to_owned()];
    let mut values = vec![SqlValue::Text(updated.to_owned())];
    for (name, v) in changes {
        let Some(f) = c.field(name) else {
            return Err(Error::internal(format!("unknown field {name}")));
        };
        values.push(to_sql(f, v));
        sets.push(format!("{} = ?{}", q(name), values.len()));
    }
    values.push(SqlValue::Text(id.to_owned()));
    let sql = format!(
        "UPDATE {} SET {} WHERE \"id\" = ?{}",
        q(&c.name),
        sets.join(", "),
        values.len()
    );
    conn.execute(&sql, params_from_iter(values.iter()))
        .map(drop)
        .map_err(map_err)
}

pub fn delete(conn: &Connection, c: &Collection, id: &str) -> Result<()> {
    conn.execute(
        &format!("DELETE FROM {} WHERE \"id\" = ?1", q(&c.name)),
        [id],
    )
    .map(drop)
    .map_err(map_err)
}
