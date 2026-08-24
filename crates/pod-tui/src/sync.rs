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
pub async fn run_sync(db_path: &str, tx: mpsc::UnboundedSender<Action>) -> anyhow::Result<()> {
    let db = LocalDb::open(db_path).context("open local database")?;
    let server_url = db
        .get_config("server_url")
        .ok_or_else(|| anyhow::anyhow!("no server_url configured"))?;
    let token = db
        .get_config("auth_token")
        .ok_or_else(|| anyhow::anyhow!("not logged in"))?;

    let client = ApiClient::new(&server_url, Some(token));

    let _ = tx.send(Action::SyncProgress("Fetching podcast list…".to_string()));
    let podcasts = client.list_podcasts().await.context("fetch podcast list")?;

    // The server's list is authoritative in both directions, so converging on
    // it heals any number of missed subscribe and unsubscribe events.
    let local_ids: HashSet<String> = db.list_podcasts().into_iter().map(|p| p.id).collect();
    let server_ids: HashSet<&str> = podcasts.iter().map(|p| p.id.as_str()).collect();
    let unsubscribed: Vec<String> = local_ids
        .iter()
        .filter(|id| !server_ids.contains(id.as_str()))
        .cloned()
        .collect();

    for p in &podcasts {
        db.upsert_podcast(p);
    }

    let episode_cursor = db.get_sync_state("episode_cursor");
    // Taken before the bootstrap so changes made while it runs are replayed by
    // the delta feed rather than falling into the gap behind the cursor.
    let head = match episode_cursor {
        Some(_) => None,
        None => Some(client.sync_head().await.context("fetch sync head")?),
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

    if let Some(head) = &head {
        db.set_sync_state("episode_cursor", head);
    }

    if let Some(cursor) = &episode_cursor {
        let _ = tx.send(Action::SyncProgress("Syncing episode changes…".to_string()));
        let mut since = cursor.clone();
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

    let dirty = db.list_dirty_progress();
    for (episode_id, progress, done) in &dirty {
        match client.report_progress(episode_id, *progress, *done).await {
            Ok(_) => db.mark_progress_clean(episode_id),
            Err(e) => {
                // Non-fatal: will retry next sync cycle.
                tracing::warn!(episode_id, "failed to push progress: {}", e);
            }
        }
    }

    // Purging comes after the push above so playback progress recorded against
    // an unsubscribed podcast still reaches the server before its rows go.
    if !unsubscribed.is_empty() {
        let _ = tx.send(Action::SyncProgress(
            "Removing unsubscribed podcasts…".to_string(),
        ));
        for podcast_id in &unsubscribed {
            for path in db.remove_podcast(podcast_id) {
                if let Err(e) = tokio::fs::remove_file(&path).await {
                    tracing::warn!(?path, "failed to delete downloaded audio: {}", e);
                }
            }
        }
    }

    Ok(())
}
