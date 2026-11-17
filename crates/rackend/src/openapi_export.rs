//! `rackend openapi export` (SPEC §10.12): the OpenAPI document from the
//! database, opened read only, so it also works while the server runs.

use std::path::Path;

use rackend_core::domain::{Collection, Schema};
use rackend_core::openapi::{DocInput, ProviderRef, Version};
use rackend_core::ports::SettingRow;
use rackend_core::settings::Settings;
use serde_json::Value;

/// Output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// JSON (pretty).
    Json,
    /// YAML.
    Yaml,
}

/// What to export.
#[derive(Debug, Clone)]
pub struct Options {
    /// OpenAPI version.
    pub version: Version,
    /// Superuser and admin endpoints too.
    pub include_admin: bool,
    /// Output format.
    pub format: Format,
    /// `RACKEND_PUBLIC_URL`.
    pub public_url: Option<String>,
    /// Binary version.
    pub app_version: String,
}

fn query<T>(
    conn: &rusqlite::Connection,
    sql: &str,
    f: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>, String> {
    let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
    let rows = stmt.query_map([], f).map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

/// The document for the database in `root`, as text.
pub fn export(root: &Path, opts: &Options) -> Result<String, String> {
    let db = root.join("data.db");
    if !db.is_file() {
        return Err(format!("no database at {}", db.display()));
    }
    let conn =
        rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| format!("cannot open {}: {e}", db.display()))?;
    let json = |s: String| serde_json::from_str::<Value>(&s).unwrap_or(Value::Null);
    let rows = query(
        &conn,
        "SELECT id, name, type, fields, indexes, rules, options, created, updated FROM _collections ORDER BY created, id",
        |r| {
            Ok(serde_json::json!({
                "id": r.get::<_, String>(0)?, "name": r.get::<_, String>(1)?, "type": r.get::<_, String>(2)?,
                "fields": json(r.get(3)?), "indexes": json(r.get(4)?), "rules": json(r.get(5)?),
                "options": json(r.get(6)?), "created": r.get::<_, String>(7)?, "updated": r.get::<_, String>(8)?,
            }))
        },
    )?;
    let collections: Vec<Collection> = rows
        .iter()
        .map(|v| {
            Collection::from_input(v, None)
                .map_err(|e| format!("stored collection {} is invalid: {e:?}", v["name"]))
        })
        .collect::<Result<_, _>>()?;
    let settings_rows = query(
        &conn,
        "SELECT key, value, is_secret, updated FROM _settings",
        |r| {
            Ok(SettingRow {
                key: r.get(0)?,
                value: r.get(1)?,
                is_secret: r.get(2)?,
                updated: r.get(3)?,
            })
        },
    )?;
    let mut settings = Settings::from_rows(&settings_rows);
    if let Some(u) = &opts.public_url {
        settings.set("app", "public_url", Value::String(u.clone()));
    }
    let providers = query(
        &conn,
        "SELECT collection_id, name FROM _auth_providers WHERE enabled = 1",
        |r| {
            Ok(ProviderRef {
                collection_id: r.get(0)?,
                name: r.get(1)?,
            })
        },
    )?;
    let schema = Schema::new(collections, 1);
    let doc = rackend_core::openapi::generate(&DocInput {
        schema: &schema,
        version: opts.version,
        include_admin: opts.include_admin,
        superuser_view: true,
        app_name: settings
            .string("app", "name")
            .unwrap_or_else(|| "rackend".into()),
        app_version: opts.app_version.clone(),
        public_url: settings.public_url(),
        providers,
    });
    Ok(match opts.format {
        Format::Json => rackend_core::openapi::to_json(&doc),
        Format::Yaml => rackend_core::openapi::to_yaml(&doc),
    })
}
