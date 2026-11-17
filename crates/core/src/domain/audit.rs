//! Audit trail entries (SPEC §14.1).

use serde::Serialize;
use serde_json::Value;

/// Who performed an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorType {
    /// Instance administrator.
    Superuser,
    /// Auth-collection user.
    User,
    /// OIDC client.
    Client,
    /// The server itself (jobs, cascades).
    System,
    /// Unauthenticated request.
    Anonymous,
}

impl ActorType {
    /// Stored form.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Superuser => "superuser",
            Self::User => "user",
            Self::Client => "client",
            Self::System => "system",
            Self::Anonymous => "anonymous",
        }
    }
}

/// The actor and request metadata attached to every audited change.
#[derive(Debug, Clone, PartialEq)]
pub struct Actor {
    /// Type.
    pub kind: ActorType,
    /// Id (user, superuser or client id).
    pub id: Option<String>,
    /// Email or name at the time.
    pub label: Option<String>,
    /// Client IP.
    pub ip: Option<String>,
    /// User agent.
    pub user_agent: Option<String>,
    /// Request id.
    pub request_id: Option<String>,
}

impl Actor {
    /// The system actor (jobs, cascades).
    pub fn system() -> Self {
        Self {
            kind: ActorType::System,
            id: None,
            label: None,
            ip: None,
            user_agent: None,
            request_id: None,
        }
    }

    /// Short label for schema history (`superuser:<email>`).
    pub fn describe(&self) -> String {
        match (&self.label, &self.id) {
            (Some(l), _) => format!("{}:{l}", self.kind.as_str()),
            (None, Some(id)) => format!("{}:{id}", self.kind.as_str()),
            _ => self.kind.as_str().to_owned(),
        }
    }
}

/// One `_audit` row.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AuditEntry {
    /// UUIDv7.
    pub id: String,
    /// Timestamp.
    pub ts: String,
    /// Actor type.
    pub actor_type: ActorType,
    /// Actor id.
    pub actor_id: Option<String>,
    /// Actor label.
    pub actor_label: Option<String>,
    /// Dot-namespaced action, e.g. `record.update`.
    pub action: String,
    /// Collection.
    pub collection_id: Option<String>,
    /// Record.
    pub record_id: Option<String>,
    /// Changed fields, before/after.
    pub diff: Option<Value>,
    /// Client IP.
    pub ip: Option<String>,
    /// User agent.
    pub user_agent: Option<String>,
    /// Request id.
    pub request_id: Option<String>,
    /// Extra data (e.g. `{"cause": "<audit id>"}` for cascades).
    pub meta: Option<Value>,
}

/// Max size of a `json` field value in a diff before truncation.
pub const JSON_DIFF_MAX: usize = 8 * 1024;
