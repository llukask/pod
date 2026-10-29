//! Server configuration, layered from a TOML file and `POD_*` environment
//! variables.
//!
//! [`Config::parse`] is a pure function of the file contents and an
//! environment map, so precedence rules are testable without touching the
//! process environment. [`Config::load`] is the thin shell that reads both
//! from the real process.

use std::collections::HashMap;
use std::fmt;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use color_eyre::eyre::{Result, WrapErr, eyre};
use serde::Deserialize;
use url::Url;

use crate::fetch::Allowlist;
use crate::http::{ClientIpSource, DEFAULT_TRUSTED_PROXIES};

/// Fully resolved server configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub database_url: DatabaseUrl,
    pub listen_addr: SocketAddr,
    /// Public URL of this server, advertised in the feed fetch `User-Agent`
    /// (spec 3.2) so feed hosts can identify the poller.
    // TODO: parse into a `Url` once reqwest (and its `url` dependency) lands.
    pub base_url: String,
    pub poll_interval: Duration,
    pub poller_workers: NonZeroUsize,
    /// Host names, IP addresses and CIDRs the outbound fetch guard lets
    /// through even when they are internal, for feeds self-hosted on a LAN
    /// (spec 3.2).
    pub fetch_allowlist: Allowlist,
    pub allow_registration: bool,
    /// Where the auth rate limit reads the client IP: the TCP peer, or a
    /// header that a trusted reverse proxy sets (spec 4.1).
    pub client_ip: ClientIpSource,
    /// Base URL of the iTunes-compatible search API behind
    /// `/v1/directory/search` (spec 4.6); `/search` is appended to its path.
    /// Tests point it at a local stub.
    pub directory_base_url: Url,
    /// The built web client (`web/dist`), served at every path the API doesn't
    /// claim. Unset, the server is API-only.
    pub web_dir: Option<PathBuf>,
}

/// Postgres connection string. Its `Debug` output is redacted because the URL
/// usually carries a password.
#[derive(Clone, PartialEq, Eq)]
pub struct DatabaseUrl(String);

impl DatabaseUrl {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for DatabaseUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DatabaseUrl(<redacted>)")
    }
}

const DEFAULT_CONFIG_PATH: &str = "pod.toml";
const DEFAULT_LISTEN_ADDR: SocketAddr =
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 3000));
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(15 * 60);
const DEFAULT_DIRECTORY_BASE_URL: &str = "https://itunes.apple.com";
const DEFAULT_POLLER_WORKERS: NonZeroUsize = NonZeroUsize::new(4).expect("4 is non-zero");

impl Config {
    /// Reads the config file named by `POD_CONFIG` (default `pod.toml`) and
    /// the process environment.
    ///
    /// A missing default file is fine, since the environment alone can supply
    /// every required key. A missing file that `POD_CONFIG` names explicitly
    /// is an error, because the operator clearly expected it to exist.
    pub fn load() -> Result<Config> {
        // Non-UTF-8 variables can't be any of ours, so dropping them is safe
        // and avoids the panic in `std::env::vars`.
        let env: HashMap<String, String> = std::env::vars_os()
            .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
            .collect();

        let explicit_path = env.get("POD_CONFIG").filter(|path| !path.is_empty());
        let path = explicit_path.map_or(DEFAULT_CONFIG_PATH, String::as_str);
        let file = match std::fs::read_to_string(path) {
            Ok(contents) => Some(contents),
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound && explicit_path.is_none() =>
            {
                None
            }
            Err(error) => {
                return Err(error).with_context(|| format!("read config file {path}"));
            }
        };

        Config::parse(file.as_deref(), &env)
            .with_context(|| format!("resolve config from {path} and the environment"))
    }

