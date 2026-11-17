//! The OIDC provider (SPEC §11.8): discovery, clients, the authorization
//! code flow with hosted login and consent, tokens, userinfo, revocation and
//! RP-initiated logout.
//!
//! Sessions: signing in on a hosted page creates a browser session (a normal
//! `_sessions` row, its refresh token in the `rk_sso` cookie). Each code
//! exchange creates a client session (`client_id` set, `family_id` = the
//! browser session), whose id is the `sid` of the issued tokens. Revoking
//! either invalidates the tokens at once for rackend's own API.

use std::collections::BTreeMap;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use super::audit::entry;
use super::auth::{CachedSession, Tokens, audit_auth};
use super::sessions::{RefreshScope, user_principal};
use super::{App, Principal, RequestCtx};
use crate::auth::jwt::{self, looks_like_jwt};
use crate::auth::oauth::{ClientKind, ClientRow, CodeRow, ConsentRow};
use crate::auth::token::{
    TokenKind, ct_eq, hash_token, parse_session_token, random_token, session_token,
};
use crate::auth::{SessionRow, SubjectType};
use crate::domain::{Collection, Record};
use crate::error::{FieldError, ValidationErrors};
use crate::ports::StoreExt as _;
use crate::time::format_timestamp;
use crate::{Error, Result};

/// ID token lifetime.
pub const ID_TOKEN_TTL: Duration = Duration::minutes(5);
/// Authorization code lifetime.
pub const CODE_TTL: Duration = Duration::seconds(60);
/// A sealed authorization request is valid this long (the user signs in).
pub const REQUEST_TTL: Duration = Duration::minutes(30);
/// Name of the browser session cookie.
pub const SSO_COOKIE: &str = "rk_sso";
/// Scopes rackend knows.
pub const SCOPES: &[&str] = &["openid", "profile", "email", "offline_access"];

/// An authorization request, sealed into the `ar` parameter while the user
/// signs in or consents.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthRequest {
    /// Client.
    pub client_id: String,
    /// Redirect URI (validated).
    pub redirect_uri: String,
    /// Requested scopes that rackend and the client allow.
    pub scopes: Vec<String>,
    /// `state`.
    pub state: Option<String>,
    /// `nonce`.
    pub nonce: Option<String>,
    /// PKCE S256 challenge.
    pub code_challenge: Option<String>,
    /// `prompt` values.
    pub prompt: Vec<String>,
    /// `max_age` in seconds.
    pub max_age: Option<i64>,
    /// `login_hint`.
    pub login_hint: Option<String>,
    /// When the request arrived (unix seconds).
    pub at: i64,
    /// The user signed in on the hosted page after `at` (satisfies
    /// `prompt=login` and `max_age`).
    #[serde(default)]
    pub logged_in: bool,
    /// The user allowed the request on the consent page.
    #[serde(default)]
    pub consented: bool,
}

impl AuthRequest {
    fn has_prompt(&self, p: &str) -> bool {
        self.prompt.iter().any(|x| x == p)
    }
}

/// What to do after `/oauth2/authorize` (or a hosted page).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Authorize {
    /// Back to the client (`code` or `error`).
    Redirect(String),
    /// Show the hosted login page for this sealed request.
    Login {
        /// Sealed request.
        ar: String,
        /// The client.
        client: Box<ClientInfo>,
    },
    /// Show the consent page.
    Consent {
        /// Sealed request.
        ar: String,
        /// The client.
        client: Box<ClientInfo>,
        /// Scopes to approve.
        scopes: Vec<String>,
    },
    /// No safe place to send the user (bad client or redirect URI).
    Error {
        /// OAuth error code.
        code: &'static str,
        /// Message for the page.
        message: String,
    },
}

/// What hosted pages show about a client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientInfo {
    /// Client id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Auth collection name.
    pub collection: String,
    /// Origin of the redirect URI (for the CSP `form-action`).
    pub redirect_origin: String,
}

/// A signed-in browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SsoSession {
    /// Session id.
    pub session_id: String,
    /// User.
    pub user_id: String,
    /// Auth collection id.
    pub collection_id: String,
    /// When the user signed in.
    pub auth_time: OffsetDateTime,
    /// How (`password`, `otp`, `passkey`, `provider`).
    pub auth_method: String,
}

/// An OAuth error response (RFC 6749 §5.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthError {
    /// `invalid_request`, `invalid_client`, `invalid_grant`, …
    pub error: &'static str,
    /// Explanation.
    pub description: String,
    /// HTTP status.
    pub status: u16,
}

impl OAuthError {
    fn new(error: &'static str, description: impl Into<String>) -> Self {
        let status = match error {
            "invalid_client" | "invalid_token" => 401,
            "server_error" => 500,
            "temporarily_unavailable" => 503,
            _ => 400,
        };
        Self {
            error,
            description: description.into(),
            status,
        }
    }

    fn internal(e: &Error) -> Self {
        tracing::error!(error = %e, "OIDC request failed");
        Self::new("server_error", "The server could not complete the request.")
    }
}

/// What a token request brings.
#[derive(Debug, Clone, Default)]
pub struct TokenRequest {
    /// Form fields.
    pub form: BTreeMap<String, String>,
    /// `Authorization: Basic` client id and secret.
    pub basic: Option<(String, String)>,
}

/// What a successful code exchange produced: the client session, its
/// collection and user, the code, and the refresh token (offline access).
type Exchanged = (
    Box<SessionRow>,
    Arc<Collection>,
    Record,
    CodeRow,
    Option<String>,
);

/// After `/oauth2/logout`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Logout {
    /// Redirect there (post-logout URI with `state`).
    Redirect(String),
    /// Ask the user to confirm (no `id_token_hint`).
    Confirm,
    /// Signed out; show a page.
    Done,
}

fn origin(uri: &str) -> String {
    let rest = uri.split_once("://").map_or("", |(_, r)| r);
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    format!("{}://{host}", uri.split("://").next().unwrap_or("https"))
}

fn with_params(base: &str, params: &[(&str, &str)]) -> String {
    let mut url = base.to_owned();
    let mut sep = if base.contains('?') { '&' } else { '?' };
    for (k, v) in params {
        url.push(sep);
        url.push_str(k);
        url.push('=');
        url.push_str(&urlencode(v));
        sep = '&';
    }
    url
}

