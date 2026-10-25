//! The opaque pagination cursor of the read endpoints (spec 4.5).
//!
//! Lists are ordered by `(published_at desc, id desc)`, so a cursor is the
//! last returned item's pair, and the next page is every row strictly below
//! it. Keyset pagination, unlike an offset, neither repeats nor skips rows
//! when episodes arrive between two page requests, and the id breaks ties
//! between equal `published_at` values.
//!
//! The wire form is base64url without padding of 24 bytes: `published_at`
//! as big-endian epoch milliseconds (8 bytes), then the id (16 bytes).
//! Clients treat it as opaque, so the encoding can change; old cursors would
//! then fail with `invalid_cursor` and the client restarts from page one.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use pod_protocol::Timestamp;
use sqlx::types::Uuid;

const ENCODED_LEN: usize = 24;

/// A position in a `(published_at desc, id desc)` list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageCursor {
    pub published_at: Timestamp,
    pub id: Uuid,
}

/// A cursor string that this server didn't produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the cursor doesn't decode")]
pub struct InvalidCursor;

impl PageCursor {
    pub fn encode(&self) -> String {
        let mut bytes = [0; ENCODED_LEN];
        bytes[..8].copy_from_slice(&self.published_at.as_epoch_ms().to_be_bytes());
        bytes[8..].copy_from_slice(self.id.as_bytes());
        URL_SAFE_NO_PAD.encode(bytes)
    }

    pub fn decode(input: &str) -> Result<Self, InvalidCursor> {
        let bytes = URL_SAFE_NO_PAD.decode(input).map_err(|_| InvalidCursor)?;
        let bytes: [u8; ENCODED_LEN] = bytes.try_into().map_err(|_| InvalidCursor)?;
        let (millis, id) = bytes.split_at(8);
        let millis = i64::from_be_bytes(millis.try_into().expect("split at 8 bytes"));
        Ok(Self {
            published_at: Timestamp::from_epoch_ms(millis).map_err(|_| InvalidCursor)?,
            id: Uuid::from_slice(id).expect("the rest is 16 bytes"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cursor() -> PageCursor {
        PageCursor {
            published_at: "2026-10-20T05:00:00.120Z".parse().expect("valid RFC 3339"),
            id: "9f3a5c7e-4c1a-4f2e-8b3d-0000000000c1"
                .parse()
                .expect("valid UUID"),
        }
    }

    #[test]
    fn round_trips() {
        let encoded = cursor().encode();
        assert_eq!(encoded.len(), 32);
        assert_eq!(PageCursor::decode(&encoded), Ok(cursor()));
    }

    #[test]
    fn rejects_what_it_did_not_produce() {
        let encoded = cursor().encode();
        for input in [
            String::new(),
            "!!!!".to_owned(),
            format!("{encoded}="),
            encoded[..31].to_owned(),
            format!("{encoded}AAAA"),
            // Epoch milliseconds far outside the range timestamps support.
            URL_SAFE_NO_PAD.encode([0x7f; ENCODED_LEN]),
        ] {
            assert_eq!(PageCursor::decode(&input), Err(InvalidCursor), "{input}");
        }
    }
}
