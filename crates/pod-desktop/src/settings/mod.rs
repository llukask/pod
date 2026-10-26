//! The Settings page (wireframe D5): the account, Settings › Sync with the
//! changes that couldn't be saved, the synced playback settings and this
//! device's download settings.
//!
//! Synced settings (spec 2.4) are local writes with a `setting.set`
//! mutation, so they reach every device. Device settings (spec 7.1) are
//! plain `device_setting` rows that never leave this computer; the
//! reconciler reads them.

pub mod view;

use color_eyre::eyre::{Result, WrapErr as _};
use jiff::tz::TimeZone;
use pod_protocol::settings::{INBOX_WINDOW_DAYS, inbox_window_days};
use pod_protocol::{Mutation, RejectionCode, Timestamp};
use rusqlite::{Connection, params};

use crate::inbox::day_label;
use crate::player::settings::{
    PLAYBACK_SPEED, PLAYED_THRESHOLD_MS, PlayerSettings, SKIP_BACK_MS, SKIP_FORWARD_MS,
    SKIP_STEPS_MS, SPEEDS, load_values,
};
use crate::reconcile::plan::{self, DOWNLOAD_BUDGET_BYTES, EVICT_GRACE_HOURS, hours_to_ms};
use crate::store::{Store, Table, TableSet};
use crate::sync::SyncHandle;
use crate::sync::local_write::{self, LocalChange};

// ==============================================================================
// Values
// ==============================================================================

/// Every value the page edits, with the defaults filled in by the same
/// readers the player, the inbox and the reconciler use.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Values {
    pub player: PlayerSettings,
    pub inbox_window_days: i64,
    pub device: plan::Settings,
}

/// What the page shows, from one live query.
#[derive(Clone, Debug, PartialEq)]
pub struct SettingsPage {
    pub values: Values,
    /// The rejected log, newest first.
    pub rejected: Vec<Rejection>,
}

pub fn tables() -> TableSet {
    [
        Table::Setting,
        Table::DeviceSetting,
        Table::RejectedMutation,
        Table::Episode,
    ]
    .into_iter()
    .collect()
}

pub fn load(connection: &Connection) -> Result<SettingsPage> {
    let synced = load_values(connection).context("load the synced settings")?;
    let values = Values {
        player: PlayerSettings::from_values(&synced),
        inbox_window_days: inbox_window_days(synced.get(INBOX_WINDOW_DAYS)),
        device: crate::reconcile::load_settings(connection).context("load the device settings")?,
    };
    Ok(SettingsPage {
        values,
        rejected: load_rejected(connection).context("load the rejected log")?,
    })
}

// ==============================================================================
// Stepped settings
// ==============================================================================

/// A setting the page edits with a stepper. Each steps through a fixed
/// list that lies inside the range its reader accepts, so the controls
/// can't write a value another device would read as the default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Speed,
    SkipBack,
    SkipForward,
    PlayedThreshold,
    InboxWindow,
    Budget,
    EvictGrace,
}

