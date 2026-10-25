//! Argon2id password hashes.
//!
//! Hashing costs tens of milliseconds of CPU and 19 MiB of memory by
//! design, so both operations run on tokio's blocking pool instead of
//! stalling an async worker.

use std::fmt;
use std::sync::LazyLock;

use argon2::Argon2;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{Error as HashError, PasswordHasher, PasswordVerifier};
use color_eyre::eyre::{Result, WrapErr, eyre};

/// A plaintext password from a request. Its `Debug` output is redacted.
#[derive(Clone, PartialEq, Eq)]
pub struct Password(String);

impl Password {
    pub fn new(password: String) -> Self {
        Self(password)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Password(<redacted>)")
    }
}

/// Argon2id v19 with the crate's defaults (19 MiB, 2 passes, 1 lane), the
/// OWASP minimum. The parameters travel inside each PHC string, so changing
/// them later still verifies old hashes.
// TODO: rehash on login when a stored hash uses weaker parameters than the
// current ones.
fn argon2() -> Argon2<'static> {
    Argon2::default()
}

/// Hashes `password` with a fresh random salt into a PHC string
/// (`$argon2id$v=19$...`).
pub async fn hash(password: Password) -> Result<String> {
    tokio::task::spawn_blocking(move || {
        argon2()
            .hash_password(password.0.as_bytes())
            .map(|hash| hash.to_string())
            .map_err(|error| eyre!("{error}"))
    })
    .await
    .context("join the password hashing task")?
    .context("hash the password")
}

/// A hash of a password nobody knows, verified against when the username
/// doesn't exist so that an unknown user costs the same time as a wrong
/// password and login timing doesn't reveal which accounts exist.
static DUMMY_HASH: LazyLock<String> = LazyLock::new(|| {
    argon2()
        .hash_password(b"pod dummy password for unknown users")
        .expect("hashing with default parameters succeeds")
        .to_string()
});

/// Checks `password` against a stored PHC string, or against
/// [`DUMMY_HASH`] when there is none, in which case it is always `false`.
pub async fn verify(password: Password, stored: Option<String>) -> Result<bool> {
    tokio::task::spawn_blocking(move || {
        let known_user = stored.is_some();
        let phc = stored.unwrap_or_else(|| DUMMY_HASH.clone());
        let hash =
            PasswordHash::new(&phc).map_err(|error| eyre!("parse the stored hash: {error}"))?;
        match argon2().verify_password(password.0.as_bytes(), &hash) {
            Ok(()) => Ok(known_user),
            Err(HashError::PasswordInvalid) => Ok(false),
            Err(error) => Err(eyre!("{error}")),
        }
    })
    .await
    .context("join the password verification task")?
    .context("verify the password")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hash_verifies_only_the_same_password() {
        let phc = hash(Password::new("correct horse".into()))
            .await
            .expect("hashing succeeds");
        assert!(phc.starts_with("$argon2id$v=19$"), "{phc}");
        assert!(
            verify(Password::new("correct horse".into()), Some(phc.clone()))
                .await
                .expect("the hash parses")
        );
        assert!(
            !verify(Password::new("wrong horse".into()), Some(phc))
                .await
                .expect("the hash parses")
        );
    }

    #[tokio::test]
    async fn unknown_user_never_verifies() {
        let password = "pod dummy password for unknown users";
        assert!(
            !verify(Password::new(password.into()), None)
                .await
                .expect("the dummy hash parses")
        );
    }
}
