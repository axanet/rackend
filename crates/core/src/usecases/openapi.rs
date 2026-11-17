//! `GET /api/openapi.json` (SPEC §10.12).

use serde_json::Value;

use super::{App, RequestCtx};
use crate::Result;
use crate::openapi::{DocInput, ProviderRef, Version};

impl App {
    /// The OpenAPI document. Superusers only, unless `api.openapi_public`
    /// is on (then without operations only superusers may call); the admin
    /// endpoints (`include_admin`) are always superuser only.
    pub fn openapi_document(
        &self,
        ctx: &RequestCtx,
        version: Version,
        include_admin: bool,
    ) -> Result<Value> {
        let settings = self.settings();
        let superuser = ctx.is_superuser();
        if include_admin || !settings.bool("api", "openapi_public", false) {
            ctx.require_superuser()?;
        }
        let compiled = self.schema.get();
        let providers = self
            .providers
            .get()
            .iter()
            .filter(|p| p.enabled)
            .map(|p| ProviderRef {
                collection_id: p.collection_id.clone(),
                name: p.name.clone(),
            })
            .collect();
        Ok(crate::openapi::generate(&DocInput {
            schema: &compiled.schema,
            version,
            include_admin,
            superuser_view: superuser,
            app_name: settings
                .string("app", "name")
                .unwrap_or_else(|| "rackend".into()),
            app_version: self.version.to_owned(),
            public_url: settings.public_url(),
            providers,
        }))
    }
}
