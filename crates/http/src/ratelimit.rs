//! Rate limits (SPEC §11.10). Defaults can be changed in the settings under
//! `security.rate_limits`; changes apply on the next request.
//!
//! | Bucket | Default |
//! |---|---|
//! | `global_ip_per_min` (all `/api`) | 600 |
//! | `login_ip_per_min` (password, OTP, passkey) | 20 |
//! | `login_account_per_15min` | 10, then a lockout of 15 min doubling each time |
//! | `email_ip_per_10min` (email-sending endpoints) | 5 |
//! | `email_address_per_hour` (per target address) | 3 |

use std::collections::HashMap;
use std::net::IpAddr;
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use governor::clock::{Clock, DefaultClock};
use governor::{DefaultKeyedRateLimiter, Quota, RateLimiter};
use serde_json::Value;

/// Limit values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LimitConfig {
    /// All API requests per IP per minute.
    pub global_ip_per_min: u32,
    /// Login attempts per IP per minute.
    pub login_ip_per_min: u32,
    /// Login attempts per account per 15 minutes.
    pub login_account_per_15min: u32,
    /// Email-sending requests per IP per 10 minutes.
    pub email_ip_per_10min: u32,
    /// Email-sending requests per target address per hour.
    pub email_address_per_hour: u32,
}

impl Default for LimitConfig {
    fn default() -> Self {
        Self {
            global_ip_per_min: 600,
            login_ip_per_min: 20,
            login_account_per_15min: 10,
            email_ip_per_10min: 5,
            email_address_per_hour: 3,
        }
    }
}

impl LimitConfig {
    /// Reads `security.rate_limits` (missing keys keep their defaults; 0 disables).
    pub fn from_settings(section: Option<&serde_json::Map<String, Value>>) -> Self {
        let d = Self::default();
        let get = |k: &str, def: u32| {
            section
                .and_then(|s| s.get("rate_limits"))
                .and_then(|r| r.get(k))
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or(def)
        };
        Self {
            global_ip_per_min: get("global_ip_per_min", d.global_ip_per_min),
            login_ip_per_min: get("login_ip_per_min", d.login_ip_per_min),
            login_account_per_15min: get("login_account_per_15min", d.login_account_per_15min),
            email_ip_per_10min: get("email_ip_per_10min", d.email_ip_per_10min),
            email_address_per_hour: get("email_address_per_hour", d.email_address_per_hour),
        }
    }
}

/// Exceeded: retry after this many seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limited(pub u32);

#[derive(Debug, Default)]
struct Account {
    attempts: Vec<Instant>,
    locked_until: Option<Instant>,
    lockouts: u32,
}

struct Buckets {
    config: LimitConfig,
    global_ip: Option<DefaultKeyedRateLimiter<IpAddr>>,
    login_ip: Option<DefaultKeyedRateLimiter<IpAddr>>,
    email_ip: Option<DefaultKeyedRateLimiter<IpAddr>>,
    email_address: Option<DefaultKeyedRateLimiter<String>>,
}

/// All limiters of the instance.
pub struct RateLimits {
    buckets: Mutex<Arc<Buckets>>,
    accounts: Mutex<HashMap<String, Account>>,
    checks: std::sync::atomic::AtomicU64,
}

impl std::fmt::Debug for RateLimits {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RateLimits")
    }
}

impl Default for RateLimits {
    fn default() -> Self {
        Self {
            buckets: Mutex::new(Arc::new(Buckets::new(LimitConfig::default()))),
            accounts: Mutex::new(HashMap::new()),
            checks: std::sync::atomic::AtomicU64::new(0),
        }
    }
}

const ACCOUNT_WINDOW: Duration = Duration::from_secs(15 * 60);
const LOCKOUT_BASE: Duration = Duration::from_secs(15 * 60);

impl Buckets {
    fn new(config: LimitConfig) -> Self {
        let per_min = |n: u32| NonZeroU32::new(n).map(|n| RateLimiter::keyed(Quota::per_minute(n)));
        // `n` per `window`: a burst of `n`, refilled evenly over the window.
        fn per<K: std::hash::Hash + Eq + Clone>(
            n: u32,
            window: Duration,
        ) -> Option<DefaultKeyedRateLimiter<K>> {
            let burst = NonZeroU32::new(n)?;
            Quota::with_period(window / n).map(|q| RateLimiter::keyed(q.allow_burst(burst)))
        }
        Self {
            config,
            global_ip: per_min(config.global_ip_per_min),
            login_ip: per_min(config.login_ip_per_min),
            email_ip: per(config.email_ip_per_10min, Duration::from_secs(600)),
            email_address: per(config.email_address_per_hour, Duration::from_secs(3600)),
        }
    }
}

fn check<K: std::hash::Hash + Eq + Clone>(
    limiter: Option<&DefaultKeyedRateLimiter<K>>,
    key: K,
) -> Result<(), Limited> {
    let Some(l) = limiter else { return Ok(()) };
    l.check_key(&key).map_err(|nu| {
        let wait = nu.wait_time_from(DefaultClock::default().now());
        Limited(u32::try_from(wait.as_secs()).unwrap_or(u32::MAX).max(1))
    })
}

