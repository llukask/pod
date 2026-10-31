//! Signing in and out (spec 4.1, 5.7).
//!
//! The `account` row is the session: server URL, user, device id and bearer
//! token, as spec 5.1 has it. Its presence decides whether the window shows
//! the sign-in card or the app, and its `user_id` is what the sync apply
//! step checks a response against.
//!
//! A `401` doesn't come through here. The engine keeps the row and reports
//! [`super::engine::EngineState::SignedOut`]; signing in again as the same
//! user only swaps the token and device id, so the outbox survives and sync
//! resumes. Signing in as anyone else logs out first.

use std::fmt;
use std::time::Duration;

use color_eyre::eyre::{Result, WrapErr};
use gpui_kit::Global;
use pod_protocol::{AuthRequest, ErrorBody, ErrorCode, TokenResponse};
use reqwest::StatusCode;
use rusqlite::{Connection, OptionalExtension as _, Transaction, params};

use super::SyncHandle;
use super::engine::SyncEngine;
use crate::reconcile::Reconciler;
use crate::store::{Store, Table};

/// How long logout waits for download jobs to stop (spec 5.7). A transfer
/// that outlives it finds its row gone and deletes its own file.
pub const DOWNLOAD_STOP_TIMEOUT: Duration = Duration::from_secs(10);

/// Bounds the best-effort `POST /v1/auth/logout`.
const LOGOUT_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Shown in the user's session list on the server.
// TODO: use the host name once something else needs it too.
const DEVICE_NAME: &str = "Pod desktop";

/// The signed-in account, as the UI shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub server_url: String,
    pub username: String,
}

/// Reads the account row, `None` when signed out by logout or never signed
/// in.
pub fn load_account(connection: &Connection) -> Result<Option<Account>> {
    connection
        .query_row(
            "select server_url, username from account where id = 1",
            [],
            |row| {
                Ok(Account {
                    server_url: row.get(0)?,
                    username: row.get(1)?,
                })
            },
        )
        .optional()
        .context("read the account row")
}

/// The account's server and token, for requests outside the sync loop,
/// `None` without an account.
pub fn load_server(connection: &Connection) -> Result<Option<(String, String)>> {
    connection
        .query_row(
            "select server_url, token from account where id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .context("read the server URL and token")
}

/// What signing in did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignedIn {
    /// There was no account.
    New,
    /// The same user signed in again, after a `401`. The outbox is kept.
    Resumed,
    /// Another user's data was there, and logout cleared it first.
    Switched,
}

/// What the user typed into the sign-in form.
#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    pub server_url: String,
    pub username: String,
    pub password: String,
    /// `POST /v1/auth/register` instead of `login`.
    pub register: bool,
}

/// Masks the password.
impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("server_url", &self.server_url)
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .field("register", &self.register)
            .finish()
    }
}

/// Why signing in failed, worded for the form.
#[derive(Debug)]
pub enum SignInError {
    InvalidServerUrl,
    InvalidCredentials,
    UsernameTaken,
    RegistrationDisabled,
    /// A register rule failed (spec 4.1). Holds the server's explanation.
    Rejected(String),
    RateLimited,
    Unreachable,
    /// The TLS handshake failed, as it does against a server that only
    /// speaks plain HTTP.
    NoHttps,
    UntrustedCertificate,
    /// Another status, or a body that isn't a token response.
    Server(StatusCode),
    /// The local store failed.
    Local(color_eyre::Report),
}

impl fmt::Display for SignInError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidServerUrl => write!(f, "That isn't a server address."),
            Self::InvalidCredentials => write!(f, "Username or password is wrong."),
            Self::UsernameTaken => write!(f, "That username is taken."),
            Self::RegistrationDisabled => write!(f, "This server doesn't accept new accounts."),
            Self::Rejected(message) => write!(f, "{message}"),
            Self::RateLimited => write!(f, "Too many attempts. Try again in a minute."),
            Self::Unreachable => write!(f, "Couldn't reach the server."),
            Self::NoHttps => write!(
                f,
                "The server didn't answer over HTTPS. If it only serves HTTP, \
                 include http:// in the address."
            ),
            Self::UntrustedCertificate => {
                write!(f, "The server's HTTPS certificate isn't trusted.")
            }
            Self::Server(status) => write!(f, "The server answered {status}."),
            Self::Local(error) => write!(f, "Couldn't save the account: {error:#}"),
        }
    }
}

