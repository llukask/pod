//! Runs `conformance/inbox.json` (spec section 9, "Inbox query on a fixed
//! dataset") against `GET /v1/inbox`. Clients run the same file against
//! their SQLite query; `conformance/README.md` describes the format.

mod auth_app;
mod read_seed;

use std::path::Path;

use pod_protocol::{EpisodeId, InboxPage, PodcastId, Timestamp, settings};
use read_seed::{
    insert_episode, insert_podcast, reader, set_episode_state, set_setting, set_subscription,
};
use serde::Deserialize;
use sqlx::PgPool;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    #[expect(dead_code, reason = "documentation for fixture readers")]
    description: String,
    podcasts: Vec<FixturePodcast>,
    subscriptions: Vec<FixtureSubscription>,
    episodes: Vec<FixtureEpisode>,
    episode_states: Vec<FixtureEpisodeState>,
    cases: Vec<Case>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FixturePodcast {
    id: PodcastId,
    title: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureSubscription {
    podcast_id: PodcastId,
    subscribed: bool,
    subscribed_at: Timestamp,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureEpisode {
    id: EpisodeId,
    podcast_id: PodcastId,
    published_at: Timestamp,
    deleted: bool,
    #[expect(dead_code, reason = "documentation for fixture readers")]
    title: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureEpisodeState {
    episode_id: EpisodeId,
    podcast_id: PodcastId,
    position_ms: i64,
    played: bool,
    archived: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    name: String,
    now: Timestamp,
    inbox_window_days: Option<i64>,
    inbox: Vec<EpisodeId>,
}

fn load_fixture() -> Fixture {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance/inbox.json");
    let text = std::fs::read_to_string(&path).expect("conformance/inbox.json is readable");
    serde_json::from_str(&text).expect("conformance/inbox.json matches the documented format")
}

/// Seeds the dataset once, then runs every case against it, changing only
/// the window setting and the clock between cases.
#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn every_inbox_case_passes(pool: PgPool) {
    let fixture = load_fixture();
    assert!(!fixture.cases.is_empty());
    let reader = reader(pool).await;
    let pool = reader.pool();

    for podcast in &fixture.podcasts {
        insert_podcast(pool, podcast.id, &podcast.title).await;
    }
    for subscription in &fixture.subscriptions {
        set_subscription(
            pool,
            reader.user_id,
            subscription.podcast_id,
            subscription.subscribed,
            subscription.subscribed_at,
        )
        .await;
    }
    for episode in &fixture.episodes {
        insert_episode(
            pool,
            episode.podcast_id,
            episode.id,
            episode.published_at,
            episode.deleted,
        )
        .await;
    }
    for state in &fixture.episode_states {
        set_episode_state(
            pool,
            reader.user_id,
            state.podcast_id,
            state.episode_id,
            state.position_ms,
            state.played,
            state.archived,
        )
        .await;
    }

    for case in &fixture.cases {
        sqlx::query!(
            "delete from user_setting where user_id = $1",
            reader.user_id.as_uuid()
        )
        .execute(pool)
        .await
        .expect("settings are deletable");
        if let Some(days) = case.inbox_window_days {
            set_setting(
                pool,
                reader.user_id,
                settings::INBOX_WINDOW_DAYS,
                days.into(),
                case.now,
            )
            .await;
        }
        reader.set_now(case.now);

        let page: InboxPage = reader.get_ok("/v1/inbox?limit=200").await;
        let ids: Vec<EpisodeId> = page.items.iter().map(|item| item.episode.0.id).collect();
        assert_eq!(ids, case.inbox, "{}", case.name);
        assert_eq!(page.next_cursor, None, "{}", case.name);
    }
}
