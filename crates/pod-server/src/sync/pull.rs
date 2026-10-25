//! The pull half of a sync request: scope rows above the client's cursors
//! (spec 4.2 rules 2 to 6).
//!
//! [`pull`] reads every requested scope in one `REPEATABLE READ READ ONLY`
//! transaction. Each scope spans several tables, and writers commit in
//! version order per scope, so one snapshot for all statements is what lets
//! a client advance its cursor past every version it was sent without
//! skipping one a concurrent writer committed between two reads (spec 2.1).
//! The transaction is read-only and takes no row locks, so a pull never
//! waits for a writer.
//!
//! The user scope is filled first, then podcast scopes in request order,
//! all sharing one change limit. Within a scope, rows from each table come
//! back merged by `version`, lowest first.
//!
//! `tests/convergence.rs` is the spec 9 property test that guards the
//! snapshot rule. It reaches the rule through [`pull_with`], a test hook
//! that can break it on purpose.

use std::collections::HashSet;
use std::fmt;
use std::pin::Pin;

use color_eyre::eyre::{Result, WrapErr, eyre};
use pod_protocol::{
    DeviceId, Episode, EpisodeId, EpisodeState, EpisodeTombstone, NotSubscribedScope, Podcast,
    PodcastChanges, PodcastCursor, PodcastId, PodcastScope, PodcastStatus, ScopeChanges,
    ScopeCursors, ScopeEpisode, Setting, Subscription, UserId, UserScope,
};
use sqlx::{PgConnection, PgPool};

use crate::db::SqlTimestamp;

/// Reads the changes above `cursors` for `user_id`, at most `limit` rows in
/// total.
pub async fn pull(
    pool: &PgPool,
    user_id: UserId,
    cursors: &ScopeCursors,
    limit: u32,
) -> Result<ScopeChanges> {
    pull_with(pool, user_id, cursors, limit, PullTestHooks::default()).await
}

// ==============================================================================
// Test hook
// ==============================================================================

// Everything in this section exists only for `tests/convergence.rs`, which
// must show that it fails when the snapshot rule is broken. Production code
// calls `pull`, which passes the default hooks: the snapshot and no gaps.

/// Which isolation [`pull_with`] reads with.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PullIsolation {
    /// One `REPEATABLE READ` snapshot for every statement (spec 2.1).
    #[default]
    Snapshot,
    /// `READ COMMITTED`: each statement sees whatever committed before it
    /// started. This is the bug spec 2.1 forbids.
    ReadCommitted,
}

/// An async callback [`pull_with`] awaits between two table reads of one
/// scope, so a test can commit a write in exactly that gap.
#[doc(hidden)]
pub type BetweenStatements<'a> =
    &'a (dyn Fn() -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> + Sync);

/// Test-only knobs for [`pull_with`]. The default is what production runs.
#[doc(hidden)]
#[derive(Clone, Copy, Default)]
pub struct PullTestHooks<'a> {
    pub isolation: PullIsolation,
    pub between_statements: Option<BetweenStatements<'a>>,
}

impl fmt::Debug for PullTestHooks<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PullTestHooks")
            .field("isolation", &self.isolation)
            .field("between_statements", &self.between_statements.is_some())
            .finish()
    }
}

impl PullTestHooks<'_> {
    async fn between_statements(&self) {
        if let Some(hook) = self.between_statements {
            hook().await;
        }
    }
}

