//! Password hashing with argon2id (SPEC §11.2).

use std::sync::OnceLock;

use argon2::Argon2;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};

/// Maximum password length.
pub const MAX_LEN: usize = 256;

/// Hashes with argon2id, m=19456 KiB, t=2, p=1, random 16-byte salt, PHC string.
pub fn hash(password: &str) -> Result<String, crate::Error> {
    Argon2::default()
        .hash_password(password.as_bytes())
        .map(|h| h.to_string())
        .map_err(|e| crate::Error::internal(format!("password hashing failed: {e}")))
}

/// Verifies `password` against a PHC string. Malformed hashes never verify.
pub fn verify(password: &str, phc: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(phc) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

/// Runs a verification against a fixed hash so unknown accounts take as long
/// as known ones (SPEC §11.2).
pub fn dummy_verify(password: &str) {
    static DUMMY: OnceLock<String> = OnceLock::new();
    let h = DUMMY.get_or_init(|| hash("rackend-dummy-password").unwrap_or_default());
    let _ = verify(password, h);
}

/// The bundled list of the 10,000 most common passwords (SecLists, MIT;
/// ADR 0011), lowercased.
fn common_passwords() -> &'static std::collections::HashSet<String> {
    static LIST: OnceLock<std::collections::HashSet<String>> = OnceLock::new();
    LIST.get_or_init(|| {
        include_str!("../../data/common-passwords.txt")
            .lines()
            .map(|l| l.trim().to_lowercase())
            .filter(|l| !l.is_empty())
            .collect()
    })
}

/// Whether `password` is on the common-password list (case-insensitive).
pub fn is_common(password: &str) -> bool {
    common_passwords().contains(&password.to_lowercase())
}

/// Checks the policy (SPEC §11.2): `min_len..=256` characters, not on the
/// common-password list, no composition rules.
pub fn check_policy(password: &str, min_len: usize) -> Result<(), crate::error::FieldError> {
    let n = password.chars().count();
    if n < min_len {
        return Err(crate::error::FieldError::new(
            "too_short",
            format!("Must be at least {min_len} characters."),
        )
        .param("min", min_len));
    }
    if n > MAX_LEN {
        return Err(crate::error::FieldError::new(
            "too_long",
            format!("Must be at most {MAX_LEN} characters."),
        )
        .param("max", MAX_LEN));
    }
    if is_common(password) {
        return Err(crate::error::FieldError::new(
            "too_common",
            "This password is on a list of commonly used passwords. Choose another.",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn hash_and_verify() {
        let h = hash("correct horse battery").unwrap();
        assert!(h.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"));
        assert!(verify("correct horse battery", &h));
        assert!(!verify("wrong", &h));
        assert!(!verify("x", "not a hash"));
        assert!(check_policy("short", 10).is_err());
        assert!(check_policy("long enough!", 10).is_ok());
        assert_eq!(
            check_policy("1234567890", 10).unwrap_err().code,
            "too_common"
        );
        assert_eq!(
            check_policy("QWERTYUIOP", 10).unwrap_err().code,
            "too_common",
            "case-insensitive"
        );
    }
}
