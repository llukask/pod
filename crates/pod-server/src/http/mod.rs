//! HTTP plumbing shared by every endpoint: the router state, the error
//! type, the client IP and the rate limiter.

pub mod auth;
mod client_ip;
pub mod connect;
pub mod directory;
pub mod error;
pub mod rate_limit;
pub mod read;
pub mod sonos;
pub mod sync;
pub mod web;

use std::sync::Arc;

use color_eyre::eyre::{Result, WrapErr};
use sqlx::PgPool;

use crate::clock::Clock;
use crate::config::Config;
use crate::connect::Connect;
use crate::directory::Directory;
use crate::fetch::{FetchPolicy, Fetcher};
use crate::sonos::Sonos;
use rate_limit::RateLimiter;

pub use client_ip::{
    ClientIp, ClientIpSource, DEFAULT_TRUSTED_PROXIES, InvalidClientIpSource, Resolved,
};

/// Everything handlers share. Cloned per request, so the fields are cheap
/// handles.
#[derive(Debug, Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub allow_registration: bool,
    /// See [`ClientIp`].
    pub client_ip: Arc<ClientIpSource>,
    /// Shared by login and register, so both count against the same 10 per
    /// minute per IP and per username.
    pub auth_rate_limit: Arc<RateLimiter>,
    pub clock: Clock,
    /// The upstream podcast directory and its search cache (spec 4.6).
    pub directory: Arc<Directory>,
    /// The fetch guard's rules, which a subscribe checks its feed URL
    /// against (spec 4.4).
    pub fetch_policy: FetchPolicy,
    /// Fetches chapter files and the start of audio files (spec 17.2),
    /// behind the same guard as feeds.
    pub fetcher: Fetcher,
    /// The Sonos speakers (spec 13), `None` when `sonos_hosts` is empty.
    pub sonos: Option<Arc<Sonos>>,
    /// Each user's playback and connected devices (spec 14).
    pub connect: Arc<Connect>,
}

impl AppState {
    pub fn new(pool: PgPool, config: &Config) -> Result<Self> {
        let directory = Directory::new(&config.directory_base_url, &config.base_url)
            .context("set up the directory client")?;
        let sonos = Sonos::new(config.sonos_hosts.clone())
            .context("set up the Sonos client")?
            .map(Arc::new);
        let clock = Clock::system();
        Ok(Self {
            connect: Connect::new(pool.clone(), sonos.clone(), clock.clone()),
            pool,
            allow_registration: config.allow_registration,
            client_ip: Arc::new(config.client_ip.clone()),
            auth_rate_limit: Arc::new(RateLimiter::auth()),
            clock,
            directory: Arc::new(directory),
            fetch_policy: FetchPolicy::new(config.fetch_allowlist.clone()),
            fetcher: Fetcher::new(
                FetchPolicy::new(config.fetch_allowlist.clone()),
                &config.base_url,
            )
            .context("build the chapter fetcher")?,
            sonos,
        })
    }
}