/// [`pull`] with test hooks. Not part of the API: only the convergence
/// test calls it.
#[doc(hidden)]
pub async fn pull_with(
    pool: &PgPool,
    user_id: UserId,
    cursors: &ScopeCursors,
    limit: u32,
    hooks: PullTestHooks<'_>,
) -> Result<ScopeChanges> {
    let begin = match hooks.isolation {
        PullIsolation::Snapshot => "begin isolation level repeatable read read only",
        PullIsolation::ReadCommitted => "begin isolation level read committed read only",
    };
    let mut tx = pool
        .begin_with(begin)
        .await
        .context("begin the pull snapshot")?;
    let mut remaining = usize::try_from(limit).expect("a u32 fits in usize");

    // ==========================================================================
    // User scope
    // ==========================================================================

    let user = read_user_scope(&mut tx, user_id, cursors.user, remaining, &hooks)
        .await
        .context("read the user scope")?;
    remaining -= user.rows.len();

    // ==========================================================================
    // Podcast scopes, in request order, until the limit runs out
    // ==========================================================================

    // Read inside the snapshot, so a subscribe in this request's push (or an
    // unsubscribe racing it) agrees with the user-scope rows just read.
    let subscribed = subscribed_podcasts(&mut tx, user_id, &cursors.podcasts)
        .await
        .context("read which requested podcasts are subscribed")?;
    let mut podcasts = Vec::with_capacity(cursors.podcasts.len());
    for cursor in &cursors.podcasts {
        // Checked before the limit, so a scope the client must drop says so
        // even when no rows were left for it, and the client doesn't keep
        // asking for it.
        if !subscribed.contains(&cursor.id) {
            podcasts.push(PodcastScope::NotSubscribed(NotSubscribedScope {
                id: cursor.id,
            }));
            continue;
        }
        let page = if remaining == 0 {
            Page::unreached()
        } else {
            read_podcast_scope(&mut tx, *cursor, remaining, &hooks)
                .await
                .with_context(|| format!("read podcast scope {}", cursor.id))?
        };
        remaining -= page.rows.len();
        podcasts.push(PodcastScope::Ok(page.into_podcast_changes(*cursor)));
    }

    tx.commit().await.context("end the pull snapshot")?;
    Ok(ScopeChanges {
        user: user.into_user_scope(cursors.user),
        podcasts,
    })
}

// ==============================================================================
// Pages
// ==============================================================================

/// One scope's rows above its cursor, lowest version first, at most the
/// limit the scope was read with.
#[derive(Debug)]
struct Page<Row> {
    rows: Vec<Row>,
    /// No rows beyond the last one exist.
    complete: bool,
}

/// A row of some scope, which pages order by version.
trait Versioned {
    fn version(&self) -> i64;
}

impl<Row: Versioned> Page<Row> {
    /// The scope the limit didn't reach: no rows, and not complete because
    /// nobody looked.
    fn unreached() -> Self {
        Self {
            rows: Vec::new(),
            complete: false,
        }
    }

    /// Keeps the `limit` lowest-versioned of `rows`.
    ///
    /// Each table of the scope was read with `limit + 1` rows, so the
    /// `limit + 1` lowest versions of the whole scope are among `rows`, and
    /// one row past `limit` proves more exist without a count query.
    /// Versions within a scope are unique, so the order is total.
    fn of_lowest(mut rows: Vec<Row>, limit: usize) -> Self {
        rows.sort_unstable_by_key(Row::version);
        let complete = rows.len() <= limit;
        rows.truncate(limit);
        Self { rows, complete }
    }

    /// The scope's response `version`: the highest returned, or the cursor
    /// when nothing was.
    fn version(&self, cursor: i64) -> i64 {
        self.rows.last().map_or(cursor, Row::version)
    }
}

// ==============================================================================
// User scope
// ==============================================================================

#[derive(Debug)]
enum UserRow {
    Subscription(Subscription),
    EpisodeState(EpisodeState),
    Setting(Setting),
}

impl Versioned for UserRow {
    fn version(&self) -> i64 {
        match self {
            Self::Subscription(row) => row.version,
            Self::EpisodeState(row) => row.version,
            Self::Setting(row) => row.version,
        }
    }
}

impl Page<UserRow> {
    fn into_user_scope(self, cursor: i64) -> UserScope {
        let mut scope = UserScope {
            version: self.version(cursor),
            complete: self.complete,
            subscriptions: Vec::new(),
            episode_states: Vec::new(),
            settings: Vec::new(),
        };
        for row in self.rows {
            match row {
                UserRow::Subscription(row) => scope.subscriptions.push(row),
                UserRow::EpisodeState(row) => scope.episode_states.push(row),
                UserRow::Setting(row) => scope.settings.push(row),
            }
        }
        scope
    }
}

