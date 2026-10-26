//! Local writes (spec 5.2): every user action is one local transaction that
//! overwrites the local row, queues a coalesced outbox mutation and asks the
//! sync engine to run.
//!
//! Local writes never go through `merge` (spec 5.4). They stamp the field
//! with `at = max(now, previous at + 1 ms)`, so the user's latest action
//! always wins locally: two actions in the same millisecond get increasing
//! stamps, and so does an action made after a pull brought in a value
//! stamped by a device whose clock runs fast.
//!
//! The device id is the signed-in session's, from the `account` row. Without
//! an account there is no user state to write (logout clears it, spec 5.7),
//! so a write fails. A `401` keeps the account row, so writes keep working
//! while signed out.

use std::collections::HashSet;
use std::time::Duration;

use color_eyre::eyre::{OptionExt as _, Result, WrapErr, ensure};
use pod_protocol::{
    DeviceId, EpisodeArchived, EpisodeId, EpisodePlayed, EpisodePosition, Mutation, MutationId,
    PodcastId, SettingSet, SubscriptionSet, Timestamp,
};
use rusqlite::{OptionalExtension as _, Transaction, params};

use super::SyncHandle;
use crate::store::Store;

/// How long a position write during playback may wait for a sync. Playing
/// saves the position every few seconds, and those shouldn't each cause a
/// request.
pub const PLAYBACK_SYNC_DELAY: Duration = Duration::from_secs(30);

/// How long every other write waits, so a burst of actions shares a request.
pub const WRITE_SYNC_DELAY: Duration = Duration::from_secs(2);

// ==============================================================================
// User actions
// ==============================================================================

/// A user action on synced state, without the stamp the write adds.
#[derive(Debug, Clone, PartialEq)]
pub enum LocalChange {
    /// One field of an episode's state. `podcast_id` fills a new
    /// `episode_state` row; an existing row keeps its own.
    Episode {
        episode_id: EpisodeId,
        podcast_id: PodcastId,
        change: EpisodeChange,
    },
    /// Subscribe or unsubscribe. Only the `subscription` row and its
    /// mutation; [`super::subscribe::subscribe`] adds the placeholder and
    /// the scope (spec 5.5).
    Subscription {
        podcast_id: PodcastId,
        feed_url: String,
        subscribed: bool,
    },
    /// A synced setting.
    Setting {
        key: String,
        value: serde_json::Value,
    },
}

/// The `episode_state` field an action writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EpisodeChange {
    /// `while_playing` marks the periodic saves during playback, which may
    /// wait for a sync. Pause, stop and seek are not `while_playing`.
    Position {
        position_ms: i64,
        while_playing: bool,
    },
    Played(bool),
    Archived(bool),
}

impl EpisodeChange {
    /// The `(value, *_at, *_dev)` columns of the field. All three values
    /// are SQLite integers.
    fn columns(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Self::Position { .. } => ("position_ms", "position_at", "position_dev"),
            Self::Played(_) => ("played", "played_at", "played_dev"),
            Self::Archived(_) => ("archived", "archived_at", "archived_dev"),
        }
    }

    fn value(self) -> i64 {
        match self {
            Self::Position { position_ms, .. } => position_ms,
            Self::Played(flag) | Self::Archived(flag) => i64::from(flag),
        }
    }
}

impl LocalChange {
    /// How soon the sync engine should send this change.
    pub fn sync_delay(&self) -> Duration {
        match self {
            Self::Episode {
                change:
                    EpisodeChange::Position {
                        while_playing: true,
                        ..
                    },
                ..
            } => PLAYBACK_SYNC_DELAY,
            _ => WRITE_SYNC_DELAY,
        }
    }

