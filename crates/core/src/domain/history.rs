//! Schema history and trash rows (SPEC §6.5, §8.5).

use serde::Serialize;
use serde_json::Value;

/// One `_schema_history` row.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HistoryEntry {
    /// UUIDv7.
    pub id: String,
    /// Affected collection.
    pub collection_id: Option<String>,
    /// `create`, `update`, `delete`, `restore`, `purge`, `import`.
    pub action: String,
    /// Definition before.
    pub before: Option<Value>,
    /// Definition after.
    pub after: Option<Value>,
    /// SQL that was executed.
    pub ddl: String,
    /// Actor description.
    pub actor: String,
    /// Timestamp.
    pub ts: String,
}

/// What is in the trash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrashKind {
    /// A removed field (renamed column).
    Field,
    /// A deleted collection (renamed table).
    Collection,
}

/// One `_trash` row.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TrashEntry {
    /// UUIDv7.
    pub id: String,
    /// Kind.
    pub kind: TrashKind,
    /// Owning collection (the collection itself for `collection`).
    pub collection_id: String,
    /// Name before deletion.
    pub original_name: String,
    /// Current column/table name (`_trash_<id>`).
    pub storage_name: String,
    /// Field JSON or full collection JSON.
    pub definition: Value,
    /// When it was deleted.
    pub deleted_at: String,
    /// When the purge job may drop it.
    pub purge_after: String,
    /// Who deleted it.
    pub actor: String,
}
