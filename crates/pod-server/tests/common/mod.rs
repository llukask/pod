//! Seed data for database tests.
//!
//! Each helper writes rows the way the real writer will: in one
//! transaction, with versions from the scope owner's counter (spec 2.1).
//! They panic on failure because a broken seed is a broken test.
// TODO: switch to the version allocation helper (KIL-11) and the ingest
// writer (KIL-15) once they exist, so seeds can't drift from production
// writes.

// Each test binary uses a different subset.
#![allow(dead_code)]

use pod_protocol::{EpisodeId, PodcastId, Timestamp, UserId, normalize_feed_url};
use pod_server::db::SqlTimestamp;
use sqlx::PgPool;

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
    let mut tx = pool.begin().await.expect("a transaction starts");
    let mut counter = sqlx::query_scalar!(
        "select version_counter from podcast where id = $1 for update",
        podcast_id.as_uuid()
    )
    .fetch_one(&mut *tx)
    .await
    .expect("the podcast exists");

    let mut ids = Vec::with_capacity(count);
    for _ in 0..count {
        counter += 1;
        // The counter is unique per podcast, so it makes the guid unique too.
        let guid = format!("seed-{counter}");
        let episode_id = EpisodeId::new(podcast_id, &guid);
        let published_at = SqlTimestamp(
            Timestamp::from_epoch_ms(FIRST_PUBLISHED_MS + counter * DAY_MS)
                .expect("seed dates stay within a few years of 2026"),
        );
        // The guid stands in for the episode hash, which only ingest
        // compares.
        sqlx::query!(
            "insert into episode (id, podcast_id, guid, title, published_at,
                 audio_url, audio_type, content_hash, version)
             values ($1, $2, $3, $4, $5, $6, 'audio/mpeg', $7, $8)",
            episode_id.as_uuid(),
            podcast_id.as_uuid(),
            guid,
            format!("Episode {counter}"),
            published_at as _,
            format!("https://media.example.com/{guid}.mp3"),
            guid.as_bytes(),
            counter
        )
        .execute(&mut *tx)
        .await
        .expect("the guid is new for this podcast");
        ids.push(episode_id);
    }

    sqlx::query!(
        "update podcast set version_counter = $2 where id = $1",
        podcast_id.as_uuid(),
        counter
    )
    .execute(&mut *tx)
    .await
    .expect("the locked podcast row updates");
    tx.commit().await.expect("the seed commits");
    ids
}
