//! A podcast's page (wireframes D2 and D7, spec 5.5): the query behind it,
//! the wording of its states and of the unsubscribe confirmation, and the
//! writes it makes. [`view`] is the page.

pub mod view;

use color_eyre::eyre::{Result, WrapErr};
use jiff::tz::TimeZone;
use pod_protocol::{EpisodeId, PodcastId, PodcastStatus, Timestamp};
use rusqlite::{Connection, OptionalExtension as _, Transaction, params};

use crate::inbox::{day_label, time_left_label};
use crate::store::{Store, Table, TableSet};
use crate::sync::SyncHandle;
use crate::sync::local_write::{self, EpisodeChange, LocalChange};

/// The most episodes "keep newest N" offers (wireframe M6: Off, 1 to 10).
pub const AUTO_KEEP_MAX: u32 = 10;

/// A subscribed podcast with everything its page shows.
#[derive(Debug, Clone, PartialEq)]
pub struct PodcastPage {
    pub id: PodcastId,
    /// The feed's title, else the directory title of a placeholder, else
    /// the feed URL, as in the sidebar.
    pub title: String,
    pub author: Option<String>,
    /// The subscription's URL, which the unsubscribe mutation carries.
    pub feed_url: String,
    pub state: PodcastState,
    /// Newest first, the podcast's whole history.
    pub episodes: Vec<PodcastEpisode>,
    pub downloads: Downloads,
    pub device: DeviceSettings,
    /// The cached `artwork.file` of the podcast's artwork.
    pub artwork: Option<String>,
}

/// What the header shows about the feed, from `podcast.status` and
/// `status_message`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PodcastState {
    /// Not fetched yet: a placeholder (spec 5.5), or a subscription whose
    /// podcast row hasn't synced.
    Pending,
    Ok,
    /// Fetches keep failing; the poller keeps trying.
    Failing {
        message: Option<String>,
    },
    /// The feed answered HTTP 410 and polling stopped. Episodes stay.
    Gone {
        message: Option<String>,
    },
}

impl PodcastState {
    /// `status` is `None` when there is no podcast row yet.
    fn from_row(status: Option<&str>, message: Option<String>) -> Result<PodcastState> {
        let Some(status) = status else {
            return Ok(PodcastState::Pending);
        };
        Ok(match status.parse().context("parse the podcast status")? {
            PodcastStatus::Pending => PodcastState::Pending,
            PodcastStatus::Ok => PodcastState::Ok,
            PodcastStatus::Failing => PodcastState::Failing { message },
            PodcastStatus::Gone => PodcastState::Gone { message },
        })
    }
}

/// An episode row on the page.
#[derive(Debug, Clone, PartialEq)]
pub struct PodcastEpisode {
    pub id: EpisodeId,
    pub title: String,
    /// [`day_label`] of the publication time when the query ran.
    pub day: String,
    pub duration_ms: Option<i64>,
    pub position_ms: i64,
    pub played: bool,
    pub archived: bool,
}

/// This podcast's `download` rows, which unsubscribing removes (spec 7.4).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Downloads {
    /// Rows in any state: a queued pin goes as surely as a finished file.
    pub total: u32,
    /// The rows a `pin` wish covers.
    pub pinned: u32,
}

/// The podcast's `podcast_device` row (spec 7.1). `notify` is left out:
/// desktop has no new-episode notifications (7.6).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeviceSettings {
    /// Keep the newest N unplayed episodes downloaded, 0 for off.
    pub auto_keep: u32,
    /// Auto downloads may use a metered network.
    pub auto_on_metered: bool,
}

// ==============================================================================
// Wording
// ==============================================================================

fn plural(count: u32, one: &str, many: &str) -> String {
    if count == 1 {
        format!("1 {one}")
    } else {
        format!("{count} {many}")
    }
}

