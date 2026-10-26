//! Seed data for database tests, plus [`port`] for tests that serve HTTP.
//!
//! Each helper writes rows the way the real writer will: in one
//! transaction, with versions from the scope owner's counter (spec 2.1).
//! They panic on failure because a broken seed is a broken test.
//
// `create_podcast` and `create_episodes` write rows directly, so tests can
// pick exact versions without a podcast row write in between. Tests about
// production-shaped catalog rows ingest a feed built with [`parsed_feed`]
// and [`parsed_episode`] instead.
// TODO: seed sync tests through `pod_server::ingest` once they need real
// podcast metadata, so those seeds can't drift from production writes.

// Each test binary uses a different subset.
#![allow(dead_code)]

pub mod port;

use std::time::Duration;

use pod_protocol::{EpisodeId, PodcastId, Timestamp, UserId, normalize_feed_url};
use pod_server::db::SqlTimestamp;
use pod_server::feed::{ParsedEpisode, ParsedFeed, PodcastMetadata};
use pod_server::version::VersionScope;
use sqlx::{PgConnection, PgPool};

/// Inserts a user with an unusable password hash and an empty user scope.
pub async fn create_user(pool: &PgPool, username: &str) -> UserId {
    let user_id = UserId::new_v7();
    sqlx::query!(
        "insert into users (id, username, password_hash) values ($1, $2, '!')",
        user_id.as_uuid(),
        username
    )
    .execute(pool)
    .await
    .expect("the username is unused");
    user_id
}

/// Inserts a podcast as a first subscribe does (spec 4.4): `pending`, at
/// version 0, with its `feed_poll` and `podcast_alias` rows. Creating it
/// allocates no version, so no client receives the row until an ingest
/// writes it.
pub async fn create_podcast(pool: &PgPool, feed_url: &str) -> PodcastId {
    let url = normalize_feed_url(feed_url).expect("seed feed URLs are valid");
    let podcast_id = PodcastId::from_feed_url(&url);
    let mut tx = pool.begin().await.expect("a transaction starts");
    sqlx::query!(
        "insert into podcast (id, feed_url, version) values ($1, $2, 0)",
        podcast_id.as_uuid(),
        url.feed_url()
    )
    .execute(&mut *tx)
    .await
    .expect("the podcast is new");
    sqlx::query!(
        "insert into feed_poll (podcast_id) values ($1)",
        podcast_id.as_uuid()
    )
    .execute(&mut *tx)
    .await
    .expect("the podcast has no poll row yet");
    sqlx::query!(
        "insert into podcast_alias (normalized_url, podcast_id) values ($1, $2)",
        url.normalized(),
        podcast_id.as_uuid()
    )
    .execute(&mut *tx)
    .await
    .expect("the alias is new");
    tx.commit().await.expect("the seed commits");
    podcast_id
}

/// First `published_at` handed out, 2026-01-01T00:00:00Z.
const FIRST_PUBLISHED_MS: i64 = 1_767_225_600_000;
const DAY_MS: i64 = 86_400_000;

/// Appends `count` episodes to the podcast and returns their ids, oldest
/// first. Each episode is published one day after the previous one and
/// takes the next podcast-scope version, so versions follow publication
/// order as they do after an ingest (spec 3.3). The podcast row itself
/// stays as it is.
pub async fn create_episodes(pool: &PgPool, podcast_id: PodcastId, count: usize) -> Vec<EpisodeId> {
    let mut scope = VersionScope::begin_podcast(pool, podcast_id)
        .await
        .expect("the podcast row locks")
        .expect("the podcast exists");
    let mut ids = Vec::with_capacity(count);
    for _ in 0..count {
        let version = scope.next_version();
        ids.push(insert_episode(scope.conn(), podcast_id, version).await);
    }
    scope.commit().await.expect("the seed commits");
    ids
}

/// Inserts one episode at `version`, which the caller allocated from the
/// podcast's scope. The version doubles as the episode's ordinal: it names
/// the guid and sets `published_at` one day per version after 2026-01-01.
pub async fn insert_episode(
    conn: &mut PgConnection,
    podcast_id: PodcastId,
    version: i64,
) -> EpisodeId {
    // Versions are unique per podcast, so they make the guid unique too.
    let guid = format!("seed-{version}");
    let episode_id = EpisodeId::new(podcast_id, &guid);
    let published_at = SqlTimestamp(
        Timestamp::from_epoch_ms(FIRST_PUBLISHED_MS + version * DAY_MS)
            .expect("seed dates stay within a few years of 2026"),
    );
    // The guid stands in for the episode hash, which only ingest compares.
    sqlx::query!(
        "insert into episode (id, podcast_id, guid, title, published_at,
             audio_url, audio_type, content_hash, version)
         values ($1, $2, $3, $4, $5, $6, 'audio/mpeg', $7, $8)",
        episode_id.as_uuid(),
        podcast_id.as_uuid(),
        guid,
        format!("Episode {version}"),
        published_at as _,
        format!("https://media.example.com/{guid}.mp3"),
        guid.as_bytes(),
        version
    )
    .execute(conn)
    .await
    .expect("the guid is new for this podcast");
    episode_id
}

/// A feed as `parse_feed` would return it, with `episodes` in feed order.
pub fn parsed_feed(title: &str, episodes: Vec<ParsedEpisode>) -> ParsedFeed {
    ParsedFeed {
        metadata: PodcastMetadata {
            title: title.to_owned(),
            author: Some("Example Media".to_owned()),
            description: Some("<p>A show about examples.</p>".to_owned()),
            link: Some("https://example.com/show".to_owned()),
            artwork_url: Some("https://example.com/show.jpg".to_owned()),
            language: Some("en".to_owned()),
        },
        episodes,
        new_feed_url: None,
    }
}

/// An episode keyed by `guid`, published `day` days after 2026-01-01.
pub fn parsed_episode(podcast_id: PodcastId, guid: &str, day: i64) -> ParsedEpisode {
    ParsedEpisode {
        id: EpisodeId::new(podcast_id, guid),
        guid: guid.to_owned(),
        title: format!("Episode {guid}"),
        summary_html: "<p>Summary</p>".to_owned(),
        content_html: String::new(),
        link: None,
        published_at: Timestamp::from_epoch_ms(FIRST_PUBLISHED_MS + day * DAY_MS)
            .expect("seed dates stay within a few years of 2026"),
        duration_ms: Some(1_800_000),
        audio_url: format!("https://media.example.com/{guid}.mp3"),
        audio_type: "audio/mpeg".to_owned(),
        audio_bytes: None,
        artwork_url: None,
        season: None,
        number: None,
    }
}

/// Waits until `count` backends in the test database are blocked on a lock,
/// which proves concurrent writers reached the scope lock and are waiting.
pub async fn wait_for_lock_waiters(pool: &PgPool, count: i64) {
    for _ in 0..500 {
        let waiting = sqlx::query_scalar!(
            r#"select count(*) as "waiting!" from pg_stat_activity
               where datname = current_database() and wait_event_type = 'Lock'"#
        )
        .fetch_one(pool)
        .await
        .expect("pg_stat_activity is readable");
        if waiting == count {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{count} writers didn't block on the scope lock within 5 s");
}
