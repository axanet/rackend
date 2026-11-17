//! Rows of the email and external-provider flows (SPEC §11.4, §11.7).

use serde::Serialize;

/// A linked external identity (`_identities`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IdentityRow {
    /// Id.
    pub id: String,
    /// Auth collection.
    pub collection_id: String,
    /// The user.
    pub user_id: String,
    /// Provider row id.
    pub provider_id: String,
    /// `iss` of the ID token (or the preset's issuer).
    pub issuer: String,
    /// `sub`.
    pub subject: String,
    /// Email claimed at link time.
    pub email_at_link: Option<String>,
    /// RFC 3339.
    pub created: String,
    /// RFC 3339.
    pub last_login: Option<String>,
}

/// What a single-use token is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenPurpose {
    /// Confirm an email address (72 h).
    VerifyEmail,
    /// Set a new password (1 h).
    ResetPassword,
    /// Move to a new email address (1 h).
    ChangeEmail,
    /// Sign in by link (15 min).
    MagicLink,
    /// Exchange a finished provider login for tokens (60 s).
    Exchange,
}

impl TokenPurpose {
    /// Stored name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::VerifyEmail => "verify_email",
            Self::ResetPassword => "reset_password",
            Self::ChangeEmail => "change_email",
            Self::MagicLink => "magic_link",
            Self::Exchange => "exchange",
        }
    }
}

/// A single-use token (`_verification_tokens`), stored as SHA-256.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenRow {
    /// SHA-256 of the token.
    pub hash: String,
    /// Purpose.
    pub purpose: TokenPurpose,
    /// Auth collection.
    pub collection_id: String,
    /// The user.
    pub user_id: String,
    /// Purpose data (new email, redirect URI, auth method).
    pub payload: Option<String>,
    /// RFC 3339.
    pub expires: String,
    /// RFC 3339 once used.
    pub used_at: Option<String>,
}

/// A one-time code (`_otps`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtpRow {
    /// Id (`otp_id` given to the client).
    pub id: String,
    /// Auth collection.
    pub collection_id: String,
    /// The user.
    pub user_id: String,
    /// HMAC-SHA256 of id and code.
    pub code_hash: String,
    /// `login`.
    pub purpose: String,
    /// RFC 3339.
    pub expires: String,
    /// Wrong attempts so far.
    pub attempts: u32,
    /// RFC 3339 once used.
    pub used_at: Option<String>,
}

/// An external login provider (`_auth_providers`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRow {
    /// Id.
    pub id: String,
    /// URL name (`google`, `github`, `company-sso`).
    pub name: String,
    /// `oidc` or `oauth2_preset`.
    pub kind: String,
    /// Preset (`github`, `google`, …).
    pub preset: Option<String>,
    /// OIDC issuer.
    pub issuer: Option<String>,
    /// OAuth client id.
    pub client_id: String,
    /// Client secret, encrypted (`enc:v1:`).
    pub client_secret_enc: Option<String>,
    /// Space-separated scopes.
    pub scopes: Option<String>,
    /// The auth collection users land in.
    pub collection_id: String,
    /// Link to an existing user with the same verified email.
    pub auto_link_verified_email: bool,
    /// Claim → field mapping (JSON object).
    pub claim_mapping: Option<String>,
    /// Shown and usable.
    pub enabled: bool,
    /// RFC 3339.
    pub created: String,
    /// RFC 3339.
    pub updated: String,
}
