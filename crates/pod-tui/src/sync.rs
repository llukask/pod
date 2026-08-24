use std::collections::HashSet;

use anyhow::Context;
use tokio::sync::mpsc;

use pod_model::{
    Episode, PodcastWithEpisodeStats, ProgressState, ProgressSyncResponse, SyncResponse,
};

use crate::api_client::{ApiClient, ApiStatusError};
use crate::app::Action;
use crate::local_db::LocalDb;

/// The server calls one sync cycle makes, so tests can drive `run_sync_with`
/// against a scripted server instead of a live one.
pub(crate) trait SyncApi {
    async fn list_podcasts(&self) -> anyhow::Result<Vec<PodcastWithEpisodeStats>>;
    async fn sync_head(&self) -> anyhow::Result<String>;
    async fn list_episodes(
        &self,
        podcast_id: &str,
        per_page: i32,
        page_token: Option<&str>,
    ) -> anyhow::Result<(Vec<(Episode, Option<i32>, bool)>, Option<String>)>;
    async fn sync_changes(&self, since: &str, limit: i64) -> anyhow::Result<SyncResponse>;
    async fn sync_progress(&self, since: Option<&str>) -> anyhow::Result<ProgressSyncResponse>;
    async fn report_progress(
        &self,
        episode_id: &str,
        progress: i32,
        done: bool,
    ) -> anyhow::Result<ProgressState>;
}

impl SyncApi for ApiClient {
    async fn list_podcasts(&self) -> anyhow::Result<Vec<PodcastWithEpisodeStats>> {
        ApiClient::list_podcasts(self).await
    }
    async fn sync_head(&self) -> anyhow::Result<String> {
        ApiClient::sync_head(self).await
    }
    async fn list_episodes(
        &self,
        podcast_id: &str,
        per_page: i32,
        page_token: Option<&str>,
    ) -> anyhow::Result<(Vec<(Episode, Option<i32>, bool)>, Option<String>)> {
        ApiClient::list_episodes(self, podcast_id, per_page, page_token).await
    }
    async fn sync_changes(&self, since: &str, limit: i64) -> anyhow::Result<SyncResponse> {
        ApiClient::sync_changes(self, since, limit).await
    }
    async fn sync_progress(&self, since: Option<&str>) -> anyhow::Result<ProgressSyncResponse> {
        ApiClient::sync_progress(self, since).await
    }
    async fn report_progress(
        &self,
        episode_id: &str,
        progress: i32,
        done: bool,
    ) -> anyhow::Result<ProgressState> {
        ApiClient::report_progress(self, episode_id, progress, done).await
    }
}

/// Run a full sync cycle: reconcile the podcast list, pull episode changes,
/// pull progress changes, push dirty local progress, and purge podcasts the
/// user is no longer subscribed to.
///
/// Opens its own SQLite connection from the given path so the future is
/// `Send` (rusqlite::Connection is not Sync).
///
/// `now_playing_episode` is the episode currently held by the player, if any;
/// its podcast is exempt from the purge because playback keeps recording
/// progress against those rows.
pub async fn run_sync(
    db_path: &str,
    now_playing_episode: Option<String>,
    tx: mpsc::UnboundedSender<Action>,
) -> anyhow::Result<()> {
    let mut db = LocalDb::open(db_path).context("open local database")?;
    let server_url = db
        .get_config("server_url")
        .ok_or_else(|| anyhow::anyhow!("no server_url configured"))?;
    let token = db
        .get_config("auth_token")
        .ok_or_else(|| anyhow::anyhow!("not logged in"))?;

    let client = ApiClient::new(&server_url, Some(token));
    run_sync_with(&client, &mut db, now_playing_episode, tx).await
}

