//! The sync group and its shared collections.
//!
//! A **group** is the devices whose catalogues sync with each other. It syncs either the whole
//! **catalogue** (one device's catalogue is the start, the others begin from an empty one, and
//! every device lists every collection) or **collections only** (each device keeps its own
//! catalogue and sees only what another device allows it).
//!
//! Every shared **collection** has an owner: the device that made it. The owner says what the
//! other devices may have ([`Grant`]); each other device says what it takes ([`Take`]), never
//! more than was granted.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::clock::{Lww, Stamp};
use crate::pill::Depth;

/// The owner of a collection that lives on an Immich server starts with this (then the account).
pub const IMMICH_OWNER: &str = "immich:";

/// What a group syncs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Mode {
    /// One catalogue on every device: all collections are listed everywhere.
    Catalogue,
    /// Each device has its own catalogue and sees only the collections it was allowed.
    Collections,
}

/// What a collection's owner allows the other devices.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Grant {
    /// Not shared.
    #[default]
    None,
    /// The owner keeps the originals; others get previews they can develop on, and may not add photos.
    Preview,
    /// Others may also get the originals, and may not add photos.
    Serve,
    /// Any device may add photos; originals go both ways.
    Sync,
}

impl Grant {
    pub const ALL: [Grant; 4] = [Grant::None, Grant::Preview, Grant::Serve, Grant::Sync];
    pub fn id(self) -> &'static str {
        match self {
            Grant::None => "none",
            Grant::Preview => "preview",
            Grant::Serve => "serve",
            Grant::Sync => "sync",
        }
    }
    pub fn parse(s: &str) -> Option<Grant> {
        Grant::ALL.into_iter().find(|g| g.id() == s)
    }
    /// The most another device may take.
    pub fn most(self) -> Take {
        match self {
            Grant::None => Take::None,
            Grant::Preview => Take::Previews,
            Grant::Serve => Take::FullQuality,
            Grant::Sync => Take::FullSync,
        }
    }
}

/// What a device that doesn't own a collection takes of it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Take {
    /// Nothing is kept here.
    #[default]
    None,
    /// Previews come here; edits made on them go back.
    Previews,
    /// The originals come here; edits go back.
    FullQuality,
    /// As full quality, and photos added here go to the other devices.
    FullSync,
}

impl Take {
    pub const ALL: [Take; 4] = [Take::None, Take::Previews, Take::FullQuality, Take::FullSync];
    pub fn id(self) -> &'static str {
        match self {
            Take::None => "none",
            Take::Previews => "previews",
            Take::FullQuality => "fullQuality",
            Take::FullSync => "fullSync",
        }
    }
    pub fn parse(s: &str) -> Option<Take> {
        Take::ALL.into_iter().find(|t| t.id() == s)
    }
    /// This choice, cut to what the owner allows.
    pub fn within(self, grant: Grant) -> Take {
        self.min(grant.most())
    }
    pub fn depth(self) -> Depth {
        match self {
            Take::None => Depth::None,
            Take::Previews => Depth::Preview,
            Take::FullQuality | Take::FullSync => Depth::Full,
        }
    }
}

/// What kind of thing a shared collection is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CollectionKind {
    /// A collection of photos.
    #[default]
    Album,
    /// A collection set: holds collections, not photos.
    Set,
    /// A device's photos that are in no collection.
    Unfiled,
    /// A phone's or tablet's camera roll.
    CameraRoll,
}

/// A collection's name and place: what its owner may change.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionMeta {
    pub name: String,
    /// The shared id of the collection set it is in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
}

/// The Immich album a collection mirrors.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImmichRef {
    pub account: String,
    pub album: String,
}

/// A shared collection, as every device that knows of it holds it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Collection {
    /// The same on every device (random).
    pub id: String,
    /// The device that made it, or [`IMMICH_OWNER`] + the account.
    pub owner: String,
    #[serde(default)]
    pub kind: CollectionKind,
    pub meta: Lww<CollectionMeta>,
    pub grant: Lww<Grant>,
    /// What each other device takes (written by that device only).
    #[serde(default)]
    pub takes: BTreeMap<String, Lww<Take>>,
    /// The owner removed it. Its photos stay where they are.
    #[serde(default)]
    pub deleted: Lww<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub immich: Option<ImmichRef>,
}

/// The most devices a collection's takes are kept for (a hostile peer can't grow it without end).
pub const MAX_DEVICES: usize = 64;
/// The longest name kept, in characters.
pub const MAX_NAME: usize = 200;

impl Collection {
    pub fn new(id: &str, owner: &str, kind: CollectionKind, name: &str, parent: Option<String>, grant: Grant, s: Stamp) -> Collection {
        Collection {
            id: id.to_string(),
            owner: owner.to_string(),
            kind,
            meta: Lww::new(CollectionMeta { name: clean(name), parent }, s),
            grant: Lww::new(grant, s),
            takes: BTreeMap::new(),
            deleted: Lww::new(false, Stamp::ZERO),
            immich: None,
        }
    }

