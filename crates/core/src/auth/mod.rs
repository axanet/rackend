//! Authentication (SPEC §11). M1 contains the superuser slice (ADR 0008).

pub mod broker;
pub mod keys;
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

/// One `_passkeys` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasskeyRow {
    /// Row id.
    pub id: String,
    /// Owner type.
    pub owner_type: SubjectType,
    /// Auth collection (users only).
    pub collection_id: Option<String>,
    /// User or superuser id.
    pub user_id: String,
    /// Credential id (base64url), unique.
    pub credential_id: String,
    /// The serialized `webauthn_rs::prelude::Passkey`.
    pub credential: String,
    /// Display name chosen by the owner.
    pub name: String,
    /// Creation time.
    pub created: String,
    /// Last successful use.
    pub last_used: Option<String>,
}
