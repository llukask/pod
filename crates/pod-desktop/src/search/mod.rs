//! Search and add (wireframe D3, spec 4.6 and 5.5): what the sidebar field
//! holds, the directory request, and which results the user already
//! follows. [`view`] is the field's results pane.

pub mod view;

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::time::Duration;

use color_eyre::eyre::WrapErr as _;
use pod_protocol::{
    DirectoryResult, DirectorySearchResponse, ErrorBody, ErrorCode, FeedUrlError,
    NormalizedFeedUrl, PodcastId, normalize_feed_url,
};
use reqwest::StatusCode;
use rusqlite::Connection;

use crate::store::Store;
use crate::sync::session::load_server;

/// The server gives the directory 10 s (spec 4.6); this leaves room for the
/// round trip to the server itself.
const SEARCH_TIMEOUT: Duration = Duration::from_secs(15);

// ==============================================================================
// What the field holds
// ==============================================================================

/// The sidebar field's text, read as either a feed to subscribe to or terms
/// to search the directory for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Query {
    Empty,
    /// A URL, normalized for the offline subscribe path, or why it can't be.
    Feed(Result<NormalizedFeedUrl, FeedUrlError>),
    /// Trimmed search terms.
    Terms(String),
}

/// Decides whether `input` is a feed URL or search terms.
///
/// It is a URL when it names a scheme (`://`), so `feed://…` reaches the
/// normalizer and gets its inline error instead of a search. Without a
/// scheme it is a URL only when it reads as a host and a path:
/// no whitespace, and a dot before the first `/`, as in
/// `feeds.example.com/show.xml`; that form gets `https://`. A bare domain
/// such as `syntax.fm` stays a search, since a site's home page is rarely
/// its feed and the name finds the show.
pub fn classify(input: &str) -> Query {
    let input = input.trim();
    if input.is_empty() {
        return Query::Empty;
    }
    if input.contains("://") {
        return Query::Feed(normalize_feed_url(input));
    }
    let host_and_path = input
        .split_once('/')
        .is_some_and(|(host, _)| host.contains('.'))
        && !input.contains(char::is_whitespace);
    if host_and_path {
        return Query::Feed(normalize_feed_url(&format!("https://{input}")));
    }
    Query::Terms(input.to_owned())
}

/// Whether the user already follows the show behind `result`, by a local
/// lookup in `subscribed`.
///
/// The result's `podcast_id` is the server's canonical id, so a directory
/// URL that is an old alias of a followed show matches it (spec 4.6). The id
/// derived from the result's own URL matches too: that is the placeholder
/// subscribing from this result creates (spec 5.5) until a sync moves it to
/// the canonical id.
pub fn is_subscribed(result: &DirectoryResult, subscribed: &HashSet<PodcastId>) -> bool {
    subscribed.contains(&result.podcast_id)
        || normalize_feed_url(&result.feed_url)
            .is_ok_and(|url| subscribed.contains(&PodcastId::from_feed_url(&url)))
}

/// What a feed URL that doesn't normalize says under the field. The URL
/// stays out of it: it may carry credentials.
pub fn feed_url_problem(error: &FeedUrlError) -> &'static str {
    match error {
        FeedUrlError::Invalid(_) => "That isn't a web address.",
        FeedUrlError::UnsupportedScheme(_) => "Feed addresses start with http:// or https://.",
    }
}

// ==============================================================================
// The directory request
// ==============================================================================

/// Why a search found nothing to show, worded for the results pane.
#[derive(Debug)]
pub enum SearchError {
    /// The OS reports no network, or the server can't be connected to,
    /// which counts as no connection at the call site (see `net.rs`).
    Offline,
    /// `upstream_unavailable`: the server is fine, the directory isn't.
    Upstream,
    SignedOut,
    RateLimited,
    /// The request went out but no usable answer came back.
    Unreachable,
    /// Another status. The error code set is open, so the status decides.
    Server(StatusCode),
    /// The local store failed.
    Local(color_eyre::Report),
}

impl fmt::Display for SearchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Offline => write!(
                f,
                "Search needs a connection. You can still add a show by pasting its feed URL."
            ),
            Self::Upstream => write!(
                f,
                "The podcast directory isn't answering right now. Try again in a moment."
            ),
            Self::SignedOut => write!(f, "Sign in again to search."),
            Self::RateLimited => write!(f, "Too many searches at once. Try again in a minute."),
            Self::Unreachable => write!(f, "The server didn't answer the search."),
            Self::Server(status) => write!(f, "The search failed: the server answered {status}."),
            Self::Local(error) => write!(f, "Couldn't read the account: {error:#}"),
        }
    }
}

/// Maps a failed response to its error: the code where it is one the
/// client acts on, else the status alone (spec 4).
fn failure(status: StatusCode, code: Option<&ErrorCode>) -> SearchError {
    match (status, code) {
        (_, Some(ErrorCode::UpstreamUnavailable)) => SearchError::Upstream,
        (StatusCode::UNAUTHORIZED, _) => SearchError::SignedOut,
        (StatusCode::TOO_MANY_REQUESTS, _) => SearchError::RateLimited,
        (status, _) => SearchError::Server(status),
    }
}

/// The shows the user follows, for [`is_subscribed`].
pub fn load_subscribed(connection: &Connection) -> color_eyre::Result<HashSet<PodcastId>> {
    let mut statement = connection
        .prepare_cached("select podcast_id from subscription where subscribed = 1")
        .context("prepare the subscribed ids query")?;
    let ids = statement
        .query_map([], |row| row.get::<_, String>(0))
        .context("run the subscribed ids query")?;
    ids.map(|id| {
        id.context("read a subscribed id")?
            .parse()
            .context("parse a podcast id")
    })
    .collect()
}

