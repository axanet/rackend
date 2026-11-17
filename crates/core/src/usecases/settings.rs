//! Settings use cases (SPEC §15). Every change is audited.

use serde_json::{Value, json};

use super::audit::entry;
use super::{App, RequestCtx};
use crate::ports::{SettingRow, StoreExt};
use crate::time::format_timestamp;
use crate::{Error, Result};

impl App {
    /// All settings (secrets would be masked; none are writable before M2).
    pub fn settings_get(&self, ctx: &RequestCtx) -> Result<Value> {
        ctx.require_superuser()?;
        Ok(self.settings().to_json())
    }

    /// Merges a partial settings object, validates it and stores changed
    /// sections in one audited transaction.
    pub async fn settings_patch(&self, ctx: &RequestCtx, patch: Value) -> Result<Value> {
        ctx.require_superuser()?;
        let current = self.settings();
        let sections = current.validate_patch(&patch).map_err(Error::Validation)?;
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
                    for (k, v) in &merged {
                        let old = before
                            .get(&key)
                            .and_then(|s| s.get(k))
                            .cloned()
                            .unwrap_or(Value::Null);
                        if &old != v {
                            diff.insert(format!("{key}.{k}"), json!({ "before": old, "after": v }));
                        }
                    }
                    tx.setting_put(&SettingRow {
                        key,
                        value: Value::Object(merged).to_string(),
                        is_secret: false,
                        updated: format_timestamp(now),
                    })?;
                }
                let mut e = entry(&actor, "settings.update", now);
                e.diff = Some(Value::Object(diff));
                tx.audit_insert(&e)
            })
            .await?;
        self.reload_settings().await?;
        Ok(self.settings().to_json())
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
