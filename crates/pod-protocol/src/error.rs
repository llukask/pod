//! The error body every endpoint returns on failure (spec section 4).

use std::borrow::Cow;
use std::fmt;

use serde::{Deserialize, Serialize};
use utoipa::openapi::{RefOr, Schema};
use utoipa::{PartialSchema, ToSchema};

use crate::schema::open_code_schema;

/// A request-level error code. Per-mutation failures inside a successful
/// sync response use `RejectionCode` instead.
///
/// The set is open (spec section 4): a newer server may send a code this
/// crate doesn't know, which parses as [`ErrorCode::Unknown`] and
/// re-serializes unchanged. Clients then go by the HTTP status class.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The request body or parameters are malformed, which is a client bug.
    BadRequest,
    /// A read endpoint's pagination cursor doesn't decode.
    InvalidCursor,
    /// The bearer token is missing, unknown or expired.
    Unauthenticated,
    /// Login with a wrong username or password.
    InvalidCredentials,
    /// The server doesn't accept new accounts.
    RegistrationDisabled,
    NotFound,
    UsernameTaken,
    /// A sync request carries more than [`MAX_MUTATIONS`](crate::MAX_MUTATIONS).
    BatchTooLarge,
    /// Sent with a `Retry-After` header.
    RateLimited,
    Internal,
    /// An upstream service the request depends on (the podcast directory)
    /// failed, timed out or answered with something unusable.
    UpstreamUnavailable,
    /// A code from a newer server. Only deserialization creates one, so the
    /// server, which never parses error bodies, can't send it.
    #[serde(untagged)]
    Unknown(UnknownCode),
}

impl ErrorCode {
    /// Every known code, for exhaustive tests and documentation.
    pub const ALL: [Self; 11] = [
        Self::BadRequest,
        Self::InvalidCursor,
        Self::Unauthenticated,
        Self::InvalidCredentials,
        Self::RegistrationDisabled,
        Self::NotFound,
        Self::UsernameTaken,
        Self::BatchTooLarge,
        Self::RateLimited,
        Self::Internal,
        Self::UpstreamUnavailable,
    ];

    /// The HTTP status this code is sent with. A plain number keeps this
    /// crate free of HTTP dependencies.
    ///
    /// `None` for an unknown code: the status a newer server pairs with it is
    /// on the response itself, and that is what a client falls back on.
    pub const fn http_status(&self) -> Option<u16> {
        Some(match self {
            Self::BadRequest | Self::InvalidCursor => 400,
            Self::Unauthenticated | Self::InvalidCredentials => 401,
            Self::RegistrationDisabled => 403,
            Self::NotFound => 404,
            Self::UsernameTaken => 409,
            Self::BatchTooLarge => 413,
            Self::RateLimited => 429,
            Self::Internal => 500,
            Self::UpstreamUnavailable => 502,
            Self::Unknown(_) => return None,
        })
    }
}

/// A code string this crate doesn't know, kept verbatim for logging and
/// re-serialization. The field is private so that only deserialization
/// produces one; code that emits codes can only name known variants.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UnknownCode(String);

impl UnknownCode {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for UnknownCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl PartialSchema for ErrorCode {
    fn schema() -> RefOr<Schema> {
        open_code_schema(
            "A request-level error code. Per-mutation failures inside a successful \
             sync response use `RejectionCode` instead.",
            &Self::ALL,
        )
    }
}

impl ToSchema for ErrorCode {
    fn name() -> Cow<'static, str> {
        Cow::Borrowed("ErrorCode")
    }
}

/// `{ "error": { "code", "message" } }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

/// The payload of an `ErrorBody`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ErrorDetail {
    pub code: ErrorCode,
    /// Human-readable English text. Clients branch on `code`, never on this.
    pub message: String,
}

impl ErrorBody {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            error: ErrorDetail {
                code,
                message: message.into(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_body_wire_shape() {
        let body = ErrorBody::new(
            ErrorCode::InvalidCredentials,
            "Username or password is wrong.",
        );
        insta::assert_json_snapshot!(body, @r#"
        {
          "error": {
            "code": "invalid_credentials",
            "message": "Username or password is wrong."
          }
        }
        "#);
        let json = serde_json::to_string(&body).expect("error bodies serialize");
        assert_eq!(
            serde_json::from_str::<ErrorBody>(&json).expect("error bodies deserialize"),
            body
        );
    }

    /// A code added by a newer server parses and re-serializes as it came,
    /// so an older client can log it and fall back on the HTTP status.
    #[test]
    fn unknown_code_round_trips_unchanged() {
        let body: ErrorBody = serde_json::from_value(serde_json::json!({
            "error": { "code": "account_suspended", "message": "Ask your admin." }
        }))
        .expect("unknown codes deserialize");
        let ErrorCode::Unknown(code) = &body.error.code else {
            panic!("expected an unknown code, got {:?}", body.error.code);
        };
        assert_eq!(code.as_str(), "account_suspended");
        assert_eq!(body.error.code.http_status(), None);
        assert_eq!(
            serde_json::to_value(&body).expect("error bodies serialize"),
            serde_json::json!({
                "error": { "code": "account_suspended", "message": "Ask your admin." }
            })
        );
    }

    #[test]
    fn every_code_round_trips_and_maps_to_its_status() {
        let table: Vec<(String, u16)> = ErrorCode::ALL
            .iter()
            .map(|code| {
                let json = serde_json::to_value(code).expect("codes serialize");
                assert_eq!(
                    serde_json::from_value::<ErrorCode>(json.clone()).expect("codes deserialize"),
                    *code
                );
                (
                    json.as_str().expect("codes are strings").to_owned(),
                    code.http_status().expect("known codes have a status"),
                )
            })
            .collect();
        insta::assert_json_snapshot!(table, @r#"
        [
          [
            "bad_request",
            400
          ],
          [
            "invalid_cursor",
            400
          ],
          [
            "unauthenticated",
            401
          ],
          [
            "invalid_credentials",
            401
          ],
          [
            "registration_disabled",
            403
          ],
          [
            "not_found",
            404
          ],
          [
            "username_taken",
            409
          ],
          [
            "batch_too_large",
            413
          ],
          [
            "rate_limited",
            429
          ],
          [
            "internal",
            500
          ],
          [
            "upstream_unavailable",
            502
          ]
        ]
        "#);
    }
}
