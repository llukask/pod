//! The pull half of `POST /v1/sync` (spec 4.2 rules 2 to 6): scope rows
//! above the cursors, one shared change limit, `not_subscribed` scopes and
//! episode tombstones.

mod auth_app;
mod common;

use std::collections::BTreeSet;
use std::time::Duration;

use auth_app::{PASSWORD, TestApp, TestResponse};
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use pod_protocol::{
    DeviceId, EpisodeId, MutationId, PodcastChanges, PodcastId, PodcastScope, ScopeEpisode,
    SyncResponse, Timestamp, UserId,
};
use pod_server::db::SqlTimestamp;
use pod_server::ingest::{IngestOutcome, ingest, purge_episode};
use pod_server::version::VersionScope;
use serde_json::{Value, json};
use sqlx::PgPool;

// ==============================================================================
// Helpers
// ==============================================================================

const FEED_A: &str = "https://example.com/a.xml";
const FEED_B: &str = "https://example.com/b.xml";

/// A fixed device id, so snapshots stay stable.
fn device() -> DeviceId {
    "019a3f10-8c2e-7a41-9b3d-5e6f7a8b9c0d"
        .parse()
        .expect("literal is a UUID")
}

/// A fixed client action time, far enough in the past that the server's
/// clock clamp never moves it.
fn at(ms_after_base: i64) -> Timestamp {
    Timestamp::from_epoch_ms(1_748_764_800_120 + ms_after_base).expect("2025 is in range")
}

/// A registered user and their bearer token.
struct Fixture {
    app: TestApp,
    token: String,
    user_id: UserId,
}

async fn fixture(pool: PgPool) -> Fixture {
    let app = TestApp::new(pool);
    let response = app.post_auth("/v1/auth/register", "alice", PASSWORD).await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.body);
    let token = auth_app::token_of(&response);
    let user_id: UserId = serde_json::from_value(response.body["user_id"].clone())
        .expect("register returns the user id");
    Fixture {
        app,
        token,
        user_id,
    }
}

impl Fixture {
    fn pool(&self) -> &PgPool {
        &self.app.pool
    }

    async fn sync(&self, body: Value) -> TestResponse {
        self.app
            .send(
                Request::post("/v1/sync")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {}", self.token))
                    .body(Body::from(body.to_string()))
                    .expect("valid request"),
            )
            .await
    }

    /// Sends `body`, asserts a 200, and parses the response, checking that
    /// it survives a round trip through the protocol types unchanged.
    async fn sync_ok(&self, body: Value) -> SyncResponse {
        let response = self.sync(body).await;
        assert_eq!(response.status, StatusCode::OK, "{}", response.body);
        let parsed: SyncResponse = serde_json::from_value(response.body.clone())
            .unwrap_or_else(|error| panic!("{error}: {}", response.body));
        assert_eq!(
            serde_json::to_value(&parsed).expect("a response serializes"),
            response.body,
            "the wire body is exactly what the protocol types produce"
        );
        parsed
    }

    /// A pull-only request.
    async fn pull(&self, user: i64, podcasts: &[(PodcastId, i64)], limit: u32) -> SyncResponse {
        let podcasts: Vec<Value> = podcasts
            .iter()
            .map(|(id, version)| json!({ "id": id, "version": version }))
            .collect();
        self.sync_ok(json!({
            "mutations": [],
            "scopes": { "user": user, "podcasts": podcasts },
            "limit": limit,
        }))
        .await
    }

