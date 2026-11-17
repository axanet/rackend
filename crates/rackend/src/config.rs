//! Bootstrap environment variables (SPEC §5.2): the only env configuration.
//! Everything else is runtime configuration in the database (SPEC §15).

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, bail};
use rackend_store_sqlite::Synchronous;

/// Log output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    /// One JSON object per line (default).
    Json,
    /// Human-readable lines (default in dev mode).
    Pretty,
}

/// Parsed bootstrap configuration.
#[derive(Debug, Clone)]
pub struct Config {
    pub data_dir: PathBuf,
    pub http_addr: SocketAddr,
    pub dev: bool,
    pub log_level: String,
    pub log_format: LogFormat,
    pub sqlite_synchronous: Synchronous,
    /// `RACKEND_PUBLIC_URL`: overrides `app.public_url`.
    pub public_url: Option<String>,
    /// `RACKEND_SUPERUSER_EMAIL` / `_PASSWORD`: headless first superuser.
    pub superuser: Option<(String, String)>,
    /// `RACKEND_ENCRYPTION_KEY`: key for secrets at rest (SPEC §17.4).
    pub encryption_key: Option<[u8; 32]>,
    /// `RACKEND_RESET_SECRETS`: discard secrets that cannot be decrypted.
    pub reset_secrets: bool,
    /// `RACKEND_TRUSTED_PROXIES`: CIDRs whose `X-Forwarded-For` is trusted.
    pub trusted_proxies: Vec<rackend_http::net::Cidr>,
    /// `RACKEND_OTLP_ENDPOINT`: OpenTelemetry collector base URL.
    pub otlp_endpoint: Option<String>,
    /// Variables that are recognised but not used by this version yet.
    pub not_yet_supported: Vec<&'static str>,
}

/// Variables read in later milestones. Setting them now is not an error, but
/// the operator is told they have no effect yet.
const FUTURE_VARS: &[&str] = &[];

impl Config {
    /// Reads the configuration from the process environment.
    pub fn from_env() -> anyhow::Result<Self> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// Reads the configuration through `get` (testable).
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let val = |k: &str| {
            get(k)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };

        let dev = match val("RACKEND_DEV").as_deref() {
            None => false,
            Some(v) => {
                parse_bool(v).with_context(|| format!("RACKEND_DEV: invalid boolean {v:?}"))?
            }
        };
        let data_dir =
            val("RACKEND_DATA_DIR").map_or_else(|| PathBuf::from("./rackend_data"), PathBuf::from);
        let http_addr = val("RACKEND_HTTP_ADDR")
            .unwrap_or_else(|| "0.0.0.0:8080".into())
            .parse()
            .context("RACKEND_HTTP_ADDR: expected host:port, e.g. 0.0.0.0:8080")?;
        let log_level = val("RACKEND_LOG_LEVEL").unwrap_or_else(|| "info".into());
        let log_format = match val("RACKEND_LOG_FORMAT").as_deref() {
            None if dev => LogFormat::Pretty,
            None => LogFormat::Json,
            Some(v) if v.eq_ignore_ascii_case("json") => LogFormat::Json,
            Some(v) if v.eq_ignore_ascii_case("pretty") => LogFormat::Pretty,
            Some(v) => bail!("RACKEND_LOG_FORMAT: expected json or pretty, got {v:?}"),
        };
        let sqlite_synchronous = match val("RACKEND_SQLITE_SYNCHRONOUS").as_deref() {
            None => Synchronous::Full,
            Some(v) if v.eq_ignore_ascii_case("full") => Synchronous::Full,
            Some(v) if v.eq_ignore_ascii_case("normal") => Synchronous::Normal,
            Some(v) => bail!("RACKEND_SQLITE_SYNCHRONOUS: expected FULL or NORMAL, got {v:?}"),
        };
        let public_url = match val("RACKEND_PUBLIC_URL") {
            None => None,
            Some(u) => Some(
                rackend_core::settings::normalize_public_url(&u)
                    .map_err(|e| anyhow::anyhow!("RACKEND_PUBLIC_URL: {}", e.message))?,
            ),
        };
        let superuser = match (
            val("RACKEND_SUPERUSER_EMAIL"),
            val("RACKEND_SUPERUSER_PASSWORD"),
        ) {
            (Some(e), Some(p)) => Some((e, p)),
            (None, None) => None,
            _ => {
                bail!("set both RACKEND_SUPERUSER_EMAIL and RACKEND_SUPERUSER_PASSWORD, or neither")
            }
        };
        let encryption_key = match val("RACKEND_ENCRYPTION_KEY") {
            None => None,
            Some(k) => Some(rackend_core::secrets::SecretBox::parse_key(&k).map_err(|_| {
                anyhow::anyhow!(
                    "RACKEND_ENCRYPTION_KEY: expected 32 bytes in base64, e.g. the output of `openssl rand -base64 32`"
                )
            })?),
        };
        let reset_secrets = match val("RACKEND_RESET_SECRETS").as_deref() {
            None => false,
            Some(v) => parse_bool(v)
                .with_context(|| format!("RACKEND_RESET_SECRETS: invalid boolean {v:?}"))?,
        };
        let trusted_proxies = match val("RACKEND_TRUSTED_PROXIES") {
            None => Vec::new(),
            Some(v) => rackend_http::net::Cidr::parse_list(&v)
                .map_err(|e| anyhow::anyhow!("RACKEND_TRUSTED_PROXIES: {e}"))?,
        };
        let otlp_endpoint = match val("RACKEND_OTLP_ENDPOINT") {
            None => None,
            Some(u) if u.starts_with("http://") || u.starts_with("https://") => Some(u),
            Some(u) => bail!(
                "RACKEND_OTLP_ENDPOINT: expected an http(s) URL such as http://otel-collector:4318, got {u:?}"
            ),
        };
        let not_yet_supported = FUTURE_VARS
            .iter()
            .copied()
            .filter(|k| val(k).is_some())
            .collect();
        Ok(Self {
            data_dir,
            http_addr,
            dev,
            log_level,
            log_format,
            sqlite_synchronous,
            public_url,
            superuser,
            encryption_key,
            reset_secrets,
            trusted_proxies,
            otlp_endpoint,
            not_yet_supported,
        })
    }
}