/// Reads the user scope's lowest `limit` rows above `cursor`.
///
/// One typed query per table, each `limit + 1` rows, merged in memory. The
/// spec suggests one `UNION ALL ... ORDER BY version LIMIT n` over the three
/// tables, but their shapes differ, so a union needs every column padded
/// with nulls and loses the macros' nullability checks. The per-table
/// queries can over-fetch up to three times the limit, which only happens
/// when all three tables have that many changes: subscriptions and settings
/// stay small in practice.
async fn read_user_scope(
    conn: &mut PgConnection,
    user_id: UserId,
    cursor: i64,
    limit: usize,
    hooks: &PullTestHooks<'_>,
) -> Result<Page<UserRow>> {
    let fetch = fetch_limit(limit);
    let subscriptions = sqlx::query!(
        r#"select podcast_id, feed_url, subscribed,
                  subscribed_at as "subscribed_at: SqlTimestamp", subscribed_dev,
                  replaced_by, version
           from subscription
           where user_id = $1 and version > $2
           order by version limit $3"#,
        user_id.as_uuid(),
        cursor,
        fetch
    )
    .fetch_all(&mut *conn)
    .await
    .context("read subscriptions")?
    .into_iter()
    .map(|row| {
        UserRow::Subscription(Subscription {
            podcast_id: PodcastId::from_uuid(row.podcast_id),
            feed_url: row.feed_url,
            subscribed: row.subscribed,
            subscribed_at: row.subscribed_at.into(),
            subscribed_dev: DeviceId::from_uuid(row.subscribed_dev),
            replaced_by: row.replaced_by.map(PodcastId::from_uuid),
            version: row.version,
        })
    });
    hooks.between_statements().await;

    let episode_states = sqlx::query!(
        r#"select episode_id, podcast_id,
                  position_ms, position_at as "position_at: SqlTimestamp", position_dev,
                  played, played_at as "played_at: SqlTimestamp", played_dev,
                  archived, archived_at as "archived_at: SqlTimestamp", archived_dev,
                  version
           from episode_state
           where user_id = $1 and version > $2
           order by version limit $3"#,
        user_id.as_uuid(),
        cursor,
        fetch
    )
    .fetch_all(&mut *conn)
    .await
    .context("read episode states")?
    .into_iter()
    .map(|row| {
        UserRow::EpisodeState(EpisodeState {
            episode_id: EpisodeId::from_uuid(row.episode_id),
            podcast_id: PodcastId::from_uuid(row.podcast_id),
            position_ms: row.position_ms,
            position_at: row.position_at.map(Into::into),
            position_dev: row.position_dev.map(DeviceId::from_uuid),
            played: row.played,
            played_at: row.played_at.map(Into::into),
            played_dev: row.played_dev.map(DeviceId::from_uuid),
            archived: row.archived,
            archived_at: row.archived_at.map(Into::into),
            archived_dev: row.archived_dev.map(DeviceId::from_uuid),
            version: row.version,
        })
    });
    hooks.between_statements().await;

    let settings = sqlx::query!(
        r#"select key, value, value_at as "value_at: SqlTimestamp", value_dev, version
           from user_setting
           where user_id = $1 and version > $2
           order by version limit $3"#,
        user_id.as_uuid(),
        cursor,
        fetch
    )
    .fetch_all(&mut *conn)
    .await
    .context("read settings")?
    .into_iter()
    .map(|row| {
        UserRow::Setting(Setting {
            key: row.key,
            value: row.value,
            value_at: row.value_at.into(),
            value_dev: DeviceId::from_uuid(row.value_dev),
            version: row.version,
        })
    });

    let rows = subscriptions
        .chain(episode_states)
        .chain(settings)
        .collect();
    Ok(Page::of_lowest(rows, limit))
}

// ==============================================================================
// Podcast scopes
// ==============================================================================

/// The requested podcasts the user may pull: a subscription row with
/// `subscribed` set and no `replaced_by`. A replaced id is an alias whose
/// rows live in the canonical scope, which the user scope's redirect points
/// the client to (spec 4.4).
async fn subscribed_podcasts(
    conn: &mut PgConnection,
    user_id: UserId,
    cursors: &[PodcastCursor],
) -> Result<HashSet<PodcastId>> {
    if cursors.is_empty() {
        return Ok(HashSet::new());
    }
    let ids: Vec<_> = cursors.iter().map(|cursor| cursor.id.as_uuid()).collect();
    let subscribed = sqlx::query_scalar!(
        "select podcast_id from subscription
         where user_id = $1 and podcast_id = any($2)
           and subscribed and replaced_by is null",
        user_id.as_uuid(),
        &ids
    )
    .fetch_all(conn)
    .await
    .context("read the subscriptions of the requested podcasts")?;
    Ok(subscribed.into_iter().map(PodcastId::from_uuid).collect())
}

#[derive(Debug)]
enum PodcastRow {
    Podcast(Podcast),
    Episode(ScopeEpisode),
}

impl Versioned for PodcastRow {
    fn version(&self) -> i64 {
        match self {
            Self::Podcast(row) => row.version,
            Self::Episode(row) => row.version(),
        }
    }
}

