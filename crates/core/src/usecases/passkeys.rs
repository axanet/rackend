//! Passkeys (SPEC §11.3) for auth-collection users and superusers:
//! registration while signed in, and discoverable (usernameless) login.
//! Ceremony state lives in memory for 5 minutes (single instance).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration as StdDuration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use uuid::Uuid;
use webauthn_rs::prelude::{
    DiscoverableAuthentication, Passkey, PasskeyRegistration, PublicKeyCredential,
    RegisterPublicKeyCredential, Url, Webauthn, WebauthnBuilder,
};

use super::auth::{Tokens, audit_auth};
use super::sessions::{Subject, start_session};
use super::{App, Principal, RequestCtx};
use crate::auth::token::random_token;
use crate::auth::{PasskeyRow, SubjectType};
use crate::ports::StoreExt;
use crate::time::format_timestamp;
use crate::{Error, Result};

/// Ceremony lifetime.
const CEREMONY_TTL: StdDuration = StdDuration::from_secs(300);
/// Max open ceremonies (bounds memory).
const CEREMONY_MAX: usize = 10_000;

/// Where a passkey belongs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasskeyScope {
    /// `/api/superusers/passkeys/...`
    Superuser,
    /// `/api/collections/{c}/passkeys/...` (collection id or name).
    Collection(String),
}

#[derive(Debug)]
enum Ceremony {
    Register {
        user_id: String,
        collection_id: Option<String>,
        kind: SubjectType,
        state: PasskeyRegistration,
    },
    Login {
        scope: PasskeyScope,
        state: DiscoverableAuthentication,
    },
}

/// Open ceremonies by id.
#[derive(Debug, Default)]
pub struct Ceremonies(Mutex<HashMap<String, (Ceremony, Instant)>>);

impl Ceremonies {
    fn put(&self, c: Ceremony) -> String {
        let id = random_token();
        if let Ok(mut m) = self.0.lock() {
            m.retain(|_, (_, at)| at.elapsed() < CEREMONY_TTL);
            if m.len() < CEREMONY_MAX {
                m.insert(id.clone(), (c, Instant::now()));
            }
        }
        id
    }

    fn take(&self, id: &str) -> Option<Ceremony> {
        let mut m = self.0.lock().ok()?;
        let (c, at) = m.remove(id)?;
        (at.elapsed() < CEREMONY_TTL).then_some(c)
    }
}

fn bad(detail: &str) -> Error {
    Error::BadRequest(detail.to_owned())
}

impl App {
    /// The relying party: RP ID = host of `app.public_url`, origin = the URL.
    fn webauthn(&self) -> Result<Webauthn> {
        let settings = self.settings();
        let url = settings
            .public_url()
            .ok_or(Error::NotConfigured("public_url"))?;
        let origin = Url::parse(&url).map_err(|_| Error::NotConfigured("public_url"))?;
        let host = origin
            .host_str()
            .ok_or(Error::NotConfigured("public_url"))?
            .to_owned();
        let name = settings
            .string("app", "name")
            .unwrap_or_else(|| "rackend".into());
        WebauthnBuilder::new(&host, &origin)
            .map(|b| b.rp_name(&name))
            .and_then(WebauthnBuilder::build)
            .map_err(|e| Error::internal(format!("webauthn configuration: {e}")))
    }

    /// The signed-in owner for a scope: (kind, collection id, user id, label).
    fn passkey_owner(
        &self,
        ctx: &RequestCtx,
        scope: &PasskeyScope,
    ) -> Result<(SubjectType, Option<String>, String, String)> {
        match (scope, &ctx.principal) {
            (PasskeyScope::Superuser, Principal::Superuser { id, email, .. }) => {
                Ok((SubjectType::Superuser, None, id.clone(), email.clone()))
            }
            (
                PasskeyScope::Collection(key),
                Principal::User {
                    id,
                    collection_id,
                    fields,
                    ..
                },
            ) => {
                let c = self.auth_collection(key)?;
                if c.id.to_string() != *collection_id {
                    return Err(Error::Forbidden(
                        "Sign in as a user of this collection.".into(),
                    ));
                }
                let label = fields
                    .get("email")
                    .and_then(Value::as_str)
                    .unwrap_or(id)
                    .to_owned();
                Ok((
                    SubjectType::User,
                    Some(collection_id.clone()),
                    id.clone(),
                    label,
                ))
            }
            (_, Principal::Anonymous) => Err(Error::Unauthorized),
            _ => Err(Error::Forbidden(
                "This passkey endpoint is for another kind of account.".into(),
            )),
        }
    }

