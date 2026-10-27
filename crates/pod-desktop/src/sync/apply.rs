//! Applying one `POST /v1/sync` response to the local store (spec 5.3), all
//! inside the caller's transaction.
//!
//! [`apply_response`] needs nothing but the transaction, the user the request
//! was made for and the response: the mutations the request carried are
//! still in the outbox (rows in flight can't be coalesced away), so it reads
//! them from there by the result ids. That keeps it drivable by the
//! conformance fixtures without HTTP.
//!
//! Every write is an idempotent upsert or delete by id, and the cursors move
//! in the same transaction as the rows, so a crash re-applies at most one
//! page and re-applying it changes nothing.

use color_eyre::eyre::{Result, WrapErr};
use pod_protocol::local::{LocalEpisodeState, LocalSetting, LocalSubscription, ScopeKey};
use pod_protocol::{
    DeviceId, Episode, EpisodeId, FeedStatus, Mutation, MutationId, MutationOutcome, Podcast,
    PodcastChanges, PodcastId, PodcastScope, RejectionCode, ScopeChanges, ScopeEpisode,
    SyncResponse, Timestamp, UserId,
};
use rusqlite::{OptionalExtension as _, Transaction, params};

use super::subscribe::{follow_replacement, forget_rejected_subscribe};

/// How many rejected mutations the local log keeps (spec 5.6).
const REJECTED_LOG_SIZE: i64 = 50;

/// What applying a response did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    Applied,
    /// The account row is gone or belongs to another user, so the response
    /// answers a session that no longer exists (logout or an account switch
    /// during the request, spec 5.7). Nothing was written.
    AccountChanged,
}

/// Applies `response`, which answers a request made for `user_id`.
pub fn apply_response(
    transaction: &Transaction<'_>,
    user_id: UserId,
    response: &SyncResponse,
) -> Result<Applied> {
    let account: Option<String> = transaction
        .query_row("select user_id from account where id = 1", [], |row| {
            row.get(0)
        })
        .optional()
        .context("read the account row")?;
    let same_user = account
        .map(|id| id.parse::<UserId>())
        .transpose()
        .context("parse the account's user id")?
        == Some(user_id);
    if !same_user {
        return Ok(Applied::AccountChanged);
    }

    apply_results(transaction, response).context("apply the mutation results")?;
    if let Some(scopes) = &response.scopes {
        merge_user_scope(transaction, scopes).context("merge the user scope")?;
        // After the merge, so Y's own row from this page is in place for the
        // carry, and before the reconcile, which then gives Y its scope.
        for remote in &scopes.user.subscriptions {
            if let Some(canonical) = remote.replaced_by {
                follow_replacement(transaction, remote.podcast_id, canonical).with_context(
                    || format!("follow the replacement of podcast {}", remote.podcast_id),
                )?;
            }
        }
        reconcile_scopes(transaction).context("reconcile the podcast scopes")?;
        for scope in &scopes.podcasts {
            apply_podcast_scope(transaction, scope)
                .with_context(|| format!("apply the scope of podcast {}", scope.id()))?;
        }
    }
    Ok(Applied::Applied)
}

// ==============================================================================
// Mutation results
// ==============================================================================

/// Deletes acknowledged outbox rows by `mutation_id`, never by key, so a
/// newer coalesced row written during the request survives. Copies a
/// clamped `at` into the field the mutation wrote while that field still
/// carries the sent stamp, for every status (spec 5.3, 4.4).
fn apply_results(transaction: &Transaction<'_>, response: &SyncResponse) -> Result<()> {
    for result in &response.results {
        let id = result.id.to_string();
        let payload: Option<String> = transaction
            .query_row(
                "select payload from outbox where mutation_id = ?1",
                [&id],
                |row| row.get(0),
            )
            .optional()
            .context("read the sent mutation")?;
        // A result for a row that is gone was applied before: the same page
        // again after a crash.
        let Some(payload) = payload else { continue };
        transaction
            .execute("delete from outbox where mutation_id = ?1", [&id])
            .context("delete the acknowledged outbox row")?;
        let sent: Mutation = serde_json::from_str(&payload).context("parse the sent mutation")?;

        let stored_at = match &result.outcome {
            MutationOutcome::Applied { at } | MutationOutcome::Superseded { at } => Some(*at),
            MutationOutcome::Rejected { at, code } => {
                let reason = serde_json::to_value(code).context("serialize the rejection code")?;
                let reason = reason.as_str().unwrap_or("unknown");
                // The code set is open (spec 4.3): an unknown code is logged
                // and dropped like any other.
                tracing::warn!(mutation_id = %result.id, reason, "the server rejected a mutation");
                // The log entry is also what the UI shows the "Couldn't
                // add" message from (`subscribe::load_failures`).
                if *code == RejectionCode::InvalidFeedUrl
                    && let Mutation::SubscriptionSet(subscribe) = &sent
                {
                    forget_rejected_subscribe(transaction, subscribe)
                        .context("forget the rejected subscribe")?;
                }
                log_rejection(transaction, result.id, &payload, reason)
                    .context("log the rejected mutation")?;
                *at
            }
        };
        if let Some(stored_at) = stored_at.filter(|at| *at != sent.at()) {
            rewrite_stamp(transaction, &sent, stored_at).context("rewrite the clamped stamp")?;
        }
    }
    Ok(())
}

