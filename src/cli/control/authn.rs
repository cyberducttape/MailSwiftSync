//! Credential primitives: bearer tokens and password hashes.
//!
//! Bearer tokens are 256 random bits with a type prefix, so a leaked token is
//! recognizable in logs and secret scanners and a session token can never be
//! presented where an API key is expected. Only SHA-256 digests are stored.
//! Passwords are hashed with Argon2id at the RFC 9106 / OWASP defaults.

use argon2::{Argon2, PasswordHasher as _, PasswordVerifier as _};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::rand::SecureRandom as _;
use sha2::Digest as _;
use zeroize::Zeroizing;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TokenKind {
    Session,
    ApiKey,
    Worker,
    Enrollment,
}

impl TokenKind {
    fn prefix(self) -> &'static str {
        match self {
            TokenKind::Session => "mss_s_",
            TokenKind::ApiKey => "mss_k_",
            TokenKind::Worker => "mss_w_",
            TokenKind::Enrollment => "mss_e_",
        }
    }

    /// The kind a presented token claims to be, from its prefix alone.
    pub(super) fn of(token: &str) -> Option<Self> {
        [
            TokenKind::Session,
            TokenKind::ApiKey,
            TokenKind::Worker,
            TokenKind::Enrollment,
        ]
        .into_iter()
        .find(|kind| token.starts_with(kind.prefix()))
    }
}

pub(super) const MIN_PASSWORD_CHARS: usize = 12;
const MAX_PASSWORD_BYTES: usize = 1024;

pub(super) fn random_bytes<const N: usize>() -> Result<Zeroizing<[u8; N]>, String> {
    let mut bytes = Zeroizing::new([0_u8; N]);
    ring::rand::SystemRandom::new()
        .fill(&mut *bytes)
        .map_err(|_| "the system random number generator failed".to_owned())?;
    Ok(bytes)
}

/// A new bearer token of `kind`.
pub(super) fn new_token(kind: TokenKind) -> Result<Zeroizing<String>, String> {
    let bytes = random_bytes::<32>()?;
    Ok(Zeroizing::new(format!(
        "{}{}",
        kind.prefix(),
        URL_SAFE_NO_PAD.encode(*bytes)
    )))
}

/// The stored form of a bearer token.
pub(super) fn token_hash(token: &str) -> [u8; 32] {
    sha2::Sha256::digest(token.as_bytes()).into()
}

/// A short random identifier for rows that are named in URLs and audit
/// entries (users, tenants, keys); never a credential.
pub(super) fn new_id(prefix: &str) -> Result<String, String> {
    let bytes = random_bytes::<12>()?;
    Ok(format!("{prefix}_{}", URL_SAFE_NO_PAD.encode(*bytes)))
}

pub(super) fn validate_new_password(password: &str) -> Result<(), String> {
    if password.chars().count() < MIN_PASSWORD_CHARS {
        return Err(format!(
            "passwords need at least {MIN_PASSWORD_CHARS} characters"
        ));
    }
    if password.len() > MAX_PASSWORD_BYTES {
        return Err(format!(
            "passwords are limited to {MAX_PASSWORD_BYTES} bytes"
        ));
    }
    Ok(())
}

pub(super) fn hash_password(password: &str) -> Result<String, String> {
    validate_new_password(password)?;
    let salt = random_bytes::<16>()?;
    Argon2::default()
        .hash_password_with_salt(password.as_bytes(), &*salt)
        .map(|hash| hash.to_string())
        .map_err(|error| format!("could not hash the password: {error}"))
}

/// Whether `password` matches the stored PHC string. Oversized input is
/// refused before hashing so it cannot be used to burn CPU.
pub(super) fn verify_password(password: &str, stored: &str) -> bool {
    password.len() <= MAX_PASSWORD_BYTES
        && Argon2::default()
            .verify_password(password.as_bytes(), stored)
            .is_ok()
}

/// Spend the same work as a real verification, so a login for an unknown
/// or password-less account takes as long as one with a wrong password.
pub(super) fn equalize_password_timing(password: &str) {
    static DUMMY: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    // A random secret nobody knows, so no password can ever match it.
    let dummy = DUMMY.get_or_init(|| {
        random_bytes::<24>()
            .ok()
            .and_then(|bytes| hash_password(&URL_SAFE_NO_PAD.encode(*bytes)).ok())
    });
    if let Some(stored) = dummy {
        let _ = verify_password(password, stored);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_typed_random_and_stored_only_as_digests() {
        let first = new_token(TokenKind::ApiKey).unwrap();
        let second = new_token(TokenKind::ApiKey).unwrap();
        assert_ne!(*first, *second);
        assert_eq!(TokenKind::of(&first), Some(TokenKind::ApiKey));
        assert_eq!(TokenKind::of("mss_s_abc"), Some(TokenKind::Session));
        assert_eq!(TokenKind::of("Bearer x"), None);
        assert_eq!(first.len(), "mss_k_".len() + 43);
        assert_ne!(token_hash(&first), token_hash(&second));
    }

    #[test]
    fn passwords_hash_with_argon2id_and_verify() {
        let stored = hash_password("correct horse battery").unwrap();
        assert!(stored.starts_with("$argon2id$"));
        assert!(verify_password("correct horse battery", &stored));
        assert!(!verify_password("correct horse batterz", &stored));
        assert!(!verify_password(
            &"x".repeat(MAX_PASSWORD_BYTES + 1),
            &stored
        ));
        assert!(hash_password("short").is_err());
        assert!(!verify_password("anything", "not a phc string"));
    }
}