/// Where a setting is stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// A synced setting key: a local write with a mutation.
    Synced(&'static str),
    /// A `device_setting` key, kept on this computer.
    Device(&'static str),
}

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

impl Field {
    /// The element id of the value; the buttons add `-less` and `-more`.
    pub fn id(self) -> &'static str {
        match self {
            Field::Speed => "setting-speed",
            Field::SkipBack => "setting-skip-back",
            Field::SkipForward => "setting-skip-forward",
            Field::PlayedThreshold => "setting-played-threshold",
            Field::InboxWindow => "setting-inbox-window",
            Field::Budget => "setting-budget",
            Field::EvictGrace => "setting-evict-grace",
        }
    }

    pub fn target(self) -> Target {
        match self {
            Field::Speed => Target::Synced(PLAYBACK_SPEED),
            Field::SkipBack => Target::Synced(SKIP_BACK_MS),
            Field::SkipForward => Target::Synced(SKIP_FORWARD_MS),
            Field::PlayedThreshold => Target::Synced(PLAYED_THRESHOLD_MS),
            Field::InboxWindow => Target::Synced(INBOX_WINDOW_DAYS),
            Field::Budget => Target::Device(DOWNLOAD_BUDGET_BYTES),
            Field::EvictGrace => Target::Device(EVICT_GRACE_HOURS),
        }
    }

    /// The steps, in the unit [`Field::get`] reads: times, milliseconds,
    /// days, bytes or hours.
    pub fn steps(self) -> Vec<f64> {
        let ms = |steps: &[i64]| steps.iter().map(|ms| *ms as f64).collect();
        match self {
            Field::Speed => SPEEDS.to_vec(),
            Field::SkipBack | Field::SkipForward => ms(&SKIP_STEPS_MS),
            Field::PlayedThreshold => ms(&[0, 5_000, 10_000, 15_000, 30_000, 60_000, 120_000]),
            Field::InboxWindow => vec![1.0, 3.0, 7.0, 14.0, 30.0, 60.0, 90.0, 180.0, 365.0],
            Field::Budget => [0.5, 1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0]
                .map(|gib| gib * GIB)
                .to_vec(),
            Field::EvictGrace => vec![0.0, 1.0, 6.0, 12.0, 24.0, 48.0, 72.0, 168.0],
        }
    }

    pub fn get(self, values: &Values) -> f64 {
        match self {
            Field::Speed => values.player.speed,
            Field::SkipBack => values.player.skip_back_ms as f64,
            Field::SkipForward => values.player.skip_forward_ms as f64,
            Field::PlayedThreshold => values.player.played_threshold_ms as f64,
            Field::InboxWindow => values.inbox_window_days as f64,
            Field::Budget => values.device.budget as f64,
            Field::EvictGrace => values.device.grace_ms as f64 / 3_600_000.0,
        }
    }

    /// Sets the value the page shows until the store confirms the write.
    pub fn set(self, values: &mut Values, value: f64) {
        let int = value.round() as i64;
        match self {
            Field::Speed => values.player.speed = value,
            Field::SkipBack => values.player.skip_back_ms = int,
            Field::SkipForward => values.player.skip_forward_ms = int,
            Field::PlayedThreshold => values.player.played_threshold_ms = int,
            Field::InboxWindow => values.inbox_window_days = int,
            Field::Budget => values.device.budget = value.round() as u64,
            Field::EvictGrace => values.device.grace_ms = hours_to_ms(value),
        }
    }

    /// The value as stored: a number for the speed, an integer otherwise,
    /// as spec 2.4 and the reconciler read them.
    pub fn json(self, value: f64) -> serde_json::Value {
        match self {
            Field::Speed => value.into(),
            _ => (value.round() as i64).into(),
        }
    }

    pub fn label(self, value: f64) -> String {
        let millis = |ms: f64| {
            if ms >= 60_000.0 && ms % 60_000.0 == 0.0 {
                format!("{} min", ms / 60_000.0)
            } else {
                format!("{} s", ms / 1000.0)
            }
        };
        match self {
            Field::Speed => format!("{value}×"),
            Field::SkipBack | Field::SkipForward | Field::PlayedThreshold => millis(value),
            Field::InboxWindow if value == 1.0 => "1 day".to_owned(),
            Field::InboxWindow => format!("{value} days"),
            Field::Budget if value < GIB => format!("{} MiB", (value / GIB * 1024.0).round()),
            Field::Budget => format!("{} GiB", (value / GIB * 10.0).round() / 10.0),
            Field::EvictGrace if value == 0.0 => "None".to_owned(),
            Field::EvictGrace if value >= 48.0 && value % 24.0 == 0.0 => {
                format!("{} days", value / 24.0)
            }
            Field::EvictGrace => format!("{value} h"),
        }
    }
}

/// The step before (`up` false) or after `current` in `steps`. A value
/// between steps moves to the neighbouring one; at either end, or beyond
/// it, the value stays.
pub fn step(steps: &[f64], current: f64, up: bool) -> f64 {
    let next = if up {
        steps.iter().find(|step| **step > current)
    } else {
        steps.iter().rev().find(|step| **step < current)
    };
    next.copied().unwrap_or(current)
}

// ==============================================================================
// Writes
// ==============================================================================

/// Stores `value` under `target`: a synced setting through a local write,
/// so the next sync takes it to the other devices, or a device setting
/// as a plain row.
pub async fn save(
    store: &Store,
    sync: &SyncHandle,
    target: Target,
    value: serde_json::Value,
) -> Result<()> {
    match target {
        Target::Synced(key) => {
            local_write::write(
                store,
                sync,
                LocalChange::Setting {
                    key: key.to_owned(),
                    value,
                },
            )
            .await
            .with_context(|| format!("write synced setting {key}"))?;
        }
        Target::Device(key) => {
            let text = value.to_string();
            store
                .write(move |transaction| {
                    transaction
                        .execute(
                            "insert into device_setting (key, value) values (?1, ?2)
                             on conflict (key) do update set value = excluded.value",
                            params![key, text],
                        )
                        .context("upsert the device_setting row")?;
                    Ok(())
                })
                .await
                .with_context(|| format!("write device setting {key}"))?;
        }
    }
    Ok(())
}

