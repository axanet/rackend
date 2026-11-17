//! Authentication (SPEC §11). M1 contains the superuser slice (ADR 0008).

pub mod password;
pub mod session;
pub mod token;

pub use session::{SessionRow, SubjectType};

/// A stored superuser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Superuser {
    /// UUIDv7.
    pub id: String,
    /// Lowercased email.
    pub email: String,
    /// argon2id PHC string.
    pub password_hash: String,
    /// Creation time.
    pub created: String,
    /// Last change.
    pub updated: String,
}
