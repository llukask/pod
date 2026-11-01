//! The `/v1/connect` requests (spec 14.3 to 14.5) and the Sonos group list
//! (13.2) on the account's server, with the sync engine's HTTP client.
//! They run on the tokio side.

use std::fmt;
use std::time::Duration;

use pod_protocol::{
    ConnectCommand, ConnectReport, ErrorBody, ErrorCode, Playback, SonosPlayer,
    SonosPlayersResponse, TransferRequest,
};
use reqwest::{Method, StatusCode};

use crate::store::Store;
use crate::sync::session::load_server;

/// A report, a command or the group list. A command for a Sonos group
/// waits for the speaker, which answers within a few seconds.
const TIMEOUT: Duration = Duration::from_secs(15);

/// A transfer waits up to 3 s for the source's release, and a Sonos target
/// loads the episode, which some podcast hosts keep a speaker waiting for
/// for about 30 s.
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(45);

/// Why a Connect request failed.
#[derive(Debug)]
pub enum ConnectError {
    /// `409 no_session`: nothing is active (a command), the device has no
    /// stream (a report), or there is no playback (a transfer).
    NoSession,
    /// `404`: the target device or the episode is gone.
    NotFound,
    /// `502`: the speaker didn't answer or refused.
    Speaker,
    SignedOut,
    /// The server couldn't be reached or gave no usable answer.
    Unreachable,
    /// Another status. The error code set is open, so the status decides.
    Server(StatusCode),
    /// The local store failed.
    Local(color_eyre::Report),
}

impl ConnectError {
    /// What the notification says, about the device named `name`.
    pub fn message(&self, name: &str) -> String {
        match self {
            Self::NoSession => format!("Nothing to play on {name} yet."),
            Self::NotFound => format!("Couldn't play on {name}."),
            Self::Speaker => format!("Couldn't reach {name}."),
            Self::SignedOut => "Sign in again to play on another device.".to_owned(),
            Self::Unreachable => "The server didn't answer.".to_owned(),
            Self::Server(status) => {
                format!("Couldn't play on {name}: the server answered {status}.")
            }
            Self::Local(error) => format!("Couldn't read the account: {error:#}"),
        }
    }
}

impl fmt::Display for ConnectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message("the device"))
    }
}

/// Maps a failed response to its error: the code where it is one the
/// client acts on, else the status alone (spec 4).
fn failure(status: StatusCode, code: Option<&ErrorCode>) -> ConnectError {
    match (status, code) {
        (_, Some(ErrorCode::NoSession)) => ConnectError::NoSession,
        (StatusCode::NOT_FOUND, _) => ConnectError::NotFound,
        (_, Some(ErrorCode::UpstreamUnavailable)) | (StatusCode::BAD_GATEWAY, _) => {
            ConnectError::Speaker
        }
        (StatusCode::UNAUTHORIZED, _) => ConnectError::SignedOut,
        (status, _) => ConnectError::Server(status),
    }
}

/// A command's answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Commanded {
    /// The playback the server answered with.
    pub playback: Playback,
    /// `202`: the command went to a client device over its stream, and
    /// `playback` is from before it ran there. A Sonos group's `200` is the
    /// playback after it.
    pub relayed: bool,
}

/// The Connect endpoints of the account's server. Cloning is cheap.
#[derive(Debug, Clone)]
pub struct Api {
    store: Store,
    http: reqwest::Client,
}

impl Api {
    /// `http` is the sync engine's client.
    pub fn new(store: Store, http: reqwest::Client) -> Api {
        Api { store, http }
    }

    /// Every Sonos group of the household, for the output picker. Empty
    /// when the server has Sonos off.
    pub async fn players(&self) -> Result<Vec<SonosPlayer>, ConnectError> {
        let (_, bytes) = self
            .request(Method::GET, "/v1/sonos/players", None, TIMEOUT)
            .await?;
        decoded::<SonosPlayersResponse>(serde_json::from_slice(&bytes)).map(|body| body.players)
    }