/// Why a request got no answer. A failed TLS handshake gets its own
/// error, since the address defaults to `https://` and a plain HTTP server
/// fails exactly there.
fn request_failure(error: &reqwest::Error) -> SignInError {
    // hyper and tokio-rustls hand the handshake's error up inside nested
    // `io::Error`s, whose `source` skips the error they wrap, so the walk
    // steps into their payload instead.
    let tls = std::iter::successors(Some(error as &dyn std::error::Error), |error| {
        match error.downcast_ref::<std::io::Error>() {
            Some(io) => io.get_ref().map(|inner| inner as &dyn std::error::Error),
            None => error.source(),
        }
    })
    .find_map(|error| error.downcast_ref::<rustls::Error>());
    match tls {
        Some(rustls::Error::InvalidCertificate(_)) => SignInError::UntrustedCertificate,
        Some(_) => SignInError::NoHttps,
        None => SignInError::Unreachable,
    }
}

/// Turns what the user typed into a base URL: `https://` when no scheme is
/// given, no trailing slash. `None` when it isn't an http(s) URL.
pub fn normalize_server_url(input: &str) -> Option<String> {
    let input = input.trim().trim_end_matches('/');
    let with_scheme = if input.contains("://") {
        input.to_owned()
    } else {
        format!("https://{input}")
    };
    let url = reqwest::Url::parse(&with_scheme).ok()?;
    let usable = matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some_and(|host| !host.is_empty())
        && url.query().is_none()
        && url.fragment().is_none();
    usable.then_some(with_scheme)
}

/// What logout throws away, for its confirmation (wireframe D7).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LogoutSummary {
    /// Outbox rows the server hasn't seen.
    pub unsent: u64,
    /// Bytes of audio on disk, complete or partial.
    pub download_bytes: u64,
}

// TODO: name the most important unsent change ("including your place in
// …", wireframe D7) once the confirmation has room for it.
pub fn load_logout_summary(connection: &Connection) -> Result<LogoutSummary> {
    connection
        .query_row(
            "select (select count(*) from outbox),
                    (select coalesce(sum(bytes_done), 0) from download)",
            [],
            |row| {
                Ok(LogoutSummary {
                    unsent: row.get::<_, i64>(0)?.max(0).unsigned_abs(),
                    download_bytes: row.get::<_, i64>(1)?.max(0).unsigned_abs(),
                })
            },
        )
        .context("count unsent changes and downloaded bytes")
}

// ==============================================================================
// The session service
// ==============================================================================

/// Signs in and out. Cheap to clone. Registered as a GPUI global; its
/// futures need the tokio runtime ([`crate::runtime::Background`]).
#[derive(Clone, Debug)]
pub struct Session {
    store: Store,
    sync: SyncHandle,
    engine: SyncEngine,
    /// Logout stops its transfers and deletes its downloads root.
    reconciler: Reconciler,
}

impl Global for Session {}

/// How a sign-in leaves the account row.
#[derive(Debug)]
enum Stored {
    Inserted,
    Updated,
    /// The row belongs to another user and was left alone.
    OtherUser,
}

impl Session {
    pub fn new(store: Store, sync: SyncHandle, engine: SyncEngine, reconciler: Reconciler) -> Self {
        Self {
            store,
            sync,
            engine,
            reconciler,
        }
    }

    /// Logs in or registers, stores the account row and asks for a sync.
    pub async fn sign_in(&self, credentials: Credentials) -> Result<SignedIn, SignInError> {
        let server_url =
            normalize_server_url(&credentials.server_url).ok_or(SignInError::InvalidServerUrl)?;
        let username = credentials.username.trim().to_lowercase();
        let token = self.authenticate(&server_url, &credentials).await?;

        let row = AccountRow {
            server_url,
            username,
            token,
        };
        let signed_in = match self.store_account(row.clone()).await? {
            Stored::Inserted => SignedIn::New,
            Stored::Updated => SignedIn::Resumed,
            Stored::OtherUser => {
                self.logout().await.map_err(SignInError::Local)?;
                match self.store_account(row).await? {
                    Stored::Inserted => SignedIn::Switched,
                    stored => {
                        return Err(SignInError::Local(color_eyre::eyre::eyre!(
                            "the account row survived logout ({stored:?})"
                        )));
                    }
                }
            }
        };
        tracing::info!(?signed_in, "signed in");
        self.sync.request_sync(Duration::ZERO);
        Ok(signed_in)
    }

