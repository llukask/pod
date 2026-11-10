//! `GET /v1/episodes/{id}/chapters` (spec 17) against a local stub that
//! serves chapter files and audio files with ID3 tags.
//!
//! The stub runs once per test binary on a port from [`bind_test_port`],
//! because sandboxed development environments can only bind a few fixed
//! ports. Tests tell their requests apart by path, each using its own.

mod auth_app;
mod common;
mod read_seed;

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use auth_app::{DAY_MS, assert_error};
use axum::Router;
use axum::extract::Path;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use common::port::bind_test_port;
use id3::frame::{Chapter as Id3Chapter, Frame};
use id3::{TagLike, Version};
use pod_protocol::{ChapterList, EpisodeId, PodcastId, Timestamp};
use read_seed::{Reader, insert_episode, insert_podcast, reader};
use serde_json::json;
use sqlx::PgPool;

/// 2026-10-01T00:00:00Z.
const BASE_MS: i64 = 1_790_812_800_000;

fn day(days: i64) -> Timestamp {
    Timestamp::from_epoch_ms(BASE_MS + days * DAY_MS).expect("2026 is in range")
}

fn podcast(n: u8) -> PodcastId {
    format!("7c1e2a3b-4d5e-4f60-8a1b-2c3d4e5f60{n:02x}")
        .parse()
        .expect("valid UUID")
}

fn episode(podcast_id: PodcastId, n: u32) -> EpisodeId {
    EpisodeId::new(podcast_id, &format!("episode-{n}"))
}

// ==============================================================================
// Stub
// ==============================================================================

/// The `Range` header of every request, by path, in order.
fn requests() -> &'static Mutex<HashMap<String, Vec<Option<String>>>> {
    static REQUESTS: OnceLock<Mutex<HashMap<String, Vec<Option<String>>>>> = OnceLock::new();
    REQUESTS.get_or_init(Mutex::default)
}

fn requests_to(path: &str) -> Vec<Option<String>> {
    requests()
        .lock()
        .expect("no test panics while holding the lock")
        .get(path)
        .cloned()
        .unwrap_or_default()
}

/// The stub's base URL, started on first use.
fn stub_url() -> String {
    static URL: OnceLock<String> = OnceLock::new();
    URL.get_or_init(|| {
        let listener = bind_test_port("POD_TEST_CHAPTERS_PORT");
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
                axum::serve(listener, Router::new().route("/{name}", get(serve)))
                    .await
                    .expect("the stub runs until the process exits");
            });
        });
        format!("http://{address}")
    })
    .clone()
}

/// An ID3v2.4 tag with two chapters, the second linked.
fn tag() -> Vec<u8> {
    let mut tag = id3::Tag::new();
    for (id, start, title) in [("ch0", 0, "Cold open"), ("ch1", 95_000, "Interview")] {
        tag.add_frame(Id3Chapter {
            element_id: id.to_owned(),
            start_time: start,
            end_time: start + 1000,
            start_offset: u32::MAX,
            end_offset: u32::MAX,
            frames: vec![Frame::text("TIT2", title)],
        });
    }
    let mut bytes = Vec::new();
    tag.write_to(&mut bytes, Version::Id3v24)
        .expect("an in-memory tag writes");
    bytes
}