/// The banner of a failing or gone feed (D7 annotation 2), `None` for the
/// others. A gone feed's episodes and downloads stay while the user stays
/// subscribed, and the banner says so.
pub fn status_notice(state: &PodcastState, episodes: u32, downloads: u32) -> Option<String> {
    match state {
        PodcastState::Pending | PodcastState::Ok => None,
        PodcastState::Failing { message } => Some(format!(
            "{}. Pod keeps checking for new episodes.",
            message.as_deref().unwrap_or("The feed can't be fetched")
        )),
        PodcastState::Gone { message } => {
            let mut notice = message
                .as_ref()
                .map_or(String::new(), |message| format!("{message}. "));
            notice.push_str(
                "The publisher removed this feed, and Pod has stopped checking for new episodes.",
            );
            let kept = match (episodes, downloads) {
                (0, 0) => None,
                (1, 0) => Some("Your 1 episode is kept.".to_owned()),
                (episodes, 0) => Some(format!("Your {episodes} episodes are kept.")),
                (episodes, downloads) => Some(format!(
                    "Your {} and {} are kept.",
                    plural(episodes, "episode", "episodes"),
                    plural(downloads, "download", "downloads")
                )),
            };
            if let Some(kept) = kept {
                notice.push(' ');
                notice.push_str(&kept);
            }
            Some(notice)
        }
    }
}

/// The unsubscribe confirmation's message (D7 annotation 3, M11).
/// Unsubscribing removes every download of the show, pins included.
pub fn unsubscribe_message(downloads: Downloads) -> String {
    let removed = match downloads {
        Downloads { total: 0, .. } => None,
        Downloads { total, pinned: 0 } => Some(format!(
            "{} will be removed from this computer.",
            plural(total, "download", "downloads")
        )),
        Downloads { total, pinned } if pinned == total => Some(format!(
            "{} will be removed from this computer.",
            plural(total, "pinned download", "pinned downloads")
        )),
        Downloads { total, pinned } => Some(format!(
            "{} will be removed from this computer, including {pinned} you pinned.",
            plural(total, "download", "downloads")
        )),
    };
    match removed {
        Some(removed) => format!("{removed} Your listening history stays."),
        None => "Your listening history stays.".to_owned(),
    }
}

/// The row's progress column: "Played", "21 min left" or "Unplayed".
pub fn progress_label(episode: &PodcastEpisode) -> String {
    if episode.played {
        return "Played".to_owned();
    }
    time_left_label(episode.duration_ms, episode.position_ms)
        .unwrap_or_else(|| "Unplayed".to_owned())
}

// ==============================================================================
// Query
// ==============================================================================

/// The tables [`load_page`] reads.
pub fn tables() -> TableSet {
    [
        Table::Podcast,
        Table::Episode,
        Table::Subscription,
        Table::EpisodeState,
        Table::PodcastDevice,
        Table::Download,
        Table::DownloadWish,
        Table::Artwork,
    ]
    .into_iter()
    .collect()
}

