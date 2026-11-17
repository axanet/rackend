//! Server-side sessions (SPEC §11.5). All tokens map to one session row.

/// Who a session belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubjectType {
    /// An auth-collection user.
    User,
    /// A superuser.
    Superuser,
}

impl SubjectType {
    /// Stored form.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Superuser => "superuser",
        }
    }
}

/// One `_sessions` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    /// Session id (UUIDv7), embedded in its tokens.
    pub id: String,
    /// Family id (constant across refreshes).
    pub family_id: String,
    /// Subject type.
    pub subject_type: SubjectType,
    /// Auth collection (users only).
    pub collection_id: Option<String>,
    /// User or superuser id.
    pub user_id: String,
    /// OIDC client (provider tokens only).
    pub client_id: Option<String>,
    /// `password`, `passkey`, …
    pub auth_method: String,
    /// SHA-256 of the current access token.
    pub access_hash: String,
    /// Access token expiry.
    pub access_expires: String,
    /// SHA-256 of the current refresh token.
    pub refresh_hash: Option<String>,
    /// SHA-256 of the previous refresh token (reuse detection).
    pub prev_refresh_hash: Option<String>,
    /// Refresh token expiry.
    pub refresh_expires: Option<String>,
    /// Creation time.
    pub created: String,
    /// Last use (updated at most once per minute).
    pub last_seen: String,
    /// Revocation time.
    pub revoked_at: Option<String>,
    /// `logout`, `refresh_reuse`, `password_change`, …
    pub revoked_reason: Option<String>,
    /// Client IP at creation.
    pub ip: Option<String>,
    /// User agent at creation.
    pub user_agent: Option<String>,
    /// Scopes granted to an OIDC client session.
    pub scopes: Option<String>,
}
