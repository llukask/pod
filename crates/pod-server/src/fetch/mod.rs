//! Outbound HTTP for user-supplied URLs, behind a guard against server-side
//! request forgery (spec 3.2).
//!
//! Every server-side fetch of a URL a user supplied (feeds, and anything
//! else later) goes through [`Fetcher::get`]. It checks scheme and port,
//! resolves host names itself and refuses internal addresses, follows
//! redirects by hand so every hop gets the same checks, and caps time and
//! body size.
//!
//! Nothing here logs URLs: feed URLs can carry credentials, and the spec
//! wants logs keyed by podcast id, which only the caller knows. Error
//! messages leave the URL out for the same reason.

pub mod guard;

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use color_eyre::eyre::{Result, WrapErr};
use percent_encoding::percent_decode_str;
use reqwest::StatusCode;
use reqwest::header::{HeaderMap, LOCATION};
use url::Url;

use guard::GuardedResolver;
pub use guard::{
    Allowlist, BlockReason, BlockedAddress, InvalidAllowlistEntry, Lookup, ResolveFailed,
    SystemLookup, check_url, classify,
};

/// Redirects followed before giving up (spec 3.2).
pub const MAX_REDIRECTS: usize = 5;

/// Largest response body accepted (spec 3.2).
pub const MAX_BODY_BYTES: usize = 20 * 1024 * 1024;

/// What a [`Fetcher`] may reach and how long it may take.
#[derive(Debug, Clone)]
pub struct FetchPolicy {
    pub allowlist: Allowlist,
    /// Ports a URL may name. Production uses [`FetchPolicy::new`]'s 80 and
    /// 443, as spec 3.2 requires; the field is public so tests can reach a
    /// local server on another port. The config deliberately has no key for
    /// it.
    pub allowed_ports: Vec<u16>,
    /// Budget for the whole fetch: every redirect hop plus the body.
    pub timeout: Duration,
}

impl FetchPolicy {
    pub fn new(allowlist: Allowlist) -> FetchPolicy {
        FetchPolicy {
            allowlist,
            allowed_ports: vec![80, 443],
            timeout: Duration::from_secs(20),
        }
    }
}

/// Why a fetch produced no response. HTTP error statuses are not errors;
/// they come back in [`FetchResponse::status`].
///
/// No variant carries the URL, which may hold credentials.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("unsupported URL scheme {0:?}, expected http or https")]
    UnsupportedScheme(String),
    #[error("URL has no host")]
    MissingHost,
    #[error("port {0} is not allowed, only 80 and 443")]
    PortNotAllowed(u16),
    #[error(transparent)]
    Blocked(#[from] BlockedAddress),
    #[error(transparent)]
    Resolve(#[from] ResolveFailed),
    #[error("more than {MAX_REDIRECTS} redirects")]
    TooManyRedirects,
    #[error("redirect {0} has a missing or invalid Location header")]
    InvalidRedirect(StatusCode),
    #[error("response body exceeds {MAX_BODY_BYTES} bytes")]
    BodyTooLarge,
    #[error("timed out")]
    Timeout,
    /// Connection, TLS or protocol failure. The inner error has its URL
    /// stripped.
    #[error("request failed")]
    Transport(#[source] reqwest::Error),
}

/// A completed fetch, whatever its status.
pub struct FetchResponse {
    pub status: StatusCode,
    /// Headers of the final response, including `ETag`, `Last-Modified` and
    /// `Retry-After`.
    pub headers: HeaderMap,
    /// The body, decompressed, at most [`MAX_BODY_BYTES`].
    pub body: Bytes,
    /// The URL that produced this response, after all redirects.
    pub final_url: Url,
    /// The target of the last 301 or 308 in the redirect chain, if any.
    ///
    /// Spec 3.2 says a permanent redirect anywhere in the chain updates
    /// `feed_url`. This is the URL to update it to: everything before the
    /// last permanent hop has moved for good, while a temporary redirect
    /// after it may point somewhere else next time, so the final URL would
    /// pin a temporary location. For `A -301-> B -302-> C` this is `B`; for
    /// `A -302-> B -301-> C` it is `C`.
    pub permanent_redirect: Option<Url>,
}

/// URLs here can carry credentials, so `Debug` prints them redacted.
impl fmt::Debug for FetchResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FetchResponse")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .field("body_len", &self.body.len())
            .field("final_url", &Redacted(&self.final_url))
            .field(
                "permanent_redirect",
                &self.permanent_redirect.as_ref().map(Redacted),
            )
            .finish()
    }
}

struct Redacted<'a>(&'a Url);

impl fmt::Debug for Redacted<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut url = self.0.clone();
        // Both setters only fail for URLs that can't carry credentials.
        if !url.username().is_empty() {
            let _ = url.set_username("redacted");
        }
        if url.password().is_some() {
            let _ = url.set_password(Some("redacted"));
        }
        write!(f, "{:?}", url.as_str())
    }
}

/// The guarded HTTP client. Cheap to clone; clones share connections.
#[derive(Debug, Clone)]
pub struct Fetcher {
    client: reqwest::Client,
    policy: Arc<FetchPolicy>,
}

impl Fetcher {
    /// Builds a fetcher on the system resolver. `base_url` is this server's
    /// public URL, advertised in the `User-Agent`.
    pub fn new(policy: FetchPolicy, base_url: &str) -> Result<Fetcher> {
        Fetcher::with_lookup(policy, base_url, Arc::new(SystemLookup))
    }