    /// Resolves the configuration from the contents of the TOML file (if one
    /// exists) and an environment map.
    pub fn parse(file: Option<&str>, env: &HashMap<String, String>) -> Result<Config> {
        let from_env = PartialConfig::from_env(env).context("read POD_* environment variables")?;
        let from_file = match file {
            Some(contents) => toml::from_str(contents).context("parse the TOML config file")?,
            None => PartialConfig::default(),
        };
        // sqlx tooling reads plain DATABASE_URL, so honouring it lets one
        // exported variable serve both the CLI and the server.
        let fallback = PartialConfig {
            database_url: env
                .get("DATABASE_URL")
                .filter(|url| !url.is_empty())
                .cloned(),
            ..PartialConfig::default()
        };

        from_env.or(from_file).or(fallback).resolve()
    }
}

// ==============================================================================
// Layering
// ==============================================================================

// Every source (environment, file, fallbacks) yields the same partial shape.
// Sources merge key by key with `or`, highest priority first, and `resolve`
// applies defaults and checks required keys once at the end.

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PartialConfig {
    database_url: Option<String>,
    listen_addr: Option<SocketAddr>,
    base_url: Option<String>,
    poll_interval_secs: Option<u64>,
    poller_workers: Option<NonZeroUsize>,
    fetch_allowlist: Option<Vec<String>>,
    allow_registration: Option<bool>,
    client_ip_header: Option<String>,
    trusted_proxies: Option<Vec<String>>,
    directory_base_url: Option<String>,
    web_dir: Option<PathBuf>,
}

impl PartialConfig {
    /// Reads `POD_*` variables. An empty value counts as unset, so
    /// `POD_X= cargo run` falls through to the file instead of failing.
    fn from_env(env: &HashMap<String, String>) -> Result<PartialConfig> {
        let get = |key: &str| {
            env.get(key)
                .map(String::as_str)
                .filter(|value| !value.is_empty())
        };
        Ok(PartialConfig {
            database_url: get("POD_DATABASE_URL").map(str::to_owned),
            listen_addr: parse_env(env, "POD_LISTEN_ADDR").context("parse POD_LISTEN_ADDR")?,
            base_url: get("POD_BASE_URL").map(str::to_owned),
            poll_interval_secs: parse_env(env, "POD_POLL_INTERVAL_SECS")
                .context("parse POD_POLL_INTERVAL_SECS")?,
            poller_workers: parse_env(env, "POD_POLLER_WORKERS")
                .context("parse POD_POLLER_WORKERS")?,
            fetch_allowlist: get("POD_FETCH_ALLOWLIST").map(split_list),
            allow_registration: parse_env(env, "POD_ALLOW_REGISTRATION")
                .context("parse POD_ALLOW_REGISTRATION")?,
            client_ip_header: get("POD_CLIENT_IP_HEADER").map(str::to_owned),
            trusted_proxies: get("POD_TRUSTED_PROXIES").map(split_list),
            directory_base_url: get("POD_DIRECTORY_BASE_URL").map(str::to_owned),
            web_dir: get("POD_WEB_DIR").map(PathBuf::from),
        })
    }

    fn or(self, lower: PartialConfig) -> PartialConfig {
        PartialConfig {
            database_url: self.database_url.or(lower.database_url),
            listen_addr: self.listen_addr.or(lower.listen_addr),
            base_url: self.base_url.or(lower.base_url),
            poll_interval_secs: self.poll_interval_secs.or(lower.poll_interval_secs),
            poller_workers: self.poller_workers.or(lower.poller_workers),
            fetch_allowlist: self.fetch_allowlist.or(lower.fetch_allowlist),
            allow_registration: self.allow_registration.or(lower.allow_registration),
            client_ip_header: self.client_ip_header.or(lower.client_ip_header),
            trusted_proxies: self.trusted_proxies.or(lower.trusted_proxies),
            directory_base_url: self.directory_base_url.or(lower.directory_base_url),
            web_dir: self.web_dir.or(lower.web_dir),
        }
    }

