//! Abstract schema-change steps. Core plans them; the store renders and runs
//! them as SQL inside one write transaction (SPEC §8.5).

use super::field::Field;

/// One schema change.
#[derive(Debug, Clone, PartialEq)]
pub enum DdlStep {
    /// `CREATE TABLE` with system columns plus `fields`.
    CreateTable {
        /// Table name.
        table: String,
        /// Fields (including the system fields of auth collections).
        fields: Vec<Field>,
        /// Auth collection: adds the hidden `password_hash` column.
        auth: bool,
    },
    /// `ALTER TABLE … ADD COLUMN` (NULL default).
    AddColumn {
        /// Table.
        table: String,
        /// The new field.
        field: Field,
    },
    /// `ALTER TABLE … RENAME COLUMN`.
    RenameColumn {
        /// Table.
        table: String,
        /// Current name.
        from: String,
        /// New name.
        to: String,
    },
    /// Wrap single values into JSON arrays.
    SingleToMulti {
        /// Table.
        table: String,
        /// Column.
        column: String,
    },
    /// Unwrap JSON arrays; fails if any row has more than one value.
    MultiToSingle {
        /// Table.
        table: String,
        /// Column.
        column: String,
    },
    /// `ALTER TABLE … RENAME TO`.
    RenameTable {
        /// Current name.
        from: String,
        /// New name.
        to: String,
    },
    /// `ALTER TABLE … DROP COLUMN` (purge).
    DropColumn {
        /// Table.
        table: String,
        /// Column.
        column: String,
    },
    /// `DROP TABLE` (purge).
    DropTable {
        /// Table.
        table: String,
    },
    /// `CREATE [UNIQUE] INDEX`.
    CreateIndex {
        /// Table.
        table: String,
        /// Index name.
        name: String,
        /// Columns.
        columns: Vec<String>,
        /// Unique.
        unique: bool,
    },
    /// `DROP INDEX IF EXISTS`.
    DropIndex {
        /// Index name.
        name: String,
    },
}
