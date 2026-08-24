use std::path::PathBuf;

use anyhow::Context;
use rusqlite::{params, Connection, TransactionBehavior};

use pod_model::PodcastWithEpisodeStats;
use crate::app::EpisodeRow;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS config (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS sync_state (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS podcast (
    id                    TEXT PRIMARY KEY,
    title                 TEXT NOT NULL,
    description           TEXT NOT NULL,
    image_link            TEXT NOT NULL,
    feed_url              TEXT NOT NULL,
    feed_type             TEXT NOT NULL,
    created_at            TEXT NOT NULL,
    last_updated          TEXT NOT NULL,
    last_publication_date TEXT
);

CREATE TABLE IF NOT EXISTS episode (
    id                    TEXT PRIMARY KEY,
    podcast_id            TEXT NOT NULL REFERENCES podcast(id),
    title                 TEXT NOT NULL,
    summary               TEXT NOT NULL,
    summary_type          TEXT NOT NULL,
    content_encoded       TEXT NOT NULL DEFAULT '',
    content_encoded_type  TEXT NOT NULL DEFAULT '',
    publication_date      TEXT NOT NULL,
    audio_url             TEXT NOT NULL,
    audio_type            TEXT NOT NULL,
    audio_duration        INTEGER NOT NULL,
    thumbnail_url         TEXT,
    created_at            TEXT NOT NULL,
    last_updated          TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS episode_progress (
    episode_id  TEXT PRIMARY KEY REFERENCES episode(id),
    progress    INTEGER NOT NULL DEFAULT 0,
    done        INTEGER NOT NULL DEFAULT 0,
    dirty       INTEGER NOT NULL DEFAULT 0,
    updated_at  TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS episode_download (
    episode_id       TEXT PRIMARY KEY REFERENCES episode(id),
    file_path        TEXT NOT NULL,
    total_bytes      INTEGER NOT NULL DEFAULT 0,
    downloaded_bytes INTEGER NOT NULL DEFAULT 0,
    status           TEXT NOT NULL DEFAULT 'pending',
    error            TEXT,
    started_at       TEXT NOT NULL,
    completed_at     TEXT
);
"#;

// ==============================================================================
// Download tracking
// ==============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadStatus {
    Pending,
    Downloading,
    Complete,
    Failed,
}

impl DownloadStatus {
    fn from_str(s: &str) -> Self {
        match s {
            "downloading" => Self::Downloading,
            "complete" => Self::Complete,
            "failed" => Self::Failed,
            _ => Self::Pending,
        }
    }

}

pub struct LocalDb {
    conn: Connection,
    path: String,
}

impl LocalDb {
    /// Open (or create) the local SQLite database at the given path.
    /// Use ":memory:" for tests.
    pub fn open(path: &str) -> anyhow::Result<Self> {
        let conn = if path == ":memory:" {
            Connection::open_in_memory()?
        } else {
            Connection::open(path)?
        };
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        // The UI and background tasks each hold their own connection, so any
        // write can meet a lock held by another. The busy handler makes lock
        // acquisition wait instead of failing outright. It does not cover a
        // deferred transaction upgrading its read snapshot to a write, which
        // is why `remove_podcast` begins its transaction as immediate.
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .context("set busy timeout")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn,
            path: path.to_string(),
        })
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    // ==========================================================================
    // Config
    // ==========================================================================

    pub fn get_config(&self, key: &str) -> Option<String> {
        self.conn
            .query_row(
                "SELECT value FROM config WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .ok()
    }

    pub fn set_config(&self, key: &str, value: &str) {
        self.conn
            .execute(
                "INSERT INTO config (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .expect("failed to set config");
    }

    // ==========================================================================
    // Sync state
    // ==========================================================================

    pub fn get_sync_state(&self, key: &str) -> Option<String> {
        self.conn
            .query_row(
                "SELECT value FROM sync_state WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .ok()
    }

    pub fn set_sync_state(&self, key: &str, value: &str) {
        self.conn
            .execute(
                "INSERT INTO sync_state (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .expect("failed to set sync state");
    }

    // ==========================================================================
    // Podcasts
    // ==========================================================================

    pub fn upsert_podcast(&self, p: &PodcastWithEpisodeStats) {
        self.conn
            .execute(
                "INSERT INTO podcast (id, title, description, image_link, feed_url, feed_type, created_at, last_updated, last_publication_date)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(id) DO UPDATE SET
                     title = excluded.title,
                     description = excluded.description,
                     image_link = excluded.image_link,
                     feed_url = excluded.feed_url,
                     feed_type = excluded.feed_type,
                     last_updated = excluded.last_updated,
                     last_publication_date = excluded.last_publication_date",
                params![
                    p.id,
                    p.title,
                    p.description,
                    p.image_link,
                    p.feed_url,
                    p.feed_type,
                    p.created_at.to_rfc3339(),
                    p.last_updated.to_rfc3339(),
                    p.last_publication_date.map(|d| d.to_rfc3339()),
                ],
            )
            .expect("failed to upsert podcast");
    }

    pub fn list_podcasts(&self) -> Vec<PodcastWithEpisodeStats> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, title, description, image_link, feed_url, feed_type,
                        created_at, last_updated, last_publication_date
                 FROM podcast ORDER BY last_publication_date DESC NULLS LAST",
            )
            .expect("failed to prepare podcast query");

        stmt.query_map([], |row| {
            Ok(PodcastWithEpisodeStats {
                id: row.get(0)?,
                title: row.get(1)?,
                description: row.get(2)?,
                image_link: row.get(3)?,
                feed_url: row.get(4)?,
                feed_type: row.get(5)?,
                created_at: parse_datetime(row.get::<_, String>(6)?),
                last_updated: parse_datetime(row.get::<_, String>(7)?),
                last_publication_date: row
                    .get::<_, Option<String>>(8)?
                    .map(parse_datetime),
                feed_etag: None,
                feed_last_modified: None,
            })
        })
        .expect("failed to query podcasts")
        .filter_map(|r| r.ok())
        .collect()
    }

    pub fn podcast_exists(&self, podcast_id: &str) -> bool {
        match self.conn.query_row(
            "SELECT 1 FROM podcast WHERE id = ?1",
            params![podcast_id],
            |_| Ok(()),
        ) {
            Ok(()) => true,
            Err(rusqlite::Error::QueryReturnedNoRows) => false,
            Err(e) => panic!("failed to query podcast existence: {e}"),
        }
    }

    /// Whether this podcast's back catalogue has been fetched in full.
    ///
    /// The `podcast` row cannot carry this: episodes reference it, so it has to
    /// exist before the first episode can be written. The marker is set only
    /// once the last page has landed, which makes an interrupted bootstrap
    /// indistinguishable from one that never started.
    pub fn podcast_bootstrapped(&self, podcast_id: &str) -> bool {
        self.get_sync_state(&bootstrap_key(podcast_id)).is_some()
    }

    pub fn mark_podcast_bootstrapped(&self, podcast_id: &str) {
        self.set_sync_state(&bootstrap_key(podcast_id), "1");
    }

    /// Delete a podcast, its episodes, their progress and their downloads,
    /// including their audio files on disk.
    ///
    /// One transaction, so a crash cannot leave a half-removed podcast behind.
    /// The files are unlinked before the row deletion commits: a file gone
    /// while its row survives is retried on the next cycle (a missing file
    /// counts as already deleted), whereas a row gone while its file survives
    /// is an orphan nothing references and nothing sweeps. An unlink failure
    /// rolls the rows back and defers the whole podcast to the next cycle.
    ///
    /// The transaction begins as immediate because it reads before it writes.
    /// A deferred transaction that has taken a read snapshot cannot upgrade to
    /// a writer once another connection has committed; SQLite reports
    /// `SQLITE_BUSY` for that without invoking the busy handler, and the UI's
    /// per-second progress writes on their own connection make that collision
    /// routine. Acquiring the write lock at `BEGIN` keeps the busy handler in
    /// play.
    ///
    /// Errors are returned rather than panicking: the caller runs inside the
    /// sync task, and a purge that loses a lock race must defer to the next
    /// cycle instead of tearing the task down.
    pub fn remove_podcast(&mut self, podcast_id: &str) -> anyhow::Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("begin podcast removal transaction")?;

        const EPISODES_OF_PODCAST: &str = "SELECT id FROM episode WHERE podcast_id = ?1";

        let file_paths: Vec<PathBuf> = {
            let mut stmt = tx
                .prepare(&format!(
                    "SELECT file_path FROM episode_download
                     WHERE episode_id IN ({EPISODES_OF_PODCAST})"
                ))
                .context("prepare download path query")?;
            let paths = stmt
                .query_map(params![podcast_id], |row| {
                    Ok(PathBuf::from(row.get::<_, String>(0)?))
                })
                .context("query download paths")?
                .filter_map(|r| r.ok())
                .collect();
            paths
        };

        tx.execute(
            &format!("DELETE FROM episode_download WHERE episode_id IN ({EPISODES_OF_PODCAST})"),
            params![podcast_id],
        )
        .context("delete downloads")?;
        tx.execute(
            &format!("DELETE FROM episode_progress WHERE episode_id IN ({EPISODES_OF_PODCAST})"),
            params![podcast_id],
        )
        .context("delete progress")?;
        tx.execute(
            "DELETE FROM episode WHERE podcast_id = ?1",
            params![podcast_id],
        )
        .context("delete episodes")?;
        tx.execute("DELETE FROM podcast WHERE id = ?1", params![podcast_id])
            .context("delete podcast")?;
        tx.execute(
            "DELETE FROM sync_state WHERE key = ?1",
            params![bootstrap_key(podcast_id)],
        )
        .context("clear bootstrap marker")?;

        for path in &file_paths {
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(e).with_context(|| {
                        format!("delete downloaded audio {}", path.display())
                    });
                }
            }
        }

        tx.commit().context("commit podcast removal")?;
        Ok(())
    }

    // ==========================================================================
    // Episodes
    // ==========================================================================

    pub fn upsert_episode(&self, e: &pod_model::Episode) {
        self.conn
            .execute(
                "INSERT INTO episode (id, podcast_id, title, summary, summary_type, content_encoded, content_encoded_type, publication_date, audio_url, audio_type, audio_duration, thumbnail_url, created_at, last_updated)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
                 ON CONFLICT(id) DO UPDATE SET
                     title = excluded.title,
                     summary = excluded.summary,
                     summary_type = excluded.summary_type,
                     content_encoded = excluded.content_encoded,
                     content_encoded_type = excluded.content_encoded_type,
                     audio_url = excluded.audio_url,
                     audio_type = excluded.audio_type,
                     audio_duration = excluded.audio_duration,
                     thumbnail_url = excluded.thumbnail_url,
                     last_updated = excluded.last_updated",
                params![
                    e.id,
                    e.podcast_id,
                    e.title,
                    e.summary,
                    e.summary_type,
                    e.content_encoded,
                    e.content_encoded_type,
                    e.publication_date.to_rfc3339(),
                    e.audio_url,
                    e.audio_type,
                    e.audio_duration,
                    e.thumbnail_url,
                    e.created_at.to_rfc3339(),
                    e.last_updated.to_rfc3339(),
                ],
            )
            .expect("failed to upsert episode");
    }

    pub fn episode_podcast_id(&self, episode_id: &str) -> Option<String> {
        self.conn
            .query_row(
                "SELECT podcast_id FROM episode WHERE id = ?1",
                params![episode_id],
                |row| row.get(0),
            )
            .ok()
    }

    pub fn list_episodes(&self, podcast_id: &str) -> Vec<EpisodeRow> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT e.id, e.podcast_id, e.title, e.publication_date,
                        e.audio_url, e.audio_duration,
                        e.summary, e.content_encoded,
                        COALESCE(ep.progress, 0), COALESCE(ep.done, 0),
                        ed.status
                 FROM episode e
                 LEFT JOIN episode_progress ep ON ep.episode_id = e.id
                 LEFT JOIN episode_download ed ON ed.episode_id = e.id
                 WHERE e.podcast_id = ?1
                 ORDER BY e.publication_date DESC",
            )
            .expect("failed to prepare episode query");

        stmt.query_map(params![podcast_id], |row| {
            Ok(EpisodeRow {
                id: row.get(0)?,
                podcast_id: row.get(1)?,
                title: row.get(2)?,
                publication_date: row.get(3)?,
                audio_url: row.get(4)?,
                audio_duration: row.get(5)?,
                summary: row.get(6)?,
                content_encoded: row.get(7)?,
                progress: row.get(8)?,
                done: row.get::<_, i32>(9)? != 0,
                podcast_title: None,
                download_status: row
                    .get::<_, Option<String>>(10)?
                    .map(|s| DownloadStatus::from_str(&s)),
            })
        })
        .expect("failed to query episodes")
        .filter_map(|r| r.ok())
        .collect()
    }

    /// List episodes across all podcasts, excluding done episodes, sorted by
    /// publication date descending. Used for the inbox view.
    pub fn list_inbox_episodes(&self, limit: i64, offset: i64) -> Vec<EpisodeRow> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT e.id, e.podcast_id, e.title, e.publication_date,
                        e.audio_url, e.audio_duration,
                        e.summary, e.content_encoded,
                        COALESCE(ep.progress, 0), COALESCE(ep.done, 0),
                        p.title,
                        ed.status
                 FROM episode e
                 JOIN podcast p ON p.id = e.podcast_id
                 LEFT JOIN episode_progress ep ON ep.episode_id = e.id
                 LEFT JOIN episode_download ed ON ed.episode_id = e.id
                 WHERE COALESCE(ep.done, 0) = 0
                 ORDER BY e.publication_date DESC
                 LIMIT ?1 OFFSET ?2",
            )
            .expect("inbox query is valid static SQL");

        stmt.query_map(params![limit, offset], |row| {
            Ok(EpisodeRow {
                id: row.get(0)?,
                podcast_id: row.get(1)?,
                title: row.get(2)?,
                publication_date: row.get(3)?,
                audio_url: row.get(4)?,
                audio_duration: row.get(5)?,
                summary: row.get(6)?,
                content_encoded: row.get(7)?,
                progress: row.get(8)?,
                done: row.get::<_, i32>(9)? != 0,
                podcast_title: row.get(10)?,
                download_status: row
                    .get::<_, Option<String>>(11)?
                    .map(|s| DownloadStatus::from_str(&s)),
            })
        })
        .expect("inbox query execution")
        .filter_map(|r| r.ok())
        .collect()
    }

    // ==========================================================================
    // Progress
    // ==========================================================================

    /// Record playback progress for an episode.
    ///
    /// A dirty row holds progress the server has not seen yet, which makes it
    /// strictly newer than anything the server can echo back. A clean
    /// (server-sourced) write therefore leaves a dirty row untouched; only the
    /// dirty push, via `mark_progress_clean`, reopens it to server values.
    /// Encoding that in the `DO UPDATE` makes "a server value never overwrites
    /// unpushed local progress" a property of the table rather than of the
    /// order in which sync steps run. A locally-sourced (dirty) write wins
    /// unconditionally.
    ///
    /// Progress for an episode the client does not store is dropped rather than
    /// treated as an error: the server's progress feed is cursored on time, not
    /// on subscription, so it can name an episode that a purge has removed.
    pub fn upsert_progress(&self, episode_id: &str, progress: i32, done: bool, dirty: bool) {
        let now = chrono::Utc::now().to_rfc3339();
        let result = self
            .conn
            .execute(
                "INSERT INTO episode_progress (episode_id, progress, done, dirty, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(episode_id) DO UPDATE SET
                     progress = excluded.progress,
                     done = excluded.done,
                     dirty = excluded.dirty,
                     updated_at = excluded.updated_at
                 WHERE excluded.dirty = 1 OR episode_progress.dirty = 0",
                params![episode_id, progress, done as i32, dirty as i32, now],
            );
        match result {
            Ok(_) => {}
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                tracing::warn!(episode_id, "dropping progress for an unknown episode");
            }
            Err(e) => panic!("failed to upsert progress: {e}"),
        }
    }

    pub fn list_dirty_progress(&self) -> Vec<(String, i32, bool)> {
        let mut stmt = self
            .conn
            .prepare("SELECT episode_id, progress, done FROM episode_progress WHERE dirty = 1")
            .expect("failed to prepare dirty progress query");

        stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i32>(1)?,
                row.get::<_, i32>(2)? != 0,
            ))
        })
        .expect("failed to query dirty progress")
        .filter_map(|r| r.ok())
        .collect()
    }

    /// Whether any episode of this podcast holds progress the server has not
    /// received yet. Purging such a podcast would destroy that progress with
    /// no retry left, so the purge asks first.
    pub fn podcast_has_dirty_progress(&self, podcast_id: &str) -> bool {
        match self.conn.query_row(
            "SELECT 1 FROM episode_progress ep
             JOIN episode e ON e.id = ep.episode_id
             WHERE ep.dirty = 1 AND e.podcast_id = ?1
             LIMIT 1",
            params![podcast_id],
            |_| Ok(()),
        ) {
            Ok(()) => true,
            Err(rusqlite::Error::QueryReturnedNoRows) => false,
            Err(e) => panic!("failed to query dirty progress for podcast: {e}"),
        }
    }

    pub fn mark_progress_clean(&self, episode_id: &str) {
        self.conn
            .execute(
                "UPDATE episode_progress SET dirty = 0 WHERE episode_id = ?1",
                params![episode_id],
            )
            .expect("failed to mark progress clean");
    }

    // ==========================================================================
    // Downloads
    // ==========================================================================

    pub fn insert_download(&self, episode_id: &str, file_path: &str) {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn
            .execute(
                "INSERT INTO episode_download (episode_id, file_path, status, started_at)
                 VALUES (?1, ?2, 'pending', ?3)
                 ON CONFLICT(episode_id) DO UPDATE SET
                     file_path = excluded.file_path,
                     status = 'pending',
                     total_bytes = 0,
                     downloaded_bytes = 0,
                     error = NULL,
                     started_at = excluded.started_at,
                     completed_at = NULL",
                params![episode_id, file_path, now],
            )
            .expect("failed to insert download");
    }

    pub fn update_download_progress(
        &self,
        episode_id: &str,
        downloaded_bytes: i64,
        total_bytes: i64,
    ) {
        self.conn
            .execute(
                "UPDATE episode_download
                 SET downloaded_bytes = ?2, total_bytes = ?3, status = 'downloading'
                 WHERE episode_id = ?1",
                params![episode_id, downloaded_bytes, total_bytes],
            )
            .expect("failed to update download progress");
    }

    /// Mark a download complete. Returns false when no download row matched,
    /// which means the episode was purged while the download was in flight;
    /// the caller must unlink the file it wrote, since no row references it
    /// and nothing sweeps the downloads directory.
    #[must_use]
    pub fn complete_download(&self, episode_id: &str) -> bool {
        let now = chrono::Utc::now().to_rfc3339();
        let rows = self
            .conn
            .execute(
                "UPDATE episode_download
                 SET status = 'complete', completed_at = ?2
                 WHERE episode_id = ?1",
                params![episode_id, now],
            )
            .expect("failed to complete download");
        rows == 1
    }

    pub fn fail_download(&self, episode_id: &str, error: &str) {
        self.conn
            .execute(
                "UPDATE episode_download SET status = 'failed', error = ?2 WHERE episode_id = ?1",
                params![episode_id, error],
            )
            .expect("failed to mark download as failed");
    }

    /// Return the download status for an episode, or None if never downloaded.
    pub fn get_download_status(&self, episode_id: &str) -> Option<DownloadStatus> {
        self.conn
            .query_row(
                "SELECT status FROM episode_download WHERE episode_id = ?1",
                params![episode_id],
                |row| {
                    let status: String = row.get(0)?;
                    Ok(DownloadStatus::from_str(&status))
                },
            )
            .ok()
    }

    /// Return the local file path for a completed download, verifying the file
    /// still exists on disk. If the file was deleted externally, the download
    /// record is removed so the user can re-trigger.
    pub fn get_download_path(&self, episode_id: &str) -> Option<String> {
        let row: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT file_path, status FROM episode_download WHERE episode_id = ?1",
                params![episode_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .ok();

        match row {
            Some((path, status)) if status == "complete" => {
                if std::path::Path::new(&path).exists() {
                    Some(path)
                } else {
                    // File was removed externally — clean up the stale record.
                    self.delete_download(episode_id);
                    None
                }
            }
            _ => None,
        }
    }

    pub fn delete_download(&self, episode_id: &str) {
        self.conn
            .execute(
                "DELETE FROM episode_download WHERE episode_id = ?1",
                params![episode_id],
            )
            .expect("failed to delete download");
    }
}