    fn into_mutation(self, id: MutationId, at: Timestamp, dev: DeviceId) -> Mutation {
        match self {
            Self::Episode {
                episode_id, change, ..
            } => match change {
                EpisodeChange::Position { position_ms, .. } => {
                    Mutation::EpisodePosition(EpisodePosition {
                        id,
                        at,
                        dev,
                        episode_id,
                        position_ms,
                    })
                }
                EpisodeChange::Played(played) => Mutation::EpisodePlayed(EpisodePlayed {
                    id,
                    at,
                    dev,
                    episode_id,
                    played,
                }),
                EpisodeChange::Archived(archived) => Mutation::EpisodeArchived(EpisodeArchived {
                    id,
                    at,
                    dev,
                    episode_id,
                    archived,
                }),
            },
            Self::Subscription {
                podcast_id,
                feed_url,
                subscribed,
            } => Mutation::SubscriptionSet(SubscriptionSet {
                id,
                at,
                dev,
                podcast_id,
                feed_url,
                subscribed,
            }),
            Self::Setting { key, value } => Mutation::SettingSet(SettingSet {
                id,
                at,
                dev,
                key,
                value,
            }),
        }
    }
}

// ==============================================================================
// Stamps and coalescing
// ==============================================================================

/// The stamp for a local write made at `now` to a field last stamped
/// `previous`: `max(now, previous + 1 ms)`.
pub fn local_stamp_at(now: Timestamp, previous: Option<Timestamp>) -> Timestamp {
    // A stamp at the end of the representable range can't move on; that
    // write ties the field instead, which the device id breaks.
    let after_previous =
        previous.map(|at| Timestamp::from_epoch_ms(at.as_epoch_ms() + 1).unwrap_or(at));
    after_previous.map_or(now, |after| after.max(now))
}

/// The outbox `coalesce_key` of a mutation (spec 5.2). Queued mutations with
/// the same key replace each other.
pub fn coalesce_key(mutation: &Mutation) -> String {
    match mutation {
        Mutation::EpisodePosition(m) => format!("position:{}", m.episode_id),
        Mutation::EpisodePlayed(m) => format!("played:{}", m.episode_id),
        Mutation::EpisodeArchived(m) => format!("archived:{}", m.episode_id),
        Mutation::SubscriptionSet(m) => format!("sub:{}", m.podcast_id),
        Mutation::SettingSet(m) => format!("setting:{}", m.key),
    }
}

// ==============================================================================
// The write transaction
// ==============================================================================

/// Makes `change` locally in one transaction and asks the sync engine to
/// send it. Returns the queued mutation.
pub async fn write(store: &Store, sync: &SyncHandle, change: LocalChange) -> Result<Mutation> {
    let mut mutations = write_all(store, sync, move |_| Ok(vec![change])).await?;
    Ok(mutations.pop().expect("one change queues one mutation"))
}