impl Page<PodcastRow> {
    fn into_podcast_changes(self, cursor: PodcastCursor) -> PodcastChanges {
        let mut changes = PodcastChanges {
            id: cursor.id,
            version: self.version(cursor.version),
            complete: self.complete,
            podcast: None,
            episodes: Vec::new(),
        };
        for row in self.rows {
            match row {
                PodcastRow::Podcast(row) => changes.podcast = Some(row),
                PodcastRow::Episode(row) => changes.episodes.push(row),
            }
        }
        changes
    }
}

/// Reads a podcast scope's lowest `limit` rows above the cursor: the
/// podcast row and episodes share the scope's counter, so they merge by
/// version like the user scope's tables, and the podcast row counts against
/// the limit like any episode.
///
/// A pending podcast's row has version 0 and no episodes, so it is never
/// sent: every cursor is at least 0. A subscribed podcast with no row at
/// all (a subscribe that didn't create one) reads as an empty, complete
/// scope.
async fn read_podcast_scope(
    conn: &mut PgConnection,
    cursor: PodcastCursor,
    limit: usize,
    hooks: &PullTestHooks<'_>,
) -> Result<Page<PodcastRow>> {
    let podcast = sqlx::query!(
        "select id, feed_url, status, status_message, title, author, description, link,
                artwork_url, language, version
         from podcast
         where id = $1 and version > $2",
        cursor.id.as_uuid(),
        cursor.version
    )
    .fetch_optional(&mut *conn)
    .await
    .context("read the podcast row")?;
    let podcast = match podcast {
        None => None,
        Some(row) => Some(PodcastRow::Podcast(Podcast {
            id: PodcastId::from_uuid(row.id),
            feed_url: row.feed_url,
            status: parse_status(&row.status).context("read the podcast status")?,
            status_message: row.status_message,
            title: row.title,
            author: row.author,
            description: row.description,
            link: row.link,
            artwork_url: row.artwork_url,
            language: row.language,
            version: row.version,
        })),
    };
    hooks.between_statements().await;

    // Tombstones come from the same rows: a deleted episode keeps its
    // columns, and only its id and version go out (spec 4.2 rule 6).
    let episodes = sqlx::query!(
        r#"select id, podcast_id, title, summary_html, content_html, link,
                  published_at as "published_at: SqlTimestamp", duration_ms,
                  audio_url, audio_type, audio_bytes, artwork_url, season, number,
                  deleted, version
           from episode
           where podcast_id = $1 and version > $2
           order by version limit $3"#,
        cursor.id.as_uuid(),
        cursor.version,
        fetch_limit(limit)
    )
    .fetch_all(&mut *conn)
    .await
    .context("read episodes")?
    .into_iter()
    .map(|row| {
        let id = EpisodeId::from_uuid(row.id);
        PodcastRow::Episode(if row.deleted {
            ScopeEpisode::Deleted(EpisodeTombstone {
                id,
                version: row.version,
            })
        } else {
            ScopeEpisode::Live(Episode {
                id,
                podcast_id: PodcastId::from_uuid(row.podcast_id),
                title: row.title,
                summary_html: row.summary_html,
                content_html: row.content_html,
                link: row.link,
                published_at: row.published_at.into(),
                duration_ms: row.duration_ms,
                audio_url: row.audio_url,
                audio_type: row.audio_type,
                audio_bytes: row.audio_bytes,
                artwork_url: row.artwork_url,
                season: row.season,
                number: row.number,
                version: row.version,
            })
        })
    });

    let rows = podcast.into_iter().chain(episodes).collect();
    Ok(Page::of_lowest(rows, limit))
}

/// The `LIMIT` that reads one row past `limit`, which [`Page::of_lowest`]
/// needs to tell whether more rows exist.
fn fetch_limit(limit: usize) -> i64 {
    i64::try_from(limit + 1).expect("the limit is at most MAX_SYNC_LIMIT")
}

/// The `podcast.status` column, whose check constraint allows exactly the
/// wire values.
fn parse_status(status: &str) -> Result<PodcastStatus> {
    match status {
        "pending" => Ok(PodcastStatus::Pending),
        "ok" => Ok(PodcastStatus::Ok),
        "failing" => Ok(PodcastStatus::Failing),
        "gone" => Ok(PodcastStatus::Gone),
        other => Err(eyre!("unknown podcast status {other:?}")),
    }
}