    fn resolve(self) -> Result<Config> {
        let database_url = self.database_url.ok_or_else(|| {
            eyre!("database_url is not set (config file, POD_DATABASE_URL or DATABASE_URL)")
        })?;
        let base_url = self
            .base_url
            .ok_or_else(|| eyre!("base_url is not set (config file or POD_BASE_URL)"))?;
        // A zero interval would make the poller refetch every feed in a
        // tight loop.
        let poll_interval = match self.poll_interval_secs {
            None => DEFAULT_POLL_INTERVAL,
            Some(0) => {
                return Err(eyre!(
                    "poll_interval_secs (POD_POLL_INTERVAL_SECS) must be positive"
                ));
            }
            Some(secs) => Duration::from_secs(secs),
        };
        let directory_base_url = self
            .directory_base_url
            .as_deref()
            .unwrap_or(DEFAULT_DIRECTORY_BASE_URL);
        let directory_base_url = Url::parse(directory_base_url)
            .ok()
            .filter(|url| matches!(url.scheme(), "http" | "https") && url.has_host())
            .ok_or_else(|| {
                eyre!(
                    "directory_base_url (POD_DIRECTORY_BASE_URL) must be an http(s) URL, \
                     got {directory_base_url:?}"
                )
            })?;
        let client_ip = match &self.trusted_proxies {
            Some(proxies) => ClientIpSource::parse(self.client_ip_header.as_deref(), proxies),
            None => {
                ClientIpSource::parse(self.client_ip_header.as_deref(), DEFAULT_TRUSTED_PROXIES)
            }
        }
        .context(
            "parse client_ip_header (POD_CLIENT_IP_HEADER) and trusted_proxies \
             (POD_TRUSTED_PROXIES)",
        )?;
        Ok(Config {
            database_url: DatabaseUrl(database_url),
            listen_addr: self.listen_addr.unwrap_or(DEFAULT_LISTEN_ADDR),
            base_url,
            poll_interval,
            poller_workers: self.poller_workers.unwrap_or(DEFAULT_POLLER_WORKERS),
            fetch_allowlist: Allowlist::parse(&self.fetch_allowlist.unwrap_or_default())
                .context("parse fetch_allowlist (POD_FETCH_ALLOWLIST)")?,
            allow_registration: self.allow_registration.unwrap_or(true),
            client_ip,
            directory_base_url,
            web_dir: self.web_dir,
        })
    }
}

/// Splits a comma-separated env list, dropping blank entries.
fn split_list(list: &str) -> Vec<String> {
    list.split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_owned)
        .collect()
}

