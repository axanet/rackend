//! Collections, fields, records and their validation (SPEC §8).

pub mod audit;
pub mod collection;
pub mod ddl;
pub mod field;
pub mod files;
pub mod history;
pub mod names;
pub mod record;
pub mod schema;
pub mod value;

pub use audit::{Actor, ActorType, AuditEntry};
pub use collection::{
    AuthMethods, AuthOptions, Collection, CollectionOptions, CollectionType, IndexDef, RuleOp,
    Rules,
};
pub use ddl::DdlStep;
pub use field::{Field, FieldKind, OnDelete, ValueType};
pub use history::{HistoryEntry, TrashEntry, TrashKind};
pub use record::Record;
pub use schema::Schema;
