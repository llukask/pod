use std::collections::HashSet;

use anyhow::Context;
use tokio::sync::mpsc;

use crate::api_client::ApiClient;
use crate::app::Action;
use crate::local_db::LocalDb;

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
    for (i, p) in to_bootstrap.iter().enumerate() {
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
            let (episodes, next_token) = client
                .list_episodes(&p.id, 100, page_token.as_deref())
                .await
                .with_context(|| format!("fetch episodes for podcast {}", p.id))?;
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
            match db.remove_podcast(podcast_id) {
                Ok(paths) => {
                    for path in paths {
                        if let Err(e) = tokio::fs::remove_file(&path).await {
                            tracing::warn!(?path, "failed to delete downloaded audio: {}", e);
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(podcast_id, "failed to purge podcast: {:#}", e);
                }
            }
        }
    }

    Ok(())
}
