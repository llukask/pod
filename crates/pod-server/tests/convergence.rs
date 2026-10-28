//! The spec 9 property test: under concurrent ingests and mutation batches,
//! every client that syncs ends up with exactly the server's rows.
//!
//! The simulation runs three kinds of actor against one Postgres at once:
//!
//! - Ingest actors, one per feed, keep editing a feed (new episodes, edited
//!   episodes, new metadata) and ingest it, flip the podcast to `failing`,
//!   and purge episodes. Each write takes podcast-scope versions.
//! - Clients, several devices per user, make local writes (positions,
//!   played, archived, settings, subscribes and unsubscribes, some through
//!   alias URLs) and sync at random moments: full 5.3 loops, single
//!   push+pull rounds, push-only requests and pull-only requests. The
//!   pull-only ones take no lock, which is the case the snapshot rule of
//!   spec 2.1 protects.
//! - Postgres, which serializes the writers per scope and lets pulls run
//!   beside them.
//!
//! When the budget runs out the actors stop, every client syncs until its
//! outbox drains, then every client syncs again to pick up the others'
//! last writes, and each is compared with the server's tables
//! ([`server_view::differences`]).
//!
//! Every actor draws its choices from its own generator seeded from one
//! `u64`, printed at the start and in every failure. Rerunning with
//! `POD_CONVERGENCE_SEED=<seed>` repeats every actor's choices. It can't
//! repeat the interleaving or the wall clock, so a scheduling-dependent
//! failure may need a few runs (or a longer `POD_CONVERGENCE_SOAK_SECS`) to
//! come back.
//!
//! The tests at the bottom prove the property can fail: the same client
//! model loses a row when the pull reads its tables under `READ COMMITTED`
//! with a write committed between two of them.

#[path = "convergence/client.rs"]
mod client;
mod common;
#[path = "convergence/server_view.rs"]
mod server_view;

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use color_eyre::eyre::Report;
use futures_util::future::join_all;
use pod_protocol::{
    DeviceId, EpisodePosition, FeedError, FeedStatus, Mutation, MutationId, PodcastId,
    SubscriptionSet, Timestamp, normalize_feed_url,
};
use pod_server::feed::ParsedEpisode;
use pod_server::fetch::{Allowlist, FetchPolicy};
use pod_server::ingest::{ingest, purge_episode, set_status};
use pod_server::sync::apply_batch;
use pod_server::sync::pull::{PullIsolation, PullTestHooks};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use sqlx::types::Uuid;
use tokio::time::Instant;

use client::{Client, EpisodeWrite, RequestKind, Server};
use server_view::{ServerView, differences};

// ==============================================================================
// Configuration
// ==============================================================================

/// How long the actors run in the default (CI) configuration.
const DEFAULT_RUN: Duration = Duration::from_secs(10);
const USERS: usize = 3;
const DEVICES_PER_USER: usize = 2;
/// Feeds seeded with a podcast row and one alias URL each. One more feed
/// starts unknown, so its first subscribe creates the podcast while its
/// ingest actor is already trying to write it.
const SEEDED_FEEDS: usize = 4;

#[derive(Debug, Clone)]
struct Config {
    seed: u64,
    run_for: Duration,
    isolation: PullIsolation,
    /// Sleeps this long between two table reads of one scope, which widens
    /// the window a broken isolation leaves open.
    gap: Option<Duration>,
}

impl Config {
    /// Reads `POD_CONVERGENCE_SEED` and `POD_CONVERGENCE_SOAK_SECS`.
    fn from_env(isolation: PullIsolation) -> Self {
        let seed = match std::env::var("POD_CONVERGENCE_SEED") {
            Ok(seed) => seed.parse().expect("POD_CONVERGENCE_SEED is a u64"),
            Err(_) => fastrand::u64(..),
        };
        let run_for = match std::env::var("POD_CONVERGENCE_SOAK_SECS") {
            Ok(secs) => Duration::from_secs(
                secs.parse()
                    .expect("POD_CONVERGENCE_SOAK_SECS is a whole number of seconds"),
            ),
            Err(_) => DEFAULT_RUN,
        };
        Self {
            seed,
            run_for,
            isolation,
            gap: None,
        }
    }

