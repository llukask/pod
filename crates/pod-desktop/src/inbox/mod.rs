//! The inbox (spec 6): the query behind the inbox list and the sidebar's
//! unplayed counts, and the labels the list shows. [`view`] is the page.

pub mod view;

use color_eyre::eyre::{Result, WrapErr};
use jiff::tz::TimeZone;
use pod_protocol::settings::{INBOX_WINDOW_DAYS, inbox_window_days};
use pod_protocol::{EpisodeId, PodcastId, PodcastStatus, Timestamp, feed_host};
use rusqlite::{Connection, OptionalExtension as _, named_params};

use crate::store::{Table, TableSet};

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// An episode in the inbox, with what the list and detail panes show.
#[derive(Debug, Clone, PartialEq)]
pub struct InboxEpisode {
    pub id: EpisodeId,
    pub podcast_id: PodcastId,
    pub podcast_title: String,
    pub title: String,
    pub published_at: Timestamp,
    /// [`day_label`] of `published_at` when the query ran.
    pub day: String,
    pub duration_ms: Option<i64>,
    pub position_ms: i64,
    /// `content_html` if the feed has one, else `summary_html`. Both are
    /// sanitized HTML (spec 2.2).
    pub show_notes_html: String,
    /// The cached `artwork.file` of the episode's artwork, else of the
    /// podcast's.
    pub artwork: Option<String>,
}

/// A subscribed podcast in the sidebar.
#[derive(Debug, Clone, PartialEq)]
pub struct SidebarPodcast {
    pub id: PodcastId,
    pub title: String,
    /// `pending` while the podcast row is a placeholder or not synced yet.
    pub status: PodcastStatus,
    /// How many of its episodes are in the inbox.
    pub unplayed: usize,
    /// The cached `artwork.file` of its artwork.
    pub artwork: Option<String>,
}

/// The tables both queries read.
pub fn tables() -> TableSet {
    [
        Table::Podcast,
        Table::Episode,
        Table::Subscription,
        Table::EpisodeState,
        Table::Setting,
        Table::Artwork,
    ]
    .into_iter()
    .collect()
}

/// How far syncing has got while scopes are still loading, for the empty
/// inbox after sign-in (wireframe D6): it shows progress, not a blank page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Loading {
    pub shows_done: u32,
    pub shows_total: u32,
    pub episodes: u32,
}

/// The tables [`load_loading`] reads.
pub fn loading_tables() -> TableSet {
    [Table::SyncScope, Table::Episode].into_iter().collect()
}

/// `None` once the user scope and every podcast scope are complete.
pub fn load_loading(connection: &Connection) -> Result<Option<Loading>> {
    let (user_complete, shows_done, shows_total, episodes) = connection
        .query_row(
            "select (select count(*) from sync_scope where scope = 'user' and complete = 1),
                    (select count(*) from sync_scope where scope <> 'user' and complete = 1),
                    (select count(*) from sync_scope where scope <> 'user'),
                    (select count(*) from episode)",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, u32>(1)?,
                    row.get::<_, u32>(2)?,
                    row.get::<_, u32>(3)?,
                ))
            },
        )
        .context("count the synced scopes")?;
    let done = user_complete == 1 && shows_done == shows_total;
    Ok((!done).then_some(Loading {
        shows_done,
        shows_total,
        episodes,
    }))
}

// ==============================================================================
// Queries
// ==============================================================================

/// Spec 6's filter, shared by the list and the counts so they can't
/// disagree. It joins `podcast` (as `p`) for the list's titles. The window start is `:window_start` in epoch ms. Ids are
/// stored lowercase and hyphenated, so the newest-episode tie-break
/// compares them the way the spec does.
macro_rules! inbox_filter {
    () => {
        "from episode e
         join subscription s on s.podcast_id = e.podcast_id and s.subscribed = 1
         left join episode_state st on st.episode_id = e.id
         left join podcast p on p.id = e.podcast_id
         where coalesce(st.played, 0) = 0
           and coalesce(st.archived, 0) = 0
           and e.published_at >= :window_start
           and (
                 e.published_at >= s.subscribed_at
              or e.id = (select e2.id from episode e2
                         where e2.podcast_id = e.podcast_id
                         order by e2.published_at desc, e2.id desc limit 1)
           )"
    };
}

