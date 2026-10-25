//! HTTP plumbing shared by every endpoint: the router state, the error
//! type, the client IP and the rate limiter.

pub mod auth;
pub mod directory;
pub mod error;
pub mod rate_limit;
pub mod sync;

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::request::Parts;
use color_eyre::eyre::{Result, WrapErr, eyre};
use sqlx::PgPool;

use crate::clock::Clock;
use crate::config::Config;
use crate::directory::Directory;
use crate::fetch::FetchPolicy;
use error::ApiError;
use rate_limit::RateLimiter;

/// Everything handlers share. Cloned per request, so the fields are cheap
/// handles.
#[derive(Debug, Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub allow_registration: bool,
    /// See [`ClientIp`].
    pub trust_fly_client_ip: bool,
    /// Shared by login and register, so both count against the same 10 per
    /// minute per IP and per username.
    pub auth_rate_limit: Arc<RateLimiter>,
    pub clock: Clock,
    /// The upstream podcast directory and its search cache (spec 4.6).
    pub directory: Arc<Directory>,
    /// The fetch guard's rules, which a subscribe checks its feed URL
    /// against (spec 4.4).
    pub fetch_policy: FetchPolicy,
}

impl AppState {
    pub fn new(pool: PgPool, config: &Config) -> Result<Self> {
        let directory = Directory::new(&config.directory_base_url, &config.base_url)
            .context("set up the directory client")?;
        Ok(Self {
            pool,
            allow_registration: config.allow_registration,
            trust_fly_client_ip: config.trust_fly_client_ip,
            auth_rate_limit: Arc::new(RateLimiter::auth()),
            clock: Clock::system(),
            directory: Arc::new(directory),
            fetch_policy: FetchPolicy::new(config.fetch_allowlist.clone()),
        })
    }
}

/// The client's IP address, for rate limiting.
///
/// It is the TCP peer, which requires serving with
/// `into_make_service_with_connect_info::<SocketAddr>()`. Behind Fly.io's
/// proxy every peer is the proxy, so with `trust_fly_client_ip` set the
/// `Fly-Client-IP` header that the proxy writes wins. The flag must stay off
/// anywhere else, because there any client can send that header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientIp(pub IpAddr);

impl FromRequestParts<AppState> for ClientIp {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        if state.trust_fly_client_ip {
            // Fly's proxy always sets the header; falling back to the peer
            // only matters if the flag is on outside Fly.
            let header = parts
                .headers
                .get("fly-client-ip")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().parse().ok());
            if let Some(ip) = header {
                return Ok(Self(ip));
            }
        }
        // The extractor, not a raw extension lookup, so tests can supply
        // the peer with `MockConnectInfo`.
        let ConnectInfo(peer) = ConnectInfo::<SocketAddr>::from_request_parts(parts, state)
            .await
            .map_err(|rejection| eyre!("read the peer address: {rejection}"))?;
        Ok(Self(peer.ip()))
    }
}
