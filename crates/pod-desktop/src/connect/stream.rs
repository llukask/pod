//! The device's event stream (spec 14.2): `GET /v1/connect/events`, held
//! open on the tokio side for as long as the player keeps the task, and
//! opened again after it closes.
//!
//! The task hands the player [`Signal`]s and decides nothing else. A
//! connection counts as open for reports once its first `state` arrives,
//! which the player sees for itself.

use std::time::Duration;

use color_eyre::eyre::{Result, WrapErr as _};
use pod_protocol::{ConnectEvent, DeviceId};
use reqwest::StatusCode;
use rusqlite::{Connection, OptionalExtension as _};
use tokio::sync::mpsc;

use super::sse;

/// The first wait before opening a closed stream again, and the most it
/// doubles to (spec 14.2).
pub const FIRST_RETRY: Duration = Duration::from_secs(1);
pub const MAX_RETRY: Duration = Duration::from_secs(30);

/// How long a stream may stay silent before it counts as dead: three of the
/// server's 15 s pings.
pub const SILENCE_LIMIT: Duration = Duration::from_secs(45);

/// Opening the connection. The stream itself has no overall timeout.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// The account the stream speaks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub server_url: String,
    pub token: String,
    /// The session's device id, which is this device in `devices` and in
    /// `playback.device_id`.
    pub device_id: DeviceId,
}

/// Reads the account row's [`Link`], `None` without an account. A token
/// or device id that changes, as signing in again does, is a new link.
pub fn load_link(connection: &Connection) -> Result<Option<Link>> {
    let row = connection
        .query_row(
            "select server_url, token, device_id from account where id = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()
        .context("read the account for the event stream")?;
    let Some((server_url, token, device_id)) = row else {
        return Ok(None);
    };
    Ok(Some(Link {
        server_url,
        token,
        device_id: device_id.parse().context("parse the device id")?,
    }))
}

/// The wait before the next attempt: [`FIRST_RETRY`], doubling up to
/// [`MAX_RETRY`], and back to the start once a `state` arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    next: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Backoff { next: FIRST_RETRY }
    }
}

impl Backoff {
    /// The stream closed: the wait before the next attempt.
    pub fn failed(&mut self) -> Duration {
        let wait = self.next;
        self.next = self.next.saturating_mul(2).min(MAX_RETRY);
        wait
    }

    /// A `state` arrived, so the connection worked.
    pub fn reset(&mut self) {
        self.next = FIRST_RETRY;
    }
}

/// What the stream tells the player.
#[derive(Debug, Clone, PartialEq)]
pub enum Signal {
    Event(ConnectEvent),
    /// The connection closed or never opened. The task tries again.
    Closed,
    /// The server answered `401`. The task stops until the account changes.
    SignedOut,
    /// Another stream of this device took over (`replaced`). The task stops.
    Replaced,
}

/// How one connection ended.
#[derive(Debug, PartialEq, Eq)]
enum Ended {
    Closed,
    SignedOut,
    Replaced,
    /// Nobody listens to the signals any more.
    Dropped,
}

/// A client for the stream: the sync engine's has an overall timeout,
/// which would cut a stream that is meant to stay open.
pub fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .context("build the event stream's HTTP client")
}

/// Holds the stream for `link` until `signals` closes, a `401` or a
/// `replaced` ends it, or the task is aborted.
pub async fn run(http: reqwest::Client, link: Link, signals: mpsc::UnboundedSender<Signal>) {
    let mut backoff = Backoff::default();
    loop {
        let ended = listen(&http, &link, &signals, &mut backoff).await;
        let signal = match ended {
            Ended::Closed => Signal::Closed,
            Ended::SignedOut => Signal::SignedOut,
            Ended::Replaced => Signal::Replaced,
            Ended::Dropped => return,
        };
        if signals.send(signal).is_err() || ended != Ended::Closed {
            return;
        }
        tokio::time::sleep(backoff.failed()).await;
    }
}

/// One connection, from the request until it closes.
async fn listen(
    http: &reqwest::Client,
    link: &Link,
    signals: &mpsc::UnboundedSender<Signal>,
    backoff: &mut Backoff,
) -> Ended {
    let url = format!(
        "{}/v1/connect/events?kind=desktop",
        link.server_url.trim_end_matches('/')
    );
    let response = http
        .get(url)
        .bearer_auth(&link.token)
        .header(reqwest::header::ACCEPT, "text/event-stream")
        .send()
        .await;
    let mut response = match response {
        Ok(response) if response.status() == StatusCode::UNAUTHORIZED => return Ended::SignedOut,
        Ok(response) if response.status().is_success() => response,
        Ok(response) => {
            tracing::info!(status = %response.status(), "the server refused the event stream");
            return Ended::Closed;
        }
        Err(error) => {
            tracing::debug!(%error, "the event stream didn't open");
            return Ended::Closed;
        }
    };
    tracing::info!("the event stream is open");
    let mut parser = sse::Parser::default();
    loop {
        let chunk = match tokio::time::timeout(SILENCE_LIMIT, response.chunk()).await {
            Ok(Ok(Some(chunk))) => chunk,
            Ok(Ok(None)) => {
                tracing::info!("the server closed the event stream");
                return Ended::Closed;
            }
            Ok(Err(error)) => {
                tracing::info!(%error, "the event stream broke off");
                return Ended::Closed;
            }
            Err(_) => {
                tracing::info!("the event stream went silent");
                return Ended::Closed;
            }
        };
        for data in parser.push(&chunk) {
            let event = match serde_json::from_str::<ConnectEvent>(&data) {
                Ok(event) => event,
                Err(error) => {
                    // A newer server's event, which this client doesn't know.
                    tracing::debug!(%error, data, "skipped an event");
                    continue;
                }
            };
            match event {
                ConnectEvent::Replaced => return Ended::Replaced,
                ConnectEvent::State(_) => backoff.reset(),
                _ => {}
            }
            if signals.send(Signal::Event(event)).is_err() {
                return Ended::Dropped;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wait_doubles_to_thirty_seconds_and_resets_on_a_state() {
        let mut backoff = Backoff::default();
        let waits: Vec<_> = (0..7).map(|_| backoff.failed().as_secs()).collect();
        assert_eq!(waits, [1, 2, 4, 8, 16, 30, 30]);
        backoff.reset();
        assert_eq!(backoff.failed(), FIRST_RETRY);
    }
}