    /// An independent generator per actor, all derived from the seed.
    fn rng(&self, actor: u64) -> fastrand::Rng {
        fastrand::Rng::with_seed(self.seed ^ actor.wrapping_mul(0x9e37_79b9_7f4a_7c15))
    }
}

// ==============================================================================
// The feeds
// ==============================================================================

/// One feed of the simulated catalog.
#[derive(Debug, Clone)]
struct Feed {
    index: usize,
    url: String,
    podcast_id: PodcastId,
    /// An old URL the server already maps to this podcast. Subscribing with
    /// it names a different podcast id, which the server redirects (4.4).
    alias: Option<String>,
}

impl Feed {
    fn urls(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.url.as_str()).chain(self.alias.as_deref())
    }
}

fn podcast_id(url: &str) -> PodcastId {
    PodcastId::from_feed_url(&normalize_feed_url(url).expect("simulated URLs are valid"))
}

async fn seed_feeds(pool: &PgPool) -> Vec<Feed> {
    let mut feeds = Vec::new();
    for index in 0..SEEDED_FEEDS {
        let url = format!("https://feed{index}.example.com/rss.xml");
        let alias = format!("https://old{index}.example.com/rss.xml");
        let podcast_id = common::create_podcast(pool, &url).await;
        sqlx::query("insert into podcast_alias (normalized_url, podcast_id) values ($1, $2)")
            .bind(
                normalize_feed_url(&alias)
                    .expect("simulated URLs are valid")
                    .normalized(),
            )
            .bind(podcast_id.as_uuid())
            .execute(pool)
            .await
            .expect("the alias is new");
        feeds.push(Feed {
            index,
            url,
            podcast_id,
            alias: Some(alias),
        });
    }
    let url = "https://fresh.example.com/rss.xml".to_owned();
    feeds.push(Feed {
        index: SEEDED_FEEDS,
        podcast_id: podcast_id(&url),
        url,
        alias: None,
    });
    feeds
}

// ==============================================================================
// Actors
// ==============================================================================

/// What one actor saw go wrong. Deadlocks are expected (two users'
/// subscribe batches can lock feeds in opposite orders, see SESSION.md) and
/// only counted; any other error fails the run.
#[derive(Debug, Default)]
struct ActorReport {
    deadlocks: u64,
    errors: Vec<String>,
}

impl ActorReport {
    fn record(&mut self, actor: &str, error: Report) {
        let deadlock = error
            .chain()
            .filter_map(|cause| cause.downcast_ref::<sqlx::Error>())
            .filter_map(sqlx::Error::as_database_error)
            .any(|db| db.code().as_deref() == Some("40P01"));
        if deadlock {
            self.deadlocks += 1;
        } else {
            self.errors.push(format!("{actor}: {error:?}"));
        }
    }
}

