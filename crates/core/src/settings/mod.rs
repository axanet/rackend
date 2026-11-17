//! Runtime settings (SPEC §15): one JSON object per key in `_settings`,
//! merged over bundled defaults.

use serde_json::{Map, Value, json};

use crate::error::{FieldError, ValidationErrors};
use crate::ports::SettingRow;

/// Defaults of every settings key. A value's type in the defaults is the
/// type it must have. `null` defaults accept a string.
pub fn defaults() -> Value {
    json!({
        "app": { "name": "rackend", "public_url": null, "logo": null, "primary_color": null, "support_email": null },
        "security": {
            "cors_origins": [], "app_redirect_uris": [], "trusted_proxies": [],
            "superuser_require_passkey": false,
            "rate_limits": { "global_ip_per_min": 600, "login_ip_per_min": 20, "login_account_per_15min": 10 }
        },
        "audit": { "retention_days": 365 },
        "logs": { "retention_days": 7, "min_status": 0, "sample_rate": 1.0 },
        "metrics": { "retention_days": 7, "prometheus_public": false, "prometheus_token": null },
        "backups": { "schedule": "0 3 * * *", "keep": 7, "include_storage": true },
        "schema": { "trash_retention_days": 30 },
        "storage": {
            "backend": "fs",
            "s3": {
                "endpoint": "", "region": "us-east-1", "bucket": "", "prefix": "",
                "access_key_id": "", "secret_access_key": null,
                "force_path_style": true, "allow_http": false, "redirect_downloads": false
            },
            "thumb_cache_max_mb": 2048
        },
        "limits": { "max_json_body_kb": 1024, "batch_max_requests": 50, "rule_timeout_ms": 2000 },
        "realtime": {
            "max_connections_per_ip": 100, "max_subscriptions_per_connection": 100,
            "max_message_kb": 64, "max_outbound_queue_kb": 1024
        }
    })
}

/// Sections that are not writable yet: their features arrive later (M6).
const UNAVAILABLE_SECTIONS: &[&str] = &["smtp", "email_templates"];

/// Settings that are secrets: stored encrypted (`enc:v1:`), shown as
/// [`MASK`], never in audit diffs (SPEC §15, §17.4).
pub const SECRET_PATHS: &[&str] = &["storage.s3.secret_access_key", "metrics.prometheus_token"];

/// How a stored secret is shown. Sending it back unchanged keeps the secret.
pub const MASK: &str = "\u{2022}\u{2022}\u{2022}\u{2022}";

/// Whether a section holds secrets.
pub fn section_has_secrets(section: &str) -> bool {
    SECRET_PATHS
        .iter()
        .any(|p| p.split('.').next() == Some(section))
}

/// Replaces set secrets with [`MASK`] (unset ones stay `null`).
pub fn mask_secrets(v: &mut Value) {
    for path in SECRET_PATHS {
        let mut cur = &mut *v;
        let mut parts = path.split('.').peekable();
        while let Some(p) = parts.next() {
            let Some(next) = cur.get_mut(p) else { break };
            if parts.peek().is_none() {
                if next.as_str().is_some_and(|s| !s.is_empty()) {
                    *next = Value::String(MASK.into());
                } else {
                    *next = Value::Null;
                }
                break;
            }
            cur = next;
        }
    }
}

/// Merges `stored` over `base` key by key, recursing into objects. Keys
/// unknown to `base` are dropped.
fn deep_merge(base: &mut Map<String, Value>, stored: Map<String, Value>) {
    for (k, v) in stored {
        match (base.get_mut(&k), v) {
            (Some(Value::Object(b)), Value::Object(s)) => deep_merge(b, s),
            (Some(slot), v) => *slot = v,
            (None, _) => {}
        }
    }
}

/// A settings section with its merged new contents.
pub type SectionUpdate = (String, Map<String, Value>);

/// The effective settings.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    values: Map<String, Value>,
}

impl Default for Settings {
    fn default() -> Self {
        Self::from_rows(&[])
    }
}

impl Settings {
    /// Defaults overlaid with stored rows. Unknown stored keys are ignored.
    pub fn from_rows(rows: &[SettingRow]) -> Self {
        let Value::Object(mut values) = defaults() else {
            return Self { values: Map::new() };
        };
        for row in rows {
            if let (Some(Value::Object(base)), Ok(Value::Object(stored))) = (
                values.get_mut(&row.key),
                serde_json::from_str::<Value>(&row.value),
            ) {
                deep_merge(base, stored);
            }
        }
        Self { values }
    }

    /// Overrides one value (environment overrides).
    pub fn set(&mut self, section: &str, key: &str, value: Value) {
        if let Some(Value::Object(s)) = self.values.get_mut(section) {
            s.insert(key.to_owned(), value);
        }
    }

    /// All settings as JSON.
    pub fn to_json(&self) -> Value {
        Value::Object(self.values.clone())
    }

