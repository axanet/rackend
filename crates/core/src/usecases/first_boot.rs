//! First boot (SPEC §5.5) as idempotent "ensure" steps, so instances created
//! by earlier builds get whatever is missing on upgrade (ADR 0008).

use serde_json::json;

use uuid::Uuid;

use super::App;
use super::audit::entry;
use crate::Result;
use crate::auth::keys::{KeyAlg, KeyRow, generate};
use crate::domain::{Actor, Collection};
use crate::ports::StoreExt;
use crate::time::format_timestamp;

/// Definition of the default `users` auth collection (SPEC §8.6).
fn default_users() -> serde_json::Value {
    json!({
        "name": "users",
        "type": "auth",
        "fields": [{ "name": "name", "type": "text", "max": 200 }],
        "rules": {
            "list": "id = @request.auth.id",
            "view": "id = @request.auth.id",
            "create": "",
            "update": "id = @request.auth.id",
            "delete": "id = @request.auth.id"
        }
    })
}

impl App {
    /// Runs the first-boot steps that are still missing. Returns true when
    /// this was the first boot of the instance.
    pub async fn first_boot(&self) -> Result<bool> {
        let now = self.clock.now();
        let first = self
            .store
            .write(move |tx| {
                if tx.meta_get("instance_id")?.is_some() {
                    return Ok(false);
                }
                let id = Uuid::now_v7().to_string();
                tx.meta_set("instance_id", &id)?;
                let mut e = entry(&Actor::system(), "instance.init", now);
                e.meta = Some(json!({ "instance_id": id }));
                tx.audit_insert(&e)?;
                Ok(true)
            })
            .await?;
        if first && self.schema.get().schema.by_name("users").is_none() {
            let ctx = super::RequestCtx::system(
                super::Principal::Superuser {
                    id: "system".into(),
                    email: "system".into(),
                    session_id: String::new(),
                },
                now,
            );
            let def = default_users();
            debug_assert!(Collection::from_input(&def, None).is_ok());
            self.collection_create(&ctx, def, false).await?;
        }
        Ok(first)
    }

    /// Generates the OIDC signing keys (RS256 + ES256) if none exist. RSA
    /// generation takes a moment, so the binary runs this in the background.
    pub async fn ensure_signing_keys(&self) -> Result<()> {
        let existing = self.store.read(|tx| tx.keys_list()).await?;
        for alg in [KeyAlg::Rs256, KeyAlg::Es256] {
            if existing
                .iter()
                .any(|k| k.alg == alg.as_str() && k.retired_at.is_none())
            {
                continue;
            }
            let key = self.blocking(move || generate(alg)).await??;
            let now = self.clock.now();
            let row = KeyRow {
                kid: key.kid.clone(),
                alg: alg.as_str().to_owned(),
                private_enc: self.secrets.encrypt(&key.private_pkcs8),
                public_jwk: key.public_jwk.to_string(),
                created: format_timestamp(now),
                activated_at: Some(format_timestamp(now)),
                retired_at: None,
                delete_after: None,
            };
            self.store
                .write(move |tx| {
                    tx.key_insert(&row)?;
                    let mut e = entry(&Actor::system(), "keys.generate", now);
                    e.meta = Some(json!({ "kid": row.kid, "alg": row.alg }));
                    tx.audit_insert(&e)
                })
                .await?;
            tracing::info!(alg = alg.as_str(), kid = %key.kid, "generated a signing key");
        }
        self.reload_keys().await
    }
}
