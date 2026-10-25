//! Catalog writes for one podcast scope (spec sections 2.2, 3.2 and 3.3).
//!
//! The poller hands [`ingest`] a parsed feed and [`set_status`] a fetch
//! outcome. Housekeeping calls [`purge_episode`]. All three write through
//! [`VersionScope`], so every row they touch takes the next version of the
//! podcast scope and clients receive it by sync pull.
//!
//! Each returns `None` when the row it targets doesn't exist, the same way
//! [`VersionScope::begin_podcast`] does.

use std::collections::HashMap;

use color_eyre::eyre::{Result, WrapErr, eyre};
use pod_protocol::{EpisodeId, PodcastId, PodcastStatus};
use sqlx::PgPool;

use crate::db::SqlTimestamp;
use crate::feed::{ParsedEpisode, ParsedFeed};
use crate::version::VersionScope;

/// What [`ingest`] did with a feed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngestOutcome {
    /// The feed hash matched `feed_poll.content_hash`, so nothing was
    /// written, not even the podcast's status.
    Unchanged,
    Applied(IngestSummary),
}

/// The rows an applied ingest wrote. All zero when only episodes the
/// catalog keeps anyway left the feed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IngestSummary {
    /// The podcast row took a version, for new metadata or a status that
    /// became `ok`.
    pub podcast_written: bool,
    /// New episodes, oldest first.
    pub inserted: Vec<EpisodeId>,
    /// Known episodes whose content changed.
    pub updated: usize,
    /// Deleted episodes that were back in the feed.
    pub restored: usize,
}

/// What [`set_status`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusWrite {
    /// The podcast already had this status and message.
    Unchanged,
    Written {
        version: i64,
    },
}

/// What [`purge_episode`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PurgeOutcome {
    Purged { version: i64 },
    AlreadyDeleted,
}

// =============================================================================
// Ingest
// =============================================================================