    /// One section.
    pub fn section(&self, key: &str) -> Option<&Map<String, Value>> {
        self.values.get(key).and_then(Value::as_object)
    }

    fn get(&self, section: &str, key: &str) -> Option<&Value> {
        self.section(section).and_then(|s| s.get(key))
    }

    /// An integer setting.
    pub fn int(&self, section: &str, key: &str, default: i64) -> i64 {
        self.get(section, key)
            .and_then(Value::as_i64)
            .unwrap_or(default)
    }

    /// A string setting.
    pub fn string(&self, section: &str, key: &str) -> Option<String> {
        self.get(section, key)
            .and_then(Value::as_str)
            .map(str::to_owned)
    }

    /// A list of strings.
    pub fn strings(&self, section: &str, key: &str) -> Vec<String> {
        self.get(section, key)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// `app.public_url`, without a trailing slash.
    pub fn public_url(&self) -> Option<String> {
        self.string("app", "public_url")
            .map(|u| u.trim_end_matches('/').to_owned())
    }

    /// Validates a PATCH body and returns the merged sections that change.
    pub fn validate_patch(&self, patch: &Value) -> Result<Vec<SectionUpdate>, ValidationErrors> {
        let mut errs = ValidationErrors::default();
        let defaults = defaults();
        let mut out = Vec::new();
        let Some(patch) = patch.as_object() else {
            return Err(ValidationErrors::single(
                "",
                FieldError::new("invalid_type", "Must be an object."),
            ));
        };
        for (section, changes) in patch {
            if UNAVAILABLE_SECTIONS.contains(&section.as_str()) {
                errs.add(
                    section.clone(),
                    FieldError::new(
                        "not_available",
                        "This section is not available in this version yet.",
                    ),
                );
                continue;
            }
            let (Some(def), Some(current)) = (defaults.get(section), self.values.get(section))
            else {
                errs.add(
                    section.clone(),
                    FieldError::new("unknown_field", "Unknown settings section."),
                );
                continue;
            };
            if !changes.is_object() {
                errs.add(
                    section.clone(),
                    FieldError::new("invalid_type", "Must be an object."),
                );
                continue;
            }
            let Some(Value::Object(merged)) =
                merge_checked(section, def, current, changes, &mut errs)
            else {
                continue;
            };
            out.push((section.clone(), merged));
        }
        if errs.is_empty() { Ok(out) } else { Err(errs) }
    }
}

/// Normalizes and checks a public URL: `http(s)://host[:port][/path]`, no
/// query or fragment, no trailing slash.
pub fn normalize_public_url(url: &str) -> Result<String, FieldError> {
    let url = url.trim().trim_end_matches('/');
    let bad = || {
        FieldError::new(
            "invalid_url",
            "Must be an absolute http(s) URL without query or fragment.",
        )
    };
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .ok_or_else(bad)?;
    let host = rest.split('/').next().unwrap_or_default();
    if host.is_empty() || url.contains(['?', '#', ' ']) {
        return Err(bad());
    }
    Ok(url.to_owned())
}

/// Whether `s` is an IP address or a CIDR network (`10.0.0.0/8`).
fn is_cidr(s: &str) -> bool {
    let (ip, prefix) = s
        .trim()
        .split_once('/')
        .map_or((s.trim(), None), |(a, b)| (a, Some(b)));
    let Ok(ip) = ip.parse::<std::net::IpAddr>() else {
        return false;
    };
    let max = if ip.is_ipv4() { 32 } else { 128 };
    prefix.is_none_or(|p| p.parse::<u8>().is_ok_and(|p| p <= max))
}

/// Type-checks `v` against the default `def` and merges it over `cur`.
/// Objects merge key by key; a secret sent back as [`MASK`] keeps its value.
fn merge_checked(
    path: &str,
    def: &Value,
    cur: &Value,
    v: &Value,
    errs: &mut ValidationErrors,
) -> Option<Value> {
    if SECRET_PATHS.contains(&path) {
        return match v {
            Value::String(s) if s == MASK => Some(cur.clone()),
            Value::String(s) if s.is_empty() => Some(Value::Null),
            Value::Null | Value::String(_) => Some(v.clone()),
            _ => {
                errs.add(path, FieldError::new("invalid_type", "Must be a string."));
                None
            }
        };
    }
    if let (Value::Object(dm), Value::Object(m)) = (def, v) {
        let mut out = cur.as_object().cloned().unwrap_or_else(|| dm.clone());
        let mut ok = true;
        for (k, x) in m {
            let p = format!("{path}.{k}");
            let Some(d) = dm.get(k) else {
                errs.add(p, FieldError::new("unknown_field", "Unknown setting."));
                ok = false;
                continue;
            };
            let c = out.get(k).cloned().unwrap_or_else(|| d.clone());
            match merge_checked(&p, d, &c, x, errs) {
                Some(n) => {
                    out.insert(k.clone(), n);
                }
                None => ok = false,
            }
        }
        return ok.then_some(Value::Object(out));
    }
    let type_ok = match (def, v) {
        (Value::Null, Value::Null | Value::String(_)) => true,
        (Value::Bool(_), Value::Bool(_)) | (Value::String(_), Value::String(_)) => true,
        (Value::Number(dn), Value::Number(n)) => !dn.is_u64() || n.is_u64(),
        (Value::Array(_), Value::Array(a)) => a.iter().all(Value::is_string),
        _ => false,
    };
    if !type_ok {
        errs.add(
            path,
            FieldError::new("invalid_type", "Wrong type for this setting."),
        );
        return None;
    }
    if let Err(e) = check_value(path, v) {
        errs.add(path, e);
        return None;
    }
    Some(v.clone())
}

fn check_value(path: &str, v: &Value) -> Result<(), FieldError> {
    let (section, key) = path.split_once('.').unwrap_or((path, ""));
    match (section, key) {
        ("backups", "schedule")
            if v.as_str().is_some_and(|s| {
                !s.trim().is_empty() && crate::domain::cron::Cron::parse(s).is_none()
            }) =>
        {
            Err(FieldError::new(
                "invalid_schedule",
                "A cron expression with five fields, e.g. 0 3 * * *, or empty to turn scheduled backups off.",
            ))
        }
        ("storage", "backend") if !matches!(v.as_str(), Some("fs" | "s3")) => {
            Err(FieldError::new("invalid_option", "One of fs, s3."))
        }
        ("storage", "s3.endpoint")
            if v.as_str().is_some_and(|u| {
                !u.is_empty() && !(u.starts_with("https://") || u.starts_with("http://"))
            }) =>
        {
            Err(FieldError::new(
                "invalid_url",
                "An http(s) URL, e.g. https://s3.eu-central-1.amazonaws.com.",
            ))
        }
        ("storage", "s3.prefix")
            if v.as_str()
                .is_some_and(|p| !p.is_empty() && !crate::domain::files::is_safe_key(p)) =>
        {
            Err(FieldError::new(
                "invalid_option",
                "A relative path without leading or trailing slashes.",
            ))
        }
        ("security", "trusted_proxies")
            if v.as_array()
                .is_some_and(|a| a.iter().any(|x| !x.as_str().is_some_and(is_cidr))) =>
        {
            Err(FieldError::new(
                "invalid_cidr",
                "Each entry must be an IP address or a CIDR network such as 10.0.0.0/8.",
            ))
        }
        ("app", "public_url") if v.is_string() => {
            normalize_public_url(v.as_str().unwrap_or_default()).map(drop)
        }
        ("limits", "batch_max_requests") if v.as_u64().is_some_and(|n| n == 0 || n > 1000) => {
            Err(FieldError::new("out_of_range", "Between 1 and 1000."))
        }
        ("limits", "rule_timeout_ms")
            if v.as_u64().is_some_and(|n| !(10..=60_000).contains(&n)) =>
        {
            Err(FieldError::new("out_of_range", "Between 10 and 60000."))
        }
        ("limits", "max_json_body_kb")
            if v.as_u64().is_some_and(|n| !(1..=1024 * 1024).contains(&n)) =>
        {
            Err(FieldError::new("out_of_range", "Between 1 and 1048576."))
        }
        ("logs", "sample_rate") if v.as_f64().is_some_and(|n| !(0.0..=1.0).contains(&n)) => {
            Err(FieldError::new("out_of_range", "Between 0 and 1."))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn defaults_and_overrides() {
        let s = Settings::from_rows(&[SettingRow {
            key: "limits".into(),
            value: r#"{"rule_timeout_ms": 500, "bogus": 1}"#.into(),
            is_secret: false,
            updated: String::new(),
        }]);
        assert_eq!(s.int("limits", "rule_timeout_ms", 0), 500);
        assert_eq!(s.int("limits", "batch_max_requests", 0), 50);
        assert!(s.section("limits").unwrap().get("bogus").is_none());
    }

    #[test]
    fn patch_validation() {
        let s = Settings::default();
        let ok = s
            .validate_patch(&json!({"app": {"public_url": "https://x.example.com/"}}))
            .unwrap();
        assert_eq!(ok[0].1["public_url"], "https://x.example.com/");
        let e = s
            .validate_patch(&json!({"app": {"public_url": "ftp://x", "nope": 1}, "smtp": {}, "limits": {"rule_timeout_ms": "x"}}))
            .unwrap_err();
        for k in [
            "app.public_url",
            "app.nope",
            "smtp",
            "limits.rule_timeout_ms",
        ] {
            assert!(e.0.contains_key(k), "{k}: {e:?}");
        }
        assert_eq!(
            normalize_public_url("https://a.b/x/").unwrap(),
            "https://a.b/x"
        );
    }
}
