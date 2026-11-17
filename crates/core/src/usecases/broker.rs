//! External identity providers (SPEC §11.7): provider configuration, the
//! authorization code flow with PKCE, identity linking (§11.7.1), exchange
//! codes for SPAs and the identities API.

use std::sync::{Arc, RwLock};

use serde_json::{Map, Value, json};
use time::Duration;
use uuid::Uuid;

use super::audit::entry;
use super::auth::{Tokens, audit_auth};
use super::records::{system_create, system_update};
use super::sessions::{Subject, start_session};
use super::{App, Principal, RequestCtx};
use crate::auth::broker::{IdentityRow, ProviderRow, TokenPurpose, TokenRow};
use crate::auth::token::{ct_eq, hash_token, random_token};
use crate::domain::field::FieldKind;
use crate::domain::{Actor, Collection};
use crate::error::{FieldError, ValidationErrors};
use crate::ports::{IdpConfig, IdpUser, StoreExt as _};
use crate::settings::MASK;
use crate::time::format_timestamp;
use crate::{Error, Result};

/// The state cookie lives this long (SPEC §11.7).
pub const STATE_TTL: Duration = Duration::minutes(10);
/// Exchange codes live this long.
pub const EXCHANGE_TTL: Duration = Duration::seconds(60);
/// Name of the state cookie.
pub const STATE_COOKIE: &str = "rackend_provider_state";

/// Bundled provider presets.
pub fn presets() -> Value {
    serde_json::from_str(include_str!("../../data/provider-presets.json")).unwrap_or(Value::Null)
}

/// Enabled providers, cached for `auth-methods`.
#[derive(Debug, Default)]
pub struct ProviderCache(RwLock<Arc<Vec<ProviderRow>>>);

impl ProviderCache {
    fn get(&self) -> Arc<Vec<ProviderRow>> {
        self.0
            .read()
            .map_or_else(|p| Arc::clone(&p.into_inner()), |g| Arc::clone(&g))
    }

    fn set(&self, rows: Vec<ProviderRow>) {
        let rows = Arc::new(rows);
        match self.0.write() {
            Ok(mut g) => *g = rows,
            Err(p) => *p.into_inner() = rows,
        }
    }
}

/// A started provider login: send the browser to `url` and set the state
/// cookie (`cookie` is its encrypted value, `cookie_path` its path).
#[derive(Debug, Clone)]
pub struct ProviderStart {
    /// Authorization URL.
    pub url: String,
    /// Encrypted cookie value.
    pub cookie: String,
    /// Cookie path (the callback).
    pub cookie_path: String,
    /// Mark the cookie `Secure` (HTTPS public URL).
    pub secure: bool,
}

/// How a provider callback ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallbackOutcome {
    /// Redirect to the app (`#code=` or `#error=`).
    Redirect(String),
    /// Show an error page (no app to return to).
    Error {
        /// Problem code (`invalid_state`, `account_not_found`, …).
        code: &'static str,
        /// Message for the page.
        message: String,
    },
}

