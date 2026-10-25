//! `subscription.set` (spec 4.4): subscribes that create the catalog rows
//! and schedule a poll, unsubscribes that park an orphaned feed, the
//! `feed_poll` lock that keeps the two consistent across users, and alias
//! redirects.

mod common;

use pod_protocol::{
    DeviceId, Mutation, MutationId, MutationOutcome, MutationResult, PodcastId, RejectionCode,
    Subscription, SubscriptionSet, Timestamp, UserId, normalize_feed_url,
};
use pod_server::db::SqlTimestamp;
use pod_server::fetch::{Allowlist, FetchPolicy};
use pod_server::sync::apply_batch;
use sqlx::PgPool;

// ==============================================================================
// Helpers
// ==============================================================================

const FEED: &str = "https://example.com/feed.xml";
/// Where the feed at [`MOVED_TO`] used to live, recorded as its alias.
const MOVED_FROM: &str = "https://old.example.com/feed.xml";
const MOVED_TO: &str = "https://new.example.com/feed.xml";

fn podcast_id(feed_url: &str) -> PodcastId {
    PodcastId::from_feed_url(&normalize_feed_url(feed_url).expect("test URLs are valid"))
}

fn plus_ms(at: Timestamp, ms: i64) -> Timestamp {
    Timestamp::from_epoch_ms(at.as_epoch_ms() + ms).expect("tests stay near today")
}

fn phone() -> DeviceId {
    "019a3f10-8c2e-7a41-9b3d-5e6f7a8b9c0d"
        .parse()
        .expect("literal is a UUID")
}

fn subscription_set(
    podcast_id: PodcastId,
    feed_url: &str,
    subscribed: bool,
    at: Timestamp,
) -> Mutation {
    Mutation::SubscriptionSet(SubscriptionSet {
        id: MutationId::new_v7(),
        at,
        dev: phone(),
        podcast_id,
        feed_url: feed_url.to_owned(),
        subscribed,
    })
}

/// Applies one mutation as a sync request from `user_id` would, with the
/// production fetch policy and no allowlist.
async fn apply(
    pool: &PgPool,
    user_id: UserId,
    mutation: Mutation,
    now: Timestamp,
) -> MutationOutcome {
    apply_with(
        pool,
        user_id,
        mutation,
        now,
        &FetchPolicy::new(Allowlist::default()),
    )
    .await
}

async fn apply_with(
    pool: &PgPool,
    user_id: UserId,
    mutation: Mutation,
    now: Timestamp,
    policy: &FetchPolicy,
) -> MutationOutcome {
    let results = apply_batch(pool, user_id, &[Ok(mutation)], now, policy)
        .await
        .expect("the batch applies");
    let [MutationResult { ref outcome, .. }] = results[..] else {
        panic!("one mutation has one result: {results:?}");
    };
    outcome.clone()
}

async fn subscription(
    pool: &PgPool,
    user_id: UserId,
    podcast_id: PodcastId,
) -> Option<Subscription> {
    let row = sqlx::query!(
        r#"select feed_url, subscribed, subscribed_at as "subscribed_at: SqlTimestamp",
                  subscribed_dev, replaced_by, version
           from subscription where user_id = $1 and podcast_id = $2"#,
        user_id.as_uuid(),
        podcast_id.as_uuid()
    )
    .fetch_optional(pool)
    .await
    .expect("subscription is readable")?;
    Some(Subscription {
        podcast_id,
        feed_url: row.feed_url,
        subscribed: row.subscribed,
        subscribed_at: row.subscribed_at.0,
        subscribed_dev: DeviceId::from_uuid(row.subscribed_dev),
        replaced_by: row.replaced_by.map(PodcastId::from_uuid),
        version: row.version,
    })
}

/// When the feed is next due, coarsely: the tests only need to tell a feed
/// scheduled right away from one parked at `infinity`.
#[derive(Debug, PartialEq, Eq)]
enum Poll {
    Due,
    Later,
    Parked,
}

async fn poll(pool: &PgPool, podcast_id: PodcastId) -> Option<Poll> {
    let row = sqlx::query!(
        r#"select next_poll_at = 'infinity' as "parked!", next_poll_at <= now() as "due!"
           from feed_poll where podcast_id = $1"#,
        podcast_id.as_uuid()
    )
    .fetch_optional(pool)
    .await
    .expect("feed_poll is readable")?;
    Some(if row.parked {
        Poll::Parked
    } else if row.due {
        Poll::Due
    } else {
        Poll::Later
    })
}

