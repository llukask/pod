//! The artwork cache (spec 7.5): the reconciler's step that keeps the
//! images the views show in `<data_dir>/artwork`, so the sidebar and the
//! inbox have artwork offline.
//!
//! Like the download step it is load, a pure [`plan`], then the side
//! effects. Every cached file has an `artwork` row, written only once the
//! file is complete. Views join that table on the `artwork_url` columns and
//! hand GPUI the local file, never the URL, so nothing is fetched from the
//! render path, and their live queries pick up an image when it arrives.
//! Files are stored [`downscale`]d to the largest view, so a large cover
//! costs neither disk nor GPUI's decoded-image memory.

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
    /// The stored, downscaled size.
    pub bytes: u64,
    /// 0 for a search result's image: no pass touches it, so once no view
    /// wants it, it is the least recently used entry and goes first.
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
    /// The search pane's result artwork, wanted after every source while
    /// the pane shows it. [`load`] leaves it empty: the reconciler holds it
    /// in memory ([`super::Reconciler::want_transient`]).
    pub transient: Vec<String>,
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
    /// `auto_on_metered` on any podcast showing this image. Always true
    /// for a search result: the user is looking at it, as at the results
    /// the same network just delivered.
    pub metered_ok: bool,
    /// Only the search pane shows it. Its row is written with `last_used`
    /// 0 and never touched (see [`Entry::last_used`]).
    pub transient: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Least recently used first. Delete the row, then the file.
    pub evict: Vec<Entry>,
    /// Wanted entries whose `last_used` to set to now.
    pub touch: Vec<String>,
    pub fetch: Vec<Wanted>,
    /// The earliest retry time of a URL that would be fetched but for its
    /// last failure: the next pass is due then.
    pub wake_at: Option<i64>,
}

