//! Offline subscribe and placeholders (spec 5.5).
//!
//! [`subscribe`] works without a network: one local transaction writes a
//! `pending` placeholder, the subscription, its outbox mutation and a scope
//! at 0, so the show appears in the sidebar at once and fills in once a sync
//! reaches the server and the poller has fetched the feed.
//!
//! The sync apply step settles a subscribe in one of two ways the server
//! decides:
//!
//! - [`follow_replacement`]: the URL is an alias of a podcast the server
//!   already knows, so the pulled row for the id the client derived (X)
//!   names the canonical podcast (Y) in `replaced_by`. The client moves
//!   everything it holds for X over to Y.
//! - [`forget_rejected_subscribe`]: the server refuses the URL
//!   (`invalid_feed_url`), which it can do on grounds the client can't
//!   check, such as a blocked address. The placeholder goes away and
//!   [`load_failures`] gives the UI the "Couldn't add" message.
//!
//! Both take the apply step's transaction and nothing else, so the
//! conformance fixtures can drive them without HTTP.

use color_eyre::eyre::{OptionExt as _, Result, WrapErr};
use pod_protocol::local::{LocalSubscription, ScopeKey};
use pod_protocol::{
    DirectoryResult, Mutation, MutationId, NormalizedFeedUrl, PodcastId, SubscriptionSet,
    Timestamp, normalize_feed_url,
};
use rusqlite::{Connection, Transaction, params};

use super::SyncHandle;
use super::apply::load_subscription;
use super::local_write::{self, LocalChange};
use crate::store::Store;

// ==============================================================================
// Subscribing locally
// ==============================================================================

/// What a directory search result says about a show, shown on its
/// placeholder while the server hasn't fetched the feed yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryHint {
    pub title: String,
    pub artwork_url: Option<String>,
}

impl From<&DirectoryResult> for DirectoryHint {
    fn from(result: &DirectoryResult) -> Self {
        Self {
            title: result.title.clone(),
            artwork_url: result.artwork_url.clone(),
        }
    }
}

/// Subscribes to `feed_url` locally and asks the sync engine to send it.
/// Returns the podcast id derived from the URL.
///
/// The URL arrives normalized: a URL that doesn't normalize is the caller's
/// inline error (spec 5.5 step 1), and no placeholder or mutation exists for
/// it.
pub async fn subscribe(
    store: &Store,
    sync: &SyncHandle,
    feed_url: &NormalizedFeedUrl,
    hint: Option<DirectoryHint>,
) -> Result<PodcastId> {
    let podcast_id = PodcastId::from_feed_url(feed_url);
    let change = LocalChange::Subscription {
        podcast_id,
        feed_url: feed_url.feed_url().to_owned(),
        subscribed: true,
    };
    let delay = change.sync_delay();
    let stored_url = feed_url.feed_url().to_owned();
    let (title, artwork_url) =
        hint.map_or((None, None), |hint| (Some(hint.title), hint.artwork_url));
    let handle = sync.clone();
    store
        .write(move |transaction| {
            // A podcast row that is already there came from a sync and
            // stays: it shows more than a placeholder can.
            transaction
                .execute(
                    "insert into podcast (id, feed_url, status, version, local_title, local_artwork_url)
                     values (?1, ?2, 'pending', 0, ?3, ?4)
                     on conflict (id) do nothing",
                    params![podcast_id.to_string(), stored_url, title, artwork_url],
                )
                .context("insert the placeholder")?;
            let in_flight = handle.lock_in_flight();
            local_write::apply(
                transaction,
                change,
                MutationId::new_v7(),
                &in_flight,
                Timestamp::now(),
            )
                .context("write the subscription")?;
            transaction
                .execute(
                    "insert into sync_scope (scope, version) values (?1, 0)
                     on conflict (scope) do nothing",
                    [ScopeKey::Podcast(podcast_id).to_string()],
                )
                .context("add the podcast scope")?;
            Ok(())
        })
        .await
        .context("subscribe locally")?;
    sync.request_sync(delay);
    Ok(podcast_id)
}