/// The page of podcast `id`, or `None` unless it is subscribed.
pub fn load_page(
    connection: &Connection,
    id: PodcastId,
    now: Timestamp,
    zone: &TimeZone,
) -> Result<Option<PodcastPage>> {
    let id_text = id.to_string();
    let header = connection
        .query_row(
            "select coalesce(p.title, p.local_title, s.feed_url), p.author, s.feed_url,
                    p.status, p.status_message,
                    coalesce(d.auto_keep, 0), coalesce(d.auto_on_metered, 0), a.file
             from subscription s
             left join podcast p on p.id = s.podcast_id
             left join artwork a on a.url = coalesce(p.artwork_url, p.local_artwork_url)
             left join podcast_device d on d.podcast_id = s.podcast_id
             where s.podcast_id = ?1 and s.subscribed = 1",
            [&id_text],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, u32>(5)?,
                    row.get::<_, bool>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ))
            },
        )
        .optional()
        .context("read the podcast")?;
    let Some((title, author, feed_url, status, message, auto_keep, auto_on_metered, artwork)) =
        header
    else {
        return Ok(None);
    };

    let mut statement = connection
        .prepare_cached(
            "select e.id, e.title, e.published_at, e.duration_ms, coalesce(st.position_ms, 0),
                    coalesce(st.played, 0), coalesce(st.archived, 0)
             from episode e
             left join episode_state st on st.episode_id = e.id
             where e.podcast_id = ?1
             order by e.published_at desc, e.id desc",
        )
        .context("prepare the episode query")?;
    let episodes = statement
        .query_map([&id_text], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, bool>(5)?,
                row.get::<_, bool>(6)?,
            ))
        })
        .context("run the episode query")?
        .map(|row| {
            let (episode_id, title, published_at, duration_ms, position_ms, played, archived) =
                row.context("read an episode row")?;
            let published_at =
                Timestamp::from_epoch_ms(published_at).context("read the publication time")?;
            Ok(PodcastEpisode {
                id: episode_id.parse().context("parse an episode id")?,
                title,
                day: day_label(published_at, now, zone),
                duration_ms,
                position_ms,
                played,
                archived,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let downloads = connection
        .query_row(
            "select count(*), count(w.episode_id)
             from download d
             join episode e on e.id = d.episode_id
             left join download_wish w on w.episode_id = d.episode_id and w.source = 'pin'
             where e.podcast_id = ?1",
            [&id_text],
            |row| {
                Ok(Downloads {
                    total: row.get(0)?,
                    pinned: row.get(1)?,
                })
            },
        )
        .context("count the downloads")?;

    Ok(Some(PodcastPage {
        id,
        title,
        author,
        feed_url,
        state: PodcastState::from_row(status.as_deref(), message)
            .context("read the podcast's state")?,
        episodes,
        downloads,
        device: DeviceSettings {
            auto_keep,
            auto_on_metered,
        },
        artwork,
    }))
}

// ==============================================================================
// Writes
// ==============================================================================

/// Writes this device's settings for podcast `id`. They are device-local
/// (spec 7.1), so this is a plain store write without a mutation; the
/// reconciler follows `podcast_device` changes itself.
pub async fn set_device_settings(
    store: &Store,
    id: PodcastId,
    settings: DeviceSettings,
) -> Result<()> {
    store
        .write(move |transaction| {
            transaction
                .execute(
                    "insert into podcast_device (podcast_id, auto_keep, auto_on_metered)
                     values (?1, ?2, ?3)
                     on conflict (podcast_id) do update set
                         auto_keep = excluded.auto_keep,
                         auto_on_metered = excluded.auto_on_metered",
                    params![id.to_string(), settings.auto_keep, settings.auto_on_metered],
                )
                .context("upsert the podcast_device row")?;
            Ok(())
        })
        .await
        .context("write the podcast's device settings")
}

/// Marks every unplayed episode of podcast `id` played, in one local
/// transaction.
pub async fn mark_all_played(store: &Store, sync: &SyncHandle, id: PodcastId) -> Result<()> {
    local_write::write_all(store, sync, move |transaction| unplayed(transaction, id))
        .await
        .context("mark the episodes played")?;
    Ok(())
}

/// A played write for each unplayed episode of podcast `id`.
fn unplayed(transaction: &Transaction<'_>, id: PodcastId) -> Result<Vec<LocalChange>> {
    let mut statement = transaction
        .prepare(
            "select e.id from episode e
             left join episode_state st on st.episode_id = e.id
             where e.podcast_id = ?1 and coalesce(st.played, 0) = 0",
        )
        .context("prepare the unplayed query")?;
    statement
        .query_map([id.to_string()], |row| row.get::<_, String>(0))
        .context("run the unplayed query")?
        .map(|episode_id| {
            Ok(LocalChange::Episode {
                episode_id: episode_id
                    .context("read an episode id")?
                    .parse()
                    .context("parse an episode id")?,
                podcast_id: id,
                change: EpisodeChange::Played(true),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::{migrated_connection, scratch_dir};

    const PODCAST: &str = "9c3e5b1a-2f4d-5e6c-8a7b-1d2e3f4a5b6c";
    const OTHER: &str = "9c3e5b1a-2f4d-5e6c-8a7b-000000000000";
    const DEVICE: &str = "019a3f10-8c2e-7a41-9b3d-5e6f7a8b9c0d";

    fn podcast_id() -> PodcastId {
        PODCAST.parse().expect("literal is a UUID")
    }

    fn episode_id(n: u32) -> String {
        format!("4b1e2c3d-4e5f-5a6b-9c7d-8e9f0a1b2c{n:02}")
    }

    fn ts(text: &str) -> Timestamp {
        text.parse().expect("test literal is RFC 3339")
    }

    fn failing(message: &str) -> PodcastState {
        PodcastState::Failing {
            message: Some(message.to_owned()),
        }
    }

    fn gone() -> PodcastState {
        PodcastState::Gone {
            message: Some("Feed returned HTTP 410".to_owned()),
        }
    }

    #[test]
    fn only_failing_and_gone_feeds_show_a_notice() {
        assert_eq!(status_notice(&PodcastState::Pending, 0, 0), None);
        assert_eq!(status_notice(&PodcastState::Ok, 12, 2), None);
        assert_eq!(
            status_notice(&failing("Feed returned HTTP 404"), 12, 2),
            Some("Feed returned HTTP 404. Pod keeps checking for new episodes.".to_owned())
        );
        assert_eq!(
            status_notice(&PodcastState::Failing { message: None }, 12, 2),
            Some("The feed can't be fetched. Pod keeps checking for new episodes.".to_owned())
        );
    }

    #[test]
    fn a_gone_feed_says_what_is_kept() {
        assert_eq!(
            status_notice(&gone(), 412, 2),
            Some(
                "Feed returned HTTP 410. The publisher removed this feed, and Pod has \
                 stopped checking for new episodes. Your 412 episodes and 2 downloads \
                 are kept."
                    .to_owned()
            )
        );
        assert_eq!(
            status_notice(&gone(), 1, 0),
            Some(
                "Feed returned HTTP 410. The publisher removed this feed, and Pod has \
                 stopped checking for new episodes. Your 1 episode is kept."
                    .to_owned()
            )
        );
        assert_eq!(
            status_notice(&PodcastState::Gone { message: None }, 0, 0),
            Some(
                "The publisher removed this feed, and Pod has stopped checking for new \
                 episodes."
                    .to_owned()
            )
        );
    }

    #[test]
    fn the_unsubscribe_confirmation_counts_downloads_and_pins() {
        let downloads = |total, pinned| Downloads { total, pinned };
        assert_eq!(
            unsubscribe_message(downloads(0, 0)),
            "Your listening history stays."
        );
        assert_eq!(
            unsubscribe_message(downloads(3, 0)),
            "3 downloads will be removed from this computer. Your listening history stays."
        );
        assert_eq!(
            unsubscribe_message(downloads(2, 1)),
            "2 downloads will be removed from this computer, including 1 you pinned. \
             Your listening history stays."
        );
        assert_eq!(
            unsubscribe_message(downloads(1, 1)),
            "1 pinned download will be removed from this computer. \
             Your listening history stays."
        );
        assert_eq!(
            unsubscribe_message(downloads(2, 2)),
            "2 pinned downloads will be removed from this computer. \
             Your listening history stays."
        );
    }

    #[test]
    fn progress_reads_played_time_left_or_unplayed() {
        let episode = |played, position_ms| PodcastEpisode {
            id: episode_id(1).parse().expect("literal is a UUID"),
            title: "EP 151: The pager".into(),
            day: "Yesterday".into(),
            duration_ms: Some(52 * 60_000),
            position_ms,
            played,
            archived: false,
        };
        assert_eq!(progress_label(&episode(true, 31 * 60_000)), "Played");
        assert_eq!(progress_label(&episode(false, 31 * 60_000)), "21 min left");
        assert_eq!(progress_label(&episode(false, 0)), "Unplayed");
    }

    /// One subscribed podcast with three episodes a day apart, of which the
    /// middle one is played and the oldest archived; another podcast's
    /// download, which mustn't count.
    fn seed(connection: &Connection, status: Option<(&str, Option<&str>)>) {
        if let Some((status, message)) = status {
            connection
                .execute(
                    "insert into podcast (id, feed_url, status, status_message, title,
                         author, version)
                     values (?1, 'https://example.com/feed', ?2, ?3, 'Darknet Diaries',
                             'Jack Rhysider', 4)",
                    params![PODCAST, status, message],
                )
                .expect("the podcast inserts");
        }
        connection
            .execute(
                "insert into subscription
                     (podcast_id, feed_url, subscribed, subscribed_at, subscribed_dev)
                 values (?1, 'https://example.com/feed', 1, 0, ?2)",
                params![PODCAST, DEVICE],
            )
            .expect("the subscription inserts");
        for n in 1..=3 {
            connection
                .execute(
                    "insert into episode (id, podcast_id, title, summary_html, content_html,
                         published_at, duration_ms, audio_url, audio_type, version)
                     values (?1, ?2, ?3, '', '', ?4, 3120000, 'https://example.com/a.mp3',
                             'audio/mpeg', 1)",
                    params![
                        episode_id(n),
                        PODCAST,
                        format!("EP {n}"),
                        ts("2026-10-20T12:00:00Z").as_epoch_ms() + i64::from(n) * 86_400_000,
                    ],
                )
                .expect("the episode inserts");
        }
        connection
            .execute_batch(&format!(
                "insert into episode_state (episode_id, podcast_id, position_ms, played, archived)
                 values ('{two}', '{PODCAST}', 0, 1, 0), ('{one}', '{PODCAST}', 600000, 0, 1);
                 insert into download (episode_id, state, audio_url, path)
                 values ('{three}', 'complete', 'a', 'p'), ('{two}', 'queued', 'a', 'p'),
                        ('{other}', 'complete', 'a', 'p');
                 insert into episode (id, podcast_id, title, summary_html, content_html,
                     published_at, audio_url, audio_type, version)
                 values ('{other}', '{OTHER}', 'Elsewhere', '', '', 0, 'a', 'audio/mpeg', 1);
                 insert into download_wish (episode_id, source, created_at)
                 values ('{two}', 'pin', 0), ('{three}', 'auto', 0), ('{other}', 'pin', 0);
                 insert into podcast_device (podcast_id, notify, auto_keep, auto_on_metered)
                 values ('{PODCAST}', 1, 3, 1);",
                one = episode_id(1),
                two = episode_id(2),
                three = episode_id(3),
                other = episode_id(99),
            ))
            .expect("the states, downloads and settings insert");
    }

    fn page(connection: &Connection) -> Option<PodcastPage> {
        load_page(
            connection,
            podcast_id(),
            ts("2026-10-24T09:00:00Z"),
            &TimeZone::UTC,
        )
        .expect("the page query runs")
    }

    #[test]
    fn the_page_lists_every_episode_newest_first_with_its_state() {
        let connection = migrated_connection();
        seed(&connection, Some(("gone", Some("Feed returned HTTP 410"))));
        let page = page(&connection).expect("the podcast is subscribed");
        assert_eq!(page.title, "Darknet Diaries");
        assert_eq!(page.author.as_deref(), Some("Jack Rhysider"));
        assert_eq!(page.feed_url, "https://example.com/feed");
        assert_eq!(page.state, gone());
        let rows: Vec<_> = page
            .episodes
            .iter()
            .map(|e| (e.title.as_str(), e.day.as_str(), e.played, e.archived))
            .collect();
        assert_eq!(
            rows,
            [
                ("EP 3", "Yesterday", false, false),
                ("EP 2", "Thu", true, false),
                ("EP 1", "Wed", false, true),
            ]
        );
        assert_eq!(page.episodes[2].position_ms, 600_000);
        assert_eq!(
            page.downloads,
            Downloads {
                total: 2,
                pinned: 1
            }
        );
        assert_eq!(
            page.device,
            DeviceSettings {
                auto_keep: 3,
                auto_on_metered: true
            }
        );
    }

    #[test]
    fn a_subscription_without_a_podcast_row_is_pending() {
        let connection = migrated_connection();
        seed(&connection, None);
        let page = page(&connection).expect("the podcast is subscribed");
        assert_eq!(page.state, PodcastState::Pending);
        assert_eq!(page.title, "https://example.com/feed");
    }

    #[test]
    fn an_unsubscribed_podcast_has_no_page() {
        let connection = migrated_connection();
        seed(&connection, Some(("ok", None)));
        connection
            .execute("update subscription set subscribed = 0", [])
            .expect("the subscription updates");
        assert_eq!(page(&connection), None);
    }

    async fn signed_in_store() -> (tempfile::TempDir, Store) {
        let dir = scratch_dir();
        let store = Store::open(&dir.path().join("pod.db")).expect("the store opens");
        store
            .write(|transaction| {
                transaction
                    .execute(
                        "insert into account
                             (id, server_url, user_id, username, device_id, token)
                         values (1, 'https://pod.example', ?1, 'kilian', ?1, 'secret')",
                        [DEVICE],
                    )
                    .context("insert the account")?;
                seed(transaction, Some(("ok", None)));
                Ok(())
            })
            .await
            .expect("the store is seeded");
        (dir, store)
    }

    #[tokio::test]
    async fn mark_all_played_writes_the_unplayed_episodes_in_one_go() {
        let (_dir, store) = signed_in_store().await;
        let sync = SyncHandle::default();
        mark_all_played(&store, &sync, podcast_id())
            .await
            .expect("marking all played works");
        assert!(sync.take_wanted().is_some());

        let (played, queued): (i64, String) = store
            .read(|connection| {
                connection
                    .query_row(
                        "select (select count(*) from episode_state
                                 where podcast_id = ?1 and played = 1),
                                (select group_concat(coalesce_key, ' ') from
                                    (select coalesce_key from outbox order by coalesce_key))",
                        [PODCAST],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .context("count the played episodes")
            })
            .await
            .expect("the store reads");
        assert_eq!(played, 3);
        assert_eq!(
            queued,
            format!("played:{} played:{}", episode_id(1), episode_id(3)),
            "the already played episode gets no mutation"
        );
    }

    #[tokio::test]
    async fn device_settings_keep_the_notify_flag() {
        let (_dir, store) = signed_in_store().await;
        let settings = DeviceSettings {
            auto_keep: 5,
            auto_on_metered: false,
        };
        set_device_settings(&store, podcast_id(), settings)
            .await
            .expect("the settings write");
        set_device_settings(&store, OTHER.parse().expect("literal is a UUID"), settings)
            .await
            .expect("the settings write");
        let rows: String = store
            .read(|connection| {
                connection
                    .query_row(
                        "select group_concat(row, ' ') from (select podcast_id || ':' || notify
                             || auto_keep || auto_on_metered as row
                         from podcast_device order by podcast_id)",
                        [],
                        |row| row.get(0),
                    )
                    .context("read the settings")
            })
            .await
            .expect("the store reads");
        assert_eq!(rows, format!("{OTHER}:050 {PODCAST}:150"));
    }

    #[test]
    fn statuses_parse_and_an_unknown_one_fails() {
        assert_eq!(
            PodcastState::from_row(Some("ok"), None).expect("ok parses"),
            PodcastState::Ok
        );
        assert!(PodcastState::from_row(Some("weird"), None).is_err());
    }
}