/// Every cached image's file by URL, for the result grid's artwork. Results
/// come from the network, so there is no column to join on.
pub fn load_artwork_files(connection: &Connection) -> color_eyre::Result<HashMap<String, String>> {
    connection
        .prepare_cached("select url, file from artwork")
        .context("prepare the artwork files query")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .context("run the artwork files query")?
        .collect::<rusqlite::Result<_>>()
        .context("read the artwork files")
}

/// `GET /v1/directory/search` on the account's server (spec 4.6). `http`
/// is the sync engine's client; `connected` is the OS network state, and
/// `false` fails at once without a request.
pub async fn search(
    store: &Store,
    http: &reqwest::Client,
    connected: bool,
    term: String,
) -> Result<Vec<DirectoryResult>, SearchError> {
    if !connected {
        return Err(SearchError::Offline);
    }
    let Some((server_url, token)) = store.read(load_server).await.map_err(SearchError::Local)?
    else {
        return Err(SearchError::SignedOut);
    };
    let url = reqwest::Url::parse_with_params(
        &format!("{}/v1/directory/search", server_url.trim_end_matches('/')),
        [("q", term.as_str())],
    )
    .map_err(|error| SearchError::Local(error.into()))?;
    let response = http
        .get(url)
        .bearer_auth(token)
        .timeout(SEARCH_TIMEOUT)
        .send()
        .await
        .map_err(|error| {
            tracing::info!(%error, "the search request failed");
            if error.is_connect() {
                SearchError::Offline
            } else {
                SearchError::Unreachable
            }
        })?;
    let status = response.status();
    let bytes = response.bytes().await.map_err(|error| {
        tracing::info!(%error, "the search response broke off");
        SearchError::Unreachable
    })?;
    if status.is_success() {
        return serde_json::from_slice::<DirectorySearchResponse>(&bytes)
            .map(|body| body.results)
            .map_err(|error| {
                tracing::warn!(%error, "the search response doesn't parse");
                SearchError::Unreachable
            });
    }
    let code = serde_json::from_slice::<ErrorBody>(&bytes)
        .map(|body| body.error.code)
        .ok();
    tracing::info!(%status, ?code, "the server failed the search");
    Err(failure(status, code.as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(input: &str) -> String {
        match classify(input) {
            Query::Feed(Ok(url)) => url.feed_url().to_owned(),
            other => panic!("{input:?} is {other:?}"),
        }
    }

    #[test]
    fn a_scheme_makes_a_url() {
        assert_eq!(
            feed("  https://feeds.example.com/show.xml "),
            "https://feeds.example.com/show.xml"
        );
        assert_eq!(feed("http://example.com/rss#top"), "http://example.com/rss");
        assert!(matches!(
            classify("feed://example.com/rss"),
            Query::Feed(Err(FeedUrlError::UnsupportedScheme(_)))
        ));
        assert!(matches!(
            classify("https://"),
            Query::Feed(Err(FeedUrlError::Invalid(_)))
        ));
    }

    #[test]
    fn a_host_and_path_make_an_https_url() {
        assert_eq!(
            feed("feeds.example.com/show.xml"),
            "https://feeds.example.com/show.xml"
        );
        assert_eq!(feed("example.com/"), "https://example.com/");
    }

    #[test]
    fn anything_else_searches() {
        assert_eq!(classify(""), Query::Empty);
        assert_eq!(classify("   "), Query::Empty);
        for (input, terms) in [
            ("syntax", "syntax"),
            ("  the daily ", "the daily"),
            ("syntax.fm", "syntax.fm"),
            ("ac/dc", "ac/dc"),
            ("the a.i. show/podcast", "the a.i. show/podcast"),
        ] {
            assert_eq!(classify(input), Query::Terms(terms.to_owned()), "{input:?}");
        }
    }

    #[test]
    fn failures_go_by_the_known_code_then_the_status() {
        let bad_gateway = StatusCode::BAD_GATEWAY;
        assert!(matches!(
            failure(bad_gateway, Some(&ErrorCode::UpstreamUnavailable)),
            SearchError::Upstream
        ));
        assert!(
            matches!(failure(bad_gateway, None), SearchError::Server(_)),
            "a proxy's 502 isn't the directory's"
        );
        assert!(matches!(
            failure(StatusCode::UNAUTHORIZED, Some(&ErrorCode::Unauthenticated)),
            SearchError::SignedOut
        ));
        assert!(matches!(
            failure(StatusCode::TOO_MANY_REQUESTS, None),
            SearchError::RateLimited
        ));
    }

    fn result(feed_url: &str, podcast_id: PodcastId) -> DirectoryResult {
        DirectoryResult {
            title: "Syntax".to_owned(),
            author: None,
            feed_url: feed_url.to_owned(),
            artwork_url: None,
            podcast_id,
        }
    }

    fn id_of(feed_url: &str) -> PodcastId {
        PodcastId::from_feed_url(&normalize_feed_url(feed_url).expect("the URL is valid"))
    }

    #[test]
    fn subscribed_by_canonical_id_or_by_the_results_own_url() {
        let followed = id_of("https://new.example.com/feed.xml");
        let subscribed = HashSet::from([followed]);

        let old_alias = result("https://old.example.com/feed.xml", followed);
        assert!(is_subscribed(&old_alias, &subscribed), "an old alias");

        let placeholder = id_of("https://other.example.com/feed.xml");
        let just_added = result(
            "https://other.example.com/feed.xml",
            id_of("https://canonical.example.com/feed.xml"),
        );
        assert!(!is_subscribed(&just_added, &subscribed));
        assert!(
            is_subscribed(&just_added, &HashSet::from([placeholder])),
            "the placeholder under the result's own URL"
        );
    }
}
