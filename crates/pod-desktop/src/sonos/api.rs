//! The `/v1/sonos` requests (spec 13.2) on the account's server, with the
//! sync engine's HTTP client. They run on the tokio side.

use std::fmt;
use std::time::Duration;

use pod_protocol::{
    ErrorBody, ErrorCode, SonosCommand, SonosPlayRequest, SonosPlayer, SonosPlayersResponse,
    SonosReleased,
};
use reqwest::{Method, StatusCode};

use crate::store::Store;
use crate::sync::session::load_server;

/// A list, a command or a release. The server asks the speakers, which
/// answer within a few seconds.
const TIMEOUT: Duration = Duration::from_secs(15);

/// `play` loads the episode on the group, and some podcast hosts keep a
/// speaker waiting for the stream for about 30 s.
const PLAY_TIMEOUT: Duration = Duration::from_secs(45);

/// Why a Sonos request failed.
#[derive(Debug)]
pub enum SonosError {
    /// `409 no_session`, or `404` for a group that is no longer a
    /// coordinator: the session is over.
    Gone,
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

impl SonosError {
    /// What the notification says, about the group named `name`.
    pub fn message(&self, name: &str) -> String {
        match self {
            Self::Gone => format!("Couldn't play on {name}."),
            Self::Speaker => format!("Couldn't reach {name}."),
            Self::SignedOut => "Sign in again to play on a speaker.".to_owned(),
            Self::Unreachable => "The server didn't answer.".to_owned(),
            Self::Server(status) => {
                format!("Couldn't play on {name}: the server answered {status}.")
            }
            Self::Local(error) => format!("Couldn't read the account: {error:#}"),
        }
    }
}

impl fmt::Display for SonosError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message("the speaker"))
    }
}

/// Maps a failed response to its error: the code where it is one the
/// client acts on, else the status alone (spec 4).
fn failure(status: StatusCode, code: Option<&ErrorCode>) -> SonosError {
    match (status, code) {
        (_, Some(ErrorCode::NoSession)) | (StatusCode::NOT_FOUND, _) => SonosError::Gone,
        (_, Some(ErrorCode::UpstreamUnavailable)) | (StatusCode::BAD_GATEWAY, _) => {
            SonosError::Speaker
        }
        (StatusCode::UNAUTHORIZED, _) => SonosError::SignedOut,
        (status, _) => SonosError::Server(status),
    }
}

/// The Sonos endpoints of the account's server. Cloning is cheap.
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

    /// Every group of the household. Empty when the server has Sonos off,
    /// which hides it.
    pub async fn players(&self) -> Result<Vec<SonosPlayer>, SonosError> {
        let bytes = self
            .request(Method::GET, "/v1/sonos/players", None, TIMEOUT)
            .await?;
        decoded::<SonosPlayersResponse>(serde_json::from_slice(&bytes)).map(|body| body.players)
    }

    /// Starts `request.episode_id` on the group `player_id`.
    pub async fn play(
        &self,
        player_id: &str,
        request: SonosPlayRequest,
    ) -> Result<SonosPlayer, SonosError> {
        let body = serde_json::to_vec(&request).map_err(|error| SonosError::Local(error.into()))?;
        let bytes = self
            .request(
                Method::POST,
                &format!("{}/play", player_path(player_id)),
                Some(body),
                PLAY_TIMEOUT,
            )
            .await?;
        decoded(serde_json::from_slice(&bytes))
    }

    pub async fn command(
        &self,
        player_id: &str,
        command: SonosCommand,
    ) -> Result<SonosPlayer, SonosError> {
        let body = serde_json::to_vec(&command).map_err(|error| SonosError::Local(error.into()))?;
        let bytes = self
            .request(
                Method::POST,
                &format!("{}/command", player_path(player_id)),
                Some(body),
                TIMEOUT,
            )
            .await?;
        decoded(serde_json::from_slice(&bytes))
    }

    /// Ends the session on `player_id` and says where it stood.
    pub async fn release(&self, player_id: &str) -> Result<SonosReleased, SonosError> {
        let bytes = self
            .request(
                Method::POST,
                &format!("{}/release", player_path(player_id)),
                None,
                TIMEOUT,
            )
            .await?;
        decoded(serde_json::from_slice(&bytes))
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
        timeout: Duration,
    ) -> Result<Vec<u8>, SonosError> {
        let Some((server_url, token)) = self
            .store
            .read(load_server)
            .await
            .map_err(SonosError::Local)?
        else {
            return Err(SonosError::SignedOut);
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
            tracing::info!(%error, path, "the Sonos request failed");
            SonosError::Unreachable
        })?;
        let status = response.status();
        let bytes = response.bytes().await.map_err(|error| {
            tracing::info!(%error, path, "the Sonos response broke off");
            SonosError::Unreachable
        })?;
        if status.is_success() {
            return Ok(bytes.to_vec());
        }
        let code = serde_json::from_slice::<ErrorBody>(&bytes)
            .map(|body| body.error.code)
            .ok();
        tracing::info!(%status, ?code, path, "the server failed the Sonos request");
        Err(failure(status, code.as_ref()))
    }
}

/// The group's path. Coordinator ids are `RINCON_` and hex, but the server
/// names them, so they are escaped like any path segment.
fn player_path(player_id: &str) -> String {
    let mut url =
        reqwest::Url::parse("http://pod.invalid/v1/sonos/players/").expect("the base URL is valid");
    url.path_segments_mut()
        .expect("an http URL has path segments")
        .pop_if_empty()
        .push(player_id);
    url.path().to_owned()
}

fn decoded<T>(body: serde_json::Result<T>) -> Result<T, SonosError> {
    body.map_err(|error| {
        tracing::warn!(%error, "the Sonos response doesn't parse");
        SonosError::Unreachable
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_go_by_the_known_code_then_the_status() {
        assert!(matches!(
            failure(StatusCode::CONFLICT, Some(&ErrorCode::NoSession)),
            SonosError::Gone
        ));
        assert!(matches!(
            failure(StatusCode::NOT_FOUND, Some(&ErrorCode::NotFound)),
            SonosError::Gone
        ));
        assert!(matches!(
            failure(
                StatusCode::BAD_GATEWAY,
                Some(&ErrorCode::UpstreamUnavailable)
            ),
            SonosError::Speaker
        ));
        assert!(matches!(
            failure(StatusCode::BAD_GATEWAY, None),
            SonosError::Speaker
        ));
        assert!(matches!(
            failure(StatusCode::UNAUTHORIZED, Some(&ErrorCode::Unauthenticated)),
            SonosError::SignedOut
        ));
        assert!(matches!(
            failure(StatusCode::CONFLICT, None),
            SonosError::Server(StatusCode::CONFLICT)
        ));
    }

    #[test]
    fn group_ids_are_one_path_segment() {
        assert_eq!(
            player_path("RINCON_804AF246458401400"),
            "/v1/sonos/players/RINCON_804AF246458401400"
        );
        assert_eq!(player_path("a/b c"), "/v1/sonos/players/a%2Fb%20c");
    }

    #[test]
    fn messages_name_the_group() {
        assert_eq!(
            SonosError::Speaker.message("Couch"),
            "Couldn't reach Couch."
        );
    }
}
