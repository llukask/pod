//! The seed helpers build rows the way the real writers will: versions come
//! from the scope owner's counter (spec 2.1).

mod common;

use pod_protocol::EpisodeId;
use sqlx::PgPool;

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn seeded_user_starts_with_an_empty_scope(pool: PgPool) {
    let user_id = common::create_user(&pool, "alice").await;
    let user = sqlx::query!(
        "select username, version_counter from users where id = $1",
        user_id.as_uuid()
    )
    .fetch_one(&pool)
    .await
    .expect("the seeded user exists");
    assert_eq!(user.username, "alice");
    assert_eq!(user.version_counter, 0);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn seeded_podcast_is_pending_and_scheduled(pool: PgPool) {
    let podcast_id = common::create_podcast(&pool, "http://Feeds.Example.com/daily.xml").await;
    let podcast = sqlx::query!(
        r#"select p.feed_url, p.status, p.version, p.version_counter,
                  a.normalized_url as "normalized_url?", f.podcast_id as "polled?"
           from podcast p
           left join podcast_alias a on a.podcast_id = p.id
           left join feed_poll f on f.podcast_id = p.id
           where p.id = $1"#,
        podcast_id.as_uuid()
    )
    .fetch_one(&pool)
    .await
    .expect("the seeded podcast exists");
    // The stored URL keeps its scheme; only the alias (and the id) use the
    // normalized form.
    assert_eq!(podcast.feed_url, "http://feeds.example.com/daily.xml");
    assert_eq!(podcast.status, "pending");
    assert_eq!((podcast.version, podcast.version_counter), (0, 0));
    assert_eq!(
        podcast.normalized_url.as_deref(),
        Some("https://feeds.example.com/daily.xml")
    );
    assert!(podcast.polled.is_some(), "a feed_poll row exists");
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn seeded_episodes_take_versions_from_the_podcast_counter(pool: PgPool) {
    let podcast_id = common::create_podcast(&pool, "https://feeds.example.com/daily.xml").await;
    let mut seeded = common::create_episodes(&pool, podcast_id, 3).await;
    seeded.extend(common::create_episodes(&pool, podcast_id, 2).await);

    let episodes = sqlx::query!(
        "select id, version from episode where podcast_id = $1 order by published_at",
        podcast_id.as_uuid()
    )
    .fetch_all(&pool)
    .await
    .expect("episodes are readable");
    let ids: Vec<EpisodeId> = episodes
        .iter()
        .map(|row| EpisodeId::from_uuid(row.id))
        .collect();
    let versions: Vec<i64> = episodes.iter().map(|row| row.version).collect();
    assert_eq!(ids, seeded, "returned oldest first");
    assert_eq!(versions, [1, 2, 3, 4, 5]);

    let counter = sqlx::query_scalar!(
        "select version_counter from podcast where id = $1",
        podcast_id.as_uuid()
    )
    .fetch_one(&pool)
    .await
    .expect("the podcast exists");
    assert_eq!(counter, 5);
}