/// Percent-encodes a query value.
pub fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn pkce_s256(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn unix(t: OffsetDateTime) -> i64 {
    t.unix_timestamp()
}

fn parse_ts(s: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok()
}

impl App {
    /// `app.public_url`, the issuer.
    pub fn issuer(&self) -> Result<String> {
        self.settings()
            .public_url()
            .ok_or(Error::NotConfigured("public_url"))
    }

    /// `/.well-known/openid-configuration`.
    pub fn oidc_discovery(&self) -> Result<Value> {
        let iss = self.issuer()?;
        Ok(json!({
            "issuer": iss,
            "authorization_endpoint": format!("{iss}/oauth2/authorize"),
            "token_endpoint": format!("{iss}/oauth2/token"),
            "userinfo_endpoint": format!("{iss}/oauth2/userinfo"),
            "jwks_uri": format!("{iss}/oauth2/jwks"),
            "revocation_endpoint": format!("{iss}/oauth2/revoke"),
            "end_session_endpoint": format!("{iss}/oauth2/logout"),
            "response_types_supported": ["code"],
            "response_modes_supported": ["query"],
            "grant_types_supported": ["authorization_code", "refresh_token"],
            "subject_types_supported": ["public"],
            "id_token_signing_alg_values_supported": ["RS256", "ES256"],
            "token_endpoint_auth_methods_supported": ["client_secret_basic", "client_secret_post", "none"],
            "revocation_endpoint_auth_methods_supported": ["client_secret_basic", "client_secret_post", "none"],
            "code_challenge_methods_supported": ["S256"],
            "scopes_supported": SCOPES,
            "claims_supported": ["sub", "iss", "aud", "exp", "iat", "auth_time", "nonce", "sid", "azp", "email", "email_verified", "name", "picture", "updated_at"],
            "prompt_values_supported": ["none", "login", "consent"],
            "claims_parameter_supported": false,
            "request_parameter_supported": false,
            "request_uri_parameter_supported": false,
            "authorization_response_iss_parameter_supported": true,
            "ui_locales_supported": ["en"],
        }))
    }

    /// `/oauth2/jwks`.
    pub fn oidc_jwks(&self) -> Value {
        self.keys.get().jwks()
    }

    // Clients --------------------------------------------------------------

    async fn client(&self, id: &str) -> Result<Option<ClientRow>> {
        let id = id.to_owned();
        Ok(self
            .store
            .read(|tx| tx.oauth_clients_list())
            .await?
            .into_iter()
            .find(|c| c.id == id))
    }

    fn client_json(&self, c: &ClientRow) -> Value {
        let compiled = self.schema.get();
        json!({
            "id": c.id, "name": c.name, "kind": c.kind.as_str(),
            "has_secret": c.secret_hash.is_some(),
            "redirect_uris": c.redirect_uris, "post_logout_redirect_uris": c.post_logout_redirect_uris,
            "scopes": c.scopes,
            "collection": compiled.schema.by_id_or_name(&c.collection_id).map(|x| x.name.clone()),
            "first_party": c.first_party, "dev": c.dev, "require_pkce": c.require_pkce,
            "id_token_alg": c.id_token_alg, "access_ttl_s": c.access_ttl_s, "refresh_ttl_s": c.refresh_ttl_s,
            "enabled": c.enabled, "created": c.created, "updated": c.updated,
        })
    }

    /// `GET /api/oauth-clients`.
    pub async fn oauth_clients(&self, ctx: &RequestCtx) -> Result<Value> {
        ctx.require_superuser()?;
        let rows = self.store.read(|tx| tx.oauth_clients_list()).await?;
        Ok(json!({ "items": rows.iter().map(|c| self.client_json(c)).collect::<Vec<_>>() }))
    }

    /// `POST /api/oauth-clients` or `PATCH /api/oauth-clients/{id}`. A new
    /// confidential client (or `rotate_secret: true`) returns its secret
    /// once, as `client_secret`.
    pub async fn oauth_client_save(
        &self,
        ctx: &RequestCtx,
        id: Option<&str>,
        body: Value,
    ) -> Result<Value> {
        ctx.require_superuser()?;
        let b = body
            .as_object()
            .cloned()
            .ok_or_else(|| Error::BadRequest("Send a JSON object.".into()))?;
        let existing = match id {
            Some(id) => Some(self.client(id).await?.ok_or(Error::NotFound)?),
            None => None,
        };
        let now = format_timestamp(ctx.now);
        let mut c = existing.clone().unwrap_or_else(|| ClientRow {
            id: Uuid::now_v7().to_string(),
            name: String::new(),
            kind: ClientKind::Public,
            secret_hash: None,
            redirect_uris: Vec::new(),
            post_logout_redirect_uris: Vec::new(),
            scopes: SCOPES.iter().map(|s| (*s).to_owned()).collect(),
            collection_id: String::new(),
            first_party: false,
            dev: false,
            id_token_alg: "RS256".into(),
            require_pkce: true,
            access_ttl_s: 900,
            refresh_ttl_s: 2_592_000,
            enabled: true,
            created: now.clone(),
            updated: now.clone(),
        });
        c.updated = now;
        let mut errs = ValidationErrors::default();
        let s = |k: &str| {
            b.get(k)
                .and_then(Value::as_str)
                .map(|v| v.trim().to_owned())
        };
        let list = |k: &str| -> Option<Vec<String>> {
            b.get(k).map(|v| {
                v.as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(|x| x.trim().to_owned())
                            .filter(|x| !x.is_empty())
                            .collect()
                    })
                    .unwrap_or_default()
            })
        };
        if let Some(n) = s("name") {
            c.name = n;
        }
        if c.name.is_empty() {
            errs.add("name", FieldError::new("required", "Required."));
        }
        if existing.is_none()
            && let Some(k) = s("kind")
        {
            match ClientKind::parse(&k) {
                Some(k) => c.kind = k,
                None => errs.add(
                    "kind",
                    FieldError::new("invalid_kind", "Must be public or confidential."),
                ),
            }
        }
        for (key, target) in [
            ("redirect_uris", &mut c.redirect_uris),
            (
                "post_logout_redirect_uris",
                &mut c.post_logout_redirect_uris,
            ),
        ] {
            if let Some(v) = list(key) {
                for u in &v {
                    let ok = (u.starts_with("https://") || u.starts_with("http://"))
                        && !u.contains('#')
                        && !u.contains('*');
                    if !ok && !u.contains("://") || u.contains('#') || u.contains('*') {
                        errs.add(
                            key,
                            FieldError::new(
                                "invalid_uri",
                                "Absolute URIs without fragments or wildcards.",
                            ),
                        );
                    }
                }
                *target = v;
            }
        }
        if let Some(v) = list("scopes") {
            if !v.iter().all(|x| SCOPES.contains(&x.as_str())) || !v.iter().any(|x| x == "openid") {
                errs.add(
                    "scopes",
                    FieldError::new(
                        "invalid_scope",
                        "Use openid, profile, email, offline_access; openid is required.",
                    ),
                );
            }
            c.scopes = v;
        }
        if let Some(col) = s("collection") {
            match self.auth_collection(&col) {
                Ok(col) => c.collection_id = col.id.to_string(),
                Err(_) => errs.add(
                    "collection",
                    FieldError::new("not_found", "No auth collection with this name."),
                ),
            }
        }
        if c.collection_id.is_empty() {
            errs.add("collection", FieldError::new("required", "Required."));
        }
        for (key, target) in [
            ("first_party", &mut c.first_party),
            ("dev", &mut c.dev),
            ("enabled", &mut c.enabled),
            ("require_pkce", &mut c.require_pkce),
        ] {
            if let Some(v) = b.get(key).and_then(Value::as_bool) {
                *target = v;
            }
        }
        if c.kind == ClientKind::Public {
            c.require_pkce = true;
        }
        if let Some(a) = s("id_token_alg") {
            if a == "RS256" || a == "ES256" {
                c.id_token_alg = a;
            } else {
                errs.add(
                    "id_token_alg",
                    FieldError::new("invalid", "RS256 or ES256."),
                );
            }
        }
        for (key, target, min) in [
            ("access_ttl_s", &mut c.access_ttl_s, 60),
            ("refresh_ttl_s", &mut c.refresh_ttl_s, 300),
        ] {
            if let Some(v) = b.get(key) {
                match v.as_i64() {
                    Some(n) if n >= min => *target = n,
                    _ => errs.add(
                        key,
                        FieldError::new("invalid", format!("At least {min} seconds.")),
                    ),
                }
            }
        }
        if !errs.is_empty() {
            return Err(Error::Validation(errs));
        }
        let mut secret = None;
        let rotate = b.get("rotate_secret").and_then(Value::as_bool) == Some(true);
        if c.kind == ClientKind::Confidential && (c.secret_hash.is_none() || rotate) {
            let s = format!("rk_cs_{}", random_token());
            c.secret_hash = Some(hash_token(&s));
            secret = Some(s);
        }
        let (row, actor, at) = (c.clone(), ctx.actor(), ctx.now);
        let action = if existing.is_some() {
            "oauth_client.update"
        } else {
            "oauth_client.create"
        };
        let rotated = secret.is_some() && existing.is_some();
        self.store
            .write(move |tx| {
                tx.oauth_client_put(&row)?;
                let mut e = entry(&actor, action, at);
                e.meta = Some(json!({ "id": row.id, "name": row.name, "secret_rotated": rotated }));
                tx.audit_insert(&e)
            })
            .await?;
        let mut out = self.client_json(&c);
        if let Some(s) = secret {
            out["client_secret"] = json!(s);
        }
        Ok(out)
    }

    /// `DELETE /api/oauth-clients/{id}`: also ends the client's sessions.
    pub async fn oauth_client_delete(&self, ctx: &RequestCtx, id: &str) -> Result<()> {
        ctx.require_superuser()?;
        let (id, actor, at) = (id.to_owned(), ctx.actor(), ctx.now);
        let revoked = self
            .store
            .write(move |tx| {
                if !tx.oauth_client_delete(&id)? {
                    return Err(Error::NotFound);
                }
                let ids =
                    tx.sessions_revoke_client(&id, "client_deleted", &format_timestamp(at))?;
                let mut e = entry(&actor, "oauth_client.delete", at);
                e.meta = Some(json!({ "id": id }));
                tx.audit_insert(&e)?;
                Ok(ids)
            })
            .await?;
        for s in revoked {
            self.sessions.invalidate(&s);
        }
        Ok(())
    }

    /// Creates the `default` client once (public, first party, bound to
    /// `users`); deleting it later is respected.
    pub async fn ensure_default_client(&self) -> Result<()> {
        let Some(users) = self.schema.get().schema.by_name("users").cloned() else {
            return Ok(());
        };
        let (now, dev) = (format_timestamp(self.clock.now()), self.dev_mode);
        self.store
            .write(move |tx| {
                if tx.meta_get("oidc_default_client")?.is_some() {
                    return Ok(());
                }
                tx.oauth_client_put(&ClientRow {
                    id: "default".into(),
                    name: "Default app".into(),
                    kind: ClientKind::Public,
                    secret_hash: None,
                    redirect_uris: Vec::new(),
                    post_logout_redirect_uris: Vec::new(),
                    scopes: SCOPES.iter().map(|s| (*s).to_owned()).collect(),
                    collection_id: users.id.to_string(),
                    first_party: true,
                    dev,
                    id_token_alg: "RS256".into(),
                    require_pkce: true,
                    access_ttl_s: 900,
                    refresh_ttl_s: 2_592_000,
                    enabled: true,
                    created: now.clone(),
                    updated: now.clone(),
                })?;
                tx.meta_set("oidc_default_client", &now)
            })
            .await
    }

    // Authorization --------------------------------------------------------

    /// Seals a request into the `ar` parameter (encrypted, expires).
    pub fn seal(&self, req: &AuthRequest) -> String {
        let s = serde_json::to_string(req).unwrap_or_default();
        URL_SAFE_NO_PAD.encode(self.secrets.encrypt_str(&s))
    }

    /// Opens a sealed request, if it is intact and recent.
    pub fn unseal(&self, ar: &str, now: OffsetDateTime) -> Option<AuthRequest> {
        let enc = String::from_utf8(URL_SAFE_NO_PAD.decode(ar).ok()?).ok()?;
        let req: AuthRequest = serde_json::from_str(&self.secrets.decrypt_str(&enc).ok()?).ok()?;
        (unix(now) - req.at <= REQUEST_TTL.whole_seconds()).then_some(req)
    }

    fn client_info(&self, c: &ClientRow, redirect_uri: &str) -> ClientInfo {
        ClientInfo {
            id: c.id.clone(),
            name: c.name.clone(),
            collection: self
                .schema
                .get()
                .schema
                .by_id_or_name(&c.collection_id)
                .map(|x| x.name.clone())
                .unwrap_or_default(),
            redirect_origin: origin(redirect_uri),
        }
    }

    /// The client a sealed request belongs to, for hosted pages.
    pub async fn request_client(&self, req: &AuthRequest) -> Option<ClientInfo> {
        let c = self.client(&req.client_id).await.ok()??;
        Some(self.client_info(&c, &req.redirect_uri))
    }

    /// Validates `/oauth2/authorize` parameters into a request.
    pub async fn authorize_request(
        &self,
        q: &BTreeMap<String, String>,
        now: OffsetDateTime,
    ) -> std::result::Result<AuthRequest, Authorize> {
        let get = |k: &str| q.get(k).filter(|v| !v.is_empty()).cloned();
        let fail = |code: &'static str, message: &str| Authorize::Error {
            code,
            message: message.to_owned(),
        };
        let client_id =
            get("client_id").ok_or_else(|| fail("invalid_request", "client_id is missing."))?;
        let client = self
            .client(&client_id)
            .await
            .map_err(|_| fail("server_error", "Try again later."))?
            .filter(|c| c.enabled)
            .ok_or_else(|| fail("invalid_client", "This app is not registered here."))?;
        let redirect_uri = get("redirect_uri")
            .ok_or_else(|| fail("invalid_request", "redirect_uri is missing."))?;
        if !client.redirect_allowed(&redirect_uri) {
            return Err(fail(
                "invalid_request",
                "This redirect URI is not registered for the app.",
            ));
        }
        let state = get("state");
        let back = |error: &str, desc: &str| {
            let mut p = vec![("error", error), ("error_description", desc)];
            if let Some(s) = &state {
                p.push(("state", s.as_str()));
            }
            Authorize::Redirect(with_params(&redirect_uri, &p))
        };
        for unsupported in ["request", "request_uri"] {
            if q.contains_key(unsupported) {
                let e = if unsupported == "request" {
                    "request_not_supported"
                } else {
                    "request_uri_not_supported"
                };
                return Err(back(e, "Request objects are not supported."));
            }
        }
        if get("response_type").as_deref() != Some("code") {
            return Err(back(
                "unsupported_response_type",
                "Only response_type=code is supported.",
            ));
        }
        if get("response_mode").is_some_and(|m| m != "query") {
            return Err(back(
                "invalid_request",
                "Only response_mode=query is supported.",
            ));
        }
        let requested: Vec<String> = get("scope")
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        if !requested.iter().any(|s| s == "openid") {
            return Err(back("invalid_scope", "The openid scope is required."));
        }
        let scopes: Vec<String> = requested
            .into_iter()
            .filter(|s| SCOPES.contains(&s.as_str()) && client.scopes.contains(s))
            .fold(Vec::new(), |mut acc, s| {
                if !acc.contains(&s) {
                    acc.push(s);
                }
                acc
            });
        let code_challenge = get("code_challenge");
        match (&code_challenge, get("code_challenge_method").as_deref()) {
            (Some(_), Some("S256")) => {}
            (Some(_), _) => {
                return Err(back(
                    "invalid_request",
                    "code_challenge_method must be S256.",
                ));
            }
            (None, _) if client.require_pkce => {
                return Err(back(
                    "invalid_request",
                    "PKCE (code_challenge with S256) is required.",
                ));
            }
            (None, _) => {}
        }
        let prompt: Vec<String> = get("prompt")
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        if prompt.iter().any(|p| p == "none") && prompt.len() > 1 {
            return Err(back("invalid_request", "prompt=none cannot be combined."));
        }
        if prompt.iter().any(|p| p == "select_account") {
            // One account per browser: treat as a normal request.
        }
        let max_age =
            match get("max_age") {
                Some(v) => Some(v.parse::<i64>().ok().filter(|n| *n >= 0).ok_or_else(|| {
                    back("invalid_request", "max_age must be a number of seconds.")
                })?),
                None => None,
            };
        Ok(AuthRequest {
            client_id,
            redirect_uri,
            scopes,
            state,
            nonce: get("nonce"),
            code_challenge,
            prompt,
            max_age,
            login_hint: get("login_hint"),
            at: unix(now),
            logged_in: false,
            consented: false,
        })
    }

    /// Continues a request: login, consent, then a code for the client.
    pub async fn authorize(
        &self,
        ctx: &RequestCtx,
        req: AuthRequest,
        sso: Option<SsoSession>,
    ) -> Authorize {
        let Ok(Some(client)) = self.client(&req.client_id).await else {
            return Authorize::Error {
                code: "invalid_client",
                message: "This app is not registered here.".into(),
            };
        };
        if !client.enabled || !client.redirect_allowed(&req.redirect_uri) {
            return Authorize::Error {
                code: "invalid_request",
                message: "This app cannot sign in here.".into(),
            };
        }
        let iss = match self.issuer() {
            Ok(i) => i,
            Err(_) => {
                return Authorize::Error {
                    code: "server_error",
                    message: "The public URL is not configured.".into(),
                };
            }
        };
        let back = |error: &str, desc: &str| {
            let mut p = vec![("error", error), ("error_description", desc)];
            if let Some(s) = &req.state {
                p.push(("state", s.as_str()));
            }
            p.push(("iss", iss.as_str()));
            Authorize::Redirect(with_params(&req.redirect_uri, &p))
        };
        let sso = sso.filter(|s| s.collection_id == client.collection_id);
        let fresh = |s: &SsoSession| -> bool {
            // Signed in on the hosted page for this very request.
            if req.logged_in {
                return true;
            }
            if req.has_prompt("login") {
                return false;
            }
            // `max_age=0` always asks again (OIDC Core §3.1.2.1).
            match req.max_age {
                Some(m) => m > 0 && unix(ctx.now) - unix(s.auth_time) <= m,
                None => true,
            }
        };
        let Some(sso) = sso.filter(|s| fresh(s)) else {
            if req.has_prompt("none") {
                return back("login_required", "The user is not signed in.");
            }
            let info = self.client_info(&client, &req.redirect_uri);
            return Authorize::Login {
                ar: self.seal(&req),
                client: Box::new(info),
            };
        };
        if !client.first_party && !req.consented {
            let (cid, uid, clid) = (
                client.collection_id.clone(),
                sso.user_id.clone(),
                client.id.clone(),
            );
            let consent = self
                .store
                .read(move |tx| tx.oauth_consent_get(&cid, &uid, &clid))
                .await
                .ok()
                .flatten();
            let covered = consent.is_some_and(|c| {
                let granted: Vec<&str> = c.scopes.split_whitespace().collect();
                req.scopes.iter().all(|s| granted.contains(&s.as_str()))
            });
            if !covered || req.has_prompt("consent") {
                if req.has_prompt("none") {
                    return back("consent_required", "The user has not approved this app.");
                }
                let info = self.client_info(&client, &req.redirect_uri);
                let scopes = req.scopes.clone();
                return Authorize::Consent {
                    ar: self.seal(&req),
                    client: Box::new(info),
                    scopes,
                };
            }
        }
        let code = random_token();
        let row = CodeRow {
            hash: hash_token(&code),
            client_id: client.id.clone(),
            session_id: sso.session_id.clone(),
            redirect_uri: req.redirect_uri.clone(),
            scopes: req.scopes.join(" "),
            code_challenge: req.code_challenge.clone().unwrap_or_default(),
            nonce: req.nonce.clone(),
            auth_time: format_timestamp(sso.auth_time),
            expires: format_timestamp(ctx.now + CODE_TTL),
            used_at: None,
            issued_session_id: None,
        };
        if let Err(e) = self.store.write(move |tx| tx.oauth_code_insert(&row)).await {
            tracing::error!(error = %e, "cannot store an authorization code");
            return back("server_error", "Try again later.");
        }
        let mut p = vec![("code", code.as_str())];
        if let Some(s) = &req.state {
            p.push(("state", s.as_str()));
        }
        p.push(("iss", iss.as_str()));
        Authorize::Redirect(with_params(&req.redirect_uri, &p))
    }

    /// The consent page's answer.
    pub async fn consent(
        &self,
        ctx: &RequestCtx,
        mut req: AuthRequest,
        sso: Option<SsoSession>,
        allow: bool,
    ) -> Authorize {
        let Some(s) = sso.clone() else {
            return self.authorize(ctx, req, None).await;
        };
        if !allow {
            let mut p = vec![
                ("error", "access_denied"),
                ("error_description", "The user declined."),
            ];
            if let Some(st) = &req.state {
                p.push(("state", st.as_str()));
            }
            let iss = self.issuer().unwrap_or_default();
            p.push(("iss", iss.as_str()));
            return Authorize::Redirect(with_params(&req.redirect_uri, &p));
        }
        let Ok(Some(client)) = self.client(&req.client_id).await else {
            return Authorize::Error {
                code: "invalid_client",
                message: "This app is not registered here.".into(),
            };
        };
        let row = ConsentRow {
            user_id: s.user_id.clone(),
            collection_id: client.collection_id.clone(),
            client_id: client.id.clone(),
            scopes: req.scopes.join(" "),
            granted_at: format_timestamp(ctx.now),
        };
        let actor = super::sessions::Subject {
            kind: SubjectType::User,
            collection: None,
            id: s.user_id.clone(),
            label: None,
            record: Value::Null,
        }
        .actor(ctx);
        let at = ctx.now;
        let _ = self
            .store
            .write(move |tx| {
                let mut merged: Vec<String> = tx
                    .oauth_consent_get(&row.collection_id, &row.user_id, &row.client_id)?
                    .map(|c| c.scopes.split_whitespace().map(str::to_owned).collect())
                    .unwrap_or_default();
                for sc in row.scopes.split_whitespace() {
                    if !merged.iter().any(|m| m == sc) {
                        merged.push(sc.to_owned());
                    }
                }
                tx.oauth_consent_put(&ConsentRow {
                    scopes: merged.join(" "),
                    ..row.clone()
                })?;
                let mut e = entry(&actor, "oauth.consent", at);
                e.meta = Some(json!({ "client": row.client_id, "scopes": row.scopes }));
                tx.audit_insert(&e)
            })
            .await;
        req.consented = true;
        self.authorize(ctx, req, sso).await
    }

    // Browser sessions -----------------------------------------------------

    /// The browser session behind an `rk_sso` cookie (a session refresh
    /// token, checked without rotating it).
    pub async fn sso_session(&self, token: &str) -> Option<SsoSession> {
        let sid = parse_session_token(TokenKind::Refresh, token)?.to_owned();
        let presented = hash_token(token);
        let now = self.clock.now();
        let compiled = self.schema.get();
        let row = self
            .store
            .read(move |tx| tx.session_get(&sid))
            .await
            .ok()??;
        let valid = row.revoked_at.is_none()
            && row.client_id.is_none()
            && row.subject_type == SubjectType::User
            && row
                .refresh_hash
                .as_deref()
                .is_some_and(|h| ct_eq(h, &presented))
            && row
                .refresh_expires
                .as_deref()
                .is_some_and(|e| e > format_timestamp(now).as_str());
        if !valid {
            return None;
        }
        let c = compiled
            .schema
            .by_id_or_name(row.collection_id.as_deref()?)?;
        let idle = c.auth().map(|o| Duration::seconds(o.idle_timeout_s))?;
        if parse_ts(&row.last_seen).is_some_and(|t| now - t > idle) {
            return None;
        }
        Some(SsoSession {
            session_id: row.id,
            user_id: row.user_id,
            collection_id: row.collection_id?,
            auth_time: parse_ts(&row.created)?,
            auth_method: row.auth_method,
        })
    }

    /// Ends a browser session and every client session started from it.
    pub async fn sso_end(&self, ctx: &RequestCtx, session_id: &str, reason: &str) -> Result<()> {
        let (sid, reason2, at) = (session_id.to_owned(), reason.to_owned(), ctx.now);
        let ids = self
            .store
            .write(move |tx| tx.sessions_revoke_family(&sid, &reason2, &format_timestamp(at)))
            .await?;
        for s in ids {
            self.sessions.invalidate(&s);
        }
        Ok(())
    }

    // Tokens ---------------------------------------------------------------

    async fn authenticate_client(
        &self,
        r: &TokenRequest,
    ) -> std::result::Result<ClientRow, OAuthError> {
        let form_id = r.form.get("client_id").cloned();
        let (id, secret) = match &r.basic {
            Some((id, secret)) => {
                if form_id.as_ref().is_some_and(|f| f != id) {
                    return Err(OAuthError::new(
                        "invalid_request",
                        "client_id does not match the credentials.",
                    ));
                }
                (id.clone(), Some(secret.clone()))
            }
            None => (
                form_id.ok_or_else(|| {
                    OAuthError::new("invalid_client", "Client authentication is missing.")
                })?,
                r.form.get("client_secret").cloned(),
            ),
        };
        let client = self
            .client(&id)
            .await
            .map_err(|e| OAuthError::internal(&e))?
            .filter(|c| c.enabled)
            .ok_or_else(|| OAuthError::new("invalid_client", "Unknown client."))?;
        match (client.kind, &client.secret_hash, secret) {
            (ClientKind::Public, _, None) => Ok(client),
            (ClientKind::Public, _, Some(_)) => Err(OAuthError::new(
                "invalid_client",
                "Public clients have no secret.",
            )),
            (ClientKind::Confidential, Some(hash), Some(s)) if ct_eq(hash, &hash_token(&s)) => {
                Ok(client)
            }
            _ => Err(OAuthError::new(
                "invalid_client",
                "Client authentication failed.",
            )),
        }
    }

    fn sign_jwt(
        &self,
        alg: &str,
        claims: &Value,
        typ: &str,
        now: OffsetDateTime,
    ) -> Result<String> {
        let set = self.keys.get();
        let key = set
            .active(alg, now)
            .or_else(|| set.active("RS256", now))
            .ok_or_else(|| Error::internal("no active signing key"))?;
        jwt::sign(&key.material, &key.row.kid, typ, claims).map_err(Error::internal)
    }

    fn user_claims(&self, c: &Collection, r: &Record, scopes: &[String]) -> Map<String, Value> {
        let mut m = Map::new();
        if scopes.iter().any(|s| s == "email") {
            m.insert("email".into(), r.get("email").clone());
            m.insert(
                "email_verified".into(),
                Value::Bool(r.get("email_verified") == &Value::Bool(true)),
            );
        }
        if scopes.iter().any(|s| s == "profile") {
            let o = c.auth().cloned().unwrap_or_default();
            for (claim, field) in &o.oidc_claims {
                let v = r.get(field);
                if !v.is_null()
                    && v != &Value::String(String::new())
                    && ["name", "picture"].contains(&claim.as_str())
                {
                    let v = match (claim.as_str(), v) {
                        // Files become download URLs.
                        ("picture", Value::String(f))
                            if c.field(field).is_some_and(|x| {
                                matches!(x.kind, crate::domain::FieldKind::File { .. })
                            }) =>
                        {
                            Value::String(format!(
                                "{}/api/files/{}/{}/{f}",
                                self.issuer().unwrap_or_default(),
                                c.name,
                                r.id
                            ))
                        }
                        _ => v.clone(),
                    };
                    m.insert(claim.clone(), v);
                }
            }
            if let Some(t) = parse_ts(&r.updated) {
                m.insert("updated_at".into(), json!(unix(t)));
            }
        }
        m
    }

    /// Builds the token response for a client session.
    #[allow(clippy::too_many_arguments)]
    fn token_response(
        &self,
        client: &ClientRow,
        c: &Collection,
        record: &Record,
        session_id: &str,
        scopes: &[String],
        nonce: Option<&str>,
        auth_time: OffsetDateTime,
        refresh: Option<String>,
        now: OffsetDateTime,
    ) -> Result<Value> {
        let iss = self.issuer()?;
        let scope = scopes.join(" ");
        let access = json!({
            "iss": iss, "sub": record.id.to_string(), "aud": [client.id.clone(), format!("{iss}/api")],
            "exp": unix(now) + client.access_ttl_s, "iat": unix(now), "sid": session_id,
            "scope": scope, "client_id": client.id, "jti": Uuid::now_v7().to_string(),
        });
        let access_token = self.sign_jwt("RS256", &access, "at+jwt", now)?;
        let mut id = json!({
            "iss": iss, "sub": record.id.to_string(), "aud": client.id, "azp": client.id,
            "exp": unix(now + ID_TOKEN_TTL), "iat": unix(now), "auth_time": unix(auth_time), "sid": session_id,
        });
        if let Some(n) = nonce {
            id["nonce"] = json!(n);
        }
        if let Value::Object(m) = &mut id {
            m.extend(self.user_claims(c, record, scopes));
        }
        let id_token = self.sign_jwt(&client.id_token_alg, &id, "JWT", now)?;
        let mut out = json!({
            "access_token": access_token, "token_type": "Bearer", "expires_in": client.access_ttl_s,
            "id_token": id_token, "scope": scope,
        });
        if let Some(r) = refresh {
            out["refresh_token"] = json!(r);
        }
        Ok(out)
    }

    /// `POST /oauth2/token`.
    pub async fn oauth_token(
        &self,
        ctx: &RequestCtx,
        r: TokenRequest,
    ) -> std::result::Result<Value, OAuthError> {
        let client = self.authenticate_client(&r).await?;
        match r.form.get("grant_type").map(String::as_str) {
            Some("authorization_code") => self.grant_code(ctx, &client, &r).await,
            Some("refresh_token") => self.grant_refresh(ctx, &client, &r).await,
            Some(_) => Err(OAuthError::new(
                "unsupported_grant_type",
                "Use authorization_code or refresh_token.",
            )),
            None => Err(OAuthError::new("invalid_request", "grant_type is missing.")),
        }
    }

    async fn grant_code(
        &self,
        ctx: &RequestCtx,
        client: &ClientRow,
        r: &TokenRequest,
    ) -> std::result::Result<Value, OAuthError> {
        let code = r
            .form
            .get("code")
            .ok_or_else(|| OAuthError::new("invalid_request", "code is missing."))?;
        let redirect_uri = r.form.get("redirect_uri").cloned().unwrap_or_default();
        let verifier = r.form.get("code_verifier").cloned();
        let hash = hash_token(code);
        let now = ctx.now;
        let compiled = self.schema.get();
        let (client2, ctx2) = (client.clone(), ctx.clone());
        let outcome = self
            .store
            .write(move |tx| -> Result<std::result::Result<Exchanged, (OAuthError, Option<String>)>> {
                let now_s = format_timestamp(now);
                let Some(row) = tx.oauth_code_take(&hash, &now_s)? else {
                    return Ok(Err((OAuthError::new("invalid_grant", "Unknown code."), None)));
                };
                if row.used_at.is_some() {
                    // Reuse: revoke what the first exchange created.
                    if let Some(sid) = &row.issued_session_id
                        && let Some(mut s) = tx.session_get(sid)?
                        && s.revoked_at.is_none()
                    {
                        s.revoked_at = Some(now_s.clone());
                        s.revoked_reason = Some("code_reuse".into());
                        tx.session_update(&s)?;
                    }
                    return Ok(Err((OAuthError::new("invalid_grant", "The code was used already."), row.issued_session_id.clone())));
                }
                if row.expires.as_str() <= now_s.as_str() || row.client_id != client2.id {
                    return Ok(Err((OAuthError::new("invalid_grant", "The code expired or belongs to another client."), None)));
                }
                if row.redirect_uri != redirect_uri {
                    return Ok(Err((OAuthError::new("invalid_grant", "redirect_uri does not match the authorization request."), None)));
                }
                match (row.code_challenge.as_str(), verifier.as_deref()) {
                    ("", None) => {}
                    ("", Some(_)) => return Ok(Err((OAuthError::new("invalid_grant", "No PKCE challenge was sent."), None))),
                    (_, None) => return Ok(Err((OAuthError::new("invalid_grant", "code_verifier is missing."), None))),
                    (c, Some(v)) => {
                        if !ct_eq(c, &pkce_s256(v)) {
                            return Ok(Err((OAuthError::new("invalid_grant", "code_verifier does not match."), None)));
                        }
                    }
                }
                let Some(sso) = tx.session_get(&row.session_id)?.filter(|s| s.revoked_at.is_none()) else {
                    return Ok(Err((OAuthError::new("invalid_grant", "The sign-in has ended."), None)));
                };
                let Some(c) = sso.collection_id.as_deref().and_then(|id| compiled.schema.by_id_or_name(id)).cloned() else {
                    return Ok(Err((OAuthError::new("invalid_grant", "The account is gone."), None)));
                };
                let Some(record) = tx.record_get(&c, &sso.user_id)? else {
                    return Ok(Err((OAuthError::new("invalid_grant", "The account is gone."), None)));
                };
                let scopes: Vec<&str> = row.scopes.split_whitespace().collect();
                let offline = scopes.contains(&"offline_access");
                let id = Uuid::now_v7().to_string();
                let refresh = offline.then(|| session_token(TokenKind::Refresh, &id));
                let lifetime = if offline { client2.refresh_ttl_s } else { client2.access_ttl_s };
                let session = SessionRow {
                    id: id.clone(),
                    family_id: sso.id.clone(),
                    subject_type: SubjectType::User,
                    collection_id: Some(c.id.to_string()),
                    user_id: sso.user_id.clone(),
                    client_id: Some(client2.id.clone()),
                    auth_method: sso.auth_method.clone(),
                    access_hash: hash_token(&random_token()),
                    access_expires: format_timestamp(now + Duration::seconds(client2.access_ttl_s)),
                    refresh_hash: refresh.as_deref().map(hash_token),
                    prev_refresh_hash: None,
                    refresh_expires: Some(format_timestamp(now + Duration::seconds(lifetime))),
                    created: now_s.clone(),
                    last_seen: now_s,
                    revoked_at: None,
                    revoked_reason: None,
                    ip: ctx2.ip.clone(),
                    user_agent: ctx2.user_agent.clone(),
                    scopes: Some(row.scopes.clone()),
                };
                tx.session_insert(&session)?;
                tx.oauth_code_set_issued(&hash, &id)?;
                let subject = super::sessions::Subject::user(&c, &record);
                audit_auth(
                    tx,
                    &subject.actor(&ctx2),
                    "auth.login.success",
                    now,
                    json!({ "method": "oidc", "client": client2.id, "session": id, "collection": c.id }),
                )?;
                Ok(Ok((Box::new(session), c, record, row, refresh)))
            })
            .await
            .map_err(|e| OAuthError::internal(&e))?;
        let (session, c, record, code_row, refresh) = match outcome {
            Ok(v) => v,
            Err((e, revoked)) => {
                if let Some(s) = revoked {
                    self.sessions.invalidate(&s);
                }
                return Err(e);
            }
        };
        let scopes: Vec<String> = code_row
            .scopes
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        let auth_time = parse_ts(&code_row.auth_time).unwrap_or(now);
        self.token_response(
            client,
            &c,
            &record,
            &session.id,
            &scopes,
            code_row.nonce.as_deref(),
            auth_time,
            refresh,
            now,
        )
        .map_err(|e| OAuthError::internal(&e))
    }

    async fn grant_refresh(
        &self,
        ctx: &RequestCtx,
        client: &ClientRow,
        r: &TokenRequest,
    ) -> std::result::Result<Value, OAuthError> {
        let token = r
            .form
            .get("refresh_token")
            .ok_or_else(|| OAuthError::new("invalid_request", "refresh_token is missing."))?;
        // Check a narrowed scope before rotating, so a refused request does
        // not use up the refresh token.
        if let Some(asked) = r.form.get("scope")
            && let Some(sid) = parse_session_token(TokenKind::Refresh, token).map(str::to_owned)
        {
            let granted = self
                .store
                .read(move |tx| tx.session_get(&sid))
                .await
                .map_err(|e| OAuthError::internal(&e))?
                .and_then(|s| s.scopes)
                .unwrap_or_default();
            let granted: Vec<&str> = granted.split_whitespace().collect();
            if asked.split_whitespace().any(|a| !granted.contains(&a)) {
                return Err(OAuthError::new(
                    "invalid_scope",
                    "A refresh cannot add scopes.",
                ));
            }
        }
        let tokens: Tokens = self
            .refresh(
                ctx,
                RefreshScope::Client {
                    client_id: &client.id,
                    access_ttl_s: client.access_ttl_s,
                    refresh_ttl_s: client.refresh_ttl_s,
                },
                token,
            )
            .await
            .map_err(|e| match e {
                Error::Unauthorized => OAuthError::new(
                    "invalid_grant",
                    "The refresh token is invalid, expired or revoked.",
                ),
                e => OAuthError::internal(&e),
            })?;
        let sid = tokens.session_id.clone();
        let compiled = self.schema.get();
        let loaded = self
            .store
            .read(move |tx| {
                let Some(s) = tx.session_get(&sid)? else {
                    return Ok(None);
                };
                let auth_time = tx.session_get(&s.family_id)?.map(|f| f.created);
                let Some(c) = s
                    .collection_id
                    .as_deref()
                    .and_then(|id| compiled.schema.by_id_or_name(id))
                    .cloned()
                else {
                    return Ok(None);
                };
                Ok(tx
                    .record_get(&c, &s.user_id)?
                    .map(|rec| (s, auth_time, c, rec)))
            })
            .await
            .map_err(|e| OAuthError::internal(&e))?;
        let Some((s, auth_time, c, record)) = loaded else {
            return Err(OAuthError::new("invalid_grant", "The account is gone."));
        };
        let mut scopes: Vec<String> = s
            .scopes
            .as_deref()
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        if let Some(asked) = r.form.get("scope") {
            let asked: Vec<&str> = asked.split_whitespace().collect();
            if asked.iter().any(|a| !scopes.iter().any(|s| s == a)) {
                return Err(OAuthError::new(
                    "invalid_scope",
                    "A refresh cannot add scopes.",
                ));
            }
            scopes.retain(|s| asked.contains(&s.as_str()));
        }
        let auth_time = auth_time.as_deref().and_then(parse_ts).unwrap_or(ctx.now);
        self.token_response(
            client,
            &c,
            &record,
            &s.id,
            &scopes,
            None,
            auth_time,
            Some(tokens.refresh_token),
            ctx.now,
        )
        .map_err(|e| OAuthError::internal(&e))
    }

    // Access tokens --------------------------------------------------------

    /// Verifies an access JWT (signature, `iss`, `aud`, `exp`); returns its claims.
    pub fn verify_access_jwt(&self, token: &str) -> Result<Value> {
        let set = self.keys.get();
        let claims = jwt::verify(token, |kid| set.find(kid)).map_err(|_| Error::Unauthorized)?;
        let iss = self.issuer().map_err(|_| Error::Unauthorized)?;
        let aud_ok = claims["aud"]
            .as_array()
            .is_some_and(|a| a.iter().any(|x| x.as_str() == Some(&format!("{iss}/api"))));
        let now = unix(self.clock.now());
        if claims["iss"].as_str() != Some(iss.as_str())
            || !aud_ok
            || claims["exp"].as_i64().is_none_or(|e| e <= now)
        {
            return Err(Error::Unauthorized);
        }
        Ok(claims)
    }

    /// The principal of an access JWT: valid token and an active session.
    pub(crate) async fn authenticate_jwt(&self, token: &str) -> Result<Principal> {
        let claims = self.verify_access_jwt(token)?;
        let sid = claims["sid"]
            .as_str()
            .ok_or(Error::Unauthorized)?
            .to_owned();
        if let Some(c) = self.sessions.get(&sid)
            && c.access_hash.is_empty()
        {
            return Ok(c.principal);
        }
        let compiled = self.schema.get();
        let (sid2, client) = (sid.clone(), claims["client_id"].as_str().map(str::to_owned));
        let now = format_timestamp(self.clock.now());
        let principal = self
            .store
            .read(move |tx| {
                let Some(row) = tx.session_get(&sid2)? else {
                    return Ok(None);
                };
                let live = row.revoked_at.is_none()
                    && row.client_id.is_some()
                    && row.client_id == client
                    && row
                        .refresh_expires
                        .as_deref()
                        .is_some_and(|e| e > now.as_str());
                if !live {
                    return Ok(None);
                }
                let Some(c) = row
                    .collection_id
                    .as_deref()
                    .and_then(|id| compiled.schema.by_id_or_name(id))
                    .cloned()
                else {
                    return Ok(None);
                };
                Ok(tx
                    .record_get(&c, &row.user_id)?
                    .map(|r| (user_principal(&c, &r, &row.id), row.refresh_expires)))
            })
            .await?;
        let Some((principal, expires)) = principal else {
            return Err(Error::Unauthorized);
        };
        self.sessions.put(
            sid,
            CachedSession {
                principal: principal.clone(),
                access_hash: String::new(),
                access_expires: expires.unwrap_or_default(),
                at: std::time::Instant::now(),
            },
        );
        Ok(principal)
    }

    /// Whether `token` is an access JWT rather than an opaque token.
    pub fn is_jwt(token: &str) -> bool {
        looks_like_jwt(token)
    }

    /// `GET/POST /oauth2/userinfo`.
    pub async fn oauth_userinfo(&self, token: &str) -> std::result::Result<Value, OAuthError> {
        let claims = self.verify_access_jwt(token).map_err(|_| {
            OAuthError::new("invalid_token", "The access token is invalid or expired.")
        })?;
        let principal = self
            .authenticate_jwt(token)
            .await
            .map_err(|_| OAuthError::new("invalid_token", "The session has ended."))?;
        let Principal::User {
            id, collection_id, ..
        } = principal
        else {
            return Err(OAuthError::new("invalid_token", "Not a user token."));
        };
        let compiled = self.schema.get();
        let c = compiled
            .schema
            .by_id_or_name(&collection_id)
            .cloned()
            .ok_or_else(|| OAuthError::new("invalid_token", "The account is gone."))?;
        let (c2, id2) = (Arc::clone(&c), id.clone());
        let record = self
            .store
            .read(move |tx| tx.record_get(&c2, &id2))
            .await
            .map_err(|e| OAuthError::internal(&e))?
            .ok_or_else(|| OAuthError::new("invalid_token", "The account is gone."))?;
        let scopes: Vec<String> = claims["scope"]
            .as_str()
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        let mut out = Map::new();
        out.insert("sub".into(), json!(id));
        out.extend(self.user_claims(&c, &record, &scopes));
        Ok(Value::Object(out))
    }

    /// `POST /oauth2/revoke` (RFC 7009): always succeeds for a valid client.
    pub async fn oauth_revoke(
        &self,
        ctx: &RequestCtx,
        r: TokenRequest,
    ) -> std::result::Result<(), OAuthError> {
        let client = self.authenticate_client(&r).await?;
        let Some(token) = r.form.get("token") else {
            return Err(OAuthError::new("invalid_request", "token is missing."));
        };
        let sid = if looks_like_jwt(token) {
            self.verify_access_jwt(token)
                .ok()
                .filter(|c| c["client_id"].as_str() == Some(client.id.as_str()))
                .and_then(|c| c["sid"].as_str().map(str::to_owned))
        } else {
            parse_session_token(TokenKind::Refresh, token).map(str::to_owned)
        };
        let Some(sid) = sid else { return Ok(()) };
        let sid_out = sid.clone();
        let (presented, jwt_token, client_id, at) = (
            hash_token(token),
            looks_like_jwt(token),
            client.id.clone(),
            ctx.now,
        );
        let revoked = self
            .store
            .write(move |tx| {
                let Some(mut row) = tx.session_get(&sid)? else {
                    return Ok(false);
                };
                let owns = row.client_id.as_deref() == Some(client_id.as_str());
                let matches = jwt_token
                    || row
                        .refresh_hash
                        .as_deref()
                        .is_some_and(|h| ct_eq(h, &presented));
                if !owns || !matches || row.revoked_at.is_some() {
                    return Ok(false);
                }
                row.revoked_at = Some(format_timestamp(at));
                row.revoked_reason = Some("revoked".into());
                tx.session_update(&row)?;
                Ok(true)
            })
            .await
            .map_err(|e| OAuthError::internal(&e))?;
        if revoked {
            self.sessions.invalidate(&sid_out);
        }
        Ok(())
    }

    /// `GET/POST /oauth2/logout` (RP-initiated logout). Without a valid
    /// `id_token_hint` the user confirms first, so other sites cannot sign
    /// people out. `confirmed` comes from the confirmation page.
    pub async fn oauth_logout(
        &self,
        ctx: &RequestCtx,
        q: &BTreeMap<String, String>,
        sso: Option<SsoSession>,
        confirmed: bool,
    ) -> Logout {
        let get = |k: &str| q.get(k).filter(|v| !v.is_empty()).cloned();
        let set = self.keys.get();
        // Expired ID tokens are fine as hints; the signature is not optional.
        let hint = get("id_token_hint").and_then(|t| jwt::verify(&t, |kid| set.find(kid)).ok());
        let iss = self.issuer().unwrap_or_default();
        let hint = hint.filter(|c| c["iss"].as_str() == Some(iss.as_str()));
        let client_id = get("client_id").or_else(|| {
            hint.as_ref()
                .and_then(|c| c["aud"].as_str().map(str::to_owned))
        });
        if hint.is_none() && !confirmed && sso.is_some() {
            return Logout::Confirm;
        }
        if let Some(s) = &sso {
            let same_user = hint
                .as_ref()
                .is_none_or(|c| c["sub"].as_str() == Some(s.user_id.as_str()));
            if same_user {
                let _ = self.sso_end(ctx, &s.session_id, "logout").await;
            }
        }
        let target = match (get("post_logout_redirect_uri"), client_id) {
            (Some(uri), Some(cid)) => match self.client(&cid).await {
                Ok(Some(c)) if c.post_logout_allowed(&uri) => Some(uri),
                _ => None,
            },
            _ => None,
        };
        match target {
            Some(uri) => match get("state") {
                Some(state) => Logout::Redirect(with_params(&uri, &[("state", state.as_str())])),
                None => Logout::Redirect(uri),
            },
            None => Logout::Done,
        }
    }

    /// A browser session for a hosted-page sign-in (password, code,
    /// passkey, provider): returns the `rk_sso` cookie value.
    pub fn sso_cookie_value(tokens: &Tokens) -> String {
        tokens.refresh_token.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(
            origin("https://app.example.com:8443/cb?x=1"),
            "https://app.example.com:8443"
        );
        assert_eq!(
            with_params("https://a/cb", &[("code", "a b"), ("state", "s&t")]),
            "https://a/cb?code=a%20b&state=s%26t"
        );
        assert_eq!(
            with_params("https://a/cb?x=1", &[("code", "c")]),
            "https://a/cb?x=1&code=c"
        );
        // base64url(SHA-256(verifier)), no padding (checked with Python's hashlib).
        assert_eq!(
            pkce_s256("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWzIgjFk"),
            "vMcmKl5vfFovOaDSYSBnKdkHDgRIXhXhFGECGQBvFu8"
        );
    }
}
