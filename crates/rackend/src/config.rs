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
    /// Variables that are recognised but not used by this version yet.
    pub not_yet_supported: Vec<&'static str>,
}

/// Variables read in later milestones. Setting them now is not an error, but
/// the operator is told they have no effect yet.
const FUTURE_VARS: &[&str] = &[
    "RACKEND_PUBLIC_URL",
    "RACKEND_ENCRYPTION_KEY",
    "RACKEND_TRUSTED_PROXIES",
    "RACKEND_OTLP_ENDPOINT",
    "RACKEND_SUPERUSER_EMAIL",
    "RACKEND_SUPERUSER_PASSWORD",
];

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
        let c = cfg(&[("RACKEND_PUBLIC_URL", "https://x")]).unwrap();
        assert_eq!(c.not_yet_supported, vec!["RACKEND_PUBLIC_URL"]);
    }
}