    pub fn is_immich(&self) -> bool {
        self.owner.starts_with(IMMICH_OWNER)
    }

    pub fn gone(&self) -> bool {
        self.deleted.v
    }

    /// What `device` takes, cut to what is granted. The owner has everything.
    pub fn take_of(&self, device: &str) -> Take {
        if self.owner == device {
            return Take::FullSync;
        }
        self.takes.get(device).map_or(Take::None, |t| t.v).within(self.grant.v)
    }

    /// Whether `device` is told the collection exists.
    pub fn listed_for(&self, device: &str, mode: Mode) -> bool {
        // (a device that took it is told when the owner stops sharing)
        self.owner == device || mode == Mode::Catalogue || self.grant.v != Grant::None || self.takes.contains_key(device)
    }

    /// Whether the collection's photos (their records and which ones they are) go to `device`.
    pub fn shares_with(&self, device: &str) -> bool {
        !self.gone() && self.kind != CollectionKind::Set && (self.owner == device || self.take_of(device) != Take::None)
    }

    /// Whether photos `device` adds become part of the collection everywhere.
    pub fn may_add(&self, device: &str) -> bool {
        if self.gone() || self.kind == CollectionKind::Set {
            return false;
        }
        if self.is_immich() {
            return self.take_of(device) != Take::None;
        }
        self.owner == device || self.take_of(device) == Take::FullSync
    }

    /// How much of each photo `device` should hold.
    pub fn depth_for(&self, device: &str) -> Depth {
        if self.gone() || self.kind == CollectionKind::Set {
            Depth::None
        } else if self.owner == device {
            Depth::Full
        } else {
            self.take_of(device).depth()
        }
    }

    /// Whether `device` may be sent the photos at `depth` by whoever holds them.
    pub fn may_fetch(&self, device: &str, depth: Depth) -> bool {
        depth != Depth::None && self.depth_for(device) >= depth
    }

    /// Takes over whatever `other` (the same collection on another device) wrote later. Says
    /// whether anything changed.
    pub fn merge(&mut self, other: &Collection) -> bool {
        if other.id != self.id {
            return false;
        }
        let mut changed = false;
        let mut meta = other.meta.clone();
        meta.v.name = clean(&meta.v.name);
        changed |= self.meta.merge(&meta);
        changed |= self.grant.merge(&other.grant);
        changed |= self.deleted.merge(&other.deleted);
        for (device, take) in other.takes.iter().take(MAX_DEVICES) {
            let room = self.takes.len() < MAX_DEVICES && device.len() <= 64;
            match self.takes.get_mut(device) {
                Some(mine) => changed |= mine.merge(take),
                None if room => {
                    self.takes.insert(device.clone(), take.clone());
                    changed = true;
                }
                None => {}
            }
        }
        let ok = |s: &str| s.len() <= 128 && !s.chars().any(char::is_control);
        if self.immich.is_none()
            && let Some(r) = other.immich.as_ref().filter(|r| ok(&r.account) && ok(&r.album) && !r.account.is_empty())
        {
            self.immich = Some(r.clone());
            changed = true;
        }
        changed
    }
}

/// A name without control characters, cut to [`MAX_NAME`].
pub fn clean(name: &str) -> String {
    name.chars().filter(|c| !c.is_control()).take(MAX_NAME).collect::<String>().trim().to_string()
}

/// A device of the group.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub name: String,
    /// It left the group (or was removed from it).
    #[serde(default)]
    pub left: bool,
}

/// The devices that sync, and what they sync.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    /// The same on every device (random).
    pub id: String,
    pub mode: Mode,
    /// Catalogue mode: the device whose catalogue the group started from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keeper: Option<String>,
    #[serde(default)]
    pub devices: BTreeMap<String, Lww<DeviceInfo>>,
}

impl Group {
    pub fn has(&self, device: &str) -> bool {
        self.devices.get(device).is_some_and(|d| !d.v.left)
    }

    /// The devices in the group now.
    pub fn members(&self) -> impl Iterator<Item = (&String, &DeviceInfo)> {
        self.devices.iter().filter(|(_, d)| !d.v.left).map(|(id, d)| (id, &d.v))
    }

    pub fn name_of(&self, device: &str) -> Option<&str> {
        self.devices.get(device).map(|d| d.v.name.as_str())
    }

    /// Whether another device of the group already has `name` (names are compared without case).
    pub fn name_taken(&self, name: &str, except: &str) -> bool {
        let n = name.trim().to_lowercase();
        self.members().any(|(id, d)| id != except && d.name.trim().to_lowercase() == n)
    }

