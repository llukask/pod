//! The error every handler returns, rendered in the shared error shape
//! (spec section 4).

use std::borrow::Cow;

use axum::Json;
use axum::extract::{FromRequest, Request};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use color_eyre::eyre::Report;
use pod_protocol::{ErrorBody, ErrorCode};
use serde::de::DeserializeOwned;

use super::rate_limit::RetryAfter;

/// A failed request. Construct it with the named constructors, or with `?`
/// on an eyre `Result` for anything unexpected, which becomes a `500
/// internal` whose report is logged but never sent.
#[derive(Debug)]
pub struct ApiError(Kind);

#[derive(Debug)]
enum Kind {
    /// Any code except `rate_limited`, which needs its header, and
    /// `internal`, which carries a report to log.
    Client {
        code: ErrorCode,
        message: Cow<'static, str>,
    },
    RateLimited(RetryAfter),
    Internal(Report),
}

impl ApiError {
    fn client(code: ErrorCode, message: impl Into<Cow<'static, str>>) -> Self {
        Self(Kind::Client {
            code,
            message: message.into(),
        })
    }

    pub fn bad_request(message: impl Into<Cow<'static, str>>) -> Self {
        Self::client(ErrorCode::BadRequest, message)
    }

    pub fn unauthenticated() -> Self {
        Self::client(
            ErrorCode::Unauthenticated,
            "Missing, unknown or expired bearer token.",
        )
    }

    pub fn invalid_credentials() -> Self {
        Self::client(
            ErrorCode::InvalidCredentials,
            "Username or password is wrong.",
        )
    }

    pub fn registration_disabled() -> Self {
        Self::client(
            ErrorCode::RegistrationDisabled,
            "This server doesn't accept new accounts.",
        )
    }

    pub fn username_taken() -> Self {
        Self::client(ErrorCode::UsernameTaken, "That username is taken.")
    }

    pub fn not_found() -> Self {
        Self::client(ErrorCode::NotFound, "Not found.")
    }

    pub fn batch_too_large(message: impl Into<Cow<'static, str>>) -> Self {
        Self::client(ErrorCode::BatchTooLarge, message)
    }

    /// The podcast directory failed. The caller logs why; the client only
    /// learns that retrying later may help.
    pub fn upstream_unavailable() -> Self {
        Self::client(
            ErrorCode::UpstreamUnavailable,
            "The podcast directory is unavailable, try again later.",
        )
    }

    pub fn rate_limited(retry_after: RetryAfter) -> Self {
        Self(Kind::RateLimited(retry_after))
    }
}

impl From<Report> for ApiError {
    fn from(report: Report) -> Self {
        Self(Kind::Internal(report))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (code, message, retry_after) = match self.0 {
            Kind::Client { code, message } => (code, message, None),
            Kind::RateLimited(retry_after) => (
                ErrorCode::RateLimited,
                "Too many attempts, try again later.".into(),
                Some(retry_after),
            ),
            Kind::Internal(report) => {
                tracing::error!(error = ?report, "request failed");
                (ErrorCode::Internal, "Internal server error.".into(), None)
            }
        };
        let status = StatusCode::from_u16(code.http_status())
            .expect("every error code maps to a valid status");
        let mut response = (status, Json(ErrorBody::new(code, message))).into_response();
        if let Some(retry_after) = retry_after {
            response.headers_mut().insert(
                header::RETRY_AFTER,
                HeaderValue::from(retry_after.as_secs_ceil()),
            );
        }
        response
    }
}

/// `axum::Json`, except that a malformed body is a `400 bad_request` in the
/// shared shape rather than axum's plain-text rejection. A missing or wrong
/// `Content-Type` (axum's 415) is a `bad_request` too, since the spec's
/// table has no other code for it.
#[derive(Debug, Clone, Copy, Default)]
pub struct ApiJson<T>(pub T);

impl<T, S> FromRequest<S> for ApiJson<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, ApiError> {
        match Json::<T>::from_request(request, state).await {
            Ok(Json(value)) => Ok(Self(value)),
            Err(rejection) => Err(ApiError::bad_request(rejection.body_text())),
        }
    }
}