/// Keeps editing one feed and writing it to the catalog until `deadline`.
async fn ingest_actor(
    pool: &PgPool,
    feed: &Feed,
    mut rng: fastrand::Rng,
    deadline: Instant,
) -> ActorReport {
    let name = format!("ingest {}", feed.index);
    let mut report = ActorReport::default();
    let mut episodes: Vec<ParsedEpisode> = Vec::new();
    let mut title_revision = 0;
    let mut next_guid = 0_i64;
    while Instant::now() < deadline {
        let roll = rng.u32(0..100);
        let outcome = match roll {
            0..45 => {
                for _ in 0..rng.usize(1..=3) {
                    let guid = format!("g{next_guid}");
                    episodes.push(common::parsed_episode(feed.podcast_id, &guid, next_guid));
                    next_guid += 1;
                }
                None
            }
            45..70 if !episodes.is_empty() => {
                let index = rng.usize(..episodes.len());
                let episode = &mut episodes[index];
                episode.title = format!("Episode {} take {}", episode.guid, rng.u32(..));
                None
            }
            70..80 => {
                title_revision += 1;
                None
            }
            80..88 => {
                let outage = FeedStatus::Failing {
                    failing_since: Timestamp::now(),
                    error: FeedError::Http { status: 503 },
                };
                Some(set_status(pool, feed.podcast_id, &outage).await.map(drop))
            }
            88..100 if !episodes.is_empty() => {
                let episode = episodes[rng.usize(..episodes.len())].id;
                Some(purge_episode(pool, episode).await.map(drop))
            }
            _ => None,
        };
        // A purge or status flip writes on its own; everything else changed
        // the feed, which the next ingest writes.
        let outcome = match outcome {
            Some(outcome) => outcome,
            None => {
                let parsed = common::parsed_feed(
                    &format!("Show {} revision {title_revision}", feed.index),
                    episodes.clone(),
                );
                // `None` until a subscribe creates the fresh feed's podcast.
                ingest(pool, feed.podcast_id, &parsed).await.map(drop)
            }
        };
        if let Err(error) = outcome {
            report.record(&name, error);
        }
        tokio::time::sleep(Duration::from_millis(rng.u64(0..10))).await;
    }
    report
}

/// Makes local writes and syncs at random moments until `deadline`.
async fn client_actor(
    client: &mut Client,
    server: &Server<'_>,
    feeds: &[Feed],
    mut rng: fastrand::Rng,
    deadline: Instant,
) -> ActorReport {
    let mut report = ActorReport::default();
    while Instant::now() < deadline {
        if rng.u32(0..100) < 55 {
            for _ in 0..rng.usize(1..=3) {
                random_local_write(client, feeds, &mut rng);
            }
        } else {
            let limit = rng.u32(3..=60);
            let max_batch = rng.usize(1..=20);
            let outcome = match rng.u32(0..100) {
                0..35 => client.sync(server, limit, max_batch).await,
                35..65 => client
                    .request(server, RequestKind::PushPull, limit, max_batch)
                    .await
                    .map(drop),
                65..95 => client
                    .request(server, RequestKind::PullOnly, limit, max_batch)
                    .await
                    .map(drop),
                _ => client
                    .request(server, RequestKind::PushOnly, limit, max_batch)
                    .await
                    .map(drop),
            };
            if let Err(error) = outcome {
                report.record(&client.name, error);
            }
        }
        tokio::time::sleep(Duration::from_millis(rng.u64(0..8))).await;
    }
    report
}

/// Setting keys the clients write: known ones and one no client knows,
/// which the server must store and sync unchanged (spec 2.4).
const SETTING_KEYS: [&str; 4] = [
    "playback_speed",
    "skip_back_ms",
    "inbox_window_days",
    "x_experiment",
];

fn random_local_write(client: &mut Client, feeds: &[Feed], rng: &mut fastrand::Rng) {
    let known: Vec<_> = client.episodes.keys().copied().collect();
    match rng.u32(0..100) {
        0..55 if !known.is_empty() => {
            let episode = known[rng.usize(..known.len())];
            let write = match rng.u32(0..4) {
                0 | 1 => EpisodeWrite::Position(rng.i64(0..4_000_000)),
                2 => EpisodeWrite::Played(rng.bool()),
                _ => EpisodeWrite::Archived(rng.bool()),
            };
            client.set_episode_field(episode, write);
        }
        55..75 => {
            let key = SETTING_KEYS[rng.usize(..SETTING_KEYS.len())];
            // Values with an exact binary form, so jsonb gives back the same
            // JSON number.
            let value = match key {
                "playback_speed" => serde_json::json!([1.25, 1.5, 1.75, 2.5][rng.usize(..4)]),
                _ => serde_json::json!(rng.u32(..100_000)),
            };
            client.set_setting(key, value);
        }
        _ => {
            let urls: Vec<&str> = feeds.iter().flat_map(Feed::urls).collect();
            let url = urls[rng.usize(..urls.len())];
            client.set_subscribed(url, rng.u32(0..100) < 70);
        }
    }
}

