//! Queries behind the read endpoints for the online web client (spec 4.5).
//!
//! A user reads only podcasts they are subscribed to: a subscription row
//! with `subscribed` set and no `replaced_by`, the same rule the sync pull
//! uses for podcast scopes. Deleted episodes are left out of every list,
//! since they only exist to tell syncing clients to drop them.
//!
//! Paginated lists are ordered by `(published_at desc, id desc)` and page
//! with a [`PageCursor`]. Each query reads one row past the limit, so a full
//! page knows whether another follows without a count query.

pub mod cursor;

use std::collections::BTreeMap;

use color_eyre::eyre::{Result, WrapErr};
use pod_protocol::settings::{INBOX_WINDOW_DAYS, inbox_window_days};
use pod_protocol::{
    DeviceId, Episode, EpisodeId, EpisodeItem, EpisodeState, InboxItem, KNOWN_SETTINGS,
    LiveEpisode, Podcast, PodcastId, PodcastItem, PodcastStatus, Timestamp, UserId,
};
use sqlx::PgPool;

use crate::db::SqlTimestamp;
pub use cursor::{InvalidCursor, PageCursor};

/// Which page of a list to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageRequest {
    /// Read rows strictly after this position; `None` for the first page.
    pub after: Option<PageCursor>,
    /// Already clamped by [`pod_protocol::clamp_page_limit`].
    pub limit: u32,
}

/// One page of a list and the cursor of the page after it.
#[derive(Debug, Clone, PartialEq)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<PageCursor>,
}

impl<T> Page<T> {
    /// Cuts `rows`, read with [`fetch_limit`], down to `limit` items. The
    /// next cursor is the last kept item's position, and only exists when
    /// the extra row proved more follow.
    fn of(mut rows: Vec<T>, limit: u32, position: impl Fn(&T) -> PageCursor) -> Self {
        let limit = usize::try_from(limit).expect("a u32 fits in usize");
        let more = rows.len() > limit;
        rows.truncate(limit);
        let next_cursor = if more {
            rows.last().map(position)
        } else {
            None
        };
        Self {
            items: rows,
            next_cursor,
        }
    }
}

/// The SQL `LIMIT` that reads one row past `limit`.
fn fetch_limit(limit: u32) -> i64 {
    i64::from(limit) + 1
}

fn episode_position(episode: &Episode) -> PageCursor {
    PageCursor {
        published_at: episode.published_at,
        id: episode.id.as_uuid(),
    }
}

/// Builds an [`Episode`] from a query record that selects the episode
/// columns under their own names, with `published_at` read as a
/// [`SqlTimestamp`]. The episode and inbox queries add different columns
/// around the same episode columns, so their records are different types.
macro_rules! episode_from_row {
    ($row:ident) => {
        Episode {
            id: EpisodeId::from_uuid($row.id),
            podcast_id: PodcastId::from_uuid($row.podcast_id),
            title: $row.title,
            summary_html: $row.summary_html,
            content_html: $row.content_html,
            link: $row.link,
            published_at: $row.published_at.into(),
            duration_ms: $row.duration_ms,
            audio_url: $row.audio_url,
            audio_type: $row.audio_type,
            audio_bytes: $row.audio_bytes,
            artwork_url: $row.artwork_url,
            season: $row.season,
            number: $row.number,
            version: $row.version,
        }
    };
}

// ==============================================================================
// Podcasts
// ==============================================================================

/// The user's subscribed podcasts, newest episode first, or only `only`
/// when given.
///
/// Podcasts without a live episode sort first (Postgres puts nulls first in
/// a descending order): a `pending` podcast is one the user just subscribed
/// to, and the top of the list is where they look for it. A subscription
/// whose podcast row doesn't exist is skipped.
pub async fn podcasts(
    pool: &PgPool,
    user_id: UserId,
    only: Option<PodcastId>,
) -> Result<Vec<PodcastItem>> {
    let rows = sqlx::query!(
        r#"select p.id, p.feed_url, p.status, p.status_message, p.title, p.author,
                  p.description, p.link, p.artwork_url, p.language, p.version,
                  latest.published_at as "latest_published_at: SqlTimestamp"
           from subscription s
           join podcast p on p.id = s.podcast_id
           cross join lateral (
               select max(e.published_at) as published_at from episode e
               where e.podcast_id = p.id and not e.deleted
           ) latest
           where s.user_id = $1 and s.subscribed and s.replaced_by is null
             and ($2::uuid is null or p.id = $2)
           order by latest.published_at desc nulls first, p.id desc"#,
        user_id.as_uuid(),
        only.map(|id| id.as_uuid()),
    )
    .fetch_all(pool)
    .await
    .context("read the subscribed podcasts")?;
    rows.into_iter()
        .map(|row| {
            Ok(PodcastItem {
                podcast: Podcast {
                    id: PodcastId::from_uuid(row.id),
                    feed_url: row.feed_url,
                    status: row
                        .status
                        .parse::<PodcastStatus>()
                        .context("read the podcast status")?,
                    status_message: row.status_message,
                    title: row.title,
                    author: row.author,
                    description: row.description,
                    link: row.link,
                    artwork_url: row.artwork_url,
                    language: row.language,
                    version: row.version,
                },
                latest_published_at: row.latest_published_at.map(Into::into),
            })
        })
        .collect()
}

