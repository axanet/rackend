//! Settings use cases (SPEC §15). Every change is audited.

use serde_json::{Value, json};

use super::audit::entry;
use super::{App, RequestCtx};
use crate::ports::{SettingRow, StoreExt};
use crate::time::format_timestamp;
use crate::{Error, Result};

impl App {
    /// All settings; secrets are masked (SPEC §15).
    pub fn settings_get(&self, ctx: &RequestCtx) -> Result<Value> {
        ctx.require_superuser()?;
        let mut v = self.settings().to_json();
        crate::settings::mask_secrets(&mut v);
        Ok(v)
    }

    /// Merges a partial settings object, validates it and stores changed
    /// sections in one audited transaction.
    pub async fn settings_patch(&self, ctx: &RequestCtx, patch: Value) -> Result<Value> {
        self.settings_patch_with(ctx, patch, false).await
    }

    /// Like [`App::settings_patch`]; `force` allows switching the storage
    /// backend while files exist (they are not moved, SPEC §12.6).
    pub async fn settings_patch_with(
        &self,
        ctx: &RequestCtx,
        patch: Value,
        force: bool,
    ) -> Result<Value> {
        ctx.require_superuser()?;
        let current = self.settings();
        let mut sections = current.validate_patch(&patch).map_err(Error::Validation)?;
        let storage_changed = sections.iter().any(|(k, _)| k == "storage");
        let old_backend = current.string("storage", "backend");
        if let Some((_, m)) = sections.iter().find(|(k, _)| k == "storage")
            && m.get("backend").and_then(Value::as_str) != old_backend.as_deref()
            && !force
        {
            let (count, _) = self.store.read(|tx| tx.files_usage()).await?;
            if count > 0 {
                let files = if count == 1 {
                    "1 file is".to_owned()
                } else {
                    format!("{count} files are")
                };
                return Err(Error::Conflict(format!(
                    "{files} stored in the current backend and would not be moved. Copy them yourself, then retry with ?force=true."
                )));
            }
        }
        // Secrets are stored encrypted.
        for (key, merged) in &mut sections {
            for path in crate::settings::SECRET_PATHS {
                let mut parts = path.split('.');
                if parts.next() != Some(key.as_str()) {
                    continue;
                }
                let rest: Vec<&str> = parts.collect();
                let mut cur: Option<&mut Value> = None;
                for (i, p) in rest.iter().enumerate() {
                    let next = if i == 0 {
                        merged.get_mut(*p)
                    } else {
                        cur.and_then(|c| c.get_mut(*p))
                    };
                    cur = next;
                }
                if let Some(v) = cur
                    && let Some(plain) = v
                        .as_str()
                        .filter(|s| !s.is_empty() && !crate::secrets::is_encrypted(s))
                {
                    *v = Value::String(self.secrets.encrypt_str(plain));
                }
            }
        }
        let (actor, now) = (ctx.actor(), ctx.now);
        let before = current.to_json();
        self.store
            .write(move |tx| {
                let mut diff = serde_json::Map::new();
                for (key, mut merged) in sections {
                    if key == "app"
                        && let Some(Value::String(u)) = merged.get("public_url")
                    {
                        let normalized = crate::settings::normalize_public_url(u)
                            .map_err(|e| Error::field("app.public_url", e))?;
                        merged.insert("public_url".into(), Value::String(normalized));
                    }
                    let mut masked_before =
                        json!({ &key: before.get(&key).cloned().unwrap_or(Value::Null) });
                    let mut masked_after = json!({ &key: Value::Object(merged.clone()) });
                    let raw_after = masked_after.clone();
                    crate::settings::mask_secrets(&mut masked_before);
                    crate::settings::mask_secrets(&mut masked_after);
                    for (k, v) in masked_after[&key].as_object().into_iter().flatten() {
                        let old = masked_before[&key].get(k).cloned().unwrap_or(Value::Null);
                        if &old != v {
                            diff.insert(format!("{key}.{k}"), json!({ "before": old, "after": v }));
                        }
                    }
                    for path in crate::settings::SECRET_PATHS
                        .iter()
                        .filter(|p| p.starts_with(&format!("{key}.")))
                    {
                        let ptr = format!("/{}", path.replace('.', "/"));
                        let (b, a) = (before.pointer(&ptr), raw_after.pointer(&ptr));
                        if b != a {
                            diff.insert((*path).to_owned(), json!({ "changed": true }));
                        }
                    }
                    let is_secret = crate::settings::section_has_secrets(&key);
                    tx.setting_put(&SettingRow {
                        key,
                        value: Value::Object(merged).to_string(),
                        is_secret,
                        updated: format_timestamp(now),
                    })?;
                }
                let mut e = entry(&actor, "settings.update", now);
                e.diff = Some(Value::Object(diff));
                tx.audit_insert(&e)
            })
            .await?;
        self.reload_settings().await?;
        if storage_changed {
            self.rebuild_blobs();
        }
        let mut v = self.settings().to_json();
        crate::settings::mask_secrets(&mut v);
        Ok(v)
    }
}

impl App {
    /// Number of superusers (for `needs_setup`).
    pub async fn store_superusers_count(&self) -> Result<u64> {
        self.store.read(|tx| tx.superusers_count()).await
    }

    /// Audit entries (SPEC §14.1).
    pub async fn audit_list(
        &self,
        ctx: &RequestCtx,
        q: crate::ports::AuditQuery,
    ) -> Result<(Vec<crate::domain::AuditEntry>, u64)> {
        ctx.require_superuser()?;
        self.store.read(move |tx| tx.audit_query(&q)).await
    }
}
