//! Millisecond timestamps (spec sections 2 and 4).
//!
//! The server truncates every timestamp to whole milliseconds before storing
//! or comparing it, and clients keep epoch milliseconds in SQLite. Holding the
//! truncation in the type means a value compares the same on every side.

use std::fmt;
use std::str::FromStr;

use jiff::{RoundMode, TimestampRound, Unit};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use utoipa::openapi::schema::{ObjectBuilder, SchemaFormat, Type};
use utoipa::openapi::{KnownFormat, RefOr, Schema};

/// An instant with whole-millisecond precision.
///
/// It serializes as RFC 3339 in UTC with exactly three fractional digits
/// (`2026-10-24T09:12:03.120Z`). Parsing accepts any RFC 3339 instant, with
/// any offset and precision, and floors it to the millisecond, which matches
/// Postgres `date_trunc('milliseconds', ...)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(jiff::Timestamp);

/// A timestamp outside the range `jiff` supports (years -9999 to 9999).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("timestamp is out of range: {0} ms since the epoch")]
pub struct TimestampOutOfRange(pub i64);

/// A string that isn't an RFC 3339 instant.
#[derive(Debug, Clone, thiserror::Error)]
#[error("invalid RFC 3339 timestamp: {0}")]
pub struct ParseTimestampError(#[from] jiff::Error);

impl Timestamp {
    /// The current time, floored to the millisecond.
    pub fn now() -> Self {
        Self::from_jiff(jiff::Timestamp::now())
    }

    /// Floors `timestamp` to the millisecond.
    pub fn from_jiff(timestamp: jiff::Timestamp) -> Self {
        let floored = timestamp
            .round(
                TimestampRound::new()
                    .smallest(Unit::Millisecond)
                    .mode(RoundMode::Floor),
            )
            .expect("flooring never moves past jiff's minimum, which is a whole second");
        Self(floored)
    }

    /// The instant as a `jiff` timestamp, for arithmetic such as the
    /// server's clamp to `now + 60 s`.
    pub const fn as_jiff(self) -> jiff::Timestamp {
        self.0
    }

    /// The timestamp `epoch_ms` milliseconds after 1970-01-01T00:00:00Z, the
    /// form clients store in SQLite.
    pub fn from_epoch_ms(epoch_ms: i64) -> Result<Self, TimestampOutOfRange> {
        jiff::Timestamp::from_millisecond(epoch_ms)
            .map(Self)
            .map_err(|_| TimestampOutOfRange(epoch_ms))
    }

    /// Milliseconds since 1970-01-01T00:00:00Z.
    pub fn as_epoch_ms(self) -> i64 {
        self.0.as_millisecond()
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // jiff's `Display` for `Timestamp` prints UTC with a `Z` suffix and
        // honours the precision as the exact number of fractional digits.
        write!(f, "{:.3}", self.0)
    }
}

impl FromStr for Timestamp {
    type Err = ParseTimestampError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::from_jiff(s.parse()?))
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

impl utoipa::PartialSchema for Timestamp {
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .schema_type(Type::String)
            .format(Some(SchemaFormat::KnownFormat(KnownFormat::DateTime)))
            .description(Some(
                "RFC 3339 instant. The server always writes UTC with exactly three \
                 fractional digits and floors any input to whole milliseconds.",
            ))
            .examples(["2026-10-24T09:12:03.120Z"])
            .into()
    }
}

impl utoipa::ToSchema for Timestamp {}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> Timestamp {
        s.parse().expect("test literal is RFC 3339")
    }

    #[test]
    fn serializes_with_exactly_three_fraction_digits() {
        let json = |s| serde_json::to_string(&ts(s)).expect("timestamps serialize");
        assert_eq!(
            json("2026-10-24T09:12:03Z"),
            r#""2026-10-24T09:12:03.000Z""#
        );
        assert_eq!(
            json("2026-10-24T09:12:03.12Z"),
            r#""2026-10-24T09:12:03.120Z""#
        );
    }

    #[test]
    fn parsing_floors_to_the_millisecond_and_normalizes_to_utc() {
        assert_eq!(
            ts("2026-10-24T11:12:03.120999+02:00").to_string(),
            "2026-10-24T09:12:03.120Z"
        );
        assert_eq!(
            ts("1969-12-31T23:59:59.9995Z").as_epoch_ms(),
            -1,
            "pre-epoch instants floor rather than truncate toward zero"
        );
    }

    #[test]
    fn round_trips_through_epoch_ms() {
        let at = ts("2026-10-24T09:12:03.120Z");
        assert_eq!(at.as_epoch_ms(), 1_792_833_123_120);
        assert_eq!(Timestamp::from_epoch_ms(at.as_epoch_ms()), Ok(at));
        assert!(Timestamp::from_epoch_ms(i64::MAX).is_err());
    }

    #[test]
    fn now_is_whole_milliseconds() {
        let now = Timestamp::now();
        assert_eq!(now.as_jiff().subsec_nanosecond() % 1_000_000, 0);
    }

    #[test]
    fn rejects_non_timestamps() {
        assert!(serde_json::from_str::<Timestamp>(r#""yesterday""#).is_err());
        assert!(serde_json::from_str::<Timestamp>("1792833123120").is_err());
    }

    #[test]
    fn orders_chronologically() {
        assert!(ts("2026-10-24T09:12:03.119Z") < ts("2026-10-24T09:12:03.120Z"));
    }
}