    fn auth_collection(&self, key: &str) -> Result<Arc<crate::domain::Collection>> {
        let compiled = self.schema.get();
        let c = compiled.schema.by_id_or_name(key).ok_or(Error::NotFound)?;
        let opts = c.auth().ok_or(Error::NotFound)?;
        if !opts.methods.passkey {
            return Err(Error::Forbidden(
                "Passkeys are disabled for this collection.".into(),
            ));
        }
        Ok(Arc::clone(c))
    }

    /// Starts adding a passkey to the signed-in account.
    pub async fn passkey_register_start(
        &self,
        ctx: &RequestCtx,
        scope: &PasskeyScope,
    ) -> Result<Value> {
        let webauthn = self.webauthn()?;
        let (kind, collection_id, user_id, label) = self.passkey_owner(ctx, scope)?;
        let uid = user_id.clone();
        let existing = self.store.read(move |tx| tx.passkeys_for(&uid)).await?;
        let exclude: Vec<_> = existing
            .iter()
            .filter_map(|p| serde_json::from_str::<Passkey>(&p.credential).ok())
            .map(|p| p.cred_id().clone())
            .collect();
        let unique =
            Uuid::try_parse(&user_id).map_err(|_| Error::internal("account id is not a UUID"))?;
        let (options, state) = webauthn
            .start_passkey_registration(unique, &label, &label, Some(exclude))
            .map_err(|e| Error::internal(format!("passkey registration: {e}")))?;
        let id = self.ceremonies.put(Ceremony::Register {
            user_id,
            collection_id,
            kind,
            state,
        });
        Ok(json!({ "ceremony_id": id, "options": options }))
    }

