//! The sync engine against a real `pod-server`, in-process, on a fresh
//! migrated Postgres database per test (`#[sqlx::test]` reads
//! `DATABASE_URL`).
//!
//! Each test has its own database and so its own server, on a port from
//! [`bind_test_port`] because the sandboxed dev environment can't bind port
//! 0 (see CLAUDE.md).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::extract::Request;
use axum::middleware::{self, Next};
use color_eyre::eyre::WrapErr;
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use axum::response::{IntoResponse as _, Response};
use pod_protocol::{
    DeviceId, EpisodeId, MutationOutcome, MutationResult, PodcastChanges, PodcastId, PodcastScope,
    ScopeChanges, SyncRequest, SyncResponse, Timestamp, UserId, UserScope, normalize_feed_url,
};
use pod_server::feed::{ParsedEpisode, ParsedFeed, PodcastMetadata};
use rusqlite::params;
use rusqlite::types::Value;
use sqlx::PgPool;
use tempfile::TempDir;
use tokio::sync::{oneshot, watch};

use super::SyncHandle;
use super::engine::{EngineState, SyncEngine};
use super::local_write::{self, LocalChange};
use super::session::{Credentials, Session, SignedIn};
use super::subscribe::{self, DirectoryHint};
use crate::download::{self, Downloader};
use crate::net::NetworkState;
use crate::reconcile::Reconciler;
use crate::store::tests::scratch_dir;
use crate::store::{Store, Table};
use crate::test_port::bind_test_port;

// ==============================================================================
// The server
// ==============================================================================

/// Overrides the port [`bind_test_port`] picks for each server.
const PORT_VAR: &str = "POD_TEST_SYNC_PORT";

/// A `pod-server` serving one test's database. Stops when dropped.
pub(super) struct LiveServer {
    pub(super) base_url: String,
    /// `POST /v1/sync` requests served so far.
    sync_requests: Arc<AtomicUsize>,
    _stop: oneshot::Sender<()>,
}

impl LiveServer {
    pub(super) async fn start(pool: PgPool) -> LiveServer {
        let listener = bind_test_port(PORT_VAR);
        let address = listener
            .local_addr()
            .expect("a bound listener has an address");
        let env = HashMap::from([
            ("DATABASE_URL".to_owned(), "postgres://unused".to_owned()),
            ("POD_BASE_URL".to_owned(), format!("http://{address}")),
        ]);
        let config = pod_server::config::Config::parse(None, &env).expect("the defaults are valid");
        let state = pod_server::AppState::new(pool, &config).expect("the app state builds");
        let sync_requests = Arc::new(AtomicUsize::new(0));
        let counter = sync_requests.clone();
        let app = pod_server::app(state).layer(middleware::from_fn(
            move |request: Request, next: Next| {
                let counter = counter.clone();
                async move {
                    if request.uri().path() == "/v1/sync" {
                        counter.fetch_add(1, Ordering::SeqCst);
                    }
                    next.run(request).await
                }
            },
        ));
        LiveServer::serve(app, listener, sync_requests)
    }

    /// Serves `app` on `listener` from [`bind_test_port`].
    pub(super) fn serve(
        app: axum::Router,
        listener: std::net::TcpListener,
        sync_requests: Arc<AtomicUsize>,
    ) -> LiveServer {
        let base_url = format!(
            "http://{}",
            listener
                .local_addr()
                .expect("a bound listener has an address")
        );
        let listener =
            tokio::net::TcpListener::from_std(listener).expect("the listener joins the runtime");
        let (stop, stopped) = oneshot::channel::<()>();
        tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
            .expect("the test server runs");
        });
        LiveServer {
            base_url,
            sync_requests,
            _stop: stop,
        }
    }

    fn sync_requests(&self) -> usize {
        self.sync_requests.load(Ordering::SeqCst)
    }
}

/// Seeds the catalog the way the poller does after fetching a feed with
/// `count` episodes.
async fn ingest_feed(pool: &PgPool, podcast_id: PodcastId, count: i64) {
    const FIRST_PUBLISHED_MS: i64 = 1_767_225_600_000;
    let episodes = (0..count)
        .map(|index| {
            let guid = format!("episode-{index}");
            ParsedEpisode {
                id: EpisodeId::new(podcast_id, &guid),
                title: format!("Episode {index}"),
                summary_html: "<p>Summary</p>".to_owned(),
                content_html: String::new(),
                link: None,
                published_at: Timestamp::from_epoch_ms(FIRST_PUBLISHED_MS + index * 86_400_000)
                    .expect("seed dates stay near 2026"),
                duration_ms: Some(1_800_000),
                audio_url: format!("https://media.example.com/{guid}.mp3"),
                audio_type: "audio/mpeg".to_owned(),
                audio_bytes: None,
                artwork_url: None,
                season: None,
                number: None,
                guid,
            }
        })
        .collect();
    let feed = ParsedFeed {
        metadata: PodcastMetadata {
            title: "The Example Show".to_owned(),
            author: Some("Example Media".to_owned()),
            description: None,
            link: None,
            artwork_url: None,
            language: Some("en".to_owned()),
        },
        episodes,
        new_feed_url: None,
    };
    pod_server::ingest::ingest(pool, podcast_id, &feed)
        .await
        .expect("the ingest runs")
        .expect("the podcast exists");
}

