// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Passwords, session tokens and device tokens.
//!
//! This is the part of the hub where a mistake is not a bug report but a
//! stranger reading a family's day, so it is deliberately small and dull:
//! Argon2id for passwords, random bytes for everything else, constant-time
//! comparison throughout, and no cleverness anywhere.
//!
//! Tokens are stored as hashes, not as themselves. A hub database that leaks —
//! a backup on a NAS, a stolen Raspberry Pi — then yields nothing that can be
//! replayed against a running server.

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Argon2, password_hash::rand_core::OsRng};
use base64::Engine as _;
use sha2::{Digest as _, Sha256};
use subtle::ConstantTimeEq as _;

/// How many random bytes a token carries. 32 bytes is 256 bits, which is past
/// the point where guessing is the attack anyone would choose.
const TOKEN_BYTES: usize = 32;

/// A secret the client keeps and the server only ever sees hashed.
///
/// `Debug` deliberately hides the value: a token in a log file is a token in
/// whatever reads log files.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Token(hidden)")
    }
}

impl Token {
    /// A fresh token from the operating system's randomness.
    #[must_use]
    pub fn generate() -> Self {
        let mut bytes = [0u8; TOKEN_BYTES];
        // Straight from the operating system. A userspace generator would need
        // seeding, and getting that wrong is a class of bug worth not having.
        getrandom::fill(&mut bytes).expect("the operating system has randomness");
        Self(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
    }

    /// Reads a token a client presented. No validation beyond length: whether
    /// it is *valid* is decided by comparing its hash, never by its shape.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        (raw.len() >= 16 && raw.len() <= 256).then(|| Self(raw.to_owned()))
    }

    /// What goes in the database.
    ///
    /// A plain SHA-256 rather than Argon2: a 256-bit random token has no
    /// structure to guess at, so the slow hash that protects a human-chosen
    /// password buys nothing here and would cost a hash on every request.
    #[must_use]
    pub fn digest(&self) -> String {
        hex(&Sha256::digest(self.0.as_bytes()))
    }

    /// Constant-time comparison against a stored digest.
    #[must_use]
    pub fn matches(&self, stored_digest: &str) -> bool {
        let ours = self.digest();
        ours.as_bytes().ct_eq(stored_digest.as_bytes()).into()
    }

    /// The value to hand to the client. Consumes the token, because handing it
    /// over is the last thing that should happen to it.
    #[must_use]
    pub fn into_secret(self) -> String {
        self.0
    }
}

/// A short code a parent reads off one screen and types into another.
///
/// Deliberately not a token: it is six digits because a person has to copy it,
/// and it is therefore only safe because it expires in minutes and can be used
/// once. Anything longer would be retyped wrongly; anything longer-lived would
/// be guessable.
#[must_use]
pub fn enrolment_code() -> String {
    let mut bytes = [0u8; 4];
    getrandom::fill(&mut bytes).expect("the operating system has randomness");
    // Modulo bias across 2^32 into a million buckets is about one part in four
    // thousand — far below what matters for a code that lives for minutes.
    let n = u32::from_be_bytes(bytes) % 1_000_000;
    format!("{n:06}")
}

/// Hashes a parent's password for storage.
pub fn hash_password(password: &str) -> anyhow::Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| anyhow::anyhow!("cannot hash password: {e}"))
}

/// Checks a password against a stored hash.
///
/// Returns false for a malformed stored hash rather than erroring: a corrupt
/// row should deny access, not crash the login endpoint for everyone.
#[must_use]
pub fn verify_password(password: &str, stored: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(stored) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

/// Reasons a password is refused when set.
///
/// Length only. Composition rules — a digit, a symbol, a capital — are known to
/// push people towards `Passwort1!` and away from anything memorable and long,
/// so they are not imposed.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PasswordError {
    #[error("a password needs at least {minimum} characters")]
    TooShort { minimum: usize },
    #[error("that password is too common to use")]
    TooCommon,
}

/// The shortest password accepted.
pub const MIN_PASSWORD_LEN: usize = 10;

/// Passwords so common that length alone does not save them.
#[rustfmt::skip]
const REFUSED: &[&str] = &[
    "password12", "passwort12", "1234567890", "qwertzuiop", "qwertyuiop",
    "passwordpassword", "administrator",
];

/// Checks a password a parent is choosing.
pub fn check_password(password: &str) -> Result<(), PasswordError> {
    if password.chars().count() < MIN_PASSWORD_LEN {
        return Err(PasswordError::TooShort {
            minimum: MIN_PASSWORD_LEN,
        });
    }
    let lowered = password.to_lowercase();
    if REFUSED.iter().any(|c| lowered == *c) {
        return Err(PasswordError::TooCommon);
    }
    Ok(())
}