    async fn authenticate(
        &self,
        server_url: &str,
        credentials: &Credentials,
    ) -> Result<TokenResponse, SignInError> {
        let path = if credentials.register {
            "register"
        } else {
            "login"
        };
        let body = AuthRequest {
            username: credentials.username.clone(),
            password: credentials.password.clone(),
            device_name: DEVICE_NAME.to_owned(),
        };
        let response = self
            .engine
            .http()
            .post(format!("{server_url}/v1/auth/{path}"))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(serde_json::to_vec(&body).expect("an auth request serializes"))
            .send()
            .await
            .map_err(|error| {
                tracing::info!(error = ?error, "the sign-in request failed");
                request_failure(&error)
            })?;
        let status = response.status();
        let bytes = response.bytes().await.map_err(|error| {
            tracing::info!(%error, "the sign-in response broke off");
            SignInError::Unreachable
        })?;
        if status.is_success() {
            return serde_json::from_slice(&bytes).map_err(|error| {
                tracing::warn!(%error, "the sign-in response doesn't parse");
                SignInError::Server(status)
            });
        }
        let error = serde_json::from_slice::<ErrorBody>(&bytes)
            .ok()
            .map(|body| body.error);
        tracing::info!(%status, code = ?error.as_ref().map(|error| &error.code), "sign-in refused");
        // The code set is open (spec 4), so an unknown code falls back to
        // its status.
        Err(match (status, error) {
            (_, Some(error)) if error.code == ErrorCode::UsernameTaken => {
                SignInError::UsernameTaken
            }
            (_, Some(error)) if error.code == ErrorCode::RegistrationDisabled => {
                SignInError::RegistrationDisabled
            }
            (StatusCode::BAD_REQUEST, Some(error)) => SignInError::Rejected(error.message),
            (StatusCode::UNAUTHORIZED, _) => SignInError::InvalidCredentials,
            (StatusCode::TOO_MANY_REQUESTS, _) => SignInError::RateLimited,
            (status, _) => SignInError::Server(status),
        })
    }

    /// Writes the account row unless another user's row is there.
    async fn store_account(&self, row: AccountRow) -> Result<Stored, SignInError> {
        self.store
            .write(move |transaction| row.store(transaction))
            .await
            .map_err(SignInError::Local)
    }

    /// Logs out (spec 5.7), in this order:
    ///
    /// 1. Take the sync mutex, cancelling the request in flight, so no
    ///    response is applied after this.
    /// 2. Cancel every download job and the artwork fetches and wait for
    ///    them to stop, at most [`DOWNLOAD_STOP_TIMEOUT`]. No reconciler
    ///    pass runs from here until
    ///    the downloads directory is gone, so none can start a transfer or
    ///    requeue a row of the store being wiped.
    /// 3. In one transaction, delete every table except `device_setting`.
    /// 4. Delete the downloads directory and the artwork cache.
    /// 5. `POST /v1/auth/logout`, best effort.
    ///
    /// Unsent outbox rows are lost; the confirmation said so.
    pub async fn logout(&self) -> Result<()> {
        let _paused = self.engine.pause().await;

        let downloads_stopped = self.reconciler.cancel_all(DOWNLOAD_STOP_TIMEOUT).await;

        let account = self
            .store
            .write(|transaction| {
                let account = transaction
                    .query_row(
                        "select server_url, token from account where id = 1",
                        [],
                        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                    )
                    .optional()
                    .context("read the account")?;
                delete_account_data(transaction).context("clear the tables")?;
                Ok(account)
            })
            .await
            .context("delete the account's data")?;

        for directory in [self.reconciler.root(), self.reconciler.artwork_root()] {
            match tokio::fs::remove_dir_all(directory).await {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    // The startup sweeps delete whatever is left (spec 7.2).
                    tracing::warn!(%error, directory = %directory.display(), "couldn't delete a cache directory");
                }
                _ => {}
            }
        }
        drop(downloads_stopped);