/// Sets the `*_at` of the field `sent` wrote to the server's `stored_at`,
/// if the field still carries the stamp `sent` went out with. The stamp is
/// compared with the mutation's own `dev`, which a re-login doesn't change.
fn rewrite_stamp(
    transaction: &Transaction<'_>,
    sent: &Mutation,
    stored_at: Timestamp,
) -> Result<()> {
    let (table, key_column, key, at_column, dev_column) = match sent {
        Mutation::SubscriptionSet(m) => (
            "subscription",
            "podcast_id",
            m.podcast_id.to_string(),
            "subscribed_at",
            "subscribed_dev",
        ),
        Mutation::EpisodePosition(m) => (
            "episode_state",
            "episode_id",
            m.episode_id.to_string(),
            "position_at",
            "position_dev",
        ),
        Mutation::EpisodePlayed(m) => (
            "episode_state",
            "episode_id",
            m.episode_id.to_string(),
            "played_at",
            "played_dev",
        ),
        Mutation::EpisodeArchived(m) => (
            "episode_state",
            "episode_id",
            m.episode_id.to_string(),
            "archived_at",
            "archived_dev",
        ),
        Mutation::SettingSet(m) => ("setting", "key", m.key.clone(), "value_at", "value_dev"),
    };
    transaction
        .execute(
            &format!(
                "update {table} set {at_column} = ?1
                 where {key_column} = ?2 and {at_column} = ?3 and {dev_column} = ?4"
            ),
            params![
                stored_at.as_epoch_ms(),
                key,
                sent.at().as_epoch_ms(),
                sent.dev().to_string(),
            ],
        )
        .context("update the field's stamp")?;
    Ok(())
}

/// Records a mutation the server won't take in the local log Settings shows,
/// keeping the newest [`REJECTED_LOG_SIZE`].
pub fn log_rejection(
    transaction: &Transaction<'_>,
    mutation_id: MutationId,
    payload: &str,
    reason: &str,
) -> Result<()> {
    transaction
        .execute(
            "insert or replace into rejected_mutation (mutation_id, payload, reason, rejected_at)
             values (?1, ?2, ?3, ?4)",
            params![
                mutation_id.to_string(),
                payload,
                reason,
                Timestamp::now().as_epoch_ms()
            ],
        )
        .context("insert the log entry")?;
    transaction
        .execute(
            "delete from rejected_mutation where mutation_id not in (
                 select mutation_id from rejected_mutation
                 order by rejected_at desc, mutation_id desc limit ?1)",
            [REJECTED_LOG_SIZE],
        )
        .context("trim the log")?;
    Ok(())
}

// ==============================================================================
// User scope
// ==============================================================================

