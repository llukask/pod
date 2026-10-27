//! The read endpoints for the web client (spec 4.5): podcasts, one podcast,
//! a podcast's episodes, the inbox and settings. The inbox rules themselves
//! are pinned by `conformance_inbox.rs`; this file covers the shapes,
//! pagination and access rules.

mod auth_app;
mod common;
mod read_seed;

use std::collections::BTreeMap;

use auth_app::{DAY_MS, assert_error};
use axum::http::{Method, StatusCode};
use pod_protocol::{
    EpisodeId, EpisodePage, InboxPage, PodcastId, PodcastItem, PodcastList, PodcastStatus,
    SettingsResponse, Timestamp,
};
use read_seed::{
    Reader, insert_episode, insert_podcast, item_ids, reader, set_episode_state, set_setting,
};
use serde_json::json;
use sqlx::PgPool;

/// 2026-10-01T00:00:00Z.
const BASE_MS: i64 = 1_790_812_800_000;

fn day(days: i64) -> Timestamp {
    Timestamp::from_epoch_ms(BASE_MS + days * DAY_MS).expect("2026 is in range")
}

fn podcast(n: u8) -> PodcastId {
    format!("5a0f3c1e-2b4d-4e6f-8a1b-3c5d7e9f00{n:02x}")
        .parse()
        .expect("valid UUID")
}

fn episode(podcast_id: PodcastId, n: u32) -> EpisodeId {
    EpisodeId::new(podcast_id, &format!("episode-{n}"))
}

/// The ids of `episodes` in the endpoints' order: `published_at`
/// descending, then id descending.
fn sorted_desc(mut episodes: Vec<(Timestamp, EpisodeId)>) -> Vec<String> {
    episodes.sort_by(|a, b| b.cmp(a));
    episodes.into_iter().map(|(_, id)| id.to_string()).collect()
}

/// Follows `next_cursor` from the first page to the last, asserting no page
/// is empty or over `limit`, and returns every id in order.
async fn walk_pages(reader: &Reader, path: &str, limit: u32) -> Vec<String> {
    let mut ids = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let url = match &cursor {
            None => format!("{path}?limit={limit}"),
            Some(cursor) => format!("{path}?limit={limit}&cursor={cursor}"),
        };
        let response = reader.get(&url).await;
        assert_eq!(response.status, StatusCode::OK, "{url}: {}", response.body);
        let page = item_ids(&response.body);
        assert!(!page.is_empty() && page.len() <= limit as usize, "{url}");
        ids.extend(page);
        match response.body["next_cursor"].as_str() {
            None => return ids,
            Some(next) => cursor = Some(next.to_owned()),
        }
    }
}