        if let Some((server_url, token)) = account {
            let result = self
                .engine
                .http()
                .post(format!("{server_url}/v1/auth/logout"))
                .bearer_auth(token)
                .timeout(LOGOUT_REQUEST_TIMEOUT)
                .send()
                .await;
            match result {
                Ok(response) if response.status().is_success() => {}
                Ok(response) => {
                    tracing::info!(status = %response.status(), "the server refused the logout");
                }
                Err(error) => tracing::info!(%error, "the logout request failed"),
            }
        }
        tracing::info!("logged out");
        Ok(())
    }
}

/// Logout's local step 3 (spec 5.7): deletes every table except
/// `device_setting`, and from that the remembered episode, which is the
/// account's.
pub(super) fn delete_account_data(transaction: &Transaction<'_>) -> Result<()> {
    for table in Table::ALL {
        if table != Table::DeviceSetting {
            transaction
                .execute(&format!("delete from {}", table.name()), [])
                .with_context(|| format!("clear {}", table.name()))?;
        }
    }
    transaction
        .execute(
            "delete from device_setting where key = ?1",
            [crate::player::LAST_EPISODE_ID],
        )
        .context("forget the last episode")?;
    Ok(())
}

/// The account row a sign-in writes.
#[derive(Clone, Debug)]
struct AccountRow {
    server_url: String,
    username: String,
    token: TokenResponse,
}

impl AccountRow {
    fn store(&self, transaction: &Transaction<'_>) -> Result<Stored> {
        let current: Option<String> = transaction
            .query_row("select user_id from account where id = 1", [], |row| {
                row.get(0)
            })
            .optional()
            .context("read the account's user")?;
        let user_id = self.token.user_id.to_string();
        let device_id = self.token.device_id.to_string();
        match current {
            None => {
                transaction
                    .execute(
                        "insert into account (id, server_url, user_id, username, device_id, token)
                         values (1, ?1, ?2, ?3, ?4, ?5)",
                        params![
                            self.server_url,
                            user_id,
                            self.username,
                            device_id,
                            self.token.token
                        ],
                    )
                    .context("insert the account")?;
                Ok(Stored::Inserted)
            }
            // Mutations already queued keep the device id they were stamped
            // with; only new ones get the new id (spec 5.7).
            Some(current) if current == user_id => {
                transaction
                    .execute(
                        "update account set server_url = ?1, username = ?2, device_id = ?3,
                                            token = ?4
                         where id = 1",
                        params![self.server_url, self.username, device_id, self.token.token],
                    )
                    .context("update the account")?;
                Ok(Stored::Updated)
            }
            Some(_) => Ok(Stored::OtherUser),
        }
    }
}

/// `1.4 GB`, in decimal units like file managers on Linux and macOS.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} bytes");
    }
    let mut value = bytes as f64 / 1000.0;
    let mut unit = 0;
    while value >= 999.95 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_urls_get_a_scheme_and_lose_the_trailing_slash() {
        assert_eq!(
            normalize_server_url(" pod.kilian.ltd/ ").as_deref(),
            Some("https://pod.kilian.ltd")
        );
        assert_eq!(
            normalize_server_url("http://127.0.0.1:3004").as_deref(),
            Some("http://127.0.0.1:3004")
        );
        assert_eq!(
            normalize_server_url("https://example.com/pod/").as_deref(),
            Some("https://example.com/pod")
        );
        assert_eq!(normalize_server_url(""), None);
        assert_eq!(normalize_server_url("ftp://example.com"), None);
        assert_eq!(normalize_server_url("https://example.com/?a=b"), None);
    }

    #[test]
    fn sizes_read_like_a_file_manager() {
        assert_eq!(format_bytes(0), "0 bytes");
        assert_eq!(format_bytes(999), "999 bytes");
        assert_eq!(format_bytes(1_000), "1.0 KB");
        assert_eq!(format_bytes(1_400_000_000), "1.4 GB");
        assert_eq!(format_bytes(999_999_999), "1.0 GB");
    }
}
