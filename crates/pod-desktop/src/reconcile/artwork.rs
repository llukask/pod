//! The artwork cache (spec 7.5): the reconciler's step that keeps the
//! images the views show in `<data_dir>/artwork`, so the sidebar and the
//! inbox have artwork offline.
//!
//! Like the download step it is load, a pure [`plan`], then the side
//! effects. Every cached file has an `artwork` row, written only once the
//! file is complete. Views join that table on the `artwork_url` columns and
//! hand GPUI the local file, never the URL, so nothing is fetched from the
//! render path, and their live queries pick up an image when it arrives.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use color_eyre::eyre::{Result, WrapErr, bail};
use futures_util::StreamExt as _;
use pod_protocol::Timestamp;
use rusqlite::{Connection, Transaction, params};
use sha2::{Digest as _, Sha256};
use tokio_util::sync::CancellationToken;

use crate::inbox;
use crate::net::NetworkState;
use crate::store::Store;

const MIB: u64 = 1024 * 1024;

/// The cache's size cap (spec 7.5).
pub const CAP: u64 = 200 * MIB;
/// What fitting assumes for an image not fetched yet.
pub const ESTIMATE: u64 = 256 * 1024;
/// How stale `last_used` may get on a wanted entry before a pass bumps it.
/// Bumping on every pass would write on every pass.
pub const TOUCH_AFTER_MS: i64 = 60 * 60 * 1000;

// ==============================================================================
// Inputs
// ==============================================================================

/// An `artwork_url` a view shows, with the podcast it belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    pub url: String,
    pub podcast_id: String,
}

/// An `artwork` row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub url: String,
    pub file: String,
    pub bytes: u64,
    pub last_used: i64,
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    /// Subscribed podcasts' artwork, then inbox episodes' newest first: the
    /// order in which the cap lets them in. URLs may repeat.
    pub sources: Vec<Source>,
    /// Podcasts whose auto downloads may use a metered network.
    pub auto_on_metered: HashSet<String>,
    /// `download_on_metered`.
    pub on_metered: bool,
    pub cached: Vec<Entry>,
}

// ==============================================================================
// Plan
// ==============================================================================

/// A URL the cache should hold, in priority order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wanted {
    pub url: String,
    /// The auto-download rule of spec 7.4: `download_on_metered`, or
    /// `auto_on_metered` on any podcast showing this image.
    pub metered_ok: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Least recently used first. Delete the row, then the file.
    pub evict: Vec<Entry>,
    /// Wanted entries whose `last_used` to set to now.
    pub touch: Vec<String>,
    pub fetch: Vec<String>,
}

/// The URLs to cache, each once at its first position.
pub fn wanted(snapshot: &Snapshot) -> Vec<Wanted> {
    let mut wanted: Vec<Wanted> = Vec::new();
    let mut position: HashMap<&str, usize> = HashMap::new();
    for source in &snapshot.sources {
        let metered_ok =
            snapshot.on_metered || snapshot.auto_on_metered.contains(&source.podcast_id);
        match position.get(source.url.as_str()) {
            Some(&index) => wanted[index].metered_ok |= metered_ok,
            None => {
                position.insert(&source.url, wanted.len());
                wanted.push(Wanted {
                    url: source.url.clone(),
                    metered_ok,
                });
            }
        }
    }
    wanted
}

