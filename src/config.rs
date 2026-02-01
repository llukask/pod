use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
struct ConfigFile {
    database_url: Option<String>,
    base_url: Option<String>,
    cookie_domain: Option<String>,
    cookie_key: Option<String>,
    port: Option<u16>,
    refresh_interval_secs: Option<u64>,
    allow_registration: Option<bool>,
}

#[derive(Debug)]
pub struct Config {
    pub database_url: String,
    pub base_url: String,
    pub cookie_domain: String,
    pub cookie_key: Option<String>,
    pub port: u16,
    pub refresh_interval_secs: u64,
    pub allow_registration: bool,
}

/// Resolve a config value: env var takes priority, then file value.
fn resolve(env_var: &str, file_val: Option<String>) -> Option<String> {
    std::env::var(env_var).ok().or(file_val)
}

/// Resolve a config value, parsing it from string to the target type.
fn resolve_parsed<T: std::str::FromStr>(env_var: &str, file_val: Option<T>) -> Option<T> {
    std::env::var(env_var)
        .ok()
        .and_then(|v| v.parse().ok())
        .or(file_val)
}

impl Config {
    /// Load configuration from a TOML file (if present) with environment variable overrides.
    ///
    /// Resolution order for each field: env var > TOML file value > default (where applicable).
    ///
    /// The config file path defaults to `pod.toml` in the working directory and can be
    /// overridden with `--config <path>`.
    pub fn load() -> Result<Self> {
        let config_path = parse_config_path();
        let file = load_config_file(config_path.as_deref())?;

        let mut missing = Vec::new();

        let database_url = resolve("DATABASE_URL", file.database_url);
        if database_url.is_none() {
            missing.push("database_url / DATABASE_URL");
        }

        let base_url = resolve("BASE_URL", file.base_url);
        if base_url.is_none() {
            missing.push("base_url / BASE_URL");
        }

        let cookie_domain = resolve("COOKIE_DOMAIN", file.cookie_domain);
        if cookie_domain.is_none() {
            missing.push("cookie_domain / COOKIE_DOMAIN");
        }

        if !missing.is_empty() {
            bail!(
                "missing required configuration: {}",
                missing.join(", ")
            );
        }

        Ok(Config {
            database_url: database_url.unwrap(),
            base_url: base_url.unwrap(),
            cookie_domain: cookie_domain.unwrap(),
            cookie_key: resolve("COOKIE_KEY", file.cookie_key),
            port: resolve_parsed("PORT", file.port).unwrap_or(3000),
            refresh_interval_secs: resolve_parsed("REFRESH_INTERVAL_SECS", file.refresh_interval_secs)
                .unwrap_or(600),
            allow_registration: resolve_parsed("ALLOW_REGISTRATION", file.allow_registration)
                .unwrap_or(true),
        })
    }
}

/// Parse `--config <path>` from CLI arguments.
fn parse_config_path() -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.windows(2)
        .find(|pair| pair[0] == "--config")
        .map(|pair| pair[1].clone())
}

/// Read and parse the TOML config file, returning defaults if the file doesn't exist.
fn load_config_file(path: Option<&str>) -> Result<ConfigFile> {
    let path = path.unwrap_or("pod.toml");

    if !Path::new(path).exists() {
        return Ok(ConfigFile::default());
    }

    let contents =
        std::fs::read_to_string(path).with_context(|| format!("failed to read {path}"))?;
    toml::from_str(&contents).with_context(|| format!("failed to parse {path}"))
}
