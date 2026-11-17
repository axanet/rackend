//! Per-request context: who is calling and the request values rules see.

use std::collections::BTreeMap;

use serde_json::Map;
use time::OffsetDateTime;

use crate::domain::{Actor, ActorType};
use crate::rules::eval::{AuthValues, RequestValues};
use crate::{Error, Result};

/// The authenticated subject of a request.
#[derive(Debug, Clone, PartialEq)]
pub enum Principal {
    /// No credentials.
    Anonymous,
    /// A superuser (bypasses all rules).
    Superuser {
        /// Superuser id.
        id: String,
        /// Email.
        email: String,
        /// Session id.
        session_id: String,
    },
    /// An auth-collection user (M2).
    User {
        /// Record id.
        id: String,
        /// Auth collection id.
        collection_id: String,
        /// Auth collection name.
        collection_name: String,
        /// Other fields for `@request.auth.*`.
        fields: Map<String, serde_json::Value>,
        /// Session id.
        session_id: String,
    },
}

/// Everything a use case needs to know about the current request.
#[derive(Debug, Clone)]
pub struct RequestCtx {
    /// Caller.
    pub principal: Principal,
    /// Client IP.
    pub ip: Option<String>,
    /// User agent.
    pub user_agent: Option<String>,
    /// Request id.
    pub request_id: Option<String>,
    /// HTTP method.
    pub method: String,
    /// Query parameters.
    pub query: BTreeMap<String, String>,
    /// Headers (lowercase, `-` → `_`).
    pub headers: BTreeMap<String, String>,
    /// Request time.
    pub now: OffsetDateTime,
}

impl RequestCtx {
    /// A context for internal callers (jobs, tests).
    pub fn system(principal: Principal, now: OffsetDateTime) -> Self {
        Self {
            principal,
            ip: None,
            user_agent: None,
            request_id: None,
            method: "GET".into(),
            query: BTreeMap::new(),
            headers: BTreeMap::new(),
            now,
        }
    }

    /// Whether the caller is a superuser.
    pub fn is_superuser(&self) -> bool {
        matches!(self.principal, Principal::Superuser { .. })
    }

    /// Fails with `Unauthorized`/`Forbidden` unless the caller is a superuser.
    pub fn require_superuser(&self) -> Result<()> {
        match self.principal {
            Principal::Superuser { .. } => Ok(()),
            Principal::Anonymous => Err(Error::Unauthorized),
            Principal::User { .. } => Err(Error::Forbidden("Only superusers can do this.".into())),
        }
    }

    /// The audit actor.
    pub fn actor(&self) -> Actor {
        let (kind, id, label) = match &self.principal {
            Principal::Anonymous => (ActorType::Anonymous, None, None),
            Principal::Superuser { id, email, .. } => {
                (ActorType::Superuser, Some(id.clone()), Some(email.clone()))
            }
            Principal::User { id, fields, .. } => (
                ActorType::User,
                Some(id.clone()),
                fields
                    .get("email")
                    .and_then(|e| e.as_str())
                    .map(str::to_owned),
            ),
        };
        Actor {
            kind,
            id,
            label,
            ip: self.ip.clone(),
            user_agent: self.user_agent.clone(),
            request_id: self.request_id.clone(),
        }
    }

    /// Request values for rules, without a body.
    pub fn request_values(&self) -> RequestValues {
        let mut rv = RequestValues::new(self.now);
        rv.method = self.method.clone();
        rv.query = self.query.clone();
        rv.headers = self.headers.clone();
        rv.auth = match &self.principal {
            Principal::User {
                id,
                collection_id,
                collection_name,
                fields,
                ..
            } => Some(AuthValues {
                id: id.clone(),
                collection_id: collection_id.clone(),
                collection_name: collection_name.clone(),
                fields: fields.clone(),
            }),
            _ => None,
        };
        rv
    }
}
