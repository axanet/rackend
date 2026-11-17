//! Key management for secrets at rest (SPEC §17.4).
//!
//! - With `RACKEND_ENCRYPTION_KEY`, that key is used. If secrets were
//!   encrypted with the auto-generated key stored in `_meta`, they are
//!   re-encrypted with the env key in one audited transaction and the stored
//!   key is deleted.
//! - Without it, a key is generated once and stored in `_meta.local_secret_key`.
//! - `_meta.secret_check` holds a known value encrypted with the current key,
//!   so a wrong or missing env key is detected at startup instead of
//!   corrupting secrets. Recovery needs `RACKEND_RESET_SECRETS=true`, which
//!   discards the unreadable secrets (ADR 0011).

use serde_json::json;
use time::OffsetDateTime;

use super::audit::entry;
use crate::domain::Actor;
use crate::ports::{SettingRow, Store, StoreExt, WriteTx};
use crate::secrets::{SecretBox, is_encrypted};

const LOCAL_KEY: &str = "local_secret_key";
const CHECK: &str = "secret_check";

/// Why secrets could not be initialized.
#[derive(Debug, thiserror::Error)]
pub enum SecretsInitError {
    /// The configured key cannot decrypt the existing secrets.
    #[error(
        "RACKEND_ENCRYPTION_KEY does not match the key the secrets were encrypted with. Set the original key, or start once with RACKEND_RESET_SECRETS=true to discard the stored secrets (SMTP and S3 passwords, provider secrets, signing keys)."
    )]
    WrongKey,
    /// Secrets were encrypted with an env key that is no longer set.
    #[error(
        "the secrets were encrypted with RACKEND_ENCRYPTION_KEY, which is not set. Set it again, or start once with RACKEND_RESET_SECRETS=true to discard the stored secrets."
    )]
    MissingKey,
    /// A storage error.
    #[error(transparent)]
    Store(#[from] crate::Error),
}

/// Decides the key, migrates or verifies existing secrets, and returns the box.
pub async fn init(
    store: &dyn Store,
    env_key: Option<[u8; 32]>,
    reset: bool,
    now: OffsetDateTime,
) -> Result<SecretBox, SecretsInitError> {
    let outcome = store
        .write(move |tx| {
            let local = tx
                .meta_get(LOCAL_KEY)?
                .and_then(|k| SecretBox::parse_key(&k).ok());
            let check = tx.meta_get(CHECK)?;
            let stored_box = local.map(|k| SecretBox::new(&k));

            let (key, migrate_from) = match (env_key, &stored_box) {
                (Some(env), Some(old)) => (env, Some(old.clone())),
                (Some(env), None) => (env, None),
                (None, Some(_)) => (local.unwrap_or_default(), None),
                (None, None) => {
                    if check.is_some() && !reset {
                        return Ok(Err(false));
                    }
                    let k = SecretBox::generate_key();
                    tx.meta_set(LOCAL_KEY, &SecretBox::encode_key(&k))?;
                    (k, None)
                }
            };
            let current = SecretBox::new(&key);

            if let Some(old) = migrate_from {
                reencrypt(tx, &old, &current)?;
                tx.meta_set(LOCAL_KEY, "")?;
                tx.meta_set(CHECK, &current.check_value())?;
                let mut e = entry(&Actor::system(), "secrets.rekey", now);
                e.meta = Some(json!({ "to": "RACKEND_ENCRYPTION_KEY" }));
                tx.audit_insert(&e)?;
                return Ok(Ok(current));
            }

            match &check {
                Some(c) if current.verify_check(c) => {}
                Some(_) if !reset => return Ok(Err(true)),
                Some(_) => {
                    discard(tx)?;
                    tx.audit_insert(&entry(&Actor::system(), "secrets.reset", now))?;
                }
                None => {}
            }
            tx.meta_set(CHECK, &current.check_value())?;
            Ok(Ok(current))
        })
        .await?;
    outcome.map_err(|wrong| {
        if wrong {
            SecretsInitError::WrongKey
        } else {
            SecretsInitError::MissingKey
        }
    })
}

/// Re-encrypts every secret with a new key.
fn reencrypt(tx: &mut dyn WriteTx, old: &SecretBox, new: &SecretBox) -> crate::Result<()> {
    for row in tx.settings_list()?.into_iter().filter(|r| r.is_secret) {
        tx.setting_put(&SettingRow {
            value: reencrypt_json(&row.value, old, new)?,
            ..row
        })?;
    }
    for k in tx.keys_list()? {
        let plain = old
            .decrypt(&k.private_enc)
            .map_err(crate::Error::internal)?;
        tx.key_set_private(&k.kid, &new.encrypt(&plain))?;
    }
    Ok(())
}

/// Re-encrypts the encrypted string members of a settings JSON object.
fn reencrypt_json(value: &str, old: &SecretBox, new: &SecretBox) -> crate::Result<String> {
    let mut v: serde_json::Value = serde_json::from_str(value).map_err(crate::Error::internal)?;
    fn walk(v: &mut serde_json::Value, old: &SecretBox, new: &SecretBox) -> crate::Result<()> {
        match v {
            serde_json::Value::String(s) if is_encrypted(s) => {
                let plain = old.decrypt(s).map_err(crate::Error::internal)?;
                *s = new.encrypt(&plain);
            }
            serde_json::Value::Object(m) => {
                for x in m.values_mut() {
                    walk(x, old, new)?;
                }
            }
            serde_json::Value::Array(a) => {
                for x in a {
                    walk(x, old, new)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    walk(&mut v, old, new)?;
    Ok(v.to_string())
}

/// Drops secrets that can no longer be decrypted.
fn discard(tx: &mut dyn WriteTx) -> crate::Result<()> {
    for row in tx.settings_list()?.into_iter().filter(|r| r.is_secret) {
        tx.setting_put(&SettingRow {
            value: "{}".into(),
            ..row
        })?;
    }
    tx.keys_delete_all()?;
    Ok(())
}
