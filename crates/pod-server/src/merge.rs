//! Merging a new feed into the show its `<podcast:guid>` names (spec 18).
//!
//! A pending podcast Y whose first fetched feed carries the guid of a live
//! podcast X is the same show under a second URL. Rather than ingest a
//! second copy, the poller points Y's URLs at X, redirects Y's subscribers
//! to X and deletes Y. Every step can run again, so a crash between them
//! only means Y's next poll finishes the merge.

use color_eyre::eyre::{Result, WrapErr};
use pod_protocol::{PodcastId, PodcastStatus, UserId};
use sqlx::PgPool;
use sqlx::types::Uuid;

use crate::db::SqlTimestamp;
use crate::feed::ParsedFeed;
use crate::sync::redirect_subscription;
use crate::version::VersionScope;

/// What [`merge`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeOutcome {
    /// Y is gone and its subscribers follow X.
    Merged {
        into: PodcastId,
        /// X now polls Y's URL.
        moved: bool,
    },
    /// No live podcast has the feed's guid, or Y was ingested meanwhile.
    NotADuplicate,
}

/// The live podcast that `podcast_id`'s feed duplicates, if `podcast_id` is
/// still at version 0 and the feed names a guid another podcast has. Of
/// several, the lowest id.
pub async fn duplicate_of(
    pool: &PgPool,
    podcast_id: PodcastId,
    guid: Uuid,
) -> Result<Option<PodcastId>> {
    let target = sqlx::query_scalar!(
        "select x.id from podcast x
         where x.podcast_guid = $2 and x.version > 0 and x.id <> $1
           and exists (select from podcast y where y.id = $1 and y.version = 0)
         order by x.id
         limit 1",
        podcast_id.as_uuid(),
        guid
    )
    .fetch_optional(pool)
    .await
    .context("look for a podcast with the same guid")?;
    Ok(target.map(PodcastId::from_uuid))
}

/// Merges the pending podcast `from` into the live podcast `into` (spec
/// 18.2), given `from`'s freshly parsed `feed`.
pub async fn merge(
    pool: &PgPool,
    from: PodcastId,
    into: PodcastId,
    feed: &ParsedFeed,
) -> Result<MergeOutcome> {
    // =========================================================================
    // Step 1: aliases, the possible move, and the poll schedule.
    // =========================================================================

    // Lock order: X's podcast row (the version scope), then Y's, then the
    // alias and poll rows, as ingest locks `podcast` before `feed_poll`.
    let Some(mut scope) = VersionScope::begin_podcast(pool, into)
        .await
        .context("lock the podcast merged into")?
    else {
        return Ok(MergeOutcome::NotADuplicate);
    };
    let pending = sqlx::query!(
        "select feed_url, version from podcast where id = $1 for update",
        from.as_uuid()
    )
    .fetch_optional(scope.conn())
    .await
    .context("lock the pending podcast")?;
    let Some(pending) = pending.filter(|row| row.version == 0) else {
        // A concurrent poll ingested it, or a previous merge deleted it.
        return Ok(MergeOutcome::NotADuplicate);
    };

    sqlx::query!(
        "update podcast_alias set podcast_id = $2 where podcast_id = $1",
        from.as_uuid(),
        into.as_uuid()
    )
    .execute(scope.conn())
    .await
    .context("point the pending podcast's aliases at the show")?;

    // X moves to Y's URL when its own feed is dead, or Y's is newer: the
    // listener subscribed to Y because that's where new episodes are.
    let live = sqlx::query!(
        r#"select p.status,
                  (select max(e.published_at) from episode e
                   where e.podcast_id = p.id and not e.deleted)
                      as "newest: SqlTimestamp"
           from podcast p where p.id = $1"#,
        into.as_uuid()
    )
    .fetch_one(scope.conn())
    .await
    .context("read the show's status and newest episode")?;
    let newest_in_feed = feed
        .episodes
        .iter()
        .map(|episode| episode.published_at)
        .max();
    let moved = live.status != PodcastStatus::Ok.as_str()
        || newest_in_feed > live.newest.map(|newest| newest.0);
    if moved {
        let version = scope.next_version();
        sqlx::query!(
            "update podcast set feed_url = $2, version = $3, updated_at = now() where id = $1",
            into.as_uuid(),
            pending.feed_url,
            version
        )
        .execute(scope.conn())
        .await
        .context("move the show to the new feed URL")?;
        // The validators and hash belong to the old URL's responses.
        sqlx::query!(
            "update feed_poll set etag = null, last_modified = null, content_hash = null
             where podcast_id = $1",
            into.as_uuid()
        )
        .execute(scope.conn())
        .await
        .context("forget the old feed's validators")?;
    }
    // Y's subscribers are about to follow X, so X is polled now, even if
    // it had been parked without subscribers or as gone.
    sqlx::query!(
        "update feed_poll set next_poll_at = date_trunc('milliseconds', now())
         where podcast_id = $1",
        into.as_uuid()
    )
    .execute(scope.conn())
    .await
    .context("schedule a poll of the show")?;
    scope
        .commit()
        .await
        .context("commit the merge's catalog step")?;

    // =========================================================================
    // Step 2: redirect every subscriber, one user scope at a time.
    // =========================================================================

    // A subscribe that resolved Y before step 1 committed can still add a
    // row for Y, so the list is read again until it comes back empty.
    loop {
        let users = sqlx::query_scalar!(
            "select user_id from subscription
             where podcast_id = $1 and replaced_by is null
             order by user_id",
            from.as_uuid()
        )
        .fetch_all(pool)
        .await
        .context("list the pending podcast's subscribers")?;
        if users.is_empty() {
            break;
        }
        for user_id in users {
            let user_id = UserId::from_uuid(user_id);
            let Some(mut scope) = VersionScope::begin_user(pool, user_id)
                .await
                .context("lock the subscriber's scope")?
            else {
                continue;
            };
            redirect_subscription(&mut scope, user_id, from, into)
                .await
                .context("redirect a subscription to the show")?;
            scope.commit().await.context("commit a redirect")?;
        }
    }

    // =========================================================================
    // Step 3: delete Y, which no client ever received (version 0).
    // =========================================================================

    // TODO: a subscribe whose transaction resolved Y before step 1 and
    // commits only after this can still leave a row for Y without
    // `replaced_by`. Its client then shows a podcast that never loads.
    let mut tx = pool
        .begin()
        .await
        .context("begin deleting the pending podcast")?;
    let locked = sqlx::query_scalar!(
        "select id from podcast where id = $1 and version = 0 for update",
        from.as_uuid()
    )
    .fetch_optional(&mut *tx)
    .await
    .context("lock the pending podcast")?;
    if locked.is_some() {
        sqlx::query!(
            "delete from feed_poll where podcast_id = $1",
            from.as_uuid()
        )
        .execute(&mut *tx)
        .await
        .context("delete the pending podcast's poll row")?;
        sqlx::query!("delete from podcast where id = $1", from.as_uuid())
            .execute(&mut *tx)
            .await
            .context("delete the pending podcast")?;
    }
    tx.commit()
        .await
        .context("commit deleting the pending podcast")?;

    tracing::info!(podcast_id = %from, into = %into, moved, "merged a duplicate feed into its show");
    Ok(MergeOutcome::Merged { into, moved })
}