// ==============================================================================
// Alias replacement
// ==============================================================================

/// Moves what this device holds for `replaced` (X) to `canonical` (Y), after
/// the pull merged a subscription row for X that carries
/// `replaced_by = canonical` (spec 5.5). Runs whatever the merge decided.
pub(super) fn follow_replacement(
    transaction: &Transaction<'_>,
    replaced: PodcastId,
    canonical: PodcastId,
) -> Result<()> {
    // The pull that carried `replaced_by` merged X's row just before.
    let alias = load_subscription(transaction, replaced)
        .context("load the alias row")?
        .ok_or_eyre("the pulled alias row is stored")?;
    let x = replaced.to_string();
    let y = canonical.to_string();

    // 1. Carry X's subscription fields into Y when X's stamp is strictly
    // newer, the server's rule for the same carry (spec 4.4, "Alias
    // redirect" step 1). Not `merge`: its same-device tie rule would let X
    // undo a later mutation for Y that the server clamped into the same
    // millisecond.
    let target = load_subscription(transaction, canonical).context("load the canonical row")?;
    if target.is_none_or(|target| carries(&alias, &target)) {
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
                    y,
                    alias.feed_url,
                    alias.subscribed,
                    alias.subscribed_at.as_epoch_ms(),
                    alias.subscribed_dev.to_string(),
                ],
            )
            .context("carry the subscription over")?;
    }

    // 2. Point queued mutations for X at Y, coalesce key included, so a
    // later action on Y replaces them. Only `subscription.set` names a
    // podcast, and its key is `sub:<id>`. The mutations of the request this
    // page answers already left the outbox with their results; one the
    // server received for X is redirected there (spec 4.4).
    let queued: Vec<(String, String)> = transaction
        .prepare_cached("select mutation_id, payload from outbox where coalesce_key = ?1")
        .context("prepare the outbox query")?
        .query_map([format!("sub:{x}")], |row| Ok((row.get(0)?, row.get(1)?)))
        .context("query the queued mutations")?
        .collect::<rusqlite::Result<_>>()
        .context("read the queued mutations")?;
    for (mutation_id, payload) in queued {
        let mut mutation: Mutation =
            serde_json::from_str(&payload).context("parse a queued mutation")?;
        if let Mutation::SubscriptionSet(subscription) = &mut mutation {
            subscription.podcast_id = canonical;
        }
        transaction
            .execute(
                "update outbox set coalesce_key = ?1, payload = ?2 where mutation_id = ?3",
                params![
                    local_write::coalesce_key(&mutation),
                    serde_json::to_string(&mutation).context("serialize the mutation")?,
                    mutation_id,
                ],
            )
            .context("redirect the queued mutation")?;
    }

    // 3. This device's settings for the show follow it, unless Y has its
    // own.
    transaction
        .execute(
            "insert into podcast_device (podcast_id, notify, auto_keep, auto_on_metered)
                 select ?2, notify, auto_keep, auto_on_metered
                 from podcast_device where podcast_id = ?1
                 on conflict (podcast_id) do nothing",
            params![x, y],
        )
        .context("copy the podcast settings")?;

    // 4. A placeholder becomes Y's while Y has no row of its own, so the
    // sidebar keeps the directory title until Y's first page lands. The
    // pulled podcast row keeps `local_*`, and the scope rule removes the row
    // again should Y end up unsubscribed.
    transaction
        .execute(
            "update podcast set id = ?2
             where id = ?1 and version = 0
               and not exists (select 1 from podcast where id = ?2)",
            params![x, y],
        )
        .context("hand the placeholder over")?;

    // 5. Nothing is left under X. Its subscription row came from this page
    // and is final: X never gets a podcast row on the server. Episodes and
    // the device settings go too, as for any podcast nobody subscribes to;
    // `episode_state` rows stay, since they are user state.
    for (sql, what) in [
        ("delete from podcast where id = ?1", "delete X's podcast"),
        (
            "delete from episode where podcast_id = ?1",
            "delete X's episodes",
        ),
        (
            "delete from podcast_device where podcast_id = ?1",
            "delete X's podcast settings",
        ),
        (
            "delete from subscription where podcast_id = ?1",
            "delete X's subscription",
        ),
    ] {
        transaction.execute(sql, [&x]).context(what)?;
    }
    transaction
        .execute(
            "delete from sync_scope where scope = ?1",
            [ScopeKey::Podcast(replaced).to_string()],
        )
        .context("delete X's scope")?;
    Ok(())
}

