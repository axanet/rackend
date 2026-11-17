//! Secrets at rest (SPEC §17.4): XChaCha20-Poly1305 with a 32-byte key.
//!
//! Stored form: `enc:v1:` + base64url(nonce (24 bytes) || ciphertext). A
//! known value encrypted with the current key (`_meta.secret_check`) lets
//! startup detect a wrong or missing `RACKEND_ENCRYPTION_KEY` before any
//! secret is used.

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};

use crate::auth::token::random_bytes;

const PREFIX: &str = "enc:v1:";
/// Plaintext of the key check value.
const CHECK: &[u8] = b"rackend secret key check v1";

/// Errors of secret handling.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SecretError {
    /// The value is not in `enc:v1:` form.
    #[error("not an encrypted value")]
    Format,
    /// Decryption failed: wrong key or tampered value.
    #[error("cannot decrypt: wrong key or corrupted value")]
    Decrypt,
    /// The key is not 32 bytes of base64.
    #[error(
        "the key must be 32 bytes, base64 encoded (generate one with: openssl rand -base64 32)"
    )]
    BadKey,
}

/// Encrypts and decrypts secrets with one key.
#[derive(Clone)]
pub struct SecretBox {
    cipher: XChaCha20Poly1305,
    /// Key for MACs (file tokens), derived from the secret key.
    mac_key: [u8; 32],
}

impl std::fmt::Debug for SecretBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretBox(..)")
    }
}

impl SecretBox {
    /// A box for `key`.
    pub fn new(key: &[u8; 32]) -> Self {
        use sha2::Digest as _;
        let mut h = sha2::Sha256::new();
        h.update(b"rackend mac key v1\0");
        h.update(key);
        Self {
            cipher: XChaCha20Poly1305::new(key.into()),
            mac_key: h.finalize().into(),
        }
    }

    /// HMAC-SHA256 of `msg` under a key derived from the secret key and
    /// `label` (domain separation). Rotating the secret key invalidates MACs.
    pub fn mac(&self, label: &str, msg: &[u8]) -> [u8; 32] {
        use hmac::{KeyInit as _, Mac as _};
        let Ok(mut m) = hmac::Hmac::<sha2::Sha256>::new_from_slice(&self.mac_key) else {
            return [0; 32];
        };
        m.update(label.as_bytes());
        m.update(b"\0");
        m.update(msg);
        m.finalize().into_bytes().into()
    }

    /// A new random key.
    pub fn generate_key() -> [u8; 32] {
        random_bytes::<32>()
    }

    /// Parses a base64 (standard or url-safe) 32-byte key.
    pub fn parse_key(s: &str) -> Result<[u8; 32], SecretError> {
        let s = s.trim();
        let bytes = STANDARD
            .decode(s)
            .or_else(|_| URL_SAFE_NO_PAD.decode(s.trim_end_matches('=')))
            .map_err(|_| SecretError::BadKey)?;
        bytes.try_into().map_err(|_| SecretError::BadKey)
    }

    /// Encodes a key as standard base64 (for `_meta.local_secret_key`).
    pub fn encode_key(key: &[u8; 32]) -> String {
        STANDARD.encode(key)
    }

    /// Encrypts `plaintext`.
    pub fn encrypt(&self, plaintext: &[u8]) -> String {
        let nonce_bytes = random_bytes::<24>();
        let nonce = XNonce::from(nonce_bytes);
        // Encryption with a valid key and nonce cannot fail for in-memory buffers.
        let ct = self.cipher.encrypt(&nonce, plaintext).unwrap_or_default();
        let mut out = Vec::with_capacity(24 + ct.len());
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ct);
        format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(out))
    }

    /// Decrypts a value produced by [`encrypt`](Self::encrypt).
    pub fn decrypt(&self, value: &str) -> Result<Vec<u8>, SecretError> {
        let raw = value.strip_prefix(PREFIX).ok_or(SecretError::Format)?;
        let bytes = URL_SAFE_NO_PAD
            .decode(raw)
            .map_err(|_| SecretError::Format)?;
        if bytes.len() < 24 + 16 {
            return Err(SecretError::Format);
        }
        let (nonce, ct) = bytes.split_at(24);
        let nonce: [u8; 24] = nonce.try_into().map_err(|_| SecretError::Format)?;
        self.cipher
            .decrypt(&XNonce::from(nonce), ct)
            .map_err(|_| SecretError::Decrypt)
    }

    /// Encrypts a UTF-8 string.
    pub fn encrypt_str(&self, s: &str) -> String {
        self.encrypt(s.as_bytes())
    }

    /// Decrypts to a UTF-8 string.
    pub fn decrypt_str(&self, value: &str) -> Result<String, SecretError> {
        String::from_utf8(self.decrypt(value)?).map_err(|_| SecretError::Decrypt)
    }

    /// The key check value for this key.
    pub fn check_value(&self) -> String {
        self.encrypt(CHECK)
    }

    /// Whether `value` is a key check value made with this key.
    pub fn verify_check(&self, value: &str) -> bool {
        self.decrypt(value).is_ok_and(|v| v == CHECK)
    }
}

/// Whether a stored string is an encrypted value.
pub fn is_encrypted(s: &str) -> bool {
    s.starts_with(PREFIX)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn round_trip_and_wrong_key() {
        let k = SecretBox::generate_key();
        let b = SecretBox::new(&k);
        let e = b.encrypt_str("smtp password");
        assert!(is_encrypted(&e));
        assert_ne!(e, b.encrypt_str("smtp password"), "random nonce");
        assert_eq!(b.decrypt_str(&e).unwrap(), "smtp password");
        let other = SecretBox::new(&SecretBox::generate_key());
        assert_eq!(other.decrypt(&e), Err(SecretError::Decrypt));
        assert!(b.verify_check(&b.check_value()));
        assert!(!other.verify_check(&b.check_value()));
        let mut tampered = e.clone();
        tampered.pop();
        assert!(b.decrypt(&tampered).is_err());
    }

    #[test]
    fn key_parsing() {
        let k = SecretBox::generate_key();
        assert_eq!(SecretBox::parse_key(&SecretBox::encode_key(&k)).unwrap(), k);
        assert_eq!(SecretBox::parse_key("short"), Err(SecretError::BadKey));
    }
}
