//! Random tokens, hashing and constant-time comparison (SPEC §11.5, §17.1).

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::TryRng as _;
use rand::rngs::SysRng;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// `n` random bytes from the OS CSPRNG.
///
/// # Panics
/// Only if the operating system cannot provide randomness, in which case no
/// secure token can be issued at all.
#[allow(clippy::expect_used)]
pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut buf = [0u8; N];
    // Provably infallible in practice: getrandom only fails when the OS has no
    // entropy source, which is unrecoverable for a security product.
    SysRng
        .try_fill_bytes(&mut buf)
        .expect("OS random number generator unavailable");
    buf
}

/// 32 random bytes, base64url without padding (43 characters).
pub fn random_token() -> String {
    URL_SAFE_NO_PAD.encode(random_bytes::<32>())
}

/// SHA-256 of a token, lowercase hex: what is stored.
pub fn hash_token(token: &str) -> String {
    let d = Sha256::digest(token.as_bytes());
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// Constant-time string equality.
pub fn ct_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

/// Kind of session token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// `rk_at_…`
    Access,
    /// `rk_rt_…`
    Refresh,
}

/// Builds `rk_at_<session_id>_<secret>` / `rk_rt_<session_id>_<secret>`.
pub fn session_token(kind: TokenKind, session_id: &str) -> String {
    let prefix = match kind {
        TokenKind::Access => "rk_at_",
        TokenKind::Refresh => "rk_rt_",
    };
    format!("{prefix}{session_id}_{}", random_token())
}

/// Splits a session token into its session id, if well-formed.
pub fn parse_session_token(kind: TokenKind, token: &str) -> Option<&str> {
    let prefix = match kind {
        TokenKind::Access => "rk_at_",
        TokenKind::Refresh => "rk_rt_",
    };
    let rest = token.strip_prefix(prefix)?;
    let (id, secret) = rest.split_once('_')?;
    (uuid::Uuid::try_parse(id).is_ok() && secret.len() == 43).then_some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_round_trip() {
        let id = uuid::Uuid::now_v7().to_string();
        let t = session_token(TokenKind::Access, &id);
        assert_eq!(
            parse_session_token(TokenKind::Access, &t),
            Some(id.as_str())
        );
        assert_eq!(parse_session_token(TokenKind::Refresh, &t), None);
        assert_ne!(random_token(), random_token());
        assert_eq!(hash_token("x").len(), 64);
        assert!(ct_eq("abc", "abc") && !ct_eq("abc", "abd") && !ct_eq("a", "ab"));
    }
}
