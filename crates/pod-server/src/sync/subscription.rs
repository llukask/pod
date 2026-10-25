//! Applying `subscription.set` (spec 4.4), including alias redirects.
//!
//! A subscription is user-scope state, but subscribing also decides what
//! the poller does: the first subscribe to a feed creates its catalog rows
//! and schedules a poll, and the last unsubscribe parks the feed at
//! `infinity`. Neither allocates from `podcast.version_counter` or locks the
//! `podcast` row, so a subscribe never waits for an ingest of the feed.
//!
//! Lock order, which every writer that shares these rows keeps, so none can
//! wait on another in a cycle:
//!
//! 1. `users` row (the batch's [`VersionScope`]).
//! 2. For a feed the server hasn't seen: the new `podcast`, `feed_poll` and
//!    `podcast_alias` rows, inserted `on conflict do nothing`. A concurrent
//!    first subscribe from another user waits here for ours to commit.
//! 3. The canonical podcast's `feed_poll` row, `for update`.
//! 4. The user's `subscription` rows, already serialized by 1.
//!
//! Ingest locks `podcast` and then writes `feed_poll`; the poller's
//! `feed_poll` writes are single statements. No path takes `feed_poll` and
//! then `users` or `podcast`.
//!
//! The `feed_poll` lock comes before this user's subscription row changes
//! and before counting subscribers (spec 3.1): an unsubscribe that counts
//! while another user's subscribe of the same feed is still uncommitted
//! would park a feed that has a subscriber.
// TODO: two batches from different users that each subscribe to several
// feeds in opposite orders can deadlock on steps 2 and 3. Postgres aborts
// one of them, which fails that request and the client retries it. Taking
// the batch's `feed_poll` locks up front in id order would avoid it.

use color_eyre::eyre::{Result, WrapErr};
use pod_protocol::{
    DeviceId, MutationOutcome, NormalizedFeedUrl, PodcastId, RejectionCode, Stamp, SubscriptionSet,
    Timestamp, UserId, normalize_feed_url, wins,
};
use sqlx::types::Uuid;
use url::Url;

use crate::db::SqlTimestamp;
use crate::fetch::{FetchPolicy, check_url};
use crate::version::VersionScope;

/// The fields of a subscription row that last-writer-wins decides.
/// `feed_url` has no stamp of its own; it moves with `subscribed`, as in
/// the clients' merge.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Fields {
    subscribed: bool,
    feed_url: String,
    stamp: Stamp,
}

/// A stored subscription row.
#[derive(Debug, Clone)]
struct Row {
    fields: Fields,
    replaced_by: Option<PodcastId>,
}