    /// Takes over whatever `other` (the same group on another device) wrote later.
    pub fn merge(&mut self, other: &Group) -> bool {
        if other.id != self.id {
            return false;
        }
        let mut changed = false;
        for (id, info) in other.devices.iter().take(MAX_DEVICES) {
            let mut info = info.clone();
            info.v.name = clean(&info.v.name);
            let room = self.devices.len() < MAX_DEVICES && id.len() <= 64;
            match self.devices.get_mut(id) {
                Some(mine) => changed |= mine.merge(&info),
                None if room => {
                    self.devices.insert(id.clone(), info);
                    changed = true;
                }
                None => {}
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coll(grant: Grant) -> Collection {
        Collection::new("c", "owner", CollectionKind::Album, "Trip", None, grant, Stamp { t: 1, c: 0, d: 0 })
    }

    #[test]
    fn a_device_never_takes_more_than_the_owner_allows() {
        let mut c = coll(Grant::Preview);
        c.takes.insert("tablet".into(), Lww::new(Take::FullSync, Stamp { t: 2, c: 0, d: 0 }));
        assert_eq!(c.take_of("tablet"), Take::Previews);
        assert!(c.shares_with("tablet") && !c.may_add("tablet"));
        assert!(c.may_fetch("tablet", Depth::Preview) && !c.may_fetch("tablet", Depth::Full));
        // the owner widens it: the device's own choice now counts in full
        c.grant = Lww::new(Grant::Sync, Stamp { t: 3, c: 0, d: 0 });
        assert_eq!(c.take_of("tablet"), Take::FullSync);
        assert!(c.may_add("tablet") && c.may_fetch("tablet", Depth::Full));
        // and nothing at all once the owner stops sharing
        c.grant = Lww::new(Grant::None, Stamp { t: 4, c: 0, d: 0 });
        assert!(!c.shares_with("tablet") && c.shares_with("owner"));
        assert!(!c.may_fetch("stranger", Depth::Preview));
    }

    #[test]
    fn serve_gives_originals_but_only_the_owner_adds() {
        let mut c = coll(Grant::Serve);
        c.takes.insert("laptop".into(), Lww::new(Take::FullSync, Stamp { t: 2, c: 0, d: 0 }));
        assert_eq!(c.take_of("laptop"), Take::FullQuality);
        assert!(c.may_fetch("laptop", Depth::Full) && !c.may_add("laptop") && c.may_add("owner"));
    }

    #[test]
    fn collections_are_listed_by_mode() {
        let c = coll(Grant::None);
        assert!(c.listed_for("tablet", Mode::Catalogue), "a shared catalogue lists every collection");
        assert!(!c.listed_for("tablet", Mode::Collections), "collections-only shows what was allowed");
        assert!(c.listed_for("owner", Mode::Collections));
        assert!(coll(Grant::Preview).listed_for("tablet", Mode::Collections));
    }

    #[test]
    fn merging_two_copies_gives_the_same_collection_both_ways() {
        let mut a = coll(Grant::Preview);
        let mut b = a.clone();
        a.meta = Lww::new(CollectionMeta { name: "Trip 2026".into(), parent: None }, Stamp { t: 5, c: 0, d: 1 });
        b.takes.insert("tablet".into(), Lww::new(Take::Previews, Stamp { t: 6, c: 0, d: 2 }));
        b.deleted = Lww::new(true, Stamp { t: 7, c: 0, d: 2 });
        let (a0, b0) = (a.clone(), b.clone());
        assert!(a.merge(&b0) && b.merge(&a0));
        assert_eq!(a, b);
        assert!(!a.merge(&b), "merging again changes nothing");
        assert!(!a.shares_with("tablet"), "a removed collection shares nothing");
    }

    #[test]
    fn device_names_are_unique_without_case() {
        let mut g = Group { id: "g".into(), mode: Mode::Collections, keeper: None, devices: BTreeMap::new() };
        g.devices.insert("a".into(), Lww::new(DeviceInfo { name: "Studio".into(), left: false }, Stamp::ZERO));
        g.devices.insert("b".into(), Lww::new(DeviceInfo { name: "Tablet".into(), left: true }, Stamp::ZERO));
        assert!(g.name_taken(" studio ", "c") && !g.name_taken("studio", "a"));
        assert!(!g.name_taken("Tablet", "c"), "a device that left frees its name");
    }

    #[test]
    fn hostile_collections_are_bounded() {
        let mut c = coll(Grant::Sync);
        let mut other = c.clone();
        for i in 0..1000 {
            other.takes.insert(format!("d{i}"), Lww::new(Take::FullSync, Stamp { t: 9, c: 0, d: 0 }));
        }
        other.meta = Lww::new(CollectionMeta { name: "x\u{0}".repeat(5000), parent: None }, Stamp { t: 9, c: 0, d: 0 });
        other.immich = Some(ImmichRef { account: "u".repeat(10_000), album: "a\u{0}".into() });
        c.merge(&other);
        assert!(c.takes.len() <= MAX_DEVICES);
        assert!(c.immich.is_none(), "an oversized Immich reference is dropped");
        assert!(c.meta.v.name.chars().count() <= MAX_NAME && !c.meta.v.name.contains('\u{0}'));
    }
}