fn parse_bool(v: &str) -> Option<bool> {
    match v.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::collections::HashMap;

    fn cfg(vars: &[(&str, &str)]) -> anyhow::Result<Config> {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect();
        Config::from_lookup(|k| map.get(k).cloned())
    }

    #[test]
    fn defaults_need_no_env() {
        let c = cfg(&[]).unwrap();
        assert_eq!(c.data_dir, PathBuf::from("./rackend_data"));
        assert_eq!(c.http_addr.to_string(), "0.0.0.0:8080");
        assert!(!c.dev);
        assert_eq!(c.log_format, LogFormat::Json);
        assert_eq!(c.sqlite_synchronous, Synchronous::Full);
    }

    #[test]
    fn dev_mode_defaults_to_pretty_logs() {
        assert_eq!(
            cfg(&[("RACKEND_DEV", "true")]).unwrap().log_format,
            LogFormat::Pretty
        );
    }

    #[test]
    fn invalid_values_are_rejected() {
        assert!(cfg(&[("RACKEND_HTTP_ADDR", "nope")]).is_err());
        assert!(cfg(&[("RACKEND_SQLITE_SYNCHRONOUS", "OFF")]).is_err());
        assert!(cfg(&[("RACKEND_DEV", "maybe")]).is_err());
    }

    #[test]
    fn future_vars_are_reported() {
        let c = cfg(&[("RACKEND_OTLP_ENDPOINT", "http://collector:4318")]).unwrap();
        assert_eq!(c.otlp_endpoint.as_deref(), Some("http://collector:4318"));
        assert!(c.not_yet_supported.is_empty());
        assert!(cfg(&[("RACKEND_OTLP_ENDPOINT", "collector:4318")]).is_err());
        assert!(cfg(&[("RACKEND_SUPERUSER_EMAIL", "a@b.c")]).is_err());
        assert!(cfg(&[("RACKEND_PUBLIC_URL", "ftp://x")]).is_err());
        assert!(cfg(&[("RACKEND_ENCRYPTION_KEY", "short")]).is_err());
        assert!(cfg(&[("RACKEND_TRUSTED_PROXIES", "10.0.0.0/33")]).is_err());
        let c = cfg(&[
            (
                "RACKEND_ENCRYPTION_KEY",
                "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            ),
            ("RACKEND_TRUSTED_PROXIES", "10.0.0.0/8, ::1"),
        ])
        .unwrap();
        assert!(c.encryption_key.is_some());
        assert_eq!(c.trusted_proxies.len(), 2);
    }
}