/// Hex, for storing a digest in a text column.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_digest_matches_the_published_vectors() {
        // Kept after swapping a hand-written SHA-256 for the sha2 crate. The
        // point is no longer whether the algorithm is right — it is whether
        // this file uses it correctly, and that is just as easy to get wrong.
        assert_eq!(
            hex(&Sha256::digest(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(&Sha256::digest(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex(&Sha256::digest(
                b"The quick brown fox jumps over the lazy dog"
            )),
            "d7a8fbb307d7809469ca9abcb0082e4f8d5651e46d3cdb762d02d0bf37c9e592"
        );
        assert_eq!(
            hex(&Sha256::digest([b'a'; 1000])),
            "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3"
        );
    }

    #[test]
    fn tokens_are_unique_and_long_enough_to_be_dull() {
        let a = Token::generate();
        let b = Token::generate();
        assert_ne!(a, b);
        assert!(a.clone().into_secret().len() >= 40, "256 bits, base64");
    }

    #[test]
    fn a_token_never_prints_itself() {
        // A token in a log is a token in whatever reads logs.
        let token = Token::generate();
        let secret = token.clone().into_secret();
        let printed = format!("{token:?}");
        assert!(!printed.contains(&secret), "{printed}");
        assert_eq!(printed, "Token(hidden)");
    }

    #[test]
    fn a_token_matches_only_its_own_digest() {
        let token = Token::generate();
        let stored = token.digest();
        assert!(token.matches(&stored));
        assert!(!Token::generate().matches(&stored));
        assert!(!token.matches("not a digest"));
        assert!(!token.matches(""));
    }

    #[test]
    fn the_stored_digest_is_not_the_token() {
        // The point of storing a hash: a leaked database yields nothing that
        // can be replayed.
        let token = Token::generate();
        let digest = token.digest();
        assert!(!digest.contains(&token.clone().into_secret()));
        assert_eq!(digest.len(), 64, "hex sha-256");
    }

    #[test]
    fn presented_tokens_are_bounded_in_length() {
        assert!(Token::parse("short").is_none());
        assert!(Token::parse(&"x".repeat(1000)).is_none());
        assert!(Token::parse(&Token::generate().into_secret()).is_some());
        // Trimmed, because a token pasted from a terminal carries a newline.
        let secret = Token::generate().into_secret();
        assert_eq!(
            Token::parse(&format!("  {secret}\n")).map(Token::into_secret),
            Some(secret)
        );
    }

    #[test]
    fn enrolment_codes_are_six_digits_a_person_can_retype() {
        for _ in 0..50 {
            let code = enrolment_code();
            assert_eq!(code.len(), 6, "{code}");
            assert!(code.chars().all(|c| c.is_ascii_digit()), "{code}");
        }
    }

    #[test]
    fn a_password_verifies_against_its_own_hash_and_nothing_else() {
        let stored = hash_password("richtiges pferd batterie klammer").unwrap();
        assert!(verify_password("richtiges pferd batterie klammer", &stored));
        assert!(!verify_password(
            "Richtiges Pferd Batterie Klammer",
            &stored
        ));
        assert!(!verify_password("", &stored));
    }

    #[test]
    fn the_same_password_hashes_differently_every_time() {
        // Salted, so two parents choosing the same password do not produce the
        // same row.
        let a = hash_password("richtiges pferd batterie klammer").unwrap();
        let b = hash_password("richtiges pferd batterie klammer").unwrap();
        assert_ne!(a, b);
        assert!(verify_password("richtiges pferd batterie klammer", &a));
        assert!(verify_password("richtiges pferd batterie klammer", &b));
    }

    #[test]
    fn a_corrupt_stored_hash_denies_rather_than_crashes() {
        // One damaged row must not take the login endpoint down for everyone.
        assert!(!verify_password("anything", ""));
        assert!(!verify_password("anything", "not-a-phc-string"));
        assert!(!verify_password("anything", "$argon2id$v=19$m=1,t=1,p=1$"));
    }

    #[test]
    fn passwords_are_judged_on_length_not_on_punctuation() {
        // Composition rules push people towards Passwort1! and away from
        // anything long and memorable.
        assert!(check_password("richtiges pferd batterie klammer").is_ok());
        assert_eq!(
            check_password("kurz"),
            Err(PasswordError::TooShort {
                minimum: MIN_PASSWORD_LEN
            })
        );
        // Long enough, and still a terrible idea.
        assert_eq!(check_password("qwertzuiop"), Err(PasswordError::TooCommon));
        assert_eq!(check_password("Passwort12"), Err(PasswordError::TooCommon));
    }
}