/// What to evict, touch and fetch at `now`. `retry_at` holds URLs whose
/// last fetch failed, and when they may be tried again.
///
/// The cap is fitted before fetching, the way spec 7.4 fits downloads to
/// their budget, so a pass never fetches an image the next pass evicts.
/// Wanted images enter in priority order while they fit, sized by their
/// file or by [`ESTIMATE`]. Everything else is evictable, but stays cached
/// until a fetch needs its room.
pub fn plan(
    snapshot: &Snapshot,
    now: i64,
    network: NetworkState,
    retry_at: &HashMap<String, i64>,
) -> Plan {
    let cached: HashMap<&str, &Entry> = snapshot
        .cached
        .iter()
        .map(|entry| (entry.url.as_str(), entry))
        .collect();

    let mut target: Vec<&Wanted> = Vec::new();
    let mut fitted = 0;
    let wanted = wanted(snapshot);
    for wanted in &wanted {
        let size = cached
            .get(wanted.url.as_str())
            .map_or(ESTIMATE, |entry| entry.bytes);
        if fitted + size <= CAP {
            fitted += size;
            target.push(wanted);
        }
    }

    let touch = target
        .iter()
        .filter_map(|wanted| cached.get(wanted.url.as_str()))
        .filter(|entry| entry.last_used + TOUCH_AFTER_MS <= now)
        .map(|entry| entry.url.clone())
        .collect();

    let fetch: Vec<String> = target
        .iter()
        .filter(|wanted| !cached.contains_key(wanted.url.as_str()))
        .filter(|wanted| network.connected && (!network.metered || wanted.metered_ok))
        .filter(|wanted| retry_at.get(&wanted.url).is_none_or(|&at| at <= now))
        .map(|wanted| wanted.url.clone())
        .collect();

    let protected: HashSet<&str> = target.iter().map(|wanted| wanted.url.as_str()).collect();
    let mut evictable: Vec<&Entry> = snapshot
        .cached
        .iter()
        .filter(|entry| !protected.contains(entry.url.as_str()))
        .collect();
    evictable.sort_by(|a, b| (a.last_used, &a.url).cmp(&(b.last_used, &b.url)));
    let mut needed = snapshot.cached.iter().map(|entry| entry.bytes).sum::<u64>()
        + ESTIMATE * fetch.len() as u64;
    let mut evict = Vec::new();
    for entry in evictable {
        if needed <= CAP {
            break;
        }
        needed = needed.saturating_sub(entry.bytes);
        evict.push(entry.clone());
    }

    Plan {
        evict,
        touch,
        fetch,
    }
}

// ==============================================================================
// Reading and writing the table
// ==============================================================================

/// Reads what [`plan`] needs in one snapshot.
pub fn load(connection: &Connection, now: Timestamp) -> Result<Snapshot> {
    let podcasts: Vec<(String, String)> = connection
        .prepare_cached(
            "select s.podcast_id, coalesce(p.artwork_url, p.local_artwork_url)
             from subscription s join podcast p on p.id = s.podcast_id
             where s.subscribed = 1 and coalesce(p.artwork_url, p.local_artwork_url) is not null
             order by s.subscribed_at desc, s.podcast_id",
        )
        .context("prepare the podcast artwork query")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .context("query the podcast artwork")?
        .collect::<rusqlite::Result<_>>()
        .context("read the podcast artwork")?;
    let episodes = inbox::load_artwork_urls(connection, now).context("read the inbox artwork")?;
    let sources = podcasts
        .into_iter()
        .chain(episodes)
        .map(|(podcast_id, url)| Source { url, podcast_id })
        .collect();

    let auto_on_metered = connection
        .prepare_cached("select podcast_id from podcast_device where auto_on_metered = 1")
        .context("prepare the auto_on_metered query")?
        .query_map([], |row| row.get(0))
        .context("query podcast_device")?
        .collect::<rusqlite::Result<_>>()
        .context("read podcast_device")?;

    let cached = connection
        .prepare_cached("select url, file, bytes, last_used from artwork")
        .context("prepare the artwork query")?
        .query_map([], |row| {
            Ok(Entry {
                url: row.get(0)?,
                file: row.get(1)?,
                bytes: row.get::<_, i64>(2)?.try_into().unwrap_or(0),
                last_used: row.get(3)?,
            })
        })
        .context("query artwork")?
        .collect::<rusqlite::Result<_>>()
        .context("read artwork")?;

    Ok(Snapshot {
        sources,
        auto_on_metered,
        on_metered: super::load_settings(connection)
            .context("read the device settings")?
            .on_metered,
        cached,
    })
}

