//! Builds the use-case [`RequestCtx`] from an HTTP request: authentication
//! via `Authorization: Bearer` or the session cookie (SPEC §10.1, §11.6).

use std::collections::BTreeMap;
use std::net::SocketAddr;

use axum::extract::{ConnectInfo, FromRequestParts, Query};
use axum::http::header::{self, HeaderMap, HeaderValue};
use axum::http::request::Parts;
use axum::http::{Method, Uri};
use rackend_core::usecases::{Principal, RequestCtx};

use crate::AppState;
use crate::middleware::RequestId;
use crate::problem::{ApiError, ProblemCode};

/// Access token cookie.
pub const SESSION_COOKIE: &str = "rk_session";
/// Refresh token cookie (path-scoped to the refresh endpoint).
pub const REFRESH_COOKIE: &str = "rk_refresh";
/// Header selecting cookie mode on login/refresh.
pub const AUTH_MODE_HEADER: &str = "x-rackend-auth-mode";

/// The request context of a handler.
#[derive(Debug, Clone)]
pub struct Ctx(pub RequestCtx);

/// Reads a cookie value.
pub fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_owned())
}

fn bearer(headers: &HeaderMap) -> Option<String> {
    let v = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = v.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| token.trim().to_owned())
}

fn invalid_token() -> ApiError {
    ApiError::with_detail(
        ProblemCode::Unauthorized,
        "The access token is invalid or expired.",
    )
}

/// The origin (`scheme://host[:port]`) of a URL.
fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let host = rest.split('/').next()?;
    Some(format!(
        "{}://{}",
        scheme.to_ascii_lowercase(),
        host.to_ascii_lowercase()
    ))
}

/// CSRF protection for cookie-authenticated unsafe requests (SPEC §11.6):
/// `Origin` (or `Referer`) must match `app.public_url`, a configured CORS
/// origin, or (before a public URL is set) the request's own host.
fn check_origin(state: &AppState, parts: &Parts) -> Result<(), ApiError> {
    if matches!(parts.method, Method::GET | Method::HEAD | Method::OPTIONS) {
        return Ok(());
    }
    origin_allowed(state, &parts.headers)
}

/// The `Origin` (or `Referer`) check of [`check_origin`] for any method.
/// WebSocket upgrades authenticated by cookie need it: browsers send
/// cookies on cross-site WebSocket requests (SPEC §11.6).
pub fn origin_allowed(state: &AppState, h: &HeaderMap) -> Result<(), ApiError> {
    let origin = h
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .or_else(|| {
            h.get(header::REFERER)
                .and_then(|v| v.to_str().ok())
                .and_then(origin_of)
        });
    let Some(origin) = origin.map(|o| o.to_ascii_lowercase()) else {
        return Err(ApiError::with_detail(
            ProblemCode::Forbidden,
            "Cookie-authenticated requests need an Origin header.",
        ));
    };
    let settings = state.app().settings();
    let mut allowed: Vec<String> = settings
        .strings("security", "cors_origins")
        .iter()
        .filter_map(|o| origin_of(o))
        .collect();
    match settings.public_url() {
        Some(u) => allowed.extend(origin_of(&u)),
        None => {
            if let Some(host) = h.get(header::HOST).and_then(|v| v.to_str().ok()) {
                allowed.push(format!("http://{host}").to_ascii_lowercase());
                allowed.push(format!("https://{host}").to_ascii_lowercase());
            }
        }
    }
    let dev_local = state.dev()
        && (origin.starts_with("http://localhost") || origin.starts_with("http://127.0.0.1"));
    if dev_local || allowed.contains(&origin) {
        Ok(())
    } else {
        Err(ApiError::with_detail(
            ProblemCode::Forbidden,
            "Cross-origin request rejected (CSRF protection).",
        ))
    }
}

fn query_map(uri: &Uri) -> BTreeMap<String, String> {
    Query::<BTreeMap<String, String>>::try_from_uri(uri)
        .map(|q| q.0)
        .unwrap_or_default()
}

fn header_map(h: &HeaderMap) -> BTreeMap<String, String> {
    h.iter()
        .filter(|(k, _)| *k != header::AUTHORIZATION && *k != header::COOKIE)
        .filter_map(|(k, v)| {
            Some((
                k.as_str().to_ascii_lowercase().replace('-', "_"),
                v.to_str().ok()?.to_owned(),
            ))
        })
        .collect()
}

impl FromRequestParts<AppState> for Ctx {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let app = state.app();
        let principal = if let Some(token) = bearer(&parts.headers) {
            app.authenticate(&token)
                .await
                .map_err(|_| invalid_token())?
        } else if let Some(token) = cookie(&parts.headers, SESSION_COOKIE).filter(|t| !t.is_empty())
        {
            check_origin(state, parts)?;
            app.authenticate(&token)
                .await
                .map_err(|_| invalid_token())?
        } else {
            Principal::Anonymous
        };
        let peer = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0.ip());
        let ip = crate::net::client_ip(peer, &parts.headers, &state.trusted_proxies())
            .map(|ip| ip.to_string());
        Ok(Ctx(RequestCtx {
            principal,
            ip,
            user_agent: parts
                .headers
                .get(header::USER_AGENT)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned),
            request_id: parts.extensions.get::<RequestId>().map(|r| r.0.to_string()),
            method: parts.method.as_str().to_owned(),
            query: query_map(&parts.uri),
            headers: header_map(&parts.headers),
            now: app.clock.now(),
        }))
    }
}

/// Whether the request carries a bearer token.
pub fn has_bearer(headers: &HeaderMap) -> bool {
    bearer(headers).is_some()
}

/// Whether the client asked for cookie mode.
pub fn wants_cookies(headers: &HeaderMap) -> bool {
    headers
        .get(AUTH_MODE_HEADER)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("cookie"))
}

/// `Set-Cookie` values for a session (SPEC §11.6). `Secure` is dropped only
/// in dev mode for plain-http localhost.
pub fn session_cookies(
    access: &str,
    refresh: &str,
    access_ttl: i64,
    refresh_ttl: i64,
    refresh_path: &str,
    secure: bool,
) -> Vec<HeaderValue> {
    let sec = if secure { "; Secure" } else { "" };
    [
        format!("{SESSION_COOKIE}={access}; Path=/; HttpOnly; SameSite=Lax; Max-Age={access_ttl}{sec}"),
        format!("{REFRESH_COOKIE}={refresh}; Path={refresh_path}; HttpOnly; SameSite=Lax; Max-Age={refresh_ttl}{sec}"),
    ]
    .into_iter()
    .filter_map(|c| HeaderValue::from_str(&c).ok())
    .collect()
}

/// `Set-Cookie` values that clear the session cookies.
pub fn clear_cookies(refresh_path: &str, secure: bool) -> Vec<HeaderValue> {
    session_cookies("", "", 0, 0, refresh_path, secure)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cookies_and_bearer() {
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            HeaderValue::from_static("a=1; rk_session=tok; b=2"),
        );
        h.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer abc"),
        );
        assert_eq!(cookie(&h, SESSION_COOKIE).as_deref(), Some("tok"));
        assert_eq!(bearer(&h).as_deref(), Some("abc"));
        assert_eq!(
            origin_of("https://Example.com:8443/x?y").as_deref(),
            Some("https://example.com:8443")
        );
    }
}