// ==============================================================================
// The run
// ==============================================================================

/// Runs the simulation and returns every difference between a client and
/// the server, plus a summary line of what ran.
async fn simulate(pool: &PgPool, config: &Config) -> (Vec<String>, String) {
    // One connection per actor, plus room for the final checks.
    let pool = PgPoolOptions::new()
        .max_connections(u32::try_from(USERS * DEVICES_PER_USER + SEEDED_FEEDS + 4).expect("small"))
        .connect_with((*pool.connect_options()).clone())
        .await
        .expect("the test database accepts connections");
    let policy = FetchPolicy::new(Allowlist::default());
    let gap = config.gap;
    let sleep_in_gap = move || -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(tokio::time::sleep(gap.unwrap_or_default()))
    };
    let server = Server {
        pool: &pool,
        policy: &policy,
        hooks: PullTestHooks {
            isolation: config.isolation,
            between_statements: gap.is_some().then_some(&sleep_in_gap as _),
        },
    };

    let feeds = seed_feeds(&pool).await;
    let mut clients = Vec::new();
    let mut setup = config.rng(0);
    for user in 0..USERS {
        let user_id = common::create_user(&pool, &format!("user{user}")).await;
        for device in 0..DEVICES_PER_USER {
            // The second device of each user runs two minutes fast, past the
            // 60 s the server allows, so its stamps get clamped.
            let skew = if device == 1 {
                120_000
            } else {
                setup.i64(-5_000..=5_000)
            };
            clients.push(Client::new(
                format!("user{user}/device{device}"),
                user_id,
                DeviceId::from_uuid(Uuid::from_u128(setup.u128(..))),
                skew,
            ));
        }
    }

    // --------------------------------------------------------------------------
    // Concurrent phase
    // --------------------------------------------------------------------------

    let deadline = Instant::now() + config.run_for;
    let ingests =
        join_all(feeds.iter().map(|feed| {
            ingest_actor(&pool, feed, config.rng(1_000 + feed.index as u64), deadline)
        }));
    let syncing = join_all(clients.iter_mut().enumerate().map(|(index, client)| {
        client_actor(
            client,
            &server,
            &feeds,
            config.rng(2_000 + index as u64),
            deadline,
        )
    }));
    let (ingest_reports, client_reports) = tokio::join!(ingests, syncing);

    let mut problems: Vec<String> = Vec::new();
    let mut deadlocks = 0;
    for report in ingest_reports.into_iter().chain(client_reports) {
        deadlocks += report.deadlocks;
        problems.extend(report.errors);
    }

    // --------------------------------------------------------------------------
    // Settle: drain every outbox, then let everyone pull the last writes
    // --------------------------------------------------------------------------

    for _pass in 0..2 {
        for client in &mut clients {
            if let Err(error) = client.sync(&server, 100, 50).await {
                problems.push(format!("{}: final sync failed: {error:?}", client.name));
            }
        }
    }

    // --------------------------------------------------------------------------
    // The property
    // --------------------------------------------------------------------------

    for client in &clients {
        let view = ServerView::read(&pool, client.user_id).await;
        problems.extend(differences(client, &view));
    }

    let mut totals = client::ClientStats::default();
    for client in &clients {
        let s = &client.stats;
        totals.requests += s.requests;
        totals.pull_only += s.pull_only;
        totals.push_only += s.push_only;
        totals.mutations_sent += s.mutations_sent;
        totals.applied += s.applied;
        totals.superseded += s.superseded;
        totals.clamp_rewrites += s.clamp_rewrites;
        totals.merged_rows += s.merged_rows;
        totals.redirects += s.redirects;
        totals.outbox_redirects += s.outbox_redirects;
        totals.catalog_rows += s.catalog_rows;
        totals.tombstones += s.tombstones;
        totals.not_subscribed += s.not_subscribed;
        totals.skipped_pages += s.skipped_pages;
    }
    let catalog: (i64, i64) = sqlx::query_as(
        "select (select count(*) from episode), (select coalesce(sum(version_counter), 0)::bigint from podcast)",
    )
    .fetch_one(&pool)
    .await
    .expect("the catalog is readable");
    let summary = format!(
        "{} episodes, {} podcast-scope versions, {deadlocks} deadlocks retried; {totals:?}",
        catalog.0, catalog.1
    );
    // `#[sqlx::test]` drops the test database when the test ends, which
    // fails while this second pool still holds connections to it.
    pool.close().await;
    (problems, summary)
}

