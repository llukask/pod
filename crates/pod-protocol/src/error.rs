//! The error body every endpoint returns on failure (spec section 4).

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// A request-level error code. Per-mutation failures inside a successful
/// sync response use `RejectionCode` instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
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
}

impl ErrorCode {
    /// Every code, for exhaustive tests and documentation.
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
    pub const fn http_status(self) -> u16 {
        match self {
            Self::BadRequest | Self::InvalidCursor => 400,
            Self::Unauthenticated | Self::InvalidCredentials => 401,
            Self::RegistrationDisabled => 403,
            Self::NotFound => 404,
            Self::UsernameTaken => 409,
            Self::BatchTooLarge => 413,
            Self::RateLimited => 429,
            Self::Internal => 500,
            Self::UpstreamUnavailable => 502,
        }
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
                    code.http_status(),
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
