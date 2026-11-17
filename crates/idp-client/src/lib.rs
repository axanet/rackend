//! `rackend-idp-client`: the core `IdpClient` port (SPEC §11.7).
//!
//! - `oidc`: discovery (cached 24 h), authorization code + PKCE S256, `state`
//!   and `nonce`, full ID token validation by `openidconnect`; an unknown
//!   `kid` refreshes the keys once.
//! - `oauth2_preset` `github`: code + PKCE, then `/user` and `/user/emails`.
//!
//! HTTP goes through `reqwest` with rustls, no redirects (SSRF hardening),
//! a 10 s timeout and a 1 MiB response limit.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use openidconnect::core::{
    CoreAuthenticationFlow, CoreClient, CoreIdTokenClaims, CoreJwsSigningAlgorithm,
    CoreProviderMetadata,
};
use openidconnect::{
    AsyncHttpClient, AuthorizationCode, ClaimsVerificationError, ClientId, ClientSecret, CsrfToken,
    HttpRequest, HttpResponse, IssuerUrl, Nonce, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl,
    Scope, SignatureVerificationError, TokenResponse as _,
};
use rackend_core::ports::{BoxFuture, IdpClient, IdpConfig, IdpError, IdpStart, IdpUser};
use serde_json::Value;

/// How long discovery documents (and their keys) are cached.
const DISCOVERY_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// Largest accepted response.
const MAX_BODY: usize = 1024 * 1024;

/// GitHub endpoints (overridable for tests).
#[derive(Debug, Clone)]
pub struct GithubEndpoints {
    /// `https://github.com/login/oauth/authorize`.
    pub authorize: String,
    /// `https://github.com/login/oauth/access_token`.
    pub token: String,
    /// `https://api.github.com`.
    pub api: String,
}

impl Default for GithubEndpoints {
    fn default() -> Self {
        Self {
            authorize: "https://github.com/login/oauth/authorize".into(),
            token: "https://github.com/login/oauth/access_token".into(),
            api: "https://api.github.com".into(),
        }
    }
}

/// The external provider client.
pub struct Idp {
    http: reqwest::Client,
    discovery: Mutex<HashMap<String, (Instant, CoreProviderMetadata)>>,
    github: GithubEndpoints,
}

impl std::fmt::Debug for Idp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Idp").finish_non_exhaustive()
    }
}

/// An HTTP failure.
#[derive(Debug)]
pub struct HttpError(String);

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HttpError {}

fn setup(e: impl std::fmt::Display) -> IdpError {
    IdpError::Setup(e.to_string())
}

fn login(e: impl std::fmt::Display) -> IdpError {
    IdpError::Login(e.to_string())
}

/// The asymmetric ID token algorithms the provider advertises (RS256 when it
/// lists none). `none` and HMAC are never accepted.
fn allowed_algs(m: &CoreProviderMetadata) -> Vec<CoreJwsSigningAlgorithm> {
    use CoreJwsSigningAlgorithm as A;
    let algs: Vec<A> = m
        .id_token_signing_alg_values_supported()
        .iter()
        .filter(|a| {
            matches!(
                a,
                A::RsaSsaPkcs1V15Sha256
                    | A::RsaSsaPkcs1V15Sha384
                    | A::RsaSsaPkcs1V15Sha512
                    | A::EcdsaP256Sha256
                    | A::EcdsaP384Sha384
                    | A::RsaSsaPssSha256
                    | A::RsaSsaPssSha384
                    | A::RsaSsaPssSha512
                    | A::EdDsa
            )
        })
        .cloned()
        .collect();
    if algs.is_empty() {
        vec![A::RsaSsaPkcs1V15Sha256]
    } else {
        algs
    }
}

/// The reqwest client for `openidconnect`.
struct Http<'a>(&'a reqwest::Client);

impl<'c> AsyncHttpClient<'c> for Http<'_> {
    type Error = HttpError;
    type Future = BoxFuture<'c, Result<HttpResponse, HttpError>>;

    fn call(&'c self, request: HttpRequest) -> Self::Future {
        Box::pin(async move {
            let (parts, body) = request.into_parts();
            let res = self
                .0
                .request(parts.method, parts.uri.to_string())
                .headers(parts.headers)
                .body(body)
                .send()
                .await
                .map_err(|e| HttpError(e.to_string()))?;
            let status = res.status();
            let headers = res.headers().clone();
            let body = read_limited(res).await?;
            let mut out = http::Response::builder().status(status);
            if let Some(h) = out.headers_mut() {
                *h = headers;
            }
            out.body(body).map_err(|e| HttpError(e.to_string()))
        })
    }
}

