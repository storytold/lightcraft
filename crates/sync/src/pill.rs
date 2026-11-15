//! The pill on a photo or a collection: two letters (where the original lives, how much of it is
//! here) and an arrow (which way changes are still to travel).

use serde::{Deserialize, Serialize};

use crate::model::{Collection, Grant};

/// Where the original lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Location {
    /// This device owns the original.
    Local,
    /// Another device owns the original.
    Remote,
    /// An Immich server holds the original.
    Immich,
    /// Every device holds the originals and may add to them.
    Equal,
}

impl Location {
    pub fn letter(self) -> char {
        match self {
            Location::Local => 'L',
            Location::Remote => 'R',
            Location::Immich => 'I',
            Location::Equal => 'E',
        }
    }
}

/// How much of a photo is synced.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Depth {
    /// Neither the image nor a preview.
    #[default]
    None,
    /// The owning device keeps the original; a preview (good for developing) travels.
    Preview,
    /// The original travels.
    Full,
}

impl Depth {
    pub fn letter(self) -> char {
        match self {
            Depth::None => 'N',
            Depth::Preview => 'P',
            Depth::Full => 'F',
        }
    }
}

/// Which way changes are still to travel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Status {
    /// Nothing waits on either side.
    #[default]
    Synced,
    /// This device has changes the others don't have yet.
    LocalAhead,
    /// Another device has something this one hasn't fetched yet.
    RemoteAhead,
}

impl Status {
    /// `↑` out, `↓` in, nothing when in step.
    pub fn arrow(self) -> Option<char> {
        match self {
            Status::Synced => None,
            Status::LocalAhead => Some('↑'),
            Status::RemoteAhead => Some('↓'),
        }
    }
}

/// What is shown on a photo or a collection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pill {
    pub location: Location,
    pub depth: Depth,
    pub status: Status,
    /// Travelling right now (the arrow pulses).
    pub syncing: bool,
}

impl Pill {
    /// The two letters, e.g. `RF`.
    pub fn letters(&self) -> String {
        [self.location.letter(), self.depth.letter()].iter().collect()
    }
    /// The letters and the arrow, e.g. `RF ↓`.
    pub fn text(&self) -> String {
        match self.status.arrow() {
            Some(a) => format!("{} {a}", self.letters()),
            None => self.letters(),
        }
    }
}

/// The letters of collection `c` on device `me`.
pub fn of_collection(c: &Collection, me: &str) -> (Location, Depth) {
    if c.is_immich() {
        return (Location::Immich, c.take_of(me).depth());
    }
    if c.owner == me {
        return match c.grant.v {
            Grant::None => (Location::Local, Depth::None),
            Grant::Preview => (Location::Local, Depth::Preview),
            Grant::Serve => (Location::Local, Depth::Full),
            Grant::Sync => (Location::Equal, Depth::Full),
        };
    }
    let take = c.take_of(me);
    let location = if take == crate::model::Take::FullSync { Location::Equal } else { Location::Remote };
    (location, take.depth())
}

/// The letters of a photo in several collections: those of the collection that syncs the most
/// of it (the first such, so the answer doesn't flicker).
pub fn of_photo<'a>(collections: impl Iterator<Item = &'a Collection>, me: &str) -> Option<(Location, Depth)> {
    let mut best: Option<(Location, Depth)> = None;
    for c in collections.filter(|c| !c.gone()) {
        let p = of_collection(c, me);
        if best.is_none_or(|b| p.1 > b.1) {
            best = Some(p);
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::{Lww, Stamp};
    use crate::model::{CollectionKind, Take};

    fn coll(owner: &str, grant: Grant) -> Collection {
        Collection::new("c", owner, CollectionKind::Album, "Trip", None, grant, Stamp::ZERO)
    }

    #[test]
    fn letters_follow_who_owns_and_what_is_taken() {
        assert_eq!(of_collection(&coll("me", Grant::None), "me"), (Location::Local, Depth::None));
        assert_eq!(of_collection(&coll("me", Grant::Preview), "me"), (Location::Local, Depth::Preview));
        assert_eq!(of_collection(&coll("me", Grant::Serve), "me"), (Location::Local, Depth::Full));
        assert_eq!(of_collection(&coll("me", Grant::Sync), "me"), (Location::Equal, Depth::Full));
        let mut c = coll("other", Grant::Sync);
        assert_eq!(of_collection(&c, "me"), (Location::Remote, Depth::None));
        c.takes.insert("me".into(), Lww::new(Take::Previews, Stamp::ZERO));
        assert_eq!(of_collection(&c, "me"), (Location::Remote, Depth::Preview));
        c.takes.insert("me".into(), Lww::new(Take::FullQuality, Stamp::ZERO));
        assert_eq!(of_collection(&c, "me"), (Location::Remote, Depth::Full));
        c.takes.insert("me".into(), Lww::new(Take::FullSync, Stamp::ZERO));
        assert_eq!(of_collection(&c, "me"), (Location::Equal, Depth::Full));
        let mut i = coll("immich:home", Grant::Sync);
        i.takes.insert("me".into(), Lww::new(Take::Previews, Stamp::ZERO));
        assert_eq!(of_collection(&i, "me"), (Location::Immich, Depth::Preview));
    }

    #[test]
    fn a_pill_reads_as_letters_and_an_arrow() {
        let p = Pill { location: Location::Remote, depth: Depth::Full, status: Status::RemoteAhead, syncing: true };
        assert_eq!(p.text(), "RF ↓");
        assert_eq!(Pill { status: Status::Synced, ..p }.text(), "RF");
        assert_eq!(Pill { status: Status::LocalAhead, location: Location::Local, depth: Depth::Preview, syncing: false }.text(), "LP ↑");
    }

    #[test]
    fn a_photo_shows_the_collection_that_syncs_most_of_it() {
        let a = coll("me", Grant::Preview);
        let b = coll("me", Grant::Sync);
        assert_eq!(of_photo([&a, &b].into_iter(), "me"), Some((Location::Equal, Depth::Full)));
        assert_eq!(of_photo(std::iter::empty(), "me"), None);
    }
}
