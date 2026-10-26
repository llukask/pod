//! `GET /v1/directory/search` (spec 4.6) against a stubbed iTunes Search
//! API.
//!
//! The stub runs once per test binary on a port from [`bind_test_port`],
//! because sandboxed development environments can only bind a few fixed
//! ports (see CLAUDE.md). It answers by search term, and tests run
//! concurrently, so each test searches for its own terms.

mod auth_app;
mod common;

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use auth_app::{HOUR_MS, TestApp, assert_error};
use axum::Router;
use axum::extract::Query;
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use pod_protocol::{PodcastId, normalize_feed_url};
use serde_json::{Value, json};
use sqlx::PgPool;

use common::port::bind_test_port;

// ==============================================================================
// Stub directory
// ==============================================================================

/// Upstream requests per search term, so cache tests can tell whether a
/// search reached the stub.
fn hits() -> &'static Mutex<HashMap<String, usize>> {
    static HITS: OnceLock<Mutex<HashMap<String, usize>>> = OnceLock::new();
    HITS.get_or_init(Mutex::default)
}

fn hit_count(term: &str) -> usize {
    hits()
        .lock()
        .expect("no test panics while holding the lock")
        .get(term)
        .copied()
        .unwrap_or(0)
}

/// The stub's base URL. The first call binds a port with
/// [`bind_test_port`] (`POD_TEST_DIRECTORY_PORT` overrides the choice) and
/// starts the stub on its own thread and runtime, so it outlives each test's
/// runtime.
fn stub_url() -> String {
    static URL: OnceLock<String> = OnceLock::new();
    URL.get_or_init(|| {
        let listener = bind_test_port("POD_TEST_DIRECTORY_PORT");
        let address = listener
            .local_addr()
            .expect("a bound listener has an address");
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("the stub runtime builds");
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener)
                    .expect("the listener joins the runtime");
                axum::serve(listener, Router::new().route("/search", get(itunes_search)))
                    .await
                    .expect("the stub runs until the process exits");
            });
        });
        format!("http://{address}")
    })
    .clone()
}

/// Answers like the iTunes Search API. Terms starting with `status-<code>`
/// answer with that status and `garbage` with a non-JSON body; any other term
/// `t` finds one podcast at `https://feeds.example.org/<t>.xml`, plus
/// the extra results [`results_for`] lists for some terms.
async fn itunes_search(Query(query): Query<HashMap<String, String>>) -> Response {
    let fixed = [("media", "podcast"), ("entity", "podcast"), ("limit", "50")];
    if fixed
        .iter()
        .any(|(key, value)| query.get(*key).map(String::as_str) != Some(*value))
    {
        return (StatusCode::BAD_REQUEST, "unexpected parameters").into_response();
    }
    let term = query.get("term").cloned().unwrap_or_default();
    *hits()
        .lock()
        .expect("no test panics while holding the lock")
        .entry(term.clone())
        .or_default() += 1;

    if let Some(code) = term.strip_prefix("status-") {
        let code = code[..3].parse().expect("tests pass numeric statuses");
        return StatusCode::from_u16(code)
            .expect("tests pass valid statuses")
            .into_response();
    }
    if term == "garbage" {
        return "<html>Service Unavailable</html>".into_response();
    }
    let results = results_for(&term);
    axum::Json(json!({ "resultCount": results.len(), "results": results })).into_response()
}

fn results_for(term: &str) -> Vec<Value> {
    let slug = term.replace(' ', "-");
    let simple = json!({
        "wrapperType": "track",
        "kind": "podcast",
        "collectionName": format!("Show about {term}"),
        "artistName": "Example Media",
        "feedUrl": format!("https://feeds.example.org/{slug}.xml"),
        "artworkUrl100": format!("https://img.example.org/{slug}-100.jpg"),
    });
    match term {
        "mapping" => vec![
            json!({
                "collectionName": "The Daily",
                "artistName": "The New York Times",
                "feedUrl": "HTTP://Feeds.Example.org:80/daily.xml#latest",
                "artworkUrl600": "https://img.example.org/daily-600.jpg",
                "artworkUrl100": "https://img.example.org/daily-100.jpg",
                "trackCount": 2300,
            }),
            json!({ "collectionName": "No Feed", "artistName": "Example Media" }),
            json!({ "collectionName": "FTP Feed", "feedUrl": "ftp://feeds.example.org/x.xml" }),
            json!({ "collectionName": "Not A URL", "feedUrl": "feeds.example.org/x.xml" }),
            json!({
                "collectionName": "Minimal",
                "feedUrl": "https://feeds.example.org/minimal.xml",
            }),
        ],
        "many" => (0..60)
            .map(|index| {
                json!({
                    "collectionName": format!("Show {index}"),
                    "feedUrl": format!("https://feeds.example.org/many-{index}.xml"),
                })
            })
            .collect(),
        _ => vec![simple],
    }
}

