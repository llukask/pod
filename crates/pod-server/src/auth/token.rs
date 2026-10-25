//! Bearer tokens and the hash stored in `session.token_hash` (spec 2.3).

use std::fmt;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

const TOKEN_BYTES: usize = 32;

/// A bearer token: 32 random bytes, base64url encoded without padding. It
/// only exists in memory between generation and the response, or between
/// the `Authorization` header and hashing; its `Debug` output is redacted.
#[derive(Clone, PartialEq, Eq)]
pub struct SessionToken(String);

impl SessionToken {
    pub fn generate() -> Self {
        let mut bytes = [0u8; TOKEN_BYTES];
        getrandom::fill(&mut bytes).expect("the OS random source is available");
        Self(URL_SAFE_NO_PAD.encode(bytes))
    }

    /// Accepts only the exact form [`SessionToken::generate`] produces, so
    /// anything else is rejected before it reaches the database. The base64
    /// engine refuses non-canonical trailing bits, which makes the text and
    /// the bytes correspond one to one.
    pub fn parse(text: &str) -> Option<Self> {
        let bytes = URL_SAFE_NO_PAD.decode(text).ok()?;
        (bytes.len() == TOKEN_BYTES).then(|| Self(text.to_owned()))
    }

    /// The SHA-256 of the token text, which is what the spec's
    /// "sha256 of the bearer token" names. A fast hash is enough because the
    /// token carries 256 bits of entropy, unlike a password.
    pub fn hash(&self) -> TokenHash {
        TokenHash(Sha256::digest(self.0.as_bytes()).into())
    }

    /// The token text, for the one response that hands it to the client.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SessionToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionToken(<redacted>)")
    }
}

/// The primary key of a session row.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct TokenHash(pub [u8; 32]);

/// Not secret, but 32 decimal bytes are noise in a log line.
impl fmt::Debug for TokenHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TokenHash(..)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_tokens_parse_back_and_differ() {
        let token = SessionToken::generate();
        assert_eq!(token.expose().len(), 43);
        assert_eq!(SessionToken::parse(token.expose()), Some(token.clone()));
        assert_ne!(SessionToken::generate(), token);
    }

    #[test]
    fn parse_rejects_other_shapes() {
        let token = SessionToken::generate();
        let padded = format!("{}=", token.expose());
        let standard_alphabet = URL_SAFE_NO_PAD
            .encode([0xfb; 32])
            .replace('-', "+")
            .replace('_', "/");
        for text in [
            "",
            "not a token",
            &padded,
            &token.expose()[..42],
            &URL_SAFE_NO_PAD.encode([0u8; 31]),
            &standard_alphabet,
        ] {
            assert_eq!(SessionToken::parse(text), None, "{text:?}");
        }
    }

    #[test]
    fn debug_redacts_the_token() {
        let token = SessionToken::generate();
        assert!(!format!("{token:?}").contains(token.expose()));
    }
}