/// Merges the pulled user-state rows through `pod-protocol`'s `merge`
/// (spec 5.4) and moves the user cursor.
fn merge_user_scope(transaction: &Transaction<'_>, scopes: &ScopeChanges) -> Result<()> {
    let user = &scopes.user;
    for remote in &user.subscriptions {
        let row = match load_subscription(transaction, remote.podcast_id)? {
            Some(mut local) => {
                local.merge(remote);
                local
            }
            None => LocalSubscription::from(remote),
        };
        transaction
            .execute(
                "insert or replace into subscription
                     (podcast_id, feed_url, subscribed, subscribed_at, subscribed_dev, replaced_by)
                 values (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    row.podcast_id.to_string(),
                    row.feed_url,
                    row.subscribed,
                    row.subscribed_at.as_epoch_ms(),
                    row.subscribed_dev.to_string(),
                    row.replaced_by.map(|id| id.to_string()),
                ],
            )
            .context("write the merged subscription")?;
    }
    for remote in &user.episode_states {
        let row = match load_episode_state(transaction, remote.episode_id)? {
            Some(mut local) => {
                local.merge(remote);
                local
            }
            None => LocalEpisodeState::from(remote),
        };
        let ms = |at: Option<Timestamp>| at.map(Timestamp::as_epoch_ms);
        let text = |dev: Option<DeviceId>| dev.map(|dev| dev.to_string());
        transaction
            .execute(
                "insert or replace into episode_state
                     (episode_id, podcast_id, position_ms, position_at, position_dev,
                      played, played_at, played_dev, archived, archived_at, archived_dev)
                 values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    row.episode_id.to_string(),
                    row.podcast_id.to_string(),
                    row.position_ms,
                    ms(row.position_at),
                    text(row.position_dev),
                    row.played,
                    ms(row.played_at),
                    text(row.played_dev),
                    row.archived,
                    ms(row.archived_at),
                    text(row.archived_dev),
                ],
            )
            .context("write the merged episode state")?;
    }
    for remote in &user.settings {
        let row = match load_setting(transaction, &remote.key)? {
            Some(mut local) => {
                local.merge(remote);
                local
            }
            None => LocalSetting::from(remote),
        };
        transaction
            .execute(
                "insert or replace into setting (key, value, value_at, value_dev)
                 values (?1, ?2, ?3, ?4)",
                params![
                    row.key,
                    serde_json::to_string(&row.value).context("serialize the setting value")?,
                    row.value_at.as_epoch_ms(),
                    row.value_dev.to_string(),
                ],
            )
            .context("write the merged setting")?;
    }
    set_scope(transaction, ScopeKey::User, user.version, user.complete)
}

