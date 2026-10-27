//! Ingest, status and purge writes to the podcast scope (spec 2.2, 3.2,
//! 3.3), checked against Postgres.

mod common;

use pod_protocol::{EpisodeId, FeedError, FeedStatus, PodcastId, Timestamp, normalize_feed_url};
use pod_server::feed::{ParsedFeed, parse_feed};
use pod_server::ingest::{
    IngestOutcome, IngestSummary, PurgeOutcome, StatusWrite, ingest, purge_episode, set_status,
};
use sqlx::PgPool;

const FEED_URL: &str = "https://feeds.example.com/daily.xml";
const SINCE: &str = "2026-10-20T05:00:00.000Z";

/// Failing with an HTTP `status` since [`SINCE`].
fn failing(status: u16) -> FeedStatus {
    FeedStatus::Failing {
        failing_since: SINCE.parse::<Timestamp>().expect("a valid timestamp"),
        error: FeedError::Http { status },
    }
}

#[derive(Debug, PartialEq, Eq)]
struct PodcastRow {
    status: String,
    feed_error: Option<serde_json::Value>,
    title: Option<String>,
    version: i64,
    version_counter: i64,
    metadata_hash: Option<Vec<u8>>,
}

async fn podcast_row(pool: &PgPool, podcast_id: PodcastId) -> PodcastRow {
    sqlx::query_as!(
        PodcastRow,
        "select status, feed_error, title, version, version_counter, metadata_hash
         from podcast where id = $1",
        podcast_id.as_uuid()
    )
    .fetch_one(pool)
    .await
    .expect("the podcast exists")
}

/// `(guid, title, deleted, version)` of every episode, by version.
async fn episodes(pool: &PgPool, podcast_id: PodcastId) -> Vec<(String, String, bool, i64)> {
    sqlx::query!(
        "select guid, title, deleted, version from episode
         where podcast_id = $1 order by version",
        podcast_id.as_uuid()
    )
    .fetch_all(pool)
    .await
    .expect("the episodes are readable")
    .into_iter()
    .map(|row| (row.guid, row.title, row.deleted, row.version))
    .collect()
}

async fn stored_feed_hash(pool: &PgPool, podcast_id: PodcastId) -> Option<Vec<u8>> {
    sqlx::query_scalar!(
        "select content_hash from feed_poll where podcast_id = $1",
        podcast_id.as_uuid()
    )
    .fetch_one(pool)
    .await
    .expect("the podcast has a poll row")
}

async fn ingest_ok(pool: &PgPool, podcast_id: PodcastId, feed: &ParsedFeed) -> IngestOutcome {
    ingest(pool, podcast_id, feed)
        .await
        .expect("the ingest runs")
        .expect("the podcast exists")
}

fn applied(outcome: IngestOutcome) -> IngestSummary {
    match outcome {
        IngestOutcome::Applied(summary) => summary,
        IngestOutcome::Unchanged => panic!("the ingest wrote nothing"),
    }
}

/// Three episodes in feed order (newest first), with `b` and `a` sharing a
/// publication date so the guid tie-break shows.
async fn seeded(pool: &PgPool) -> (PodcastId, ParsedFeed) {
    let podcast_id = common::create_podcast(pool, FEED_URL).await;
    let feed = common::parsed_feed(
        "The Daily",
        vec![
            common::parsed_episode(podcast_id, "c", 3),
            common::parsed_episode(podcast_id, "b", 1),
            common::parsed_episode(podcast_id, "a", 1),
        ],
    );
    (podcast_id, feed)
}