/// Applies a `subscription.set` inside the batch's user scope.
///
/// The outcome reports the last-writer-wins decision on the canonical
/// podcast's row, which is the mutation's own podcast unless it is
/// redirected.
pub(super) async fn apply_subscription(
    scope: &mut VersionScope,
    user_id: UserId,
    mutation: &SubscriptionSet,
    stamp: Stamp,
    policy: &FetchPolicy,
) -> Result<MutationOutcome> {
    let rejected = MutationOutcome::Rejected {
        at: Some(stamp.at),
        code: RejectionCode::InvalidFeedUrl,
    };

    // ==========================================================================
    // Feed URL
    // ==========================================================================

    // A subscribe to a URL the fetch guard refuses outright (scheme, port,
    // literal internal address) is rejected here, so the client drops its
    // placeholder instead of showing a podcast that can only fail. Host
    // names are resolved only when the poller fetches, never inside this
    // transaction. An unsubscribe skips the guard: the user must be able to
    // leave a feed even after the allowlist that admitted it shrank.
    let url = match normalize_feed_url(&mutation.feed_url) {
        Ok(url) => url,
        Err(error) => {
            tracing::info!(%user_id, mutation_id = %mutation.id, %error, "rejected a feed URL");
            return Ok(rejected);
        }
    };
    if mutation.subscribed {
        let fetched = Url::parse(url.feed_url()).expect("a normalized feed URL parses");
        if let Err(error) = check_url(&fetched, policy) {
            tracing::info!(%user_id, mutation_id = %mutation.id, %error, "rejected a feed URL");
            return Ok(rejected);
        }
    }

    // ==========================================================================
    // Canonical podcast
    // ==========================================================================

    // The mutation names X. The canonical podcast Y is where an earlier
    // redirect sent X, or else what the URL resolves to. The URL wins over
    // `mutation.podcast_id` even when no alias explains the difference: the
    // id is only ever derived from the URL, and redirecting turns a client
    // that derived it differently into a client that follows `replaced_by`.
    let named = load_row(scope, user_id, mutation.podcast_id)
        .await
        .context("load the named subscription")?;
    let canonical = match named.as_ref().and_then(|row| row.replaced_by) {
        Some(canonical) => canonical,
        None => resolve(scope, &url, mutation.subscribed)
            .await
            .context("resolve the feed URL")?,
    };
    let redirected = canonical != mutation.podcast_id;

    // An unsubscribe of a feed nobody subscribed to has no `feed_poll` row,
    // and neither has a podcast id that only an alias led to.
    let polled = sqlx::query_scalar!(
        "select podcast_id from feed_poll where podcast_id = $1 for update",
        canonical.as_uuid()
    )
    .fetch_optional(scope.conn())
    .await
    .context("lock the feed's poll row")?
    .is_some();

    // ==========================================================================
    // Last-writer-wins on Y
    // ==========================================================================

    let stored = if redirected {
        load_row(scope, user_id, canonical)
            .await
            .context("load the canonical subscription")?
    } else {
        named.clone()
    };
    let was_subscribed = stored.as_ref().is_some_and(|row| row.fields.subscribed);
    let mut fields = stored.map(|row| row.fields);

    // An X row nobody redirected yet holds the user's own choice for X.
    // Clients carry it into Y when they act on the redirect (spec 5.5 step
    // 1), so the server does the same to end in the state they compute.
    // The carry takes X only when its stamp is strictly newer: a later `at`,
    // or the same `at` and a greater device id. On an exact tie Y keeps its
    // fields. That is `wins` minus its same-device tie rule, because such a
    // tie here means one device's mutations for X and then Y were clamped
    // into one millisecond, and Y, written later in batch order, holds the
    // newer action.
    if redirected
        && let Some(x) = named.as_ref().filter(|x| x.replaced_by.is_none())
        && fields
            .as_ref()
            .is_none_or(|y| (x.fields.stamp.at, x.fields.stamp.dev) > (y.stamp.at, y.stamp.dev))
    {
        fields = Some(x.fields.clone());
    }
    let won = merge(
        &mut fields,
        Fields {
            subscribed: mutation.subscribed,
            feed_url: url.feed_url().to_owned(),
            stamp,
        },
    );

    // Y is written whenever it changed, and on every redirect even when
    // nothing changed: a client that still holds X needs Y's current state,
    // and only a new version sends it again.
    let fields = fields.expect("the mutation wins against a missing row");
    if won || redirected {
        write_canonical(scope, user_id, canonical, &fields)
            .await
            .context("write the canonical subscription")?;
    }

    // ==========================================================================
    // Redirect of X
    // ==========================================================================

    if redirected {
        write_redirect(scope, user_id, mutation.podcast_id, &url, canonical)
            .await
            .context("write the redirected subscription")?;
    }

    // ==========================================================================
    // Poll schedule
    // ==========================================================================

    // A subscribe that creates or reactivates a subscription polls the feed
    // right away (spec 3.4), which also brings back a feed parked as gone
    // or orphaned. A feed whose last subscriber left is parked.
    // TODO: a podcast X with its own catalog rows (only possible when the
    // client's id disagreed with the URL) keeps its schedule when its last
    // subscriber is redirected away; parking it would need a second
    // `feed_poll` lock in a fixed order.
    if polled {
        if fields.subscribed && !was_subscribed {
            sqlx::query!(
                "update feed_poll set next_poll_at = date_trunc('milliseconds', now())
                 where podcast_id = $1",
                canonical.as_uuid()
            )
            .execute(scope.conn())
            .await
            .context("schedule a poll")?;
        } else if !fields.subscribed {
            // TODO: this scans `subscription`, which has no index on
            // `podcast_id` (see SESSION.md).
            sqlx::query!(
                "update feed_poll set next_poll_at = 'infinity'
                 where podcast_id = $1 and not exists (
                     select 1 from subscription where podcast_id = $1 and subscribed)",
                canonical.as_uuid()
            )
            .execute(scope.conn())
            .await
            .context("park a feed without subscribers")?;
        }
    }

    Ok(if won {
        MutationOutcome::Applied { at: stamp.at }
    } else {
        MutationOutcome::Superseded { at: stamp.at }
    })
}

