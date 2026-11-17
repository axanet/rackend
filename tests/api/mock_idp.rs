//! An in-process OpenID provider for broker tests: discovery, JWKS, an
//! authorization endpoint that answers at once with the configured user, and
//! a token endpoint that checks PKCE and signs ES256 ID tokens.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Form, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use p256::ecdsa::signature::Signer as _;
use p256::ecdsa::{Signature, SigningKey};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

type Params = HashMap<String, String>;

struct Key {
    kid: String,
    key: SigningKey,
}

impl Key {
    fn new(seed: u8) -> Self {
        let key = SigningKey::from_slice(&[seed; 32]).expect("valid scalar");
        Self {
            kid: format!("k{seed}"),
            key,
        }
    }

    fn jwk(&self) -> Value {
        let point = self.key.verifying_key().to_sec1_point(false);
        json!({
            "kty": "EC", "crv": "P-256", "use": "sig", "alg": "ES256", "kid": self.kid,
            "x": URL_SAFE_NO_PAD.encode(point.x().unwrap()),
            "y": URL_SAFE_NO_PAD.encode(point.y().unwrap()),
        })
    }

    fn sign(&self, claims: &Value) -> String {
        let header = json!({"alg": "ES256", "typ": "JWT", "kid": self.kid});
        let input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        let sig: Signature = self.key.sign(input.as_bytes());
        format!("{input}.{}", URL_SAFE_NO_PAD.encode(sig.to_bytes()))
    }
}

struct Grant {
    challenge: String,
    nonce: String,
    client_id: String,
    claims: Value,
}

struct Inner {
    base: String,
    keys: Vec<Key>,
    next: Value,
    grants: HashMap<String, Grant>,
    /// Sign with a wrong nonce (replay attempt).
    bad_nonce: bool,
    /// The authorization request parameters seen last.
    last_authorize: Params,
    jwks_fetches: u32,
}

/// The provider; `base` is its issuer URL.
#[derive(Clone)]
pub struct MockIdp {
    pub base: String,
    inner: Arc<Mutex<Inner>>,
}

impl MockIdp {
    pub async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let inner = Arc::new(Mutex::new(Inner {
            base: base.clone(),
            keys: vec![Key::new(7)],
            next: json!({}),
            grants: HashMap::new(),
            bad_nonce: false,
            last_authorize: Params::new(),
            jwks_fetches: 0,
        }));
        let app = Router::new()
            .route("/.well-known/openid-configuration", get(discovery))
            .route("/jwks", get(jwks))
            .route("/authorize", get(authorize))
            .route("/token", post(token))
            .with_state(Arc::clone(&inner));
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self { base, inner }
    }

    /// The user the next sign-in returns (`sub`, `email`, `email_verified`, …).
    pub fn next_user(&self, claims: Value) {
        self.inner.lock().unwrap().next = claims;
    }

    pub fn bad_nonce(&self, on: bool) {
        self.inner.lock().unwrap().bad_nonce = on;
    }

    /// Replaces the signing key (the old one leaves the JWKS).
    pub fn rotate_key(&self, seed: u8) {
        self.inner.lock().unwrap().keys = vec![Key::new(seed)];
    }

    pub fn last_authorize(&self) -> Params {
        self.inner.lock().unwrap().last_authorize.clone()
    }

    pub fn jwks_fetches(&self) -> u32 {
        self.inner.lock().unwrap().jwks_fetches
    }
}

type S = State<Arc<Mutex<Inner>>>;

async fn discovery(State(s): S) -> Response {
    let base = s.lock().unwrap().base.clone();
    axum::Json(json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/authorize"),
        "token_endpoint": format!("{base}/token"),
        "jwks_uri": format!("{base}/jwks"),
        "response_types_supported": ["code"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["ES256"],
        "scopes_supported": ["openid", "email", "profile"],
        "code_challenge_methods_supported": ["S256"],
    }))
    .into_response()
}

async fn jwks(State(s): S) -> Response {
    let mut g = s.lock().unwrap();
    g.jwks_fetches += 1;
    let keys: Vec<Value> = g.keys.iter().map(Key::jwk).collect();
    axum::Json(json!({ "keys": keys })).into_response()
}

async fn authorize(State(s): S, Query(q): Query<Params>) -> Response {
    let mut g = s.lock().unwrap();
    g.last_authorize = q.clone();
    let redirect = q["redirect_uri"].clone();
    let state = q.get("state").cloned().unwrap_or_default();
    if g.next.get("deny").is_some() {
        return (
            StatusCode::FOUND,
            [(
                header::LOCATION,
                format!("{redirect}?error=access_denied&state={state}"),
            )],
        )
            .into_response();
    }
    assert_eq!(
        q.get("code_challenge_method").map(String::as_str),
        Some("S256")
    );
    let code = uuid::Uuid::now_v7().to_string();
    let grant = Grant {
        challenge: q["code_challenge"].clone(),
        nonce: q.get("nonce").cloned().unwrap_or_default(),
        client_id: q["client_id"].clone(),
        claims: g.next.clone(),
    };
    g.grants.insert(code.clone(), grant);
    (
        StatusCode::FOUND,
        [(
            header::LOCATION,
            format!("{redirect}?code={code}&state={state}"),
        )],
    )
        .into_response()
}

async fn token(State(s): S, headers: HeaderMap, Form(f): Form<Params>) -> Response {
    let mut g = s.lock().unwrap();
    // The client authenticates (HTTP Basic, the openidconnect default).
    assert!(
        headers.contains_key(header::AUTHORIZATION),
        "no client authentication"
    );
    let Some(grant) = g.grants.remove(&f["code"]) else {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({"error": "invalid_grant"})),
        )
            .into_response();
    };
    let verifier = f.get("code_verifier").cloned().unwrap_or_default();
    if URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())) != grant.challenge {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({"error": "invalid_grant", "error_description": "PKCE"})),
        )
            .into_response();
    }
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let mut claims = json!({
        "iss": g.base,
        "aud": grant.client_id,
        "iat": now,
        "exp": now + 300,
        "nonce": if g.bad_nonce { "replayed".to_owned() } else { grant.nonce },
    });
    for (k, v) in grant.claims.as_object().cloned().unwrap_or_default() {
        claims[k] = v;
    }
    let id_token = g.keys[0].sign(&claims);
    axum::Json(json!({
        "access_token": "mock-access-token",
        "token_type": "Bearer",
        "expires_in": 300,
        "id_token": id_token,
    }))
    .into_response()
}