// ==============================================================================
// Helpers
// ==============================================================================

async fn app_with_user(pool: PgPool) -> (TestApp, String) {
    let app = TestApp::with_directory(pool, &stub_url());
    let token = app.register("alice").await;
    (app, token)
}

fn podcast_id_of(feed_url: &str) -> PodcastId {
    PodcastId::from_feed_url(&normalize_feed_url(feed_url).expect("test feed URLs are valid"))
}

// ==============================================================================
// Tests
// ==============================================================================

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn maps_itunes_results_and_drops_unusable_feeds(pool: PgPool) {
    let (app, token) = app_with_user(pool).await;
    let response = app
        .with_token(Method::GET, "/v1/directory/search?q=Mapping", &token)
        .await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.body);
    assert_eq!(
        response.body,
        json!({ "results": [
            {
                "title": "The Daily",
                "author": "The New York Times",
                "feed_url": "http://feeds.example.org/daily.xml",
                "artwork_url": "https://img.example.org/daily-600.jpg",
                "podcast_id": podcast_id_of("https://feeds.example.org/daily.xml").to_string(),
            },
            {
                "title": "Minimal",
                "author": null,
                "feed_url": "https://feeds.example.org/minimal.xml",
                "artwork_url": null,
                "podcast_id": podcast_id_of("https://feeds.example.org/minimal.xml").to_string(),
            },
        ] })
    );
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn known_alias_resolves_to_the_canonical_podcast(pool: PgPool) {
    // The podcast was first subscribed at its old URL and has since moved,
    // so the directory's (new) URL is an alias of it.
    let canonical = common::create_podcast(&pool, "https://old.example.org/moved.xml").await;
    let moved = normalize_feed_url("https://feeds.example.org/moved.xml").expect("valid URL");
    sqlx::query!(
        "insert into podcast_alias (normalized_url, podcast_id) values ($1, $2)",
        moved.normalized(),
        canonical.as_uuid()
    )
    .execute(&pool)
    .await
    .expect("the alias is new");
    let (app, token) = app_with_user(pool).await;

    let response = app
        .with_token(Method::GET, "/v1/directory/search?q=moved", &token)
        .await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.body);
    let result = &response.body["results"][0];
    assert_eq!(result["feed_url"], "https://feeds.example.org/moved.xml");
    assert_eq!(result["podcast_id"], canonical.to_string());
    assert_ne!(
        canonical,
        podcast_id_of("https://feeds.example.org/moved.xml")
    );

    // A podcast subscribed at exactly the directory's URL has the derived id.
    let response = app
        .with_token(Method::GET, "/v1/directory/search?q=unknown%20show", &token)
        .await;
    assert_eq!(
        response.body["results"][0]["podcast_id"],
        podcast_id_of("https://feeds.example.org/unknown-show.xml").to_string()
    );
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn cache_hit_skips_upstream_until_the_hour_is_up(pool: PgPool) {
    let (app, token) = app_with_user(pool).await;
    let search = |path: &'static str| app.with_token(Method::GET, path, &token);

    let first = search("/v1/directory/search?q=cached%20term").await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.body);
    assert_eq!(hit_count("cached term"), 1);

    // Same term after normalization, and another limit: still cached.
    let second = search("/v1/directory/search?q=%20Cached%20%20TERM%20&limit=5").await;
    assert_eq!(second.body, first.body);
    assert_eq!(hit_count("cached term"), 1);

    app.advance_ms(HOUR_MS - 1);
    search("/v1/directory/search?q=cached%20term").await;
    assert_eq!(hit_count("cached term"), 1);

    app.advance_ms(1);
    let refreshed = search("/v1/directory/search?q=cached%20term").await;
    assert_eq!(refreshed.status, StatusCode::OK, "{}", refreshed.body);
    assert_eq!(hit_count("cached term"), 2);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn cached_results_pick_up_new_aliases(pool: PgPool) {
    let (app, token) = app_with_user(pool.clone()).await;
    let path = "/v1/directory/search?q=fresh%20alias";
    let before = app.with_token(Method::GET, path, &token).await;
    assert_eq!(
        before.body["results"][0]["podcast_id"],
        podcast_id_of("https://feeds.example.org/fresh-alias.xml").to_string()
    );

    let canonical = common::create_podcast(&pool, "https://old.example.org/fresh.xml").await;
    let moved = normalize_feed_url("https://feeds.example.org/fresh-alias.xml").expect("valid");
    sqlx::query!(
        "insert into podcast_alias (normalized_url, podcast_id) values ($1, $2)",
        moved.normalized(),
        canonical.as_uuid()
    )
    .execute(&pool)
    .await
    .expect("the alias is new");

    let after = app.with_token(Method::GET, path, &token).await;
    assert_eq!(hit_count("fresh alias"), 1, "the second search is cached");
    assert_eq!(
        after.body["results"][0]["podcast_id"],
        canonical.to_string()
    );
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn limit_defaults_to_25_and_clamps_to_1_through_50(pool: PgPool) {
    let (app, token) = app_with_user(pool).await;
    for (query, expected) in [
        ("q=many", 25),
        ("q=many&limit=3", 3),
        ("q=many&limit=0", 1),
        ("q=many&limit=500", 50),
    ] {
        let response = app
            .with_token(
                Method::GET,
                &format!("/v1/directory/search?{query}"),
                &token,
            )
            .await;
        assert_eq!(
            response.status,
            StatusCode::OK,
            "{query}: {}",
            response.body
        );
        let results = response.body["results"]
            .as_array()
            .expect("results is an array");
        assert_eq!(results.len(), expected, "{query}");
        assert_eq!(results[0]["title"], "Show 0", "{query}");
    }
    assert_eq!(hit_count("many"), 1);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn upstream_failures_are_502_and_not_cached(pool: PgPool) {
    let (app, token) = app_with_user(pool).await;
    for (term, query) in [
        ("status-500 a", "q=status-500%20a"),
        ("status-503 b", "q=status-503%20b"),
        ("status-404 c", "q=status-404%20c"),
        ("garbage", "q=garbage"),
    ] {
        let path = format!("/v1/directory/search?{query}");
        for attempt in 1..=2 {
            let response = app.with_token(Method::GET, &path, &token).await;
            assert_error(&response, StatusCode::BAD_GATEWAY, "upstream_unavailable");
            assert_eq!(hit_count(term), attempt, "{term}");
        }
    }
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn unreachable_directory_is_502(pool: PgPool) {
    // Port 9 (discard) is in the fetch policy because the operator
    // configured it, and nothing listens there in test environments.
    let app = TestApp::with_directory(pool, "http://127.0.0.1:9");
    let token = app.register("alice").await;
    let response = app
        .with_token(Method::GET, "/v1/directory/search?q=anything", &token)
        .await;
    assert_error(&response, StatusCode::BAD_GATEWAY, "upstream_unavailable");
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn bad_requests_and_missing_auth(pool: PgPool) {
    let (app, token) = app_with_user(pool).await;
    for query in [
        "",
        "?q=",
        "?q=%20%20",
        "?limit=5",
        "?q=x&limit=-1",
        "?q=x&limit=ten",
    ] {
        let response = app
            .with_token(Method::GET, &format!("/v1/directory/search{query}"), &token)
            .await;
        assert_error(&response, StatusCode::BAD_REQUEST, "bad_request");
    }

    let response = app
        .send(
            axum::http::Request::get("/v1/directory/search?q=daily")
                .body(axum::body::Body::empty())
                .expect("valid request"),
        )
        .await;
    assert_error(&response, StatusCode::UNAUTHORIZED, "unauthenticated");
    assert_eq!(hit_count("daily"), 0);
}