/// Serves `/<name>`:
///
/// - `*.json` is a chapters file; `broken*.json` answers 500.
/// - `tagged*.mp3` is an ID3 tag and fake audio, honouring `Range`.
/// - `stubborn*.mp3` is the same file but ignores `Range`.
/// - `plain*.mp3` is audio without a tag; `missing*.mp3` answers 404.
async fn serve(Path(name): Path<String>, headers: HeaderMap) -> Response {
    let range = headers
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    requests()
        .lock()
        .expect("no test panics while holding the lock")
        .entry(format!("/{name}"))
        .or_default()
        .push(range.clone());

    if name.starts_with("broken") || name.starts_with("missing") {
        let status = if name.starts_with("broken") {
            StatusCode::INTERNAL_SERVER_ERROR
        } else {
            StatusCode::NOT_FOUND
        };
        return status.into_response();
    }
    if name.ends_with(".json") {
        return axum::Json(json!({
            "version": "1.2.0",
            "chapters": [
                { "startTime": 0, "title": "Welcome" },
                { "startTime": 30.25, "title": "Sponsor", "toc": false },
                { "startTime": 61.5, "title": "News", "url": "https://example.com/news" }
            ]
        }))
        .into_response();
    }
    let mut file = if name.starts_with("plain") {
        Vec::new()
    } else {
        tag()
    };
    file.extend(std::iter::repeat_n(0xffu8, 256 * 1024));
    let ranged = name.starts_with("tagged");
    match range
        .as_deref()
        .and_then(|range| range.strip_prefix("bytes=0-"))
    {
        Some(last) if ranged => {
            let last: usize = last.parse().expect("the server asks for a closed range");
            let end = (last + 1).min(file.len());
            (
                StatusCode::PARTIAL_CONTENT,
                [(header::CONTENT_TYPE, "audio/mpeg")],
                file[..end].to_vec(),
            )
                .into_response()
        }
        _ => ([(header::CONTENT_TYPE, "audio/mpeg")], file).into_response(),
    }
}

// ==============================================================================
// Seeding
// ==============================================================================

/// A subscribed show with one episode whose sources are set as given.
async fn seeded(
    reader: &Reader,
    n: u8,
    audio: &str,
    chapters_url: Option<&str>,
) -> (PodcastId, EpisodeId) {
    let pool = reader.pool();
    let show = podcast(n);
    insert_podcast(pool, show, "Show").await;
    reader.subscribe(show, day(0)).await;
    let id = episode(show, 1);
    insert_episode(pool, show, id, day(1), false).await;
    set_sources(pool, id, &format!("{}/{audio}", stub_url()), chapters_url).await;
    (show, id)
}

async fn set_sources(pool: &PgPool, id: EpisodeId, audio_url: &str, chapters_url: Option<&str>) {
    sqlx::query!(
        "update episode set audio_url = $2, chapters_url = $3 where id = $1",
        id.as_uuid(),
        audio_url,
        chapters_url.map(|path| format!("{}/{path}", stub_url())),
    )
    .execute(pool)
    .await
    .expect("the episode updates");
}

fn summary(list: &ChapterList) -> Vec<(i64, &str)> {
    list.chapters
        .iter()
        .map(|chapter| (chapter.start_ms, chapter.title.as_str()))
        .collect()
}

