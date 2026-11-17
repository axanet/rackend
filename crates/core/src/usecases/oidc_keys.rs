//! Signing keys of the OIDC provider (SPEC §11.8.6): an in-memory key set
//! for signing and verification, the JWKS, scheduled rotation and
//! emergency rotation.
//!
//! Each algorithm has at most one active key (the newest one whose
//! `activated_at` has passed and that is not retired). A new key is created
//! 7 days before the active one is 90 days old, so relying parties see it in
//! the JWKS before it signs anything. Retired keys stay in the JWKS for
//! `max(longest refresh lifetime, 7 days)`, then they are deleted.

use std::sync::{Arc, RwLock};

use serde_json::{Value, json};
use time::{Duration, OffsetDateTime};

use super::App;
use super::RequestCtx;
use super::audit::entry;
use crate::auth::jwt::KeyMaterial;
use crate::auth::keys::{KeyAlg, KeyRow, generate};
use crate::domain::Actor;
use crate::ports::StoreExt as _;
use crate::time::format_timestamp;
use crate::{Error, Result};

/// Age at which a key is replaced.
pub const ROTATE_AFTER: Duration = Duration::days(90);
/// How long a new key is published before it signs.
pub const PUBLISH_BEFORE: Duration = Duration::days(7);

/// A key with its decrypted material.
#[derive(Debug, Clone)]
pub struct KeyEntry {
    /// The stored row.
    pub row: KeyRow,
    /// Signing and verification key.
    pub material: KeyMaterial,
}

/// All keys of the instance.
#[derive(Debug, Default)]
pub struct KeySet {
    entries: Vec<KeyEntry>,
}

fn parse_ts(s: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok()
}

fn alg_of(s: &str) -> Option<KeyAlg> {
    match s {
        "RS256" => Some(KeyAlg::Rs256),
        "ES256" => Some(KeyAlg::Es256),
        _ => None,
    }
}

impl KeySet {
    /// The key that signs with `alg` now.
    pub fn active(&self, alg: &str, now: OffsetDateTime) -> Option<&KeyEntry> {
        self.entries
            .iter()
            .filter(|e| e.row.alg == alg && e.row.retired_at.is_none())
            .filter(|e| {
                e.row
                    .activated_at
                    .as_deref()
                    .and_then(parse_ts)
                    .is_some_and(|t| t <= now)
            })
            .max_by(|a, b| a.row.activated_at.cmp(&b.row.activated_at))
    }

    /// The key with this id (any published key verifies).
    pub fn find(&self, kid: &str) -> Option<&KeyMaterial> {
        self.entries
            .iter()
            .find(|e| e.row.kid == kid)
            .map(|e| &e.material)
    }

    /// `{"keys": [...]}` with every stored key: upcoming, active and retired.
    pub fn jwks(&self) -> Value {
        let keys: Vec<Value> = self
            .entries
            .iter()
            .filter_map(|e| serde_json::from_str::<Value>(&e.row.public_jwk).ok())
            .collect();
        json!({ "keys": keys })
    }

    /// Whether no key can sign.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// The key set, shared and replaced as a whole.
#[derive(Debug, Default)]
pub struct KeyCache(RwLock<Arc<KeySet>>);

impl KeyCache {
    /// The current set.
    pub fn get(&self) -> Arc<KeySet> {
        self.0
            .read()
            .map_or_else(|p| Arc::clone(&p.into_inner()), |g| Arc::clone(&g))
    }

    fn set(&self, s: KeySet) {
        let s = Arc::new(s);
        match self.0.write() {
            Ok(mut g) => *g = s,
            Err(p) => *p.into_inner() = s,
        }
    }
}

impl App {
    /// Re-reads and decrypts the signing keys.
    pub async fn reload_keys(&self) -> Result<()> {
        let rows = self.store.read(|tx| tx.keys_list()).await?;
        let mut entries = Vec::new();
        for row in rows {
            let Some(alg) = alg_of(&row.alg) else {
                continue;
            };
            let material = self
                .secrets
                .decrypt(&row.private_enc)
                .map_err(|e| {
                    Error::internal(format!("cannot decrypt signing key {}: {e}", row.kid))
                })
                .and_then(|der| KeyMaterial::from_pkcs8(alg, &der).map_err(Error::internal))?;
            entries.push(KeyEntry { row, material });
        }
        self.keys.set(KeySet { entries });
        Ok(())
    }

    /// How long retired keys stay in the JWKS.
    fn key_retention(&self) -> Duration {
        let compiled = self.schema.get();
        let longest = compiled
            .schema
            .all()
            .iter()
            .filter_map(|c| c.auth().map(|o| o.refresh_ttl_s))
            .max()
            .unwrap_or(0);
        Duration::seconds(longest).max(Duration::days(7))
    }

    async fn new_key(
        &self,
        alg: KeyAlg,
        activated_at: OffsetDateTime,
        now: OffsetDateTime,
    ) -> Result<KeyRow> {
        let key = self.blocking(move || generate(alg)).await??;
        Ok(KeyRow {
            kid: key.kid,
            alg: alg.as_str().to_owned(),
            private_enc: self.secrets.encrypt(&key.private_pkcs8),
            public_jwk: key.public_jwk.to_string(),
            created: format_timestamp(now),
            activated_at: Some(format_timestamp(activated_at)),
            retired_at: None,
            delete_after: None,
        })
    }