/// Whether the user may read the podcast: subscribed, and the podcast row
/// exists (an alias id has a subscription row but no podcast).
async fn is_readable(pool: &PgPool, user_id: UserId, podcast_id: PodcastId) -> Result<bool> {
    sqlx::query_scalar!(
        r#"select exists (
               select from subscription s join podcast p on p.id = s.podcast_id
               where s.user_id = $1 and s.podcast_id = $2
                 and s.subscribed and s.replaced_by is null
           ) as "readable!""#,
        user_id.as_uuid(),
        podcast_id.as_uuid(),
    )
    .fetch_one(pool)
    .await
    .context("check the subscription")
}

// ==============================================================================
// Episodes
// ==============================================================================

/// One page of a podcast's live episodes with the user's state on each, or
/// `None` when the user may not read the podcast.
///
/// The keyset condition and order match the `episode (podcast_id,
/// published_at desc)` index, so a page costs an index range scan however
/// deep it is.
pub async fn episodes(
    pool: &PgPool,
    user_id: UserId,
    podcast_id: PodcastId,
    page: PageRequest,
) -> Result<Option<Page<EpisodeItem>>> {
    if !is_readable(pool, user_id, podcast_id)
        .await
        .context("check that the podcast is readable")?
    {
        return Ok(None);
    }
    let rows = sqlx::query!(
        r#"select e.id, e.podcast_id, e.title, e.summary_html, e.content_html, e.link,
                  e.published_at as "published_at: SqlTimestamp", e.duration_ms,
                  e.audio_url, e.audio_type, e.audio_bytes, e.artwork_url, e.season,
                  e.number, e.version,
                  st.version as "state_version?",
                  st.position_ms as "position_ms?",
                  st.position_at as "position_at: SqlTimestamp",
                  st.position_dev,
                  st.played as "played?",
                  st.played_at as "played_at: SqlTimestamp",
                  st.played_dev,
                  st.archived as "archived?",
                  st.archived_at as "archived_at: SqlTimestamp",
                  st.archived_dev
           from episode e
           left join episode_state st on st.user_id = $2 and st.episode_id = e.id
           where e.podcast_id = $1 and not e.deleted
             and ($3::timestamptz is null or (e.published_at, e.id) < ($3, $4::uuid))
           order by e.published_at desc, e.id desc
           limit $5"#,
        podcast_id.as_uuid(),
        user_id.as_uuid(),
        page.after.map(|cursor| SqlTimestamp(cursor.published_at)) as _,
        page.after.map(|cursor| cursor.id),
        fetch_limit(page.limit),
    )
    .fetch_all(pool)
    .await
    .context("read a page of episodes")?;

    let items = rows
        .into_iter()
        .map(|row| {
            // The left join fills every state column or none; the version
            // is never null in a stored row, so it tells which.
            let episode_state = row.state_version.map(|version| EpisodeState {
                episode_id: EpisodeId::from_uuid(row.id),
                podcast_id: PodcastId::from_uuid(row.podcast_id),
                position_ms: row.position_ms.unwrap_or_default(),
                position_at: row.position_at.map(Into::into),
                position_dev: row.position_dev.map(DeviceId::from_uuid),
                played: row.played.unwrap_or_default(),
                played_at: row.played_at.map(Into::into),
                played_dev: row.played_dev.map(DeviceId::from_uuid),
                archived: row.archived.unwrap_or_default(),
                archived_at: row.archived_at.map(Into::into),
                archived_dev: row.archived_dev.map(DeviceId::from_uuid),
                version,
            });
            EpisodeItem {
                episode: LiveEpisode(episode_from_row!(row)),
                episode_state,
            }
        })
        .collect();
    Ok(Some(Page::of(items, page.limit, |item| {
        episode_position(&item.episode.0)
    })))
}

