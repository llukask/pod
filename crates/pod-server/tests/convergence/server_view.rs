//! The server's rows for one user, read straight from the tables, and the
//! convergence property: a settled client holds exactly those rows.
//!
//! The reads are plain SQL on purpose. Using `pull` as the oracle would let
//! a bug in its row mapping cancel out.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;

use pod_protocol::local::{LocalEpisodeState, LocalSetting, LocalSubscription};
use pod_protocol::{DeviceId, Episode, EpisodeId, Podcast, PodcastId, PodcastStatus, UserId};
use pod_server::db::SqlTimestamp;
use sqlx::types::Uuid;
use sqlx::{PgPool, Row};

use super::client::Client;

/// One podcast scope as the server holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerPodcast {
    pub version_counter: i64,
    /// `None` while the row is at version 0: no client ever receives it.
    pub podcast: Option<Podcast>,
    /// Live episodes only; deleted ones must be absent on the client.
    pub episodes: BTreeMap<EpisodeId, Episode>,
}

/// Everything a client of one user should end up with.
#[derive(Debug, Clone)]
pub struct ServerView {
    pub user_version_counter: i64,
    /// Rows without `replaced_by`. A client deletes a replaced row once it
    /// acted on it (spec 5.5 step 4), so those must be absent.
    pub subscriptions: BTreeMap<PodcastId, LocalSubscription>,
    pub episode_states: BTreeMap<EpisodeId, LocalEpisodeState>,
    pub settings: BTreeMap<String, LocalSetting>,
    pub podcasts: BTreeMap<PodcastId, ServerPodcast>,
}

impl ServerView {
    pub async fn read(pool: &PgPool, user_id: UserId) -> Self {
        let user = user_id.as_uuid();
        let user_version_counter: i64 =
            sqlx::query_scalar("select version_counter from users where id = $1")
                .bind(user)
                .fetch_one(pool)
                .await
                .expect("the user exists");

        let mut subscriptions = BTreeMap::new();
        for row in sqlx::query(
            "select podcast_id, feed_url, subscribed, subscribed_at, subscribed_dev, replaced_by
             from subscription where user_id = $1 and replaced_by is null",
        )
        .bind(user)
        .fetch_all(pool)
        .await
        .expect("subscription is readable")
        {
            let podcast_id = PodcastId::from_uuid(row.get("podcast_id"));
            subscriptions.insert(
                podcast_id,
                LocalSubscription {
                    podcast_id,
                    feed_url: row.get("feed_url"),
                    subscribed: row.get("subscribed"),
                    subscribed_at: row.get::<SqlTimestamp, _>("subscribed_at").0,
                    subscribed_dev: DeviceId::from_uuid(row.get("subscribed_dev")),
                    replaced_by: row
                        .get::<Option<Uuid>, _>("replaced_by")
                        .map(PodcastId::from_uuid),
                },
            );
        }

        let mut episode_states = BTreeMap::new();
        for row in sqlx::query(
            "select episode_id, podcast_id, position_ms, position_at, position_dev,
                    played, played_at, played_dev, archived, archived_at, archived_dev
             from episode_state where user_id = $1",
        )
        .bind(user)
        .fetch_all(pool)
        .await
        .expect("episode_state is readable")
        {
            let at = |column: &str| row.get::<Option<SqlTimestamp>, _>(column).map(|at| at.0);
            let dev = |column: &str| row.get::<Option<Uuid>, _>(column).map(DeviceId::from_uuid);
            let episode_id = EpisodeId::from_uuid(row.get("episode_id"));
            episode_states.insert(
                episode_id,
                LocalEpisodeState {
                    episode_id,
                    podcast_id: PodcastId::from_uuid(row.get("podcast_id")),
                    position_ms: row.get("position_ms"),
                    position_at: at("position_at"),
                    position_dev: dev("position_dev"),
                    played: row.get("played"),
                    played_at: at("played_at"),
                    played_dev: dev("played_dev"),
                    archived: row.get("archived"),
                    archived_at: at("archived_at"),
                    archived_dev: dev("archived_dev"),
                },
            );
        }

        let mut settings = BTreeMap::new();
        for row in sqlx::query(
            "select key, value, value_at, value_dev from user_setting where user_id = $1",
        )
        .bind(user)
        .fetch_all(pool)
        .await
        .expect("user_setting is readable")
        {
            let key: String = row.get("key");
            settings.insert(
                key.clone(),
                LocalSetting {
                    key,
                    value: row.get("value"),
                    value_at: row.get::<SqlTimestamp, _>("value_at").0,
                    value_dev: DeviceId::from_uuid(row.get("value_dev")),
                },
            );
        }

        Self {
            user_version_counter,
            subscriptions,
            episode_states,
            settings,
            podcasts: read_catalog(pool).await,
        }
    }
}

