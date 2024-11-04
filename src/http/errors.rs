use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect};
use thiserror::Error;

use crate::feed::GetFeedError;

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("SQL error: {0}")]
    SQL(#[from] sqlx::Error),
    #[error("HTTP request error: {0}")]
    Request(#[from] reqwest::Error),
    #[error("OAuth token error: {0}")]
    TokenError(
        #[from]
        oauth2::RequestTokenError<
            oauth2::reqwest::Error<reqwest::Error>,
            oauth2::StandardErrorResponse<oauth2::basic::BasicErrorResponseType>,
        >,
    ),
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

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let response = match self {
            Self::SQL(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
            Self::Request(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
            Self::TokenError(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
            Self::Unauthorized => return Redirect::to("/").into_response(),
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
