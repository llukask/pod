//! HTTP plumbing shared by every endpoint: the router state, the error
//! type, the client IP and the rate limiter.

pub mod auth;
mod client_ip;
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
use crate::directory::Directory;
use crate::fetch::FetchPolicy;
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
    /// The Sonos speakers (spec 13), `None` when `sonos_hosts` is empty.
    pub sonos: Option<Arc<Sonos>>,
}

impl AppState {
    pub fn new(pool: PgPool, config: &Config) -> Result<Self> {
        let directory = Directory::new(&config.directory_base_url, &config.base_url)
            .context("set up the directory client")?;
        Ok(Self {
            pool,
            allow_registration: config.allow_registration,
            client_ip: Arc::new(config.client_ip.clone()),
            auth_rate_limit: Arc::new(RateLimiter::auth()),
            clock: Clock::system(),
            directory: Arc::new(directory),
            fetch_policy: FetchPolicy::new(config.fetch_allowlist.clone()),
            sonos: Sonos::new(config.sonos_hosts.clone())
                .context("set up the Sonos client")?
                .map(Arc::new),
        })
    }
}