/// The sync cycle proper, over an injected server and an open database.
pub(crate) async fn run_sync_with<A: SyncApi>(
    client: &A,
    db: &mut LocalDb,
    now_playing_episode: Option<String>,
    tx: mpsc::UnboundedSender<Action>,
) -> anyhow::Result<()> {
    // Local state goes up before any remote state comes down, so the server
    // echoes values at least as new as ours for the rest of the cycle.
    let _ = tx.send(Action::SyncProgress("Pushing playback progress…".to_string()));
    let dirty = db.list_dirty_progress();
    for (episode_id, progress, done) in &dirty {
        match client.report_progress(episode_id, *progress, *done).await {
            Ok(_) => db.mark_progress_clean(episode_id),
            Err(e) => {
                // Non-fatal: the row stays dirty and is retried next cycle.
                tracing::warn!(episode_id, "failed to push progress: {}", e);
            }
        }
    }

    let _ = tx.send(Action::SyncProgress("Fetching podcast list…".to_string()));
    let podcasts = client.list_podcasts().await.context("fetch podcast list")?;

    for p in &podcasts {
        db.upsert_podcast(p);
    }

    // The cursor is committed before the bootstrap loop. The per-podcast
    // markers, not the cursor, gate bootstrapping, so a crash mid-bootstrap
    // re-runs the unmarked podcasts while the delta feed from this head
    // replays anything published since as idempotent upserts. Committing it
    // after the loop would let a retry adopt a newer head and strand episodes
    // published between the two heads for podcasts already marked.
    let episode_cursor = match db.get_sync_state("episode_cursor") {
        Some(cursor) => cursor,
        None => {
            let head = client.sync_head().await.context("fetch sync head")?;
            db.set_sync_state("episode_cursor", &head);
            head
        }
    };

    // Every `episode_change` row for a newly visible podcast sits behind the
    // cursor, so the delta feed will never deliver its back catalogue.
    let to_bootstrap: Vec<_> = podcasts
        .iter()
        .filter(|p| !db.podcast_bootstrapped(&p.id))
        .collect();
    let total = to_bootstrap.len();
    'podcasts: for (i, p) in to_bootstrap.iter().enumerate() {
        let _ = tx.send(Action::SyncProgress(format!(
            "Syncing episodes ({}/{total}): {}",
            i + 1,
            p.title
        )));
        // Progress and done are persisted alongside the episodes, which is
        // what restores a user's listening state when they re-subscribe: the
        // server keeps its `user_episode` rows across an unsubscribe, but
        // `/sync/progress` is cursored on timestamps and will never replay
        // them.
        let mut page_token: Option<String> = None;
        loop {
            let page = client.list_episodes(&p.id, 100, page_token.as_deref()).await;
            let (episodes, next_token) = match page {
                Ok(page) => page,
                // The episode listing answers 404 to a caller who is not
                // subscribed, so an unsubscribe racing this cycle surfaces
                // here (possibly mid-pagination). Skipping without the
                // bootstrap marker keeps the outcome convergent: the next
                // cycle reconciles the podcast away if the unsubscribe was
                // real, and re-fetches from page one if the user came back.
                // Any other status is still a real failure.
                Err(e)
                    if e.downcast_ref::<ApiStatusError>()
                        .is_some_and(|s| s.status == reqwest::StatusCode::NOT_FOUND) =>
                {
                    tracing::warn!(
                        podcast_id = p.id.as_str(),
                        "bootstrap skipped: no longer subscribed"
                    );
                    continue 'podcasts;
                }
                Err(e) => {
                    return Err(e)
                        .with_context(|| format!("fetch episodes for podcast {}", p.id));
                }
            };
            for (episode, progress, done) in &episodes {
                db.upsert_episode(episode);
                if let Some(progress) = progress {
                    db.upsert_progress(&episode.id, *progress, *done, false);
                }
            }
            if next_token.is_none() || episodes.is_empty() {
                break;
            }
            page_token = next_token;
        }
        // Set only once the whole back catalogue has landed, so an interrupted
        // bootstrap runs again on the next sync rather than stranding the
        // podcast with whichever pages happened to arrive.
        db.mark_podcast_bootstrapped(&p.id);
    }

    let _ = tx.send(Action::SyncProgress("Syncing episode changes…".to_string()));
    let mut since = episode_cursor;
    loop {
        let resp = client
            .sync_changes(&since, 500)
            .await
            .context("fetch sync changes")?;
        for change in &resp.changes {
            if change.op == "upsert" {
                db.upsert_episode(&change.episode);
            }
            // TODO: handle "delete" ops.
        }
        since = resp.next_since.clone();
        if !resp.has_more {
            // Only commit the cursor after the final page so a crash
            // mid-pagination doesn't skip unprocessed pages.
            db.set_sync_state("episode_cursor", &resp.next_since);
            break;
        }
    }

    let _ = tx.send(Action::SyncProgress("Syncing playback progress…".to_string()));
    let progress_since = db.get_sync_state("progress_since");
    let progress_resp = client
        .sync_progress(progress_since.as_deref())
        .await
        .context("fetch progress changes")?;
    for change in &progress_resp.changes {
        // Server wins: overwrite local progress (mark clean since it came
        // from the server).
        db.upsert_progress(&change.episode_id, change.progress, change.done, false);
    }
    db.set_sync_state(
        "progress_since",
        &progress_resp.server_time.to_rfc3339(),
    );

    // The purge is the only step in the cycle whose effects cannot be healed
    // by a later sync (the audio files are gone for good), so it is decided
    // against a fresh subscription list rather than the snapshot taken before
    // the bootstrap: a first sync can page entire back catalogues, and a
    // re-subscribe from another device or the web in that window must win.
    // The server's list is authoritative in both directions, so converging on
    // it heals any number of missed subscribe and unsubscribe events.
    let fresh = client
        .list_podcasts()
        .await
        .context("re-fetch podcast list before purge")?;
    let fresh_ids: HashSet<&str> = fresh.iter().map(|p| p.id.as_str()).collect();
    let unsubscribed: Vec<String> = db
        .list_podcasts()
        .into_iter()
        .map(|p| p.id)
        .filter(|id| !fresh_ids.contains(id.as_str()))
        .collect();
    let playing_podcast = now_playing_episode
        .as_deref()
        .and_then(|id| db.episode_podcast_id(id));

    if !unsubscribed.is_empty() {
        let _ = tx.send(Action::SyncProgress(
            "Removing unsubscribed podcasts…".to_string(),
        ));
        for podcast_id in &unsubscribed {
            // Playback records progress against these rows once per second;
            // purging them mid-listen would silently drop all of it.
            if playing_podcast.as_deref() == Some(podcast_id.as_str()) {
                tracing::info!(podcast_id, "purge deferred: an episode is playing");
                continue;
            }
            // A dirty row here means the push at the top of the cycle failed.
            // Purging would delete progress the server never received; the
            // podcast waits so the push can retry.
            if db.podcast_has_dirty_progress(podcast_id) {
                tracing::warn!(podcast_id, "purge deferred: unpushed progress remains");
                continue;
            }
            // A failed purge (a lost lock race, most likely) defers to the
            // next cycle; the cycle itself still completes.
            if let Err(e) = db.remove_podcast(podcast_id) {
                tracing::warn!(podcast_id, "failed to purge podcast: {:#}", e);
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::*;

    type EpisodePage = anyhow::Result<(Vec<(Episode, Option<i32>, bool)>, Option<String>)>;

    /// A scripted server. `podcast_lists` holds subscription snapshots as
    /// podcast ids, popped once per `list_podcasts` call (the cycle calls it
    /// twice: once for reconciliation, once freshly before the purge); the
    /// last list keeps answering when the queue runs dry. `episode_pages` is
    /// popped once per `list_episodes` call across all podcasts. The delta
    /// and progress feeds answer empty.
    #[derive(Default)]
    struct FakeApi {
        podcast_lists: Mutex<VecDeque<Vec<String>>>,
        episode_pages: Mutex<VecDeque<EpisodePage>>,
        fail_report_progress: bool,
        reported: Mutex<Vec<(String, i32, bool)>>,
    }

    impl SyncApi for FakeApi {
        async fn list_podcasts(&self) -> anyhow::Result<Vec<PodcastWithEpisodeStats>> {
            let mut lists = self.podcast_lists.lock().expect("mutex is never poisoned");
            let ids = if lists.len() > 1 {
                lists.pop_front().expect("len checked above")
            } else {
                lists
                    .front()
                    .cloned()
                    .expect("test scripts at least one podcast list")
            };
            Ok(ids.iter().map(|id| test_podcast(id)).collect())
        }
        async fn sync_head(&self) -> anyhow::Result<String> {
            Ok("head-1".to_string())
        }
        async fn list_episodes(
            &self,
            _podcast_id: &str,
            _per_page: i32,
            _page_token: Option<&str>,
        ) -> anyhow::Result<(Vec<(Episode, Option<i32>, bool)>, Option<String>)> {
            self.episode_pages
                .lock()
                .expect("mutex is never poisoned")
                .pop_front()
                .expect("test scripts enough episode pages")
        }
        async fn sync_changes(&self, since: &str, _limit: i64) -> anyhow::Result<SyncResponse> {
            Ok(SyncResponse {
                server_time: chrono::Utc::now(),
                next_since: since.to_string(),
                has_more: false,
                changes: vec![],
            })
        }
        async fn sync_progress(
            &self,
            _since: Option<&str>,
        ) -> anyhow::Result<ProgressSyncResponse> {
            Ok(ProgressSyncResponse {
                server_time: chrono::Utc::now(),
                changes: vec![],
            })
        }
        async fn report_progress(
            &self,
            episode_id: &str,
            progress: i32,
            done: bool,
        ) -> anyhow::Result<ProgressState> {
            if self.fail_report_progress {
                anyhow::bail!("server unreachable");
            }
            self.reported
                .lock()
                .expect("mutex is never poisoned")
                .push((episode_id.to_string(), progress, done));
            Ok(ProgressState { progress, done })
        }
    }

    fn test_podcast(id: &str) -> PodcastWithEpisodeStats {
        let now = chrono::Utc::now();
        PodcastWithEpisodeStats {
            id: id.to_string(),
            title: format!("Podcast {id}"),
            description: String::new(),
            image_link: String::new(),
            feed_url: String::new(),
            feed_type: "rss".to_string(),
            created_at: now,
            last_updated: now,
            last_publication_date: None,
            feed_etag: None,
            feed_last_modified: None,
        }
    }

    fn test_episode(id: &str, podcast_id: &str) -> Episode {
        let now = chrono::Utc::now();
        Episode {
            id: id.to_string(),
            podcast_id: podcast_id.to_string(),
            title: format!("Episode {id}"),
            summary: String::new(),
            summary_type: String::new(),
            content_encoded: String::new(),
            content_encoded_type: String::new(),
            publication_date: now,
            audio_url: format!("https://audio.example/{id}.mp3"),
            audio_type: "audio/mpeg".to_string(),
            audio_duration: 100,
            thumbnail_url: None,
            created_at: now,
            last_updated: now,
        }
    }

    fn channel() -> mpsc::UnboundedSender<Action> {
        mpsc::unbounded_channel().0
    }

    /// The upgrade scenario: an existing install has a fully synced podcast
    /// with unpushed offline progress but no bootstrap marker, so the next
    /// cycle re-bootstraps it and the server hands back its stale progress.
    /// With the push also failing (the same offline spell that made the row
    /// dirty), the local value must survive to be pushed later.
    #[tokio::test]
    async fn dirty_progress_survives_bootstrap_returning_stale_server_value() {
        let mut db = LocalDb::open(":memory:").unwrap();
        db.upsert_podcast(&test_podcast("p1"));
        db.upsert_episode(&test_episode("e1", "p1"));
        db.upsert_progress("e1", 1200, false, true);

        let api = FakeApi {
            podcast_lists: Mutex::new(VecDeque::from([vec!["p1".to_string()]])),
            episode_pages: Mutex::new(VecDeque::from([Ok((
                vec![(test_episode("e1", "p1"), Some(0), false)],
                None,
            ))])),
            fail_report_progress: true,
            ..FakeApi::default()
        };

        run_sync_with(&api, &mut db, None, channel())
            .await
            .expect("cycle completes despite the failed push");

        assert!(db.podcast_bootstrapped("p1"));
        assert_eq!(
            db.list_dirty_progress(),
            vec![("e1".to_string(), 1200, false)]
        );
    }

    /// A dirty row at purge time means the push failed this cycle. Purging
    /// anyway would delete the only copy of that progress, so the podcast
    /// must wait for a cycle in which the push lands.
    #[tokio::test]
    async fn podcast_with_unpushed_dirty_progress_is_not_purged() {
        let mut db = LocalDb::open(":memory:").unwrap();
        db.upsert_podcast(&test_podcast("p1"));
        db.upsert_episode(&test_episode("e1", "p1"));
        db.mark_podcast_bootstrapped("p1");
        db.upsert_progress("e1", 600, false, true);

        let api = FakeApi {
            podcast_lists: Mutex::new(VecDeque::from([vec![]])),
            fail_report_progress: true,
            ..FakeApi::default()
        };

        run_sync_with(&api, &mut db, None, channel())
            .await
            .expect("cycle completes despite the failed push");

        assert!(db.podcast_exists("p1"));
        assert_eq!(
            db.list_dirty_progress(),
            vec![("e1".to_string(), 600, false)]
        );
    }

    /// The complement of the test above: once the push succeeds, nothing
    /// holds the purge back.
    #[tokio::test]
    async fn purge_removes_podcast_once_progress_is_pushed() {
        let mut db = LocalDb::open(":memory:").unwrap();
        db.upsert_podcast(&test_podcast("p1"));
        db.upsert_episode(&test_episode("e1", "p1"));
        db.mark_podcast_bootstrapped("p1");
        db.upsert_progress("e1", 600, false, true);

        let api = FakeApi {
            podcast_lists: Mutex::new(VecDeque::from([vec![]])),
            ..FakeApi::default()
        };

        run_sync_with(&api, &mut db, None, channel())
            .await
            .expect("cycle completes");

        assert_eq!(
            *api.reported.lock().unwrap(),
            vec![("e1".to_string(), 600, false)]
        );
        assert!(!db.podcast_exists("p1"));
        assert!(db.list_dirty_progress().is_empty());
    }

    /// The podcast whose episode is playing keeps its rows even when
    /// unsubscribed, so the once-per-second progress writes keep landing.
    #[tokio::test]
    async fn purge_skips_the_podcast_currently_playing() {
        let mut db = LocalDb::open(":memory:").unwrap();
        db.upsert_podcast(&test_podcast("p1"));
        db.upsert_episode(&test_episode("e1", "p1"));
        db.mark_podcast_bootstrapped("p1");

        let api = FakeApi {
            podcast_lists: Mutex::new(VecDeque::from([vec![]])),
            ..FakeApi::default()
        };

        run_sync_with(&api, &mut db, Some("e1".to_string()), channel())
            .await
            .expect("cycle completes");

        assert!(db.podcast_exists("p1"));
    }

    /// A bootstrap cut short must not mark the podcast, so the next cycle
    /// starts it over from page one; a re-subscribe after an unsubscribe
    /// starts from scratch the same way.
    #[tokio::test]
    async fn interrupted_bootstrap_is_retried_and_resubscribing_starts_over() {
        let mut db = LocalDb::open(":memory:").unwrap();

        // First cycle dies fetching page two.
        let api = FakeApi {
            podcast_lists: Mutex::new(VecDeque::from([vec!["p1".to_string()]])),
            episode_pages: Mutex::new(VecDeque::from([
                Ok((
                    vec![(test_episode("e1", "p1"), None, false)],
                    Some("page-2".to_string()),
                )),
                Err(anyhow::anyhow!("connection reset")),
            ])),
            ..FakeApi::default()
        };
        run_sync_with(&api, &mut db, None, channel())
            .await
            .expect_err("cycle fails mid-bootstrap");
        assert!(!db.podcast_bootstrapped("p1"));
        assert_eq!(db.list_episodes("p1").len(), 1);

        // The retry re-fetches from page one, re-upserting what already
        // landed, and only then sets the marker.
        let api = FakeApi {
            podcast_lists: Mutex::new(VecDeque::from([vec!["p1".to_string()]])),
            episode_pages: Mutex::new(VecDeque::from([
                Ok((
                    vec![(test_episode("e1", "p1"), None, false)],
                    Some("page-2".to_string()),
                )),
                Ok((vec![(test_episode("e2", "p1"), None, false)], None)),
            ])),
            ..FakeApi::default()
        };
        run_sync_with(&api, &mut db, None, channel())
            .await
            .expect("retried cycle completes");
        assert!(db.podcast_bootstrapped("p1"));
        assert_eq!(db.list_episodes("p1").len(), 2);

        // Unsubscribing purges the marker with the rows, so a re-subscribe
        // bootstraps again instead of leaving the podcast permanently empty.
        let api = FakeApi {
            podcast_lists: Mutex::new(VecDeque::from([vec![]])),
            ..FakeApi::default()
        };
        run_sync_with(&api, &mut db, None, channel())
            .await
            .expect("purge cycle completes");
        assert!(!db.podcast_exists("p1"));
        assert!(!db.podcast_bootstrapped("p1"));
    }

    /// An unsubscribe landing mid-cycle surfaces as a 404 from the episode
    /// listing. The cycle skips that podcast without marking it and carries
    /// on with the rest.
    #[tokio::test]
    async fn bootstrap_skips_podcast_unsubscribed_mid_cycle() {
        let mut db = LocalDb::open(":memory:").unwrap();

        let not_subscribed = crate::api_client::ApiStatusError {
            status: reqwest::StatusCode::NOT_FOUND,
            endpoint: "list_episodes".to_string(),
            url: "http://server.example/api/v1/podcasts/p1/episodes".to_string(),
            body: String::new(),
        };
        let api = FakeApi {
            podcast_lists: Mutex::new(VecDeque::from([vec![
                "p1".to_string(),
                "p2".to_string(),
            ]])),
            episode_pages: Mutex::new(VecDeque::from([
                Err(not_subscribed.into()),
                Ok((vec![(test_episode("e2", "p2"), None, false)], None)),
            ])),
            ..FakeApi::default()
        };

        run_sync_with(&api, &mut db, None, channel())
            .await
            .expect("cycle completes despite the 404");

        assert!(!db.podcast_bootstrapped("p1"));
        assert!(db.podcast_bootstrapped("p2"));
        assert_eq!(db.list_episodes("p2").len(), 1);
    }
}
