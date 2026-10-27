//! Feed URL normalization (spec section 1).
//!
//! Parsing follows the WHATWG URL standard via the `url` crate, which
//! normalizes more than the spec lists: percent-encoding, IDN hosts, empty
//! paths, dot segments and more. `conformance/README.md` lists every rule a
//! client implementation has to reproduce, and `conformance/ids.json` has a
//! case for each.

use std::fmt;

use url::Url;

/// A feed URL that passed normalization.
///
/// It carries two forms. [`feed_url`](Self::feed_url) keeps the scheme the
/// user gave and is what gets stored and fetched. [`normalized`](Self::normalized)
/// has `http` mapped to `https` and is what the podcast id hashes, so the
/// same feed over either scheme is one podcast.
#[derive(Clone, PartialEq, Eq)]
pub struct NormalizedFeedUrl {
    feed_url: Url,
    normalized: Url,
}

impl NormalizedFeedUrl {
    /// The normalized URL with its original scheme, for storage and fetching.
    pub fn feed_url(&self) -> &str {
        self.feed_url.as_str()
    }

    /// The normalized URL with an `https` scheme, the podcast id's input.
    pub fn normalized(&self) -> &str {
        self.normalized.as_str()
    }

    /// [`feed_url`](Self::feed_url) without its credentials, for showing to
    /// the user (spec 1: a private feed's secret never ends up in a message).
    pub fn without_credentials(&self) -> String {
        let mut url = self.feed_url.clone();
        // Both setters only fail for URLs without a host, and a normalized
        // feed URL always has one.
        let _ = url.set_username("");
        let _ = url.set_password(None);
        url.into()
    }
}

/// Private feeds put credentials in the URL, and the spec says they are never
/// logged, so `Debug` masks them.
impl fmt::Debug for NormalizedFeedUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NormalizedFeedUrl")
            .field("feed_url", &redact_credentials(&self.feed_url))
            .field("normalized", &redact_credentials(&self.normalized))
            .finish()
    }
}

fn redact_credentials(url: &Url) -> String {
    let mut url = url.clone();
    // Both setters only fail for URLs without a host, and a normalized feed
    // URL always has one.
    if !url.username().is_empty() {
        let _ = url.set_username("redacted");
    }
    if url.password().is_some() {
        let _ = url.set_password(Some("redacted"));
    }
    url.into()
}

/// The host of a stored feed URL, with a non-default port, for naming a
/// podcast the server has no title for (spec 5.5). Never the credentials
/// or the path: the host is enough to recognize the feed by, and the rest
/// may carry a secret. Falls back to the URL text without credentials when
/// the URL has no host, which a normalized feed URL can't lack.
pub fn feed_host(feed_url: &str) -> String {
    let Ok(url) = Url::parse(feed_url) else {
        return feed_url.to_owned();
    };
    match (url.host_str(), url.port()) {
        (Some(host), Some(port)) => format!("{host}:{port}"),
        (Some(host), None) => host.to_owned(),
        (None, _) => redact_credentials(&url),
    }
}

/// Why a feed URL was rejected. The messages never include the URL, since it
/// may carry credentials.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FeedUrlError {
    /// The input doesn't parse as an absolute URL.
    #[error("not a valid URL: {0}")]
    Invalid(#[from] url::ParseError),
    /// The URL parses but its scheme isn't `http` or `https`.
    #[error("unsupported URL scheme {0:?}, expected http or https")]
    UnsupportedScheme(String),
}

/// Normalizes a user-supplied feed URL: trim, parse, accept only `http` and
/// `https`, drop the fragment, and derive the `https` form for the id.
pub fn normalize_feed_url(input: &str) -> Result<NormalizedFeedUrl, FeedUrlError> {
    // Parsing already lowercases the scheme and host, drops the scheme's
    // default port and leaves the query's parameter order alone.
    let mut feed_url = Url::parse(input.trim())?;
    match feed_url.scheme() {
        "http" | "https" => {}
        other => return Err(FeedUrlError::UnsupportedScheme(other.to_owned())),
    }
    feed_url.set_fragment(None);

    // Changing the scheme also drops a port that is the new scheme's default,
    // so `http://host:443/` and `https://host/` share an id. `https://host:80/`
    // keeps its port: 80 is only the default for `http`.
    let mut normalized = feed_url.clone();
    if normalized.scheme() == "http" {
        normalized
            .set_scheme("https")
            .expect("http and https are both special schemes, so switching is allowed");
    }

    Ok(NormalizedFeedUrl {
        feed_url,
        normalized,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_masks_credentials() {
        let url = normalize_feed_url("http://alice:hunter2@example.com/feed.xml")
            .expect("URL with credentials is valid");
        let debug = format!("{url:?}");
        assert!(
            !debug.contains("alice") && !debug.contains("hunter2"),
            "{debug}"
        );
        assert!(debug.contains("redacted:redacted@example.com"), "{debug}");
    }

    #[test]
    fn the_display_form_drops_credentials_and_keeps_the_scheme() {
        let url = normalize_feed_url("http://alice:hunter2@example.com/feed.xml")
            .expect("URL with credentials is valid");
        assert_eq!(url.without_credentials(), "http://example.com/feed.xml");
    }

    #[test]
    fn the_host_names_a_feed_without_its_path_or_credentials() {
        assert_eq!(
            feed_host("https://alice:hunter2@feeds.example.com/private/feed.xml"),
            "feeds.example.com"
        );
        assert_eq!(feed_host("http://127.0.0.1:3007/wide-art"), "127.0.0.1:3007");
        assert_eq!(feed_host("not a url"), "not a url");
    }

    #[test]
    fn errors_do_not_echo_the_url() {
        let error = normalize_feed_url("ftp://alice:hunter2@example.com/feed.xml")
            .expect_err("ftp is rejected");
        assert!(!error.to_string().contains("hunter2"), "{error}");
    }
}