/// Writes the difference between `feed` and the stored catalog (spec 3.3).
///
/// Skips when the feed hash matches `feed_poll.content_hash`. That check
/// runs under the scope lock, so of two concurrent ingests of the same feed
/// the second finds the first's hash and writes nothing.
///
/// A successful ingest leaves the podcast `ok` with no status message (spec
/// 3.2), and writes that in the same podcast row version as any metadata
/// change. A skipped ingest doesn't touch the status: the poller calls
/// [`set_status`] itself when the feed is unchanged (or answers 304).
///
/// Episodes missing from the feed stay as they are. A deleted episode that
/// is back in the feed is restored.
pub async fn ingest(
    pool: &PgPool,
    podcast_id: PodcastId,
    feed: &ParsedFeed,
) -> Result<Option<IngestOutcome>> {
    let Some(mut scope) = VersionScope::begin_podcast(pool, podcast_id)
        .await
        .context("lock the podcast scope for ingest")?
    else {
        return Ok(None);
    };

    // -------------------------------------------------------------------------
    // Compare the feed hash. Under the lock, a concurrent ingest that got
    // there first has committed its hash already.
    // -------------------------------------------------------------------------

    let feed_hash = feed.feed_hash();
    // A left join, so a podcast without a poll row still ingests. Subscribe
    // creates both rows together, so none should exist.
    let podcast = sqlx::query!(
        r#"select p.metadata_hash, p.status, p.status_message,
                  f.content_hash as "feed_hash?"
           from podcast p left join feed_poll f on f.podcast_id = p.id
           where p.id = $1"#,
        podcast_id.as_uuid()
    )
    .fetch_one(scope.conn())
    .await
    .context("read the stored podcast hashes")?;
    if podcast.feed_hash.as_deref() == Some(feed_hash.as_bytes()) {
        scope.commit().await.context("end an unchanged ingest")?;
        return Ok(Some(IngestOutcome::Unchanged));
    }

    // -------------------------------------------------------------------------
    // Diff against the stored episodes.
    // -------------------------------------------------------------------------

    let stored = sqlx::query!(
        "select guid, content_hash, deleted from episode where podcast_id = $1",
        podcast_id.as_uuid()
    )
    .fetch_all(scope.conn())
    .await
    .context("load the stored episode hashes")?
    .into_iter()
    .map(|row| {
        let episode = StoredEpisode {
            content_hash: row.content_hash,
            deleted: row.deleted,
        };
        (row.guid, episode)
    })
    .collect();
    let metadata_hash = feed.metadata.content_hash();
    let write_podcast = podcast.metadata_hash.as_deref() != Some(metadata_hash.as_bytes())
        || podcast.status != status_text(PodcastStatus::Ok)
        || podcast.status_message.is_some();
    let changes = episode_changes(&feed.episodes, &stored);

    // -------------------------------------------------------------------------
    // Write the podcast row first, then episodes oldest first, so a client
    // backfilling a large catalogue gets the title and artwork on its first
    // page.
    // -------------------------------------------------------------------------

    let mut summary = IngestSummary {
        podcast_written: write_podcast,
        ..IngestSummary::default()
    };
    if write_podcast {
        let version = scope.next_version();
        let metadata = &feed.metadata;
        sqlx::query!(
            "update podcast
             set title = $2, author = $3, description = $4, link = $5,
                 artwork_url = $6, language = $7, metadata_hash = $8,
                 status = $9, status_message = null,
                 version = $10, updated_at = now()
             where id = $1",
            podcast_id.as_uuid(),
            metadata.title,
            metadata.author,
            metadata.description,
            metadata.link,
            metadata.artwork_url,
            metadata.language,
            metadata_hash.as_bytes(),
            status_text(PodcastStatus::Ok),
            version
        )
        .execute(scope.conn())
        .await
        .context("write the podcast row")?;
    }

    // TODO: batch these through `unnest` if first ingests of feeds with
    // thousands of episodes turn out slow; one statement per episode keeps
    // the version assignment plain.
    for (episode, change) in changes {
        let version = scope.next_version();
        let content_hash = episode.content_hash();
        // One upsert covers all three changes: the id derives from the
        // guid, and an update or restore rewrites every feed field anyway.
        sqlx::query!(
            "insert into episode (id, podcast_id, guid, title, summary_html,
                 content_html, link, published_at, duration_ms, audio_url,
                 audio_type, audio_bytes, artwork_url, season, number,
                 content_hash, version)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13,
                     $14, $15, $16, $17)
             on conflict (podcast_id, guid) do update set
                 title = excluded.title,
                 summary_html = excluded.summary_html,
                 content_html = excluded.content_html,
                 link = excluded.link,
                 published_at = excluded.published_at,
                 duration_ms = excluded.duration_ms,
                 audio_url = excluded.audio_url,
                 audio_type = excluded.audio_type,
                 audio_bytes = excluded.audio_bytes,
                 artwork_url = excluded.artwork_url,
                 season = excluded.season,
                 number = excluded.number,
                 content_hash = excluded.content_hash,
                 deleted = false,
                 version = excluded.version,
                 updated_at = now()",
            episode.id.as_uuid(),
            podcast_id.as_uuid(),
            episode.guid,
            episode.title,
            episode.summary_html,
            episode.content_html,
            episode.link,
            SqlTimestamp(episode.published_at) as _,
            episode.duration_ms,
            episode.audio_url,
            episode.audio_type,
            episode.audio_bytes,
            episode.artwork_url,
            episode.season,
            episode.number,
            content_hash.as_bytes(),
            version
        )
        .execute(scope.conn())
        .await
        .context("write an episode row")?;
        match change {
            EpisodeChange::Insert => summary.inserted.push(episode.id),
            EpisodeChange::Update => summary.updated += 1,
            EpisodeChange::Restore => summary.restored += 1,
        }
    }

    // -------------------------------------------------------------------------
    // Record the feed hash, so the next poll of the same feed stops early,
    // and commit together with the counter.
    // -------------------------------------------------------------------------

    sqlx::query!(
        "update feed_poll set content_hash = $2 where podcast_id = $1",
        podcast_id.as_uuid(),
        feed_hash.as_bytes()
    )
    .execute(scope.conn())
    .await
    .context("store the feed hash")?;
    scope.commit().await.context("commit the ingest")?;
    Ok(Some(IngestOutcome::Applied(summary)))
}

/// An episode row as the diff sees it.
#[derive(Debug)]
struct StoredEpisode {
    content_hash: Vec<u8>,
    deleted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EpisodeChange {
    Insert,
    Update,
    Restore,
}

/// The episodes of the feed that need a write, oldest first (spec 3.3 steps
/// 3 and 5). Stored episodes the feed no longer has don't appear.
///
/// Ties in `published_at` are common (items without a date all get the
/// epoch), so the guid breaks them and the same feed always gets its
/// versions in the same order.
fn episode_changes<'feed>(
    episodes: &'feed [ParsedEpisode],
    stored: &HashMap<String, StoredEpisode>,
) -> Vec<(&'feed ParsedEpisode, EpisodeChange)> {
    let mut changes: Vec<_> = episodes
        .iter()
        .filter_map(|episode| {
            let change = match stored.get(&episode.guid) {
                None => EpisodeChange::Insert,
                Some(row) if row.deleted => EpisodeChange::Restore,
                Some(row) if row.content_hash != episode.content_hash().as_bytes() => {
                    EpisodeChange::Update
                }
                Some(_) => return None,
            };
            Some((episode, change))
        })
        .collect();
    changes.sort_by(|(a, _), (b, _)| (a.published_at, &a.guid).cmp(&(b.published_at, &b.guid)));
    changes
}