impl RateLimits {
    fn buckets(&self, config: LimitConfig) -> Arc<Buckets> {
        let mut g = self
            .buckets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if g.config != config {
            *g = Arc::new(Buckets::new(config));
        }
        // Forget idle keys now and then so memory stays bounded.
        if self
            .checks
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .is_multiple_of(10_000)
        {
            if let Some(l) = &g.global_ip {
                l.retain_recent();
            }
            if let Some(l) = &g.login_ip {
                l.retain_recent();
            }
            if let Some(l) = &g.email_ip {
                l.retain_recent();
            }
            if let Some(l) = &g.email_address {
                l.retain_recent();
            }
        }
        Arc::clone(&g)
    }

    /// Every `/api` request.
    pub fn check_global(&self, config: LimitConfig, ip: IpAddr) -> Result<(), Limited> {
        check(self.buckets(config).global_ip.as_ref(), ip)
    }

    /// A login attempt: per IP, then per account (`account` is
    /// `collection:email`). Locked accounts get a growing lockout.
    pub fn check_login(
        &self,
        config: LimitConfig,
        ip: Option<IpAddr>,
        account: &str,
    ) -> Result<(), Limited> {
        if let Some(ip) = ip {
            check(self.buckets(config).login_ip.as_ref(), ip)?;
        }
        if config.login_account_per_15min == 0 {
            return Ok(());
        }
        let now = Instant::now();
        let mut accounts = self
            .accounts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if accounts.len() > 100_000 {
            accounts.retain(|_, a| {
                a.locked_until.is_some_and(|t| t > now)
                    || a.attempts.iter().any(|t| now - *t < ACCOUNT_WINDOW)
            });
        }
        let a = accounts.entry(account.to_owned()).or_default();
        if let Some(until) = a.locked_until {
            if until > now {
                return Err(Limited(
                    u32::try_from((until - now).as_secs())
                        .unwrap_or(u32::MAX)
                        .max(1),
                ));
            }
            a.locked_until = None;
        }
        a.attempts.retain(|t| now - *t < ACCOUNT_WINDOW);
        if a.attempts.len() >= config.login_account_per_15min as usize {
            let lock = LOCKOUT_BASE * 2u32.saturating_pow(a.lockouts.min(6));
            a.lockouts += 1;
            a.locked_until = Some(now + lock);
            a.attempts.clear();
            return Err(Limited(u32::try_from(lock.as_secs()).unwrap_or(u32::MAX)));
        }
        a.attempts.push(now);
        Ok(())
    }

    /// An email-sending request (verification, reset, email change, OTP,
    /// magic link): per IP, then per target address. Limited the same way
    /// whether or not the address has an account.
    pub fn check_email(
        &self,
        config: LimitConfig,
        ip: Option<IpAddr>,
        address: &str,
    ) -> Result<(), Limited> {
        let b = self.buckets(config);
        if let Some(ip) = ip {
            check(b.email_ip.as_ref(), ip)?;
        }
        check(b.email_address.as_ref(), address.trim().to_lowercase())
    }

    /// A successful login clears the account's attempts and lockout history.
    pub fn login_succeeded(&self, account: &str) {
        let mut accounts = self
            .accounts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        accounts.remove(account);
    }
}

/// The 429 problem for an exceeded limit.
pub fn limited_error(l: Limited) -> crate::ApiError {
    let mut e = crate::ApiError::with_detail(
        crate::ProblemCode::RateLimited,
        "Too many requests. Retry later.",
    );
    e.problem_mut().retry_after_s = Some(l.0);
    e
}

/// Middleware: the global per-IP limit on `/api` (SPEC §11.10).
pub async fn global_limit(
    axum::extract::State(state): axum::extract::State<crate::AppState>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::response::IntoResponse as _;
    if req.uri().path().starts_with("/api/") && req.uri().path() != "/api/health" {
        let peer = req
            .extensions()
            .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
            .map(|c| c.0.ip());
        if let Some(ip) = crate::net::client_ip(peer, req.headers(), &state.trusted_proxies())
            && let Err(l) = state.limits().check_global(state.limit_config(), ip)
        {
            return limited_error(l).into_response();
        }
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_lockout_after_attempts() {
        let rl = RateLimits::default();
        let cfg = LimitConfig {
            login_account_per_15min: 3,
            login_ip_per_min: 100,
            ..LimitConfig::default()
        };
        for _ in 0..3 {
            assert!(rl.check_login(cfg, None, "users:a@b.co").is_ok());
        }
        let Err(Limited(wait)) = rl.check_login(cfg, None, "users:a@b.co") else {
            unreachable!("expected a lockout")
        };
        assert_eq!(wait, 900);
        assert!(
            rl.check_login(cfg, None, "users:other@b.co").is_ok(),
            "per account"
        );
        rl.login_succeeded("users:a@b.co");
        assert!(rl.check_login(cfg, None, "users:a@b.co").is_ok());
    }

    #[test]
    fn per_ip_buckets() {
        let rl = RateLimits::default();
        let cfg = LimitConfig {
            global_ip_per_min: 2,
            ..LimitConfig::default()
        };
        let ip: IpAddr = [192, 0, 2, 1].into();
        assert!(rl.check_global(cfg, ip).is_ok());
        assert!(rl.check_global(cfg, ip).is_ok());
        assert!(rl.check_global(cfg, ip).is_err());
        assert!(rl.check_global(cfg, [192, 0, 2, 2].into()).is_ok());
    }
}