/// Whether X's fields replace Y's in [`follow_replacement`]: X's stamp is
/// strictly newer, a later `at` or the same `at` and a greater device.
fn carries(alias: &LocalSubscription, canonical: &LocalSubscription) -> bool {
    (alias.subscribed_at, alias.subscribed_dev)
        > (canonical.subscribed_at, canonical.subscribed_dev)
}

// ==============================================================================
// Rejected subscribes
// ==============================================================================

/// Deletes the placeholder, scope and subscription row of a subscribe the
/// server rejected with `invalid_feed_url` (spec 5.5).
pub(super) fn forget_rejected_subscribe(
    transaction: &Transaction<'_>,
    sent: &SubscriptionSet,
) -> Result<()> {
    if !sent.subscribed {
        // The server never fetch-checks an unsubscribe, and a placeholder
        // only comes with a subscribe.
        return Ok(());
    }
    let x = sent.podcast_id.to_string();
    // A newer local action on the show has replaced the rejected one and
    // stands: it is still in the outbox and gets its own answer.
    let removed = transaction
        .execute(
            "delete from subscription
             where podcast_id = ?1 and subscribed = 1
               and subscribed_at = ?2 and subscribed_dev = ?3",
            params![x, sent.at.as_epoch_ms(), sent.dev.to_string()],
        )
        .context("delete the subscription")?;
    if removed == 0 {
        return Ok(());
    }
    transaction
        .execute("delete from podcast where id = ?1", [&x])
        .context("delete the placeholder")?;
    transaction
        .execute("delete from episode where podcast_id = ?1", [&x])
        .context("delete the episodes")?;
    transaction
        .execute(
            "delete from sync_scope where scope = ?1",
            [ScopeKey::Podcast(sent.podcast_id).to_string()],
        )
        .context("delete the scope")?;
    Ok(())
}

/// A subscribe the server rejected, from the rejected-mutation log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscribeFailure {
    pub mutation_id: MutationId,
    /// "Couldn't add <url>: <reason>" (spec 5.5).
    pub message: String,
}

