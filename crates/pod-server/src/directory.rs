//! Podcast directory search (spec 4.6): the iTunes Search API behind an
//! in-memory cache, with podcast ids resolved through `podcast_alias`.
//!
//! The cache holds the upstream results mapped to [`DirectoryEntry`] but not
//! their podcast ids. Aliases change whenever a feed moves, so ids are
//! resolved against the database on every request with one query, and a
//! cached search never hands out a stale id.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use color_eyre::eyre::{Result, WrapErr, eyre};
use pod_protocol::{
    DirectoryResult, MAX_DIRECTORY_LIMIT, NormalizedFeedUrl, PodcastId, Timestamp,
    normalize_feed_url,
};
use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use serde::Deserialize;
use sqlx::PgPool;
use url::Url;

use crate::fetch::{Allowlist, FetchError, FetchPolicy, Fetcher};

/// How long a search stays cached (spec 4.6).
pub const CACHE_TTL_MS: i64 = 60 * 60 * 1000;

/// Searches kept at most. Each holds up to [`MAX_DIRECTORY_LIMIT`] results of
/// a few hundred bytes, so a full cache stays around 10 MB.
pub const CACHE_CAPACITY: usize = 1000;

/// A search screen waits on this, so it is shorter than a feed fetch's 20 s.
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(10);

// ==============================================================================
// Request parameters
// ==============================================================================

/// A search term normalized for the cache key: trimmed, inner whitespace
/// collapsed to single spaces, lowercased. iTunes search ignores case and
/// spacing, so `"The  Daily"` and `"the daily"` share an entry.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SearchTerm(String);

