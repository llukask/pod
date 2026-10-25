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
        default: SettingDefault::Int(30),
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
}
