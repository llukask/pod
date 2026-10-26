//! Synced settings the clients understand, with their defaults (spec 2.4).
//!
//! The server fills these defaults for keys a user never set, and clients use
//! them before the first sync. Keys outside this table are still stored and
//! synced unchanged, so older clients keep what newer ones write.

/// A known setting's default value, typed as the spec's table types it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SettingDefault {
    Int(i64),
    Number(f64),
}

impl SettingDefault {
    /// The default as the JSON a [`Setting`](crate::Setting) carries.
    pub fn to_json(self) -> serde_json::Value {
        match self {
            Self::Int(value) => value.into(),
            Self::Number(value) => value.into(),
        }
    }
}

/// A setting key the clients understand.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KnownSetting {
    pub key: &'static str,
    pub default: SettingDefault,
}

pub const INBOX_WINDOW_DAYS: &str = "inbox_window_days";
pub const PLAYBACK_SPEED: &str = "playback_speed";
pub const SKIP_BACK_MS: &str = "skip_back_ms";
pub const SKIP_FORWARD_MS: &str = "skip_forward_ms";
pub const PLAYED_THRESHOLD_MS: &str = "played_threshold_ms";

/// Every known setting, in the spec's order.
pub const KNOWN_SETTINGS: [KnownSetting; 5] = [
    KnownSetting {
        key: INBOX_WINDOW_DAYS,
        default: SettingDefault::Int(DEFAULT_INBOX_WINDOW_DAYS),
    },
    KnownSetting {
        key: PLAYBACK_SPEED,
        default: SettingDefault::Number(1.0),
    },
    KnownSetting {
        key: SKIP_BACK_MS,
        default: SettingDefault::Int(15_000),
    },
    KnownSetting {
        key: SKIP_FORWARD_MS,
        default: SettingDefault::Int(30_000),
    },
    KnownSetting {
        key: PLAYED_THRESHOLD_MS,
        default: SettingDefault::Int(30_000),
    },
];

/// The default for `key`, or `None` for a key outside [`KNOWN_SETTINGS`].
pub fn setting_default(key: &str) -> Option<serde_json::Value> {
    KNOWN_SETTINGS
        .iter()
        .find(|setting| setting.key == key)
        .map(|setting| setting.default.to_json())
}

/// The window used when the setting is unset or unusable.
pub const DEFAULT_INBOX_WINDOW_DAYS: i64 = 30;

/// Longest window honoured, so `now - window` stays a valid timestamp.
/// A century covers every real back catalogue.
pub const MAX_INBOX_WINDOW_DAYS: i64 = 36_500;

/// The inbox window in days for the stored `inbox_window_days` value
/// (spec 6). The server and every client read the setting through this.
///
/// The spec types the setting as an int. Anything else (a fraction, a
/// negative number, a string, null) is a client bug that mustn't empty or
/// break the inbox, so it reads as the default, the value a client sees
/// before it ever set the key.
pub fn inbox_window_days(stored: Option<&serde_json::Value>) -> i64 {
    stored
        .and_then(serde_json::Value::as_i64)
        .filter(|days| *days >= 0)
        .map_or(DEFAULT_INBOX_WINDOW_DAYS, |days| {
            days.min(MAX_INBOX_WINDOW_DAYS)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_spec_table() {
        let defaults: serde_json::Map<String, serde_json::Value> = KNOWN_SETTINGS
            .iter()
            .map(|setting| (setting.key.to_owned(), setting.default.to_json()))
            .collect();
        insta::assert_json_snapshot!(defaults, @r#"
        {
          "inbox_window_days": 30,
          "playback_speed": 1.0,
          "skip_back_ms": 15000,
          "skip_forward_ms": 30000,
          "played_threshold_ms": 30000
        }
        "#);
    }

    #[test]
    fn unknown_keys_have_no_default() {
        assert_eq!(
            setting_default(PLAYBACK_SPEED),
            Some(serde_json::json!(1.0))
        );
        assert_eq!(setting_default("theme"), None);
    }

    #[test]
    fn the_window_setting_reads_non_negative_integers() {
        use serde_json::json;
        assert_eq!(inbox_window_days(None), 30);
        assert_eq!(inbox_window_days(Some(&json!(7))), 7);
        assert_eq!(inbox_window_days(Some(&json!(0))), 0);
        assert_eq!(inbox_window_days(Some(&json!(1_000_000))), 36_500);
        for unusable in [json!(-1), json!(2.5), json!("7"), json!(null), json!([7])] {
            assert_eq!(inbox_window_days(Some(&unusable)), 30, "{unusable}");
        }
    }
}