/// Replaces `fields` with `new` if `new` wins, and reports whether it did.
fn merge(fields: &mut Option<Fields>, new: Fields) -> bool {
    let won = wins(new.stamp, fields.as_ref().map(|old| old.stamp));
    if won {
        *fields = Some(new);
    }
    won
}

async fn load_row(
    scope: &mut VersionScope,
    user_id: UserId,
    podcast_id: PodcastId,
) -> Result<Option<Row>> {
    let row = sqlx::query!(
        r#"select feed_url, subscribed, subscribed_at as "subscribed_at: SqlTimestamp",
                  subscribed_dev, replaced_by
           from subscription where user_id = $1 and podcast_id = $2"#,
        user_id.as_uuid(),
        podcast_id.as_uuid()
    )
    .fetch_optional(scope.conn())
    .await
    .context("read the subscription")?;
    Ok(row.map(|row| Row {
        fields: Fields {
            subscribed: row.subscribed,
            feed_url: row.feed_url,
            stamp: Stamp {
                at: row.subscribed_at.0,
                dev: DeviceId::from_uuid(row.subscribed_dev),
            },
        },
        replaced_by: row.replaced_by.map(PodcastId::from_uuid),
    }))
}

/// The podcast a feed URL belongs to: its alias owner if the server knows
/// the URL, otherwise the id derived from it.
///
/// With `create`, a URL the server doesn't know gets its catalog rows: a
/// `pending` podcast at version 0, its `feed_poll` row (due now) and its
/// alias. They are inserted `on conflict do nothing` because another user
/// may subscribe to the same new feed at the same moment; that
/// transaction's insert waits for ours and then finds the rows. No version
/// is allocated: clients pull from version 0 exclusive, so nobody receives
/// the podcast before its first ingest, and each shows its own placeholder.
async fn resolve(
    scope: &mut VersionScope,
    url: &NormalizedFeedUrl,
    create: bool,
) -> Result<PodcastId> {
    let owner = sqlx::query_scalar!(
        "select podcast_id from podcast_alias where normalized_url = $1",
        url.normalized()
    )
    .fetch_optional(scope.conn())
    .await
    .context("look up the feed URL's alias")?;
    if let Some(owner) = owner {
        return Ok(PodcastId::from_uuid(owner));
    }
    let podcast_id = PodcastId::from_feed_url(url);
    if !create {
        return Ok(podcast_id);
    }

    sqlx::query!(
        "insert into podcast (id, feed_url, version) values ($1, $2, 0)
         on conflict do nothing",
        podcast_id.as_uuid(),
        url.feed_url()
    )
    .execute(scope.conn())
    .await
    .context("create the podcast")?;
    sqlx::query!(
        "insert into feed_poll (podcast_id) values ($1) on conflict do nothing",
        podcast_id.as_uuid()
    )
    .execute(scope.conn())
    .await
    .context("create the feed's poll row")?;
    sqlx::query!(
        "insert into podcast_alias (normalized_url, podcast_id) values ($1, $2)
         on conflict do nothing",
        url.normalized(),
        podcast_id.as_uuid()
    )
    .execute(scope.conn())
    .await
    .context("create the feed URL's alias")?;

    // A feed move the poller committed since the lookup above may own the
    // URL now. Following it keeps the subscription on the podcast that
    // every later subscribe to this URL resolves to.
    // TODO: the podcast created above then has no subscriber and stays in
    // the poll set; park or remove it.
    let owner = sqlx::query_scalar!(
        "select podcast_id from podcast_alias where normalized_url = $1",
        url.normalized()
    )
    .fetch_one(scope.conn())
    .await
    .context("read the feed URL's alias owner")?;
    Ok(PodcastId::from_uuid(owner))
}

