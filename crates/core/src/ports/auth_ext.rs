//! Mail and external identity provider ports (SPEC §4.3, §11.4, §11.7).

use std::sync::Arc;

use serde_json::Value;

use super::BoxFuture;

/// An email to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Email {
    /// Recipient address.
    pub to: String,
    /// Subject line.
    pub subject: String,
    /// HTML body.
    pub html: String,
    /// Plain-text body.
    pub text: String,
}

/// A mail failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("mail: {0}")]
pub struct MailError(pub String);

/// Sends email.
pub trait Mailer: Send + Sync + std::fmt::Debug {
    /// Sends one message.
    fn send(&self, email: Email) -> BoxFuture<'_, Result<(), MailError>>;
}

/// How to talk to the SMTP server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmtpTls {
    /// STARTTLS on a plain connection (port 587).
    StartTls,
    /// TLS from the start (port 465).
    Tls,
    /// No encryption (local relays and tests only).
    None,
}

/// SMTP settings, with the password decrypted.
#[derive(Clone, PartialEq, Eq)]
pub struct SmtpConfig {
    /// Host name.
    pub host: String,
    /// Port.
    pub port: u16,
    /// User name (empty: no authentication).
    pub username: String,
    /// Password.
    pub password: String,
    /// Encryption.
    pub tls: SmtpTls,
    /// Sender display name.
    pub from_name: String,
    /// Sender address.
    pub from_address: String,
}

impl std::fmt::Debug for SmtpConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SmtpConfig")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("tls", &self.tls)
            .field("from_address", &self.from_address)
            .finish_non_exhaustive()
    }
}

/// Builds mailers from the settings (they can change at runtime).
pub trait MailerFactory: Send + Sync + std::fmt::Debug {
    /// A mailer for `config`.
    fn build(&self, config: &SmtpConfig) -> Result<Arc<dyn Mailer>, MailError>;
}

/// How to reach a provider (from `_auth_providers`, secret decrypted).
#[derive(Clone, PartialEq, Eq)]
pub struct IdpConfig {
    /// `oidc` or `oauth2_preset`.
    pub kind: String,
    /// Preset name for `oauth2_preset` (`github`).
    pub preset: Option<String>,
    /// OIDC issuer URL.
    pub issuer: Option<String>,
    /// Client id.
    pub client_id: String,
    /// Client secret.
    pub client_secret: String,
    /// Scopes.
    pub scopes: Vec<String>,
    /// Our callback URL registered at the provider.
    pub redirect_uri: String,
}

impl std::fmt::Debug for IdpConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IdpConfig")
            .field("kind", &self.kind)
            .field("preset", &self.preset)
            .field("issuer", &self.issuer)
            .field("client_id", &self.client_id)
            .field("scopes", &self.scopes)
            .field("redirect_uri", &self.redirect_uri)
            .finish_non_exhaustive()
    }
}

/// The start of an authorization code flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdpStart {
    /// Where to send the browser.
    pub url: String,
    /// `state` sent to the provider.
    pub state: String,
    /// `nonce` sent to the provider (OIDC).
    pub nonce: String,
    /// PKCE verifier (S256).
    pub pkce_verifier: String,
}

/// A user as the provider describes them, after validation.
#[derive(Debug, Clone, PartialEq)]
pub struct IdpUser {
    /// Issuer (`iss`, or the preset's base URL).
    pub issuer: String,
    /// Subject (`sub`, or the provider's user id).
    pub subject: String,
    /// Email, lowercased.
    pub email: Option<String>,
    /// Whether the provider verified the email.
    pub email_verified: bool,
    /// All claims, for the claim mapping.
    pub claims: Value,
}

/// A provider failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdpError {
    /// Configuration or discovery failed.
    #[error("provider setup: {0}")]
    Setup(String),
    /// The provider refused the code, or a token did not validate.
    #[error("provider login failed: {0}")]
    Login(String),
}

/// Talks to external identity providers (authorization code + PKCE S256).
pub trait IdpClient: Send + Sync + std::fmt::Debug {
    /// OIDC discovery metadata of `issuer` (cached).
    fn discover<'a>(&'a self, issuer: &'a str) -> BoxFuture<'a, Result<Value, IdpError>>;
    /// The authorization URL with fresh `state`, `nonce` and PKCE challenge.
    fn start<'a>(&'a self, config: &'a IdpConfig) -> BoxFuture<'a, Result<IdpStart, IdpError>>;
    /// Exchanges the code and validates the result (ID token signature,
    /// `iss`, `aud`, `exp`, `nonce`).
    fn finish<'a>(
        &'a self,
        config: &'a IdpConfig,
        code: &'a str,
        pkce_verifier: &'a str,
        nonce: &'a str,
    ) -> BoxFuture<'a, Result<IdpUser, IdpError>>;
}
