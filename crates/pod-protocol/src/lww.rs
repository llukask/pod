//! Last-writer-wins rule shared by the server and every client (spec 4.4
//! and 5.4).
//!
//! Each mutable user-state field carries the time of the action that wrote
//! it and the device that made it. The server decides each mutation with
//! [`wins`], after clamping its `at` with [`clamp_mutation_at`]. Clients run
//! the same [`wins`] when they merge pulled rows (see the `merge` methods in
//! [`crate::local`]), so both sides agree on every field.

use jiff::SignedDuration;

use crate::{DeviceId, Timestamp};

/// How far past the server's clock a mutation's `at` may lie. Later stamps
/// are pulled back to `server_now + MAX_CLOCK_LEAD`, so a device with a fast
/// clock can't pin a field against every later edit (spec 4.4).
pub const MAX_CLOCK_LEAD: SignedDuration = SignedDuration::from_secs(60);

/// The `(*_at, *_dev)` pair stamped on a field by the action that wrote it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Stamp {
    pub at: Timestamp,
    pub dev: DeviceId,
}

impl Stamp {
    /// Reads a nullable `*_at` / `*_dev` column pair.
    ///
    /// Every write sets both columns together, so a pair is either fully
    /// stamped or fully null. A half-null pair can't be produced by any rule
    /// and reads as unstamped.
    pub fn from_columns(at: Option<Timestamp>, dev: Option<DeviceId>) -> Option<Self> {
        Some(Self { at: at?, dev: dev? })
    }
}

/// Whether a write stamped `new` replaces a field currently stamped `old`
/// (`None` when the field was never written).
///
/// The later `at` wins, and the greater device id breaks a tie. An exact tie
/// from the same device also goes to the new write: clamping can collapse two
/// of one device's mutations into the same millisecond, and since a device's
/// mutations arrive in the order it made them, the one applied later is the
/// newer action (spec 4.4).
///
/// Device ids compare as their canonical lowercase strings (spec 5.4).
/// [`DeviceId`]'s `Ord` compares the UUID bytes, which orders the same way:
/// the canonical string is the bytes in order as lowercase hex, with hyphens
/// at fixed positions, and ASCII orders `0-9` before `a-f`. A property test
/// in this module checks the two agree.
pub fn wins(new: Stamp, old: Option<Stamp>) -> bool {
    old.is_none_or(|old| new.at > old.at || (new.at == old.at && new.dev >= old.dev))
}

/// The `at` the server stores for a mutation stamped `at` that arrives at
/// `server_now`: `truncate_ms(min(at, server_now + 60 s))` (spec 4.4).
///
/// [`Timestamp`] already holds whole milliseconds, so the truncation is in
/// the type.
pub fn clamp_mutation_at(at: Timestamp, server_now: Timestamp) -> Timestamp {
    // `checked_add` only fails past jiff's maximum instant, which no `at`
    // can exceed, so `at` is already within the limit.
    match server_now.as_jiff().checked_add(MAX_CLOCK_LEAD) {
        Ok(limit) => at.min(Timestamp::from_jiff(limit)),
        Err(_) => at,
    }
}

#[cfg(test)]
mod tests {
    use quickcheck::quickcheck;
    use uuid::Uuid;

    use super::*;

    quickcheck! {
        fn device_id_order_matches_canonical_string_order(a: u128, b: u128) -> bool {
            let a = DeviceId::from_uuid(Uuid::from_u128(a));
            let b = DeviceId::from_uuid(Uuid::from_u128(b));
            a.cmp(&b) == a.to_string().cmp(&b.to_string())
        }
    }

    #[test]
    fn a_half_null_column_pair_reads_as_unstamped() {
        let at: Timestamp = "2026-10-24T09:00:00Z".parse().expect("literal is RFC 3339");
        let dev = DeviceId::new_v7();
        assert_eq!(Stamp::from_columns(Some(at), None), None);
        assert_eq!(Stamp::from_columns(None, Some(dev)), None);
        assert_eq!(
            Stamp::from_columns(Some(at), Some(dev)),
            Some(Stamp { at, dev })
        );
    }

    #[test]
    fn clamping_survives_a_server_clock_at_the_end_of_time() {
        let end = Timestamp::from_jiff(jiff::Timestamp::MAX);
        assert_eq!(clamp_mutation_at(end, end), end);
    }
}