    /// Builds a fetcher that resolves host names through `lookup`.
    pub fn with_lookup(
        policy: FetchPolicy,
        base_url: &str,
        lookup: Arc<dyn Lookup>,
    ) -> Result<Fetcher> {
        let policy = Arc::new(policy);
        let resolver = GuardedResolver {
            lookup,
            allowlist: Arc::new(policy.allowlist.clone()),
        };
        let client = reqwest::Client::builder()
            .user_agent(format!("pod/{} (+{base_url})", env!("CARGO_PKG_VERSION")))
            // A proxy would resolve the host itself and connect wherever its
            // DNS says, voiding every address check, so environment proxy
            // settings (HTTP_PROXY and friends) are ignored.
            .no_proxy()
            .dns_resolver(Arc::new(resolver))
            // Redirects are followed in `get`, so each hop passes the guard.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("build the HTTP client")?;
        Ok(Fetcher { client, policy })
    }

    /// GETs `url` with the caller's extra `headers` (conditional request
    /// headers, typically) on every hop.
    ///
    /// Credentials in the URL go out as basic auth. A redirect to the same
    /// origin keeps them, so a private feed that moves within its host still
    /// authenticates; a redirect to another origin drops them unless its
    /// `Location` carries its own.
    pub async fn get(&self, url: &Url, headers: HeaderMap) -> Result<FetchResponse, FetchError> {
        tokio::time::timeout(self.policy.timeout, self.follow_redirects(url, &headers))
            .await
            .map_err(|_elapsed| FetchError::Timeout)?
    }

    async fn follow_redirects(
        &self,
        url: &Url,
        headers: &HeaderMap,
    ) -> Result<FetchResponse, FetchError> {
        let mut current = url.clone();
        let mut permanent_redirect = None;
        for _hop in 0..=MAX_REDIRECTS {
            check_url(&current, &self.policy)?;
            let response = self.send(&current, headers).await?;
            let status = response.status();
            if !is_redirect(status) {
                return Ok(FetchResponse {
                    status,
                    headers: response.headers().clone(),
                    body: read_capped(response).await?,
                    final_url: current,
                    permanent_redirect,
                });
            }
            let next = redirect_target(&current, response.headers())
                .ok_or(FetchError::InvalidRedirect(status))?;
            if matches!(
                status,
                StatusCode::MOVED_PERMANENTLY | StatusCode::PERMANENT_REDIRECT
            ) {
                permanent_redirect = Some(next.clone());
            }
            current = next;
        }
        Err(FetchError::TooManyRedirects)
    }

    async fn send(&self, url: &Url, headers: &HeaderMap) -> Result<reqwest::Response, FetchError> {
        let mut bare = url.clone();
        let _ = bare.set_username("");
        let _ = bare.set_password(None);
        let mut request = self.client.get(bare).headers(headers.clone());
        if !url.username().is_empty() || url.password().is_some() {
            let username = percent_decode_str(url.username()).decode_utf8_lossy();
            let password = url
                .password()
                .map(|password| percent_decode_str(password).decode_utf8_lossy());
            request = request.basic_auth(username, password);
        }
        request.send().await.map_err(transport_error)
    }
}

fn is_redirect(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::MOVED_PERMANENTLY
            | StatusCode::FOUND
            | StatusCode::SEE_OTHER
            | StatusCode::TEMPORARY_REDIRECT
            | StatusCode::PERMANENT_REDIRECT
    )
}

/// Resolves `Location` against the current URL and carries credentials over
/// to a same-origin target that has none of its own.
fn redirect_target(current: &Url, headers: &HeaderMap) -> Option<Url> {
    let location = headers.get(LOCATION)?.to_str().ok()?;
    let mut next = current.join(location).ok()?;
    next.set_fragment(None);
    let has_credentials = !next.username().is_empty() || next.password().is_some();
    if !has_credentials && next.origin() == current.origin() {
        // Same origin means same special scheme and a host, so both setters
        // succeed.
        let _ = next.set_username(current.username());
        let _ = next.set_password(current.password());
    }
    Some(next)
}

/// Reads the body chunk by chunk and stops as soon as it passes the cap, so a
/// huge or endless body never sits in memory.
async fn read_capped(mut response: reqwest::Response) -> Result<Bytes, FetchError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_BODY_BYTES as u64)
    {
        return Err(FetchError::BodyTooLarge);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
        if body.len() + chunk.len() > MAX_BODY_BYTES {
            return Err(FetchError::BodyTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Bytes::from(body))
}

/// Maps a reqwest error to the guard's own errors where the resolver caused
/// it, since reqwest reports resolver failures as generic connect errors.
fn transport_error(error: reqwest::Error) -> FetchError {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(&error);
    while let Some(cause) = source {
        if let Some(blocked) = cause.downcast_ref::<BlockedAddress>() {
            return FetchError::Blocked(*blocked);
        }
        if let Some(failed) = cause.downcast_ref::<ResolveFailed>() {
            return FetchError::Resolve(failed.clone());
        }
        source = cause.source();
    }
    if error.is_timeout() {
        return FetchError::Timeout;
    }
    FetchError::Transport(error.without_url())
}