pub(super) fn load_subscription(
    transaction: &Transaction<'_>,
    podcast_id: PodcastId,
) -> Result<Option<LocalSubscription>> {
    let row = transaction
        .query_row(
            "select feed_url, subscribed, subscribed_at, subscribed_dev, replaced_by
             from subscription where podcast_id = ?1",
            [podcast_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, bool>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .optional()
        .context("read the local subscription")?;
    let Some((feed_url, subscribed, at, dev, replaced_by)) = row else {
        return Ok(None);
    };
    Ok(Some(LocalSubscription {
        podcast_id,
        feed_url,
        subscribed,
        subscribed_at: Timestamp::from_epoch_ms(at).context("read subscribed_at")?,
        subscribed_dev: dev.parse().context("parse subscribed_dev")?,
        replaced_by: replaced_by
            .map(|id| id.parse())
            .transpose()
            .context("parse replaced_by")?,
    }))
}

fn load_episode_state(
    transaction: &Transaction<'_>,
    episode_id: EpisodeId,
) -> Result<Option<LocalEpisodeState>> {
    type Field = (Option<i64>, Option<String>);
    let row = transaction
        .query_row(
            "select podcast_id, position_ms, position_at, position_dev,
                    played, played_at, played_dev, archived, archived_at, archived_dev
             from episode_state where episode_id = ?1",
            [episode_id.to_string()],
            |row| {
                let field =
                    |at, dev| -> rusqlite::Result<Field> { Ok((row.get(at)?, row.get(dev)?)) };
                Ok((
                    row.get::<_, String>(0)?,
                    (row.get::<_, i64>(1)?, field(2, 3)?),
                    (row.get::<_, bool>(4)?, field(5, 6)?),
                    (row.get::<_, bool>(7)?, field(8, 9)?),
                ))
            },
        )
        .optional()
        .context("read the local episode state")?;
    let Some((podcast_id, position, played, archived)) = row else {
        return Ok(None);
    };
    fn stamp((at, dev): Field) -> Result<(Option<Timestamp>, Option<DeviceId>)> {
        Ok((
            at.map(Timestamp::from_epoch_ms)
                .transpose()
                .context("read a field's stamp")?,
            dev.map(|dev| dev.parse())
                .transpose()
                .context("parse a field's device id")?,
        ))
    }
    let (position_at, position_dev) = stamp(position.1)?;
    let (played_at, played_dev) = stamp(played.1)?;
    let (archived_at, archived_dev) = stamp(archived.1)?;
    Ok(Some(LocalEpisodeState {
        episode_id,
        podcast_id: podcast_id.parse().context("parse the state's podcast id")?,
        position_ms: position.0,
        position_at,
        position_dev,
        played: played.0,
        played_at,
        played_dev,
        archived: archived.0,
        archived_at,
        archived_dev,
    }))
}

fn load_setting(transaction: &Transaction<'_>, key: &str) -> Result<Option<LocalSetting>> {
    let row = transaction
        .query_row(
            "select value, value_at, value_dev from setting where key = ?1",
            [key],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()
        .context("read the local setting")?;
    let Some((value, at, dev)) = row else {
        return Ok(None);
    };
    Ok(Some(LocalSetting {
        key: key.to_owned(),
        value: serde_json::from_str(&value).context("parse the setting value")?,
        value_at: Timestamp::from_epoch_ms(at).context("read value_at")?,
        value_dev: dev.parse().context("parse value_dev")?,
    }))
}

// ==============================================================================
// Podcast scopes
// ==============================================================================

/// Makes the scopes follow the merged subscription table (spec 5.3): every
/// subscribed podcast has a scope (new ones start at 0), no other podcast
/// does, and an unsubscribed podcast keeps no catalog rows. Its
/// `episode_state` rows stay, since they are user state.
///
/// Judging the merged table rather than the pulled rows means a pulled
/// unsubscribe that lost the merge to a newer local subscribe deletes
/// nothing.
fn reconcile_scopes(transaction: &Transaction<'_>) -> Result<()> {
    transaction
        .execute_batch(
            "insert into sync_scope (scope, version, complete)
                 select 'podcast:' || podcast_id, 0, 0 from subscription where subscribed = 1
                 on conflict (scope) do nothing;
             delete from sync_scope
                 where scope like 'podcast:%'
                 and substr(scope, 9) not in
                     (select podcast_id from subscription where subscribed = 1);
             delete from episode
                 where podcast_id in (select podcast_id from subscription where subscribed = 0);
             delete from podcast
                 where id in (select podcast_id from subscription where subscribed = 0);",
        )
        .context("align scopes and catalog with the subscriptions")?;
    Ok(())
}

fn apply_podcast_scope(transaction: &Transaction<'_>, scope: &PodcastScope) -> Result<()> {
    let key = ScopeKey::Podcast(scope.id()).to_string();
    let changes = match scope {
        PodcastScope::NotSubscribed(_) => {
            transaction
                .execute("delete from sync_scope where scope = ?1", [&key])
                .context("drop the scope")?;
            return Ok(());
        }
        PodcastScope::Ok(changes) => changes,
    };
    // The reconcile above removed the scope when the podcast's unsubscribe
    // is still in the outbox. The server, which hasn't seen it, still serves
    // the scope, and writing the page would bring back catalog rows the
    // scope rule forbids.
    let has_scope = transaction
        .query_row("select 1 from sync_scope where scope = ?1", [&key], |_| {
            Ok(())
        })
        .optional()
        .context("look up the scope")?
        .is_some();
    if !has_scope {
        return Ok(());
    }
    write_podcast_page(transaction, changes)?;
    set_scope(
        transaction,
        ScopeKey::Podcast(changes.id),
        changes.version,
        changes.complete,
    )
}

fn write_podcast_page(transaction: &Transaction<'_>, changes: &PodcastChanges) -> Result<()> {
    if let Some(podcast) = &changes.podcast {
        upsert_podcast(transaction, podcast).context("write the podcast row")?;
    }
    for episode in &changes.episodes {
        match episode {
            ScopeEpisode::Live(episode) => {
                upsert_episode(transaction, episode).context("write an episode")?;
            }
            ScopeEpisode::Deleted(tombstone) => {
                transaction
                    .execute(
                        "delete from episode where id = ?1",
                        [tombstone.id.to_string()],
                    )
                    .context("delete a deleted episode")?;
            }
        }
    }
    Ok(())
}

/// Writes the server's columns and keeps the placeholder's `local_*` ones.
/// The status lands in three columns: `status` holds the tag, and only a
/// failing podcast fills `failing_since` and `feed_error` (spec 5.5).
fn upsert_podcast(transaction: &Transaction<'_>, podcast: &Podcast) -> Result<()> {
    let (failing_since, feed_error) = match &podcast.status {
        FeedStatus::Failing {
            failing_since,
            error,
        } => (
            Some(failing_since.as_epoch_ms()),
            Some(serde_json::to_string(error).context("serialize the feed error")?),
        ),
        FeedStatus::Pending | FeedStatus::Ok | FeedStatus::Gone => (None, None),
    };
    transaction
        .execute(
            "insert into podcast
                 (id, feed_url, status, failing_since, feed_error, title, author, description,
                  link, artwork_url, language, version)
             values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             on conflict (id) do update set
                 feed_url = excluded.feed_url, status = excluded.status,
                 failing_since = excluded.failing_since, feed_error = excluded.feed_error,
                 title = excluded.title, author = excluded.author,
                 description = excluded.description, link = excluded.link,
                 artwork_url = excluded.artwork_url, language = excluded.language,
                 version = excluded.version",
            params![
                podcast.id.to_string(),
                podcast.feed_url,
                podcast.status.kind().as_str(),
                failing_since,
                feed_error,
                podcast.title,
                podcast.author,
                podcast.description,
                podcast.link,
                podcast.artwork_url,
                podcast.language,
                podcast.version,
            ],
        )
        .context("upsert the podcast")?;
    Ok(())
}

fn upsert_episode(transaction: &Transaction<'_>, episode: &Episode) -> Result<()> {
    transaction
        .execute(
            "insert or replace into episode
                 (id, podcast_id, title, summary_html, content_html, link, published_at,
                  duration_ms, audio_url, audio_type, audio_bytes, artwork_url, season, number,
                  version)
             values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                episode.id.to_string(),
                episode.podcast_id.to_string(),
                episode.title,
                episode.summary_html,
                episode.content_html,
                episode.link,
                episode.published_at.as_epoch_ms(),
                episode.duration_ms,
                episode.audio_url,
                episode.audio_type,
                episode.audio_bytes,
                episode.artwork_url,
                episode.season,
                episode.number,
                episode.version,
            ],
        )
        .context("upsert the episode")?;
    Ok(())
}

fn set_scope(
    transaction: &Transaction<'_>,
    scope: ScopeKey,
    version: i64,
    complete: bool,
) -> Result<()> {
    transaction
        .execute(
            "insert or replace into sync_scope (scope, version, complete) values (?1, ?2, ?3)",
            params![scope.to_string(), version, complete],
        )
        .context("move the scope cursor")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use crate::store::tests::scratch_dir;
    use serde_json::json;

    const USER: &str = "019a3f10-0000-7a41-9b3d-000000000001";
    const DEVICE: &str = "019a3f10-8c2e-7a41-9b3d-5e6f7a8b9c0d";
    const OTHER_DEVICE: &str = "019a3f10-8c2e-7a41-9b3d-ffffffffffff";
    const PODCAST: &str = "9c3e5b1a-2f4d-5e6c-8a7b-1d2e3f4a5b6c";
    const EPISODE: &str = "4b1e2c3d-4e5f-5a6b-9c7d-8e9f0a1b2c3d";
    const GONE_EPISODE: &str = "4b1e2c3d-4e5f-5a6b-9c7d-000000000000";

    /// A signed-in store holding `seed` (SQL).
    async fn store_with(seed: &str) -> (tempfile::TempDir, Store) {
        let dir = scratch_dir();
        let store = Store::open(&dir.path().join("pod.db")).expect("the store opens");
        let seed = seed.to_owned();
        store
            .write(move |transaction| {
                transaction
                    .execute(
                        "insert into account (id, server_url, user_id, username, device_id, token)
                         values (1, 'https://pod.example', ?1, 'alice', ?2, 'token')",
                        params![USER, DEVICE],
                    )
                    .context("insert the account")?;
                transaction.execute_batch(&seed).context("seed the store")
            })
            .await
            .expect("the seed commits");
        (dir, store)
    }

    async fn apply(store: &Store, user: &str, response: serde_json::Value) -> Applied {
        let user_id: UserId = user.parse().expect("literal is a UUID");
        let response: SyncResponse = serde_json::from_value(response).expect("a sync response");
        store
            .write(move |transaction| apply_response(transaction, user_id, &response))
            .await
            .expect("the response applies")
    }

    async fn rows(store: &Store, sql: &str) -> Vec<String> {
        let sql = sql.to_owned();
        store
            .read(move |connection| {
                let mut statement = connection.prepare(&sql).context("prepare the query")?;
                let rows = statement
                    .query_map([], |row| row.get(0))
                    .context("run the query")?
                    .collect::<rusqlite::Result<_>>()
                    .context("read the rows")?;
                Ok(rows)
            })
            .await
            .expect("the store reads")
    }

    fn subscription(subscribed: bool, at: &str, version: i64) -> serde_json::Value {
        json!({
            "podcast_id": PODCAST, "feed_url": "https://feeds.example.com/show.xml",
            "subscribed": subscribed, "subscribed_at": at, "subscribed_dev": OTHER_DEVICE,
            "replaced_by": null, "version": version
        })
    }

    fn page(
        user_version: i64,
        subscriptions: serde_json::Value,
        podcasts: serde_json::Value,
    ) -> serde_json::Value {
        json!({
            "server_time": "2026-10-24T09:00:00Z",
            "results": [],
            "scopes": {
                "user": {
                    "version": user_version, "complete": true,
                    "subscriptions": subscriptions, "episode_states": [], "settings": []
                },
                "podcasts": podcasts
            }
        })
    }

    fn podcast_page(version: i64, complete: bool) -> serde_json::Value {
        json!({
            "id": PODCAST, "status": "ok", "version": version, "complete": complete,
            "podcast": {
                "id": PODCAST, "feed_url": "https://feeds.example.com/show.xml",
                "status": "ok", "title": "The Example Show",
                "author": null, "description": null, "link": null, "artwork_url": null,
                "language": "en", "version": version - 1
            },
            "episodes": [
                {
                    "id": EPISODE, "podcast_id": PODCAST, "title": "Pilot",
                    "summary_html": "", "content_html": "", "link": null,
                    "published_at": "2026-10-20T05:00:00Z", "duration_ms": null,
                    "audio_url": "https://media.example.com/pilot.mp3",
                    "audio_type": "audio/mpeg", "audio_bytes": null, "artwork_url": null,
                    "season": null, "number": 1, "deleted": false, "version": version - 2
                },
                { "id": GONE_EPISODE, "deleted": true, "version": version }
            ]
        })
    }

    #[tokio::test]
    async fn a_response_for_another_user_changes_nothing() {
        let (_dir, store) = store_with("").await;
        let other_user = "019a3f10-0000-7a41-9b3d-000000000002";
        let response = page(
            5,
            json!([subscription(true, "2026-10-24T08:00:00Z", 5)]),
            json!([]),
        );
        assert_eq!(
            apply(&store, other_user, response).await,
            Applied::AccountChanged
        );
        assert_eq!(
            rows(&store, "select podcast_id from subscription").await,
            Vec::<String>::new()
        );
        assert_eq!(
            rows(&store, "select scope from sync_scope").await,
            Vec::<String>::new()
        );
    }

    #[tokio::test]
    async fn a_response_arriving_after_logout_changes_nothing() {
        let (_dir, store) = store_with("").await;
        store
            .write(|transaction| {
                transaction
                    .execute("delete from account", [])
                    .context("log out")
            })
            .await
            .expect("the logout commits");
        let response = page(
            5,
            json!([subscription(true, "2026-10-24T08:00:00Z", 5)]),
            json!([podcast_page(3, true)]),
        );
        assert_eq!(apply(&store, USER, response).await, Applied::AccountChanged);
        for query in [
            "select podcast_id from subscription",
            "select scope from sync_scope",
            "select id from podcast",
            "select id from episode",
        ] {
            assert_eq!(rows(&store, query).await, Vec::<String>::new(), "{query}");
        }
    }

    #[tokio::test]
    async fn a_new_subscription_gets_a_scope_and_its_pages_land() {
        let (_dir, store) = store_with(&format!(
            "insert into episode (id, podcast_id, title, summary_html, content_html, published_at,
                                  audio_url, audio_type, version)
             values ('{GONE_EPISODE}', '{PODCAST}', 'Gone', '', '', 0, 'u', 'audio/mpeg', 1);"
        ))
        .await;
        let response = page(
            5,
            json!([subscription(true, "2026-10-24T08:00:00Z", 5)]),
            json!([]),
        );
        assert_eq!(apply(&store, USER, response).await, Applied::Applied);
        assert_eq!(
            rows(
                &store,
                "select scope || ' ' || version || ' ' || complete from sync_scope order by scope"
            )
            .await,
            [format!("podcast:{PODCAST} 0 0"), "user 5 1".to_owned()]
        );

        // Applying the same page twice, as after a crash, ends the same.
        for _ in 0..2 {
            let response = page(5, json!([]), json!([podcast_page(12, true)]));
            apply(&store, USER, response).await;
        }
        assert_eq!(
            rows(&store, "select title from podcast").await,
            ["The Example Show"]
        );
        assert_eq!(rows(&store, "select id from episode").await, [EPISODE]);
        assert_eq!(
            rows(&store, &format!("select version || ' ' || complete from sync_scope where scope = 'podcast:{PODCAST}'")).await,
            ["12 1"]
        );
    }

    #[tokio::test]
    async fn an_unsubscribe_drops_the_catalog_and_keeps_the_listening_state() {
        let (_dir, store) = store_with(&format!(
            "insert into subscription values
                 ('{PODCAST}', 'https://feeds.example.com/show.xml', 1, 1000, '{OTHER_DEVICE}', null);
             insert into sync_scope values ('podcast:{PODCAST}', 12, 1);
             insert into podcast (id, feed_url, status, version)
                 values ('{PODCAST}', 'https://feeds.example.com/show.xml', 'ok', 11);
             insert into episode (id, podcast_id, title, summary_html, content_html, published_at,
                                  audio_url, audio_type, version)
                 values ('{EPISODE}', '{PODCAST}', 'Pilot', '', '', 0, 'u', 'audio/mpeg', 10);
             insert into episode_state (episode_id, podcast_id, position_ms, position_at, position_dev)
                 values ('{EPISODE}', '{PODCAST}', 5000, 1000, '{DEVICE}');"
        ))
        .await;
        let response = page(
            6,
            json!([subscription(false, "2026-10-24T08:00:00Z", 6)]),
            json!([]),
        );
        apply(&store, USER, response).await;
        assert_eq!(rows(&store, "select scope from sync_scope").await, ["user"]);
        assert_eq!(
            rows(&store, "select id from podcast").await,
            Vec::<String>::new()
        );
        assert_eq!(
            rows(&store, "select id from episode").await,
            Vec::<String>::new()
        );
        assert_eq!(
            rows(&store, "select episode_id from episode_state").await,
            [EPISODE]
        );
    }

    #[tokio::test]
    async fn a_page_for_a_podcast_unsubscribed_locally_is_skipped() {
        // The unsubscribe is still in the outbox, so the server serves the
        // scope; the local row, stamped later, wins the merge.
        let (_dir, store) = store_with(&format!(
            "insert into subscription values
                 ('{PODCAST}', 'https://feeds.example.com/show.xml', 0, 1792832400000, '{DEVICE}', null);"
        ))
        .await;
        let response = page(
            5,
            json!([subscription(true, "2026-10-24T08:00:00Z", 5)]),
            json!([podcast_page(12, true)]),
        );
        apply(&store, USER, response).await;
        assert_eq!(rows(&store, "select scope from sync_scope").await, ["user"]);
        assert_eq!(
            rows(&store, "select id from podcast").await,
            Vec::<String>::new()
        );
        assert_eq!(
            rows(&store, "select id from episode").await,
            Vec::<String>::new()
        );
        assert_eq!(
            rows(&store, "select subscribed_dev from subscription").await,
            [DEVICE],
            "the newer local unsubscribe stands"
        );
    }

    // ==========================================================================
    // Offline subscribe (spec 5.5)
    // ==========================================================================

    /// The id the client derived from the URL it was given (X).
    const ALIAS: &str = "1a2b3c4d-5e6f-5a7b-8c9d-0e1f2a3b4c5d";
    const ALIAS_URL: &str = "https://old.example.com/show.xml";
    const SUBSCRIBED_AT: &str = "2026-10-24T08:00:00Z";
    const SUBSCRIBED_AT_MS: i64 = 1_792_828_800_000;
    const MUTATION: &str = "019a3f10-9999-7a41-9b3d-000000000001";

    /// A subscribe to [`ALIAS_URL`] made offline: the placeholder, the
    /// subscription, the scope at 0, the queued mutation, and podcast
    /// settings the user picked for the show meanwhile.
    fn offline_subscribe() -> String {
        let payload = json!({
            "type": "subscription.set", "id": MUTATION, "at": SUBSCRIBED_AT, "dev": DEVICE,
            "podcast_id": ALIAS, "feed_url": ALIAS_URL, "subscribed": true
        });
        format!(
            "insert into podcast (id, feed_url, status, version, local_title)
                 values ('{ALIAS}', '{ALIAS_URL}', 'pending', 0, 'The Example Show');
             insert into subscription values
                 ('{ALIAS}', '{ALIAS_URL}', 1, {SUBSCRIBED_AT_MS}, '{DEVICE}', null);
             insert into sync_scope values ('podcast:{ALIAS}', 0, 0);
             insert into outbox (mutation_id, coalesce_key, payload, created_at)
                 values ('{MUTATION}', 'sub:{ALIAS}', '{payload}', {SUBSCRIBED_AT_MS});
             insert into podcast_device (podcast_id, auto_keep) values ('{ALIAS}', 3);"
        )
    }

    /// The server's row for X after an alias redirect (spec 4.4): epoch
    /// stamp, nil device, `replaced_by` the canonical podcast.
    fn replaced_alias(version: i64) -> serde_json::Value {
        json!({
            "podcast_id": ALIAS, "feed_url": ALIAS_URL, "subscribed": false,
            "subscribed_at": "1970-01-01T00:00:00Z",
            "subscribed_dev": "00000000-0000-0000-0000-000000000000",
            "replaced_by": PODCAST, "version": version
        })
    }

    #[tokio::test]
    async fn a_replaced_placeholder_moves_to_the_canonical_podcast() {
        let (_dir, store) = store_with(&offline_subscribe()).await;
        // The subscribe is still queued here: the server learned of X from
        // another device. Applying the page twice, as after a crash, ends
        // the same.
        for _ in 0..2 {
            apply(&store, USER, page(5, json!([replaced_alias(5)]), json!([]))).await;
        }

        assert_eq!(
            rows(
                &store,
                "select podcast_id || ' ' || feed_url || ' ' || subscribed || ' '
                        || subscribed_at || ' ' || subscribed_dev || ' '
                        || coalesce(replaced_by, '-') from subscription"
            )
            .await,
            [format!(
                "{PODCAST} {ALIAS_URL} 1 {SUBSCRIBED_AT_MS} {DEVICE} -"
            )],
            "the subscribe carries over to the canonical podcast"
        );
        let outbox = rows(&store, "select coalesce_key || ' ' || payload from outbox").await;
        assert_eq!(outbox.len(), 1);
        let (key, payload) = outbox[0].split_once(' ').expect("key and payload");
        assert_eq!(key, format!("sub:{PODCAST}"));
        let queued: Mutation = serde_json::from_str(payload).expect("the payload is a mutation");
        let Mutation::SubscriptionSet(queued) = queued else {
            panic!("the queued mutation is still a subscribe: {queued:?}");
        };
        assert_eq!(queued.podcast_id.to_string(), PODCAST);
        assert_eq!(queued.feed_url, ALIAS_URL);
        assert_eq!(queued.id.to_string(), MUTATION);
        assert_eq!(
            rows(
                &store,
                "select podcast_id || ' ' || auto_keep from podcast_device"
            )
            .await,
            [format!("{PODCAST} 3")],
            "the podcast settings carry over"
        );
        assert_eq!(
            rows(&store, "select scope from sync_scope order by scope").await,
            [format!("podcast:{PODCAST}"), "user".to_owned()]
        );
        assert_eq!(
            rows(
                &store,
                "select id || ' ' || coalesce(local_title, '-') from podcast"
            )
            .await,
            [format!("{PODCAST} The Example Show")],
            "the placeholder now stands for the canonical podcast"
        );
    }

    #[tokio::test]
    async fn a_replacement_keeps_what_the_canonical_podcast_already_has() {
        let (_dir, store) = store_with(&format!(
            "{}
             insert into podcast (id, feed_url, status, title, version)
                 values ('{PODCAST}', 'https://feeds.example.com/show.xml', 'ok',
                         'The Example Show', 11);
             insert into podcast_device (podcast_id, auto_keep) values ('{PODCAST}', 1);",
            offline_subscribe()
        ))
        .await;
        // Y's row ties X's stamp exactly, which means the server applied a
        // later mutation for Y from this device: Y keeps its fields.
        let canonical = json!({
            "podcast_id": PODCAST, "feed_url": "https://feeds.example.com/show.xml",
            "subscribed": true, "subscribed_at": SUBSCRIBED_AT, "subscribed_dev": DEVICE,
            "replaced_by": null, "version": 6
        });
        apply(
            &store,
            USER,
            page(6, json!([replaced_alias(5), canonical]), json!([])),
        )
        .await;

        assert_eq!(
            rows(
                &store,
                "select podcast_id || ' ' || feed_url from subscription"
            )
            .await,
            [format!("{PODCAST} https://feeds.example.com/show.xml")]
        );
        assert_eq!(
            rows(
                &store,
                "select podcast_id || ' ' || auto_keep from podcast_device"
            )
            .await,
            [format!("{PODCAST} 1")],
            "the canonical podcast's own settings stand"
        );
        assert_eq!(
            rows(
                &store,
                "select id || ' ' || coalesce(title, '-') from podcast"
            )
            .await,
            [format!("{PODCAST} The Example Show")]
        );
    }

    #[tokio::test]
    async fn a_rejected_subscribe_drops_the_placeholder_and_explains_why() {
        let (_dir, store) = store_with(&offline_subscribe()).await;
        let response = json!({
            "server_time": "2026-10-24T09:00:00Z",
            "results": [
                { "id": MUTATION, "status": "rejected", "code": "invalid_feed_url",
                  "at": SUBSCRIBED_AT }
            ]
        });
        apply(&store, USER, response).await;

        for query in [
            "select id from podcast",
            "select podcast_id from subscription",
            "select scope from sync_scope",
            "select mutation_id from outbox",
        ] {
            assert_eq!(rows(&store, query).await, Vec::<String>::new(), "{query}");
        }
        let failures = store
            .read(super::super::subscribe::load_failures)
            .await
            .expect("the log reads");
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].mutation_id.to_string(), MUTATION);
        assert_eq!(
            failures[0].message,
            format!("Couldn't add {ALIAS_URL}: the server won't fetch from this address")
        );
    }
}
