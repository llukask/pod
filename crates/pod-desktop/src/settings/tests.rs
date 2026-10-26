use jiff::tz::TimeZone;
use pod_protocol::settings::MAX_INBOX_WINDOW_DAYS;
use serde_json::json;

use super::*;
use crate::player::settings::SPEED_RANGE;

const ALL: [Field; 7] = [
    Field::Speed,
    Field::SkipBack,
    Field::SkipForward,
    Field::PlayedThreshold,
    Field::InboxWindow,
    Field::Budget,
    Field::EvictGrace,
];

#[test]
fn every_step_reads_back_as_itself() {
    let mut values = Values {
        player: PlayerSettings::default(),
        inbox_window_days: 30,
        device: plan::Settings::default(),
    };
    for field in ALL {
        for value in field.steps() {
            field.set(&mut values, value);
            assert_eq!(field.get(&values), value, "{field:?}");
        }
    }
}

/// The readers fall back to the default for values outside these ranges
/// (spec 2.4, 6 and 7.1), so no control may step there.
#[test]
fn steps_stay_inside_the_ranges_the_readers_accept() {
    for field in ALL {
        let steps = field.steps();
        assert!(steps.is_sorted(), "{field:?} steps ascend");
        let range = match field {
            Field::Speed => SPEED_RANGE,
            Field::InboxWindow => 0.0..=MAX_INBOX_WINDOW_DAYS as f64,
            Field::Budget => 1.0..=f64::MAX,
            _ => 0.0..=f64::MAX,
        };
        for value in steps {
            assert!(range.contains(&value), "{field:?} step {value}");
        }
    }
}

#[test]
fn steppers_move_to_the_neighbouring_step_and_stop_at_the_ends() {
    let steps = [1.0, 2.0, 5.0];
    assert_eq!(step(&steps, 2.0, true), 5.0);
    assert_eq!(step(&steps, 2.0, false), 1.0);
    assert_eq!(
        step(&steps, 3.0, true),
        5.0,
        "between steps goes to the next"
    );
    assert_eq!(step(&steps, 3.0, false), 2.0);
    assert_eq!(step(&steps, 5.0, true), 5.0);
    assert_eq!(step(&steps, 1.0, false), 1.0);
    assert_eq!(step(&steps, 9.0, true), 9.0, "beyond the end stays");
    assert_eq!(step(&steps, 9.0, false), 5.0);
}

#[test]
fn values_are_stored_as_their_readers_type_them() {
    assert_eq!(Field::Speed.json(1.3), json!(1.3));
    assert_eq!(Field::SkipBack.json(10_000.0), json!(10_000));
    assert_eq!(Field::InboxWindow.json(7.0), json!(7));
    assert_eq!(Field::Budget.json(5.0 * GIB), json!(5_368_709_120_u64));
    assert_eq!(Field::EvictGrace.json(48.0), json!(48));
}

#[test]
fn each_field_has_its_wording() {
    let cases = [
        (Field::Speed, 1.0, "1×"),
        (Field::Speed, 1.75, "1.75×"),
        (Field::SkipBack, 15_000.0, "15 s"),
        (Field::SkipForward, 60_000.0, "1 min"),
        (Field::PlayedThreshold, 0.0, "0 s"),
        (Field::PlayedThreshold, 120_000.0, "2 min"),
        (Field::InboxWindow, 1.0, "1 day"),
        (Field::InboxWindow, 30.0, "30 days"),
        (Field::Budget, 0.5 * GIB, "512 MiB"),
        (Field::Budget, 10.0 * GIB, "10 GiB"),
        (Field::EvictGrace, 0.0, "None"),
        (Field::EvictGrace, 24.0, "24 h"),
        (Field::EvictGrace, 168.0, "7 days"),
    ];
    for (field, value, label) in cases {
        assert_eq!(field.label(value), label, "{field:?} {value}");
    }
}

fn payload(mutation: serde_json::Value) -> String {
    let mut mutation = mutation;
    let fields = mutation.as_object_mut().expect("an object");
    fields.insert("id".into(), json!("018f0000-0000-7000-8000-000000000001"));
    fields.insert("at".into(), json!("2026-10-24T10:12:30.000Z"));
    fields.insert("dev".into(), json!("018f0000-0000-7000-8000-000000000002"));
    mutation.to_string()
}

#[test]
fn logged_changes_say_what_they_were() {
    let episode = "018f0000-0000-7000-8000-000000000003";
    let podcast = "018f0000-0000-7000-8000-000000000004";
    let played = payload(json!({"type": "episode.played", "episode_id": episode, "played": true}));
    assert_eq!(
        describe_change(&played, Some("Episode 1")),
        "Mark “Episode 1” played"
    );
    assert_eq!(describe_change(&played, None), "Mark an episode played");
    let archived =
        payload(json!({"type": "episode.archived", "episode_id": episode, "archived": false}));
    assert_eq!(
        describe_change(&archived, Some("Episode 1")),
        "Unarchive “Episode 1”"
    );
    let position =
        payload(json!({"type": "episode.position", "episode_id": episode, "position_ms": 5}));
    assert_eq!(
        describe_change(&position, None),
        "Save the position in an episode"
    );
    let subscribe = payload(json!({
        "type": "subscription.set", "podcast_id": podcast,
        "feed_url": "https://example.com/feed.xml", "subscribed": true
    }));
    assert_eq!(
        describe_change(&subscribe, None),
        "Subscribe to https://example.com/feed.xml"
    );
    let speed = payload(json!({"type": "setting.set", "key": "playback_speed", "value": 2}));
    assert_eq!(describe_change(&speed, None), "Change the playback speed");
    let theme = payload(json!({"type": "setting.set", "key": "theme", "value": "dark"}));
    assert_eq!(describe_change(&theme, None), "Change the setting “theme”");
    assert_eq!(describe_change("{\"type\": \"later\"}", None), "A change");
}

#[test]
fn every_reason_reads_as_a_sentence_and_unknown_codes_fall_back() {
    assert_eq!(
        describe_reason("invalid_mutation"),
        "The server didn't understand it."
    );
    assert_eq!(
        describe_reason("unknown_episode"),
        "The server doesn't know this episode."
    );
    assert_eq!(
        describe_reason("invalid_feed_url"),
        "The server can't use this feed address."
    );
    assert_eq!(
        describe_reason("bad_request"),
        "The server refused it five times."
    );
    assert_eq!(
        describe_reason("quota_exceeded"),
        "The server refused it (quota_exceeded)."
    );
}

#[test]
fn the_last_sync_shows_the_day_only_before_today() {
    let zone = TimeZone::UTC;
    let at = |text: &str| {
        Timestamp::from_epoch_ms(
            text.parse::<jiff::Timestamp>()
                .expect("a valid timestamp")
                .as_millisecond(),
        )
        .expect("in range")
    };
    let now = at("2026-10-26T15:00:00Z");
    assert_eq!(
        last_sync_label(at("2026-10-26T10:02:00Z"), now, &zone),
        "Last full sync 10:02"
    );
    assert_eq!(
        last_sync_label(at("2026-10-25T23:59:00Z"), now, &zone),
        "Last full sync Yesterday 23:59"
    );
}