/// Every logged subscribe the server rejected as `invalid_feed_url`, oldest
/// first.
pub fn load_failures(connection: &Connection) -> Result<Vec<SubscribeFailure>> {
    let mut statement = connection
        .prepare_cached(
            "select mutation_id, payload from rejected_mutation
             where reason = 'invalid_feed_url'
             order by rejected_at, mutation_id",
        )
        .context("prepare the rejected-subscribe query")?;
    let rows: Vec<(String, String)> = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .context("query the rejected subscribes")?
        .collect::<rusqlite::Result<_>>()
        .context("read the rejected subscribes")?;
    let mut failures = Vec::new();
    for (mutation_id, payload) in rows {
        let Mutation::SubscriptionSet(sent) =
            serde_json::from_str(&payload).context("parse a rejected mutation")?
        else {
            continue;
        };
        // The URL normalized when the user subscribed, so the fallback is
        // for a log written by something else. A private feed's credentials
        // stay out of the message either way.
        let url = normalize_feed_url(&sent.feed_url)
            .map_or_else(|_| "the feed".to_owned(), |url| url.without_credentials());
        failures.push(SubscribeFailure {
            mutation_id: mutation_id.parse().context("parse a mutation id")?,
            // TODO: a more specific reason once rejections carry one; the
            // server logs it but sends only the code (spec 4.3).
            message: format!("Couldn't add {url}: the server won't fetch from this address"),
        });
    }
    Ok(failures)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::scratch_dir;

    const USER: &str = "019a3f10-0000-7a41-9b3d-000000000001";
    const DEVICE: &str = "019a3f10-8c2e-7a41-9b3d-5e6f7a8b9c0d";
    const FEED_URL: &str = "https://feeds.example.com/show.xml";

    async fn signed_in_store() -> (tempfile::TempDir, Store) {
        let dir = scratch_dir();
        let store = Store::open(&dir.path().join("pod.db")).expect("the store opens");
        store
            .write(|transaction| {
                transaction
                    .execute(
                        "insert into account (id, server_url, user_id, username, device_id, token)
                         values (1, 'https://pod.example', ?1, 'alice', ?2, 'token')",
                        params![USER, DEVICE],
                    )
                    .context("insert the account")
            })
            .await
            .expect("the account is inserted");
        (dir, store)
    }

    async fn rows(store: &Store, sql: &'static str) -> Vec<String> {
        store
            .read(move |connection| {
                let mut statement = connection.prepare(sql).context("prepare the query")?;
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

    #[tokio::test]
    async fn subscribing_writes_the_placeholder_subscription_mutation_and_scope_at_once() {
        let (_dir, store) = signed_in_store().await;
        let sync = SyncHandle::default();
        let url = normalize_feed_url(FEED_URL).expect("the literal is a valid feed URL");
        let hint = DirectoryHint {
            title: "The Example Show".to_owned(),
            artwork_url: Some("https://cdn.example.com/art.jpg".to_owned()),
        };
        let id = subscribe(&store, &sync, &url, Some(hint))
            .await
            .expect("the subscribe commits");
        assert_eq!(id, PodcastId::from_feed_url(&url));

        assert_eq!(
            rows(
                &store,
                "select id || ' ' || feed_url || ' ' || status || ' ' || version || ' '
                        || local_title || ' ' || local_artwork_url from podcast"
            )
            .await,
            [format!(
                "{id} {FEED_URL} pending 0 The Example Show https://cdn.example.com/art.jpg"
            )]
        );
        assert_eq!(
            rows(
                &store,
                "select podcast_id || ' ' || subscribed || ' ' || subscribed_dev from subscription"
            )
            .await,
            [format!("{id} 1 {DEVICE}")]
        );
        assert_eq!(
            rows(&store, "select coalesce_key from outbox").await,
            [format!("sub:{id}")]
        );
        assert_eq!(
            rows(&store, "select scope || ' ' || version from sync_scope").await,
            [format!("podcast:{id} 0")]
        );
        assert!(
            sync.take_wanted().is_some(),
            "the subscribe asks for a sync"
        );

        // The sidebar lists the placeholder under its directory title.
        let sidebar = store
            .read(|connection| crate::inbox::load_podcasts(connection, Timestamp::now()))
            .await
            .expect("the sidebar query runs");
        assert_eq!(sidebar.len(), 1);
        assert_eq!(sidebar[0].title, "The Example Show");
    }

    #[tokio::test]
    async fn resubscribing_keeps_a_synced_podcast_row() {
        let (_dir, store) = signed_in_store().await;
        let url = normalize_feed_url(FEED_URL).expect("the literal is a valid feed URL");
        let id = PodcastId::from_feed_url(&url);
        store
            .write(move |transaction| {
                transaction
                    .execute(
                        "insert into podcast (id, feed_url, status, title, version)
                         values (?1, ?2, 'ok', 'The Example Show', 12)",
                        params![id.to_string(), FEED_URL],
                    )
                    .context("insert the synced podcast")
            })
            .await
            .expect("the seed commits");
        subscribe(&store, &SyncHandle::default(), &url, None)
            .await
            .expect("the subscribe commits");
        assert_eq!(
            rows(&store, "select status || ' ' || version from podcast").await,
            ["ok 12"]
        );
    }
}