// ==============================================================================
// GET /v1/podcasts and /v1/podcasts/{id}
// ==============================================================================

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn podcasts_lists_subscriptions_newest_first(pool: PgPool) {
    let reader = reader(pool).await;
    let pool = reader.pool();
    let (older, newer, unsubscribed, foreign) = (podcast(1), podcast(2), podcast(3), podcast(4));
    for (id, title) in [
        (older, "Older"),
        (newer, "Newer"),
        (unsubscribed, "Unsubscribed"),
        (foreign, "Foreign"),
    ] {
        insert_podcast(pool, id, title).await;
        insert_episode(pool, id, episode(id, 1), day(1), false).await;
    }
    insert_episode(pool, older, episode(older, 2), day(5), false).await;
    insert_episode(pool, newer, episode(newer, 2), day(8), false).await;
    // A deleted episode doesn't count as the newest.
    insert_episode(pool, older, episode(older, 3), day(20), true).await;
    let pending = common::create_podcast(pool, "https://example.com/pending.xml").await;

    reader.subscribe(older, day(0)).await;
    reader.subscribe(newer, day(0)).await;
    reader.subscribe(pending, day(9)).await;
    reader.subscribe(unsubscribed, day(0)).await;
    reader.unsubscribe(unsubscribed, day(2)).await;

    let list: PodcastList = reader.get_ok("/v1/podcasts").await;
    let summary: Vec<_> = list
        .items
        .iter()
        .map(|item| {
            (
                item.podcast.id,
                item.podcast.status.kind(),
                item.podcast.title.as_deref(),
                item.latest_published_at,
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            (pending, PodcastStatus::Pending, None, None),
            (newer, PodcastStatus::Ok, Some("Newer"), Some(day(8))),
            (older, PodcastStatus::Ok, Some("Older"), Some(day(5))),
        ],
        "a pending podcast without episodes leads, then newest episode first"
    );
    assert_eq!(list.items[0].podcast.version, 0);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn podcasts_is_empty_without_subscriptions(pool: PgPool) {
    let reader = reader(pool).await;
    let list: PodcastList = reader.get_ok("/v1/podcasts").await;
    assert!(list.items.is_empty());
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn podcast_returns_only_subscribed_podcasts(pool: PgPool) {
    let reader = reader(pool).await;
    let pool = reader.pool();
    let (subscribed, unsubscribed, foreign) = (podcast(1), podcast(2), podcast(3));
    for id in [subscribed, unsubscribed, foreign] {
        insert_podcast(pool, id, "Show").await;
    }
    insert_episode(pool, subscribed, episode(subscribed, 1), day(3), false).await;
    reader.subscribe(subscribed, day(0)).await;
    reader.subscribe(unsubscribed, day(0)).await;
    reader.unsubscribe(unsubscribed, day(1)).await;
    // Subscribed, but the podcast row doesn't exist (an alias id).
    let rowless = podcast(9);
    reader.subscribe(rowless, day(0)).await;

    let item: PodcastItem = reader.get_ok(&format!("/v1/podcasts/{subscribed}")).await;
    assert_eq!(item.podcast.id, subscribed);
    assert_eq!(item.latest_published_at, Some(day(3)));

    for id in [unsubscribed, foreign, rowless] {
        for path in [
            format!("/v1/podcasts/{id}"),
            format!("/v1/podcasts/{id}/episodes"),
        ] {
            assert_error(&reader.get(&path).await, StatusCode::NOT_FOUND, "not_found");
        }
    }
    for path in [
        "/v1/podcasts/not-a-uuid",
        "/v1/podcasts/not-a-uuid/episodes",
    ] {
        assert_error(
            &reader.get(path).await,
            StatusCode::BAD_REQUEST,
            "bad_request",
        );
    }
}

// ==============================================================================
// GET /v1/podcasts/{id}/episodes
// ==============================================================================

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn episodes_come_newest_first_with_the_users_state(pool: PgPool) {
    let reader = reader(pool).await;
    let pool = reader.pool();
    let show = podcast(1);
    insert_podcast(pool, show, "Show").await;
    for n in 1..=4 {
        insert_episode(pool, show, episode(show, n), day(i64::from(n)), false).await;
    }
    insert_episode(pool, show, episode(show, 5), day(5), true).await;
    set_episode_state(
        pool,
        reader.user_id,
        show,
        episode(show, 2),
        90_000,
        true,
        false,
    )
    .await;
    reader.subscribe(show, day(0)).await;

    let page: EpisodePage = reader
        .get_ok(&format!("/v1/podcasts/{show}/episodes"))
        .await;
    let ids: Vec<EpisodeId> = page.items.iter().map(|item| item.episode.0.id).collect();
    assert_eq!(
        ids,
        [4, 3, 2, 1].map(|n| episode(show, n)),
        "newest first, deleted episodes left out"
    );
    assert_eq!(page.next_cursor, None);
    let states: Vec<_> = page
        .items
        .iter()
        .map(|item| {
            item.episode_state
                .as_ref()
                .map(|state| (state.episode_id, state.position_ms, state.played))
        })
        .collect();
    assert_eq!(
        states,
        [None, None, Some((episode(show, 2), 90_000, true)), None]
    );
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn episode_pages_are_stable_across_equal_published_at(pool: PgPool) {
    let reader = reader(pool).await;
    let pool = reader.pool();
    let show = podcast(1);
    insert_podcast(pool, show, "Show").await;
    reader.subscribe(show, day(0)).await;
    // Seven episodes share one instant, between two others, so every page
    // boundary below falls inside the tie.
    let mut seeded = vec![(day(9), episode(show, 0)), (day(1), episode(show, 99))];
    for n in 1..=7 {
        seeded.push((day(5), episode(show, n)));
    }
    for (published_at, id) in &seeded {
        insert_episode(pool, show, *id, *published_at, false).await;
    }
    let path = format!("/v1/podcasts/{show}/episodes");

    let expected = sorted_desc(seeded.clone());
    for limit in [1, 2, 3, 4] {
        assert_eq!(
            walk_pages(&reader, &path, limit).await,
            expected,
            "limit {limit}"
        );
    }

    // A cursor addresses a position, not an offset: an episode published
    // above it after the first page doesn't shift the pages that follow.
    let first = reader.get(&format!("{path}?limit=3")).await;
    let cursor = first.body["next_cursor"]
        .as_str()
        .expect("more pages follow")
        .to_owned();
    insert_episode(pool, show, episode(show, 50), day(5), false).await;
    insert_episode(pool, show, episode(show, 51), day(30), false).await;
    let rest = reader
        .get(&format!("{path}?limit=200&cursor={cursor}"))
        .await;
    let mut with_new = seeded;
    with_new.push((day(5), episode(show, 50)));
    let all_now = sorted_desc(with_new);
    let after_cursor: Vec<String> = all_now
        .iter()
        .skip_while(|id| **id != expected[2])
        .skip(1)
        .cloned()
        .collect();
    assert_eq!(item_ids(&rest.body), after_cursor);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn limit_is_clamped_and_must_be_a_number(pool: PgPool) {
    let reader = reader(pool).await;
    let pool = reader.pool();
    let show = podcast(1);
    insert_podcast(pool, show, "Show").await;
    reader.subscribe(show, day(0)).await;
    for n in 1..=3 {
        insert_episode(pool, show, episode(show, n), day(i64::from(n)), false).await;
    }
    let path = format!("/v1/podcasts/{show}/episodes");

    let zero = reader.get(&format!("{path}?limit=0")).await;
    assert_eq!(item_ids(&zero.body).len(), 1, "{}", zero.body);
    let huge = reader.get(&format!("{path}?limit=100000")).await;
    assert_eq!(item_ids(&huge.body).len(), 3, "{}", huge.body);
    assert!(huge.body["next_cursor"].is_null());
    // Empty parameters read as absent, as in the spec's `?cursor=&limit=`.
    let default = reader.get(&format!("{path}?cursor=&limit=")).await;
    assert_eq!(item_ids(&default.body).len(), 3, "{}", default.body);

    for path in [
        format!("{path}?limit=many"),
        "/v1/inbox?limit=-1".to_owned(),
    ] {
        assert_error(
            &reader.get(&path).await,
            StatusCode::BAD_REQUEST,
            "bad_request",
        );
    }
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn malformed_cursors_are_invalid_cursor(pool: PgPool) {
    let reader = reader(pool).await;
    let show = podcast(1);
    insert_podcast(reader.pool(), show, "Show").await;
    reader.subscribe(show, day(0)).await;
    // Not base64url, padded, and the wrong length.
    for cursor in [
        "!!!!",
        "AAAA",
        "AAAAAAAAAAA=",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    ] {
        for path in [
            format!("/v1/podcasts/{show}/episodes?cursor={cursor}"),
            format!("/v1/inbox?cursor={cursor}"),
        ] {
            assert_error(
                &reader.get(&path).await,
                StatusCode::BAD_REQUEST,
                "invalid_cursor",
            );
        }
    }
}

// ==============================================================================
// GET /v1/inbox
// ==============================================================================

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn inbox_items_carry_their_podcast_and_position(pool: PgPool) {
    let reader = reader(pool).await;
    let pool = reader.pool();
    let show = podcast(1);
    insert_podcast(pool, show, "Show").await;
    reader.subscribe(show, day(0)).await;
    insert_episode(pool, show, episode(show, 1), day(2), false).await;
    insert_episode(pool, show, episode(show, 2), day(3), false).await;
    set_episode_state(
        pool,
        reader.user_id,
        show,
        episode(show, 1),
        125_000,
        false,
        false,
    )
    .await;
    reader.set_now(day(4));

    let page: InboxPage = reader.get_ok("/v1/inbox").await;
    let items: Vec<_> = page
        .items
        .iter()
        .map(|item| {
            (
                item.episode.0.id,
                item.podcast_title.as_deref(),
                item.podcast_artwork_url.clone(),
                item.position_ms,
            )
        })
        .collect();
    let artwork = Some(format!("https://images.example.com/{show}.jpg"));
    assert_eq!(
        items,
        [
            (episode(show, 2), Some("Show"), artwork.clone(), 0),
            (episode(show, 1), Some("Show"), artwork, 125_000),
        ]
    );
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn inbox_pages_are_stable_across_equal_published_at(pool: PgPool) {
    let reader = reader(pool).await;
    let pool = reader.pool();
    let mut seeded = Vec::new();
    for (podcast_id, oldest) in [(podcast(1), day(1)), (podcast(2), day(2))] {
        insert_podcast(pool, podcast_id, "Show").await;
        reader.subscribe(podcast_id, day(0)).await;
        for n in 1..=4 {
            insert_episode(pool, podcast_id, episode(podcast_id, n), day(5), false).await;
            seeded.push((day(5), episode(podcast_id, n)));
        }
        insert_episode(pool, podcast_id, episode(podcast_id, 9), oldest, false).await;
        seeded.push((oldest, episode(podcast_id, 9)));
    }
    reader.set_now(day(6));

    let expected = sorted_desc(seeded);
    for limit in [1, 2, 3] {
        assert_eq!(
            walk_pages(&reader, "/v1/inbox", limit).await,
            expected,
            "limit {limit}"
        );
    }
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn an_unusable_window_setting_falls_back_to_30_days(pool: PgPool) {
    let reader = reader(pool).await;
    let pool = reader.pool();
    let show = podcast(1);
    insert_podcast(pool, show, "Show").await;
    reader.subscribe(show, day(0)).await;
    insert_episode(pool, show, episode(show, 1), day(1), false).await;
    insert_episode(pool, show, episode(show, 2), day(20), false).await;
    reader.set_now(day(31));

    for value in [json!("seven"), json!(-3), json!(2.5), json!(null)] {
        sqlx::query!("delete from user_setting")
            .execute(pool)
            .await
            .expect("settings are deletable");
        set_setting(
            pool,
            reader.user_id,
            "inbox_window_days",
            value.clone(),
            day(0),
        )
        .await;
        let response = reader.get("/v1/inbox").await;
        assert_eq!(
            item_ids(&response.body),
            [episode(show, 2), episode(show, 1)].map(|id| id.to_string()),
            "{value}"
        );
    }

    // A usable value applies: 15 days before day 31 excludes day 1.
    sqlx::query!("delete from user_setting")
        .execute(pool)
        .await
        .expect("settings are deletable");
    set_setting(pool, reader.user_id, "inbox_window_days", json!(15), day(0)).await;
    let response = reader.get("/v1/inbox").await;
    assert_eq!(item_ids(&response.body), [episode(show, 2).to_string()]);
}

// ==============================================================================
// GET /v1/settings
// ==============================================================================

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn settings_fill_in_defaults(pool: PgPool) {
    let reader = reader(pool).await;
    let defaults: SettingsResponse = reader.get_ok("/v1/settings").await;
    assert_eq!(
        defaults.settings,
        BTreeMap::from([
            ("inbox_window_days".to_owned(), json!(30)),
            ("playback_speed".to_owned(), json!(1.0)),
            ("played_threshold_ms".to_owned(), json!(30_000)),
            ("skip_back_ms".to_owned(), json!(15_000)),
            ("skip_forward_ms".to_owned(), json!(30_000)),
        ])
    );

    let pool = reader.pool();
    set_setting(pool, reader.user_id, "playback_speed", json!(1.5), day(0)).await;
    set_setting(
        pool,
        reader.user_id,
        "theme",
        json!({ "accent": "teal" }),
        day(0),
    )
    .await;
    let stored: SettingsResponse = reader.get_ok("/v1/settings").await;
    assert_eq!(stored.settings["playback_speed"], json!(1.5));
    assert_eq!(stored.settings["theme"], json!({ "accent": "teal" }));
    assert_eq!(stored.settings["skip_back_ms"], json!(15_000));
    assert_eq!(stored.settings.len(), 6);
}

// ==============================================================================
// Access
// ==============================================================================

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn read_endpoints_need_a_session(pool: PgPool) {
    let reader = reader(pool).await;
    let show = podcast(1);
    for path in [
        "/v1/podcasts".to_owned(),
        format!("/v1/podcasts/{show}"),
        format!("/v1/podcasts/{show}/episodes"),
        "/v1/inbox".to_owned(),
        "/v1/settings".to_owned(),
    ] {
        let response = reader
            .app
            .with_token(Method::GET, &path, "not-a-token")
            .await;
        assert_error(&response, StatusCode::UNAUTHORIZED, "unauthenticated");
    }
}