/// What the callback received.
#[derive(Debug, Clone, Default)]
pub struct CallbackParams {
    /// `code`.
    pub code: Option<String>,
    /// `state`.
    pub state: Option<String>,
    /// `error` from the provider.
    pub error: Option<String>,
    /// The state cookie.
    pub cookie: Option<String>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct FlowState {
    state: String,
    verifier: String,
    nonce: String,
    provider: String,
    collection: String,
    redirect_uri: String,
    link_user: Option<String>,
    exp: i64,
}

#[derive(Debug)]
struct Failure(&'static str, String);

impl Failure {
    fn new(code: &'static str, msg: impl Into<String>) -> Self {
        Self(code, msg.into())
    }
}

fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 40
        && n.bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && n.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// `https://…`, or `http://` for loopback hosts and in dev mode.
fn valid_issuer(u: &str, dev: bool) -> bool {
    if u.starts_with("https://") && u.len() > 8 {
        return true;
    }
    let Some(rest) = u.strip_prefix("http://") else {
        return false;
    };
    dev || rest.starts_with("localhost")
        || rest.starts_with("127.0.0.1")
        || rest.starts_with("[::1]")
}

fn scopes_of(p: &ProviderRow) -> Vec<String> {
    p.scopes
        .as_deref()
        .map(|s| s.split_whitespace().map(str::to_owned).collect())
        .unwrap_or_default()
}

fn claim_mapping(p: &ProviderRow) -> Map<String, Value> {
    p.claim_mapping
        .as_deref()
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

impl App {
    /// Re-reads the providers.
    pub async fn reload_providers(&self) -> Result<()> {
        let rows = self.store.read(|tx| tx.providers_list()).await?;
        self.providers.set(rows);
        Ok(())
    }

    /// Enabled providers of collection `c`, for `auth-methods`.
    pub(crate) fn providers_for(&self, c: &Collection) -> Vec<Value> {
        let id = c.id.to_string();
        self.providers
            .get()
            .iter()
            .filter(|p| p.enabled && p.collection_id == id)
            .map(|p| {
                json!({
                    "name": p.name,
                    "kind": p.kind,
                    "preset": p.preset,
                    "start_url": format!("/api/collections/{}/providers/{}/start", c.name, p.name),
                })
            })
            .collect()
    }

    /// The callback URL to register at the provider.
    fn callback_url(&self, collection: &str, name: &str) -> Result<String> {
        let base = self
            .settings()
            .public_url()
            .ok_or(Error::NotConfigured("public_url"))?;
        Ok(format!(
            "{base}/api/collections/{collection}/providers/{name}/callback"
        ))
    }

    fn provider_json(&self, p: &ProviderRow) -> Value {
        let compiled = self.schema.get();
        let collection = compiled
            .schema
            .by_id_or_name(&p.collection_id)
            .map(|c| c.name.clone());
        json!({
            "id": p.id,
            "name": p.name,
            "kind": p.kind,
            "preset": p.preset,
            "issuer": p.issuer,
            "client_id": p.client_id,
            "client_secret": p.client_secret_enc.as_ref().map(|_| MASK),
            "scopes": scopes_of(p),
            "collection": collection,
            "auto_link_verified_email": p.auto_link_verified_email,
            "claim_mapping": claim_mapping(p),
            "enabled": p.enabled,
            "created": p.created,
            "updated": p.updated,
            "redirect_uri": collection.and_then(|c| self.callback_url(&c, &p.name).ok()),
        })
    }

    // Admin ----------------------------------------------------------------

    /// `GET /api/auth-providers`.
    pub async fn providers_list(&self, ctx: &RequestCtx) -> Result<Value> {
        ctx.require_superuser()?;
        let rows = self.store.read(|tx| tx.providers_list()).await?;
        Ok(
            json!({ "items": rows.iter().map(|p| self.provider_json(p)).collect::<Vec<_>>(), "presets": presets() }),
        )
    }

    /// `POST /api/auth-providers` (no `id`) or `PATCH /api/auth-providers/{id}`.
    /// `client_secret`: a new value, `"••••"` or absent to keep, `null` to clear.
    pub async fn provider_save(
        &self,
        ctx: &RequestCtx,
        id: Option<&str>,
        body: Value,
    ) -> Result<Value> {
        ctx.require_superuser()?;
        let body = body
            .as_object()
            .cloned()
            .ok_or_else(|| Error::BadRequest("Send a JSON object.".into()))?;
        let rows = self.store.read(|tx| tx.providers_list()).await?;
        let existing = match id {
            Some(id) => Some(
                rows.iter()
                    .find(|p| p.id == id)
                    .cloned()
                    .ok_or(Error::NotFound)?,
            ),
            None => None,
        };
        let now = format_timestamp(ctx.now);
        let mut p = existing.clone().unwrap_or_else(|| ProviderRow {
            id: Uuid::now_v7().to_string(),
            name: String::new(),
            kind: "oidc".into(),
            preset: None,
            issuer: None,
            client_id: String::new(),
            client_secret_enc: None,
            scopes: None,
            collection_id: String::new(),
            auto_link_verified_email: false,
            claim_mapping: None,
            enabled: true,
            created: now.clone(),
            updated: now.clone(),
        });
        p.updated = now;
        let mut errs = ValidationErrors::default();
        let str_of = |k: &str| {
            body.get(k)
                .and_then(Value::as_str)
                .map(str::trim)
                .map(str::to_owned)
        };
        if let Some(n) = str_of("name") {
            p.name = n;
        }
        if !valid_name(&p.name) {
            errs.add(
                "name",
                FieldError::new(
                    "invalid_name",
                    "Use 1 to 40 lowercase letters, digits, - or _.",
                ),
            );
        }
        if let Some(k) = str_of("kind") {
            p.kind = k;
        }
        if let Some(v) = body.get("preset") {
            p.preset = v.as_str().filter(|s| !s.is_empty()).map(str::to_owned);
        }
        match p.kind.as_str() {
            "oidc" => {
                if let Some(v) = body.get("issuer") {
                    p.issuer = v
                        .as_str()
                        .map(|s| s.trim().trim_end_matches('/').to_owned());
                }
                if !p
                    .issuer
                    .as_deref()
                    .is_some_and(|u| valid_issuer(u, self.dev_mode))
                {
                    errs.add(
                        "issuer",
                        FieldError::new("invalid_issuer", "Must be an https URL."),
                    );
                } else if p.issuer.as_deref().is_some_and(|u| u.contains('{')) {
                    errs.add(
                        "issuer",
                        FieldError::new("invalid_issuer", "Replace the placeholders in braces."),
                    );
                }
            }
            "oauth2_preset" => {
                p.issuer = None;
                if p.preset.as_deref() != Some("github") {
                    errs.add(
                        "preset",
                        FieldError::new("unknown_preset", "Supported: github."),
                    );
                }
            }
            _ => errs.add(
                "kind",
                FieldError::new("invalid_kind", "Must be oidc or oauth2_preset."),
            ),
        }
        if let Some(c) = str_of("client_id") {
            p.client_id = c;
        }
        if p.client_id.is_empty() {
            errs.add("client_id", FieldError::new("required", "Required."));
        }
        match body.get("client_secret") {
            Some(Value::Null) => p.client_secret_enc = None,
            Some(Value::String(s)) if s != MASK && !s.is_empty() => {
                p.client_secret_enc = Some(self.secrets.encrypt_str(s));
            }
            _ => {}
        }
        if let Some(v) = body.get("scopes") {
            let scopes: Vec<String> = match v {
                Value::Array(a) => a
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect(),
                Value::String(s) => s.split_whitespace().map(str::to_owned).collect(),
                _ => Vec::new(),
            };
            p.scopes = (!scopes.is_empty()).then(|| scopes.join(" "));
        }
        if let Some(c) = str_of("collection") {
            match self.auth_collection(&c) {
                Ok(c) => p.collection_id = c.id.to_string(),
                Err(_) => errs.add(
                    "collection",
                    FieldError::new("not_found", "No auth collection with this name."),
                ),
            }
        }
        if p.collection_id.is_empty() {
            errs.add("collection", FieldError::new("required", "Required."));
        }
        if let Some(b) = body
            .get("auto_link_verified_email")
            .and_then(Value::as_bool)
        {
            p.auto_link_verified_email = b;
        }
        if let Some(b) = body.get("enabled").and_then(Value::as_bool) {
            p.enabled = b;
        }
        match body.get("claim_mapping") {
            Some(Value::Object(m)) if m.values().all(Value::is_string) => {
                p.claim_mapping = (!m.is_empty()).then(|| Value::Object(m.clone()).to_string());
            }
            Some(Value::Null) => p.claim_mapping = None,
            Some(_) => errs.add(
                "claim_mapping",
                FieldError::new("invalid", "Map claim names to field names."),
            ),
            None => {}
        }
        if !errs.is_empty() {
            return Err(Error::Validation(errs));
        }
        let (row, actor, at) = (p.clone(), ctx.actor(), ctx.now);
        let action = if existing.is_some() {
            "auth_provider.update"
        } else {
            "auth_provider.create"
        };
        self.store
            .write(move |tx| {
                tx.provider_put(&row)?;
                let mut e = entry(&actor, action, at);
                e.meta = Some(json!({ "id": row.id, "name": row.name, "kind": row.kind, "issuer": row.issuer }));
                tx.audit_insert(&e)
            })
            .await?;
        self.reload_providers().await?;
        Ok(self.provider_json(&p))
    }

    /// `DELETE /api/auth-providers/{id}`. Linked identities stay, so
    /// re-adding the provider restores them.
    pub async fn provider_delete(&self, ctx: &RequestCtx, id: &str) -> Result<()> {
        ctx.require_superuser()?;
        let (id, actor, at) = (id.to_owned(), ctx.actor(), ctx.now);
        self.store
            .write(move |tx| {
                if !tx.provider_delete(&id)? {
                    return Err(Error::NotFound);
                }
                let mut e = entry(&actor, "auth_provider.delete", at);
                e.meta = Some(json!({ "id": id }));
                tx.audit_insert(&e)
            })
            .await?;
        self.reload_providers().await
    }

    /// `POST /api/auth-providers/discover {issuer, collection?, name?}`:
    /// validates discovery and returns the metadata and the redirect URI to
    /// register at the provider.
    pub async fn provider_discover(&self, ctx: &RequestCtx, body: &Value) -> Result<Value> {
        ctx.require_superuser()?;
        let issuer = body
            .get("issuer")
            .and_then(Value::as_str)
            .map(|s| s.trim().trim_end_matches('/').to_owned())
            .unwrap_or_default();
        if !valid_issuer(&issuer, self.dev_mode) {
            return Err(Error::field(
                "issuer",
                FieldError::new("invalid_issuer", "Must be an https URL."),
            ));
        }
        let metadata = self.idp.discover(&issuer).await.map_err(|e| {
            Error::field("issuer", FieldError::new("discovery_failed", e.to_string()))
        })?;
        let collection = body
            .get("collection")
            .and_then(Value::as_str)
            .unwrap_or("users");
        let name = body.get("name").and_then(Value::as_str).unwrap_or("{name}");
        Ok(json!({
            "metadata": metadata,
            "redirect_uri": self.callback_url(collection, name).ok(),
        }))
    }

    // Login flow -----------------------------------------------------------

    fn provider_by_name(&self, c: &Collection, name: &str) -> Result<ProviderRow> {
        let id = c.id.to_string();
        self.providers
            .get()
            .iter()
            .find(|p| p.enabled && p.collection_id == id && p.name == name)
            .cloned()
            .ok_or(Error::NotFound)
    }

    fn idp_config(&self, c: &Collection, p: &ProviderRow) -> Result<IdpConfig> {
        let client_secret = match &p.client_secret_enc {
            Some(enc) => self
                .secrets
                .decrypt_str(enc)
                .map_err(|_| Error::internal("cannot decrypt the provider's client secret"))?,
            None => String::new(),
        };
        let mut scopes = scopes_of(p);
        if scopes.is_empty() {
            scopes = if p.kind == "oidc" {
                vec!["openid".into(), "email".into(), "profile".into()]
            } else {
                vec!["read:user".into(), "user:email".into()]
            };
        }
        Ok(IdpConfig {
            kind: p.kind.clone(),
            preset: p.preset.clone(),
            issuer: p.issuer.clone(),
            client_id: p.client_id.clone(),
            client_secret,
            scopes,
            redirect_uri: self.callback_url(&c.name, &p.name)?,
        })
    }

    /// `GET /api/collections/{c}/providers/{name}/start?redirect_uri=&link=`.
    /// With `link`, the caller must be signed in to this collection; the
    /// provider identity is then added to their account.
    pub async fn provider_start(
        &self,
        ctx: &RequestCtx,
        key: &str,
        name: &str,
        redirect_uri: Option<&str>,
        link: bool,
    ) -> Result<ProviderStart> {
        let c = self.auth_collection(key)?;
        let p = self.provider_by_name(&c, name)?;
        let redirect_uri = redirect_uri.unwrap_or_default();
        if redirect_uri.is_empty() {
            return Err(Error::field(
                "redirect_uri",
                FieldError::new("required", "Pass the app URI to return to."),
            ));
        }
        if !self.redirect_allowed(redirect_uri) {
            return Err(Error::field(
                "redirect_uri",
                FieldError::new(
                    "redirect_not_allowed",
                    "Add this URI to security.app_redirect_uris first.",
                ),
            ));
        }
        let link_user = if link {
            match &ctx.principal {
                Principal::User {
                    id, collection_id, ..
                } if *collection_id == c.id.to_string() => Some(id.clone()),
                Principal::Anonymous => return Err(Error::Unauthorized),
                _ => {
                    return Err(Error::Forbidden(
                        "Sign in as a user of this collection.".into(),
                    ));
                }
            }
        } else {
            None
        };
        let cfg = self.idp_config(&c, &p)?;
        let started = self.idp.start(&cfg).await.map_err(|e| {
            tracing::warn!(error = %e, provider = %p.name, "cannot start a provider sign-in");
            Error::unavailable(crate::error::UnavailableReason::IdentityProvider)
        })?;
        let flow = FlowState {
            state: started.state,
            verifier: started.pkce_verifier,
            nonce: started.nonce,
            provider: p.id.clone(),
            collection: c.id.to_string(),
            redirect_uri: redirect_uri.to_owned(),
            link_user,
            exp: (ctx.now + STATE_TTL).unix_timestamp(),
        };
        let cookie = self.secrets.encrypt_str(
            &serde_json::to_string(&flow).map_err(|e| Error::internal(e.to_string()))?,
        );
        Ok(ProviderStart {
            url: started.url,
            cookie,
            cookie_path: format!("/api/collections/{}/providers/{}/callback", c.name, p.name),
            secure: cfg.redirect_uri.starts_with("https://"),
        })
    }

    /// `GET /api/collections/{c}/providers/{name}/callback`.
    pub async fn provider_callback(
        &self,
        ctx: &RequestCtx,
        key: &str,
        name: &str,
        params: CallbackParams,
    ) -> CallbackOutcome {
        let flow = params
            .cookie
            .as_deref()
            .and_then(|v| self.secrets.decrypt_str(v).ok())
            .and_then(|s| serde_json::from_str::<FlowState>(&s).ok())
            .filter(|f| f.exp > ctx.now.unix_timestamp());
        let Some(flow) = flow else {
            return CallbackOutcome::Error {
                code: "invalid_state",
                message:
                    "The sign-in took too long or was started in another browser. Start again."
                        .into(),
            };
        };
        let back =
            |frag: String| CallbackOutcome::Redirect(format!("{}#{frag}", flow.redirect_uri));
        match self
            .finish_provider_login(ctx, key, name, &params, &flow)
            .await
        {
            Ok(code) => back(format!("code={code}")),
            Err(Failure(code, message)) => {
                tracing::info!(code, %message, provider = name, "provider sign-in failed");
                back(format!("error={code}"))
            }
        }
    }

    async fn finish_provider_login(
        &self,
        ctx: &RequestCtx,
        key: &str,
        name: &str,
        params: &CallbackParams,
        flow: &FlowState,
    ) -> std::result::Result<String, Failure> {
        let state_ok = params
            .state
            .as_deref()
            .is_some_and(|s| ct_eq(s, &flow.state));
        if !state_ok {
            return Err(Failure::new("invalid_state", "state mismatch"));
        }
        if let Some(e) = &params.error {
            let code = if e == "access_denied" {
                "access_denied"
            } else {
                "provider_error"
            };
            return Err(Failure::new(code, e.clone()));
        }
        let c = self
            .auth_collection(key)
            .map_err(|_| Failure::new("invalid_state", "collection"))?;
        let p = self
            .provider_by_name(&c, name)
            .map_err(|_| Failure::new("invalid_state", "provider"))?;
        if p.id != flow.provider || c.id.to_string() != flow.collection {
            return Err(Failure::new(
                "invalid_state",
                "provider or collection mismatch",
            ));
        }
        let code = params
            .code
            .as_deref()
            .ok_or_else(|| Failure::new("provider_error", "no code"))?;
        let cfg = self
            .idp_config(&c, &p)
            .map_err(|e| Failure::new("provider_error", e.to_string()))?;
        let user = self
            .idp
            .finish(&cfg, code, &flow.verifier, &flow.nonce)
            .await
            .map_err(|e| Failure::new("provider_error", e.to_string()))?;
        let exchange = random_token();
        self.link_identity(
            ctx,
            &c,
            &p,
            user,
            flow.link_user.clone(),
            hash_token(&exchange),
        )
        .await?;
        Ok(exchange)
    }

    /// The identity linking rules (SPEC §11.7.1), then an exchange code.
    async fn link_identity(
        &self,
        ctx: &RequestCtx,
        c: &Arc<Collection>,
        p: &ProviderRow,
        user: IdpUser,
        link_user: Option<String>,
        exchange_hash: String,
    ) -> std::result::Result<(), Failure> {
        let opts = c.auth().cloned().unwrap_or_default();
        let mut mapping = Map::new();
        for (claim, field) in &opts.oidc_claims {
            mapping.insert(claim.clone(), Value::String(field.clone()));
        }
        mapping.extend(claim_mapping(p));
        let mut signup = Map::new();
        if let Some(email) = &user.email {
            signup.insert("email".into(), Value::String(email.clone()));
            signup.insert("email_verified".into(), Value::Bool(user.email_verified));
        }
        for (claim, field) in &mapping {
            let (Some(field), Some(v)) = (
                field.as_str(),
                user.claims.get(claim).and_then(Value::as_str),
            ) else {
                continue;
            };
            if ["email", "email_verified", "id"].contains(&field) {
                continue;
            }
            // Text-like fields only: avatars and other files are not fetched (SPEC §17.6).
            if let Some(f) = c.field(field)
                && matches!(f.kind, FieldKind::Text { .. } | FieldKind::Url { .. })
                && f.normalize(&Value::String(v.to_owned())).is_ok()
            {
                signup.insert(field.to_owned(), Value::String(v.to_owned()));
            }
        }
        let w = self.system_write_ctx(ctx, Actor { ..ctx.actor() });
        let (c2, p2, ctx2) = (Arc::clone(c), p.clone(), ctx.clone());
        let res = self
            .store
            .write(move |tx| {
                let now = format_timestamp(ctx2.now);
                let cid = c2.id.to_string();
                let found = tx.identity_by_subject(&user.issuer, &user.subject)?;
                let mut audit: Option<&'static str> = None;
                // Rule 1: a known identity.
                let record = if let Some(i) = &found {
                    if i.collection_id != cid {
                        return Ok(Err(Failure::new("account_not_found", "identity of another collection")));
                    }
                    if link_user.as_ref().is_some_and(|u| *u != i.user_id) {
                        return Ok(Err(Failure::new("identity_in_use", "linked to another account")));
                    }
                    tx.identity_touch(&i.id, &now)?;
                    tx.record_get(&c2, &i.user_id)?
                } else if let Some(uid) = &link_user {
                    // Rule 2: the signed-in user links this identity.
                    audit = Some("identity.link");
                    tx.record_get(&c2, uid)?
                } else {
                    let by_email = match &user.email {
                        Some(e) => tx.auth_find_by_email(&c2, e)?.map(|(r, _)| r),
                        None => None,
                    };
                    match by_email {
                        // Rule 3: verified email, provider trusted for it. The
                        // account's email must be verified too, or whoever
                        // registered it first (without access to the inbox)
                        // would share the account (pre-registration hijacking).
                        Some(r)
                            if p2.auto_link_verified_email
                                && user.email_verified
                                && r.get("email_verified") == &Value::Bool(true) =>
                        {
                            audit = Some("identity.auto_linked");
                            Some(r)
                        }
                        // Never match on an unverified (or untrusted) email.
                        Some(_) => {
                            return Ok(Err(Failure::new(
                                "email_in_use",
                                "an account with this email exists; sign in and link the provider",
                            )));
                        }
                        // Rule 4: sign up.
                        None if opts.signup_enabled => {
                            audit = Some("identity.link");
                            Some(system_create(tx, &w, &c2, &signup)?)
                        }
                        // Rule 5.
                        None => return Ok(Err(Failure::new("account_not_found", "sign-up is disabled"))),
                    }
                };
                let Some(record) = record else {
                    return Ok(Err(Failure::new("account_not_found", "user is gone")));
                };
                let user_id = record.id.to_string();
                if found.is_none() {
                    tx.identity_insert(&IdentityRow {
                        id: Uuid::now_v7().to_string(),
                        collection_id: cid.clone(),
                        user_id: user_id.clone(),
                        provider_id: p2.id.clone(),
                        issuer: user.issuer.clone(),
                        subject: user.subject.clone(),
                        email_at_link: user.email.clone(),
                        created: now.clone(),
                        last_login: Some(now.clone()),
                    })?;
                }
                if let Some(action) = audit {
                    audit_auth(
                        tx,
                        &Subject::user(&c2, &record).actor(&ctx2),
                        action,
                        ctx2.now,
                        json!({ "provider": p2.name, "issuer": user.issuer, "subject": user.subject, "collection": c2.name }),
                    )?;
                }
                // The provider vouches for the address it verified.
                if user.email_verified
                    && user.email.is_some()
                    && record.get("email").as_str() == user.email.as_deref()
                    && record.get("email_verified") != &Value::Bool(true)
                {
                    let mut changes = Map::new();
                    changes.insert("email_verified".into(), Value::Bool(true));
                    system_update(tx, &w, &c2, &record, changes)?;
                }
                tx.token_insert(&TokenRow {
                    hash: exchange_hash,
                    purpose: TokenPurpose::Exchange,
                    collection_id: cid,
                    user_id,
                    payload: Some(json!({ "provider": p2.name }).to_string()),
                    expires: format_timestamp(ctx2.now + EXCHANGE_TTL),
                    used_at: None,
                })?;
                Ok(Ok(w.take_events()))
            })
            .await;
        match res {
            Ok(Ok(events)) => {
                self.hub.publish(events);
                Ok(())
            }
            Ok(Err(f)) => Err(f),
            Err(Error::Conflict(m)) => Err(Failure::new("email_in_use", m)),
            Err(e) => Err(Failure::new("server_error", e.to_string())),
        }
    }

    /// `POST /api/collections/{c}/auth-with-exchange-code {code}`.
    pub async fn auth_with_exchange_code(
        &self,
        ctx: &RequestCtx,
        key: &str,
        code: &str,
    ) -> Result<Tokens> {
        let c = self.auth_collection(key)?;
        let (hash, ctx2) = (hash_token(code), ctx.clone());
        let out = self
            .store
            .write(move |tx| {
                let Some(row) = tx
                    .token_take(&hash, TokenPurpose::Exchange, &format_timestamp(ctx2.now))?
                    .filter(|t| t.collection_id == c.id.to_string())
                else {
                    return Err(Error::Unauthorized);
                };
                let record = tx
                    .record_get(&c, &row.user_id)?
                    .ok_or(Error::Unauthorized)?;
                start_session(tx, &Subject::user(&c, &record), &ctx2, "provider")
            })
            .await;
        self.observe.counters.login("provider", out.is_ok());
        out
    }

    // Identities -----------------------------------------------------------

    fn own_identity_user(&self, ctx: &RequestCtx, c: &Collection) -> Result<String> {
        match &ctx.principal {
            Principal::User {
                id, collection_id, ..
            } if *collection_id == c.id.to_string() => Ok(id.clone()),
            Principal::Anonymous => Err(Error::Unauthorized),
            _ => Err(Error::Forbidden(
                "Sign in as a user of this collection.".into(),
            )),
        }
    }

    /// `GET /api/collections/{c}/identities`.
    pub async fn identities_list(&self, ctx: &RequestCtx, key: &str) -> Result<Vec<Value>> {
        let c = self.auth_collection(key)?;
        let uid = self.own_identity_user(ctx, &c)?;
        let rows = self.store.read(move |tx| tx.identities_for(&uid)).await?;
        let providers = self.providers.get();
        Ok(rows
            .iter()
            .map(|i| {
                let provider = providers
                    .iter()
                    .find(|p| p.id == i.provider_id)
                    .map(|p| p.name.clone());
                json!({
                    "id": i.id,
                    "provider": provider,
                    "issuer": i.issuer,
                    "email": i.email_at_link,
                    "created": i.created,
                    "last_login": i.last_login,
                })
            })
            .collect())
    }

    /// `DELETE /api/collections/{c}/identities/{id}`: refused when it is the
    /// last way to sign in.
    pub async fn identity_unlink(&self, ctx: &RequestCtx, key: &str, id: &str) -> Result<()> {
        let c = self.auth_collection(key)?;
        let uid = self.own_identity_user(ctx, &c)?;
        let opts = c.auth().cloned().unwrap_or_default();
        let email_login = self.smtp_configured() && (opts.methods.otp || opts.methods.magic_link);
        let (c2, id2, ctx2) = (Arc::clone(&c), id.to_owned(), ctx.clone());
        self.store
            .write(move |tx| {
                let identities = tx.identities_for(&uid)?;
                if !identities.iter().any(|i| i.id == id2) {
                    return Err(Error::NotFound);
                }
                let record = tx.record_get(&c2, &uid)?.ok_or(Error::NotFound)?;
                let has_password =
                    opts.methods.password && tx.auth_password_hash(&c2, &uid)?.is_some();
                let has_passkey = opts.methods.passkey && !tx.passkeys_for(&uid)?.is_empty();
                let has_email = email_login && record.get("email").as_str().is_some();
                if identities.len() < 2 && !has_password && !has_passkey && !has_email {
                    return Err(Error::Conflict(
                        "This is your only way to sign in. Add a password or a passkey first."
                            .into(),
                    ));
                }
                tx.identity_delete(&uid, &id2)?;
                audit_auth(
                    tx,
                    &ctx2.actor(),
                    "identity.unlink",
                    ctx2.now,
                    json!({ "identity": id2, "collection": c2.name }),
                )
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_parse() {
        let p = presets();
        let names: Vec<&str> = p
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v["preset"].as_str())
            .collect();
        for n in [
            "google",
            "microsoft",
            "apple",
            "gitlab",
            "authentik",
            "pocket-id",
            "keycloak",
            "zitadel",
            "github",
        ] {
            assert!(names.contains(&n), "{n}");
        }
    }

    #[test]
    fn issuers() {
        assert!(valid_issuer("https://accounts.google.com", false));
        assert!(valid_issuer("http://127.0.0.1:8080/realms/x", false));
        assert!(!valid_issuer("http://idp.example.com", false));
        assert!(valid_issuer("http://idp.example.com", true));
        assert!(!valid_issuer("ftp://x", true));
    }

    #[test]
    fn names() {
        assert!(valid_name("google"));
        assert!(valid_name("pocket-id"));
        assert!(!valid_name("Google"));
        assert!(!valid_name("-x"));
        assert!(!valid_name(""));
    }
}
