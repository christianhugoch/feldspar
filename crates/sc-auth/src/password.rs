//! Password hashing with argon2id (technical design §7.1).
//!
//! This module owns the *write* side of credentials — turning a plaintext
//! password into a stored hash — which the create-first-user flow needs to
//! populate [`password_hash`](super::COL_PASSWORD_HASH). Verification (the read
//! side, used at login) lands with the login flow in a later Phase 5 item.
//!
//! Hashes are stored as PHC strings (`$argon2id$v=19$m=…,t=…,p=…$salt$hash`),
//! which embed the algorithm, version, parameters, and a random per-password
//! salt, so a stored hash is self-describing and future parameter changes stay
//! backward-compatible.

use argon2::Argon2;
use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, SaltString};
use sc_error::{Error, Result};

/// Hash a plaintext password with argon2id (a fresh random salt each call) and
/// return the PHC string to store in the `password_hash` column.
///
/// [`Argon2::default`] is argon2id at version 0x13 with the crate's recommended
/// default parameters.
pub fn hash_password(plaintext: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(plaintext.as_bytes(), &salt)
        .map_err(|e| Error::auth(format!("hashing password failed: {e}")))?;
    Ok(hash.to_string())
}

/// Whether `hash` is a well-formed PHC password-hash string. Used to reject
/// obviously-corrupt stored hashes before they reach verification.
pub fn is_valid_hash(hash: &str) -> bool {
    PasswordHash::new(hash).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_argon2id_phc_and_parseable() {
        let hash = hash_password("correct horse battery staple").unwrap();
        assert!(hash.starts_with("$argon2id$"), "got {hash}");
        assert!(is_valid_hash(&hash));
    }

    #[test]
    fn hashing_uses_a_random_salt() {
        // The same password hashed twice yields different strings (distinct salts).
        let a = hash_password("hunter2").unwrap();
        let b = hash_password("hunter2").unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn garbage_is_not_a_valid_hash() {
        assert!(!is_valid_hash("not-a-hash"));
        assert!(!is_valid_hash(""));
    }
}
