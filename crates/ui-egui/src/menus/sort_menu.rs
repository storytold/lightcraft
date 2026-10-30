//! What the Sort menus offer for the view being shown. The bottom bar's popup and View ▸ Sort (the
//! native menu bar, the in-window one and the control channel) both ask this one model, so they can
//! never disagree about what is on, what is checked and why something is off.
//!
//! The grid decides what a sort means: Recently Added is always newest import first, a shuffle has
//! no date headers, and Recently Deleted draws none whatever the grouping (`panels::grid`).

use lightcraft_catalog::{GroupBy, Sort, SortKey};
use lightcraft_engine::{LibrarySource, Session};

/// Why part of the menu is off, as a dim line in the menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortNote {
    /// Shown above the keys: they have no say in this view.
    RecentlyAdded,
    /// Shown above the date groups: this view has no headers.
    RecentlyDeleted,
}

impl SortNote {
    /// The message key (and the English text).
    pub fn text(self) -> &'static str {
        match self {
            SortNote::RecentlyAdded => "Recently Added always shows the newest imports first.",
            SortNote::RecentlyDeleted => "Recently Deleted shows no date groups.",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SortMenu {
    sort: Sort,
    source: LibrarySource,
    has_previous: bool,
}

impl SortMenu {
    pub fn of(session: &Session) -> Self {
        SortMenu { sort: session.sort, source: session.source, has_previous: session.has_previous_shuffle() }
    }

    /// Recently Added orders by import date whatever the sort says.
    fn fixed_order(&self) -> bool {
        self.source == LibrarySource::RecentlyAdded
    }

    /// The sort keys, the direction and Reshuffle all have a say only when the sort orders the view.
    pub fn keys_enabled(&self) -> bool {
        !self.fixed_order()
    }

    /// A key is checked only while it is the order shown.
    pub fn key_checked(&self, key: SortKey) -> bool {
        self.keys_enabled() && self.sort.key == key
    }

    /// A shuffle has no direction worth choosing.
    pub fn direction_enabled(&self) -> bool {
        self.keys_enabled() && self.sort.key != SortKey::Random
    }

    /// `None` where no direction is in force (a shuffle, Recently Added), so no row claims one.
    pub fn direction_checked(&self, ascending: bool) -> Option<bool> {
        self.direction_enabled().then_some(self.sort.ascending == ascending)
    }

    /// Reshuffle means something only under Random (and where the sort is used at all).
    pub fn reshuffle_enabled(&self) -> bool {
        self.keys_enabled() && self.sort.key == SortKey::Random
    }

    /// Previous Shuffle: only where Reshuffle is, and once a reshuffle has replaced an order to go back to.
    pub fn previous_enabled(&self) -> bool {
        self.reshuffle_enabled() && self.has_previous
    }

    /// Date headers: only the date keys draw them (not a shuffle, File Name, Rating or File Size), and not
    /// Recently Deleted. Recently Added orders by import date, so it groups by import date whatever the key.
    pub fn groups_enabled(&self) -> bool {
        match self.source {
            LibrarySource::RecentlyDeleted => false,
            LibrarySource::RecentlyAdded => true,
            _ => matches!(self.sort.key, SortKey::CaptureDate | SortKey::ImportDate | SortKey::EditDate),
        }
    }

    /// The stored choice stays while the groups are off, and is not shown as in force.
    pub fn group_checked(&self, group: GroupBy) -> bool {
        self.groups_enabled() && self.sort.group == group
    }

    pub fn keys_note(&self) -> Option<SortNote> {
        self.fixed_order().then_some(SortNote::RecentlyAdded)
    }

    pub fn groups_note(&self) -> Option<SortNote> {
        (self.source == LibrarySource::RecentlyDeleted).then_some(SortNote::RecentlyDeleted)
    }
}
