//! `/v1/connect/*` (spec 14). The operations live in [`crate::connect`];
//! this module maps them to responses and turns a device's events into a
//! server-sent event stream.

use std::convert::Infallible;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::http::header::{CACHE_CONTROL, HeaderName, HeaderValue};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::Stream;
use pod_protocol::{
    ConnectCommand, ConnectReport, ConnectState, DeviceKind, Playback, TransferRequest,
};
use serde::Deserialize;

use super::AppState;
use super::error::{ApiError, ApiJson};
use crate::auth::AuthSession;
use crate::connect::{ConnectError, Delivery};
use crate::sonos::SonosError;

/// How often an idle stream carries a `: ping` comment.
const PING: Duration = Duration::from_secs(15);

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/connect", get(state))
        .route("/v1/connect/events", get(events))
        .route("/v1/connect/report", post(report))
        .route("/v1/connect/command", post(command))
        .route("/v1/connect/transfer", post(transfer))
}

fn api_error(user: &AuthSession, error: ConnectError) -> ApiError {
    match error {
        ConnectError::NotFound => ApiError::not_found(),
        ConnectError::NoSession => ApiError::no_playback(),
        ConnectError::BadRequest(message) => ApiError::bad_request(message),
        ConnectError::Sonos(SonosError::NotFound) => ApiError::not_found(),
        ConnectError::Sonos(SonosError::NoSession) => ApiError::no_playback(),
        ConnectError::Sonos(SonosError::Speaker(error)) => {
            tracing::warn!(
                user_id = %user.user_id,
                error = format!("{:#}", color_eyre::eyre::Report::new(error)),
                "a Sonos speaker failed"
            );
            ApiError::speaker_unavailable()
        }
        ConnectError::Sonos(SonosError::Internal(report)) | ConnectError::Internal(report) => {
            report.into()
        }
    }
}

#[derive(Debug, Deserialize)]
struct EventsQuery {
    kind: Option<String>,
}

/// Opens the device's event stream (spec 14.2). The device stays available
/// until the stream ends, which drops its guard.
async fn events(
    State(state): State<AppState>,
    session: AuthSession,
    Query(query): Query<EventsQuery>,
) -> Result<Response, ApiError> {
    let kind = query
        .kind
        .as_deref()
        .and_then(|kind| serde_json::from_value::<DeviceKind>(kind.into()).ok())
        .ok_or_else(|| ApiError::bad_request("kind must be web, desktop or android"))?;
    let (receiver, guard) = state
        .connect
        .attach(session.user_id, session.device_id, kind)
        .await
        .map_err(|error| api_error(&session, error))?;
    let stream = futures_util::stream::unfold((receiver, guard), |(mut receiver, guard)| async {
        let event = receiver.recv().await?;
        let data = Event::default().data(event.to_json());
        Some((Ok::<_, Infallible>(data), (receiver, guard)))
    });
    let sse = Sse::new(stream).keep_alive(KeepAlive::new().interval(PING).text("ping"));
    Ok(with_stream_headers(sse).into_response())
}

/// Keeps nginx from buffering the stream, and caches from storing it.
fn with_stream_headers<S>(sse: Sse<S>) -> impl IntoResponse
where
    S: Stream<Item = Result<Event, Infallible>> + Send + 'static,
{
    (
        [
            (
                HeaderName::from_static("x-accel-buffering"),
                HeaderValue::from_static("no"),
            ),
            (CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
        sse,
    )
}

async fn state(
    State(state): State<AppState>,
    session: AuthSession,
) -> Result<Json<ConnectState>, ApiError> {
    let connect_state = state
        .connect
        .state(session.user_id)
        .await
        .map_err(|error| api_error(&session, error))?;
    Ok(Json(connect_state))
}

async fn report(
    State(state): State<AppState>,
    session: AuthSession,
    ApiJson(report): ApiJson<ConnectReport>,
) -> Result<Json<Playback>, ApiError> {
    let playback = state
        .connect
        .report(session.user_id, session.device_id, report)
        .await
        .map_err(|error| api_error(&session, error))?;
    Ok(Json(playback))
}

async fn command(
    State(state): State<AppState>,
    session: AuthSession,
    ApiJson(command): ApiJson<ConnectCommand>,
) -> Result<(StatusCode, Json<Playback>), ApiError> {
    let (delivery, playback) = state
        .connect
        .command(session.user_id, command)
        .await
        .map_err(|error| api_error(&session, error))?;
    let status = match delivery {
        Delivery::Done => StatusCode::OK,
        Delivery::Sent => StatusCode::ACCEPTED,
    };
    Ok((status, Json(playback)))
}

async fn transfer(
    State(state): State<AppState>,
    session: AuthSession,
    ApiJson(request): ApiJson<TransferRequest>,
) -> Result<Json<Playback>, ApiError> {
    let playback = state
        .connect
        .transfer(session.user_id, &request.device_id)
        .await
        .map_err(|error| api_error(&session, error))?;
    Ok(Json(playback))
}
