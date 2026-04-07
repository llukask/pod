use anyhow::Context;
use tokio::sync::mpsc;

use crate::api_client::ApiClient;
use crate::app::Action;
use crate::local_db::LocalDb;

/// Run a full sync cycle: pull podcast list, pull episode changes, pull
/// progress changes, and push dirty local progress.
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

    // ---- 1. Pull podcast list ----
    let _ = tx.send(Action::SyncProgress("Fetching podcast list…".to_string()));
    let podcasts = client.list_podcasts().await.context("fetch podcast list")?;
    let total = podcasts.len();
    for p in &podcasts {
        db.upsert_podcast(p);
    }

    // ---- 2. Pull episode changes (delta sync) ----
    let episode_cursor = db.get_sync_state("episode_cursor");
    if let Some(cursor) = &episode_cursor {
        // Incremental: use /sync/changes.
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
    } else {
        // Initial bootstrap: get head cursor, then fetch all episodes via
        // the paginated episode list API.
        let head = client.sync_head().await.context("fetch sync head")?;

        for (i, p) in podcasts.iter().enumerate() {
            let _ = tx.send(Action::SyncProgress(format!(
                "Syncing episodes ({}/{total}): {}",
                i + 1,
                p.title
            )));
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
        }

        db.set_sync_state("episode_cursor", &head);
    }

    // ---- 3. Pull progress changes ----
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

    // ---- 4. Push dirty local progress ----
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

    Ok(())
}
