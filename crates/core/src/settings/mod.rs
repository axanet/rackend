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
        "metrics": { "retention_days": 7, "prometheus_public": false },
        "backups": { "schedule": "0 3 * * *", "keep": 7, "include_storage": true },
        "schema": { "trash_retention_days": 30 },
        "limits": { "max_json_body_kb": 1024, "batch_max_requests": 50, "rule_timeout_ms": 2000 },
        "realtime": {
            "max_connections_per_ip": 100, "max_subscriptions_per_connection": 100,
            "max_message_kb": 64, "max_outbound_queue_kb": 1024
        }
    })
}

/// Keys whose values are secrets. Writing them needs encryption at rest,
/// which arrives with M2 (SecretBox).
const SECRET_SECTIONS: &[&str] = &["smtp", "storage", "email_templates"];

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
                for (k, v) in stored {
                    if base.contains_key(&k) {
                        base.insert(k, v);
                    }
                }
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
            if SECRET_SECTIONS.contains(&section.as_str()) {
                errs.add(
                    section.clone(),
                    FieldError::new(
                        "not_available",
                        "This section needs encryption at rest, which is not available yet.",
                    ),
                );
                continue;
            }
            let (Some(def), Some(current)) = (
                defaults.get(section).and_then(Value::as_object),
                self.section(section),
            ) else {
                errs.add(
                    section.clone(),
                    FieldError::new("unknown_field", "Unknown settings section."),
                );
                continue;
            };
            let Some(changes) = changes.as_object() else {
                errs.add(
                    section.clone(),
                    FieldError::new("invalid_type", "Must be an object."),
                );
                continue;
            };
            let mut merged = current.clone();
            for (k, v) in changes {
                let path = format!("{section}.{k}");
                let Some(d) = def.get(k) else {
                    errs.add(path, FieldError::new("unknown_field", "Unknown setting."));
                    continue;
                };
                let ok = match (d, v) {
                    (Value::Null, Value::Null | Value::String(_)) => true,
                    (Value::Bool(_), Value::Bool(_)) | (Value::String(_), Value::String(_)) => true,
                    (Value::Number(dn), Value::Number(n)) => !dn.is_u64() || n.is_u64(),
                    (Value::Array(_), Value::Array(a)) => a.iter().all(Value::is_string),
                    // Nested objects of counters: known keys, non-negative integers.
                    (Value::Object(dm), Value::Object(m)) => {
                        m.iter().all(|(k, x)| dm.contains_key(k) && x.is_u64())
                    }
                    _ => false,
                };
                if !ok {
                    errs.add(
                        path,
                        FieldError::new("invalid_type", "Wrong type for this setting."),
                    );
                    continue;
                }
                if let Err(e) = check_value(section, k, v) {
                    errs.add(path, e);
                    continue;
                }
                match (merged.get_mut(k), v) {
                    // Partial updates of nested objects keep the other keys.
                    (Some(Value::Object(cur)), Value::Object(m)) => {
                        for (mk, mv) in m {
                            cur.insert(mk.clone(), mv.clone());
                        }
                    }
                    _ => {
                        merged.insert(k.clone(), v.clone());
                    }
                }
            }
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

fn check_value(section: &str, key: &str, v: &Value) -> Result<(), FieldError> {
    match (section, key) {
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