/// The inbox at `now`, newest first, with days labelled in `zone`. Reading
/// the system zone can touch the disk, which is one more reason this runs
/// on the store's read thread.
// TODO: the window moves with the clock, but the query only re-runs on
// writes, so an episode can outstay the window until the next one. Re-run
// it at the next window edge if that turns out to matter.
pub fn load_inbox(
    connection: &Connection,
    now: Timestamp,
    zone: &TimeZone,
) -> Result<Vec<InboxEpisode>> {
    let window_start = window_start(connection, now).context("compute the inbox window")?;
    let mut statement = connection
        .prepare_cached(concat!(
            "select e.id, e.podcast_id, coalesce(p.title, p.local_title, s.feed_url), e.title,
                    e.published_at, e.duration_ms, coalesce(st.position_ms, 0),
                    case when e.content_html <> '' then e.content_html else e.summary_html end,
                    coalesce(
                        (select file from artwork where url = e.artwork_url),
                        (select file from artwork
                         where url = coalesce(p.artwork_url, p.local_artwork_url))
                    ) ",
            inbox_filter!(),
            " order by e.published_at desc, e.id desc"
        ))
        .context("prepare the inbox query")?;
    let rows = statement
        .query_map(named_params! { ":window_start": window_start }, |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, Option<i64>>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, Option<String>>(8)?,
            ))
        })
        .context("run the inbox query")?;
    rows.map(|row| {
        let (
            id,
            podcast_id,
            podcast_title,
            title,
            published_at,
            duration_ms,
            position_ms,
            notes,
            artwork,
        ) = row.context("read an inbox row")?;
        let published_at =
            Timestamp::from_epoch_ms(published_at).context("read the publication time")?;
        Ok(InboxEpisode {
            id: id.parse().context("parse an episode id")?,
            podcast_id: podcast_id.parse().context("parse a podcast id")?,
            podcast_title,
            title,
            published_at,
            day: day_label(published_at, now, zone),
            duration_ms,
            position_ms,
            show_notes_html: notes,
            artwork,
        })
    })
    .collect()
}

/// What the sidebar and the podcast page call a podcast: the feed's title,
/// else the directory title the placeholder was added under, else the
/// feed's host (spec 5.5). The host, not the URL: a feed the server never
/// fetched has no other name, and `https://feeds.example.com/x/y.rss` is
/// recognizable by `feeds.example.com` alone.
pub fn display_title(title: Option<String>, local_title: Option<String>, feed_url: &str) -> String {
    title
        .or(local_title)
        .unwrap_or_else(|| feed_host(feed_url))
}

/// The subscribed podcasts, newest episode first, with their inbox counts.
pub fn load_podcasts(connection: &Connection, now: Timestamp) -> Result<Vec<SidebarPodcast>> {
    let window_start = window_start(connection, now).context("compute the inbox window")?;
    let mut statement = connection
        .prepare_cached(concat!(
            "with inbox as (select e.podcast_id ",
            inbox_filter!(),
            ")
             select s.podcast_id, p.title, p.local_title, s.feed_url,
                    coalesce(p.status, 'pending'),
                    (select count(*) from inbox i where i.podcast_id = s.podcast_id),
                    a.file
             from subscription s
             left join podcast p on p.id = s.podcast_id
             left join artwork a on a.url = coalesce(p.artwork_url, p.local_artwork_url)
             where s.subscribed = 1
             order by (select max(e.published_at) from episode e
                       where e.podcast_id = s.podcast_id) desc nulls last,
                      coalesce(p.title, p.local_title, s.feed_url) collate nocase"
        ))
        .context("prepare the podcast list query")?;
    let rows = statement
        .query_map(named_params! { ":window_start": window_start }, |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        })
        .context("run the podcast list query")?;
    rows.map(|row| {
        let (id, title, local_title, feed_url, status, unplayed, artwork) =
            row.context("read a podcast row")?;
        Ok(SidebarPodcast {
            id: id.parse().context("parse a podcast id")?,
            title: display_title(title, local_title, &feed_url),
            status: status.parse().context("parse the podcast status")?,
            unplayed: usize::try_from(unplayed).context("read the inbox count")?,
            artwork,
        })
    })
    .collect()
}