/// Makes every change `changes` returns in one transaction, like [`write`]
/// does one, and asks for a sync as soon as the most urgent of them wants
/// one. `changes` runs inside the transaction, so the batch is chosen from
/// the state it applies to.
pub async fn write_all<F>(store: &Store, sync: &SyncHandle, changes: F) -> Result<Vec<Mutation>>
where
    F: FnOnce(&Transaction<'_>) -> Result<Vec<LocalChange>> + Send + 'static,
{
    let handle = sync.clone();
    let (mutations, delay) = store
        .write(move |transaction| {
            let changes = changes(transaction).context("choose the changes")?;
            let delay = changes.iter().map(LocalChange::sync_delay).min();
            // Held until the transaction ends, so the in-flight set can't
            // change between the coalescing check and the commit.
            let in_flight = handle.lock_in_flight();
            let now = Timestamp::now();
            let mutations = changes
                .into_iter()
                .map(|change| apply(transaction, change, MutationId::new_v7(), &in_flight, now))
                .collect::<Result<Vec<_>>>()
                .context("apply the changes")?;
            Ok((mutations, delay))
        })
        .await
        .context("write the changes locally")?;
    if let Some(delay) = delay {
        sync.request_sync(delay);
    }
    Ok(mutations)
}

/// The body of [`write`]'s transaction, with the mutation id, the clock and
/// the in-flight set passed in. [`super::subscribe::subscribe`] runs it inside its own
/// transaction.
pub(super) fn apply(
    transaction: &Transaction<'_>,
    change: LocalChange,
    id: MutationId,
    in_flight: &HashSet<MutationId>,
    now: Timestamp,
) -> Result<Mutation> {
    let dev = device_id(transaction).context("read this device's id")?;
    let at = match &change {
        LocalChange::Episode {
            episode_id,
            podcast_id,
            change,
        } => {
            ensure!(
                change.value() >= 0,
                "a position can't be negative, got {change:?}"
            );
            let (value_column, at_column, dev_column) = change.columns();
            let previous = previous_at(
                transaction,
                &format!("select {at_column} from episode_state where episode_id = ?1"),
                &episode_id.to_string(),
            )
            .context("read the episode state's stamp")?;
            let at = local_stamp_at(now, previous);
            transaction
                .execute(
                    &format!(
                        "insert into episode_state
                             (episode_id, podcast_id, {value_column}, {at_column}, {dev_column})
                         values (?1, ?2, ?3, ?4, ?5)
                         on conflict (episode_id) do update set
                             {value_column} = excluded.{value_column},
                             {at_column} = excluded.{at_column},
                             {dev_column} = excluded.{dev_column}"
                    ),
                    params![
                        episode_id.to_string(),
                        podcast_id.to_string(),
                        change.value(),
                        at.as_epoch_ms(),
                        dev.to_string(),
                    ],
                )
                .context("write the episode state")?;
            at
        }
        LocalChange::Subscription {
            podcast_id,
            feed_url,
            subscribed,
        } => {
            let previous = previous_at(
                transaction,
                "select subscribed_at from subscription where podcast_id = ?1",
                &podcast_id.to_string(),
            )
            .context("read the subscription's stamp")?;
            let at = local_stamp_at(now, previous);
            // `replaced_by` is the server's to set (spec 5.5), so a local
            // write leaves it alone.
            transaction
                .execute(
                    "insert into subscription
                         (podcast_id, feed_url, subscribed, subscribed_at, subscribed_dev)
                     values (?1, ?2, ?3, ?4, ?5)
                     on conflict (podcast_id) do update set
                         feed_url = excluded.feed_url,
                         subscribed = excluded.subscribed,
                         subscribed_at = excluded.subscribed_at,
                         subscribed_dev = excluded.subscribed_dev",
                    params![
                        podcast_id.to_string(),
                        feed_url,
                        subscribed,
                        at.as_epoch_ms(),
                        dev.to_string(),
                    ],
                )
                .context("write the subscription")?;
            at
        }
        LocalChange::Setting { key, value } => {
            let previous = previous_at(
                transaction,
                "select value_at from setting where key = ?1",
                key,
            )
            .context("read the setting's stamp")?;
            let at = local_stamp_at(now, previous);
            transaction
                .execute(
                    "insert into setting (key, value, value_at, value_dev)
                     values (?1, ?2, ?3, ?4)
                     on conflict (key) do update set
                         value = excluded.value,
                         value_at = excluded.value_at,
                         value_dev = excluded.value_dev",
                    params![
                        key,
                        serde_json::to_string(value).context("serialize the setting value")?,
                        at.as_epoch_ms(),
                        dev.to_string(),
                    ],
                )
                .context("write the setting")?;
            at
        }
    };
    let mutation = change.into_mutation(id, at, dev);
    enqueue(transaction, &mutation, in_flight, now).context("queue the mutation")?;
    Ok(mutation)
}

fn device_id(transaction: &Transaction<'_>) -> Result<DeviceId> {
    let device_id: String = transaction
        .query_row("select device_id from account where id = 1", [], |row| {
            row.get(0)
        })
        .optional()
        .context("query the account row")?
        .ok_or_eyre("no account is signed in")?;
    device_id.parse().context("parse the account's device id")
}

/// Runs `query`, which selects one nullable epoch-ms column of the row keyed
/// by `key`. `None` when there is no row or the field was never written.
fn previous_at(transaction: &Transaction<'_>, query: &str, key: &str) -> Result<Option<Timestamp>> {
    let epoch_ms: Option<i64> = transaction
        .query_row(query, [key], |row| row.get(0))
        .optional()
        .context("query the stamp")?
        .flatten();
    epoch_ms
        .map(Timestamp::from_epoch_ms)
        .transpose()
        .context("read the stamp as a timestamp")
}

/// Deletes the queued, not-in-flight outbox rows with the mutation's
/// coalesce key and inserts the mutation.
fn enqueue(
    transaction: &Transaction<'_>,
    mutation: &Mutation,
    in_flight: &HashSet<MutationId>,
    now: Timestamp,
) -> Result<()> {
    let key = coalesce_key(mutation);
    let queued: Vec<String> = transaction
        .prepare_cached("select mutation_id from outbox where coalesce_key = ?1")
        .context("prepare the coalescing query")?
        .query_map([&key], |row| row.get(0))
        .context("query the rows with the same key")?
        .collect::<rusqlite::Result<_>>()
        .context("read the rows with the same key")?;
    for id in queued {
        let parsed: MutationId = id.parse().context("parse a queued mutation id")?;
        if !in_flight.contains(&parsed) {
            transaction
                .execute("delete from outbox where mutation_id = ?1", [&id])
                .context("delete the replaced outbox row")?;
        }
    }
    // `created_at` is the wall clock, not the mutation's `at`, so the outbox
    // keeps the order the user acted in even when a field's stamp was pushed
    // past `now` by a future-stamped pull.
    transaction
        .execute(
            "insert into outbox (mutation_id, coalesce_key, payload, created_at)
             values (?1, ?2, ?3, ?4)",
            params![
                mutation.id().to_string(),
                key,
                serde_json::to_string(mutation).context("serialize the mutation")?,
                now.as_epoch_ms(),
            ],
        )
        .context("insert the outbox row")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::scratch_dir;
    use rusqlite::Connection;

    const DEVICE: &str = "019a3f10-8c2e-7a41-9b3d-5e6f7a8b9c0d";
    const OTHER_DEVICE: &str = "019a3f10-8c2e-7a41-9b3d-ffffffffffff";
    const EPISODE: &str = "4b1e2c3d-4e5f-5a6b-9c7d-8e9f0a1b2c3d";
    const PODCAST: &str = "9c3e5b1a-2f4d-5e6c-8a7b-1d2e3f4a5b6c";

    fn ts(text: &str) -> Timestamp {
        text.parse().expect("test literal is RFC 3339")
    }

    fn position(position_ms: i64, while_playing: bool) -> LocalChange {
        LocalChange::Episode {
            episode_id: EPISODE.parse().expect("literal is a UUID"),
            podcast_id: PODCAST.parse().expect("literal is a UUID"),
            change: EpisodeChange::Position {
                position_ms,
                while_playing,
            },
        }
    }

    /// A store with a signed-in account.
    async fn signed_in_store() -> (tempfile::TempDir, Store) {
        let dir = scratch_dir();
        let store = Store::open(&dir.path().join("pod.db")).expect("the store opens");
        store
            .write(|transaction| {
                transaction
                    .execute(
                        "insert into account
                             (id, server_url, user_id, username, device_id, token)
                         values (1, 'https://pod.example', ?1, 'kilian', ?2, 'secret')",
                        params![OTHER_DEVICE, DEVICE],
                    )
                    .context("insert the account")
            })
            .await
            .expect("the account is inserted");
        (dir, store)
    }

    async fn apply_at(store: &Store, change: LocalChange, now: Timestamp) -> Mutation {
        store
            .write(move |transaction| {
                apply(
                    transaction,
                    change,
                    MutationId::new_v7(),
                    &HashSet::new(),
                    now,
                )
            })
            .await
            .expect("the local write commits")
    }

    async fn outbox_ids(store: &Store) -> Vec<MutationId> {
        store
            .read(|connection: &Connection| {
                let mut statement = connection
                    .prepare("select mutation_id from outbox order by created_at, mutation_id")
                    .context("prepare the outbox query")?;
                let ids = statement
                    .query_map([], |row| row.get::<_, String>(0))
                    .context("query the outbox")?
                    .map(|id| {
                        id.context("read a mutation id")?
                            .parse()
                            .context("parse a mutation id")
                    })
                    .collect::<Result<_>>()?;
                Ok(ids)
            })
            .await
            .expect("the outbox reads")
    }

    #[test]
    fn a_stamp_is_now_unless_the_field_is_as_new_or_newer() {
        let now = ts("2026-10-24T09:12:03.120Z");
        assert_eq!(local_stamp_at(now, None), now);
        assert_eq!(local_stamp_at(now, Some(ts("2026-10-24T09:00:00Z"))), now);
        assert_eq!(
            local_stamp_at(now, Some(now)),
            ts("2026-10-24T09:12:03.121Z")
        );
        assert_eq!(
            local_stamp_at(now, Some(ts("2026-10-24T10:00:00Z"))),
            ts("2026-10-24T10:00:00.001Z")
        );
    }

    #[test]
    fn only_position_saves_during_playback_wait_long() {
        assert_eq!(position(1, true).sync_delay(), PLAYBACK_SYNC_DELAY);
        assert_eq!(position(1, false).sync_delay(), WRITE_SYNC_DELAY);
    }

    #[tokio::test]
    async fn two_writes_in_the_same_millisecond_get_increasing_stamps() {
        let (_dir, store) = signed_in_store().await;
        let now = ts("2026-10-24T09:12:03.120Z");
        let first = apply_at(&store, position(1000, false), now).await;
        let second = apply_at(&store, position(2000, false), now).await;
        assert_eq!(first.at(), now);
        assert_eq!(second.at(), ts("2026-10-24T09:12:03.121Z"));
        assert_eq!(second.dev(), DEVICE.parse().expect("literal is a UUID"));
    }

    #[tokio::test]
    async fn a_write_after_a_future_stamped_pull_still_wins_locally() {
        let (_dir, store) = signed_in_store().await;
        let pulled_at = ts("2026-10-24T10:00:00Z");
        store
            .write(move |transaction| {
                transaction
                    .execute(
                        "insert into episode_state
                             (episode_id, podcast_id, position_ms, position_at, position_dev)
                         values (?1, ?2, 5000, ?3, ?4)",
                        params![EPISODE, PODCAST, pulled_at.as_epoch_ms(), OTHER_DEVICE],
                    )
                    .context("insert the pulled state")
            })
            .await
            .expect("the pulled row is stored");

        let mutation = apply_at(&store, position(1000, false), ts("2026-10-24T09:00:00Z")).await;
        assert_eq!(mutation.at(), ts("2026-10-24T10:00:00.001Z"));

        let row: (i64, i64, String) = store
            .read(|connection| {
                connection
                    .query_row(
                        "select position_ms, position_at, position_dev from episode_state",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .context("read the state")
            })
            .await
            .expect("the state reads");
        assert_eq!(
            row,
            (
                1000,
                ts("2026-10-24T10:00:00.001Z").as_epoch_ms(),
                DEVICE.to_owned()
            )
        );
    }

    #[tokio::test]
    async fn coalescing_replaces_a_queued_row_and_keeps_an_in_flight_one() {
        let (_dir, store) = signed_in_store().await;
        let sync = SyncHandle::default();
        let write = |change| write(&store, &sync, change);
        write(position(1000, false))
            .await
            .expect("the write commits");
        let replacing = write(position(2000, false))
            .await
            .expect("the write commits");
        assert_eq!(outbox_ids(&store).await, vec![replacing.id()]);

        sync.set_in_flight([replacing.id()]);
        let newest = write(position(3000, false))
            .await
            .expect("the write commits");
        let mut queued = outbox_ids(&store).await;
        queued.sort_unstable();
        assert_eq!(queued, vec![replacing.id(), newest.id()]);
    }

    #[tokio::test]
    async fn each_kind_of_change_gets_its_own_coalesce_key() {
        let (_dir, store) = signed_in_store().await;
        let sync = SyncHandle::default();
        let episode = |change| LocalChange::Episode {
            episode_id: EPISODE.parse().expect("literal is a UUID"),
            podcast_id: PODCAST.parse().expect("literal is a UUID"),
            change,
        };
        let changes = [
            position(1000, true),
            episode(EpisodeChange::Played(true)),
            episode(EpisodeChange::Archived(true)),
            LocalChange::Subscription {
                podcast_id: PODCAST.parse().expect("literal is a UUID"),
                feed_url: "https://example.com/feed.xml".into(),
                subscribed: true,
            },
            LocalChange::Setting {
                key: "playback_speed".into(),
                value: serde_json::json!(1.3),
            },
        ];
        let mut keys = Vec::new();
        for change in changes {
            let mutation = write(&store, &sync, change)
                .await
                .expect("the write commits");
            keys.push(coalesce_key(&mutation));
        }
        assert_eq!(
            keys,
            [
                format!("position:{EPISODE}"),
                format!("played:{EPISODE}"),
                format!("archived:{EPISODE}"),
                format!("sub:{PODCAST}"),
                "setting:playback_speed".to_owned(),
            ]
        );
        assert_eq!(outbox_ids(&store).await.len(), keys.len());
    }

    #[tokio::test]
    async fn a_write_records_the_mutation_and_asks_for_a_sync() {
        let (_dir, store) = signed_in_store().await;
        let sync = SyncHandle::default();
        let mutation = write(
            &store,
            &sync,
            LocalChange::Setting {
                key: "playback_speed".into(),
                value: serde_json::json!(1.3),
            },
        )
        .await
        .expect("the write commits");
        assert!(sync.take_wanted().is_some());

        let (key, payload): (String, String) = store
            .read(|connection| {
                connection
                    .query_row("select coalesce_key, payload from outbox", [], |row| {
                        Ok((row.get(0)?, row.get(1)?))
                    })
                    .context("read the outbox row")
            })
            .await
            .expect("the outbox reads");
        assert_eq!(key, "setting:playback_speed");
        let queued: Mutation = serde_json::from_str(&payload).expect("the payload is a mutation");
        assert_eq!(queued, mutation);
    }

    #[tokio::test]
    async fn a_batch_commits_together_and_an_empty_one_asks_for_no_sync() {
        let (_dir, store) = signed_in_store().await;
        let sync = SyncHandle::default();
        let error = write_all(&store, &sync, |_| {
            Ok(vec![position(1000, false), position(-1, false)])
        })
        .await
        .expect_err("a negative position fails the batch");
        assert!(format!("{error:#}").contains("can't be negative"));
        assert_eq!(
            outbox_ids(&store).await,
            Vec::new(),
            "nothing of it commits"
        );
        assert_eq!(sync.take_wanted(), None);

        let mutations = write_all(&store, &sync, |_| {
            Ok(vec![position(1000, true), position(2000, false)])
        })
        .await
        .expect("the batch commits");
        assert_eq!(mutations.len(), 2);
        assert_eq!(
            outbox_ids(&store).await,
            vec![mutations[1].id()],
            "the batch coalesces like single writes"
        );
        let wanted = sync.take_wanted().expect("the batch asks for a sync");
        assert!(
            wanted <= std::time::Instant::now() + WRITE_SYNC_DELAY,
            "the most urgent change sets the delay"
        );

        let none = write_all(&store, &sync, |_| Ok(Vec::new()))
            .await
            .expect("an empty batch commits");
        assert!(none.is_empty());
        assert_eq!(sync.take_wanted(), None);
    }

    #[tokio::test]
    async fn a_write_without_an_account_fails_and_changes_nothing() {
        let dir = scratch_dir();
        let store = Store::open(&dir.path().join("pod.db")).expect("the store opens");
        let sync = SyncHandle::default();
        let error = write(&store, &sync, position(1000, false))
            .await
            .expect_err("there is no device id to stamp with");
        assert!(format!("{error:#}").contains("no account is signed in"));
        assert_eq!(outbox_ids(&store).await, Vec::new());
        assert_eq!(sync.take_wanted(), None);
    }
}