async fn read_limited(mut res: reqwest::Response) -> Result<Vec<u8>, HttpError> {
    let mut body = Vec::new();
    while let Some(chunk) = res.chunk().await.map_err(|e| HttpError(e.to_string()))? {
        if body.len() + chunk.len() > MAX_BODY {
            return Err(HttpError("response too large".into()));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

impl Default for Idp {
    fn default() -> Self {
        Self::new(GithubEndpoints::default())
    }
}

impl Idp {
    /// A client; `github` sets the GitHub endpoints.
    pub fn new(github: GithubEndpoints) -> Self {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .user_agent(concat!("rackend/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_default();
        Self {
            http,
            discovery: Mutex::new(HashMap::new()),
            github,
        }
    }

    async fn metadata(&self, issuer: &str, fresh: bool) -> Result<CoreProviderMetadata, IdpError> {
        if !fresh
            && let Some((at, m)) = self
                .discovery
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(issuer)
            && at.elapsed() < DISCOVERY_TTL
        {
            return Ok(m.clone());
        }
        let url = IssuerUrl::new(issuer.to_owned()).map_err(setup)?;
        let m = CoreProviderMetadata::discover_async(url, &Http(&self.http))
            .await
            .map_err(|e| setup(format!("discovery of {issuer} failed: {e}")))?;
        self.discovery
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(issuer.to_owned(), (Instant::now(), m.clone()));
        Ok(m)
    }

    fn oidc_client(
        m: CoreProviderMetadata,
        cfg: &IdpConfig,
    ) -> Result<
        CoreClient<
            openidconnect::EndpointSet,
            openidconnect::EndpointNotSet,
            openidconnect::EndpointNotSet,
            openidconnect::EndpointNotSet,
            openidconnect::EndpointMaybeSet,
            openidconnect::EndpointMaybeSet,
        >,
        IdpError,
    > {
        let secret =
            (!cfg.client_secret.is_empty()).then(|| ClientSecret::new(cfg.client_secret.clone()));
        Ok(
            CoreClient::from_provider_metadata(m, ClientId::new(cfg.client_id.clone()), secret)
                .set_redirect_uri(RedirectUrl::new(cfg.redirect_uri.clone()).map_err(setup)?),
        )
    }

    async fn oidc_start(&self, cfg: &IdpConfig) -> Result<IdpStart, IdpError> {
        let issuer = cfg.issuer.as_deref().ok_or_else(|| setup("no issuer"))?;
        let client = Self::oidc_client(self.metadata(issuer, false).await?, cfg)?;
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let mut req = client.authorize_url(
            CoreAuthenticationFlow::AuthorizationCode,
            CsrfToken::new_random,
            Nonce::new_random,
        );
        for s in cfg.scopes.iter().filter(|s| *s != "openid") {
            req = req.add_scope(Scope::new(s.clone()));
        }
        let (url, state, nonce) = req.set_pkce_challenge(challenge).url();
        Ok(IdpStart {
            url: url.to_string(),
            state: state.secret().clone(),
            nonce: nonce.secret().clone(),
            pkce_verifier: verifier.secret().clone(),
        })
    }

    async fn oidc_finish(
        &self,
        cfg: &IdpConfig,
        code: &str,
        verifier: &str,
        nonce: &str,
    ) -> Result<IdpUser, IdpError> {
        let issuer = cfg.issuer.as_deref().ok_or_else(|| setup("no issuer"))?;
        let metadata = self.metadata(issuer, false).await?;
        let algs = allowed_algs(&metadata);
        let client = Self::oidc_client(metadata, cfg)?;
        let tokens = client
            .exchange_code(AuthorizationCode::new(code.to_owned()))
            .map_err(setup)?
            .set_pkce_verifier(PkceCodeVerifier::new(verifier.to_owned()))
            .request_async(&Http(&self.http))
            .await
            .map_err(|e| login(format!("token request failed: {e}")))?;
        let id_token = tokens.id_token().ok_or_else(|| login("no ID token"))?;
        let nonce = Nonce::new(nonce.to_owned());
        let verifier = client.id_token_verifier().set_allowed_algs(algs.clone());
        let claims: CoreIdTokenClaims = match id_token.claims(&verifier, &nonce) {
            Ok(c) => c.clone(),
            // Keys rotated: fetch them again, once.
            Err(ClaimsVerificationError::SignatureVerification(
                SignatureVerificationError::NoMatchingKey,
            )) => {
                let client = Self::oidc_client(self.metadata(issuer, true).await?, cfg)?;
                id_token
                    .claims(&client.id_token_verifier().set_allowed_algs(algs), &nonce)
                    .map_err(|e| login(format!("ID token rejected: {e}")))?
                    .clone()
            }
            Err(e) => return Err(login(format!("ID token rejected: {e}"))),
        };
        Ok(IdpUser {
            issuer: claims.issuer().to_string(),
            subject: claims.subject().to_string(),
            email: claims.email().map(|e| e.to_string().trim().to_lowercase()),
            email_verified: claims.email_verified() == Some(true),
            claims: serde_json::to_value(&claims).unwrap_or(Value::Null),
        })
    }

    fn github_start(&self, cfg: &IdpConfig) -> Result<IdpStart, IdpError> {
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let state = CsrfToken::new_random();
        let mut url = openidconnect::url::Url::parse(&self.github.authorize).map_err(setup)?;
        url.query_pairs_mut()
            .append_pair("client_id", &cfg.client_id)
            .append_pair("redirect_uri", &cfg.redirect_uri)
            .append_pair("scope", &cfg.scopes.join(" "))
            .append_pair("state", state.secret())
            .append_pair("code_challenge", challenge.as_str())
            .append_pair("code_challenge_method", "S256")
            .append_pair("allow_signup", "true");
        Ok(IdpStart {
            url: url.to_string(),
            state: state.secret().clone(),
            nonce: String::new(),
            pkce_verifier: verifier.secret().clone(),
        })
    }

    async fn get_json(&self, url: &str, token: &str) -> Result<Value, IdpError> {
        let res = self
            .http
            .get(url)
            .bearer_auth(token)
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .map_err(login)?;
        if !res.status().is_success() {
            return Err(login(format!("{url} answered {}", res.status())));
        }
        let body = read_limited(res).await.map_err(login)?;
        serde_json::from_slice(&body).map_err(login)
    }

    async fn github_finish(
        &self,
        cfg: &IdpConfig,
        code: &str,
        verifier: &str,
    ) -> Result<IdpUser, IdpError> {
        let res = self
            .http
            .post(&self.github.token)
            .header("Accept", "application/json")
            .form(&[
                ("client_id", cfg.client_id.as_str()),
                ("client_secret", cfg.client_secret.as_str()),
                ("code", code),
                ("redirect_uri", cfg.redirect_uri.as_str()),
                ("code_verifier", verifier),
            ])
            .send()
            .await
            .map_err(login)?;
        let body = read_limited(res).await.map_err(login)?;
        let token: Value = serde_json::from_slice(&body).map_err(login)?;
        let access = token["access_token"].as_str().ok_or_else(|| {
            login(format!(
                "token request failed: {}",
                token["error"].as_str().unwrap_or("no token")
            ))
        })?;
        let api = self.github.api.trim_end_matches('/');
        let user = self.get_json(&format!("{api}/user"), access).await?;
        let id = user["id"]
            .as_u64()
            .map(|n| n.to_string())
            .ok_or_else(|| login("GitHub returned no user id"))?;
        let emails = self
            .get_json(&format!("{api}/user/emails"), access)
            .await
            .unwrap_or(Value::Null);
        let list = emails.as_array().cloned().unwrap_or_default();
        let pick = list
            .iter()
            .find(|e| e["primary"] == true && e["verified"] == true)
            .or_else(|| list.iter().find(|e| e["verified"] == true))
            .or_else(|| list.iter().find(|e| e["primary"] == true));
        let (email, verified) = match pick {
            Some(e) => (
                e["email"].as_str().map(|s| s.trim().to_lowercase()),
                e["verified"] == true,
            ),
            None => (
                user["email"].as_str().map(|s| s.trim().to_lowercase()),
                false,
            ),
        };
        let mut claims = user.clone();
        if let Some(o) = claims.as_object_mut() {
            o.insert("sub".into(), Value::String(id.clone()));
            if let Some(n) = user["name"].as_str().or_else(|| user["login"].as_str()) {
                o.insert("name".into(), Value::String(n.to_owned()));
            }
            if let Some(p) = user["avatar_url"].as_str() {
                o.insert("picture".into(), Value::String(p.to_owned()));
            }
        }
        Ok(IdpUser {
            issuer: "https://github.com".into(),
            subject: id,
            email,
            email_verified: verified,
            claims,
        })
    }
}

impl IdpClient for Idp {
    fn discover<'a>(&'a self, issuer: &'a str) -> BoxFuture<'a, Result<Value, IdpError>> {
        Box::pin(async move {
            let m = self.metadata(issuer, true).await?;
            serde_json::to_value(&m).map_err(setup)
        })
    }

    fn start<'a>(&'a self, config: &'a IdpConfig) -> BoxFuture<'a, Result<IdpStart, IdpError>> {
        Box::pin(async move {
            match (config.kind.as_str(), config.preset.as_deref()) {
                ("oidc", _) => self.oidc_start(config).await,
                ("oauth2_preset", Some("github")) => self.github_start(config),
                (k, p) => Err(setup(format!(
                    "unsupported provider {k}/{}",
                    p.unwrap_or("none")
                ))),
            }
        })
    }

    fn finish<'a>(
        &'a self,
        config: &'a IdpConfig,
        code: &'a str,
        pkce_verifier: &'a str,
        nonce: &'a str,
    ) -> BoxFuture<'a, Result<IdpUser, IdpError>> {
        Box::pin(async move {
            match (config.kind.as_str(), config.preset.as_deref()) {
                ("oidc", _) => self.oidc_finish(config, code, pkce_verifier, nonce).await,
                ("oauth2_preset", Some("github")) => {
                    self.github_finish(config, code, pkce_verifier).await
                }
                (k, p) => Err(setup(format!(
                    "unsupported provider {k}/{}",
                    p.unwrap_or("none")
                ))),
            }
        })
    }
}
