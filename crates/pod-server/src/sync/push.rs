//! The push half of a sync request: applying mutations (spec 4.4).
//!
//! [`apply_batch`] runs a request's mutations in one transaction that first
//! locks the user scope, so two requests from one user (two devices, or a
//! retry racing the original) run one after the other and can't deadlock.
//! Each mutation then goes through [`apply`]: a replay returns its stored
//! result, anything else is decided field by field with [`wins`] and
//! recorded in `applied_mutation` for the next replay.

use color_eyre::eyre::{Result, WrapErr, eyre};
use pod_protocol::{
    DeviceId, EpisodeId, Mutation, MutationOutcome, MutationRejection, MutationResult, PodcastId,
    RejectionCode, Stamp, Timestamp, UserId, clamp_mutation_at, wins,
};
use sqlx::PgPool;

use crate::db::SqlTimestamp;
use crate::fetch::FetchPolicy;
use crate::version::VersionScope;

use super::subscription::apply_subscription;

/// Applies a request's mutations in request order and commits, returning
/// one result per entry.
///
/// `mutations` is the output of `SyncRequest::parse_mutations`: an `Err`
/// entry is reported `rejected: invalid_mutation`. `now` is the server time
/// every `at` in the batch is clamped against. `policy` is the fetch
/// guard's, which a subscribe checks its feed URL against.
///
/// An `Err` return rolls the whole batch back, results and
/// `applied_mutation` rows included, so the client keeps its outbox and a
/// retry gets a fresh evaluation.
pub async fn apply_batch(
    pool: &PgPool,
    user_id: UserId,
    mutations: &[Result<Mutation, MutationRejection>],
    now: Timestamp,
    policy: &FetchPolicy,
) -> Result<Vec<MutationResult>> {
    // The session that authenticated the request references the user with
    // `on delete cascade`, so a missing user means the account was deleted
    // mid-request. Failing the request is the right answer for that race.
    let mut scope = VersionScope::begin_user(pool, user_id)
        .await
        .context("lock the user scope")?
        .ok_or_else(|| eyre!("user {user_id} doesn't exist"))?;
    let mut results = Vec::with_capacity(mutations.len());
    for mutation in mutations {
        results.push(
            apply(&mut scope, user_id, mutation, now, policy)
                .await
                .context("apply a mutation")?,
        );
    }
    scope.commit().await.context("commit the mutations")?;
    Ok(results)
}

/// Applies one mutation inside the batch's scope (spec 4.4 `apply`).
async fn apply(
    scope: &mut VersionScope,
    user_id: UserId,
    mutation: &Result<Mutation, MutationRejection>,
    now: Timestamp,
    policy: &FetchPolicy,
) -> Result<MutationResult> {
    let id = match mutation {
        Ok(mutation) => mutation.id(),
        Err(rejection) => rejection.id,
    };

    // ==========================================================================
    // Replay
    // ==========================================================================

    // A retried request, or an id repeated within this batch, gets the
    // stored result: the earlier evaluation already wrote whatever it wrote,
    // and a fresh one could disagree with what the client was told (the
    // clamp moves with the clock, and newer writes may have landed since).
    let stored = sqlx::query_scalar!(
        "select result from applied_mutation where user_id = $1 and mutation_id = $2",
        user_id.as_uuid(),
        id.as_uuid()
    )
    .fetch_optional(scope.conn())
    .await
    .context("look up an earlier result")?;
    if let Some(stored) = stored {
        return serde_json::from_value(stored).context("read a stored result");
    }

    // ==========================================================================
    // Evaluation
    // ==========================================================================

    let outcome = match mutation {
        // The rejection echoes the client's `at` clamped like any other, so
        // every result carries the `at` the server would have stored.
        Err(rejection) => MutationOutcome::Rejected {
            at: rejection.at.map(|at| clamp_mutation_at(at, now)),
            code: RejectionCode::InvalidMutation,
        },
        Ok(mutation) => {
            let stamp = Stamp {
                at: clamp_mutation_at(mutation.at(), now),
                dev: mutation.dev(),
            };
            match mutation {
                Mutation::EpisodePosition(m) => {
                    let write = EpisodeWrite::Position(m.position_ms);
                    apply_episode(scope, user_id, m.episode_id, write, stamp).await
                }
                Mutation::EpisodePlayed(m) => {
                    let write = EpisodeWrite::Played(m.played);
                    apply_episode(scope, user_id, m.episode_id, write, stamp).await
                }
                Mutation::EpisodeArchived(m) => {
                    let write = EpisodeWrite::Archived(m.archived);
                    apply_episode(scope, user_id, m.episode_id, write, stamp).await
                }
                Mutation::SettingSet(m) => {
                    apply_setting(scope, user_id, &m.key, &m.value, stamp).await
                }
                Mutation::SubscriptionSet(m) => {
                    apply_subscription(scope, user_id, m, stamp, policy).await
                }
            }
            .with_context(|| format!("apply {} mutation {id}", mutation.type_name()))?
        }
    };

    // ==========================================================================
    // Record
    // ==========================================================================

    // Rejections are recorded too. A rejected mutation can never succeed,
    // so a replay answering with the stored rejection is the same answer a
    // fresh evaluation gives, without the lookup.
    let result = MutationResult { id, outcome };
    sqlx::query!(
        "insert into applied_mutation (user_id, mutation_id, result) values ($1, $2, $3)",
        user_id.as_uuid(),
        id.as_uuid(),
        serde_json::to_value(&result).context("serialize the result")?
    )
    .execute(scope.conn())
    .await
    .context("record the result")?;
    Ok(result)
}