fn report(config: &Config, problems: &[String]) -> String {
    const SHOWN: usize = 30;
    let mut text = format!(
        "{} differences (seed {}, rerun with POD_CONVERGENCE_SEED={}):\n",
        problems.len(),
        config.seed,
        config.seed
    );
    for problem in problems.iter().take(SHOWN) {
        text.push_str(problem);
        text.push('\n');
    }
    if problems.len() > SHOWN {
        text.push_str(&format!("... and {} more\n", problems.len() - SHOWN));
    }
    text
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn clients_converge_to_the_server_state(pool: PgPool) {
    let config = Config::from_env(PullIsolation::Snapshot);
    eprintln!("convergence seed {} for {:?}", config.seed, config.run_for);
    let (problems, summary) = simulate(&pool, &config).await;
    eprintln!("{summary}");
    assert!(problems.is_empty(), "{}", report(&config, &problems));
}

// ==============================================================================
// Found by the property
// ==============================================================================

/// A device running more than 60 s fast subscribes through an alias and
/// unsubscribes the canonical feed in one batch. The server clamps both to
/// the same millisecond, applies the alias subscribe to the canonical row
/// first, then the unsubscribe, which wins the same-device tie (spec 4.4):
/// unsubscribed. When the client then carries its own alias row into the
/// canonical row (spec 5.5 step 1), `merge` would let the older subscribe
/// win the same tie: subscribed. Step 1 carries only a strictly newer stamp,
/// so the canonical row keeps its fields (see `Client::apply_response`),
/// which this test pins. The `merge` rule first diverged in a 90 s soak with
/// seed 12453087336824142324 and in about one in three 10 s runs. The second
/// batch is the opposite order.
#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_clamped_alias_redirect_tie_keeps_the_canonical_row(pool: PgPool) {
    let policy = FetchPolicy::new(Allowlist::default());
    let server = Server {
        pool: &pool,
        policy: &policy,
        hooks: PullTestHooks::default(),
    };
    let feeds = seed_feeds(&pool).await;
    let feed = &feeds[0];
    let alias = feed.alias.as_deref().expect("seeded feeds have an alias");
    let user_id = common::create_user(&pool, "alice").await;
    let mut fast = Client::new(
        "alice/fast".to_owned(),
        user_id,
        DeviceId::new_v7(),
        120_000,
    );

    fast.set_subscribed(alias, true);
    fast.set_subscribed(&feed.url, false);
    fast.sync(&server, 500, 200)
        .await
        .expect("the device syncs");
    let problems = differences(&fast, &ServerView::read(&pool, user_id).await);
    assert!(problems.is_empty(), "{problems:#?}");
    fast.set_subscribed(&feed.url, true);
    fast.set_subscribed(alias, false);
    fast.sync(&server, 500, 200)
        .await
        .expect("the device syncs");

    let problems = differences(&fast, &ServerView::read(&pool, user_id).await);
    assert!(problems.is_empty(), "{problems:#?}");
}

// ==============================================================================
// The property can fail
// ==============================================================================