// ==============================================================================
// Settings › Sync
// ==============================================================================

/// A change in the rejected log, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rejection {
    pub mutation_id: String,
    /// What the change was, e.g. "Mark “Episode 1” played".
    pub change: String,
    /// Why it couldn't be saved.
    pub reason: String,
}

fn load_rejected(connection: &Connection) -> Result<Vec<Rejection>> {
    let mut statement = connection
        .prepare_cached(
            "select rejected.mutation_id, rejected.payload, rejected.reason, episode.title
             from rejected_mutation rejected
             left join episode on episode.id = json_extract(rejected.payload, '$.episode_id')
             order by rejected.rejected_at desc, rejected.mutation_id desc",
        )
        .context("prepare the rejected log query")?;
    statement
        .query_map([], |row| {
            let payload: String = row.get(1)?;
            let reason: String = row.get(2)?;
            let title: Option<String> = row.get(3)?;
            Ok(Rejection {
                mutation_id: row.get(0)?,
                change: describe_change(&payload, title.as_deref()),
                reason: describe_reason(&reason),
            })
        })
        .context("query the rejected log")?
        .collect::<rusqlite::Result<_>>()
        .context("read the rejected log")
}

/// The change a logged mutation made, from its JSON payload. `episode`
/// is the episode's title when the store still has it.
pub fn describe_change(payload: &str, episode: Option<&str>) -> String {
    let Ok(mutation) = serde_json::from_str::<Mutation>(payload) else {
        return "A change".to_owned();
    };
    let episode = episode.map_or("an episode".to_owned(), |title| format!("“{title}”"));
    match mutation {
        Mutation::SubscriptionSet(m) if m.subscribed => format!("Subscribe to {}", m.feed_url),
        Mutation::SubscriptionSet(m) => format!("Unsubscribe from {}", m.feed_url),
        Mutation::EpisodePosition(_) => format!("Save the position in {episode}"),
        Mutation::EpisodePlayed(m) if m.played => format!("Mark {episode} played"),
        Mutation::EpisodePlayed(_) => format!("Mark {episode} unplayed"),
        Mutation::EpisodeArchived(m) if m.archived => format!("Archive {episode}"),
        Mutation::EpisodeArchived(_) => format!("Unarchive {episode}"),
        Mutation::SettingSet(m) => match m.key.as_str() {
            PLAYBACK_SPEED => "Change the playback speed".to_owned(),
            SKIP_BACK_MS => "Change the skip back interval".to_owned(),
            SKIP_FORWARD_MS => "Change the skip forward interval".to_owned(),
            PLAYED_THRESHOLD_MS => "Change when episodes count as played".to_owned(),
            INBOX_WINDOW_DAYS => "Change how far back the inbox goes".to_owned(),
            key => format!("Change the setting “{key}”"),
        },
    }
}

/// Why the server refused a change, for a rejection code or the engine's
/// `bad_request` after 5 refused attempts. The codes are an open set
/// (spec 4.3), so an unknown one reads as a plain refusal.
pub fn describe_reason(reason: &str) -> String {
    if reason == "bad_request" {
        return "The server refused it five times.".to_owned();
    }
    match serde_json::from_value(reason.into()) {
        Ok(RejectionCode::InvalidMutation) => "The server didn't understand it.".to_owned(),
        Ok(RejectionCode::UnknownEpisode) => "The server doesn't know this episode.".to_owned(),
        Ok(RejectionCode::InvalidFeedUrl) => "The server can't use this feed address.".to_owned(),
        Ok(RejectionCode::Unknown(_)) | Err(_) => format!("The server refused it ({reason})."),
    }
}

/// "Last full sync" and when, in `zone`: the time alone today, the day
/// and the time before that.
pub fn last_sync_label(at: Timestamp, now: Timestamp, zone: &TimeZone) -> String {
    let time = at
        .as_jiff()
        .to_zoned(zone.clone())
        .strftime("%H:%M")
        .to_string();
    match day_label(at, now, zone).as_str() {
        "Today" => format!("Last full sync {time}"),
        day => format!("Last full sync {day} {time}"),
    }
}

#[cfg(test)]
mod tests;