fn parse_env<T>(env: &HashMap<String, String>, key: &str) -> Result<Option<T>>
where
    T: FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    env.get(key)
        .filter(|value| !value.is_empty())
        .map(|value| value.parse().with_context(|| format!("parse {value:?}")))
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    const REQUIRED_ENV: &[(&str, &str)] = &[
        ("POD_DATABASE_URL", "postgresql://pod@db/pod"),
        ("POD_BASE_URL", "https://pod.example.org"),
    ];

    #[test]
    fn env_alone_with_defaults() {
        let config = Config::parse(None, &env(REQUIRED_ENV)).expect("required keys are set");
        assert_eq!(
            config,
            Config {
                database_url: DatabaseUrl("postgresql://pod@db/pod".into()),
                listen_addr: "0.0.0.0:3000".parse().expect("valid address literal"),
                base_url: "https://pod.example.org".into(),
                poll_interval: Duration::from_secs(900),
                poller_workers: NonZeroUsize::new(4).expect("4 is non-zero"),
                fetch_allowlist: Allowlist::default(),
                allow_registration: true,
                client_ip: ClientIpSource::parse(None, DEFAULT_TRUSTED_PROXIES)
                    .expect("the defaults parse"),
                directory_base_url: Url::parse("https://itunes.apple.com")
                    .expect("valid URL literal"),
                web_dir: None,
            }
        );
    }

    #[test]
    fn file_alone_sets_every_key() {
        let file = r#"
            database_url = "postgresql://pod@file/pod"
            listen_addr = "127.0.0.1:8080"
            base_url = "https://file.example.org"
            poll_interval_secs = 60
            poller_workers = 2
            fetch_allowlist = ["nas.lan"]
            allow_registration = false
            client_ip_header = "X-Real-IP"
            trusted_proxies = ["10.0.0.0/8"]
            directory_base_url = "http://127.0.0.1:3009"
            web_dir = "/srv/pod-web"
        "#;
        let config = Config::parse(Some(file), &env(&[])).expect("file sets every key");
        assert_eq!(
            config,
            Config {
                database_url: DatabaseUrl("postgresql://pod@file/pod".into()),
                listen_addr: "127.0.0.1:8080".parse().expect("valid address literal"),
                base_url: "https://file.example.org".into(),
                poll_interval: Duration::from_secs(60),
                poller_workers: NonZeroUsize::new(2).expect("2 is non-zero"),
                fetch_allowlist: Allowlist::parse(&["nas.lan"]).expect("valid host name"),
                allow_registration: false,
                client_ip: ClientIpSource::parse(Some("x-real-ip"), &["10.0.0.0/8"])
                    .expect("valid header and CIDR"),
                directory_base_url: Url::parse("http://127.0.0.1:3009").expect("valid URL literal"),
                web_dir: Some("/srv/pod-web".into()),
            }
        );
    }

    #[test]
    fn env_overrides_file() {
        let file = r#"
            database_url = "postgresql://pod@file/pod"
            listen_addr = "127.0.0.1:8080"
            base_url = "https://file.example.org"
            poll_interval_secs = 60
            poller_workers = 2
            fetch_allowlist = ["nas.lan"]
            allow_registration = false
            client_ip_header = "X-Real-IP"
            trusted_proxies = ["10.0.0.0/8"]
            directory_base_url = "http://127.0.0.1:3009"
            web_dir = "/srv/pod-web"
        "#;
        let env = env(&[
            ("POD_DATABASE_URL", "postgresql://pod@env/pod"),
            ("POD_LISTEN_ADDR", "[::1]:9000"),
            ("POD_BASE_URL", "https://env.example.org"),
            ("POD_POLL_INTERVAL_SECS", "120"),
            ("POD_POLLER_WORKERS", "8"),
            ("POD_FETCH_ALLOWLIST", "media.lan, nas.lan ,,"),
            ("POD_ALLOW_REGISTRATION", "true"),
            ("POD_CLIENT_IP_HEADER", "X-Forwarded-Client"),
            ("POD_TRUSTED_PROXIES", "127.0.0.1, ::1 ,,"),
            (
                "POD_DIRECTORY_BASE_URL",
                "https://directory.example.org/itunes",
            ),
            ("POD_WEB_DIR", "/nix/store/pod-web"),
        ]);
        let config = Config::parse(Some(file), &env).expect("every key is set");
        assert_eq!(
            config,
            Config {
                database_url: DatabaseUrl("postgresql://pod@env/pod".into()),
                listen_addr: "[::1]:9000".parse().expect("valid address literal"),
                base_url: "https://env.example.org".into(),
                poll_interval: Duration::from_secs(120),
                poller_workers: NonZeroUsize::new(8).expect("8 is non-zero"),
                fetch_allowlist: Allowlist::parse(&["media.lan", "nas.lan"])
                    .expect("valid host names"),
                allow_registration: true,
                client_ip: ClientIpSource::parse(
                    Some("x-forwarded-client"),
                    &["127.0.0.1/32", "::1/128"]
                )
                .expect("valid header and addresses"),
                directory_base_url: Url::parse("https://directory.example.org/itunes")
                    .expect("valid URL literal"),
                web_dir: Some("/nix/store/pod-web".into()),
            }
        );
    }

    #[test]
    fn database_url_precedence() {
        let fallback = ("DATABASE_URL", "postgresql://pod@fallback/pod");
        let base = ("POD_BASE_URL", "https://pod.example.org");
        let file = r#"database_url = "postgresql://pod@file/pod""#;
        let database_url = |file: Option<&str>, pairs: &[(&str, &str)]| {
            Config::parse(file, &env(pairs))
                .expect("database url is set")
                .database_url
                .expose()
                .to_owned()
        };

        assert_eq!(
            database_url(None, &[base, fallback]),
            "postgresql://pod@fallback/pod"
        );
        assert_eq!(
            database_url(Some(file), &[base, fallback]),
            "postgresql://pod@file/pod"
        );
        assert_eq!(
            database_url(
                Some(file),
                &[
                    base,
                    fallback,
                    ("POD_DATABASE_URL", "postgresql://pod@env/pod")
                ]
            ),
            "postgresql://pod@env/pod"
        );
    }

    #[test]
    fn empty_env_values_count_as_unset() {
        let mut pairs = REQUIRED_ENV.to_vec();
        pairs.push(("POD_POLLER_WORKERS", ""));
        let file = "poller_workers = 3";
        let config = Config::parse(Some(file), &env(&pairs)).expect("required keys are set");
        assert_eq!(config.poller_workers.get(), 3);
    }

    #[test]
    fn missing_required_keys_are_errors() {
        let error = Config::parse(None, &env(&[("POD_BASE_URL", "https://pod.example.org")]))
            .expect_err("database url is missing");
        assert!(format!("{error:#}").contains("database_url"), "{error:#}");

        let error = Config::parse(
            None,
            &env(&[("POD_DATABASE_URL", "postgresql://pod@db/pod")]),
        )
        .expect_err("base url is missing");
        assert!(format!("{error:#}").contains("base_url"), "{error:#}");
    }

    #[test]
    fn invalid_values_are_errors() {
        for (key, value) in [
            ("POD_POLLER_WORKERS", "0"),
            ("POD_POLL_INTERVAL_SECS", "0"),
            ("POD_POLL_INTERVAL_SECS", "15m"),
            ("POD_LISTEN_ADDR", "localhost"),
            ("POD_ALLOW_REGISTRATION", "yes"),
            ("POD_CLIENT_IP_HEADER", "X Real IP"),
            ("POD_TRUSTED_PROXIES", "127.0.0.1,localhost"),
            ("POD_FETCH_ALLOWLIST", "nas.lan,10.0.0.0/33"),
            ("POD_DIRECTORY_BASE_URL", "itunes.apple.com"),
            ("POD_DIRECTORY_BASE_URL", "ftp://itunes.apple.com"),
        ] {
            let mut pairs = REQUIRED_ENV.to_vec();
            pairs.push((key, value));
            let error =
                Config::parse(None, &env(&pairs)).expect_err(&format!("{key}={value} is invalid"));
            assert!(format!("{error:#}").contains(key), "{key}: {error:#}");
        }
    }

    #[test]
    fn unknown_file_keys_are_errors() {
        let error = Config::parse(Some("pol_interval_secs = 60"), &env(REQUIRED_ENV))
            .expect_err("a misspelt key must not be silently ignored");
        assert!(
            format!("{error:#}").contains("pol_interval_secs"),
            "{error:#}"
        );

        let error = Config::parse(Some("trust_fly_client_ip = true"), &env(REQUIRED_ENV))
            .expect_err("trust_fly_client_ip is not a config key");
        assert!(
            format!("{error:#}").contains("trust_fly_client_ip"),
            "{error:#}"
        );
    }

    #[test]
    fn debug_redacts_database_url() {
        let config = Config::parse(None, &env(REQUIRED_ENV)).expect("required keys are set");
        let debug = format!("{config:?}");
        assert!(!debug.contains("pod@db"), "{debug}");
    }

    #[test]
    fn example_config_parses() {
        let example = include_str!("../../../pod.example.toml");
        Config::parse(Some(example), &env(&[])).expect("the example config stays valid");
    }
}