    /// Finishes adding a passkey.
    pub async fn passkey_register_finish(
        &self,
        ctx: &RequestCtx,
        scope: &PasskeyScope,
        ceremony_id: &str,
        credential: Value,
        name: Option<String>,
    ) -> Result<Value> {
        let webauthn = self.webauthn()?;
        let (_, _, owner, _) = self.passkey_owner(ctx, scope)?;
        let Some(Ceremony::Register {
            user_id,
            collection_id,
            kind,
            state,
        }) = self.ceremonies.take(ceremony_id)
        else {
            return Err(bad("Unknown or expired ceremony; start again."));
        };
        if user_id != owner {
            return Err(bad("This ceremony belongs to another account."));
        }
        let cred: RegisterPublicKeyCredential = serde_json::from_value(credential)
            .map_err(|e| bad(&format!("Invalid credential: {e}")))?;
        let passkey = webauthn
            .finish_passkey_registration(&cred, &state)
            .map_err(|e| Error::BadRequest(format!("The passkey could not be verified: {e}")))?;
        let row = PasskeyRow {
            id: Uuid::now_v7().to_string(),
            owner_type: kind,
            collection_id,
            user_id,
            credential_id: URL_SAFE_NO_PAD.encode::<&[u8]>(passkey.cred_id().as_ref()),
            credential: serde_json::to_string(&passkey).map_err(Error::internal)?,
            name: name
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| "Passkey".into()),
            created: format_timestamp(ctx.now),
            last_used: None,
        };
        let (ctx2, row2) = (ctx.clone(), row.clone());
        self.store
            .write(move |tx| {
                if tx.passkey_by_credential(&row2.credential_id)?.is_some() {
                    return Err(Error::Conflict(
                        "This passkey is already registered.".into(),
                    ));
                }
                tx.passkey_insert(&row2)?;
                audit_auth(
                    tx,
                    &ctx2.actor(),
                    "passkey.add",
                    ctx2.now,
                    json!({ "passkey": row2.id, "name": row2.name }),
                )
            })
            .await?;
        Ok(passkey_json(&row))
    }

    /// Starts a discoverable (usernameless) passkey login.
    pub fn passkey_login_start(&self, scope: &PasskeyScope) -> Result<Value> {
        if let PasskeyScope::Collection(key) = scope {
            self.auth_collection(key)?;
        }
        let webauthn = self.webauthn()?;
        let (mut options, state) = webauthn
            .start_discoverable_authentication()
            .map_err(|e| Error::internal(format!("passkey login: {e}")))?;
        // A plain "Sign in with a passkey" button, not autofill UI.
        options.mediation = None;
        let id = self.ceremonies.put(Ceremony::Login {
            scope: scope.clone(),
            state,
        });
        Ok(json!({ "ceremony_id": id, "options": options }))
    }

    /// Finishes a passkey login and starts a session.
    pub async fn passkey_login_finish(
        &self,
        ctx: &RequestCtx,
        scope: &PasskeyScope,
        ceremony_id: &str,
        credential: Value,
    ) -> Result<Tokens> {
        let webauthn = self.webauthn()?;
        let Some(Ceremony::Login {
            scope: started,
            state,
        }) = self.ceremonies.take(ceremony_id)
        else {
            return Err(bad("Unknown or expired ceremony; start again."));
        };
        if &started != scope {
            return Err(bad("This ceremony was started for another login endpoint."));
        }
        let cred: PublicKeyCredential = serde_json::from_value(credential)
            .map_err(|e| bad(&format!("Invalid credential: {e}")))?;
        let (user_uuid, cred_id) = webauthn
            .identify_discoverable_authentication(&cred)
            .map_err(|_| Error::Unauthorized)?;
        let cred_id = URL_SAFE_NO_PAD.encode(cred_id);
        let lookup = cred_id.clone();
        let row = self
            .store
            .read(move |tx| tx.passkey_by_credential(&lookup))
            .await?;
        let collection = match scope {
            PasskeyScope::Superuser => None,
            PasskeyScope::Collection(key) => Some(self.auth_collection(key)?),
        };
        let matches = |r: &PasskeyRow| {
            r.user_id == user_uuid.to_string()
                && match &collection {
                    None => r.owner_type == SubjectType::Superuser,
                    Some(c) => {
                        r.owner_type == SubjectType::User
                            && r.collection_id.as_deref() == Some(&c.id.to_string())
                    }
                }
        };
        let Some(mut row) = row.filter(matches) else {
            self.audit_passkey_failure(ctx).await?;
            return Err(Error::Unauthorized);
        };
        let mut passkey: Passkey =
            serde_json::from_str(&row.credential).map_err(Error::internal)?;
        let result = match webauthn.finish_discoverable_authentication_v2(&cred, state, &passkey) {
            Ok(r) => r,
            Err(_) => {
                self.audit_passkey_failure(ctx).await?;
                return Err(Error::Unauthorized);
            }
        };
        passkey.update_credential(&result);
        row.credential = serde_json::to_string(&passkey).map_err(Error::internal)?;
        row.last_used = Some(format_timestamp(ctx.now));
        let ctx2 = ctx.clone();
        self.store
            .write(move |tx| {
                tx.passkey_update(&row)?;
                let subject = match &collection {
                    None => tx
                        .superuser_by_id(&row.user_id)?
                        .map(|s| Subject::superuser(&s)),
                    Some(c) => tx
                        .record_get(c, &row.user_id)?
                        .map(|r| Subject::user(c, &r)),
                };
                let subject = subject.ok_or(Error::Unauthorized)?;
                start_session(tx, &subject, &ctx2, "passkey")
            })
            .await
    }

    async fn audit_passkey_failure(&self, ctx: &RequestCtx) -> Result<()> {
        let ctx = ctx.clone();
        self.store
            .write(move |tx| {
                audit_auth(
                    tx,
                    &ctx.actor(),
                    "auth.login.failure",
                    ctx.now,
                    json!({ "method": "passkey" }),
                )
            })
            .await
    }

    /// The signed-in account's passkeys.
    pub async fn passkeys_list(
        &self,
        ctx: &RequestCtx,
        scope: &PasskeyScope,
    ) -> Result<Vec<Value>> {
        let (_, _, user_id, _) = self.passkey_owner(ctx, scope)?;
        let rows = self.store.read(move |tx| tx.passkeys_for(&user_id)).await?;
        Ok(rows.iter().map(passkey_json).collect())
    }

    /// Deletes one of the signed-in account's passkeys. A user's last login
    /// method cannot be removed.
    pub async fn passkey_delete(
        &self,
        ctx: &RequestCtx,
        scope: &PasskeyScope,
        id: &str,
    ) -> Result<()> {
        let (_, collection_id, user_id, _) = self.passkey_owner(ctx, scope)?;
        let collection =
            collection_id.and_then(|c| self.schema.get().schema.by_id_or_name(&c).cloned());
        let (ctx2, id) = (ctx.clone(), id.to_owned());
        self.store
            .write(move |tx| {
                let keys = tx.passkeys_for(&user_id)?;
                if !keys.iter().any(|k| k.id == id) {
                    return Err(Error::NotFound);
                }
                if let Some(c) = &collection
                    && keys.len() == 1
                    && tx.auth_password_hash(c, &user_id)?.is_none()
                {
                    return Err(Error::Conflict(
                        "This is the account's last way to sign in; add a password or another passkey first.".into(),
                    ));
                }
                tx.passkey_delete(&user_id, &id)?;
                audit_auth(tx, &ctx2.actor(), "passkey.remove", ctx2.now, json!({ "passkey": id }))
            })
            .await
    }
}

fn passkey_json(p: &PasskeyRow) -> Value {
    json!({ "id": p.id, "name": p.name, "created": p.created, "last_used": p.last_used })
}