// ==============================================================================
// Inbox
// ==============================================================================

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// One page of the user's inbox at `now` (spec section 6).
pub async fn inbox(
    pool: &PgPool,
    user_id: UserId,
    now: Timestamp,
    page: PageRequest,
) -> Result<Page<InboxItem>> {
    let stored_window = sqlx::query_scalar!(
        "select value from user_setting where user_id = $1 and key = $2",
        user_id.as_uuid(),
        INBOX_WINDOW_DAYS,
    )
    .fetch_optional(pool)
    .await
    .context("read the inbox window setting")?;
    let window_ms = inbox_window_days(stored_window.as_ref()) * DAY_MS;
    let window_start = Timestamp::from_epoch_ms(now.as_epoch_ms() - window_ms)
        .context("compute the start of the inbox window")?;

    // Spec 6's filter, plus what only the server has to say: the
    // subscription belongs to this user and isn't an alias, and deleted
    // episodes (which clients have already removed) neither show up nor
    // count as a podcast's newest. The tie-break of the newest-episode
    // subquery is the list's own order, so "newest" means the episode that
    // would be listed first.
    //
    // Per subscribed podcast, the window condition is a range scan of the
    // `episode (podcast_id, published_at desc)` index, and the newest-episode
    // subquery reads that index from the top, so the cost grows with the
    // episodes inside the window rather than with the back catalogue.
    let rows = sqlx::query!(
        r#"select e.id, e.podcast_id, e.title, e.summary_html, e.content_html, e.link,
                  e.published_at as "published_at: SqlTimestamp", e.duration_ms,
                  e.audio_url, e.audio_type, e.audio_bytes, e.artwork_url, e.season,
                  e.number, e.version,
                  p.title as podcast_title, p.artwork_url as podcast_artwork_url,
                  coalesce(st.position_ms, 0) as "position_ms!"
           from subscription s
           join podcast p on p.id = s.podcast_id
           join episode e on e.podcast_id = s.podcast_id
           left join episode_state st on st.user_id = s.user_id and st.episode_id = e.id
           where s.user_id = $1 and s.subscribed and s.replaced_by is null
             and not e.deleted
             and not coalesce(st.played, false)
             and not coalesce(st.archived, false)
             and e.published_at >= $2
             and (e.published_at >= s.subscribed_at
                  or e.id = (select newest.id from episode newest
                             where newest.podcast_id = e.podcast_id and not newest.deleted
                             order by newest.published_at desc, newest.id desc
                             limit 1))
             and ($3::timestamptz is null or (e.published_at, e.id) < ($3, $4::uuid))
           order by e.published_at desc, e.id desc
           limit $5"#,
        user_id.as_uuid(),
        SqlTimestamp(window_start) as _,
        page.after.map(|cursor| SqlTimestamp(cursor.published_at)) as _,
        page.after.map(|cursor| cursor.id),
        fetch_limit(page.limit),
    )
    .fetch_all(pool)
    .await
    .context("read a page of the inbox")?;

    let items = rows
        .into_iter()
        .map(|row| InboxItem {
            podcast_title: row.podcast_title,
            podcast_artwork_url: row.podcast_artwork_url,
            position_ms: row.position_ms,
            episode: LiveEpisode(episode_from_row!(row)),
        })
        .collect();
    Ok(Page::of(items, page.limit, |item| {
        episode_position(&item.episode.0)
    }))
}

// ==============================================================================
// Settings
// ==============================================================================

/// Every setting the user stored, plus the defaults of the known keys they
/// didn't.
pub async fn settings(
    pool: &PgPool,
    user_id: UserId,
) -> Result<BTreeMap<String, serde_json::Value>> {
    let stored = sqlx::query!(
        "select key, value from user_setting where user_id = $1",
        user_id.as_uuid(),
    )
    .fetch_all(pool)
    .await
    .context("read the user's settings")?;
    let mut settings: BTreeMap<_, _> = KNOWN_SETTINGS
        .iter()
        .map(|known| (known.key.to_owned(), known.default.to_json()))
        .collect();
    settings.extend(stored.into_iter().map(|row| (row.key, row.value)));
    Ok(settings)
}