    /// Writes the user's subscription row as subscribe would, with the next
    /// user-scope version.
    async fn set_subscription(
        &self,
        podcast_id: PodcastId,
        feed_url: &str,
        subscribed: bool,
        replaced_by: Option<PodcastId>,
    ) -> i64 {
        let mut scope = VersionScope::begin_user(self.pool(), self.user_id)
            .await
            .expect("the user row locks")
            .expect("the user exists");
        let version = scope.next_version();
        sqlx::query!(
            "insert into subscription
                 (user_id, podcast_id, feed_url, subscribed, subscribed_at, subscribed_dev,
                  replaced_by, version)
             values ($1, $2, $3, $4, $5, $6, $7, $8)
             on conflict (user_id, podcast_id) do update set
                 subscribed = excluded.subscribed, subscribed_at = excluded.subscribed_at,
                 replaced_by = excluded.replaced_by, version = excluded.version",
            self.user_id.as_uuid(),
            podcast_id.as_uuid(),
            feed_url,
            subscribed,
            SqlTimestamp(at(version)) as _,
            device().as_uuid(),
            replaced_by.map(|id| id.as_uuid()),
            version
        )
        .execute(scope.conn())
        .await
        .expect("the subscription writes");
        scope.commit().await.expect("the subscription commits");
        version
    }

    /// The user's current user-scope version.
    async fn user_counter(&self) -> i64 {
        sqlx::query_scalar!(
            "select version_counter from users where id = $1",
            self.user_id.as_uuid()
        )
        .fetch_one(self.pool())
        .await
        .expect("the user exists")
    }
}

/// Creates a podcast and ingests `count` episodes, published one a day, so
/// the podcast row takes version 1 and the episodes versions 2 onwards.
async fn ingested_podcast(pool: &PgPool, feed_url: &str, count: i64) -> PodcastId {
    let podcast_id = common::create_podcast(pool, feed_url).await;
    let episodes = (0..count)
        .rev()
        .map(|day| common::parsed_episode(podcast_id, &format!("ep-{day:03}"), day))
        .collect();
    let feed = common::parsed_feed(&format!("Show at {feed_url}"), episodes);
    let outcome = ingest(pool, podcast_id, &feed)
        .await
        .expect("the ingest runs")
        .expect("the podcast exists");
    assert!(matches!(outcome, IngestOutcome::Applied(_)), "{outcome:?}");
    podcast_id
}

fn position(episode_id: EpisodeId, position_ms: i64, at: Timestamp) -> Value {
    json!({
        "id": MutationId::new_v7(), "at": at, "dev": device(), "type": "episode.position",
        "episode_id": episode_id, "position_ms": position_ms,
    })
}

fn setting(key: &str, value: Value, at: Timestamp) -> Value {
    json!({
        "id": MutationId::new_v7(), "at": at, "dev": device(), "type": "setting.set",
        "key": key, "value": value,
    })
}

fn episode_id(podcast_id: PodcastId, day: i64) -> EpisodeId {
    EpisodeId::new(podcast_id, &format!("ep-{day:03}"))
}

fn scope_ok(scope: &PodcastScope) -> &PodcastChanges {
    match scope {
        PodcastScope::Ok(changes) => changes,
        PodcastScope::NotSubscribed(_) => panic!("expected an ok scope, got {scope:?}"),
    }
}

/// The number of rows a response carries, which the limit caps.
fn row_count(response: &SyncResponse) -> usize {
    let scopes = response.scopes.as_ref().expect("the request had scopes");
    let user = &scopes.user;
    let podcasts: usize = scopes
        .podcasts
        .iter()
        .map(|scope| match scope {
            PodcastScope::Ok(changes) => {
                usize::from(changes.podcast.is_some()) + changes.episodes.len()
            }
            PodcastScope::NotSubscribed(_) => 0,
        })
        .sum();
    user.subscriptions.len() + user.episode_states.len() + user.settings.len() + podcasts
}

