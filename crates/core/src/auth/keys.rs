//! Signing keys for the OIDC provider (SPEC §11.8.6): RS256 (2048-bit) and
//! ES256. Private keys are stored encrypted with the SecretBox; the public
//! half is kept as a JWK. The `kid` is the RFC 7638 JWK thumbprint.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use p256::ecdsa::SigningKey as P256Key;
use p256::elliptic_curve::Generate as _;
use p256::pkcs8::EncodePrivateKey as _;
use rand::rngs::SysRng;
use rsa::RsaPrivateKey;
use rsa::traits::PublicKeyParts as _;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Signing algorithm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAlg {
    /// RSASSA-PKCS1-v1_5 with SHA-256, 2048-bit keys.
    Rs256,
    /// ECDSA P-256 with SHA-256.
    Es256,
}

impl KeyAlg {
    /// JOSE name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rs256 => "RS256",
            Self::Es256 => "ES256",
        }
    }
}

/// A freshly generated key pair.
#[derive(Debug)]
pub struct GeneratedKey {
    /// RFC 7638 thumbprint.
    pub kid: String,
    /// Algorithm.
    pub alg: KeyAlg,
    /// PKCS#8 DER of the private key (encrypt before storing).
    pub private_pkcs8: Vec<u8>,
    /// Public JWK (with `kid`, `alg`, `use`).
    pub public_jwk: Value,
}

/// One `_keys` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyRow {
    /// Key id.
    pub kid: String,
    /// `RS256` or `ES256`.
    pub alg: String,
    /// Encrypted PKCS#8 private key.
    pub private_enc: String,
    /// Public JWK JSON.
    pub public_jwk: String,
    /// Creation time.
    pub created: String,
    /// When it became the active signing key.
    pub activated_at: Option<String>,
    /// When it stopped signing.
    pub retired_at: Option<String>,
    /// When it is removed from the JWKS.
    pub delete_after: Option<String>,
}

fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// RFC 7638 thumbprint over the required members in lexicographic order.
fn thumbprint(required: &Value) -> String {
    // serde_json::Map is ordered by key without preserve_order: canonical.
    let canonical = required.to_string();
    b64(&Sha256::digest(canonical.as_bytes()))
}

fn rng() -> rand::rand_core::UnwrapErr<SysRng> {
    rand::rand_core::UnwrapErr(SysRng)
}

/// Generates a key pair. RSA generation takes a moment; call it off the
/// async threads.
pub fn generate(alg: KeyAlg) -> Result<GeneratedKey, crate::Error> {
    let (required, private_pkcs8) = match alg {
        KeyAlg::Rs256 => {
            let key = RsaPrivateKey::new(&mut rng(), 2048)
                .map_err(|e| crate::Error::internal(format!("RSA key generation: {e}")))?;
            let pkcs8 = rsa::pkcs8::EncodePrivateKey::to_pkcs8_der(&key)
                .map_err(|e| crate::Error::internal(format!("RSA key encoding: {e}")))?;
            let required = json!({
                "e": b64(&key.e().to_be_bytes_trimmed_vartime()),
                "kty": "RSA",
                "n": b64(&key.n().to_be_bytes_trimmed_vartime()),
            });
            (required, pkcs8.as_bytes().to_vec())
        }
        KeyAlg::Es256 => {
            let key = P256Key::generate_from_rng(&mut rng());
            let pkcs8 = key
                .to_pkcs8_der()
                .map_err(|e| crate::Error::internal(format!("P-256 key encoding: {e}")))?;
            let point = key.verifying_key().to_sec1_point(false);
            let (Some(x), Some(y)) = (point.x(), point.y()) else {
                return Err(crate::Error::internal(
                    "P-256 public key has no coordinates",
                ));
            };
            let required = json!({ "crv": "P-256", "kty": "EC", "x": b64(x), "y": b64(y) });
            (required, pkcs8.as_bytes().to_vec())
        }
    };
    let kid = thumbprint(&required);
    let mut jwk = required;
    if let Value::Object(m) = &mut jwk {
        m.insert("kid".into(), json!(kid));
        m.insert("alg".into(), json!(alg.as_str()));
        m.insert("use".into(), json!("sig"));
    }
    Ok(GeneratedKey {
        kid,
        alg,
        private_pkcs8,
        public_jwk: jwk,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn es256_jwk() {
        let k = generate(KeyAlg::Es256).unwrap();
        assert_eq!(k.public_jwk["kty"], "EC");
        assert_eq!(k.public_jwk["kid"], k.kid);
        assert_eq!(k.kid.len(), 43);
        assert!(!k.private_pkcs8.is_empty());
    }
}