/// Writes the plan's evictions and touches, and returns the files whose
/// rows it deleted. Deleting the row first takes the image out of the
/// views before its file goes.
pub fn apply(transaction: &Transaction<'_>, plan: &Plan, now: i64) -> Result<Vec<String>> {
    let mut deleted = Vec::new();
    for entry in &plan.evict {
        let changed = transaction
            .execute(
                "delete from artwork where url = ?1 and file = ?2",
                params![entry.url, entry.file],
            )
            .context("evict an artwork row")?;
        if changed > 0 {
            deleted.push(entry.file.clone());
        }
    }
    for url in &plan.touch {
        transaction
            .execute(
                "update artwork set last_used = ?2 where url = ?1",
                params![url, now],
            )
            .context("touch an artwork row")?;
    }
    Ok(deleted)
}

// ==============================================================================
// Files and fetching
// ==============================================================================

/// How many images download at once.
const CONCURRENCY: usize = 4;
/// A larger response is abandoned. Podcast artwork is a few MiB at most.
const MAX_FILE: u64 = 10 * MIB;
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a URL that failed waits before the next try.
const RETRY_AFTER_MS: i64 = 15 * 60 * 1000;

/// The cache file for `url`: the hex SHA-256 of the URL, so any URL makes a
/// safe, fixed-length name. GPUI sniffs the image format from the bytes, so
/// the name needs no extension.
pub fn file_name(url: &str) -> String {
    Sha256::digest(url.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn remove_files(root: &Path, files: &[String]) {
    for file in files {
        super::remove_file(&root.join(file));
    }
}

/// Deletes the files in `root` not named in `files`: `.part` files of
/// fetches a quit interrupted, and files whose rows went without them.
pub fn sweep(root: &Path, files: &HashSet<String>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !files.contains(&name) {
            super::remove_file(&entry.path());
        }
    }
}

/// What a fetch batch shares.
#[derive(Clone, Debug)]
pub struct Fetcher {
    pub store: Store,
    pub client: reqwest::Client,
    pub root: PathBuf,
    /// When each URL whose fetch failed may be tried again. In memory: a
    /// restart retries at once.
    pub retry_at: Arc<std::sync::Mutex<HashMap<String, i64>>>,
}

impl Fetcher {
    /// Fetches `urls`, a few at a time, and records each finished file. A
    /// cancelled batch writes no more rows.
    pub async fn fetch_all(self, urls: Vec<String>, cancel: CancellationToken) {
        futures_util::stream::iter(urls)
            .map(|url| {
                let fetcher = self.clone();
                let cancel = cancel.clone();
                async move {
                    tokio::select! {
                        () = cancel.cancelled() => {}
                        () = fetcher.fetch(url) => {}
                    }
                }
            })
            .buffer_unordered(CONCURRENCY)
            .collect::<()>()
            .await;
    }

    async fn fetch(&self, url: String) {
        let file = file_name(&url);
        let result = self.download(&url, &file).await;
        let now = Timestamp::now().as_epoch_ms();
        let result = match result {
            Ok(bytes) => {
                let url = url.clone();
                self.store
                    .write(move |transaction| {
                        transaction
                            .execute(
                                "insert into artwork (url, file, bytes, last_used)
                                 values (?1, ?2, ?3, ?4)
                                 on conflict (url) do update set file = excluded.file,
                                     bytes = excluded.bytes, last_used = excluded.last_used",
                                params![url, file, i64::try_from(bytes).unwrap_or(i64::MAX), now],
                            )
                            .context("record the artwork file")
                    })
                    .await
                    .map(|_| ())
            }
            Err(error) => Err(error),
        };
        if let Err(error) = result {
            // A connect error is "no connection" (spec 7.2), not this URL's
            // fault: the next pass after connectivity returns tries again.
            let offline = error.chain().any(|cause| {
                cause
                    .downcast_ref::<reqwest::Error>()
                    .is_some_and(reqwest::Error::is_connect)
            });
            tracing::info!(url, error = format!("{error:#}"), "artwork fetch failed");
            if !offline {
                self.retry_at
                    .lock()
                    .expect("no thread panics while holding the lock")
                    .insert(url, now + RETRY_AFTER_MS);
            }
        }
    }

    /// Writes the image to `<root>/<file>` through a `.part` file and
    /// returns its size.
    // TODO: decode and downscale on the way in. A 3000 px cover decodes to
    // 36 MB in GPUI's image cache, and the sidebar shows it at 20 px.
    async fn download(&self, url: &str, file: &str) -> Result<u64> {
        let mut response = self
            .client
            .get(url)
            .timeout(FETCH_TIMEOUT)
            .send()
            .await
            .context("request the image")?
            .error_for_status()
            .context("check the response status")?;
        if response
            .content_length()
            .is_some_and(|length| length > MAX_FILE)
        {
            bail!("the image is larger than {MAX_FILE} bytes");
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.context("read the image")? {
            body.extend_from_slice(&chunk);
            if body.len() as u64 > MAX_FILE {
                bail!("the image is larger than {MAX_FILE} bytes");
            }
        }
        tokio::fs::create_dir_all(&self.root)
            .await
            .context("create the artwork root")?;
        let part = self.root.join(format!("{file}.part"));
        tokio::fs::write(&part, &body)
            .await
            .context("write the image")?;
        tokio::fs::rename(&part, self.root.join(file))
            .await
            .context("publish the image")?;
        Ok(body.len() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONLINE: NetworkState = NetworkState::ASSUMED;
    const METERED: NetworkState = NetworkState {
        connected: true,
        metered: true,
    };
    const OFFLINE: NetworkState = NetworkState {
        connected: false,
        metered: false,
    };
    const NOW: i64 = 1_800_000_000_000;

    fn source(url: &str, podcast_id: &str) -> Source {
        Source {
            url: url.to_owned(),
            podcast_id: podcast_id.to_owned(),
        }
    }

    fn entry(url: &str, bytes: u64, last_used: i64) -> Entry {
        Entry {
            url: url.to_owned(),
            file: format!("file-of-{url}"),
            bytes,
            last_used,
        }
    }

    fn urls(urls: &[&str]) -> Vec<String> {
        urls.iter().map(|url| (*url).to_owned()).collect()
    }

    #[test]
    fn wanted_keeps_the_first_position_and_any_metered_permission() {
        let snapshot = Snapshot {
            sources: vec![
                source("a.jpg", "p1"),
                source("b.jpg", "p2"),
                source("a.jpg", "p2"),
                source("c.jpg", "p1"),
            ],
            auto_on_metered: HashSet::from(["p2".to_owned()]),
            ..Snapshot::default()
        };
        let wanted = |url: &str, metered_ok| Wanted {
            url: url.to_owned(),
            metered_ok,
        };
        assert_eq!(
            super::wanted(&snapshot),
            [
                wanted("a.jpg", true),
                wanted("b.jpg", true),
                wanted("c.jpg", false)
            ]
        );
        let everywhere = Snapshot {
            on_metered: true,
            ..snapshot
        };
        assert!(super::wanted(&everywhere).iter().all(|w| w.metered_ok));
    }

    #[test]
    fn fetches_what_is_missing_and_plans_nothing_once_it_is_cached() {
        let mut snapshot = Snapshot {
            sources: vec![source("a.jpg", "p1"), source("b.jpg", "p1")],
            cached: vec![entry("a.jpg", 1000, NOW)],
            ..Snapshot::default()
        };
        let first = plan(&snapshot, NOW, ONLINE, &HashMap::new());
        assert_eq!(first.fetch, urls(&["b.jpg"]));
        assert!(first.evict.is_empty() && first.touch.is_empty());

        snapshot.cached.push(entry("b.jpg", 1000, NOW));
        assert_eq!(
            plan(&snapshot, NOW, ONLINE, &HashMap::new()),
            Plan::default()
        );
    }

    #[test]
    fn offline_fetches_nothing_and_metered_only_what_may_use_it() {
        let snapshot = Snapshot {
            sources: vec![source("a.jpg", "p1"), source("b.jpg", "p2")],
            auto_on_metered: HashSet::from(["p2".to_owned()]),
            ..Snapshot::default()
        };
        assert!(
            plan(&snapshot, NOW, OFFLINE, &HashMap::new())
                .fetch
                .is_empty()
        );
        assert_eq!(
            plan(&snapshot, NOW, METERED, &HashMap::new()).fetch,
            urls(&["b.jpg"])
        );
    }

    #[test]
    fn a_failed_url_waits_for_its_retry_time() {
        let snapshot = Snapshot {
            sources: vec![source("a.jpg", "p1")],
            ..Snapshot::default()
        };
        let retry_at = HashMap::from([("a.jpg".to_owned(), NOW + 1)]);
        assert!(plan(&snapshot, NOW, ONLINE, &retry_at).fetch.is_empty());
        assert_eq!(
            plan(&snapshot, NOW + 1, ONLINE, &retry_at).fetch,
            urls(&["a.jpg"])
        );
    }

    #[test]
    fn unwanted_files_stay_until_a_fetch_needs_their_room_then_go_least_recently_used_first() {
        let half = CAP / 2;
        let mut snapshot = Snapshot {
            sources: vec![source("new.jpg", "p1")],
            cached: vec![
                entry("newer.jpg", half, NOW - 10),
                entry("older.jpg", half - ESTIMATE, NOW - 20),
            ],
            ..Snapshot::default()
        };
        // Exactly full with the estimate: nothing goes.
        let fits = plan(&snapshot, NOW, ONLINE, &HashMap::new());
        assert!(fits.evict.is_empty());
        assert_eq!(fits.fetch, urls(&["new.jpg"]));

        snapshot.cached[1].bytes += 1;
        let tight = plan(&snapshot, NOW, ONLINE, &HashMap::new());
        assert_eq!(tight.evict, [snapshot.cached[1].clone()]);
        assert_eq!(tight.fetch, urls(&["new.jpg"]));
    }

    #[test]
    fn wanted_images_beyond_the_cap_give_way_to_earlier_ones() {
        let snapshot = Snapshot {
            sources: vec![
                source("podcast.jpg", "p1"),
                source("episode.jpg", "p1"),
                source("old-episode.jpg", "p1"),
            ],
            cached: vec![
                entry("old-episode.jpg", CAP / 2, NOW),
                entry("episode.jpg", CAP / 2, NOW),
            ],
            ..Snapshot::default()
        };
        let plan = plan(&snapshot, NOW, ONLINE, &HashMap::new());
        assert_eq!(plan.fetch, urls(&["podcast.jpg"]));
        assert_eq!(plan.evict, [snapshot.cached[0].clone()]);
    }

    #[test]
    fn a_stale_wanted_entry_is_touched() {
        let snapshot = Snapshot {
            sources: vec![source("a.jpg", "p1"), source("b.jpg", "p1")],
            cached: vec![
                entry("a.jpg", 1, NOW - TOUCH_AFTER_MS),
                entry("b.jpg", 1, NOW - TOUCH_AFTER_MS + 1),
                entry("unwanted.jpg", 1, 0),
            ],
            ..Snapshot::default()
        };
        assert_eq!(
            plan(&snapshot, NOW, OFFLINE, &HashMap::new()).touch,
            urls(&["a.jpg"])
        );
    }
}
