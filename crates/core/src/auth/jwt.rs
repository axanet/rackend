//! Compact JWS (JWT) signing and verification with the instance's keys
//! (SPEC §11.8.6): RS256 (PKCS#1 v1.5, blinded) and ES256.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use p256::ecdsa::signature::{Signer as _, Verifier as _};
use rand::rngs::SysRng;
use rsa::signature::{RandomizedSigner as _, SignatureEncoding as _};
use serde_json::{Value, json};
use sha2::Sha256;

use super::keys::KeyAlg;

/// A key that signs and verifies.
#[derive(Clone)]
pub enum KeyMaterial {
    /// RS256.
    Rs256 {
        /// Signer.
        signing: rsa::pkcs1v15::SigningKey<Sha256>,
        /// Verifier.
        verifying: rsa::pkcs1v15::VerifyingKey<Sha256>,
    },
    /// ES256.
    Es256 {
        /// Signer.
        signing: p256::ecdsa::SigningKey,
        /// Verifier.
        verifying: p256::ecdsa::VerifyingKey,
    },
}

impl std::fmt::Debug for KeyMaterial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Rs256 { .. } => "KeyMaterial(RS256)",
            Self::Es256 { .. } => "KeyMaterial(ES256)",
        })
    }
}

/// Why a token was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JwtError {
    /// Not three base64url parts with JSON header and payload.
    #[error("malformed token")]
    Malformed,
    /// No key with this `kid`, or a different algorithm.
    #[error("unknown key")]
    UnknownKey,
    /// The signature does not verify.
    #[error("bad signature")]
    BadSignature,
}

impl KeyMaterial {
    /// From a PKCS#8 DER private key.
    pub fn from_pkcs8(alg: KeyAlg, der: &[u8]) -> Result<Self, String> {
        match alg {
            KeyAlg::Rs256 => {
                let key = <rsa::RsaPrivateKey as rsa::pkcs8::DecodePrivateKey>::from_pkcs8_der(der)
                    .map_err(|e| e.to_string())?;
                let verifying = rsa::pkcs1v15::VerifyingKey::new(key.to_public_key());
                Ok(Self::Rs256 {
                    signing: rsa::pkcs1v15::SigningKey::new(key),
                    verifying,
                })
            }
            KeyAlg::Es256 => {
                let signing =
                    <p256::ecdsa::SigningKey as p256::pkcs8::DecodePrivateKey>::from_pkcs8_der(der)
                        .map_err(|e| e.to_string())?;
                let verifying = *signing.verifying_key();
                Ok(Self::Es256 { signing, verifying })
            }
        }
    }

    /// `RS256` or `ES256`.
    pub fn alg(&self) -> &'static str {
        match self {
            Self::Rs256 { .. } => "RS256",
            Self::Es256 { .. } => "ES256",
        }
    }

    fn sign(&self, input: &[u8]) -> Result<Vec<u8>, String> {
        match self {
            Self::Rs256 { signing, .. } => signing
                .try_sign_with_rng(&mut SysRng, input)
                .map(|s| s.to_vec())
                .map_err(|e| e.to_string()),
            Self::Es256 { signing, .. } => {
                let sig: p256::ecdsa::Signature = signing.sign(input);
                Ok(sig.to_bytes().to_vec())
            }
        }
    }

    fn verify(&self, input: &[u8], sig: &[u8]) -> bool {
        match self {
            Self::Rs256 { verifying, .. } => rsa::pkcs1v15::Signature::try_from(sig)
                .is_ok_and(|s| verifying.verify(input, &s).is_ok()),
            Self::Es256 { verifying, .. } => p256::ecdsa::Signature::from_slice(sig)
                .is_ok_and(|s| verifying.verify(input, &s).is_ok()),
        }
    }
}

/// Signs `claims` as a JWT with `kid`.
pub fn sign(key: &KeyMaterial, kid: &str, typ: &str, claims: &Value) -> Result<String, String> {
    let header = json!({ "alg": key.alg(), "kid": kid, "typ": typ });
    let input = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(header.to_string()),
        URL_SAFE_NO_PAD.encode(claims.to_string())
    );
    let sig = key.sign(input.as_bytes())?;
    Ok(format!("{input}.{}", URL_SAFE_NO_PAD.encode(sig)))
}

/// The header of a token, without verifying anything.
pub fn header(token: &str) -> Result<Value, JwtError> {
    let h = token.split('.').next().ok_or(JwtError::Malformed)?;
    let bytes = URL_SAFE_NO_PAD.decode(h).map_err(|_| JwtError::Malformed)?;
    serde_json::from_slice(&bytes).map_err(|_| JwtError::Malformed)
}

/// Verifies the signature with the key `find` returns for the header's
/// `kid`, and returns the claims. Callers check `iss`, `aud`, `exp`.
pub fn verify<'a>(
    token: &str,
    find: impl Fn(&str) -> Option<&'a KeyMaterial>,
) -> Result<Value, JwtError> {
    let mut parts = token.split('.');
    let (Some(h), Some(p), Some(s), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(JwtError::Malformed);
    };
    let head = header(token)?;
    let kid = head
        .get("kid")
        .and_then(Value::as_str)
        .ok_or(JwtError::UnknownKey)?;
    let key = find(kid).ok_or(JwtError::UnknownKey)?;
    if head.get("alg").and_then(Value::as_str) != Some(key.alg()) {
        return Err(JwtError::UnknownKey);
    }
    let sig = URL_SAFE_NO_PAD.decode(s).map_err(|_| JwtError::Malformed)?;
    if !key.verify(format!("{h}.{p}").as_bytes(), &sig) {
        return Err(JwtError::BadSignature);
    }
    let payload = URL_SAFE_NO_PAD.decode(p).map_err(|_| JwtError::Malformed)?;
    serde_json::from_slice(&payload).map_err(|_| JwtError::Malformed)
}

/// Whether a bearer token looks like a JWT (three parts) rather than an
/// opaque `rk_at_` token.
pub fn looks_like_jwt(token: &str) -> bool {
    token.starts_with("ey") && token.bytes().filter(|b| *b == b'.').count() == 2
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::auth::keys::generate;

    #[test]
    fn round_trip_both_algorithms() {
        for alg in [KeyAlg::Es256, KeyAlg::Rs256] {
            let g = generate(alg).unwrap();
            let key = KeyMaterial::from_pkcs8(alg, &g.private_pkcs8).unwrap();
            let token = sign(&key, &g.kid, "JWT", &json!({ "sub": "u1" })).unwrap();
            assert!(looks_like_jwt(&token));
            let claims = verify(&token, |kid| (kid == g.kid).then_some(&key)).unwrap();
            assert_eq!(claims["sub"], "u1");
            // Tampering is detected.
            let mut bad = token.clone();
            bad.replace_range(
                token.find('.').unwrap() + 2..token.find('.').unwrap() + 3,
                "x",
            );
            assert!(verify(&bad, |_| Some(&key)).is_err());
            assert_eq!(verify(&token, |_| None), Err(JwtError::UnknownKey));
        }
    }
}