fn bootstrap_key(podcast_id: &str) -> String {
    format!("bootstrapped:{podcast_id}")
}

fn parse_datetime(s: String) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(&s)
        .expect("invalid datetime in local db")
        .with_timezone(&chrono::Utc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_round_trip() {
        let db = LocalDb::open(":memory:").unwrap();
        assert_eq!(db.get_config("key"), None);
        db.set_config("key", "value");
        assert_eq!(db.get_config("key"), Some("value".to_string()));
        db.set_config("key", "updated");
        assert_eq!(db.get_config("key"), Some("updated".to_string()));
    }

    #[test]
    fn sync_state_round_trip() {
        let db = LocalDb::open(":memory:").unwrap();
        assert_eq!(db.get_sync_state("cursor"), None);
        db.set_sync_state("cursor", "abc123");
        assert_eq!(db.get_sync_state("cursor"), Some("abc123".to_string()));
    }

    #[test]
    fn podcast_upsert_and_list() {
        let db = LocalDb::open(":memory:").unwrap();
        let now = chrono::Utc::now();
        let p = PodcastWithEpisodeStats {
            id: "p1".to_string(),
            title: "Test Pod".to_string(),
            description: "A test".to_string(),
            image_link: "https://img.example".to_string(),
            feed_url: "https://feed.example".to_string(),
            feed_type: "rss".to_string(),
            created_at: now,
            last_updated: now,
            last_publication_date: Some(now),
            feed_etag: None,
            feed_last_modified: None,
        };
        db.upsert_podcast(&p);
        let list = db.list_podcasts();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].title, "Test Pod");
    }

    #[test]
    fn episode_upsert_and_list() {
        let db = LocalDb::open(":memory:").unwrap();
        let now = chrono::Utc::now();

        // Need a podcast first.
        let p = PodcastWithEpisodeStats {
            id: "p1".to_string(),
            title: "Test".to_string(),
            description: String::new(),
            image_link: String::new(),
            feed_url: String::new(),
            feed_type: "rss".to_string(),
            created_at: now,
            last_updated: now,
            last_publication_date: None,
            feed_etag: None,
            feed_last_modified: None,
        };
        db.upsert_podcast(&p);

        let e = pod_model::Episode {
            id: "e1".to_string(),
            podcast_id: "p1".to_string(),
            title: "Episode 1".to_string(),
            summary: "Summary".to_string(),
            summary_type: "text/plain".to_string(),
            content_encoded: "<p>Rich</p>".to_string(),
            content_encoded_type: "text/html".to_string(),
            publication_date: now,
            audio_url: "https://audio.example/ep1.mp3".to_string(),
            audio_type: "audio/mpeg".to_string(),
            audio_duration: 3600,
            thumbnail_url: None,
            created_at: now,
            last_updated: now,
        };
        db.upsert_episode(&e);

        let episodes = db.list_episodes("p1");
        assert_eq!(episodes.len(), 1);
        assert_eq!(episodes[0].title, "Episode 1");
        assert_eq!(episodes[0].progress, 0);
        assert!(!episodes[0].done);
    }

    #[test]
    fn progress_dirty_flow() {
        let db = LocalDb::open(":memory:").unwrap();
        let now = chrono::Utc::now();

        let p = PodcastWithEpisodeStats {
            id: "p1".to_string(),
            title: "Test".to_string(),
            description: String::new(),
            image_link: String::new(),
            feed_url: String::new(),
            feed_type: "rss".to_string(),
            created_at: now,
            last_updated: now,
            last_publication_date: None,
            feed_etag: None,
            feed_last_modified: None,
        };
        db.upsert_podcast(&p);

        let e = pod_model::Episode {
            id: "e1".to_string(),
            podcast_id: "p1".to_string(),
            title: "Ep".to_string(),
            summary: String::new(),
            summary_type: String::new(),
            content_encoded: String::new(),
            content_encoded_type: String::new(),
            publication_date: now,
            audio_url: "https://audio.example/ep.mp3".to_string(),
            audio_type: "audio/mpeg".to_string(),
            audio_duration: 100,
            thumbnail_url: None,
            created_at: now,
            last_updated: now,
        };
        db.upsert_episode(&e);

        // Mark progress as dirty (local change).
        db.upsert_progress("e1", 42, false, true);
        let dirty = db.list_dirty_progress();
        assert_eq!(dirty.len(), 1);
        assert_eq!(dirty[0], ("e1".to_string(), 42, false));

        // After syncing, mark clean.
        db.mark_progress_clean("e1");
        assert!(db.list_dirty_progress().is_empty());
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

    fn test_episode(id: &str, podcast_id: &str) -> pod_model::Episode {
        let now = chrono::Utc::now();
        pod_model::Episode {
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

    /// A directory of real files for exercising the purge's unlink behavior.
    /// Unique per test so parallel tests do not collide.
    fn download_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pod-tui-test-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir is writable");
        dir
    }

    fn touch(path: &std::path::Path) {
        std::fs::write(path, b"audio").expect("temp file is writable");
    }

    #[test]
    fn remove_podcast_purges_episodes_progress_downloads_and_files() {
        let mut db = LocalDb::open(":memory:").unwrap();
        db.upsert_podcast(&test_podcast("p1"));
        db.upsert_episode(&test_episode("e1", "p1"));
        db.upsert_episode(&test_episode("e2", "p1"));
        db.upsert_progress("e1", 42, false, true);

        let dir = download_dir("purge");
        let e1_file = dir.join("e1.mp3");
        touch(&e1_file);
        db.insert_download("e1", e1_file.to_str().unwrap());

        db.remove_podcast("p1").expect("purge succeeds");

        assert!(!e1_file.exists());
        assert!(db.list_podcasts().is_empty());
        assert!(db.list_episodes("p1").is_empty());
        assert!(db.list_dirty_progress().is_empty());
        assert_eq!(db.get_download_status("e1"), None);
        std::fs::remove_dir_all(&dir).expect("cleanup temp dir");
    }

    #[test]
    fn remove_podcast_leaves_other_podcasts_intact() {
        let mut db = LocalDb::open(":memory:").unwrap();
        db.upsert_podcast(&test_podcast("p1"));
        db.upsert_podcast(&test_podcast("p2"));
        db.upsert_episode(&test_episode("e1", "p1"));
        db.upsert_episode(&test_episode("e2", "p2"));
        db.upsert_progress("e1", 10, false, true);
        db.upsert_progress("e2", 20, true, true);

        let dir = download_dir("intact");
        let e1_file = dir.join("e1.mp3");
        let e2_file = dir.join("e2.mp3");
        touch(&e1_file);
        touch(&e2_file);
        db.insert_download("e1", e1_file.to_str().unwrap());
        db.insert_download("e2", e2_file.to_str().unwrap());

        db.remove_podcast("p1").expect("purge succeeds");

        assert!(!e1_file.exists());
        assert!(e2_file.exists());
        let podcasts = db.list_podcasts();
        assert_eq!(podcasts.len(), 1);
        assert_eq!(podcasts[0].id, "p2");
        assert_eq!(db.list_episodes("p2").len(), 1);
        assert_eq!(
            db.list_dirty_progress(),
            vec![("e2".to_string(), 20, true)]
        );
        assert_eq!(db.get_download_status("e2"), Some(DownloadStatus::Pending));
        std::fs::remove_dir_all(&dir).expect("cleanup temp dir");
    }

    /// A download row whose file is already gone (deleted externally, or by a
    /// purge attempt that crashed between unlink and commit) must not block
    /// the retry.
    #[test]
    fn remove_podcast_tolerates_a_missing_download_file() {
        let mut db = LocalDb::open(":memory:").unwrap();
        db.upsert_podcast(&test_podcast("p1"));
        db.upsert_episode(&test_episode("e1", "p1"));
        db.insert_download("e1", "/nonexistent/e1.mp3");

        db.remove_podcast("p1").expect("purge succeeds");
        assert!(db.list_podcasts().is_empty());
    }

    #[test]
    fn remove_podcast_without_episodes_succeeds() {
        let mut db = LocalDb::open(":memory:").unwrap();
        db.upsert_podcast(&test_podcast("p1"));

        db.remove_podcast("p1").expect("purge succeeds");
        assert!(db.list_podcasts().is_empty());
    }

    /// A purge racing an in-flight download deletes the download row before
    /// the task finishes writing the file. The completion report must say so,
    /// so the task can unlink the file nothing references.
    #[test]
    fn complete_download_reports_a_purged_row() {
        let mut db = LocalDb::open(":memory:").unwrap();
        db.upsert_podcast(&test_podcast("p1"));
        db.upsert_episode(&test_episode("e1", "p1"));
        db.insert_download("e1", "/downloads/e1.mp3");

        assert!(db.complete_download("e1"));

        db.remove_podcast("p1").expect("purge succeeds");
        assert!(!db.complete_download("e1"));
    }

    #[test]
    fn podcast_exists_tracks_removal() {
        let mut db = LocalDb::open(":memory:").unwrap();
        assert!(!db.podcast_exists("p1"));
        db.upsert_podcast(&test_podcast("p1"));
        assert!(db.podcast_exists("p1"));
        db.remove_podcast("p1").expect("purge succeeds");
        assert!(!db.podcast_exists("p1"));
    }

    /// The sync reconcile bootstraps any subscribed podcast that is not marked
    /// bootstrapped, and sets the marker only after the last page of episodes
    /// has landed. A bootstrap cut short therefore runs again on the next sync,
    /// re-upserting what it already wrote.
    #[test]
    fn interrupted_bootstrap_is_retried_and_resubscribing_starts_over() {
        let mut db = LocalDb::open(":memory:").unwrap();
        db.upsert_podcast(&test_podcast("p1"));
        db.upsert_episode(&test_episode("e1", "p1"));

        // The marker is absent while the pagination is still in flight, so the
        // podcast is picked up again even though its row and part of its
        // episodes are already stored.
        assert!(!db.podcast_bootstrapped("p1"));

        db.upsert_episode(&test_episode("e1", "p1"));
        db.upsert_episode(&test_episode("e2", "p1"));
        db.mark_podcast_bootstrapped("p1");

        assert!(db.podcast_bootstrapped("p1"));
        assert_eq!(db.list_episodes("p1").len(), 2);

        // Unsubscribing clears the marker, so a re-subscribe fetches the back
        // catalogue again instead of leaving the podcast permanently empty.
        db.remove_podcast("p1").expect("purge succeeds");
        assert!(!db.podcast_bootstrapped("p1"));
    }

    /// A dirty row holds progress the server has not seen, so it is by
    /// definition newer than any value the server can echo back. A clean
    /// (server-sourced) write must leave it alone; the dirty push delivers it
    /// and marks it clean, after which server values apply again.
    #[test]
    fn server_write_does_not_clobber_dirty_local_progress() {
        let db = LocalDb::open(":memory:").unwrap();
        db.upsert_podcast(&test_podcast("p1"));
        db.upsert_episode(&test_episode("e1", "p1"));
        db.upsert_episode(&test_episode("e2", "p1"));

        // Offline listening: 20 minutes into e1, e2 finished.
        db.upsert_progress("e1", 1200, false, true);
        db.upsert_progress("e2", 3600, true, true);

        // A bootstrap re-fetch delivers the server's stale values.
        db.upsert_progress("e1", 0, false, false);
        db.upsert_progress("e2", 0, false, false);

        let mut dirty = db.list_dirty_progress();
        dirty.sort();
        assert_eq!(
            dirty,
            vec![
                ("e1".to_string(), 1200, false),
                ("e2".to_string(), 3600, true),
            ]
        );

        // A local write still wins unconditionally.
        db.upsert_progress("e1", 1300, false, true);
        // A clean row accepts server values.
        db.mark_progress_clean("e2");
        db.upsert_progress("e2", 100, false, false);
        assert_eq!(db.list_dirty_progress(), vec![("e1".to_string(), 1300, false)]);
        let episodes = db.list_episodes("p1");
        let e2 = episodes.iter().find(|e| e.id == "e2").unwrap();
        assert_eq!((e2.progress, e2.done), (100, false));
    }

    #[test]
    fn podcast_dirty_progress_and_episode_lookup() {
        let db = LocalDb::open(":memory:").unwrap();
        db.upsert_podcast(&test_podcast("p1"));
        db.upsert_podcast(&test_podcast("p2"));
        db.upsert_episode(&test_episode("e1", "p1"));
        db.upsert_episode(&test_episode("e2", "p2"));

        assert_eq!(db.episode_podcast_id("e1"), Some("p1".to_string()));
        assert_eq!(db.episode_podcast_id("ghost"), None);

        assert!(!db.podcast_has_dirty_progress("p1"));
        db.upsert_progress("e1", 42, false, true);
        assert!(db.podcast_has_dirty_progress("p1"));
        assert!(!db.podcast_has_dirty_progress("p2"));
        db.mark_progress_clean("e1");
        assert!(!db.podcast_has_dirty_progress("p1"));
    }

    /// `/sync/progress` is cursored on time, not on subscription, so it can
    /// carry an episode the client purged with its podcast. Dropping that row
    /// is correct: if the user re-subscribes, the bootstrap fetches the
    /// progress back along with the episode.
    #[test]
    fn progress_for_an_unknown_episode_is_dropped() {
        let db = LocalDb::open(":memory:").unwrap();
        db.upsert_progress("gone", 30, false, false);
        assert!(db.list_dirty_progress().is_empty());
    }
}