/// Every podcast scope in the catalog, which all users share.
async fn read_catalog(pool: &PgPool) -> BTreeMap<PodcastId, ServerPodcast> {
    let mut podcasts = BTreeMap::new();
    for row in sqlx::query(
        "select id, feed_url, status, status_message, title, author, description, link,
                artwork_url, language, version, version_counter
         from podcast",
    )
    .fetch_all(pool)
    .await
    .expect("podcast is readable")
    {
        let id = PodcastId::from_uuid(row.get("id"));
        let version: i64 = row.get("version");
        let status: String = row.get("status");
        let podcast = (version > 0).then(|| Podcast {
            id,
            feed_url: row.get("feed_url"),
            status: serde_json::from_value::<PodcastStatus>(serde_json::Value::String(status))
                .expect("the status column holds wire values"),
            status_message: row.get("status_message"),
            title: row.get("title"),
            author: row.get("author"),
            description: row.get("description"),
            link: row.get("link"),
            artwork_url: row.get("artwork_url"),
            language: row.get("language"),
            version,
        });
        podcasts.insert(
            id,
            ServerPodcast {
                version_counter: row.get("version_counter"),
                podcast,
                episodes: BTreeMap::new(),
            },
        );
    }
    for row in sqlx::query(
        "select id, podcast_id, title, summary_html, content_html, link, published_at,
                duration_ms, audio_url, audio_type, audio_bytes, artwork_url, season, number,
                version
         from episode where not deleted",
    )
    .fetch_all(pool)
    .await
    .expect("episode is readable")
    {
        let episode = Episode {
            id: EpisodeId::from_uuid(row.get("id")),
            podcast_id: PodcastId::from_uuid(row.get("podcast_id")),
            title: row.get("title"),
            summary_html: row.get("summary_html"),
            content_html: row.get("content_html"),
            link: row.get("link"),
            published_at: row.get::<SqlTimestamp, _>("published_at").0,
            duration_ms: row.get("duration_ms"),
            audio_url: row.get("audio_url"),
            audio_type: row.get("audio_type"),
            audio_bytes: row.get("audio_bytes"),
            artwork_url: row.get("artwork_url"),
            season: row.get("season"),
            number: row.get("number"),
            version: row.get("version"),
        };
        podcasts
            .get_mut(&episode.podcast_id)
            .expect("episodes reference their podcast")
            .episodes
            .insert(episode.id, episode);
    }
    podcasts
}

// ==============================================================================
// The property
// ==============================================================================

/// Every way `client` differs from `server`, one line each. Empty means the
/// client converged.
///
/// User scope: the same subscription, episode state and setting rows, every
/// stamp included, and the cursor at the user's counter. Catalog: for each
/// podcast the client is subscribed to, a scope at the podcast's counter, the
/// podcast row once it has a version, and exactly the live episodes; nothing
/// for any other podcast.
pub fn differences(client: &Client, server: &ServerView) -> Vec<String> {
    let mut problems = Vec::new();
    let name = &client.name;

    if client.user_cursor != server.user_version_counter {
        problems.push(format!(
            "{name}: user cursor {} but the user's counter is {}",
            client.user_cursor, server.user_version_counter
        ));
    }
    compare_maps(
        name,
        "subscription",
        &client.subscriptions,
        &server.subscriptions,
        &mut problems,
    );
    compare_maps(
        name,
        "episode_state",
        &client.episode_states,
        &server.episode_states,
        &mut problems,
    );
    compare_maps(
        name,
        "setting",
        &client.settings,
        &server.settings,
        &mut problems,
    );

    let subscribed: BTreeSet<PodcastId> = client
        .subscriptions
        .values()
        .filter(|row| row.subscribed)
        .map(|row| row.podcast_id)
        .collect();
    let scoped: BTreeSet<PodcastId> = client.podcast_scopes.keys().copied().collect();
    if scoped != subscribed {
        problems.push(format!(
            "{name}: scopes {scoped:?} don't match subscribed podcasts {subscribed:?}"
        ));
    }

    for id in &subscribed {
        let Some(expected) = server.podcasts.get(id) else {
            problems.push(format!(
                "{name}: subscribed to {id}, which has no podcast row"
            ));
            continue;
        };
        if let Some(scope) = client.podcast_scopes.get(id)
            && scope.version != expected.version_counter
        {
            problems.push(format!(
                "{name}: podcast {id} cursor {} but its counter is {}",
                scope.version, expected.version_counter
            ));
        }
        if client.podcasts.get(id) != expected.podcast.as_ref() {
            problems.push(format!(
                "{name}: podcast {id}\n    client {:?}\n    server {:?}",
                client.podcasts.get(id),
                expected.podcast
            ));
        }
        let episodes: BTreeMap<EpisodeId, Episode> = client
            .episodes
            .values()
            .filter(|episode| episode.podcast_id == *id)
            .map(|episode| (episode.id, episode.clone()))
            .collect();
        compare_maps(
            name,
            &format!("episode of {id}"),
            &episodes,
            &expected.episodes,
            &mut problems,
        );
    }
    for podcast in client.podcasts.keys().filter(|id| !subscribed.contains(id)) {
        problems.push(format!("{name}: podcast row for unsubscribed {podcast}"));
    }
    for episode in client
        .episodes
        .values()
        .filter(|episode| !subscribed.contains(&episode.podcast_id))
    {
        problems.push(format!(
            "{name}: episode {} of unsubscribed {}",
            episode.id, episode.podcast_id
        ));
    }
    problems
}

fn compare_maps<K: Ord + Debug, V: PartialEq + Debug>(
    client_name: &str,
    table: &str,
    client: &BTreeMap<K, V>,
    server: &BTreeMap<K, V>,
    problems: &mut Vec<String>,
) {
    let keys: BTreeSet<&K> = client.keys().chain(server.keys()).collect();
    for key in keys {
        let (ours, theirs) = (client.get(key), server.get(key));
        if ours != theirs {
            problems.push(format!(
                "{client_name}: {table} {key:?}\n    client {ours:?}\n    server {theirs:?}"
            ));
        }
    }
}