// ==============================================================================
// Clients
// ==============================================================================

pub(super) const PASSWORD: &str = "correct horse battery";
const FEED_URL: &str = "https://feeds.example.com/show.xml";

/// One desktop install: a store on disk and an engine over it.
pub(super) struct Client {
    pub(super) store: Store,
    pub(super) sync: SyncHandle,
    pub(super) engine: SyncEngine,
    pub(super) session: Session,
    /// Next to the database, named after it.
    pub(super) downloads_root: PathBuf,
    /// The OS connectivity signal the engine and downloads see.
    pub(super) network: watch::Sender<NetworkState>,
}

impl Client {
    pub(super) fn open(path: &Path) -> Client {
        let store = Store::open(path).expect("the store opens");
        let sync = SyncHandle::default();
        let (network_sender, network) = watch::channel(NetworkState::ASSUMED);
        let engine = SyncEngine::new(store.clone(), sync.clone(), network.clone())
            .expect("the engine builds");
        let downloads_root = path.with_extension("audio");
        let downloader = Downloader {
            store: store.clone(),
            client: download::client_builder()
                .no_proxy()
                .build()
                .expect("the test client builds"),
            root: downloads_root.clone(),
            network,
            checkpoint_every: Duration::ZERO,
        };
        let session = Session::new(
            store.clone(),
            sync.clone(),
            engine.clone(),
            Reconciler::new(downloader, |_| Ok(1 << 40)),
        );
        Client {
            store,
            sync,
            engine,
            session,
            downloads_root,
            network: network_sender,
        }
    }

    /// Registers (`register`) or logs in `username` through the session.
    pub(super) async fn sign_in(&self, base_url: &str, username: &str, register: bool) -> SignedIn {
        self.session
            .sign_in(Credentials {
                server_url: base_url.to_owned(),
                username: username.to_owned(),
                password: PASSWORD.to_owned(),
                register,
            })
            .await
            .expect("signing in works")
    }

    async fn token(&self) -> String {
        self.store
            .read(|connection| {
                connection
                    .query_row("select token from account", [], |row| row.get(0))
                    .context("read the token")
            })
            .await
            .expect("the store reads")
    }

    async fn change(&self, change: LocalChange) {
        local_write::write(&self.store, &self.sync, change)
            .await
            .expect("the local write commits");
    }

    pub(super) async fn sync(&self) -> EngineState {
        self.engine.sync().await.expect("the local store works")
    }

    /// Every row of the tables a pull writes, sorted, for comparing stores.
    async fn synced_tables(&self) -> Vec<(Table, Vec<Vec<Value>>)> {
        self.store
            .read(|connection| {
                [
                    Table::Podcast,
                    Table::Episode,
                    Table::Subscription,
                    Table::EpisodeState,
                    Table::Setting,
                    Table::SyncScope,
                    Table::Outbox,
                ]
                .into_iter()
                .map(|table| {
                    let mut statement = connection
                        .prepare(&format!("select * from {} order by 1", table.name()))
                        .context("prepare the dump")?;
                    let columns = statement.column_count();
                    let rows = statement
                        .query_map([], |row| {
                            (0..columns)
                                .map(|index| row.get::<_, Value>(index))
                                .collect()
                        })
                        .context("dump the table")?
                        .collect::<rusqlite::Result<_>>()
                        .context("read the rows")?;
                    Ok((table, rows))
                })
                .collect()
            })
            .await
            .expect("the store reads")
    }

    /// The sidebar's Podcasts group, by title.
    async fn sidebar_titles(&self) -> Vec<String> {
        self.store
            .read(|connection| crate::inbox::load_podcasts(connection, Timestamp::now()))
            .await
            .expect("the sidebar query runs")
            .into_iter()
            .map(|podcast| podcast.title)
            .collect()
    }

    pub(super) async fn count(&self, sql: &'static str) -> i64 {
        self.store
            .read(move |connection| {
                connection
                    .query_row(sql, [], |row| row.get(0))
                    .context("count rows")
            })
            .await
            .expect("the store reads")
    }
}

fn podcast_id() -> PodcastId {
    PodcastId::from_feed_url(&normalize_feed_url(FEED_URL).expect("the feed URL is valid"))
}

fn subscribe() -> LocalChange {
    LocalChange::Subscription {
        podcast_id: podcast_id(),
        feed_url: FEED_URL.to_owned(),
        subscribed: true,
    }
}