/// Writes the canonical row with a new version. `replaced_by` is left as
/// stored.
// TODO: a canonical row that itself carries `replaced_by` (a chain, which
// only clients that derived ids differently can cause) isn't followed.
async fn write_canonical(
    scope: &mut VersionScope,
    user_id: UserId,
    podcast_id: PodcastId,
    fields: &Fields,
) -> Result<()> {
    let version = scope.next_version();
    sqlx::query!(
        "insert into subscription
             (user_id, podcast_id, feed_url, subscribed, subscribed_at, subscribed_dev, version)
         values ($1, $2, $3, $4, $5, $6, $7)
         on conflict (user_id, podcast_id) do update set
             feed_url = excluded.feed_url, subscribed = excluded.subscribed,
             subscribed_at = excluded.subscribed_at, subscribed_dev = excluded.subscribed_dev,
             version = excluded.version",
        user_id.as_uuid(),
        podcast_id.as_uuid(),
        fields.feed_url,
        fields.subscribed,
        SqlTimestamp(fields.stamp.at) as _,
        fields.stamp.dev.as_uuid(),
        version
    )
    .execute(scope.conn())
    .await
    .context("upsert the subscription")?;
    Ok(())
}

/// Writes X as `subscribed = false, replaced_by = Y` with a new version.
///
/// Its stamp is the lowest there is (the Unix epoch and the nil device), so
/// X's row never wins a client's merge. A client acting on the redirect
/// merges its local X into Y (spec 5.5); if the pulled X carried a real
/// stamp, its `subscribed = false` could beat Y's state on that device
/// whenever Y's row arrived first, and the device would show Y
/// unsubscribed while the server has it subscribed.
async fn write_redirect(
    scope: &mut VersionScope,
    user_id: UserId,
    podcast_id: PodcastId,
    url: &NormalizedFeedUrl,
    canonical: PodcastId,
) -> Result<()> {
    let unstamped = Stamp {
        at: Timestamp::from_epoch_ms(0).expect("the epoch is in range"),
        dev: DeviceId::from_uuid(Uuid::nil()),
    };
    let version = scope.next_version();
    sqlx::query!(
        "insert into subscription
             (user_id, podcast_id, feed_url, subscribed, subscribed_at, subscribed_dev,
              replaced_by, version)
         values ($1, $2, $3, false, $4, $5, $6, $7)
         on conflict (user_id, podcast_id) do update set
             feed_url = excluded.feed_url, subscribed = false,
             subscribed_at = excluded.subscribed_at, subscribed_dev = excluded.subscribed_dev,
             replaced_by = excluded.replaced_by, version = excluded.version",
        user_id.as_uuid(),
        podcast_id.as_uuid(),
        url.feed_url(),
        SqlTimestamp(unstamped.at) as _,
        unstamped.dev.as_uuid(),
        canonical.as_uuid(),
        version
    )
    .execute(scope.conn())
    .await
    .context("upsert the redirected subscription")?;
    Ok(())
}
