//! OIDC provider rows and helpers (SPEC §11.8).

/// `public` (no secret) or `confidential` clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientKind {
    /// SPA or mobile app: no secret, PKCE only.
    Public,
    /// Server-side app with a secret.
    Confidential,
}

impl ClientKind {
    /// `public` or `confidential`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Confidential => "confidential",
        }
    }

    /// Parses `public` / `confidential`.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "public" => Some(Self::Public),
            "confidential" => Some(Self::Confidential),
            _ => None,
        }
    }
}

/// One `_oauth_clients` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientRow {
    /// Client id (`default` or a UUID).
    pub id: String,
    /// Display name, shown on the consent page.
    pub name: String,
    /// Public or confidential.
    pub kind: ClientKind,
    /// SHA-256 of the secret (confidential clients).
    pub secret_hash: Option<String>,
    /// Allowed redirect URIs (exact match).
    pub redirect_uris: Vec<String>,
    /// Allowed post-logout redirect URIs (exact match).
    pub post_logout_redirect_uris: Vec<String>,
    /// Scopes the client may request.
    pub scopes: Vec<String>,
    /// The auth collection its users come from.
    pub collection_id: String,
    /// Skips the consent page.
    pub first_party: bool,
    /// Accepts `http://localhost:*` and `http://127.0.0.1:*` redirect URIs.
    pub dev: bool,
    /// `RS256` or `ES256` for ID tokens.
    pub id_token_alg: String,
    /// PKCE S256 is required. Always true for public clients; confidential
    /// clients may turn it off (some server-side libraries and the OpenID
    /// conformance suite do not send it).
    pub require_pkce: bool,
    /// Access token lifetime.
    pub access_ttl_s: i64,
    /// Refresh token lifetime.
    pub refresh_ttl_s: i64,
    /// Disabled clients cannot sign in.
    pub enabled: bool,
    /// Creation time.
    pub created: String,
    /// Last change.
    pub updated: String,
}

impl ClientRow {
    /// Whether `uri` is a registered redirect URI (exact match), or a
    /// loopback URI with any port for `dev` clients.
    pub fn redirect_allowed(&self, uri: &str) -> bool {
        self.redirect_uris.iter().any(|u| u == uri) || (self.dev && is_loopback_http(uri))
    }

    /// Whether `uri` is a registered post-logout redirect URI.
    pub fn post_logout_allowed(&self, uri: &str) -> bool {
        self.post_logout_redirect_uris.iter().any(|u| u == uri)
            || (self.dev && is_loopback_http(uri))
    }
}

/// `http://localhost[:port][/…]` or `http://127.0.0.1[:port][/…]`.
pub fn is_loopback_http(uri: &str) -> bool {
    let Some(rest) = uri
        .strip_prefix("http://localhost")
        .or_else(|| uri.strip_prefix("http://127.0.0.1"))
    else {
        return false;
    };
    let rest = match rest.strip_prefix(':') {
        Some(r) => {
            let digits = r.bytes().take_while(u8::is_ascii_digit).count();
            if digits == 0 {
                return false;
            }
            &r[digits..]
        }
        None => rest,
    };
    rest.is_empty() || rest.starts_with('/') || rest.starts_with('?')
}

/// One `_oauth_codes` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeRow {
    /// SHA-256 of the code.
    pub hash: String,
    /// Client.
    pub client_id: String,
    /// The browser session that approved the request.
    pub session_id: String,
    /// The redirect URI of the request.
    pub redirect_uri: String,
    /// Granted scopes, space separated.
    pub scopes: String,
    /// PKCE S256 challenge.
    pub code_challenge: String,
    /// `nonce` of the request.
    pub nonce: Option<String>,
    /// When the user signed in.
    pub auth_time: String,
    /// Expiry (60 s).
    pub expires: String,
    /// When it was exchanged.
    pub used_at: Option<String>,
    /// The session the exchange created.
    pub issued_session_id: Option<String>,
}

/// One `_oauth_consents` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsentRow {
    /// User.
    pub user_id: String,
    /// Auth collection.
    pub collection_id: String,
    /// Client.
    pub client_id: String,
    /// Granted scopes, space separated.
    pub scopes: String,
    /// When.
    pub granted_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback() {
        assert!(is_loopback_http("http://localhost:5173/callback"));
        assert!(is_loopback_http("http://127.0.0.1:8080"));
        assert!(is_loopback_http("http://localhost/x"));
        assert!(!is_loopback_http("http://localhost.evil.com/"));
        assert!(!is_loopback_http("http://localhost:/x"));
        assert!(!is_loopback_http("https://localhost:1/"));
        assert!(!is_loopback_http("http://localhost@evil.com/"));
    }
}
