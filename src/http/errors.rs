use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect};
use axum::Json;
use thiserror::Error;

use crate::feed::GetFeedError;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("SQL error: {0}")]
    SQL(#[from] sqlx::Error),
    #[error("HTTP request error: {0}")]
    Request(#[from] reqwest::Error),
    #[error("You're not authorized!")]
    Unauthorized,
    #[error("Attempted to get a non-none value but found none")]
    OptionError,
    #[error("Attempted to parse a number to an integer but errored out: {0}")]
    ParseIntError(#[from] std::num::TryFromIntError),
    #[error("Encountered an error trying to convert an infallible value: {0}")]
    FromRequestPartsError(#[from] std::convert::Infallible),
    #[error("could not get feed: {0}")]
    GetFeedError(#[from] GetFeedError),
    #[error("Not found: {0} with id {1}")]
    NotFound(String, String),
}

impl AppError {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::NotFound(_, _) => StatusCode::NOT_FOUND,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// Web error type — redirects on Unauthorized, plain text for others.
pub type ApiError = AppError;

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        let response = match self {
            Self::SQL(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
            Self::Request(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
            Self::Unauthorized => return Redirect::to("/auth/login").into_response(),
            Self::OptionError => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Attempted to get a non-none value but found none".to_string(),
            ),
            Self::ParseIntError(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
            Self::FromRequestPartsError(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
            Self::GetFeedError(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
            Self::NotFound(kind, id) => (
                StatusCode::NOT_FOUND,
                format!("Not found: {} with id {}", kind, id),
            ),
        };

        response.into_response()
    }
}

/// JSON error type for API handlers.
pub struct JsonAppError(pub AppError);

impl IntoResponse for JsonAppError {
    fn into_response(self) -> axum::response::Response {
        let status = self.0.status_code();
        let body = serde_json::json!({ "error": self.0.to_string() });
        (status, Json(body)).into_response()
    }
}

impl<E> From<E> for JsonAppError
where
    AppError: From<E>,
{
    fn from(err: E) -> Self {
        JsonAppError(AppError::from(err))
    }
}