/// Alice subscribes on her first desktop, the server ingests the feed with
/// `episodes` episodes, and she listens into the first one there.
async fn alice_subscribes(server: &LiveServer, pool: &PgPool, episodes: i64) -> TempDir {
    let dir = scratch_dir();
    let first = Client::open(&dir.path().join("first.db"));
    first.sign_in(&server.base_url, "alice", true).await;
    first.change(subscribe()).await;
    assert!(matches!(first.sync().await, EngineState::Idle { .. }));
    assert_eq!(first.count("select count(*) from outbox").await, 0);

    ingest_feed(pool, podcast_id(), episodes).await;
    first
        .change(LocalChange::Episode {
            episode_id: EpisodeId::new(podcast_id(), "episode-0"),
            podcast_id: podcast_id(),
            change: local_write::EpisodeChange::Position {
                position_ms: 61_000,
                while_playing: false,
            },
        })
        .await;
    assert!(matches!(first.sync().await, EngineState::Idle { .. }));
    dir
}

// ==============================================================================
// Acceptance
// ==============================================================================

/// 1200 episodes take three podcast pages at the engine's limit of 500.
const EPISODES: i64 = 1200;

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_second_store_gets_the_full_history_in_one_sync(pool: PgPool) {
    let server = LiveServer::start(pool.clone()).await;
    let _first = alice_subscribes(&server, &pool, EPISODES).await;

    let dir = scratch_dir();
    let second = Client::open(&dir.path().join("second.db"));
    second.sign_in(&server.base_url, "alice", false).await;
    let mut states = second.engine.state();
    let watcher = tokio::spawn(async move {
        let mut seen = Vec::new();
        while states.changed().await.is_ok() {
            seen.push(states.borrow_and_update().clone());
        }
        seen
    });
    let state = second.sync().await;
    assert!(
        matches!(
            state,
            EngineState::Idle {
                last_synced: Some(_)
            }
        ),
        "{state:?}"
    );

    assert_eq!(
        second
            .count("select count(*) from subscription where subscribed = 1")
            .await,
        1
    );
    assert_eq!(
        second
            .count("select count(*) from podcast where title = 'The Example Show'")
            .await,
        1
    );
    assert_eq!(second.count("select count(*) from episode").await, EPISODES);
    assert_eq!(
        second.count("select position_ms from episode_state").await,
        61_000
    );
    assert_eq!(
        second
            .count("select count(*) from sync_scope where complete = 0")
            .await,
        0
    );

    drop(second);
    let seen = watcher.await.expect("the watcher doesn't panic");
    let backfill = EngineState::Running {
        backfill: Some("The Example Show".to_owned()),
    };
    assert!(seen.contains(&backfill), "{seen:?}");
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_sync_killed_between_pages_redoes_at_most_one_page(pool: PgPool) {
    let server = LiveServer::start(pool.clone()).await;
    let _first = alice_subscribes(&server, &pool, EPISODES).await;

    // An uninterrupted sync on another store is the reference.
    let dir = scratch_dir();
    let reference = Client::open(&dir.path().join("reference.db"));
    reference.sign_in(&server.base_url, "alice", false).await;
    let before = server.sync_requests();
    reference.sync().await;
    let pages = server.sync_requests() - before;
    assert!(pages >= 4, "the history takes several pages, got {pages}");

    // The process dies right after the first page of episodes commits: the
    // sync future is dropped and the store closed.
    let path = dir.path().join("killed.db");
    let killed = Client::open(&path);
    killed.sign_in(&server.base_url, "alice", false).await;
    let before = server.sync_requests();
    let mut episodes = killed.store.subscribe(Table::Episode.into());
    tokio::select! {
        state = killed.engine.sync() => panic!("the sync ended before a page landed: {state:?}"),
        changed = episodes.changed() => assert!(changed.is_some()),
    }
    let partial = killed.count("select count(*) from episode").await;
    assert!(0 < partial && partial < EPISODES, "{partial} episodes");
    drop(killed);

    let restarted = Client::open(&path);
    let state = restarted.sync().await;
    assert!(matches!(state, EngineState::Idle { .. }), "{state:?}");
    let requests = server.sync_requests() - before;
    assert!(
        requests <= pages + 1,
        "{requests} requests across the crash, {pages} without one"
    );
    // The two stores carry their own account rows and device ids, which
    // only the outbox would show, and that is empty on both.
    assert_eq!(
        restarted.synced_tables().await,
        reference.synced_tables().await
    );
}

// ==============================================================================
// Request failures, against a scripted stub
// ==============================================================================

/// How the stub answers. A pull-only request gets `pull`, one carrying
/// mutations gets `push`. `200` is an empty, complete page that applies
/// every mutation.
#[derive(Debug, Clone, Copy)]
struct Script {
    push: StatusCode,
    pull: StatusCode,
    retry_after: Option<u64>,
    /// Leave the podcast scopes out of the answer, which no server does.
    drop_podcast_scopes: bool,
    /// Never answer.
    hang: bool,
}

async fn stub(
    script: Arc<std::sync::Mutex<Script>>,
    requests: Arc<AtomicUsize>,
    body: axum::body::Bytes,
) -> Response {
    requests.fetch_add(1, Ordering::SeqCst);
    let script = *script
        .lock()
        .expect("no test panics while holding the lock");
    if script.hang {
        std::future::pending::<()>().await;
    }
    let request: SyncRequest = serde_json::from_slice(&body).expect("the engine sends JSON");
    let status = if request.mutations.is_empty() {
        script.pull
    } else {
        script.push
    };
    if status != StatusCode::OK {
        let mut response = (status, "{}").into_response();
        if let Some(seconds) = script.retry_after {
            response
                .headers_mut()
                .insert(axum::http::header::RETRY_AFTER, seconds.into());
        }
        return response;
    }
    let results = request
        .parse_mutations()
        .expect("the engine sends mutation ids")
        .into_iter()
        .map(|mutation| {
            let mutation = mutation.expect("the engine sends valid mutations");
            MutationResult {
                id: mutation.id(),
                outcome: MutationOutcome::Applied { at: mutation.at() },
            }
        })
        .collect();
    let cursors = request.scopes.expect("the engine always pulls");
    axum::Json(SyncResponse {
        server_time: Timestamp::now(),
        results,
        scopes: Some(ScopeChanges {
            user: UserScope {
                version: cursors.user,
                complete: true,
                subscriptions: Vec::new(),
                episode_states: Vec::new(),
                settings: Vec::new(),
            },
            podcasts: cursors
                .podcasts
                .iter()
                .filter(|_| !script.drop_podcast_scopes)
                .map(|cursor| {
                    PodcastScope::Ok(PodcastChanges {
                        id: cursor.id,
                        version: cursor.version,
                        complete: true,
                        podcast: None,
                        episodes: Vec::new(),
                    })
                })
                .collect(),
        }),
    })
    .into_response()
}

/// A signed-in client with one queued mutation, against a stub answering
/// per `script`.
async fn stubbed(script: Script) -> (LiveServer, Arc<std::sync::Mutex<Script>>, TempDir, Client) {
    let script = Arc::new(std::sync::Mutex::new(script));
    let handler_script = script.clone();
    let requests = Arc::new(AtomicUsize::new(0));
    let handler_requests = requests.clone();
    let app = axum::Router::new().route(
        "/v1/sync",
        axum::routing::post(move |body| {
            stub(handler_script.clone(), handler_requests.clone(), body)
        }),
    );
    let server = LiveServer::serve(app, bind_test_port(PORT_VAR), requests);
    let dir = scratch_dir();
    let client = Client::open(&dir.path().join("pod.db"));
    let base_url = server.base_url.clone();
    client
        .store
        .write(move |transaction| {
            transaction
                .execute(
                    "insert into account (id, server_url, user_id, username, device_id, token)
                     values (1, ?1, ?2, 'alice', ?3, 'token')",
                    params![
                        base_url,
                        UserId::new_v7().to_string(),
                        DeviceId::new_v7().to_string()
                    ],
                )
                .context("insert the account")
        })
        .await
        .expect("the account is stored");
    client.change(subscribe()).await;
    (server, script, dir, client)
}

const OK: Script = Script {
    push: StatusCode::OK,
    pull: StatusCode::OK,
    retry_after: None,
    drop_podcast_scopes: false,
    hang: false,
};

#[tokio::test]
async fn unauthorized_signs_out_and_keeps_the_outbox() {
    let (_server, _script, _dir, client) = stubbed(Script {
        push: StatusCode::UNAUTHORIZED,
        ..OK
    })
    .await;
    assert_eq!(client.sync().await, EngineState::SignedOut);
    assert_eq!(*client.engine.state().borrow(), EngineState::SignedOut);
    assert_eq!(client.count("select count(*) from outbox").await, 1);

    // The failed batch is no longer in flight, so a new write replaces it.
    let mutation = local_write::write(&client.store, &client.sync, subscribe())
        .await
        .expect("the local write commits");
    assert_eq!(client.count("select count(*) from outbox").await, 1);
    let queued: String = client
        .store
        .read(|connection| {
            connection
                .query_row("select mutation_id from outbox", [], |row| row.get(0))
                .context("read the outbox")
        })
        .await
        .expect("the store reads");
    assert_eq!(queued, mutation.id().to_string());
}

#[tokio::test]
async fn a_batch_refused_five_times_moves_to_the_rejected_log() {
    let (_server, _script, _dir, client) = stubbed(Script {
        push: StatusCode::BAD_REQUEST,
        ..OK
    })
    .await;
    for attempt in 1..=4 {
        // Each loop counts the attempt, then finishes pull-only.
        let state = client.sync().await;
        assert!(
            matches!(
                state,
                EngineState::Idle {
                    last_synced: Some(_)
                }
            ),
            "{state:?}"
        );
        assert_eq!(
            client.count("select attempts from outbox").await,
            attempt,
            "after attempt {attempt}"
        );
    }
    client.sync().await;
    assert_eq!(client.count("select count(*) from outbox").await, 0);
    assert_eq!(
        client
            .count("select count(*) from rejected_mutation where reason = 'bad_request'")
            .await,
        1
    );
}

#[tokio::test]
async fn server_errors_back_off_doubling_until_a_request_succeeds() {
    let (_server, script, _dir, client) = stubbed(Script {
        push: StatusCode::SERVICE_UNAVAILABLE,
        ..OK
    })
    .await;
    let backoff = |state: EngineState| match state {
        EngineState::BackingOff { retry_at } => retry_at.duration_since(Instant::now()),
        other => panic!("expected a backoff, got {other:?}"),
    };
    let first = backoff(client.sync().await);
    assert!(
        Duration::from_secs(4) < first && first <= Duration::from_secs(5),
        "{first:?}"
    );
    let second = backoff(client.sync().await);
    assert!(
        Duration::from_secs(9) < second && second <= Duration::from_secs(10),
        "{second:?}"
    );
    assert_eq!(client.count("select count(*) from outbox").await, 1);

    *script.lock().expect("not poisoned") = OK;
    assert!(matches!(client.sync().await, EngineState::Idle { .. }));
    assert_eq!(client.count("select count(*) from outbox").await, 0);
    tokio::time::timeout(
        Duration::from_secs(1),
        client.engine.loop_ended().notified(),
    )
    .await
    .expect("the reconciler hears that the loop ended");

    // A success resets the backoff, and `429` waits at least `Retry-After`.
    *script.lock().expect("not poisoned") = Script {
        pull: StatusCode::TOO_MANY_REQUESTS,
        retry_after: Some(60),
        ..OK
    };
    let limited = backoff(client.sync().await);
    assert!(
        Duration::from_secs(59) < limited && limited <= Duration::from_secs(60),
        "{limited:?}"
    );
}

#[tokio::test]
async fn a_response_that_moves_nothing_ends_the_loop() {
    let (_server, _script, _dir, client) = stubbed(Script {
        drop_podcast_scopes: true,
        ..OK
    })
    .await;
    let state = tokio::time::timeout(Duration::from_secs(10), client.sync())
        .await
        .expect("the loop stops");
    assert!(matches!(state, EngineState::Idle { .. }), "{state:?}");
    assert_eq!(client.count("select count(*) from outbox").await, 0);
}

#[tokio::test]
async fn the_driver_syncs_at_start_and_when_a_write_comes_due() {
    let (_server, _script, _dir, client) = stubbed(OK).await;
    let mut states = client.engine.state();
    let driver = tokio::spawn(client.engine.clone().run());
    tokio::time::timeout(
        Duration::from_secs(5),
        states.wait_for(|state| {
            matches!(
                state,
                EngineState::Idle {
                    last_synced: Some(_)
                }
            )
        }),
    )
    .await
    .expect("the start-up sync finishes")
    .expect("the engine is alive");
    assert_eq!(client.count("select count(*) from outbox").await, 0);

    // A write made while no loop runs waits for its debounce.
    let mut outbox = client.store.subscribe(Table::Outbox.into());
    client.change(subscribe()).await;
    let queued = Instant::now();
    loop {
        tokio::time::timeout(Duration::from_secs(5), outbox.changed())
            .await
            .expect("the write is sent once its deadline passes");
        if client.count("select count(*) from outbox").await == 0 {
            break;
        }
    }
    assert!(
        queued.elapsed() >= local_write::WRITE_SYNC_DELAY - Duration::from_millis(100),
        "the write waited for its debounce, {:?}",
        queued.elapsed()
    );
    driver.abort();
}

#[tokio::test]
async fn losing_the_network_shows_offline_and_regaining_it_syncs_at_once() {
    let (_server, _script, _dir, client) = stubbed(OK).await;
    let mut states = client.engine.state();
    let driver = tokio::spawn(client.engine.clone().run());
    let mut wait_for = async |what: &str, done: fn(&EngineState) -> bool| {
        tokio::time::timeout(Duration::from_secs(5), states.wait_for(done))
            .await
            .unwrap_or_else(|_| panic!("timed out waiting until {what}"))
            .expect("the engine is alive");
    };
    let synced = |state: &EngineState| {
        matches!(
            state,
            EngineState::Idle {
                last_synced: Some(_)
            }
        )
    };
    wait_for("the start-up sync finishes", synced).await;

    client
        .network
        .send_modify(|network| network.connected = false);
    wait_for("the status shows offline", |state| {
        *state == EngineState::Offline
    })
    .await;
    let mut outbox = client.store.subscribe(Table::Outbox.into());
    client.change(subscribe()).await;
    outbox.changed().await.expect("the store is open");
    // The write's deadline passes offline: the engine stays put.
    tokio::time::sleep(local_write::WRITE_SYNC_DELAY + Duration::from_millis(200)).await;
    assert_eq!(*client.engine.state().borrow(), EngineState::Offline);
    assert_eq!(client.count("select count(*) from outbox").await, 1);

    let reconnected = Instant::now();
    client
        .network
        .send_modify(|network| network.connected = true);
    wait_for("the reconnect sync finishes", synced).await;
    assert!(
        reconnected.elapsed() < Duration::from_secs(5),
        "no wait for the 5 min timer"
    );
    assert_eq!(client.count("select count(*) from outbox").await, 0);
    driver.abort();
}

// ==============================================================================
// Signing out and in again (spec 5.7)
// ==============================================================================

/// Every local table with its row count.
async fn row_counts(client: &Client) -> Vec<(Table, i64)> {
    client
        .store
        .read(|connection| {
            Table::ALL
                .into_iter()
                .map(|table| {
                    let count = connection
                        .query_row(
                            &format!("select count(*) from {}", table.name()),
                            [],
                            |row| row.get(0),
                        )
                        .context("count the rows")?;
                    Ok((table, count))
                })
                .collect()
        })
        .await
        .expect("the store reads")
}

async fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn logout_cancels_the_request_in_flight_and_keeps_only_device_settings() {
    let (server, _script, _dir, client) = stubbed(Script { hang: true, ..OK }).await;
    client
        .store
        .write(|transaction| {
            transaction
                .execute_batch(
                    "insert into device_setting (key, value) values ('volume', '0.8');
                     insert into download (episode_id, state, audio_url, path, bytes_done)
                     values ('e', 'complete', 'https://media.example.com/e.mp3', 'p/e.mp3', 7);",
                )
                .context("seed the device setting and a download")
        })
        .await
        .expect("the seed commits");
    std::fs::create_dir_all(client.downloads_root.join("podcast")).expect("tmp/ is writable");
    std::fs::write(client.downloads_root.join("podcast/e.mp3"), b"audio")
        .expect("tmp/ is writable");

    let engine = client.engine.clone();
    let sync = tokio::spawn(async move { engine.sync().await });
    wait_until("the request is in flight", || server.sync_requests() == 1).await;

    tokio::time::timeout(Duration::from_secs(5), client.session.logout())
        .await
        .expect("logout doesn't wait for the response")
        .expect("logout works");
    tokio::time::timeout(Duration::from_secs(1), sync)
        .await
        .expect("the cancelled sync returns")
        .expect("the sync task doesn't panic")
        .expect("the local store works");

    let left: Vec<_> = row_counts(&client)
        .await
        .into_iter()
        .filter(|(_, count)| *count > 0)
        .collect();
    assert_eq!(left, [(Table::DeviceSetting, 1)]);
    assert!(!client.downloads_root.exists());
    assert_eq!(
        *client.engine.state().borrow(),
        EngineState::Idle { last_synced: None }
    );
    assert_eq!(server.sync_requests(), 1, "no request after logout");
}

#[tokio::test]
async fn a_signed_out_driver_waits_for_sign_in_not_for_writes() {
    let (server, script, _dir, client) = stubbed(Script {
        push: StatusCode::UNAUTHORIZED,
        ..OK
    })
    .await;
    let mut states = client.engine.state();
    let driver = tokio::spawn(client.engine.clone().run());
    tokio::time::timeout(
        Duration::from_secs(5),
        states.wait_for(|state| *state == EngineState::SignedOut),
    )
    .await
    .expect("the start-up sync is refused")
    .expect("the engine is alive");

    let refused = server.sync_requests();
    client.sync.request_sync(Duration::ZERO);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        server.sync_requests(),
        refused,
        "a write doesn't retry a revoked session"
    );

    *script.lock().expect("not poisoned") = OK;
    client
        .store
        .write(|transaction| {
            transaction
                .execute("update account set token = 'fresh'", [])
                .context("store the new token")
        })
        .await
        .expect("the update commits");
    tokio::time::timeout(
        Duration::from_secs(5),
        states.wait_for(|state| {
            matches!(
                state,
                EngineState::Idle {
                    last_synced: Some(_)
                }
            )
        }),
    )
    .await
    .expect("signing in wakes the engine")
    .expect("the engine is alive");
    assert_eq!(client.count("select count(*) from outbox").await, 0);
    driver.abort();
}