// ==============================================================================
// User scope
// ==============================================================================

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn the_user_scope_merges_its_tables_by_version(pool: PgPool) {
    let f = fixture(pool).await;
    let podcast_id = ingested_podcast(f.pool(), FEED_A, 2).await;
    let subscribed = f.set_subscription(podcast_id, FEED_A, true, None).await;
    let pushed = f
        .sync_ok(json!({ "mutations": [
            position(episode_id(podcast_id, 0), 1000, at(0)),
            setting("playback_speed", json!(1.5), at(0)),
            position(episode_id(podcast_id, 1), 2000, at(0)),
        ] }))
        .await;
    assert!(
        pushed.scopes.is_none(),
        "a push-only request gets no scopes"
    );

    let response = f.pull(0, &[], 500).await;
    let user = response.scopes.expect("the request had scopes").user;

    assert_eq!(user.version, 4);
    assert!(user.complete);
    let subscription_versions: Vec<i64> = user.subscriptions.iter().map(|s| s.version).collect();
    let state_versions: Vec<i64> = user.episode_states.iter().map(|s| s.version).collect();
    let setting_versions: Vec<i64> = user.settings.iter().map(|s| s.version).collect();
    assert_eq!(subscription_versions, [subscribed]);
    assert_eq!(state_versions, [2, 4]);
    assert_eq!(setting_versions, [3]);
    assert_eq!(user.episode_states[0].position_ms, 1000);
    assert_eq!(user.episode_states[0].position_at, Some(at(0)));
    assert_eq!(user.settings[0].value, json!(1.5));
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn the_user_scope_pages_by_version_and_reports_complete(pool: PgPool) {
    let f = fixture(pool).await;
    let podcast_id = ingested_podcast(f.pool(), FEED_A, 3).await;
    f.set_subscription(podcast_id, FEED_A, true, None).await;
    f.sync_ok(json!({ "mutations": [
        position(episode_id(podcast_id, 0), 1000, at(0)),
        position(episode_id(podcast_id, 1), 1000, at(0)),
        position(episode_id(podcast_id, 2), 1000, at(0)),
    ] }))
    .await;

    let first = f.pull(0, &[], 2).await.scopes.expect("scopes").user;
    assert_eq!((first.version, first.complete), (2, false));
    assert_eq!(first.subscriptions.len() + first.episode_states.len(), 2);

    let second = f.pull(2, &[], 2).await.scopes.expect("scopes").user;
    assert_eq!((second.version, second.complete), (4, true));
    assert_eq!(second.episode_states.len(), 2);

    // Exactly `limit` rows left is complete: the extra row fetched to detect
    // more rows isn't there.
    let exact = f.pull(1, &[], 3).await.scopes.expect("scopes").user;
    assert_eq!((exact.version, exact.complete), (4, true));

    let caught_up = f.pull(4, &[], 2).await.scopes.expect("scopes").user;
    assert_eq!((caught_up.version, caught_up.complete), (4, true));
    assert!(caught_up.episode_states.is_empty());
}

// ==============================================================================
// Podcast scopes and the shared limit
// ==============================================================================

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn the_limit_is_shared_across_scopes_in_request_order(pool: PgPool) {
    let f = fixture(pool).await;
    let a = ingested_podcast(f.pool(), FEED_A, 3).await;
    let b = ingested_podcast(f.pool(), FEED_B, 2).await;
    f.set_subscription(a, FEED_A, true, None).await;
    f.set_subscription(b, FEED_B, true, None).await;
    f.sync_ok(json!({ "mutations": [setting("skip_back_ms", json!(10000), at(0))] }))
        .await;

    // Three user rows, then one of A's four rows, then nothing left for B.
    let response = f.pull(0, &[(a, 0), (b, 0)], 4).await;
    assert_eq!(row_count(&response), 4);
    let scopes = response.scopes.expect("scopes");
    assert_eq!((scopes.user.version, scopes.user.complete), (3, true));

    let first = scope_ok(&scopes.podcasts[0]);
    assert_eq!(first.id, a);
    assert_eq!((first.version, first.complete), (1, false));
    assert_eq!(first.podcast.as_ref().map(|p| p.version), Some(1));
    assert!(first.episodes.is_empty());

    let unreached = scope_ok(&scopes.podcasts[1]);
    assert_eq!(
        *unreached,
        PodcastChanges {
            id: b,
            version: 0,
            complete: false,
            podcast: None,
            episodes: Vec::new(),
        }
    );
}

/// Runs the client loop of spec 4.2 with podcast scopes in a fixed order,
/// collecting every episode id. Panics if it doesn't converge.
async fn pull_until_complete(
    f: &Fixture,
    mut user: i64,
    mut scopes: Vec<(PodcastId, i64)>,
    limit: u32,
) -> (usize, Vec<EpisodeId>) {
    let mut episodes = Vec::new();
    for round in 1..=50 {
        let response = f.pull(user, &scopes, limit).await;
        assert!(row_count(&response) <= limit as usize, "round {round}");
        let changes = response.scopes.expect("scopes");
        user = changes.user.version;
        let mut complete = changes.user.complete;
        let mut next = Vec::new();
        for scope in &changes.podcasts {
            let PodcastScope::Ok(scope) = scope else {
                continue;
            };
            complete &= scope.complete;
            episodes.extend(scope.episodes.iter().map(ScopeEpisode::id));
            next.push((scope.id, scope.version));
        }
        let known: BTreeSet<PodcastId> = next.iter().map(|(id, _)| *id).collect();
        let new_scopes: Vec<(PodcastId, i64)> = changes
            .user
            .subscriptions
            .iter()
            .filter(|s| s.subscribed && s.replaced_by.is_none() && !known.contains(&s.podcast_id))
            .map(|s| (s.podcast_id, 0))
            .collect();
        complete &= new_scopes.is_empty();
        next.extend(new_scopes);
        scopes = next;
        if complete {
            return (round, episodes);
        }
    }
    panic!("the client loop didn't converge in 50 rounds");
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_client_loop_receives_every_row_once_across_scopes(pool: PgPool) {
    let f = fixture(pool).await;
    let a = ingested_podcast(f.pool(), FEED_A, 7).await;
    let b = ingested_podcast(f.pool(), FEED_B, 5).await;
    f.set_subscription(a, FEED_A, true, None).await;
    f.set_subscription(b, FEED_B, true, None).await;

    let (_, episodes) = pull_until_complete(&f, 0, Vec::new(), 3).await;

    let expected: BTreeSet<EpisodeId> = (0..7)
        .map(|day| episode_id(a, day))
        .chain((0..5).map(|day| episode_id(b, day)))
        .collect();
    assert_eq!(episodes.len(), expected.len(), "no episode arrives twice");
    assert_eq!(episodes.into_iter().collect::<BTreeSet<_>>(), expected);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_large_backfill_takes_several_rounds_and_ends_complete(pool: PgPool) {
    let f = fixture(pool).await;
    let podcast_id = ingested_podcast(f.pool(), FEED_A, 25).await;
    f.set_subscription(podcast_id, FEED_A, true, None).await;
    let user = f.user_counter().await;

    let mut cursor = 0;
    let mut versions = Vec::new();
    let mut podcast_rows = Vec::new();
    let mut rounds = 0;
    loop {
        rounds += 1;
        let response = f.pull(user, &[(podcast_id, cursor)], 10).await;
        let scope = scope_ok(&response.scopes.as_ref().expect("scopes").podcasts[0]).clone();
        if let Some(podcast) = &scope.podcast {
            podcast_rows.push(podcast.version);
            versions.push(podcast.version);
        }
        versions.extend(scope.episodes.iter().map(ScopeEpisode::version));
        assert_eq!(
            scope.version,
            *versions.last().expect("each round has rows")
        );
        cursor = scope.version;
        if scope.complete {
            break;
        }
        assert!(rounds < 10, "the backfill doesn't converge");
    }

    assert_eq!(rounds, 3, "26 rows at 10 per round");
    assert_eq!(podcast_rows, [1], "the podcast row comes first, once");
    assert_eq!(versions, (1..=26).collect::<Vec<i64>>());
    let caught_up = f.pull(user, &[(podcast_id, cursor)], 10).await;
    let scope = scope_ok(&caught_up.scopes.expect("scopes").podcasts[0]).clone();
    assert_eq!((scope.version, scope.complete), (26, true));
    assert!(scope.podcast.is_none() && scope.episodes.is_empty());
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_pending_podcast_sends_no_row(pool: PgPool) {
    let f = fixture(pool).await;
    let podcast_id = common::create_podcast(f.pool(), FEED_A).await;
    f.set_subscription(podcast_id, FEED_A, true, None).await;

    let response = f.pull(0, &[(podcast_id, 0)], 500).await;
    let scope = scope_ok(&response.scopes.expect("scopes").podcasts[0]).clone();
    assert_eq!(
        scope,
        PodcastChanges {
            id: podcast_id,
            version: 0,
            complete: true,
            podcast: None,
            episodes: Vec::new(),
        }
    );
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_purged_episode_comes_back_as_a_tombstone(pool: PgPool) {
    let f = fixture(pool).await;
    let podcast_id = ingested_podcast(f.pool(), FEED_A, 3).await;
    f.set_subscription(podcast_id, FEED_A, true, None).await;
    let purged = episode_id(podcast_id, 1);
    purge_episode(f.pool(), purged)
        .await
        .expect("the purge runs")
        .expect("the episode exists");
    let user = f.user_counter().await;

    let response = f.pull(user, &[(podcast_id, 4)], 500).await;
    let scope = scope_ok(&response.scopes.expect("scopes").podcasts[0]).clone();
    assert_eq!((scope.version, scope.complete), (5, true));
    let [ScopeEpisode::Deleted(tombstone)] = scope.episodes.as_slice() else {
        panic!("expected one tombstone, got {:?}", scope.episodes);
    };
    assert_eq!((tombstone.id, tombstone.version), (purged, 5));

    // A backfill from 0 carries the tombstone too, in its version order.
    let backfill = f.pull(user, &[(podcast_id, 0)], 500).await;
    let scope = scope_ok(&backfill.scopes.expect("scopes").podcasts[0]).clone();
    let kinds: Vec<(i64, bool)> = scope
        .episodes
        .iter()
        .map(|e| (e.version(), matches!(e, ScopeEpisode::Deleted(_))))
        .collect();
    assert_eq!(kinds, [(2, false), (4, false), (5, true)]);
}

// ==============================================================================
// not_subscribed
// ==============================================================================

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn scopes_without_a_live_subscription_are_not_subscribed(pool: PgPool) {
    let f = fixture(pool).await;
    let never = ingested_podcast(f.pool(), FEED_A, 1).await;
    let unsubscribed = ingested_podcast(f.pool(), FEED_B, 1).await;
    let canonical = ingested_podcast(f.pool(), "https://example.com/c.xml", 1).await;
    let replaced = PodcastId::from_feed_url(
        &pod_protocol::normalize_feed_url("https://example.com/alias.xml").expect("valid URL"),
    );
    let others = ingested_podcast(f.pool(), "https://example.com/d.xml", 1).await;
    f.set_subscription(unsubscribed, FEED_B, false, None).await;
    f.set_subscription(canonical, "https://example.com/c.xml", true, None)
        .await;
    f.set_subscription(
        replaced,
        "https://example.com/alias.xml",
        true,
        Some(canonical),
    )
    .await;
    let bob = common::create_user(f.pool(), "bob").await;
    sqlx::query!(
        "insert into subscription (user_id, podcast_id, feed_url, subscribed, subscribed_at,
             subscribed_dev, version)
         values ($1, $2, 'https://example.com/d.xml', true, now(), $3, 1)",
        bob.as_uuid(),
        others.as_uuid(),
        device().as_uuid()
    )
    .execute(f.pool())
    .await
    .expect("bob's subscription writes");

    let user = f.user_counter().await;
    let response = f
        .pull(
            user,
            &[
                (never, 0),
                (unsubscribed, 0),
                (replaced, 0),
                (others, 0),
                (canonical, 0),
            ],
            500,
        )
        .await;
    let scopes = response.scopes.expect("scopes").podcasts;
    let statuses: Vec<(PodcastId, bool)> = scopes
        .iter()
        .map(|scope| (scope.id(), matches!(scope, PodcastScope::NotSubscribed(_))))
        .collect();
    assert_eq!(
        statuses,
        [
            (never, true),
            (unsubscribed, true),
            (replaced, true),
            (others, true),
            (canonical, false),
        ]
    );
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn an_unreached_foreign_scope_is_still_not_subscribed(pool: PgPool) {
    let f = fixture(pool).await;
    let mine = ingested_podcast(f.pool(), FEED_A, 3).await;
    let foreign = ingested_podcast(f.pool(), FEED_B, 1).await;
    f.set_subscription(mine, FEED_A, true, None).await;
    let user = f.user_counter().await;

    let response = f.pull(user, &[(mine, 0), (foreign, 0)], 2).await;
    let scopes = response.scopes.expect("scopes").podcasts;
    assert!(!scope_ok(&scopes[0]).complete);
    assert!(
        matches!(scopes[1], PodcastScope::NotSubscribed(scope) if scope.id == foreign),
        "{:?}",
        scopes[1]
    );
}

// ==============================================================================
// Push and pull together
// ==============================================================================

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_pull_only_request_returns_what_push_and_pull_returns(pool: PgPool) {
    let f = fixture(pool).await;
    let podcast_id = ingested_podcast(f.pool(), FEED_A, 2).await;
    f.set_subscription(podcast_id, FEED_A, true, None).await;
    let episode = episode_id(podcast_id, 0);
    f.sync_ok(json!({ "mutations": [position(episode, 5000, at(1000))] }))
        .await;
    let scopes = json!({ "user": 0, "podcasts": [{ "id": podcast_id, "version": 0 }] });

    let pushed = f
        .sync(json!({
            "mutations": [position(episode, 1000, at(0))],
            "scopes": scopes,
        }))
        .await;
    let pulled = f.sync(json!({ "mutations": [], "scopes": scopes })).await;

    assert_eq!(pushed.status, StatusCode::OK, "{}", pushed.body);
    assert_eq!(pushed.body["results"][0]["status"], "superseded");
    assert_eq!(pulled.status, StatusCode::OK, "{}", pulled.body);
    assert_eq!(pushed.body["scopes"], pulled.body["scopes"]);
    assert_eq!(
        pulled.body["scopes"]["user"]["episode_states"][0]["position_ms"],
        5000
    );
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_pull_only_request_takes_no_user_lock(pool: PgPool) {
    let f = fixture(pool).await;
    let held = VersionScope::begin_user(f.pool(), f.user_id)
        .await
        .expect("the user row locks")
        .expect("the user exists");

    let response = tokio::time::timeout(Duration::from_secs(5), f.pull(0, &[], 500))
        .await
        .expect("a pull-only request doesn't wait for the user-scope lock");
    assert!(response.results.is_empty());
    drop(held);
}

// ==============================================================================
// Response shape
// ==============================================================================

/// A user with every kind of row: a subscription, an episode state and a
/// setting in the user scope; a podcast row, a live episode and a
/// tombstone in its podcast scope; and a foreign scope.
#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn the_response_shape_matches_the_snapshot(pool: PgPool) {
    let f = fixture(pool).await;
    let podcast_id = ingested_podcast(f.pool(), FEED_A, 2).await;
    let foreign = ingested_podcast(f.pool(), FEED_B, 1).await;
    purge_episode(f.pool(), episode_id(podcast_id, 0))
        .await
        .expect("the purge runs")
        .expect("the episode exists");
    f.set_subscription(podcast_id, FEED_A, true, None).await;
    let mutation_id: MutationId = "019a3f10-0000-7000-8000-000000000001"
        .parse()
        .expect("literal is a UUID");
    let mut mutation = position(episode_id(podcast_id, 1), 1_234_000, at(0));
    mutation["id"] = json!(mutation_id);

    let response = f
        .sync_ok(json!({
            "mutations": [mutation, setting("playback_speed", json!(1.3), at(500))],
            "scopes": { "user": 0, "podcasts": [
                { "id": podcast_id, "version": 0 },
                { "id": foreign, "version": 0 },
            ] },
        }))
        .await;

    let mut body = serde_json::to_value(&response).expect("a response serializes");
    assert_eq!(body["server_time"], json!(f.app.now()));
    body["server_time"] = json!("[server_time]");
    body["results"][1]["id"] = json!("[mutation_id]");
    insta::assert_snapshot!(serde_json::to_string_pretty(&body).expect("JSON prints"));
}
