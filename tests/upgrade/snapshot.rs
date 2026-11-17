//! Shared by `generate.rs` and `upgrade_paths.rs`: what an upgrade must preserve.

use std::collections::BTreeMap;

use rusqlite::Connection;
use rusqlite::types::ValueRef;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

/// Bookkeeping tables that change by design during an upgrade. They are not
/// part of row counts or record hashes.
pub const BOOKKEEPING: &[&str] = &["_meta", "_migrations"];

/// The expected state stored next to a fixture as `<version>.expected.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Expected {
    /// Release the fixture was generated with.
    pub binary_version: String,
    /// Schema version of the fixture.
    pub schema_version: u32,
    /// Row count per table (bookkeeping tables excluded).
    pub row_counts: BTreeMap<String, u64>,
    /// SHA-256 of the canonical JSON of every row, per table.
    pub record_hashes: BTreeMap<String, String>,
    /// Tables whose hash a later migration intentionally changes, with the
    /// migration that documents it: `{ "table": "0007_name" }`.
    #[serde(default)]
    pub intended_transformations: BTreeMap<String, String>,
}

/// Tables in the database, excluding SQLite internals and bookkeeping.
pub fn data_tables(conn: &Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .expect("list tables");
    stmt.query_map([], |r| r.get::<_, String>(0))
        .expect("query tables")
        .flatten()
        .filter(|t| !BOOKKEEPING.contains(&t.as_str()))
        .collect()
}

fn to_json(v: ValueRef<'_>) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::from(i),
        ValueRef::Real(f) => Value::from(f),
        ValueRef::Text(t) => Value::from(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => Value::from(b.iter().map(|x| format!("{x:02x}")).collect::<String>()),
    }
}

/// Canonical JSON of all rows of `table` (keys sorted, rows ordered by all columns).
pub fn canonical_rows(conn: &Connection, table: &str) -> String {
    let cols: Vec<String> = {
        let mut stmt = conn
            .prepare(&format!(
                "SELECT name FROM pragma_table_info('{}') ORDER BY cid",
                table.replace('\'', "''")
            ))
            .expect("table info");
        stmt.query_map([], |r| r.get(0))
            .expect("columns")
            .flatten()
            .collect()
    };
    let order: Vec<String> = (1..=cols.len()).map(|i| i.to_string()).collect();
    let sql = format!(
        "SELECT * FROM \"{}\" ORDER BY {}",
        table.replace('"', "\"\""),
        order.join(", ")
    );
    let mut stmt = conn.prepare(&sql).expect("select rows");
    let mut rows = stmt.query([]).expect("rows");
    let mut out = Vec::new();
    while let Some(row) = rows.next().expect("row") {
        let mut obj = Map::new();
        for (i, c) in cols.iter().enumerate() {
            obj.insert(c.clone(), to_json(row.get_ref(i).expect("value")));
        }
        out.push(Value::Object(obj));
    }
    // serde_json::Map is a BTreeMap without the preserve_order feature: keys are sorted.
    Value::Array(out).to_string()
}

/// Row counts and record hashes of every data table.
pub fn snapshot(conn: &Connection) -> (BTreeMap<String, u64>, BTreeMap<String, String>) {
    let mut counts = BTreeMap::new();
    let mut hashes = BTreeMap::new();
    for t in data_tables(conn) {
        let n: i64 = conn
            .query_row(
                &format!("SELECT count(*) FROM \"{}\"", t.replace('"', "\"\"")),
                [],
                |r| r.get(0),
            )
            .expect("count");
        counts.insert(t.clone(), u64::try_from(n).unwrap_or(0));
        let digest = Sha256::digest(canonical_rows(conn, &t).as_bytes());
        hashes.insert(t, digest.iter().map(|b| format!("{b:02x}")).collect());
    }
    (counts, hashes)
}