/// The URLs to cache, each once at its first position: sources, then
/// search results.
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
                    transient: false,
                });
            }
        }
    }
    for url in &snapshot.transient {
        if !position.contains_key(url.as_str()) {
            position.insert(url, wanted.len());
            wanted.push(Wanted {
                url: url.clone(),
                metered_ok: true,
                transient: true,
            });
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
        .filter(|wanted| !wanted.transient)
        .filter_map(|wanted| cached.get(wanted.url.as_str()))
        .filter(|entry| entry.last_used + TOUCH_AFTER_MS <= now)
        .map(|entry| entry.url.clone())
        .collect();

    let mut fetch: Vec<Wanted> = Vec::new();
    let mut wake_at: Option<i64> = None;
    let fetchable = target
        .iter()
        .filter(|wanted| !cached.contains_key(wanted.url.as_str()))
        .filter(|wanted| network.connected && (!network.metered || wanted.metered_ok));
    for wanted in fetchable {
        match retry_at.get(&wanted.url) {
            Some(&at) if at > now => wake_at = Some(wake_at.map_or(at, |soonest| soonest.min(at))),
            _ => fetch.push((*wanted).clone()),
        }
    }

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
        wake_at,
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
        transient: Vec::new(),
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

// ==============================================================================
// Downscaling
// ==============================================================================

/// The longest side a cached image keeps: twice the largest view (a search
/// result's 168 px square), so it stays sharp at 2x scaling. GPUI decodes
/// the whole file into its image cache, where a 3000 px cover takes 36 MB;
/// at this size it takes under 0.5 MB.
pub const MAX_SIDE: u32 = 336;
const JPEG_QUALITY: u8 = 85;
/// Artwork is at most a few thousand pixels a side. A larger header is a
/// broken or hostile file, refused before decoding allocates for it.
const MAX_DECODE_SIDE: u32 = 8192;

/// Fits the image in `body` within [`MAX_SIDE`] square. One small enough
/// already keeps its bytes. A shrunk image is re-encoded as a JPEG, or as a
/// PNG when it has transparent pixels, which JPEG would turn black. A body
/// the `image` crate can't read fails like a failed request: nothing is
/// stored and the URL waits for its retry.
///
/// It decodes and resizes on the calling thread: run it on the blocking
/// pool.
pub fn downscale(body: Vec<u8>) -> Result<Vec<u8>> {
    let reader = || {
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(MAX_DECODE_SIDE);
        limits.max_image_height = Some(MAX_DECODE_SIDE);
        let mut reader = image::ImageReader::new(std::io::Cursor::new(&body))
            .with_guessed_format()
            .context("guess the image format")?;
        reader.limits(limits);
        Ok::<_, color_eyre::Report>(reader)
    };
    let (width, height) = reader()?.into_dimensions().context("read the image size")?;
    if width.max(height) <= MAX_SIDE {
        return Ok(body);
    }
    let image = reader()?.decode().context("decode the image")?.resize(
        MAX_SIDE,
        MAX_SIDE,
        image::imageops::FilterType::CatmullRom,
    );

    let opaque =
        !image.color().has_alpha() || image.to_rgba8().pixels().all(|pixel| pixel[3] == u8::MAX);
    let mut out = std::io::Cursor::new(Vec::new());
    if opaque {
        image
            .into_rgb8()
            .write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(
                &mut out,
                JPEG_QUALITY,
            ))
            .context("encode the JPEG")?;
    } else {
        image
            .write_with_encoder(image::codecs::png::PngEncoder::new(&mut out))
            .context("encode the PNG")?;
    }
    Ok(out.into_inner())
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
    /// Fetches `wanted`, a few at a time, and records each finished file. A
    /// cancelled batch writes no more rows.
    pub async fn fetch_all(self, wanted: Vec<Wanted>, cancel: CancellationToken) {
        futures_util::stream::iter(wanted)
            .map(|wanted| {
                let fetcher = self.clone();
                let cancel = cancel.clone();
                async move {
                    tokio::select! {
                        () = cancel.cancelled() => {}
                        () = fetcher.fetch(wanted) => {}
                    }
                }
            })
            .buffer_unordered(CONCURRENCY)
            .collect::<()>()
            .await;
    }

    async fn fetch(&self, wanted: Wanted) {
        let url = wanted.url;
        let file = file_name(&url);
        let result = self.download(&url, &file).await;
        let now = Timestamp::now().as_epoch_ms();
        let last_used = if wanted.transient { 0 } else { now };
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
                                params![
                                    url,
                                    file,
                                    i64::try_from(bytes).unwrap_or(i64::MAX),
                                    last_used
                                ],
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

    /// Writes the image, [`downscale`]d, to `<root>/<file>` through a
    /// `.part` file and returns the size written.
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
        let body = tokio::task::spawn_blocking(move || downscale(body))
            .await
            .context("wait for the downscale")?
            .context("downscale the image")?;
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

    fn fetched(plan: &Plan) -> Vec<String> {
        plan.fetch.iter().map(|wanted| wanted.url.clone()).collect()
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
            transient: false,
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
        assert_eq!(fetched(&first), urls(&["b.jpg"]));
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
            fetched(&plan(&snapshot, NOW, METERED, &HashMap::new())),
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
        let waiting = plan(&snapshot, NOW, ONLINE, &retry_at);
        assert!(waiting.fetch.is_empty());
        assert_eq!(
            waiting.wake_at,
            Some(NOW + 1),
            "the loop wakes for the retry"
        );
        let due = plan(&snapshot, NOW + 1, ONLINE, &retry_at);
        assert_eq!(fetched(&due), urls(&["a.jpg"]));
        assert_eq!(due.wake_at, None);
        assert_eq!(
            plan(&snapshot, NOW, OFFLINE, &retry_at).wake_at,
            None,
            "offline, connectivity wakes the loop instead"
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
        assert_eq!(fetched(&fits), urls(&["new.jpg"]));

        snapshot.cached[1].bytes += 1;
        let tight = plan(&snapshot, NOW, ONLINE, &HashMap::new());
        assert_eq!(tight.evict, [snapshot.cached[1].clone()]);
        assert_eq!(fetched(&tight), urls(&["new.jpg"]));
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
        assert_eq!(fetched(&plan), urls(&["podcast.jpg"]));
        assert_eq!(plan.evict, [snapshot.cached[0].clone()]);
    }

    fn encode(image: &image::DynamicImage, format: image::ImageFormat) -> Vec<u8> {
        let mut body = std::io::Cursor::new(Vec::new());
        image
            .write_to(&mut body, format)
            .expect("the test image encodes");
        body.into_inner()
    }

    fn decode(body: &[u8]) -> (image::ImageFormat, image::DynamicImage) {
        (
            image::guess_format(body).expect("the output has a known format"),
            image::load_from_memory(body).expect("the output decodes"),
        )
    }

    #[test]
    fn a_large_opaque_image_shrinks_to_a_jpeg_within_the_largest_view() {
        let cover = image::DynamicImage::new_rgb8(1200, 600);
        let body = downscale(encode(&cover, image::ImageFormat::Png)).expect("it downscales");
        let (format, image) = decode(&body);
        assert_eq!(format, image::ImageFormat::Jpeg);
        assert_eq!((image.width(), image.height()), (MAX_SIDE, MAX_SIDE / 2));
    }

    #[test]
    fn transparency_survives_as_a_png_and_opaque_alpha_becomes_a_jpeg() {
        let clear = image::DynamicImage::new_rgba8(400, 800);
        let (format, image) =
            decode(&downscale(encode(&clear, image::ImageFormat::Png)).expect("it downscales"));
        assert_eq!(format, image::ImageFormat::Png);
        assert_eq!((image.width(), image.height()), (MAX_SIDE / 2, MAX_SIDE));
        assert_eq!(image.to_rgba8().get_pixel(0, 0)[3], 0);

        let mut solid = image::RgbaImage::new(800, 800);
        solid.pixels_mut().for_each(|pixel| pixel[3] = u8::MAX);
        let solid = image::DynamicImage::ImageRgba8(solid);
        let (format, _) =
            decode(&downscale(encode(&solid, image::ImageFormat::Png)).expect("it downscales"));
        assert_eq!(format, image::ImageFormat::Jpeg);
    }

    #[test]
    fn a_small_image_keeps_its_bytes() {
        let body = encode(
            &image::DynamicImage::new_rgb8(MAX_SIDE, 10),
            image::ImageFormat::Png,
        );
        assert_eq!(downscale(body.clone()).expect("it passes through"), body);
    }

    #[test]
    fn a_body_that_is_not_an_image_fails() {
        assert!(downscale(b"<html>Not found</html>".to_vec()).is_err());
    }

    #[test]
    fn search_results_come_after_everything_else_and_any_network_may_fetch_them() {
        let snapshot = Snapshot {
            sources: vec![source("podcast.jpg", "p1")],
            transient: urls(&["result.jpg", "podcast.jpg"]),
            ..Snapshot::default()
        };
        let plan = plan(&snapshot, NOW, METERED, &HashMap::new());
        assert_eq!(
            plan.fetch,
            [Wanted {
                url: "result.jpg".to_owned(),
                metered_ok: true,
                transient: true,
            }],
            "the podcast's own image keeps its place and its metered rule"
        );

        let crowded = Snapshot {
            sources: vec![source("podcast.jpg", "p1")],
            transient: urls(&["result.jpg"]),
            cached: vec![entry("podcast.jpg", CAP - ESTIMATE + 1, NOW)],
            ..Snapshot::default()
        };
        assert!(
            super::plan(&crowded, NOW, ONLINE, &HashMap::new())
                .fetch
                .is_empty(),
            "a result never pushes out what the cap fits first"
        );
    }

    /// A search result's image is stored with `last_used` 0 and no pass
    /// touches it, so the next pass plans nothing, and once the results
    /// change it is the first to go.
    #[test]
    fn a_cached_search_result_is_left_alone_then_goes_first() {
        let mut snapshot = Snapshot {
            sources: vec![source("podcast.jpg", "p1")],
            transient: urls(&["result.jpg"]),
            cached: vec![
                entry("old.jpg", CAP / 2, NOW - 10 * TOUCH_AFTER_MS),
                entry("result.jpg", CAP / 2 - ESTIMATE, 0),
            ],
            ..Snapshot::default()
        };
        let first = plan(&snapshot, NOW, ONLINE, &HashMap::new());
        assert!(first.touch.is_empty());
        assert_eq!(fetched(&first), urls(&["podcast.jpg"]));

        snapshot.cached.push(entry("podcast.jpg", 1, NOW));
        assert_eq!(
            plan(&snapshot, NOW, ONLINE, &HashMap::new()),
            Plan::default()
        );

        snapshot.transient.clear();
        snapshot.sources.push(source("new.jpg", "p1"));
        let after = plan(&snapshot, NOW, ONLINE, &HashMap::new());
        assert_eq!(after.evict, [snapshot.cached[1].clone()]);
    }

    #[test]
    fn subscribing_to_a_result_makes_its_image_an_ordinary_entry() {
        let snapshot = Snapshot {
            sources: vec![source("result.jpg", "p1")],
            cached: vec![entry("result.jpg", 1, 0)],
            ..Snapshot::default()
        };
        assert_eq!(
            plan(&snapshot, NOW, ONLINE, &HashMap::new()).touch,
            urls(&["result.jpg"])
        );
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