// =============================================================================
// Ingest
// =============================================================================

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn first_ingest_writes_the_podcast_then_episodes_oldest_first(pool: PgPool) {
    let (podcast_id, feed) = seeded(&pool).await;

    let summary = applied(ingest_ok(&pool, podcast_id, &feed).await);

    let ids = |guids: &[&str]| -> Vec<EpisodeId> {
        guids
            .iter()
            .map(|guid| EpisodeId::new(podcast_id, guid))
            .collect()
    };
    assert_eq!(
        summary,
        IngestSummary {
            podcast_written: true,
            inserted: ids(&["a", "b", "c"]),
            updated: 0,
            restored: 0,
        }
    );
    let podcast = podcast_row(&pool, podcast_id).await;
    assert_eq!(
        podcast,
        PodcastRow {
            status: "ok".to_owned(),
            feed_error: None,
            title: Some("The Daily".to_owned()),
            version: 1,
            version_counter: 4,
            metadata_hash: Some(feed.metadata.content_hash().as_bytes().to_vec()),
        }
    );
    let versions: Vec<(String, i64)> = episodes(&pool, podcast_id)
        .await
        .into_iter()
        .map(|(guid, _, _, version)| (guid, version))
        .collect();
    assert_eq!(
        versions,
        [("a".into(), 2), ("b".into(), 3), ("c".into(), 4)]
    );
    assert_eq!(
        stored_feed_hash(&pool, podcast_id).await,
        Some(feed.feed_hash().as_bytes().to_vec())
    );
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_real_feed_ingests_in_publication_order(pool: PgPool) {
    let url = "https://feeds.npr.org/510289/podcast.xml";
    let podcast_id = common::create_podcast(&pool, url).await;
    let bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/feeds/npr-planet-money.xml"
    ))
    .expect("the fixture exists");
    let feed = parse_feed(podcast_id, url, &bytes).expect("the fixture parses");
    assert!(feed.episodes.len() > 1, "the fixture has several episodes");

    let summary = applied(ingest_ok(&pool, podcast_id, &feed).await);
    assert_eq!(summary.inserted.len(), feed.episodes.len());

    let published = sqlx::query_scalar!(
        r#"select published_at as "published_at: pod_server::db::SqlTimestamp"
           from episode where podcast_id = $1 order by version"#,
        podcast_id.as_uuid()
    )
    .fetch_all(&pool)
    .await
    .expect("the episodes are readable");
    assert!(published.is_sorted(), "versions follow publication order");
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn reingesting_the_same_feed_writes_nothing(pool: PgPool) {
    let (podcast_id, feed) = seeded(&pool).await;
    ingest_ok(&pool, podcast_id, &feed).await;
    let before = (
        podcast_row(&pool, podcast_id).await,
        episodes(&pool, podcast_id).await,
    );

    assert_eq!(
        ingest_ok(&pool, podcast_id, &feed).await,
        IngestOutcome::Unchanged
    );

    let after = (
        podcast_row(&pool, podcast_id).await,
        episodes(&pool, podcast_id).await,
    );
    assert_eq!(before, after);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_changed_episode_gets_a_new_version(pool: PgPool) {
    let (podcast_id, mut feed) = seeded(&pool).await;
    ingest_ok(&pool, podcast_id, &feed).await;

    feed.episodes[1].title = "Corrected title".to_owned();
    let summary = applied(ingest_ok(&pool, podcast_id, &feed).await);

    assert_eq!(
        summary,
        IngestSummary {
            updated: 1,
            ..IngestSummary::default()
        }
    );
    let podcast = podcast_row(&pool, podcast_id).await;
    assert_eq!((podcast.version, podcast.version_counter), (1, 5));
    assert_eq!(
        episodes(&pool, podcast_id).await,
        [
            ("a".into(), "Episode a".into(), false, 2),
            ("c".into(), "Episode c".into(), false, 4),
            ("b".into(), "Corrected title".into(), false, 5),
        ]
    );
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn an_episode_dropped_from_the_feed_stays(pool: PgPool) {
    let (podcast_id, mut feed) = seeded(&pool).await;
    ingest_ok(&pool, podcast_id, &feed).await;
    let before = episodes(&pool, podcast_id).await;

    feed.episodes.remove(0);
    let summary = applied(ingest_ok(&pool, podcast_id, &feed).await);

    assert_eq!(summary, IngestSummary::default());
    assert_eq!(episodes(&pool, podcast_id).await, before);
    assert_eq!(podcast_row(&pool, podcast_id).await.version_counter, 4);
    assert_eq!(
        stored_feed_hash(&pool, podcast_id).await,
        Some(feed.feed_hash().as_bytes().to_vec()),
        "the shorter feed is the new baseline"
    );
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn changed_metadata_rewrites_only_the_podcast_row(pool: PgPool) {
    let (podcast_id, mut feed) = seeded(&pool).await;
    ingest_ok(&pool, podcast_id, &feed).await;

    feed.metadata.title = "The Daily, renamed".to_owned();
    let summary = applied(ingest_ok(&pool, podcast_id, &feed).await);

    assert_eq!(
        summary,
        IngestSummary {
            podcast_written: true,
            ..IngestSummary::default()
        }
    );
    let podcast = podcast_row(&pool, podcast_id).await;
    assert_eq!(podcast.title.as_deref(), Some("The Daily, renamed"));
    assert_eq!((podcast.version, podcast.version_counter), (5, 5));
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_missing_podcast_is_none(pool: PgPool) {
    let url = normalize_feed_url(FEED_URL).expect("the test URL is valid");
    let podcast_id = PodcastId::from_feed_url(&url);
    let feed = common::parsed_feed("Nobody", Vec::new());

    let ingested = ingest(&pool, podcast_id, &feed)
        .await
        .expect("the ingest runs");
    let status = set_status(&pool, podcast_id, &failing(500))
        .await
        .expect("the status write runs");
    let purged = purge_episode(&pool, EpisodeId::new(podcast_id, "a"))
        .await
        .expect("the purge runs");

    assert_eq!(ingested, None);
    assert_eq!(status, None);
    assert_eq!(purged, None);
}

// =============================================================================
// Status
// =============================================================================

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_status_change_is_a_new_podcast_version(pool: PgPool) {
    let (podcast_id, feed) = seeded(&pool).await;
    ingest_ok(&pool, podcast_id, &feed).await;

    let write = set_status(&pool, podcast_id, &failing(500))
        .await
        .expect("the status write runs");

    assert_eq!(write, Some(StatusWrite::Written { version: 5 }));
    let podcast = podcast_row(&pool, podcast_id).await;
    assert_eq!(
        (
            podcast.status.as_str(),
            podcast.feed_error,
            podcast.version,
            podcast.version_counter
        ),
        (
            "failing",
            Some(serde_json::json!({ "code": "http", "status": 500 })),
            5,
            5
        )
    );
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn an_unchanged_status_allocates_no_version(pool: PgPool) {
    let podcast_id = common::create_podcast(&pool, FEED_URL).await;
    set_status(&pool, podcast_id, &failing(500))
        .await
        .expect("the status write runs");

    let write = set_status(&pool, podcast_id, &failing(500))
        .await
        .expect("the status write runs");

    assert_eq!(write, Some(StatusWrite::Unchanged));
    assert_eq!(podcast_row(&pool, podcast_id).await.version_counter, 1);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_new_reason_alone_is_a_change(pool: PgPool) {
    let podcast_id = common::create_podcast(&pool, FEED_URL).await;
    set_status(&pool, podcast_id, &failing(500))
        .await
        .expect("the status write runs");

    let timed_out = FeedStatus::Failing {
        failing_since: SINCE.parse().expect("a valid timestamp"),
        error: FeedError::Timeout,
    };
    let write = set_status(&pool, podcast_id, &timed_out)
        .await
        .expect("the status write runs");

    assert_eq!(write, Some(StatusWrite::Written { version: 2 }));
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn an_ingest_of_a_failing_podcast_folds_ok_into_the_podcast_row(pool: PgPool) {
    let (podcast_id, mut feed) = seeded(&pool).await;
    ingest_ok(&pool, podcast_id, &feed).await;
    set_status(&pool, podcast_id, &failing(500))
        .await
        .expect("the status write runs");

    // Metadata unchanged: only the status makes the podcast row change.
    feed.episodes
        .push(common::parsed_episode(podcast_id, "d", 4));
    let summary = applied(ingest_ok(&pool, podcast_id, &feed).await);

    assert_eq!(
        summary,
        IngestSummary {
            podcast_written: true,
            inserted: vec![EpisodeId::new(podcast_id, "d")],
            ..IngestSummary::default()
        }
    );
    let podcast = podcast_row(&pool, podcast_id).await;
    assert_eq!(
        (
            podcast.status.as_str(),
            podcast.feed_error,
            podcast.version,
            podcast.version_counter
        ),
        ("ok", None, 6, 7)
    );
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_skipped_ingest_leaves_the_status_to_the_poller(pool: PgPool) {
    let (podcast_id, feed) = seeded(&pool).await;
    ingest_ok(&pool, podcast_id, &feed).await;
    set_status(&pool, podcast_id, &failing(500))
        .await
        .expect("the status write runs");

    assert_eq!(
        ingest_ok(&pool, podcast_id, &feed).await,
        IngestOutcome::Unchanged
    );
    assert_eq!(podcast_row(&pool, podcast_id).await.status, "failing");
}

// =============================================================================
// Purge
// =============================================================================

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn purging_marks_the_episode_deleted_once(pool: PgPool) {
    let (podcast_id, feed) = seeded(&pool).await;
    ingest_ok(&pool, podcast_id, &feed).await;
    let episode_id = EpisodeId::new(podcast_id, "b");

    let first = purge_episode(&pool, episode_id)
        .await
        .expect("the purge runs");
    let second = purge_episode(&pool, episode_id)
        .await
        .expect("the purge runs");

    assert_eq!(first, Some(PurgeOutcome::Purged { version: 5 }));
    assert_eq!(second, Some(PurgeOutcome::AlreadyDeleted));
    assert_eq!(
        episodes(&pool, podcast_id).await.last(),
        Some(&("b".into(), "Episode b".into(), true, 5))
    );
    assert_eq!(podcast_row(&pool, podcast_id).await.version_counter, 5);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_purged_episode_stays_purged_when_other_episodes_change(pool: PgPool) {
    let (podcast_id, mut feed) = seeded(&pool).await;
    ingest_ok(&pool, podcast_id, &feed).await;
    purge_episode(&pool, EpisodeId::new(podcast_id, "b"))
        .await
        .expect("the purge runs");

    feed.episodes
        .push(common::parsed_episode(podcast_id, "d", 4));
    feed.episodes[0].title = "Corrected title".to_owned();
    let summary = applied(ingest_ok(&pool, podcast_id, &feed).await);

    assert_eq!(
        (summary.restored, summary.updated, summary.inserted.len()),
        (0, 1, 1)
    );
    assert_eq!(
        episodes(&pool, podcast_id).await,
        [
            ("a".into(), "Episode a".into(), false, 2),
            ("b".into(), "Episode b".into(), true, 5),
            ("c".into(), "Corrected title".into(), false, 6),
            ("d".into(), "Episode d".into(), false, 7),
        ]
    );
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_purged_episode_whose_content_changes_comes_back(pool: PgPool) {
    let (podcast_id, mut feed) = seeded(&pool).await;
    ingest_ok(&pool, podcast_id, &feed).await;
    purge_episode(&pool, EpisodeId::new(podcast_id, "b"))
        .await
        .expect("the purge runs");

    feed.episodes[1].title = "Republished".to_owned();
    let summary = applied(ingest_ok(&pool, podcast_id, &feed).await);

    assert_eq!((summary.restored, summary.updated), (1, 0));
    assert_eq!(
        episodes(&pool, podcast_id).await.last(),
        Some(&("b".into(), "Republished".into(), false, 6))
    );
}

// =============================================================================
// Concurrency
// =============================================================================

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn concurrent_ingests_of_one_feed_write_it_once(pool: PgPool) {
    let (podcast_id, feed) = seeded(&pool).await;

    // Holding the scope lock lines both ingests up behind it, so both start
    // before either has written anything.
    let held = pod_server::version::VersionScope::begin_podcast(&pool, podcast_id)
        .await
        .expect("the podcast row locks")
        .expect("the podcast exists");
    let spawn_ingest = || {
        let pool = pool.clone();
        let feed = feed.clone();
        tokio::spawn(async move { ingest_ok(&pool, podcast_id, &feed).await })
    };
    let (first, second) = (spawn_ingest(), spawn_ingest());
    common::wait_for_lock_waiters(&pool, 2).await;
    held.commit().await.expect("the held scope commits");

    let mut outcomes = [
        first.await.expect("the first ingest doesn't panic"),
        second.await.expect("the second ingest doesn't panic"),
    ];
    outcomes.sort_by_key(|outcome| matches!(outcome, IngestOutcome::Unchanged));
    assert!(matches!(outcomes[0], IngestOutcome::Applied(_)));
    assert_eq!(outcomes[1], IngestOutcome::Unchanged);

    let versions: Vec<i64> = episodes(&pool, podcast_id)
        .await
        .into_iter()
        .map(|(_, _, _, version)| version)
        .collect();
    assert_eq!(versions, [2, 3, 4]);
    let podcast = podcast_row(&pool, podcast_id).await;
    assert_eq!((podcast.version, podcast.version_counter), (1, 4));
}
