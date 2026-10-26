//! The synced settings the player applies (spec 2.4), and the steps the
//! player bar cycles them through.

use std::collections::HashMap;

use color_eyre::eyre::{Result, WrapErr};
use rusqlite::Connection;

pub const PLAYBACK_SPEED: &str = "playback_speed";
pub const SKIP_BACK_MS: &str = "skip_back_ms";
pub const SKIP_FORWARD_MS: &str = "skip_forward_ms";
pub const PLAYED_THRESHOLD_MS: &str = "played_threshold_ms";

/// The speeds the speed chip steps through.
pub const SPEEDS: [f64; 8] = [0.8, 1.0, 1.1, 1.2, 1.3, 1.5, 1.75, 2.0];

/// The skip intervals a secondary click on a skip button steps through.
pub const SKIP_STEPS_MS: [i64; 6] = [5_000, 10_000, 15_000, 30_000, 45_000, 60_000];

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayerSettings {
    pub speed: f64,
    pub skip_back_ms: i64,
    pub skip_forward_ms: i64,
    pub played_threshold_ms: i64,
}

impl Default for PlayerSettings {
    fn default() -> Self {
        Self {
            speed: 1.0,
            skip_back_ms: 15_000,
            skip_forward_ms: 30_000,
            played_threshold_ms: 30_000,
        }
    }
}

impl PlayerSettings {
    /// Reads the settings from their JSON values by key. A missing value,
    /// or one of the wrong type or out of range, reads as the default, as
    /// spec 2.4 asks of clients.
    pub fn from_values(values: &HashMap<String, serde_json::Value>) -> PlayerSettings {
        let defaults = PlayerSettings::default();
        // mpv's own speed limits are 0.01 to 100; anything outside a range
        // a person would listen at is more likely a bad value than a wish.
        let speed = values
            .get(PLAYBACK_SPEED)
            .and_then(serde_json::Value::as_f64)
            .filter(|speed| (0.25..=4.0).contains(speed))
            .unwrap_or(defaults.speed);
        let millis = |key: &str, default: i64| {
            values
                .get(key)
                .and_then(serde_json::Value::as_i64)
                .filter(|ms| *ms >= 0)
                .unwrap_or(default)
        };
        PlayerSettings {
            speed,
            skip_back_ms: millis(SKIP_BACK_MS, defaults.skip_back_ms),
            skip_forward_ms: millis(SKIP_FORWARD_MS, defaults.skip_forward_ms),
            played_threshold_ms: millis(PLAYED_THRESHOLD_MS, defaults.played_threshold_ms),
        }
    }

    pub fn load(connection: &Connection) -> Result<PlayerSettings> {
        let mut statement = connection
            .prepare_cached("select key, value from setting where key in (?1, ?2, ?3, ?4)")
            .context("prepare the settings query")?;
        let values = statement
            .query_map(
                [
                    PLAYBACK_SPEED,
                    SKIP_BACK_MS,
                    SKIP_FORWARD_MS,
                    PLAYED_THRESHOLD_MS,
                ],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .context("query the settings")?
            .filter_map(|row| match row {
                Ok((key, text)) => serde_json::from_str(&text)
                    .ok()
                    .map(|value| Ok((key, value))),
                Err(error) => Some(Err(error)),
            })
            .collect::<rusqlite::Result<_>>()
            .context("read the settings")?;
        Ok(PlayerSettings::from_values(&values))
    }
}

/// The step after `current` in `steps`, wrapping around. A value between
/// steps moves to the next one up.
pub fn next_step<T: PartialOrd + Copy>(steps: &[T], current: T) -> T {
    steps
        .iter()
        .copied()
        .find(|step| *step > current)
        .unwrap_or(steps[0])
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn values(pairs: &[(&str, serde_json::Value)]) -> HashMap<String, serde_json::Value> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), value.clone()))
            .collect()
    }

    #[test]
    fn missing_settings_read_as_the_spec_defaults() {
        assert_eq!(
            PlayerSettings::from_values(&HashMap::new()),
            PlayerSettings {
                speed: 1.0,
                skip_back_ms: 15_000,
                skip_forward_ms: 30_000,
                played_threshold_ms: 30_000,
            }
        );
    }

    #[test]
    fn stored_settings_apply_and_bad_values_fall_back() {
        let settings = PlayerSettings::from_values(&values(&[
            (PLAYBACK_SPEED, json!(1.3)),
            (SKIP_BACK_MS, json!(10_000)),
            (SKIP_FORWARD_MS, json!("long")),
            (PLAYED_THRESHOLD_MS, json!(-1)),
        ]));
        assert_eq!(settings.speed, 1.3);
        assert_eq!(settings.skip_back_ms, 10_000);
        assert_eq!(settings.skip_forward_ms, 30_000);
        assert_eq!(settings.played_threshold_ms, 30_000);

        let fast = PlayerSettings::from_values(&values(&[(PLAYBACK_SPEED, json!("fast"))]));
        assert_eq!(fast.speed, 1.0);
        let absurd = PlayerSettings::from_values(&values(&[(PLAYBACK_SPEED, json!(50))]));
        assert_eq!(absurd.speed, 1.0);
    }

    #[test]
    fn steps_cycle_upwards_and_wrap() {
        assert_eq!(next_step(&SPEEDS, 1.0), 1.1);
        assert_eq!(next_step(&SPEEDS, 1.15), 1.2, "between steps goes up");
        assert_eq!(next_step(&SPEEDS, 2.0), 0.8);
        assert_eq!(next_step(&SKIP_STEPS_MS, 60_000), 5_000);
    }
}