    /// The daily rotation job: publishes the next key 7 days ahead, retires
    /// replaced keys and deletes expired ones. Returns whether anything changed.
    pub async fn job_key_rotation(&self) -> Result<bool> {
        let now = self.clock.now();
        let rows = self.store.read(|tx| tx.keys_list()).await?;
        let retention = self.key_retention();
        let mut inserts = Vec::new();
        let mut updates = Vec::new();
        let mut deletes = Vec::new();
        for alg in [KeyAlg::Rs256, KeyAlg::Es256] {
            let mut live: Vec<&KeyRow> = rows
                .iter()
                .filter(|k| k.alg == alg.as_str() && k.retired_at.is_none())
                .collect();
            live.sort_by(|a, b| a.activated_at.cmp(&b.activated_at));
            let started = |k: &KeyRow| k.activated_at.as_deref().and_then(parse_ts);
            // Keys replaced by a newer active key are retired.
            let active: Vec<&&KeyRow> = live
                .iter()
                .filter(|k| started(k).is_some_and(|t| t <= now))
                .collect();
            if let Some((newest, older)) = active.split_last() {
                for k in older {
                    let mut k = (**k).clone();
                    let at = started(newest).unwrap_or(now);
                    k.retired_at = Some(format_timestamp(at));
                    k.delete_after = Some(format_timestamp(at + retention));
                    updates.push(k);
                }
                let pending = live.iter().any(|k| started(k).is_some_and(|t| t > now));
                let age = started(newest).map_or(Duration::ZERO, |t| now - t);
                if !pending && age >= ROTATE_AFTER - PUBLISH_BEFORE {
                    let at =
                        (started(newest).unwrap_or(now) + ROTATE_AFTER).max(now + PUBLISH_BEFORE);
                    inserts.push(self.new_key(alg, at, now).await?);
                }
            }
        }
        for k in &rows {
            if k.delete_after
                .as_deref()
                .and_then(parse_ts)
                .is_some_and(|t| t <= now)
            {
                deletes.push(k.kid.clone());
            }
        }
        if inserts.is_empty() && updates.is_empty() && deletes.is_empty() {
            return Ok(false);
        }
        self.store
            .write(move |tx| {
                for k in &inserts {
                    tx.key_insert(k)?;
                    let mut e = entry(&Actor::system(), "keys.generate", now);
                    e.meta =
                        Some(json!({ "kid": k.kid, "alg": k.alg, "activated_at": k.activated_at }));
                    tx.audit_insert(&e)?;
                }
                for k in &updates {
                    tx.key_update(k)?;
                    let mut e = entry(&Actor::system(), "keys.retire", now);
                    e.meta =
                        Some(json!({ "kid": k.kid, "alg": k.alg, "delete_after": k.delete_after }));
                    tx.audit_insert(&e)?;
                }
                for kid in &deletes {
                    tx.key_delete(kid)?;
                    let mut e = entry(&Actor::system(), "keys.delete", now);
                    e.meta = Some(json!({ "kid": kid }));
                    tx.audit_insert(&e)?;
                }
                Ok(())
            })
            .await?;
        self.reload_keys().await?;
        Ok(true)
    }

    /// `POST /api/keys/rotate`: replaces the active keys now (emergency).
    /// The old keys stay in the JWKS until their tokens can have expired.
    pub async fn keys_rotate(&self, ctx: &RequestCtx) -> Result<Value> {
        ctx.require_superuser()?;
        let now = ctx.now;
        let retention = self.key_retention();
        let rows = self.store.read(|tx| tx.keys_list()).await?;
        let mut fresh = Vec::new();
        for alg in [KeyAlg::Rs256, KeyAlg::Es256] {
            fresh.push(self.new_key(alg, now, now).await?);
        }
        let retire: Vec<KeyRow> = rows
            .into_iter()
            .filter(|k| k.retired_at.is_none())
            .map(|mut k| {
                k.retired_at = Some(format_timestamp(now));
                k.delete_after = Some(format_timestamp(now + retention));
                k
            })
            .collect();
        let actor = ctx.actor();
        let kids: Vec<String> = fresh.iter().map(|k| k.kid.clone()).collect();
        self.store
            .write(move |tx| {
                for k in &retire {
                    tx.key_update(k)?;
                }
                for k in &fresh {
                    tx.key_insert(k)?;
                }
                let mut e = entry(&actor, "keys.rotate", now);
                e.meta = Some(json!({
                    "new": fresh.iter().map(|k| json!({ "kid": k.kid, "alg": k.alg })).collect::<Vec<_>>(),
                    "retired": retire.iter().map(|k| k.kid.clone()).collect::<Vec<_>>(),
                }));
                tx.audit_insert(&e)
            })
            .await?;
        self.reload_keys().await?;
        Ok(json!({ "rotated": kids }))
    }

    /// `GET /api/keys`: the keys and their lifecycle, without private parts.
    pub async fn keys_overview(&self, ctx: &RequestCtx) -> Result<Value> {
        ctx.require_superuser()?;
        let rows = self.store.read(|tx| tx.keys_list()).await?;
        let now = ctx.now;
        let set = self.keys.get();
        let items: Vec<Value> = rows
            .iter()
            .map(|k| {
                let active = set.active(&k.alg, now).is_some_and(|a| a.row.kid == k.kid);
                let state = if active {
                    "active"
                } else if k.retired_at.is_some() {
                    "retired"
                } else {
                    "upcoming"
                };
                json!({
                    "kid": k.kid, "alg": k.alg, "state": state, "created": k.created,
                    "activated_at": k.activated_at, "retired_at": k.retired_at, "delete_after": k.delete_after,
                })
            })
            .collect();
        Ok(json!({ "items": items }))
    }
}