impl SearchTerm {
    /// `None` for a blank term, which has nothing to search for.
    pub fn parse(input: &str) -> Option<SearchTerm> {
        let term = input
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        (!term.is_empty()).then_some(SearchTerm(term))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The `limit` parameter clamped to `1..=MAX_DIRECTORY_LIMIT`, default
/// [`pod_protocol::DEFAULT_DIRECTORY_LIMIT`].
pub fn clamp_limit(limit: Option<u32>) -> usize {
    let limit = limit.unwrap_or(pod_protocol::DEFAULT_DIRECTORY_LIMIT);
    limit.clamp(1, MAX_DIRECTORY_LIMIT) as usize
}

// ==============================================================================
// Upstream response
// ==============================================================================

/// A directory result before its podcast id is resolved. This is what the
/// cache holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub title: String,
    pub author: Option<String>,
    pub feed_url: NormalizedFeedUrl,
    pub artwork_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ItunesResponse {
    results: Vec<ItunesResult>,
}

/// The fields of an iTunes podcast result that the directory uses. iTunes
/// sends many more, which serde skips.
#[derive(Debug, Deserialize)]
struct ItunesResult {
    #[serde(rename = "collectionName")]
    collection_name: Option<String>,
    #[serde(rename = "artistName")]
    artist_name: Option<String>,
    #[serde(rename = "feedUrl")]
    feed_url: Option<String>,
    #[serde(rename = "artworkUrl600")]
    artwork_url_600: Option<String>,
    #[serde(rename = "artworkUrl100")]
    artwork_url_100: Option<String>,
}

/// Maps an iTunes Search API body to entries, in iTunes' order.
///
/// A result without a title or without a feed URL that normalizes (spec
/// section 1) is dropped: the client could neither show nor subscribe to
/// it. Blank strings count as missing, and an artwork URL that isn't
/// `http(s)` is dropped, since clients load it as an image.
pub fn parse_itunes_response(body: &[u8]) -> Result<Vec<DirectoryEntry>, serde_json::Error> {
    let response: ItunesResponse = serde_json::from_slice(body)?;
    let entries = response
        .results
        .into_iter()
        .filter_map(|result| {
            let feed_url = normalize_feed_url(&result.feed_url?).ok()?;
            Some(DirectoryEntry {
                title: non_blank(result.collection_name)?,
                author: non_blank(result.artist_name),
                feed_url,
                artwork_url: [result.artwork_url_600, result.artwork_url_100]
                    .into_iter()
                    .filter_map(non_blank)
                    .find(|url| {
                        Url::parse(url).is_ok_and(|url| matches!(url.scheme(), "http" | "https"))
                    }),
            })
        })
        .collect();
    Ok(entries)
}

fn non_blank(value: Option<String>) -> Option<String> {
    let value = value?;
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// Why the upstream directory produced no results. Every variant is a `502
/// upstream_unavailable` for the client. None carries the search term.
#[derive(Debug, thiserror::Error)]
pub enum DirectoryError {
    #[error("fetching the directory failed")]
    Fetch(#[from] FetchError),
    #[error("the directory answered HTTP {0}")]
    Status(StatusCode),
    #[error("the directory's response is not the expected JSON")]
    Body(#[source] serde_json::Error),
}

// ==============================================================================
// Cache
// ==============================================================================

/// Searches by term with the time they were fetched. Bounded by evicting
/// expired entries first and the oldest one after that, which is an
/// O(capacity) scan, paid only on a miss next to an upstream round trip.
#[derive(Debug)]
struct SearchCache {
    entries: HashMap<SearchTerm, CachedSearch>,
    capacity: usize,
}

#[derive(Debug)]
struct CachedSearch {
    fetched_at: Timestamp,
    entries: Arc<[DirectoryEntry]>,
}

impl SearchCache {
    fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            capacity,
        }
    }

    fn get(&self, term: &SearchTerm, now: Timestamp) -> Option<Arc<[DirectoryEntry]>> {
        self.entries
            .get(term)
            .filter(|cached| is_fresh(cached, now))
            .map(|cached| Arc::clone(&cached.entries))
    }

    fn insert(&mut self, term: SearchTerm, entries: Arc<[DirectoryEntry]>, now: Timestamp) {
        if !self.entries.contains_key(&term) && self.entries.len() >= self.capacity {
            self.entries.retain(|_, cached| is_fresh(cached, now));
            if self.entries.len() >= self.capacity {
                let oldest = self
                    .entries
                    .iter()
                    .min_by_key(|(_, cached)| cached.fetched_at)
                    .map(|(term, _)| term.clone());
                if let Some(oldest) = oldest {
                    self.entries.remove(&oldest);
                }
            }
        }
        self.entries.insert(
            term,
            CachedSearch {
                fetched_at: now,
                entries,
            },
        );
    }
}

fn is_fresh(cached: &CachedSearch, now: Timestamp) -> bool {
    now.as_epoch_ms() - cached.fetched_at.as_epoch_ms() < CACHE_TTL_MS
}

// ==============================================================================
// Directory client
// ==============================================================================

/// The upstream directory and its cache. One per server, shared by every
/// request.
#[derive(Debug)]
pub struct Directory {
    fetcher: Fetcher,
    base_url: Url,
    cache: Mutex<SearchCache>,
}

impl Directory {
    /// A client for the iTunes-compatible search API at `base_url`
    /// (`/search` is appended to its path). `server_base_url` is this
    /// server's public URL, sent in the `User-Agent`.
    ///
    /// Requests go through the guarded [`Fetcher`] for its proxy bypass,
    /// timeout, body cap and per-hop redirect checks. The operator chose
    /// `base_url`, so its host is allowlisted, which also allows its port,
    /// even when internal (a stub or a self-hosted mirror); redirects
    /// elsewhere still get the usual checks.
    pub fn new(base_url: &Url, server_base_url: &str) -> Result<Directory> {
        let host = base_url
            .host_str()
            .ok_or_else(|| eyre!("the directory base URL has no host"))?;
        let policy = FetchPolicy {
            timeout: UPSTREAM_TIMEOUT,
            ..FetchPolicy::new(Allowlist::parse(&[host]).context("allowlist the directory host")?)
        };
        Ok(Directory {
            fetcher: Fetcher::new(policy, server_base_url).context("build the directory client")?,
            base_url: base_url.clone(),
            cache: Mutex::new(SearchCache::new(CACHE_CAPACITY)),
        })
    }

    /// The entries for `term`, from the cache if a search for it is younger
    /// than an hour, otherwise from upstream. Always up to
    /// [`MAX_DIRECTORY_LIMIT`] entries, so one cached search serves every
    /// `limit`; callers slice. Failures are not cached.
    // TODO: concurrent misses on one term each go upstream; coalesce them if
    // directory traffic ever makes that matter.
    pub async fn search(
        &self,
        term: &SearchTerm,
        now: Timestamp,
    ) -> Result<Arc<[DirectoryEntry]>, DirectoryError> {
        if let Some(entries) = self.lock_cache().get(term, now) {
            tracing::debug!(term = term.as_str(), "directory cache hit");
            return Ok(entries);
        }
        tracing::debug!(term = term.as_str(), "directory cache miss");
        let response = self
            .fetcher
            .get(&self.search_url(term), HeaderMap::new())
            .await?;
        if !response.status.is_success() {
            return Err(DirectoryError::Status(response.status));
        }
        let mut entries = parse_itunes_response(&response.body).map_err(DirectoryError::Body)?;
        entries.truncate(MAX_DIRECTORY_LIMIT as usize);
        let entries: Arc<[DirectoryEntry]> = entries.into();
        self.lock_cache()
            .insert(term.clone(), Arc::clone(&entries), now);
        Ok(entries)
    }

    /// Asks for [`MAX_DIRECTORY_LIMIT`] results whatever the caller's
    /// `limit`, so the cache entry serves them all. Results without a usable
    /// feed are dropped afterwards, so a response can hold fewer.
    fn search_url(&self, term: &SearchTerm) -> Url {
        let mut url = self.base_url.clone();
        url.set_path(&format!(
            "{}/search",
            self.base_url.path().trim_end_matches('/')
        ));
        url.query_pairs_mut()
            .clear()
            .append_pair("media", "podcast")
            .append_pair("entity", "podcast")
            .append_pair("term", term.as_str())
            .append_pair("limit", &MAX_DIRECTORY_LIMIT.to_string());
        url
    }

    fn lock_cache(&self) -> std::sync::MutexGuard<'_, SearchCache> {
        // Nothing panics while holding the lock (plain map operations), and
        // a poisoned cache would only hold valid entries anyway.
        self.cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

// ==============================================================================
// Podcast ids
// ==============================================================================

/// Attaches each entry's canonical podcast id (spec 4.6): the podcast a
/// `podcast_alias` row maps its normalized URL to, otherwise the id derived
/// from the URL (spec section 1). One query for the whole page.
pub async fn resolve_podcast_ids(
    pool: &PgPool,
    entries: &[DirectoryEntry],
) -> Result<Vec<DirectoryResult>> {
    let normalized: Vec<String> = entries
        .iter()
        .map(|entry| entry.feed_url.normalized().to_owned())
        .collect();
    let aliases: HashMap<String, PodcastId> = sqlx::query!(
        "select normalized_url, podcast_id from podcast_alias
         where normalized_url = any($1)",
        &normalized
    )
    .fetch_all(pool)
    .await
    .context("look up podcast aliases")?
    .into_iter()
    .map(|row| (row.normalized_url, PodcastId::from_uuid(row.podcast_id)))
    .collect();
    Ok(entries
        .iter()
        .map(|entry| DirectoryResult {
            title: entry.title.clone(),
            author: entry.author.clone(),
            feed_url: entry.feed_url.feed_url().to_owned(),
            artwork_url: entry.artwork_url.clone(),
            podcast_id: aliases
                .get(entry.feed_url.normalized())
                .copied()
                .unwrap_or_else(|| PodcastId::from_feed_url(&entry.feed_url)),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: i64) -> Timestamp {
        Timestamp::from_epoch_ms(1_800_000_000_000 + ms).expect("near today")
    }

    fn term(input: &str) -> SearchTerm {
        SearchTerm::parse(input).expect("non-blank term")
    }

    fn entries(title: &str) -> Arc<[DirectoryEntry]> {
        vec![DirectoryEntry {
            title: title.into(),
            author: None,
            feed_url: normalize_feed_url("https://feeds.example.org/a.xml")
                .expect("valid feed URL"),
            artwork_url: None,
        }]
        .into()
    }

    #[test]
    fn search_terms_normalize_case_and_whitespace() {
        assert_eq!(term("  The\t Daily \n").as_str(), "the daily");
        assert_eq!(term("Ünïcode SHOW").as_str(), "ünïcode show");
        assert_eq!(SearchTerm::parse(" \t\n"), None);
        assert_eq!(SearchTerm::parse(""), None);
    }

    #[test]
    fn limits_default_and_clamp() {
        assert_eq!(clamp_limit(None), 25);
        assert_eq!(clamp_limit(Some(0)), 1);
        assert_eq!(clamp_limit(Some(10)), 10);
        assert_eq!(clamp_limit(Some(50)), 50);
        assert_eq!(clamp_limit(Some(200)), 50);
    }

    #[test]
    fn itunes_results_map_and_unusable_ones_drop() {
        let body = br#"{ "resultCount": 6, "results": [
            { "collectionName": "The Daily", "artistName": "The New York Times",
              "feedUrl": "HTTP://Feeds.Example.org:80/daily.xml#top",
              "artworkUrl600": "https://img.example.org/600.jpg",
              "artworkUrl100": "https://img.example.org/100.jpg", "trackCount": 3 },
            { "collectionName": "Small Art", "artistName": " ",
              "feedUrl": "https://feeds.example.org/small.xml",
              "artworkUrl600": "", "artworkUrl100": "https://img.example.org/s100.jpg" },
            { "collectionName": "Bad Art", "feedUrl": "https://feeds.example.org/bad.xml",
              "artworkUrl600": "javascript:alert(1)" },
            { "collectionName": "No Feed", "artistName": "Someone" },
            { "collectionName": "FTP Feed", "feedUrl": "ftp://feeds.example.org/x.xml" },
            { "collectionName": " ", "feedUrl": "https://feeds.example.org/untitled.xml" }
        ] }"#;
        let entries = parse_itunes_response(body).expect("valid iTunes JSON");
        let summary: Vec<_> = entries
            .iter()
            .map(|entry| {
                (
                    entry.title.as_str(),
                    entry.author.as_deref(),
                    entry.feed_url.feed_url(),
                    entry.artwork_url.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                (
                    "The Daily",
                    Some("The New York Times"),
                    "http://feeds.example.org/daily.xml",
                    Some("https://img.example.org/600.jpg"),
                ),
                (
                    "Small Art",
                    None,
                    "https://feeds.example.org/small.xml",
                    Some("https://img.example.org/s100.jpg"),
                ),
                ("Bad Art", None, "https://feeds.example.org/bad.xml", None),
            ]
        );
    }

    #[test]
    fn non_itunes_bodies_are_errors() {
        assert!(parse_itunes_response(b"<html>busy</html>").is_err());
        assert!(parse_itunes_response(br#"{ "errorMessage": "Invalid value" }"#).is_err());
    }

    #[test]
    fn cache_entries_expire_after_an_hour() {
        let mut cache = SearchCache::new(10);
        cache.insert(term("daily"), entries("Daily"), at(0));
        assert!(cache.get(&term("daily"), at(CACHE_TTL_MS - 1)).is_some());
        assert!(cache.get(&term("daily"), at(CACHE_TTL_MS)).is_none());
        assert!(cache.get(&term("weekly"), at(0)).is_none());
    }

    #[test]
    fn full_cache_evicts_expired_entries_then_the_oldest() {
        let mut cache = SearchCache::new(3);
        cache.insert(term("a"), entries("A"), at(0));
        cache.insert(term("b"), entries("B"), at(10));
        cache.insert(term("c"), entries("C"), at(20));

        // `a` and `b` have expired by now, so both go and `c` stays.
        let later = at(CACHE_TTL_MS + 15);
        cache.insert(term("d"), entries("D"), later);
        assert_eq!(cache.entries.len(), 2);
        assert!(cache.get(&term("c"), later).is_some());

        // Nothing has expired, so the oldest (`c`) makes room.
        cache.insert(term("e"), entries("E"), later);
        cache.insert(term("f"), entries("F"), later);
        assert_eq!(cache.entries.len(), 3);
        assert!(cache.get(&term("c"), later).is_none());
        for kept in ["d", "e", "f"] {
            assert!(cache.get(&term(kept), later).is_some(), "{kept}");
        }

        // Refreshing a cached term doesn't evict anything.
        cache.insert(term("f"), entries("F2"), later);
        assert_eq!(cache.entries.len(), 3);
    }
}