async fn set_poll(pool: &PgPool, podcast_id: PodcastId, next_poll_at: &str) {
    sqlx::query!(
        "update feed_poll set next_poll_at = $2::text::timestamptz where podcast_id = $1",
        podcast_id.as_uuid(),
        next_poll_at
    )
    .execute(pool)
    .await
    .expect("feed_poll is writable");
}

async fn podcast_row(pool: &PgPool, podcast_id: PodcastId) -> Option<(String, String, i64, i64)> {
    sqlx::query!(
        "select feed_url, status, version, version_counter from podcast where id = $1",
        podcast_id.as_uuid()
    )
    .fetch_optional(pool)
    .await
    .expect("podcast is readable")
    .map(|row| (row.feed_url, row.status, row.version, row.version_counter))
}

async fn alias_owner(pool: &PgPool, feed_url: &str) -> Option<PodcastId> {
    let url = normalize_feed_url(feed_url).expect("test URLs are valid");
    sqlx::query_scalar!(
        "select podcast_id from podcast_alias where normalized_url = $1",
        url.normalized()
    )
    .fetch_optional(pool)
    .await
    .expect("podcast_alias is readable")
    .map(PodcastId::from_uuid)
}

async fn version_counter(pool: &PgPool, user_id: UserId) -> i64 {
    sqlx::query_scalar!(
        "select version_counter from users where id = $1",
        user_id.as_uuid()
    )
    .fetch_one(pool)
    .await
    .expect("the user exists")
}

/// The podcast at [`MOVED_TO`], with [`MOVED_FROM`] recorded as its alias the
/// way the poller records a feed move.
async fn moved_feed(pool: &PgPool) -> PodcastId {
    let canonical = common::create_podcast(pool, MOVED_TO).await;
    let old = normalize_feed_url(MOVED_FROM).expect("test URLs are valid");
    sqlx::query!(
        "insert into podcast_alias (normalized_url, podcast_id) values ($1, $2)",
        old.normalized(),
        canonical.as_uuid()
    )
    .execute(pool)
    .await
    .expect("the alias is new");
    canonical
}