// ==============================================================================
// Tests
// ==============================================================================

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn feed_chapters_need_no_fetch(pool: PgPool) {
    let reader = reader(pool).await;
    let (_, id) = seeded(&reader, 1, "missing-feed.mp3", None).await;
    sqlx::query!(
        r#"update episode set feed_chapters = '[{"start_ms": 0, "title": "From the feed", "url": null}]'
           where id = $1"#,
        id.as_uuid()
    )
    .execute(reader.pool())
    .await
    .expect("the episode updates");

    let list: ChapterList = reader.get_ok(&format!("/v1/episodes/{id}/chapters")).await;
    assert_eq!(summary(&list), [(0, "From the feed")]);
    assert!(requests_to("/missing-feed.mp3").is_empty());
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_json_file_is_fetched_once_and_cached(pool: PgPool) {
    let reader = reader(pool).await;
    let (_, id) = seeded(&reader, 2, "missing-json.mp3", Some("cached.json")).await;

    for _ in 0..2 {
        let list: ChapterList = reader.get_ok(&format!("/v1/episodes/{id}/chapters")).await;
        assert_eq!(summary(&list), [(0, "Welcome"), (61_500, "News")]);
        assert_eq!(
            list.chapters[1].url.as_deref(),
            Some("https://example.com/news")
        );
    }
    assert_eq!(requests_to("/cached.json").len(), 1);
    assert!(requests_to("/missing-json.mp3").is_empty());
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn id3_chapters_come_from_two_small_range_requests(pool: PgPool) {
    let reader = reader(pool).await;
    let (_, id) = seeded(&reader, 3, "tagged-range.mp3", None).await;

    let list: ChapterList = reader.get_ok(&format!("/v1/episodes/{id}/chapters")).await;
    assert_eq!(summary(&list), [(0, "Cold open"), (95_000, "Interview")]);
    let size = tag().len();
    assert_eq!(
        requests_to("/tagged-range.mp3"),
        [
            Some("bytes=0-9".to_owned()),
            Some(format!("bytes=0-{}", size - 1))
        ]
    );
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_host_that_ignores_range_still_works(pool: PgPool) {
    let reader = reader(pool).await;
    let (_, id) = seeded(&reader, 4, "stubborn.mp3", None).await;

    let list: ChapterList = reader.get_ok(&format!("/v1/episodes/{id}/chapters")).await;
    assert_eq!(summary(&list), [(0, "Cold open"), (95_000, "Interview")]);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_failed_json_file_falls_back_to_id3_without_caching(pool: PgPool) {
    let reader = reader(pool).await;
    let (_, id) = seeded(
        &reader,
        5,
        "tagged-fallback.mp3",
        Some("broken-fallback.json"),
    )
    .await;

    for _ in 0..2 {
        let list: ChapterList = reader.get_ok(&format!("/v1/episodes/{id}/chapters")).await;
        assert_eq!(summary(&list), [(0, "Cold open"), (95_000, "Interview")]);
    }
    // Nothing was cached, so the second request asked again.
    assert_eq!(requests_to("/broken-fallback.json").len(), 2);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn no_chapters_is_cached_for_a_week(pool: PgPool) {
    let reader = reader(pool).await;
    let (_, id) = seeded(&reader, 6, "plain-week.mp3", None).await;
    let path = format!("/v1/episodes/{id}/chapters");

    let list: ChapterList = reader.get_ok(&path).await;
    assert!(list.chapters.is_empty());
    reader.set_now(
        Timestamp::from_epoch_ms(reader.app.now().as_epoch_ms() + 6 * DAY_MS).expect("in range"),
    );
    let _: ChapterList = reader.get_ok(&path).await;
    assert_eq!(requests_to("/plain-week.mp3").len(), 1);

    reader.set_now(
        Timestamp::from_epoch_ms(reader.app.now().as_epoch_ms() + 2 * DAY_MS).expect("in range"),
    );
    let _: ChapterList = reader.get_ok(&path).await;
    assert_eq!(requests_to("/plain-week.mp3").len(), 2);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn every_source_failing_is_upstream_unavailable(pool: PgPool) {
    let reader = reader(pool).await;
    let (_, id) = seeded(&reader, 7, "missing-all.mp3", Some("broken-all.json")).await;

    assert_error(
        &reader.get(&format!("/v1/episodes/{id}/chapters")).await,
        StatusCode::BAD_GATEWAY,
        "upstream_unavailable",
    );
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn unreadable_episodes_are_not_found(pool: PgPool) {
    let reader = reader(pool).await;
    let pool = reader.pool();
    let (show, _) = seeded(&reader, 8, "plain-unread.mp3", None).await;
    let deleted = episode(show, 2);
    insert_episode(pool, show, deleted, day(2), true).await;
    let other = podcast(9);
    insert_podcast(pool, other, "Other").await;
    let unsubscribed = episode(other, 1);
    insert_episode(pool, other, unsubscribed, day(1), false).await;

    for id in [deleted, unsubscribed, episode(show, 99)] {
        assert_error(
            &reader.get(&format!("/v1/episodes/{id}/chapters")).await,
            StatusCode::NOT_FOUND,
            "not_found",
        );
    }
    assert_error(
        &reader.get("/v1/episodes/not-a-uuid/chapters").await,
        StatusCode::BAD_REQUEST,
        "bad_request",
    );
    assert!(requests_to("/plain-unread.mp3").is_empty());
}