/// Alice on one desktop, subscribed to a show with three episodes.
async fn alice_on_a_desktop(server: &LiveServer, pool: &PgPool) -> (TempDir, Client) {
    let dir = scratch_dir();
    let client = Client::open(&dir.path().join("pod.db"));
    assert_eq!(
        client.sign_in(&server.base_url, "alice", true).await,
        SignedIn::New
    );
    client.change(subscribe()).await;
    assert!(matches!(client.sync().await, EngineState::Idle { .. }));
    ingest_feed(pool, podcast_id(), 3).await;
    assert!(matches!(client.sync().await, EngineState::Idle { .. }));
    assert_eq!(client.count("select count(*) from episode").await, 3);
    (dir, client)
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_revoked_session_signs_out_until_the_same_user_signs_in_again(pool: PgPool) {
    let server = LiveServer::start(pool.clone()).await;
    let (_dir, client) = alice_on_a_desktop(&server, &pool).await;
    let old_token = client.token().await;

    sqlx::query("delete from session")
        .execute(&pool)
        .await
        .expect("the session is revoked");

    // Playback goes on: it saves the position like any local write, and the
    // catalog stays readable.
    let position = LocalChange::Episode {
        episode_id: EpisodeId::new(podcast_id(), "episode-0"),
        podcast_id: podcast_id(),
        change: local_write::EpisodeChange::Position {
            position_ms: 61_000,
            while_playing: true,
        },
    };
    client.change(position).await;
    assert_eq!(client.sync().await, EngineState::SignedOut);
    assert_eq!(client.count("select count(*) from outbox").await, 1);
    assert_eq!(client.count("select count(*) from episode").await, 3);

    assert_eq!(
        client.sign_in(&server.base_url, "alice", false).await,
        SignedIn::Resumed
    );
    assert_ne!(client.token().await, old_token);
    assert_eq!(client.count("select count(*) from outbox").await, 1);
    assert!(matches!(client.sync().await, EngineState::Idle { .. }));
    assert_eq!(client.count("select count(*) from outbox").await, 0);
    let (position_ms,): (i64,) = sqlx::query_as("select position_ms from episode_state")
        .fetch_one(&pool)
        .await
        .expect("the server stored the position");
    assert_eq!(position_ms, 61_000);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn signing_in_as_another_user_logs_the_first_one_out(pool: PgPool) {
    let server = LiveServer::start(pool.clone()).await;
    let (_dir, client) = alice_on_a_desktop(&server, &pool).await;
    let alice_token = client.token().await;

    assert_eq!(
        client.sign_in(&server.base_url, "bob", true).await,
        SignedIn::Switched
    );
    assert_eq!(client.count("select count(*) from subscription").await, 0);
    assert_eq!(client.count("select count(*) from episode").await, 0);
    assert_eq!(
        client
            .count("select count(*) from account where username = 'bob'")
            .await,
        1
    );

    let me = reqwest::Client::new()
        .get(format!("{}/v1/auth/me", server.base_url))
        .bearer_auth(alice_token)
        .send()
        .await
        .expect("the server answers");
    assert_eq!(
        me.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "logout ended alice's session"
    );
}

// ==============================================================================
// Offline subscribe (spec 5.5)
// ==============================================================================

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn an_offline_subscribe_shows_a_placeholder_that_fills_in_after_reconnecting(pool: PgPool) {
    let server = LiveServer::start(pool.clone()).await;
    let dir = scratch_dir();
    let client = Client::open(&dir.path().join("pod.db"));
    client.sign_in(&server.base_url, "alice", true).await;
    client.network.send_replace(NetworkState {
        connected: false,
        metered: false,
    });

    let url = normalize_feed_url(FEED_URL).expect("the feed URL is valid");
    let hint = DirectoryHint {
        title: "Example Show (directory)".to_owned(),
        artwork_url: None,
    };
    subscribe::subscribe(&client.store, &client.sync, &url, Some(hint))
        .await
        .expect("the subscribe commits");
    assert_eq!(client.sync().await, EngineState::Offline);
    assert_eq!(
        client.sidebar_titles().await,
        ["Example Show (directory)"],
        "the placeholder shows at once"
    );
    assert_eq!(client.count("select count(*) from outbox").await, 1);

    client.network.send_replace(NetworkState::ASSUMED);
    assert!(matches!(client.sync().await, EngineState::Idle { .. }));
    assert_eq!(client.count("select count(*) from outbox").await, 0);
    assert_eq!(
        client.sidebar_titles().await,
        ["Example Show (directory)"],
        "the placeholder stays until the poller fetched the feed"
    );

    ingest_feed(&pool, podcast_id(), 2).await;
    assert!(matches!(client.sync().await, EngineState::Idle { .. }));
    assert_eq!(client.sidebar_titles().await, ["The Example Show"]);
    assert_eq!(
        client
            .count("select count(*) from podcast where status = 'ok' and version > 0")
            .await,
        1
    );
    assert_eq!(client.count("select count(*) from episode").await, 2);
}

/// The podcast page's unsubscribe on one desktop takes the show off the
/// other one with its next sync, catalog included.
#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn an_unsubscribe_reaches_the_other_desktop_after_a_sync(pool: PgPool) {
    let server = LiveServer::start(pool.clone()).await;
    let (_first_dir, first) = alice_on_a_desktop(&server, &pool).await;
    let dir = scratch_dir();
    let second = Client::open(&dir.path().join("second.db"));
    second.sign_in(&server.base_url, "alice", false).await;
    assert!(matches!(second.sync().await, EngineState::Idle { .. }));
    assert_eq!(second.sidebar_titles().await, ["The Example Show"]);

    first
        .change(LocalChange::Subscription {
            podcast_id: podcast_id(),
            feed_url: FEED_URL.to_owned(),
            subscribed: false,
        })
        .await;
    assert_eq!(
        first.sidebar_titles().await,
        Vec::<String>::new(),
        "gone from this sidebar before any sync"
    );
    assert!(matches!(first.sync().await, EngineState::Idle { .. }));
    assert_eq!(first.count("select count(*) from episode").await, 0);

    assert!(matches!(second.sync().await, EngineState::Idle { .. }));
    assert_eq!(second.sidebar_titles().await, Vec::<String>::new());
    assert_eq!(second.count("select count(*) from episode").await, 0);
    assert_eq!(second.count("select count(*) from sync_scope").await, 1);
    assert_eq!(
        second
            .count("select count(*) from subscription where subscribed = 0")
            .await,
        1
    );
}

/// Where the show lived before it moved. The server knows it as an alias of
/// [`FEED_URL`].
const OLD_FEED_URL: &str = "https://old.example.com/show.xml";

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn subscribing_to_an_alias_ends_with_the_canonical_show_and_this_devices_settings(
    pool: PgPool,
) {
    let server = LiveServer::start(pool.clone()).await;
    let canonical = podcast_id();
    seed_moved_feed(&pool, canonical).await;
    ingest_feed(&pool, canonical, 3).await;

    let dir = scratch_dir();
    let client = Client::open(&dir.path().join("pod.db"));
    client.sign_in(&server.base_url, "alice", true).await;
    let url = normalize_feed_url(OLD_FEED_URL).expect("the feed URL is valid");
    let alias = subscribe::subscribe(&client.store, &client.sync, &url, None)
        .await
        .expect("the subscribe commits");
    assert_ne!(
        alias, canonical,
        "the client derives the id from the old URL"
    );
    // The user turns on auto-download for the placeholder before it syncs.
    client
        .store
        .write(move |transaction| {
            transaction
                .execute(
                    "insert into podcast_device (podcast_id, auto_keep) values (?1, 2)",
                    [alias.to_string()],
                )
                .context("insert the podcast settings")
        })
        .await
        .expect("the settings commit");

    assert!(matches!(client.sync().await, EngineState::Idle { .. }));

    let (subscription, settings): (String, String) = client
        .store
        .read(|connection| {
            Ok((
                connection
                    .query_row(
                        "select podcast_id || ' ' || feed_url || ' ' || subscribed
                         from subscription",
                        [],
                        |row| row.get(0),
                    )
                    .context("read the subscription")?,
                connection
                    .query_row(
                        "select podcast_id || ' ' || auto_keep from podcast_device",
                        [],
                        |row| row.get(0),
                    )
                    .context("read the podcast settings")?,
            ))
        })
        .await
        .expect("the store reads");
    assert_eq!(client.count("select count(*) from subscription").await, 1);
    assert_eq!(subscription, format!("{canonical} {OLD_FEED_URL} 1"));
    assert_eq!(settings, format!("{canonical} 2"));
    assert_eq!(client.sidebar_titles().await, ["The Example Show"]);
    assert_eq!(client.count("select count(*) from episode").await, 3);
    assert_eq!(
        client
            .count("select count(*) from sync_scope where scope like 'podcast:%'")
            .await,
        1
    );
    assert_eq!(client.count("select count(*) from podcast").await, 1);
    assert_eq!(client.count("select count(*) from outbox").await, 0);
}

/// The podcast at [`FEED_URL`], with [`OLD_FEED_URL`] recorded as its alias
/// the way the poller records a feed move (spec 3.2).
async fn seed_moved_feed(pool: &PgPool, canonical: PodcastId) {
    let old = normalize_feed_url(OLD_FEED_URL).expect("the feed URL is valid");
    let new = normalize_feed_url(FEED_URL).expect("the feed URL is valid");
    let mut transaction = pool.begin().await.expect("a transaction starts");
    sqlx::query("insert into podcast (id, feed_url, version) values ($1, $2, 0)")
        .bind(canonical.as_uuid())
        .bind(new.feed_url())
        .execute(&mut *transaction)
        .await
        .expect("the podcast is new");
    sqlx::query("insert into feed_poll (podcast_id) values ($1)")
        .bind(canonical.as_uuid())
        .execute(&mut *transaction)
        .await
        .expect("the poll row is new");
    for url in [&new, &old] {
        sqlx::query("insert into podcast_alias (normalized_url, podcast_id) values ($1, $2)")
            .bind(url.normalized())
            .bind(canonical.as_uuid())
            .execute(&mut *transaction)
            .await
            .expect("the alias is new");
    }
    transaction.commit().await.expect("the seed commits");
}