// ==============================================================================
// Subscribe
// ==============================================================================

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn subscribe_creates_a_pending_podcast_and_schedules_a_poll(pool: PgPool) {
    let user = common::create_user(&pool, "alice").await;
    let now = Timestamp::now();
    let id = podcast_id(FEED);

    // The client sends what the user typed; the server stores it normalized.
    let typed = "  HTTPS://EXAMPLE.com:443/feed.xml#latest ";
    let outcome = apply(&pool, user, subscription_set(id, typed, true, now), now).await;

    assert_eq!(outcome, MutationOutcome::Applied { at: now });
    assert_eq!(
        subscription(&pool, user, id).await,
        Some(Subscription {
            podcast_id: id,
            feed_url: FEED.to_owned(),
            subscribed: true,
            subscribed_at: now,
            subscribed_dev: phone(),
            replaced_by: None,
            version: 1,
        })
    );
    assert_eq!(
        podcast_row(&pool, id).await,
        Some((FEED.to_owned(), "pending".to_owned(), 0, 0)),
        "a pending podcast at version 0, no podcast-scope version allocated"
    );
    assert_eq!(poll(&pool, id).await, Some(Poll::Due));
    assert_eq!(alias_owner(&pool, FEED).await, Some(id));
    assert_eq!(version_counter(&pool, user).await, 1);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn subscribe_keeps_the_original_scheme_and_hashes_the_https_form(pool: PgPool) {
    let user = common::create_user(&pool, "alice").await;
    let now = Timestamp::now();
    let http = "http://example.com/feed.xml";
    let id = podcast_id(http);
    assert_eq!(id, podcast_id(FEED), "http and https share an id");

    let outcome = apply(&pool, user, subscription_set(id, http, true, now), now).await;

    assert_eq!(outcome, MutationOutcome::Applied { at: now });
    let row = subscription(&pool, user, id).await.expect("subscribed");
    assert_eq!(row.feed_url, http);
    assert_eq!(podcast_row(&pool, id).await.expect("created").0, http);
    assert_eq!(alias_owner(&pool, FEED).await, Some(id));
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn subscribe_reschedules_a_parked_feed(pool: PgPool) {
    let alice = common::create_user(&pool, "alice").await;
    let id = common::create_podcast(&pool, FEED).await;
    sqlx::query!(
        "update podcast set status = 'gone' where id = $1",
        id.as_uuid()
    )
    .execute(&pool)
    .await
    .expect("podcast is writable");
    set_poll(&pool, id, "infinity").await;
    let now = Timestamp::now();

    let outcome = apply(&pool, alice, subscription_set(id, FEED, true, now), now).await;

    assert_eq!(outcome, MutationOutcome::Applied { at: now });
    assert_eq!(
        poll(&pool, id).await,
        Some(Poll::Due),
        "a gone feed is polled again"
    );
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn subscribe_while_already_subscribed_leaves_the_schedule_alone(pool: PgPool) {
    let alice = common::create_user(&pool, "alice").await;
    let now = Timestamp::now();
    let id = podcast_id(FEED);
    apply(
        &pool,
        alice,
        subscription_set(id, FEED, true, plus_ms(now, -1_000)),
        now,
    )
    .await;
    set_poll(&pool, id, "2100-01-01T00:00:00Z").await;

    let outcome = apply(&pool, alice, subscription_set(id, FEED, true, now), now).await;

    assert_eq!(outcome, MutationOutcome::Applied { at: now });
    assert_eq!(poll(&pool, id).await, Some(Poll::Later));
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn invalid_and_refused_feed_urls_are_rejected(pool: PgPool) {
    let user = common::create_user(&pool, "alice").await;
    let now = Timestamp::now();
    let refused = [
        "not a url",
        "ftp://example.com/feed.xml",
        "https://example.com:8443/feed.xml",
        "http://127.0.0.1/feed.xml",
        "http://10.1.2.3/feed.xml",
        "http://[::1]/feed.xml",
        "http://169.254.169.254/latest/meta-data",
    ];
    for feed_url in refused {
        let id = podcast_id(FEED);
        let outcome = apply(&pool, user, subscription_set(id, feed_url, true, now), now).await;
        assert_eq!(
            outcome,
            MutationOutcome::Rejected {
                at: Some(now),
                code: RejectionCode::InvalidFeedUrl,
            },
            "{feed_url}"
        );
    }
    let podcasts = sqlx::query_scalar!(r#"select count(*) as "count!" from podcast"#)
        .fetch_one(&pool)
        .await
        .expect("podcast is readable");
    assert_eq!(podcasts, 0);
    assert_eq!(version_counter(&pool, user).await, 0);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn an_allowlisted_lan_address_is_accepted(pool: PgPool) {
    let user = common::create_user(&pool, "alice").await;
    let now = Timestamp::now();
    let feed_url = "http://10.1.2.3/feed.xml";
    let policy = FetchPolicy::new(Allowlist::parse(&["10.0.0.0/8"]).expect("valid allowlist"));

    let mutation = subscription_set(podcast_id(feed_url), feed_url, true, now);
    let outcome = apply_with(&pool, user, mutation, now, &policy).await;

    assert_eq!(outcome, MutationOutcome::Applied { at: now });
}

// ==============================================================================
// Unsubscribe
// ==============================================================================

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn the_last_unsubscribe_parks_the_feed(pool: PgPool) {
    let alice = common::create_user(&pool, "alice").await;
    let bob = common::create_user(&pool, "bob").await;
    let now = Timestamp::now();
    let id = podcast_id(FEED);
    let earlier = plus_ms(now, -1_000);
    apply(&pool, alice, subscription_set(id, FEED, true, earlier), now).await;
    apply(&pool, bob, subscription_set(id, FEED, true, earlier), now).await;

    let outcome = apply(&pool, alice, subscription_set(id, FEED, false, now), now).await;
    assert_eq!(outcome, MutationOutcome::Applied { at: now });
    assert_eq!(
        poll(&pool, id).await,
        Some(Poll::Due),
        "bob still subscribes"
    );

    apply(&pool, bob, subscription_set(id, FEED, false, now), now).await;
    assert_eq!(poll(&pool, id).await, Some(Poll::Parked));
    let row = subscription(&pool, bob, id).await.expect("bob has a row");
    assert!(!row.subscribed);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_lost_subscribe_does_not_revive_a_parked_feed(pool: PgPool) {
    let alice = common::create_user(&pool, "alice").await;
    let now = Timestamp::now();
    let id = podcast_id(FEED);
    apply(
        &pool,
        alice,
        subscription_set(id, FEED, true, plus_ms(now, -2_000)),
        now,
    )
    .await;
    apply(&pool, alice, subscription_set(id, FEED, false, now), now).await;

    let stale = subscription_set(id, FEED, true, plus_ms(now, -1_000));
    let outcome = apply(&pool, alice, stale, now).await;

    assert!(
        matches!(outcome, MutationOutcome::Superseded { .. }),
        "{outcome:?}"
    );
    assert_eq!(poll(&pool, id).await, Some(Poll::Parked));
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn unsubscribing_a_never_subscribed_podcast_stores_the_row(pool: PgPool) {
    let alice = common::create_user(&pool, "alice").await;
    let now = Timestamp::now();
    let id = podcast_id(FEED);

    let outcome = apply(&pool, alice, subscription_set(id, FEED, false, now), now).await;

    assert_eq!(outcome, MutationOutcome::Applied { at: now });
    let row = subscription(&pool, alice, id)
        .await
        .expect("the row is stored");
    assert!(!row.subscribed);
    assert_eq!(row.version, 1);
    assert_eq!(
        podcast_row(&pool, id).await,
        None,
        "an unsubscribe creates no podcast"
    );
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn an_unsubscribe_is_not_refused_by_the_fetch_guard(pool: PgPool) {
    let alice = common::create_user(&pool, "alice").await;
    let now = Timestamp::now();
    let feed_url = "http://10.1.2.3/feed.xml";
    let id = podcast_id(feed_url);

    let outcome = apply(
        &pool,
        alice,
        subscription_set(id, feed_url, false, now),
        now,
    )
    .await;

    assert_eq!(outcome, MutationOutcome::Applied { at: now });
}

// ==============================================================================
// Alias redirect
// ==============================================================================

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn subscribe_via_an_alias_writes_the_canonical_row_and_redirects_the_alias(pool: PgPool) {
    let alice = common::create_user(&pool, "alice").await;
    let canonical = moved_feed(&pool).await;
    set_poll(&pool, canonical, "infinity").await;
    let alias = podcast_id(MOVED_FROM);
    let now = Timestamp::now();

    let outcome = apply(
        &pool,
        alice,
        subscription_set(alias, MOVED_FROM, true, now),
        now,
    )
    .await;

    assert_eq!(outcome, MutationOutcome::Applied { at: now });
    let y = subscription(&pool, alice, canonical)
        .await
        .expect("Y is written");
    assert_eq!(
        (
            y.subscribed,
            y.subscribed_at,
            y.subscribed_dev,
            y.replaced_by
        ),
        (true, now, phone(), None)
    );
    assert_eq!(y.feed_url, MOVED_FROM, "the URL the user subscribed with");
    let x = subscription(&pool, alice, alias)
        .await
        .expect("X is written");
    assert!(!x.subscribed);
    assert_eq!(x.replaced_by, Some(canonical));
    assert_eq!(
        (x.subscribed_at.as_epoch_ms(), x.subscribed_dev.to_string()),
        (0, "00000000-0000-0000-0000-000000000000".to_owned()),
        "X's stamp loses every client merge"
    );
    let mut versions = [y.version, x.version];
    versions.sort_unstable();
    assert_eq!(versions, [1, 2]);
    assert_eq!(poll(&pool, canonical).await, Some(Poll::Due));
    assert_eq!(
        podcast_row(&pool, alias).await,
        None,
        "X has no podcast row"
    );
    assert_eq!(poll(&pool, alias).await, None, "nor a poll row");
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn an_unsubscribe_for_the_alias_after_the_redirect_unsubscribes_the_canonical(pool: PgPool) {
    let alice = common::create_user(&pool, "alice").await;
    let canonical = moved_feed(&pool).await;
    let alias = podcast_id(MOVED_FROM);
    let now = Timestamp::now();
    let earlier = plus_ms(now, -1_000);
    apply(
        &pool,
        alice,
        subscription_set(alias, MOVED_FROM, true, earlier),
        now,
    )
    .await;

    // A device that queued the unsubscribe before it learned about the
    // redirect still names X.
    let outcome = apply(
        &pool,
        alice,
        subscription_set(alias, MOVED_FROM, false, now),
        now,
    )
    .await;

    assert_eq!(outcome, MutationOutcome::Applied { at: now });
    let y = subscription(&pool, alice, canonical)
        .await
        .expect("Y exists");
    assert_eq!((y.subscribed, y.subscribed_at), (false, now));
    let x = subscription(&pool, alice, alias).await.expect("X exists");
    assert_eq!((x.subscribed, x.replaced_by), (false, Some(canonical)));
    assert_eq!([y.version, x.version].into_iter().max(), Some(4));
    assert_eq!(poll(&pool, canonical).await, Some(Poll::Parked));
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn the_canonical_row_is_resent_even_when_the_subscribe_loses(pool: PgPool) {
    let alice = common::create_user(&pool, "alice").await;
    let canonical = moved_feed(&pool).await;
    let alias = podcast_id(MOVED_FROM);
    let now = Timestamp::now();
    // Another device unsubscribed from the canonical podcast after this
    // device subscribed through the old URL.
    apply(
        &pool,
        alice,
        subscription_set(canonical, MOVED_TO, false, now),
        now,
    )
    .await;
    let before = subscription(&pool, alice, canonical)
        .await
        .expect("Y exists");

    let stale = subscription_set(alias, MOVED_FROM, true, plus_ms(now, -1_000));
    let outcome = apply(&pool, alice, stale, now).await;

    assert_eq!(
        outcome,
        MutationOutcome::Superseded {
            at: plus_ms(now, -1_000)
        }
    );
    let after = subscription(&pool, alice, canonical)
        .await
        .expect("Y exists");
    assert!(after.version > before.version, "Y takes a new version");
    assert_eq!(
        Subscription {
            version: before.version,
            ..after
        },
        before,
        "but keeps its fields"
    );
    let x = subscription(&pool, alice, alias)
        .await
        .expect("X is written");
    assert_eq!(x.replaced_by, Some(canonical));
    assert_eq!(poll(&pool, canonical).await, Some(Poll::Parked));
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_podcast_id_that_disagrees_with_the_url_redirects_to_the_computed_id(pool: PgPool) {
    let alice = common::create_user(&pool, "alice").await;
    let now = Timestamp::now();
    let client_id = podcast_id("https://typo.example.com/feed.xml");
    let computed = podcast_id(FEED);

    let outcome = apply(
        &pool,
        alice,
        subscription_set(client_id, FEED, true, now),
        now,
    )
    .await;

    assert_eq!(outcome, MutationOutcome::Applied { at: now });
    let y = subscription(&pool, alice, computed)
        .await
        .expect("Y is written");
    assert!(y.subscribed);
    let x = subscription(&pool, alice, client_id)
        .await
        .expect("X is written");
    assert_eq!((x.subscribed, x.replaced_by), (false, Some(computed)));
    assert_eq!(podcast_row(&pool, client_id).await, None);
    assert_eq!(poll(&pool, computed).await, Some(Poll::Due));
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_redirected_row_follows_replaced_by_whatever_url_the_mutation_names(pool: PgPool) {
    let alice = common::create_user(&pool, "alice").await;
    let canonical = moved_feed(&pool).await;
    let alias = podcast_id(MOVED_FROM);
    let now = Timestamp::now();
    apply(
        &pool,
        alice,
        subscription_set(alias, MOVED_FROM, true, plus_ms(now, -1_000)),
        now,
    )
    .await;

    let elsewhere = "https://elsewhere.example.com/feed.xml";
    let outcome = apply(
        &pool,
        alice,
        subscription_set(alias, elsewhere, false, now),
        now,
    )
    .await;

    assert_eq!(outcome, MutationOutcome::Applied { at: now });
    let y = subscription(&pool, alice, canonical)
        .await
        .expect("Y exists");
    assert!(!y.subscribed);
    assert_eq!(
        subscription(&pool, alice, podcast_id(elsewhere)).await,
        None
    );
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_redirect_carries_the_alias_rows_own_choice_into_the_canonical(pool: PgPool) {
    let alice = common::create_user(&pool, "alice").await;
    let now = Timestamp::now();
    let id = podcast_id(FEED);
    let subscribed_at = plus_ms(now, -1_000);
    apply(
        &pool,
        alice,
        subscription_set(id, FEED, true, subscribed_at),
        now,
    )
    .await;
    // FEED turns out to be an alias of another podcast after alice
    // subscribed to it under its own id.
    let canonical = common::create_podcast(&pool, MOVED_TO).await;
    let url = normalize_feed_url(FEED).expect("test URLs are valid");
    sqlx::query!(
        "update podcast_alias set podcast_id = $2 where normalized_url = $1",
        url.normalized(),
        canonical.as_uuid()
    )
    .execute(&pool)
    .await
    .expect("podcast_alias is writable");

    let older = subscription_set(id, FEED, false, plus_ms(now, -2_000));
    let outcome = apply(&pool, alice, older, now).await;

    assert_eq!(
        outcome,
        MutationOutcome::Superseded {
            at: plus_ms(now, -2_000)
        }
    );
    let y = subscription(&pool, alice, canonical)
        .await
        .expect("Y is written");
    assert_eq!((y.subscribed, y.subscribed_at), (true, subscribed_at));
}

/// A phone running fast subscribes to FEED (X) and then unsubscribes the
/// canonical podcast (Y) in one batch, and the server clamps both to the same
/// millisecond. FEED later turns out to be an alias of Y. On the next
/// redirect X's row ties Y's exactly, and Y keeps its own, later choice
/// (spec 4.4 "Alias redirect" step 1).
#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_redirect_keeps_the_canonical_row_on_a_clamped_same_device_tie(pool: PgPool) {
    let alice = common::create_user(&pool, "alice").await;
    let now = Timestamp::now();
    let x = podcast_id(FEED);
    let canonical = common::create_podcast(&pool, MOVED_TO).await;
    let fast = plus_ms(now, 120_000);
    let clamped = plus_ms(now, 60_000);
    let results = apply_batch(
        &pool,
        alice,
        &[
            Ok(subscription_set(x, FEED, true, fast)),
            Ok(subscription_set(canonical, MOVED_TO, false, fast)),
        ],
        now,
        &FetchPolicy::new(Allowlist::default()),
    )
    .await
    .expect("the batch applies");
    assert!(
        results
            .iter()
            .all(|result| result.outcome == MutationOutcome::Applied { at: clamped }),
        "{results:?}"
    );
    let url = normalize_feed_url(FEED).expect("test URLs are valid");
    sqlx::query!(
        "update podcast_alias set podcast_id = $2 where normalized_url = $1",
        url.normalized(),
        canonical.as_uuid()
    )
    .execute(&pool)
    .await
    .expect("podcast_alias is writable");

    let older = subscription_set(x, FEED, true, plus_ms(now, -1_000));
    let outcome = apply(&pool, alice, older, now).await;

    assert_eq!(
        outcome,
        MutationOutcome::Superseded {
            at: plus_ms(now, -1_000)
        }
    );
    let y = subscription(&pool, alice, canonical)
        .await
        .expect("Y is written");
    assert_eq!(
        (y.subscribed, y.feed_url.as_str(), y.subscribed_at),
        (false, MOVED_TO, clamped)
    );
}

// ==============================================================================
// Concurrency
// ==============================================================================

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn two_users_subscribing_to_a_new_feed_at_once_both_succeed(pool: PgPool) {
    let alice = common::create_user(&pool, "alice").await;
    let bob = common::create_user(&pool, "bob").await;
    let id = podcast_id(FEED);
    let now = Timestamp::now();

    // An uncommitted podcast row with the feed's id makes both subscribes
    // block on their own insert of it. Rolling it back releases them into
    // the race where one inserts the catalog rows and the other finds them.
    let mut holder = pool.begin().await.expect("a transaction starts");
    sqlx::query!(
        "insert into podcast (id, feed_url, version) values ($1, $2, 0)",
        id.as_uuid(),
        FEED
    )
    .execute(&mut *holder)
    .await
    .expect("the podcast is new");
    let tasks: Vec<_> = [alice, bob]
        .into_iter()
        .map(|user| {
            let pool = pool.clone();
            tokio::spawn(async move {
                apply(&pool, user, subscription_set(id, FEED, true, now), now).await
            })
        })
        .collect();
    common::wait_for_lock_waiters(&pool, 2).await;
    holder.rollback().await.expect("the holder rolls back");

    for task in tasks {
        let outcome = task.await.expect("the task doesn't panic");
        assert_eq!(outcome, MutationOutcome::Applied { at: now });
    }
    for user in [alice, bob] {
        assert!(
            subscription(&pool, user, id)
                .await
                .expect("subscribed")
                .subscribed
        );
    }
    let counts = sqlx::query!(
        r#"select (select count(*) from podcast) as "podcasts!",
                  (select count(*) from feed_poll) as "polls!",
                  (select count(*) from podcast_alias) as "aliases!""#
    )
    .fetch_one(&pool)
    .await
    .expect("the catalog is readable");
    assert_eq!((counts.podcasts, counts.polls, counts.aliases), (1, 1, 1));
    assert_eq!(poll(&pool, id).await, Some(Poll::Due));
}

/// Alice unsubscribes as the last subscriber while Bob subscribes. Both
/// queue on the feed's `feed_poll` row lock, held here by a third
/// transaction, and are released in `order`. Either way the feed must end
/// up polled.
async fn last_unsubscribe_races_a_subscribe(pool: PgPool, alice_first: bool) {
    let alice = common::create_user(&pool, "alice").await;
    let bob = common::create_user(&pool, "bob").await;
    let id = podcast_id(FEED);
    let now = Timestamp::now();
    apply(
        &pool,
        alice,
        subscription_set(id, FEED, true, plus_ms(now, -1_000)),
        now,
    )
    .await;

    let mut holder = pool.begin().await.expect("a transaction starts");
    sqlx::query!(
        "select podcast_id from feed_poll where podcast_id = $1 for update",
        id.as_uuid()
    )
    .fetch_one(&mut *holder)
    .await
    .expect("the feed has a poll row");
    let unsubscribe = (alice, subscription_set(id, FEED, false, now));
    let subscribe = (bob, subscription_set(id, FEED, true, now));
    let order = if alice_first {
        [unsubscribe, subscribe]
    } else {
        [subscribe, unsubscribe]
    };
    let mut tasks = Vec::new();
    for (waiting, (user, mutation)) in (1..).zip(order) {
        let task_pool = pool.clone();
        tasks.push(tokio::spawn(async move {
            apply(&task_pool, user, mutation, now).await
        }));
        // Postgres grants a row lock to its waiters in arrival order.
        common::wait_for_lock_waiters(&pool, waiting).await;
    }
    holder.commit().await.expect("the holder commits");

    for task in tasks {
        let outcome = task.await.expect("the task doesn't panic");
        assert_eq!(outcome, MutationOutcome::Applied { at: now });
    }
    assert_eq!(poll(&pool, id).await, Some(Poll::Due));
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_subscribe_after_a_concurrent_last_unsubscribe_keeps_the_feed_polled(pool: PgPool) {
    last_unsubscribe_races_a_subscribe(pool, true).await;
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_last_unsubscribe_after_a_concurrent_subscribe_keeps_the_feed_polled(pool: PgPool) {
    last_unsubscribe_races_a_subscribe(pool, false).await;
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn an_allowlisted_lan_host_may_use_any_port(pool: PgPool) {
    let user = common::create_user(&pool, "alice").await;
    let now = Timestamp::now();
    let feed_url = "http://nas.lan:8080/feed.xml";
    let mutation = || subscription_set(podcast_id(feed_url), feed_url, true, now);

    let refused = apply(&pool, user, mutation(), now).await;
    assert_eq!(
        refused,
        MutationOutcome::Rejected {
            at: Some(now),
            code: RejectionCode::InvalidFeedUrl,
        }
    );

    let policy = FetchPolicy::new(Allowlist::parse(&["nas.lan"]).expect("valid allowlist"));
    let outcome = apply_with(&pool, user, mutation(), now, &policy).await;
    assert_eq!(outcome, MutationOutcome::Applied { at: now });
}