    /// Reports the local player (spec 14.3) and returns the playback after it.
    pub async fn report(&self, report: ConnectReport) -> Result<Playback, ConnectError> {
        let (_, bytes) = self
            .post("/v1/connect/report", serde_json::to_vec(&report), TIMEOUT)
            .await?;
        decoded(serde_json::from_slice(&bytes))
    }

    /// Sends `command` to the active device (spec 14.4).
    pub async fn command(&self, command: ConnectCommand) -> Result<Commanded, ConnectError> {
        let (status, bytes) = self
            .post("/v1/connect/command", serde_json::to_vec(&command), TIMEOUT)
            .await?;
        decoded(serde_json::from_slice(&bytes)).map(|playback| Commanded {
            playback,
            relayed: status == StatusCode::ACCEPTED,
        })
    }

    /// Moves the playback to `device_id` (spec 14.5).
    pub async fn transfer(&self, device_id: String) -> Result<Playback, ConnectError> {
        let (_, bytes) = self
            .post(
                "/v1/connect/transfer",
                serde_json::to_vec(&TransferRequest { device_id }),
                TRANSFER_TIMEOUT,
            )
            .await?;
        decoded(serde_json::from_slice(&bytes))
    }

    async fn post(
        &self,
        path: &str,
        body: serde_json::Result<Vec<u8>>,
        timeout: Duration,
    ) -> Result<(StatusCode, Vec<u8>), ConnectError> {
        let body = body.map_err(|error| ConnectError::Local(error.into()))?;
        self.request(Method::POST, path, Some(body), timeout).await
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
        timeout: Duration,
    ) -> Result<(StatusCode, Vec<u8>), ConnectError> {
        let Some((server_url, token)) = self
            .store
            .read(load_server)
            .await
            .map_err(ConnectError::Local)?
        else {
            return Err(ConnectError::SignedOut);
        };
        let url = format!("{}{path}", server_url.trim_end_matches('/'));
        let mut request = self
            .http
            .request(method, url)
            .bearer_auth(token)
            .timeout(timeout);
        if let Some(body) = body {
            request = request
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body);
        }
        let response = request.send().await.map_err(|error| {
            tracing::info!(%error, path, "the Connect request failed");
            ConnectError::Unreachable
        })?;
        let status = response.status();
        let bytes = response.bytes().await.map_err(|error| {
            tracing::info!(%error, path, "the Connect response broke off");
            ConnectError::Unreachable
        })?;
        if status.is_success() {
            return Ok((status, bytes.to_vec()));
        }
        let code = serde_json::from_slice::<ErrorBody>(&bytes)
            .map(|body| body.error.code)
            .ok();
        tracing::info!(%status, ?code, path, "the server failed the Connect request");
        Err(failure(status, code.as_ref()))
    }
}

fn decoded<T>(body: serde_json::Result<T>) -> Result<T, ConnectError> {
    body.map_err(|error| {
        tracing::warn!(%error, "the Connect response doesn't parse");
        ConnectError::Unreachable
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_go_by_the_known_code_then_the_status() {
        assert!(matches!(
            failure(StatusCode::CONFLICT, Some(&ErrorCode::NoSession)),
            ConnectError::NoSession
        ));
        assert!(matches!(
            failure(StatusCode::NOT_FOUND, Some(&ErrorCode::NotFound)),
            ConnectError::NotFound
        ));
        assert!(matches!(
            failure(
                StatusCode::BAD_GATEWAY,
                Some(&ErrorCode::UpstreamUnavailable)
            ),
            ConnectError::Speaker
        ));
        assert!(matches!(
            failure(StatusCode::BAD_GATEWAY, None),
            ConnectError::Speaker
        ));
        assert!(matches!(
            failure(StatusCode::UNAUTHORIZED, Some(&ErrorCode::Unauthenticated)),
            ConnectError::SignedOut
        ));
        assert!(matches!(
            failure(StatusCode::CONFLICT, None),
            ConnectError::Server(StatusCode::CONFLICT)
        ));
    }

    #[test]
    fn messages_name_the_device() {
        assert_eq!(
            ConnectError::Speaker.message("Couch"),
            "Couldn't reach Couch."
        );
    }
}