// =============================================================================
// Status
// =============================================================================

/// Sets the podcast's `status` and `status_message` as one new podcast row
/// version (spec 3.2), or does nothing if both already hold these values.
///
/// The poller calls this for every fetch outcome an [`ingest`] doesn't
/// cover: `pending` to `failing` on the first error, `ok` to `failing` after
/// seven days of errors, back to `ok` when an unchanged feed or a 304
/// follows errors, and `gone` on a 410.
pub async fn set_status(
    pool: &PgPool,
    podcast_id: PodcastId,
    status: PodcastStatus,
    message: Option<&str>,
) -> Result<Option<StatusWrite>> {
    let Some(mut scope) = VersionScope::begin_podcast(pool, podcast_id)
        .await
        .context("lock the podcast scope for a status change")?
    else {
        return Ok(None);
    };
    let stored = sqlx::query!(
        "select status, status_message from podcast where id = $1",
        podcast_id.as_uuid()
    )
    .fetch_one(scope.conn())
    .await
    .context("read the podcast status")?;
    if stored.status == status_text(status) && stored.status_message.as_deref() == message {
        scope
            .commit()
            .await
            .context("end an unchanged status write")?;
        return Ok(Some(StatusWrite::Unchanged));
    }

    let version = scope.next_version();
    sqlx::query!(
        "update podcast
         set status = $2, status_message = $3, version = $4, updated_at = now()
         where id = $1",
        podcast_id.as_uuid(),
        status_text(status),
        message,
        version
    )
    .execute(scope.conn())
    .await
    .context("write the podcast status")?;
    scope.commit().await.context("commit the status change")?;
    Ok(Some(StatusWrite::Written { version }))
}

/// The `podcast.status` column value, which matches the wire form.
fn status_text(status: PodcastStatus) -> &'static str {
    match status {
        PodcastStatus::Pending => "pending",
        PodcastStatus::Ok => "ok",
        PodcastStatus::Failing => "failing",
        PodcastStatus::Gone => "gone",
    }
}

// =============================================================================
// Purge
// =============================================================================

/// Marks an episode deleted with a new version, so clients drop it (spec
/// 4.2 rule 6). Purging an already deleted episode writes nothing.
///
/// Feed content never deletes episodes, so this is the only way one
/// disappears. An episode still in its feed comes back with the next ingest
/// whose feed hash changed, because ingest restores deleted guids it sees
/// (spec 3.3 step 3).
// TODO: decide with the housekeeping job whether purged guids should stay
// purged (a tombstone ingest skips), see SESSION.md.
pub async fn purge_episode(pool: &PgPool, episode_id: EpisodeId) -> Result<Option<PurgeOutcome>> {
    // An episode never changes podcast, so reading this outside the lock is
    // safe.
    let Some(podcast_id) = sqlx::query_scalar!(
        "select podcast_id from episode where id = $1",
        episode_id.as_uuid()
    )
    .fetch_optional(pool)
    .await
    .context("look up the episode's podcast")?
    else {
        return Ok(None);
    };
    let podcast_id = PodcastId::from_uuid(podcast_id);
    let mut scope = VersionScope::begin_podcast(pool, podcast_id)
        .await
        .context("lock the podcast scope for a purge")?
        .ok_or_else(|| eyre!("episode {episode_id} references missing podcast {podcast_id}"))?;

    let deleted = sqlx::query_scalar!(
        "select deleted from episode where id = $1",
        episode_id.as_uuid()
    )
    .fetch_one(scope.conn())
    .await
    .context("read the episode's deleted flag")?;
    if deleted {
        scope.commit().await.context("end a repeated purge")?;
        return Ok(Some(PurgeOutcome::AlreadyDeleted));
    }

    let version = scope.next_version();
    sqlx::query!(
        "update episode set deleted = true, version = $2, updated_at = now()
         where id = $1",
        episode_id.as_uuid(),
        version
    )
    .execute(scope.conn())
    .await
    .context("mark the episode deleted")?;
    scope.commit().await.context("commit the purge")?;
    Ok(Some(PurgeOutcome::Purged { version }))
}