/// The one `episode_state` field an `episode.*` mutation writes.
#[derive(Debug, Clone, Copy)]
enum EpisodeWrite {
    Position(i64),
    Played(bool),
    Archived(bool),
}

/// Applies an `episode.*` mutation to the user's state for the episode.
///
/// A missing `episode_state` row reads as the DDL defaults with every field
/// unstamped, so the first write to any field wins and the upsert leaves
/// the other fields at their defaults.
async fn apply_episode(
    scope: &mut VersionScope,
    user_id: UserId,
    episode_id: EpisodeId,
    write: EpisodeWrite,
    stamp: Stamp,
) -> Result<MutationOutcome> {
    // The episode row supplies `podcast_id`, which `episode_state` stores
    // for the pull half, and proves the episode exists.
    let Some(row) = sqlx::query!(
        r#"select e.podcast_id,
                  s.position_at as "position_at?: SqlTimestamp", s.position_dev as "position_dev?",
                  s.played_at as "played_at?: SqlTimestamp", s.played_dev as "played_dev?",
                  s.archived_at as "archived_at?: SqlTimestamp", s.archived_dev as "archived_dev?"
           from episode e
           left join episode_state s on s.episode_id = e.id and s.user_id = $1
           where e.id = $2"#,
        user_id.as_uuid(),
        episode_id.as_uuid()
    )
    .fetch_optional(scope.conn())
    .await
    .context("load the episode state")?
    else {
        return Ok(MutationOutcome::Rejected {
            at: Some(stamp.at),
            code: RejectionCode::UnknownEpisode,
        });
    };

    let (old_at, old_dev) = match write {
        EpisodeWrite::Position(_) => (row.position_at, row.position_dev),
        EpisodeWrite::Played(_) => (row.played_at, row.played_dev),
        EpisodeWrite::Archived(_) => (row.archived_at, row.archived_dev),
    };
    let old = Stamp::from_columns(
        old_at.map(Timestamp::from),
        old_dev.map(DeviceId::from_uuid),
    );
    if !wins(stamp, old) {
        return Ok(MutationOutcome::Superseded { at: stamp.at });
    }

    let version = scope.next_version();
    let podcast_id = PodcastId::from_uuid(row.podcast_id);
    let (user, episode, podcast) = (
        user_id.as_uuid(),
        episode_id.as_uuid(),
        podcast_id.as_uuid(),
    );
    let (at, dev) = (SqlTimestamp(stamp.at), stamp.dev.as_uuid());
    match write {
        EpisodeWrite::Position(position_ms) => sqlx::query!(
            "insert into episode_state
                 (user_id, episode_id, podcast_id, position_ms, position_at, position_dev, version)
             values ($1, $2, $3, $4, $5, $6, $7)
             on conflict (user_id, episode_id) do update set
                 position_ms = excluded.position_ms, position_at = excluded.position_at,
                 position_dev = excluded.position_dev, version = excluded.version",
            user,
            episode,
            podcast,
            position_ms,
            at as _,
            dev,
            version
        ),
        EpisodeWrite::Played(played) => sqlx::query!(
            "insert into episode_state
                 (user_id, episode_id, podcast_id, played, played_at, played_dev, version)
             values ($1, $2, $3, $4, $5, $6, $7)
             on conflict (user_id, episode_id) do update set
                 played = excluded.played, played_at = excluded.played_at,
                 played_dev = excluded.played_dev, version = excluded.version",
            user,
            episode,
            podcast,
            played,
            at as _,
            dev,
            version
        ),
        EpisodeWrite::Archived(archived) => sqlx::query!(
            "insert into episode_state
                 (user_id, episode_id, podcast_id, archived, archived_at, archived_dev, version)
             values ($1, $2, $3, $4, $5, $6, $7)
             on conflict (user_id, episode_id) do update set
                 archived = excluded.archived, archived_at = excluded.archived_at,
                 archived_dev = excluded.archived_dev, version = excluded.version",
            user,
            episode,
            podcast,
            archived,
            at as _,
            dev,
            version
        ),
    }
    .execute(scope.conn())
    .await
    .context("write the episode state")?;
    Ok(MutationOutcome::Applied { at: stamp.at })
}

/// Applies a `setting.set`. Unknown keys and any JSON value, `null`
/// included, are stored as sent, so older clients keep settings that newer
/// ones write (spec 2.4).
async fn apply_setting(
    scope: &mut VersionScope,
    user_id: UserId,
    key: &str,
    value: &serde_json::Value,
    stamp: Stamp,
) -> Result<MutationOutcome> {
    let old = sqlx::query!(
        r#"select value_at as "value_at: SqlTimestamp", value_dev
           from user_setting where user_id = $1 and key = $2"#,
        user_id.as_uuid(),
        key
    )
    .fetch_optional(scope.conn())
    .await
    .context("load the setting")?
    .map(|row| Stamp {
        at: row.value_at.0,
        dev: DeviceId::from_uuid(row.value_dev),
    });
    if !wins(stamp, old) {
        return Ok(MutationOutcome::Superseded { at: stamp.at });
    }

    let version = scope.next_version();
    sqlx::query!(
        "insert into user_setting (user_id, key, value, value_at, value_dev, version)
         values ($1, $2, $3, $4, $5, $6)
         on conflict (user_id, key) do update set
             value = excluded.value, value_at = excluded.value_at,
             value_dev = excluded.value_dev, version = excluded.version",
        user_id.as_uuid(),
        key,
        value,
        SqlTimestamp(stamp.at) as _,
        stamp.dev.as_uuid(),
        version
    )
    .execute(scope.conn())
    .await
    .context("write the setting")?;
    Ok(MutationOutcome::Applied { at: stamp.at })
}