/// A pull-only request whose user-scope reads straddle a commit: the
/// subscription table is read, then another device commits an unsubscribe
/// (version v+1) and a position (v+2) in one batch, then the episode-state
/// table is read. Returns the subscribed client's differences after it
/// syncs normally afterwards.
async fn straddled_pull(pool: &PgPool, isolation: PullIsolation) -> Vec<String> {
    let policy = FetchPolicy::new(Allowlist::default());
    let feed = "https://feed.example.com/rss.xml";
    let podcast_id = common::create_podcast(pool, feed).await;
    let episodes = (0..3)
        .map(|day| common::parsed_episode(podcast_id, &format!("g{day}"), day))
        .collect::<Vec<_>>();
    let episode_id = episodes[0].id;
    ingest(pool, podcast_id, &common::parsed_feed("Show", episodes))
        .await
        .expect("the feed ingests")
        .expect("the podcast exists");
    let user_id = common::create_user(pool, "alice").await;

    let mut phone = Client::new("alice/phone".to_owned(), user_id, DeviceId::new_v7(), 0);
    let normal = Server {
        pool,
        policy: &policy,
        hooks: PullTestHooks::default(),
    };
    phone.set_subscribed(feed, true);
    phone
        .sync(&normal, 500, 200)
        .await
        .expect("the phone syncs");

    let fired = AtomicBool::new(false);
    let tablet = DeviceId::new_v7();
    let tablet_writes = || -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async {
            if fired.swap(true, Ordering::SeqCst) {
                return;
            }
            let at = Timestamp::now();
            let batch = [
                Ok(Mutation::SubscriptionSet(SubscriptionSet {
                    id: MutationId::new_v7(),
                    at,
                    dev: tablet,
                    podcast_id,
                    feed_url: feed.to_owned(),
                    subscribed: false,
                })),
                Ok(Mutation::EpisodePosition(EpisodePosition {
                    id: MutationId::new_v7(),
                    at,
                    dev: tablet,
                    episode_id,
                    position_ms: 60_000,
                })),
            ];
            apply_batch(pool, user_id, &batch, at, &policy)
                .await
                .expect("the tablet's batch applies");
        })
    };
    let straddling = Server {
        pool,
        policy: &policy,
        hooks: PullTestHooks {
            isolation,
            between_statements: Some(&tablet_writes),
        },
    };
    phone
        .request(&straddling, RequestKind::PullOnly, 500, 0)
        .await
        .expect("the pull succeeds");
    assert!(
        fired.load(Ordering::SeqCst),
        "the tablet wrote between two reads"
    );

    phone
        .sync(&normal, 500, 200)
        .await
        .expect("the phone syncs");
    differences(&phone, &ServerView::read(pool, user_id).await)
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn the_snapshot_survives_a_commit_between_two_table_reads(pool: PgPool) {
    let problems = straddled_pull(&pool, PullIsolation::Snapshot).await;
    assert!(problems.is_empty(), "{problems:#?}");
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn read_committed_loses_a_row_committed_between_two_table_reads(pool: PgPool) {
    let problems = straddled_pull(&pool, PullIsolation::ReadCommitted).await;
    eprintln!("{problems:#?}");
    assert!(
        problems
            .iter()
            .any(|problem| problem.contains("subscription")),
        "the phone kept the subscription the tablet ended: {problems:#?}"
    );
}

/// The full simulation with the snapshot rule broken. Ignored by default
/// because whether a pull straddles a commit is up to the scheduler; with
/// a 2 ms gap between table reads it does in practice every run. Run with
/// `cargo test --test convergence -- --ignored`.
#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
#[ignore = "probabilistic: shows the simulation catches a broken snapshot rule"]
async fn read_committed_pulls_diverge_under_load(pool: PgPool) {
    let config = Config {
        gap: Some(Duration::from_millis(2)),
        ..Config::from_env(PullIsolation::ReadCommitted)
    };
    eprintln!("convergence seed {} for {:?}", config.seed, config.run_for);
    let (problems, summary) = simulate(&pool, &config).await;
    eprintln!("{summary}");
    eprintln!("{}", report(&config, &problems));
    assert!(
        !problems.is_empty(),
        "READ COMMITTED pulls converged anyway (seed {})",
        config.seed
    );
}
