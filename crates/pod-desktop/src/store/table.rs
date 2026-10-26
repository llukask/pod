//! Names for the local tables, and sets of them. A [`TableSet`] is what a
//! committed write publishes and what a query subscribes to.

use std::fmt;

/// One table of the local schema (spec 5.1, 7.1 and 7.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Table {
    Podcast,
    Episode,
    Subscription,
    EpisodeState,
    Setting,
    SyncScope,
    Outbox,
    Account,
    PodcastDevice,
    DownloadWish,
    Download,
    KnownSize,
    DeviceSetting,
    RejectedMutation,
    Artwork,
}

impl Table {
    /// Every table, in schema order. A test checks this against the tables
    /// the migrations create, so a new table can't be left out.
    pub const ALL: [Table; 15] = [
        Table::Podcast,
        Table::Episode,
        Table::Subscription,
        Table::EpisodeState,
        Table::Setting,
        Table::SyncScope,
        Table::Outbox,
        Table::Account,
        Table::PodcastDevice,
        Table::DownloadWish,
        Table::Download,
        Table::KnownSize,
        Table::DeviceSetting,
        Table::RejectedMutation,
        Table::Artwork,
    ];

    /// The table's name in SQL.
    pub const fn name(self) -> &'static str {
        match self {
            Table::Podcast => "podcast",
            Table::Episode => "episode",
            Table::Subscription => "subscription",
            Table::EpisodeState => "episode_state",
            Table::Setting => "setting",
            Table::SyncScope => "sync_scope",
            Table::Outbox => "outbox",
            Table::Account => "account",
            Table::PodcastDevice => "podcast_device",
            Table::DownloadWish => "download_wish",
            Table::Download => "download",
            Table::KnownSize => "known_size",
            Table::DeviceSetting => "device_setting",
            Table::RejectedMutation => "rejected_mutation",
            Table::Artwork => "artwork",
        }
    }

    pub fn from_name(name: &str) -> Option<Table> {
        Table::ALL.into_iter().find(|table| table.name() == name)
    }

    const fn bit(self) -> Bits {
        1 << self as Bits
    }
}

/// The bits of a [`TableSet`]. Widen it when [`Table::ALL`] outgrows it;
/// the assertion below fails the build until then.
type Bits = u32;

const _: () = assert!(
    Table::ALL.len() <= Bits::BITS as usize,
    "TableSet has a bit per table"
);

/// A set of tables, one bit per [`Table`].
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct TableSet(Bits);

impl TableSet {
    pub const EMPTY: TableSet = TableSet(0);
    pub const ALL: TableSet = TableSet(TableSet::mask(Table::ALL.len()));

    /// The lowest `count` bits. Shifting the full mask right also works at
    /// `count == Bits::BITS`, where `(1 << count) - 1` would overflow.
    const fn mask(count: usize) -> Bits {
        if count == 0 {
            0
        } else {
            Bits::MAX >> (Bits::BITS as usize - count)
        }
    }

    pub fn insert(&mut self, table: Table) {
        self.0 |= table.bit();
    }

    pub fn contains(self, table: Table) -> bool {
        self.0 & table.bit() != 0
    }

    pub fn intersection(self, other: TableSet) -> TableSet {
        TableSet(self.0 & other.0)
    }

    pub fn union(self, other: TableSet) -> TableSet {
        TableSet(self.0 | other.0)
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn iter(self) -> impl Iterator<Item = Table> {
        Table::ALL
            .into_iter()
            .filter(move |table| self.contains(*table))
    }
}

impl From<Table> for TableSet {
    fn from(table: Table) -> Self {
        TableSet(table.bit())
    }
}

impl FromIterator<Table> for TableSet {
    fn from_iter<I: IntoIterator<Item = Table>>(tables: I) -> Self {
        let mut set = TableSet::EMPTY;
        for table in tables {
            set.insert(table);
        }
        set
    }
}

impl fmt::Debug for TableSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.iter().map(Table::name)).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip_and_bits_are_distinct() {
        for table in Table::ALL {
            assert_eq!(Table::from_name(table.name()), Some(table));
        }
        assert_eq!(Table::from_name("sqlite_sequence"), None);
        assert_eq!(TableSet::from_iter(Table::ALL), TableSet::ALL);
        assert_eq!(TableSet::ALL.iter().count(), Table::ALL.len());
    }

    #[test]
    fn the_all_mask_covers_any_count_up_to_the_width() {
        assert_eq!(TableSet::mask(0), 0);
        assert_eq!(TableSet::mask(15), 0x7fff);
        assert_eq!(TableSet::mask(16), 0xffff);
        assert_eq!(TableSet::mask(Bits::BITS as usize), Bits::MAX);
        const { assert!(Bits::BITS >= 32, "room for 32 tables") };
    }

    #[test]
    fn set_operations() {
        let state = TableSet::from(Table::EpisodeState);
        let downloads = TableSet::from_iter([Table::Download, Table::DownloadWish]);
        assert!(state.intersection(downloads).is_empty());
        assert_eq!(state.union(downloads).intersection(downloads), downloads);
        assert_eq!(TableSet::ALL.intersection(state), state);
        assert!(TableSet::EMPTY.is_empty());
        assert_eq!(format!("{downloads:?}"), r#"{"download_wish", "download"}"#);
    }
}