/// `(podcast_id, artwork_url)` of the inbox episodes that have their own
/// artwork, newest first, for the artwork cache (spec 7.5).
pub fn load_artwork_urls(connection: &Connection, now: Timestamp) -> Result<Vec<(String, String)>> {
    let window_start = window_start(connection, now).context("compute the inbox window")?;
    connection
        .prepare_cached(concat!(
            "select e.podcast_id, e.artwork_url ",
            inbox_filter!(),
            " and e.artwork_url is not null order by e.published_at desc, e.id desc"
        ))
        .context("prepare the inbox artwork query")?
        .query_map(named_params! { ":window_start": window_start }, |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .context("run the inbox artwork query")?
        .collect::<rusqlite::Result<_>>()
        .context("read the inbox artwork")
}

/// `now - inbox_window_days`, in epoch ms, with the synced setting read the
/// way the server reads it.
fn window_start(connection: &Connection, now: Timestamp) -> Result<i64> {
    let stored: Option<String> = connection
        .query_row(
            "select value from setting where key = ?1",
            [INBOX_WINDOW_DAYS],
            |row| row.get(0),
        )
        .optional()
        .context("read the inbox window setting")?;
    // Unparseable JSON is as unusable as a wrong type: the default applies.
    let stored = stored.and_then(|text| serde_json::from_str(&text).ok());
    Ok(now.as_epoch_ms() - inbox_window_days(stored.as_ref()) * DAY_MS)
}

// ==============================================================================
// Selection
// ==============================================================================

/// The episode to select once the list goes from `before` to `after`. The
/// selection stays if it's still listed. If its row left, the next row that
/// is still there takes over, else the nearest one above, so archiving
/// moves down the list the way a mail client does.
pub fn reselect(
    before: &[EpisodeId],
    after: &[EpisodeId],
    selected: Option<EpisodeId>,
) -> Option<EpisodeId> {
    if let Some(id) = selected
        && after.contains(&id)
    {
        return Some(id);
    }
    let Some(index) = selected.and_then(|id| before.iter().position(|row| *row == id)) else {
        return after.first().copied();
    };
    let below = before[index + 1..].iter().find(|id| after.contains(id));
    let above = before[..index].iter().rev().find(|id| after.contains(id));
    below.or(above).or(after.first()).copied()
}

/// The row `step` rows away from the selection in `ids`, stopping at either
/// end, or the first row without a selection. `None` for an empty list.
pub fn step_selection(
    ids: &[EpisodeId],
    selected: Option<EpisodeId>,
    step: isize,
) -> Option<EpisodeId> {
    let last = ids.len().checked_sub(1)?;
    let index = match selected.and_then(|id| ids.iter().position(|row| *row == id)) {
        Some(index) => index.saturating_add_signed(step).min(last),
        None => 0,
    };
    Some(ids[index])
}

// ==============================================================================
// Labels
// ==============================================================================

/// "30 s", "28 min", "1 h 4 min", "2 h". Under a minute it counts seconds,
/// otherwise it rounds to the minute.
pub fn duration_label(ms: i64) -> String {
    let seconds = (ms + 500) / 1_000;
    if seconds < 60 {
        return format!("{} s", seconds.max(1));
    }
    let minutes = (ms + 30_000) / 60_000;
    match (minutes / 60, minutes % 60) {
        (0, minutes) => format!("{minutes} min"),
        (hours, 0) => format!("{hours} h"),
        (hours, minutes) => format!("{hours} h {minutes} min"),
    }
}

/// "21 min left" for a started episode of known length.
pub fn time_left_label(duration_ms: Option<i64>, position_ms: i64) -> Option<String> {
    let remaining = duration_ms? - position_ms;
    (position_ms > 0 && remaining > 0).then(|| format!("{} left", duration_label(remaining)))
}

/// When an episode came out, as the list shows it: "Today", "Yesterday",
/// a weekday within the last week, else the date, with the year if it
/// isn't this one. Days are calendar days in `zone`.
pub fn day_label(published: Timestamp, now: Timestamp, zone: &TimeZone) -> String {
    let published = published.as_jiff().to_zoned(zone.clone()).date();
    let today = now.as_jiff().to_zoned(zone.clone()).date();
    let days_ago = published
        .until(today)
        .map(|span| span.get_days())
        .unwrap_or(i32::MAX);
    match days_ago {
        0 => "Today".to_owned(),
        1 => "Yesterday".to_owned(),
        2..=6 => published.strftime("%a").to_string(),
        _ if published.year() == today.year() => published.strftime("%b %-d").to_string(),
        _ => published.strftime("%b %-d, %Y").to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use rusqlite::params;
    use serde_json::Value;

    use super::*;
    use crate::store::tests::migrated_connection;

    fn ts(text: &str) -> Timestamp {
        text.parse().expect("test literal is RFC 3339")
    }

    fn id(text: &str) -> EpisodeId {
        text.parse().expect("test literal is a UUID")
    }

    #[test]
    fn loading_lasts_until_every_scope_is_complete() {
        let connection = migrated_connection();
        assert_eq!(
            load_loading(&connection).expect("the query runs"),
            Some(Loading {
                shows_done: 0,
                shows_total: 0,
                episodes: 0
            }),
            "nothing has synced right after sign-in"
        );
        connection
            .execute_batch(
                "insert into sync_scope (scope, version, complete) values
                     ('user', 4, 1), ('podcast:a', 9, 1), ('podcast:b', 0, 0);",
            )
            .expect("the scopes insert");
        assert_eq!(
            load_loading(&connection).expect("the query runs"),
            Some(Loading {
                shows_done: 1,
                shows_total: 2,
                episodes: 0
            })
        );
        connection
            .execute("update sync_scope set complete = 1", [])
            .expect("the scopes complete");
        assert_eq!(load_loading(&connection).expect("the query runs"), None);
    }

    /// Runs `conformance/inbox.json` against [`load_inbox`]; the README
    /// next to it describes the format.
    #[test]
    fn every_inbox_conformance_case_passes() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance/inbox.json");
        let text = std::fs::read_to_string(&path).expect("conformance/inbox.json is readable");
        let fixture: Value = serde_json::from_str(&text).expect("the fixture is JSON");
        let field = |row: &Value, key: &str| -> Value {
            row.get(key)
                .unwrap_or_else(|| panic!("fixture row {row} has `{key}`"))
                .clone()
        };
        let text_of = |row: &Value, key: &str| -> String {
            field(row, key)
                .as_str()
                .unwrap_or_else(|| panic!("`{key}` of {row} is a string"))
                .to_owned()
        };
        let ms_of = |row: &Value, key: &str| ts(&text_of(row, key)).as_epoch_ms();
        let flag_of = |row: &Value, key: &str| {
            field(row, key)
                .as_bool()
                .unwrap_or_else(|| panic!("`{key}` of {row} is a boolean"))
        };
        let rows = |key: &str| -> Vec<Value> {
            field(&fixture, key)
                .as_array()
                .unwrap_or_else(|| panic!("`{key}` is an array"))
                .clone()
        };

        let connection = migrated_connection();
        for podcast in rows("podcasts") {
            connection
                .execute(
                    "insert into podcast (id, feed_url, status, title, version)
                     values (?1, 'https://example.com/feed', 'ok', ?2, 1)",
                    params![text_of(&podcast, "id"), text_of(&podcast, "title")],
                )
                .expect("the podcast inserts");
        }
        for subscription in rows("subscriptions") {
            connection
                .execute(
                    "insert into subscription
                         (podcast_id, feed_url, subscribed, subscribed_at, subscribed_dev)
                     values (?1, 'https://example.com/feed', ?2, ?3, 'dev')",
                    params![
                        text_of(&subscription, "podcast_id"),
                        flag_of(&subscription, "subscribed"),
                        ms_of(&subscription, "subscribed_at"),
                    ],
                )
                .expect("the subscription inserts");
        }
        for episode in rows("episodes") {
            if flag_of(&episode, "deleted") {
                continue;
            }
            connection
                .execute(
                    "insert into episode (id, podcast_id, title, summary_html, content_html,
                         published_at, audio_url, audio_type, version)
                     values (?1, ?2, ?3, '', '', ?4, 'https://example.com/a.mp3', 'audio/mpeg', 1)",
                    params![
                        text_of(&episode, "id"),
                        text_of(&episode, "podcast_id"),
                        text_of(&episode, "title"),
                        ms_of(&episode, "published_at"),
                    ],
                )
                .expect("the episode inserts");
        }
        for state in rows("episode_states") {
            connection
                .execute(
                    "insert into episode_state
                         (episode_id, podcast_id, position_ms, played, archived)
                     values (?1, ?2, ?3, ?4, ?5)",
                    params![
                        text_of(&state, "episode_id"),
                        text_of(&state, "podcast_id"),
                        field(&state, "position_ms").as_i64(),
                        flag_of(&state, "played"),
                        flag_of(&state, "archived"),
                    ],
                )
                .expect("the episode state inserts");
        }

        let cases = rows("cases");
        assert!(!cases.is_empty());
        for case in cases {
            let name = text_of(&case, "name");
            connection
                .execute("delete from setting", [])
                .expect("settings are deletable");
            if let Some(days) = field(&case, "inbox_window_days").as_i64() {
                connection
                    .execute(
                        "insert into setting (key, value, value_at, value_dev)
                         values (?1, ?2, 0, 'dev')",
                        params![INBOX_WINDOW_DAYS, days.to_string()],
                    )
                    .expect("the setting inserts");
            }
            let expected: Vec<EpisodeId> = field(&case, "inbox")
                .as_array()
                .expect("`inbox` is an array")
                .iter()
                .map(|value| id(value.as_str().expect("episode ids are strings")))
                .collect();
            let inbox = load_inbox(&connection, ts(&text_of(&case, "now")), &TimeZone::UTC)
                .expect("the inbox query runs");
            let ids: Vec<EpisodeId> = inbox.iter().map(|episode| episode.id).collect();
            assert_eq!(ids, expected, "{name}");

            let podcasts = load_podcasts(&connection, ts(&text_of(&case, "now")))
                .expect("the podcast query runs");
            for podcast in &podcasts {
                let listed = inbox
                    .iter()
                    .filter(|episode| episode.podcast_id == podcast.id)
                    .count();
                assert_eq!(podcast.unplayed, listed, "{name}: {}", podcast.title);
            }
        }
    }

    #[test]
    fn a_left_row_hands_the_selection_down_then_up() {
        let [a, b, c] = [
            id("00000000-0000-4000-8000-00000000000a"),
            id("00000000-0000-4000-8000-00000000000b"),
            id("00000000-0000-4000-8000-00000000000c"),
        ];
        assert_eq!(reselect(&[a, b, c], &[a, b, c], Some(b)), Some(b));
        assert_eq!(reselect(&[a, b, c], &[a, c], Some(b)), Some(c));
        assert_eq!(reselect(&[a, b, c], &[a], Some(b)), Some(a));
        assert_eq!(reselect(&[a, b, c], &[a, b], Some(c)), Some(b));
        assert_eq!(reselect(&[a, b, c], &[], Some(b)), None);
        assert_eq!(reselect(&[], &[a, b], None), Some(a));
        assert_eq!(reselect(&[b], &[a, c], Some(b)), Some(a));
    }

    #[test]
    fn durations_round_to_minutes_or_seconds_under_one() {
        assert_eq!(duration_label(10_000), "10 s");
        assert_eq!(duration_label(30_000), "30 s");
        assert_eq!(duration_label(200), "1 s");
        assert_eq!(duration_label(59_400), "59 s");
        assert_eq!(duration_label(59_600), "1 min");
        assert_eq!(duration_label(90_000), "2 min");
        assert_eq!(duration_label(28 * 60_000 + 29_000), "28 min");
        assert_eq!(duration_label(64 * 60_000), "1 h 4 min");
        assert_eq!(duration_label(120 * 60_000), "2 h");
        assert_eq!(
            time_left_label(Some(52 * 60_000), 31 * 60_000),
            Some("21 min left".into())
        );
        assert_eq!(time_left_label(Some(52 * 60_000), 0), None);
        assert_eq!(time_left_label(None, 60_000), None);
    }

    #[test]
    fn days_are_calendar_days_in_the_local_zone() {
        let berlin = TimeZone::get("Europe/Berlin").expect("the tz database has Berlin");
        // 23:30 UTC on the 24th is already the 25th in Berlin.
        let now = ts("2026-10-24T23:30:00Z");
        assert_eq!(day_label(ts("2026-10-24T22:30:00Z"), now, &berlin), "Today");
        assert_eq!(
            day_label(ts("2026-10-24T21:30:00Z"), now, &berlin),
            "Yesterday"
        );
        assert_eq!(
            day_label(ts("2026-10-24T21:30:00Z"), now, &TimeZone::UTC),
            "Today"
        );
        assert_eq!(day_label(ts("2026-10-20T12:00:00Z"), now, &berlin), "Tue");
        assert_eq!(day_label(ts("2026-10-08T12:00:00Z"), now, &berlin), "Oct 8");
        assert_eq!(
            day_label(ts("2025-12-31T12:00:00Z"), now, &berlin),
            "Dec 31, 2025"
        );
    }
}
