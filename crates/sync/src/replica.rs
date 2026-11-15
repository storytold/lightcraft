//! One catalogue's copy of everything a sync group shares, and what is still to tell each device.
//!
//! Every shared thing — the group, a collection, a photo being in a collection, a photo's
//! record — is an item with a sequence number that goes up each time the item changes here,
//! whether by an edit on this device or by a merge of what another device sent. For each other
//! device a cursor says how far it has confirmed, so an exchange sends only what changed since,
//! and only what that device is allowed to see ([`Replica::tells`]). When a device starts to
//! see more (it takes a collection, a photo joins one), those items are queued to send again.
//!
//! Records merge field by field, each field keeping its later write ([`crate::clock::Lww`]), so
//! any number of devices settle on the same state in any order of exchanges, and a change made
//! on one field never undoes a change made to another.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::clock::{Clock, Lww, Stamp};
use crate::model::{Collection, CollectionKind, DeviceInfo, Grant, Group, Mode, Take, clean};
use crate::pill::Depth;

/// Names one shared item.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Ref {
    Group,
    Coll(String),
    /// (collection, photo key)
    Member(String, String),
    Photo(String),
}

/// One shared item as it travels.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Item {
    Group(Group),
    Coll(Collection),
    Member {
        coll: String,
        key: String,
        on: Lww<bool>,
    },
    Photo {
        key: String,
        /// What a device needs to list a photo it doesn't have (name, size, capture facts).
        info: Value,
        /// The record, field by field.
        fields: BTreeMap<String, Lww<Value>>,
        /// How much of the photo the sender holds.
        #[serde(default)]
        here: Depth,
    },
}

/// What is known of one field of a photo's record: a fingerprint of its value and when it was written.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldState {
    pub d: u64,
    pub s: Stamp,
}

/// A photo another device told of that isn't in this catalogue (yet).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Ghost {
    pub info: Value,
    pub values: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PhotoEntry {
    pub fields: BTreeMap<String, FieldState>,
    pub seq: u64,
    /// The device this state last came from (it is not sent back there).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    /// How much of the photo this device holds.
    #[serde(default)]
    pub have: Depth,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ghost: Option<Box<Ghost>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberEntry {
    pub on: Lww<bool>,
    pub seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollEntry {
    pub c: Collection,
    pub seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
}

/// How far another device has confirmed, and what to send it again.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    pub acked: u64,
    #[serde(default)]
    pub resend: BTreeSet<Ref>,
}

/// What to send a device now ([`Replica::outgoing`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outgoing {
    pub refs: Vec<Ref>,
    /// Confirming this number confirms everything in `refs` that wasn't queued to send again.
    pub upto: u64,
    /// The items of `refs` that were queued to send again.
    pub resent: Vec<Ref>,
    /// There is more than fitted.
    pub more: bool,
}

/// Why a change to the group or a collection was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refused {
    NoGroup,
    AlreadyInGroup,
    NameTaken(String),
    NoCollection,
    NotOwner,
    Owner,
    NotAllowed,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refused::NoGroup => f.write_str("this catalogue isn't syncing with another device"),
            Refused::AlreadyInGroup => f.write_str("this catalogue already syncs; stop that sync first"),
            Refused::NameTaken(n) => write!(f, "another device is already called `{n}`; give this one another name"),
            Refused::NoCollection => f.write_str("no such shared collection"),
            Refused::NotOwner => f.write_str("only the device that made the collection can change that"),
            Refused::Owner => f.write_str("this device made the collection; set what it allows instead"),
            Refused::NotAllowed => f.write_str("the collection's owner doesn't allow that"),
        }
    }
}

/// The largest key, field name and collection id taken from another device.
const MAX_ID: usize = 128;
/// The most fields a photo's record holds.
const MAX_FIELDS: usize = 32;

fn ok_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_ID && !s.chars().any(char::is_control)
}

/// One catalogue's copy of the shared state.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Replica {
    /// This device.
    pub device: String,
    pub group: Option<Group>,
    #[serde(default)]
    group_seq: u64,
    #[serde(default)]
    group_via: Option<String>,
    /// Paired devices asked to join the group that haven't yet.
    #[serde(default)]
    pub invited: BTreeSet<String>,
    #[serde(default)]
    pub collections: BTreeMap<String, CollEntry>,
    /// The catalogue's own id of each collection that is in it.
    #[serde(default)]
    pub albums: BTreeMap<String, u64>,
    /// Collections of the catalogue the person was already asked about (and didn't share).
    #[serde(default)]
    pub asked: BTreeSet<u64>,
    #[serde(default)]
    pub members: BTreeMap<String, BTreeMap<String, MemberEntry>>,
    #[serde(default)]
    pub photos: HashMap<String, PhotoEntry>,
    #[serde(default)]
    pub cursors: BTreeMap<String, Cursor>,
    /// Immich accounts (user ids) whose connection this device keeps to itself: their
    /// collections are never told to another device. Local state; it doesn't travel.
    #[serde(default)]
    pub private_immich: BTreeSet<String>,
    /// The folder this device's camera roll is watched in (`""`: the platform says where).
    /// Local state; it doesn't travel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera_roll: Option<String>,
    #[serde(default)]
    pub clock: Clock,
    #[serde(default)]
    pub seq: u64,
    #[serde(skip)]
    by_seq: BTreeMap<u64, Ref>,
}

impl Replica {
    pub fn new(device: &str) -> Replica {
        Replica { device: device.to_string(), ..Default::default() }
    }

    /// After loading: the index by sequence number (not saved).
    pub fn rebuild(&mut self) {
        self.by_seq.clear();
        if self.group.is_some() && self.group_seq > 0 {
            self.by_seq.insert(self.group_seq, Ref::Group);
        }
        for (id, e) in &self.collections {
            self.by_seq.insert(e.seq, Ref::Coll(id.clone()));
        }
        for (coll, keys) in &self.members {
            for (key, e) in keys {
                self.by_seq.insert(e.seq, Ref::Member(coll.clone(), key.clone()));
            }
        }
        for (key, e) in &self.photos {
            self.by_seq.insert(e.seq, Ref::Photo(key.clone()));
        }
        self.seq = self.seq.max(self.by_seq.keys().next_back().copied().unwrap_or(0));
    }

    fn next_seq(&mut self, old: u64, r: Ref) -> u64 {
        self.by_seq.remove(&old);
        self.seq = self.seq.saturating_add(1);
        self.by_seq.insert(self.seq, r);
        self.seq
    }

    /// Whom a merged item is marked as coming from, so it isn't sent straight back there: `from`,
    /// unless the item (numbered `old_seq`, last from `old_via`) still held a change `from` hasn't
    /// confirmed. Then the whole item goes back, and `from` keeps whatever of it is later.
    fn via_for(&self, old_seq: u64, old_via: Option<&str>, from: Option<&str>) -> Option<String> {
        let from = from?;
        let owed = old_seq > self.cursors.get(from).map_or(0, |c| c.acked) && old_via != Some(from);
        (!owed).then(|| from.to_string())
    }

    pub fn mode(&self) -> Option<Mode> {
        self.group.as_ref().map(|g| g.mode)
    }

    /// The other devices of the group.
    pub fn peers(&self) -> Vec<String> {
        self.group.iter().flat_map(|g| g.members()).map(|(id, _)| id.clone()).filter(|id| *id != self.device).collect()
    }

    pub fn in_group_with(&self, device: &str) -> bool {
        self.group.as_ref().is_some_and(|g| g.has(&self.device) && g.has(device))
    }

    fn stamp(&mut self, now_ms: u64) -> Stamp {
        let device = self.device.clone();
        self.clock.tick(now_ms, &device)
    }

    // ── the group ──────────────────────────────────────────────────────────────────────────

    /// Starts a group with this device in it.
    pub fn start_group(&mut self, id: &str, mode: Mode, keeper: Option<String>, my_name: &str, now_ms: u64) -> Result<(), Refused> {
        if self.group.as_ref().is_some_and(|g| g.has(&self.device)) {
            return Err(Refused::AlreadyInGroup);
        }
        let s = self.stamp(now_ms);
        let mut devices = BTreeMap::new();
        devices.insert(self.device.clone(), Lww::new(DeviceInfo { name: clean(my_name), left: false }, s));
        self.group = Some(Group { id: id.to_string(), mode, keeper, devices });
        self.group_via = None;
        self.group_seq = self.next_seq(self.group_seq, Ref::Group);
        Ok(())
    }

    /// Joins the group another device invited this one to.
    pub fn join_group(&mut self, group: &Group, my_name: &str, now_ms: u64) -> Result<(), Refused> {
        if self.group.as_ref().is_some_and(|g| g.has(&self.device) && g.id != group.id) {
            return Err(Refused::AlreadyInGroup);
        }
        if group.name_taken(my_name, &self.device) {
            return Err(Refused::NameTaken(clean(my_name)));
        }
        let s = self.stamp(now_ms);
        let mut g = Group { id: group.id.clone(), mode: group.mode, keeper: group.keeper.clone(), devices: BTreeMap::new() };
        g.merge(group);
        g.devices.insert(self.device.clone(), Lww::new(DeviceInfo { name: clean(my_name), left: false }, s));
        self.group = Some(g);
        self.group_via = None;
        self.group_seq = self.next_seq(self.group_seq, Ref::Group);
        Ok(())
    }

    /// This device's name in the group (unique in it).
    pub fn rename_me(&mut self, name: &str, now_ms: u64) -> Result<(), Refused> {
        let me = self.device.clone();
        let Some(g) = &self.group else { return Ok(()) };
        if !g.has(&me) {
            return Ok(());
        }
        if g.name_taken(name, &me) {
            return Err(Refused::NameTaken(clean(name)));
        }
        let s = self.stamp(now_ms);
        if let Some(g) = &mut self.group {
            g.devices.insert(me, Lww::new(DeviceInfo { name: clean(name), left: false }, s));
        }
        self.group_via = None;
        self.group_seq = self.next_seq(self.group_seq, Ref::Group);
        Ok(())
    }

    /// Marks `device` (this one, or one it removes) as having left; the others learn it.
    pub fn leave(&mut self, device: &str, now_ms: u64) -> Result<(), Refused> {
        let s = self.stamp(now_ms);
        let g = self.group.as_mut().ok_or(Refused::NoGroup)?;
        let name = g.name_of(device).unwrap_or_default().to_string();
        g.devices.insert(device.to_string(), Lww::new(DeviceInfo { name, left: true }, s));
        self.group_via = None;
        self.group_seq = self.next_seq(self.group_seq, Ref::Group);
        self.invited.remove(device);
        Ok(())
    }

    /// What another device says the group is. Only the same group merges.
    pub fn merge_group(&mut self, g: &Group, from: &str) -> bool {
        let Some(mine) = &mut self.group else { return false };
        for d in g.devices.values() {
            self.clock.observe(d.s);
        }
        let before: Vec<String> = mine.members().map(|(id, _)| id.clone()).collect();
        if !mine.merge(g) {
            return false;
        }
        let joined: Vec<String> = mine.members().map(|(id, _)| id.clone()).filter(|id| !before.contains(id)).collect();
        self.group_via = self.via_for(self.group_seq, self.group_via.as_deref(), Some(from));
        self.group_seq = self.next_seq(self.group_seq, Ref::Group);
        for d in joined {
            self.invited.remove(&d);
            // a device that just joined is told every collection it may know of
            let ids: Vec<String> = self.collections.keys().cloned().collect();
            let c = self.cursors.entry(d).or_default();
            c.resend.extend(ids.into_iter().map(Ref::Coll));
        }
        true
    }

    // ── collections ────────────────────────────────────────────────────────────────────────

    pub fn collection(&self, id: &str) -> Option<&Collection> {
        self.collections.get(id).map(|e| &e.c)
    }

    /// The shared id of the catalogue's collection `album`.
    pub fn collection_of_album(&self, album: u64) -> Option<&str> {
        self.albums.iter().find(|(_, a)| **a == album).map(|(id, _)| id.as_str())
    }

    /// Queues for each device that sees more of collection `id` than it did in `before` what it
    /// now needs: which photos are in it, and their records.
    fn reshare(&mut self, id: &str, before: Option<&Collection>) {
        let Some(now) = self.collections.get(id).map(|e| e.c.clone()) else { return };
        let devices: Vec<String> = self.peers();
        for d in devices {
            if now.shares_with(&d) && !before.is_some_and(|b| b.shares_with(&d)) {
                let keys: Vec<String> = self.members.get(id).map(|m| m.keys().cloned().collect()).unwrap_or_default();
                let c = self.cursors.entry(d).or_default();
                for k in keys {
                    c.resend.insert(Ref::Member(id.to_string(), k.clone()));
                    c.resend.insert(Ref::Photo(k));
                }
            }
        }
    }

    fn touch_collection(&mut self, id: &str, via: Option<&str>, before: Option<&Collection>) {
        let (old, old_via) = self.collections.get(id).map_or((0, None), |e| (e.seq, e.via.clone()));
        let via = self.via_for(old, old_via.as_deref(), via);
        let seq = self.next_seq(old, Ref::Coll(id.to_string()));
        if let Some(e) = self.collections.get_mut(id) {
            e.seq = seq;
            e.via = via;
        }
        self.reshare(id, before);
    }

    /// A collection made on this device.
    pub fn add_collection(
        &mut self,
        id: &str,
        kind: CollectionKind,
        name: &str,
        parent: Option<String>,
        grant: Grant,
        album: Option<u64>,
        now_ms: u64,
    ) {
        let s = self.stamp(now_ms);
        let c = Collection::new(id, &self.device, kind, name, parent, grant, s);
        self.collections.insert(id.to_string(), CollEntry { c, seq: 0, via: None });
        if let Some(a) = album {
            self.albums.insert(id.to_string(), a);
        }
        self.touch_collection(id, None, None);
    }

    fn edit_collection(
        &mut self,
        id: &str,
        now_ms: u64,
        f: impl FnOnce(&mut Collection, Stamp, &str) -> Result<bool, Refused>,
    ) -> Result<bool, Refused> {
        let before = self.collection(id).cloned().ok_or(Refused::NoCollection)?;
        let s = self.stamp(now_ms);
        let me = self.device.clone();
        let e = self.collections.get_mut(id).ok_or(Refused::NoCollection)?;
        let changed = f(&mut e.c, s, &me)?;
        if changed {
            self.touch_collection(id, None, Some(&before));
        }
        Ok(changed)
    }

    /// An Immich album (or the account's set holding them, `album: None`) as this device sees
    /// it on the server. Every device with the account writes the same id, so their copies merge;
    /// the name follows the server. Grant is fixed at Sync: every device that takes it may add.
    #[allow(clippy::too_many_arguments)]
    pub fn immich_collection(&mut self, id: &str, account: &str, album: Option<&str>, name: &str, parent: Option<String>, now_ms: u64) -> bool {
        if !ok_id(id) || !ok_id(account) {
            return false;
        }
        let meta = crate::model::CollectionMeta { name: clean(name), parent };
        match self.collections.get(id) {
            Some(e) if e.c.meta.v == meta => false,
            Some(_) => {
                let before = self.collection(id).cloned();
                let s = self.stamp(now_ms);
                if let Some(e) = self.collections.get_mut(id) {
                    e.c.meta = Lww::new(meta, s);
                }
                self.touch_collection(id, None, before.as_ref());
                true
            }
            None => {
                let s = self.stamp(now_ms);
                let kind = if album.is_some() { CollectionKind::Album } else { CollectionKind::Set };
                let owner = format!("{}{account}", crate::model::IMMICH_OWNER);
                let mut c = Collection::new(id, &owner, kind, &meta.name, meta.parent, Grant::Sync, s);
                c.immich = Some(crate::model::ImmichRef { account: account.to_string(), album: album.unwrap_or_default().to_string() });
                self.collections.insert(id.to_string(), CollEntry { c, seq: 0, via: None });
                self.touch_collection(id, None, None);
                true
            }
        }
    }

    /// An Immich album was removed on the server (or is back): any device with the account says so.
    pub fn set_immich_gone(&mut self, id: &str, gone: bool, now_ms: u64) -> Result<bool, Refused> {
        self.edit_collection(id, now_ms, |c, s, _| {
            if !c.is_immich() {
                return Err(Refused::NotOwner);
            }
            let changed = c.deleted.v != gone;
            if changed {
                c.deleted = Lww::new(gone, s);
            }
            Ok(changed)
        })
    }

    /// Whether collection `c` is an Immich one this device keeps to itself.
    pub fn is_private(&self, c: &Collection) -> bool {
        c.is_immich() && c.immich.as_ref().is_some_and(|r| self.private_immich.contains(&r.account))
    }

    /// The owner says what the other devices may have.
    pub fn set_grant(&mut self, id: &str, grant: Grant, now_ms: u64) -> Result<bool, Refused> {
        self.edit_collection(id, now_ms, |c, s, me| {
            if c.owner != me {
                return Err(Refused::NotOwner);
            }
            let changed = c.grant.v != grant;
            if changed {
                c.grant = Lww::new(grant, s);
            }
            Ok(changed)
        })
    }

    /// A device that doesn't own the collection says what it takes (never more than granted).
    pub fn set_take(&mut self, id: &str, take: Take, now_ms: u64) -> Result<bool, Refused> {
        self.edit_collection(id, now_ms, |c, s, me| {
            if c.owner == me {
                return Err(Refused::Owner);
            }
            if take > c.grant.v.most() || (c.is_immich() && take > Take::FullQuality) {
                return Err(Refused::NotAllowed);
            }
            let changed = c.takes.get(me).is_none_or(|t| t.v != take);
            if changed {
                c.takes.insert(me.to_string(), Lww::new(take, s));
            }
            Ok(changed)
        })
    }

    /// The owner renames or moves its collection.
    pub fn set_collection_meta(&mut self, id: &str, name: &str, parent: Option<String>, now_ms: u64) -> Result<bool, Refused> {
        self.edit_collection(id, now_ms, |c, s, me| {
            if c.owner != me {
                return Err(Refused::NotOwner);
            }
            let meta = crate::model::CollectionMeta { name: clean(name), parent };
            let changed = c.meta.v != meta;
            if changed {
                c.meta = Lww::new(meta, s);
            }
            Ok(changed)
        })
    }

    /// The owner removes its collection (the photos stay everywhere), or puts it back.
    pub fn set_collection_deleted(&mut self, id: &str, deleted: bool, now_ms: u64) -> Result<bool, Refused> {
        self.edit_collection(id, now_ms, |c, s, me| {
            if c.owner != me {
                return Err(Refused::NotOwner);
            }
            let changed = c.deleted.v != deleted;
            if changed {
                c.deleted = Lww::new(deleted, s);
            }
            Ok(changed)
        })
    }

    /// What another device says a collection is.
    pub fn merge_collection(&mut self, c: &Collection, from: &str) -> bool {
        if !ok_id(&c.id) || !ok_id(&c.owner) {
            return false;
        }
        self.clock.observe(c.meta.s.max(c.grant.s).max(c.deleted.s));
        for t in c.takes.values() {
            self.clock.observe(t.s);
        }
        let before = self.collection(&c.id).cloned();
        let changed = match self.collections.get_mut(&c.id) {
            // (the owner and the kind never change)
            Some(e) => e.c.merge(c),
            None => {
                let mut fresh = Collection::new(&c.id, &c.owner, c.kind, "", None, Grant::None, Stamp::ZERO);
                fresh.merge(c);
                self.collections.insert(c.id.clone(), CollEntry { c: fresh, seq: 0, via: None });
                true
            }
        };
        if changed {
            self.touch_collection(&c.id, Some(from), before.as_ref());
        }
        changed
    }

    // ── which photos are in a collection ───────────────────────────────────────────────────

    /// The shared collections photo `key` is in.
    pub fn collections_of<'a>(&'a self, key: &'a str) -> impl Iterator<Item = &'a Collection> + 'a {
        self.collections.iter().filter(move |(id, _)| self.members.get(*id).and_then(|m| m.get(key)).is_some_and(|e| e.on.v)).map(|(_, e)| &e.c)
    }

    /// The photos in collection `coll`.
    pub fn members_of<'a>(&'a self, coll: &str) -> impl Iterator<Item = &'a String> + 'a {
        self.members.get(coll).into_iter().flat_map(|m| m.iter()).filter(|(_, e)| e.on.v).map(|(k, _)| k)
    }

    fn touch_member(&mut self, coll: &str, key: &str, via: Option<&str>) {
        let old = self.members.get(coll).and_then(|m| m.get(key)).map_or(0, |e| e.seq);
        let seq = self.next_seq(old, Ref::Member(coll.to_string(), key.to_string()));
        let mut on = false;
        if let Some(e) = self.members.get_mut(coll).and_then(|m| m.get_mut(key)) {
            e.seq = seq;
            e.via = via.map(str::to_string);
            on = e.on.v;
        }
        if on {
            // whoever sees the collection now needs the photo's record too
            let Some(c) = self.collection(coll).cloned() else { return };
            for d in self.peers() {
                if c.shares_with(&d) && via != Some(d.as_str()) {
                    self.cursors.entry(d).or_default().resend.insert(Ref::Photo(key.to_string()));
                }
            }
        }
    }

    /// A photo was put into (or taken out of) a collection on this device.
    pub fn set_member(&mut self, coll: &str, key: &str, on: bool, now_ms: u64) -> bool {
        if self.members.get(coll).and_then(|m| m.get(key)).map_or(!on, |e| e.on.v == on) {
            return false;
        }
        let s = self.stamp(now_ms);
        self.members.entry(coll.to_string()).or_default().entry(key.to_string()).or_default().on = Lww::new(on, s);
        self.touch_member(coll, key, None);
        true
    }

    /// What another device says about a photo being in a collection.
    pub fn merge_member(&mut self, coll: &str, key: &str, on: &Lww<bool>, from: &str) -> bool {
        if !ok_id(coll) || !ok_id(key) {
            return false;
        }
        self.clock.observe(on.s);
        let e = self.members.entry(coll.to_string()).or_default().entry(key.to_string()).or_insert_with(|| MemberEntry {
            on: Lww::new(false, Stamp::ZERO),
            seq: 0,
            via: None,
        });
        let fresh = e.seq == 0;
        if !e.on.merge(on) && !fresh {
            return false;
        }
        self.touch_member(coll, key, Some(from));
        true
    }

    // ── photo records ──────────────────────────────────────────────────────────────────────

    fn touch_photo(&mut self, key: &str, via: Option<&str>) {
        let (old, old_via) = self.photos.get(key).map_or((0, None), |e| (e.seq, e.via.clone()));
        // (a photo seen for the first time owes nobody anything yet)
        let via = if old == 0 { via.map(str::to_string) } else { self.via_for(old, old_via.as_deref(), via) };
        let seq = self.next_seq(old, Ref::Photo(key.to_string()));
        if let Some(e) = self.photos.get_mut(key) {
            e.seq = seq;
            e.via = via;
        }
    }

    /// The photo's record as it is in this catalogue now: `(field, fingerprint, is the default)`.
    /// A field seen for the first time counts as never written while it is the default (so any
    /// device's real edit wins over it), and as written now otherwise; a field whose fingerprint
    /// changed was written now. Says whether anything was.
    pub fn local_photo(&mut self, key: &str, fields: &[(&str, u64, bool)], have: Depth, now_ms: u64) -> bool {
        let known = self.photos.get(key);
        let fresh = known.is_none();
        let differs = |name: &str, d: u64| known.and_then(|e| e.fields.get(name)).is_none_or(|f| f.d != d);
        let changed: Vec<(String, u64, bool)> =
            fields.iter().filter(|(n, d, _)| differs(n, *d)).map(|(n, d, def)| (n.to_string(), *d, *def)).collect();
        let had = known.map(|e| e.have);
        if changed.is_empty() && !fresh && had == Some(have) {
            return false;
        }
        let mut stamps = Vec::with_capacity(changed.len());
        for (name, d, default) in changed {
            let first = self.photos.get(key).is_none_or(|e| !e.fields.contains_key(&name));
            let s = if first && default { Stamp::ZERO } else { self.stamp(now_ms) };
            stamps.push((name, FieldState { d, s }));
        }
        let e = self.photos.entry(key.to_string()).or_default();
        e.have = have;
        e.ghost = None;
        let wrote = !stamps.is_empty() || fresh;
        for (name, f) in stamps {
            e.fields.insert(name, f);
        }
        if wrote {
            self.touch_photo(key, None);
        }
        wrote
    }

    /// Whether another device's write of a field replaces what is here.
    pub fn incoming_wins(&self, key: &str, field: &str, incoming: &Lww<Value>) -> bool {
        match self.photos.get(key).and_then(|e| e.fields.get(field)) {
            None => true,
            Some(mine) => match incoming.s.cmp(&mine.s) {
                std::cmp::Ordering::Greater => true,
                std::cmp::Ordering::Less => false,
                std::cmp::Ordering::Equal => crate::digest(&incoming.v) > mine.d,
            },
        }
    }

    /// Another device's write of a field was taken: `d` is the fingerprint of the value as this
    /// catalogue now holds it.
    pub fn accept_field(&mut self, key: &str, field: &str, d: u64, s: Stamp, from: &str) {
        if !ok_id(key) || !ok_id(field) {
            return;
        }
        self.clock.observe(s);
        let e = self.photos.entry(key.to_string()).or_default();
        if e.fields.len() >= MAX_FIELDS && !e.fields.contains_key(field) {
            return;
        }
        e.fields.insert(field.to_string(), FieldState { d, s });
        self.touch_photo(key, Some(from));
    }

    /// A photo another device told of that this catalogue doesn't hold: kept until it is wanted.
    /// Later writes replace earlier ones field by field. Says whether anything was taken.
    pub fn merge_ghost(&mut self, key: &str, info: &Value, fields: &BTreeMap<String, Lww<Value>>, from: &str) -> bool {
        if !ok_id(key) {
            return false;
        }
        let mut taken = false;
        for (name, lww) in fields.iter().take(MAX_FIELDS) {
            if !ok_id(name) || !self.incoming_wins(key, name, lww) {
                continue;
            }
            self.clock.observe(lww.s);
            let e = self.photos.entry(key.to_string()).or_default();
            e.fields.insert(name.clone(), FieldState { d: crate::digest(&lww.v), s: lww.s });
            e.ghost.get_or_insert_with(Default::default).values.insert(name.clone(), lww.v.clone());
            taken = true;
        }
        let e = self.photos.entry(key.to_string()).or_default();
        let g = e.ghost.get_or_insert_with(Default::default);
        if g.info.is_null() && !info.is_null() {
            g.info = info.clone();
            taken = true;
        }
        if taken || e.seq == 0 {
            self.touch_photo(key, Some(from));
        }
        taken
    }

    /// A photo that was only told of is in the catalogue now: its fields keep when they were
    /// written, with the fingerprints of the values as the catalogue holds them.
    pub fn adopt(&mut self, key: &str, digests: &[(&str, u64)], have: Depth) {
        let Some(e) = self.photos.get_mut(key) else { return };
        e.ghost = None;
        e.have = have;
        for (name, d) in digests {
            e.fields.entry(name.to_string()).or_default().d = *d;
        }
    }

    /// Photo `old` is known as `new` from now on (its original's SHA-1 became known, so its key
    /// moved from the content hash to `sha1:<hex>`; `docs/sync.md` ▸ Photo identity). Its record
    /// and its places in collections move with it; when `new` is already known (another device
    /// told of it under the new key first) the two merge field by field, each keeping its later
    /// write. Both keys' items go out again under the new key. Says whether anything moved.
    pub fn rekey(&mut self, old: &str, new: &str) -> bool {
        if old == new || !ok_id(new) {
            return false;
        }
        let mut moved = false;
        if let Some(o) = self.photos.remove(old) {
            moved = true;
            self.by_seq.remove(&o.seq);
            match self.photos.get_mut(new) {
                None => {
                    self.photos.insert(new.to_string(), PhotoEntry { seq: 0, via: None, ..o });
                }
                Some(n) => {
                    n.have = n.have.max(o.have);
                    for (name, f) in &o.fields {
                        if n.fields.get(name).is_some_and(|m| f.s <= m.s) {
                            continue;
                        }
                        n.fields.insert(name.clone(), *f);
                        match (&o.ghost, &mut n.ghost) {
                            (Some(og), Some(ng)) => {
                                if let Some(v) = og.values.get(name) {
                                    ng.values.insert(name.clone(), v.clone());
                                }
                            }
                            // the catalogue holds the later value: the told one mustn't overwrite it
                            (None, Some(ng)) => {
                                ng.values.remove(name);
                            }
                            _ => {}
                        }
                    }
                    if let (Some(og), Some(ng)) = (&o.ghost, &mut n.ghost)
                        && ng.info.is_null()
                    {
                        ng.info = og.info.clone();
                    }
                }
            }
            self.touch_photo(new, None);
        }
        let colls: Vec<String> = self.members.iter().filter(|(_, m)| m.contains_key(old)).map(|(c, _)| c.clone()).collect();
        for coll in colls {
            let Some(m) = self.members.get_mut(&coll) else { continue };
            let Some(o) = m.remove(old) else { continue };
            moved = true;
            self.by_seq.remove(&o.seq);
            let e = m.entry(new.to_string()).or_insert_with(|| MemberEntry { on: o.on.clone(), seq: 0, via: None });
            e.on.merge(&o.on);
            self.touch_member(&coll, new, None);
        }
        for c in self.cursors.values_mut() {
            let stale: Vec<Ref> = c.resend.iter().filter(|r| matches!(r, Ref::Photo(k) | Ref::Member(_, k) if k == old)).cloned().collect();
            for r in stale {
                c.resend.remove(&r);
                c.resend.insert(match r {
                    Ref::Member(coll, _) => Ref::Member(coll, new.to_string()),
                    _ => Ref::Photo(new.to_string()),
                });
            }
        }
        moved
    }

    /// The photos told of that the catalogue doesn't hold.
    pub fn ghosts(&self) -> impl Iterator<Item = &String> {
        self.photos.iter().filter(|(_, e)| e.ghost.is_some()).map(|(k, _)| k)
    }

    /// The photo is in the catalogue now, holding `have` of it.
    pub fn set_have(&mut self, key: &str, have: Depth) {
        if let Some(e) = self.photos.get_mut(key) {
            e.have = have;
        }
    }

    /// How much of photo `key` this device should hold: the most any of its collections asks for.
    pub fn depth_wanted(&self, key: &str) -> Depth {
        let me = self.device.as_str();
        self.collections_of(key).map(|c| c.depth_for(me)).max().unwrap_or(Depth::None)
    }

    /// Whether `peer` may be sent photo `key` at `depth`.
    pub fn may_fetch(&self, peer: &str, key: &str, depth: Depth) -> bool {
        // (an Immich album's files come from the server)
        self.in_group_with(peer) && self.collections_of(key).any(|c| !c.is_immich() && c.may_fetch(peer, depth))
    }

    /// How much of photo `key` this device should get from other devices: as
    /// [`Replica::depth_wanted`], without Immich albums (their files come from the server).
    pub fn depth_from_peers(&self, key: &str) -> Depth {
        let me = self.device.as_str();
        self.collections_of(key).filter(|c| !c.is_immich()).map(|c| c.depth_for(me)).max().unwrap_or(Depth::None)
    }

    // ── what to tell a device ──────────────────────────────────────────────────────────────

    /// Whether `peer` is told item `r` at all.
    pub fn tells(&self, peer: &str, r: &Ref) -> bool {
        let Some(g) = &self.group else { return false };
        if !g.has(peer) && !(matches!(r, Ref::Group) && g.devices.contains_key(peer)) {
            return false;
        }
        match r {
            Ref::Group => true,
            Ref::Coll(id) => self.collection(id).is_some_and(|c| !self.is_private(c) && c.listed_for(peer, g.mode)),
            // (each device learns an Immich album's photos from the server, not from another device)
            Ref::Member(coll, _) => self.collection(coll).is_some_and(|c| !c.is_immich() && c.shares_with(peer)),
            Ref::Photo(key) => self.collections_of(key).any(|c| !self.is_private(c) && c.shares_with(peer)),
        }
    }

    fn seq_of(&self, r: &Ref) -> Option<(u64, Option<&str>)> {
        match r {
            Ref::Group => self.group.as_ref().map(|_| (self.group_seq, self.group_via.as_deref())),
            Ref::Coll(id) => self.collections.get(id).map(|e| (e.seq, e.via.as_deref())),
            Ref::Member(c, k) => self.members.get(c).and_then(|m| m.get(k)).map(|e| (e.seq, e.via.as_deref())),
            Ref::Photo(k) => self.photos.get(k).map(|e| (e.seq, e.via.as_deref())),
        }
    }

    /// Up to `limit` items to send `peer`: first what was queued to send again, then what
    /// changed since it last confirmed, oldest first.
    pub fn outgoing(&self, peer: &str, limit: usize) -> Outgoing {
        let empty = Cursor::default();
        let cur = self.cursors.get(peer).unwrap_or(&empty);
        let mut out = Outgoing { upto: cur.acked, ..Default::default() };
        for r in &cur.resend {
            if out.refs.len() >= limit {
                out.more = true;
                return out;
            }
            // (an item that is gone or no longer told stays queued for nothing: dropped on confirm)
            out.resent.push(r.clone());
            if self.seq_of(r).is_some() && self.tells(peer, r) {
                out.refs.push(r.clone());
            }
        }
        for (seq, r) in self.by_seq.range(cur.acked.saturating_add(1)..) {
            if out.refs.len() >= limit {
                out.more = true;
                return out;
            }
            out.upto = *seq;
            if cur.resend.contains(r) {
                continue;
            }
            let via = self.seq_of(r).and_then(|(_, via)| via);
            if via != Some(peer) && self.tells(peer, r) {
                out.refs.push(r.clone());
            }
        }
        out
    }

    /// `peer` confirmed what [`Replica::outgoing`] gave.
    pub fn confirmed(&mut self, peer: &str, out: &Outgoing) {
        let c = self.cursors.entry(peer.to_string()).or_default();
        c.acked = c.acked.max(out.upto);
        for r in &out.resent {
            c.resend.remove(r);
        }
    }

    /// Whether anything waits to be sent to `peer`.
    pub fn has_outgoing(&self, peer: &str) -> bool {
        !self.outgoing(peer, 1).refs.is_empty()
    }

    /// Whether photo `key` holds a change that `peer` hasn't confirmed and didn't send itself.
    pub fn owes(&self, peer: &str, key: &str) -> bool {
        let r = Ref::Photo(key.to_string());
        let Some((seq, via)) = self.seq_of(&r) else { return false };
        let c = self.cursors.get(peer);
        c.is_some_and(|c| c.resend.contains(&r)) || (seq > c.map_or(0, |c| c.acked) && via != Some(peer))
    }

    /// Whether photo `key` has changes here that a device which should have them, and which this
    /// device has exchanged with, hasn't confirmed.
    pub fn photo_ahead(&self, key: &str) -> bool {
        let r = Ref::Photo(key.to_string());
        let Some((seq, via)) = self.seq_of(&r) else { return false };
        // only devices this one exchanges with directly: what travels on through them is theirs to pass along
        self.cursors.iter().any(|(d, c)| {
            let waiting = c.resend.contains(&r) || (seq > c.acked && via != Some(d.as_str()));
            waiting && self.tells(d, &r)
        })
    }

    /// Whether collection `id` has anything here that a device which should have it hasn't confirmed.
    pub fn collection_ahead(&self, id: &str) -> bool {
        self.members_of(id).any(|k| self.photo_ahead(k))
    }

    /// Item `r` as it travels. `record` gives a catalogue photo's facts and field values; a photo
    /// this catalogue doesn't hold travels on as it was told (so devices in a chain pass it along).
    pub fn item(&self, r: &Ref, record: impl FnOnce(&str) -> Option<(Value, BTreeMap<String, Value>)>) -> Option<Item> {
        match r {
            Ref::Group => self.group.clone().map(Item::Group),
            Ref::Coll(id) => self.collection(id).cloned().map(Item::Coll),
            Ref::Member(coll, key) => {
                let e = self.members.get(coll)?.get(key)?;
                Some(Item::Member { coll: coll.clone(), key: key.clone(), on: e.on.clone() })
            }
            Ref::Photo(key) => {
                let e = self.photos.get(key)?;
                let (info, values) = match &e.ghost {
                    Some(g) => (g.info.clone(), g.values.clone()),
                    None => record(key)?,
                };
                let fields = values.into_iter().filter_map(|(name, v)| e.fields.get(&name).map(|f| (name, Lww::new(v, f.s)))).collect();
                Some(Item::Photo { key: key.clone(), info, fields, here: e.have })
            }
        }
    }

    /// Forgets the group and everything shared (the catalogue's photos and collections stay).
    pub fn reset(&mut self) {
        let device = std::mem::take(&mut self.device);
        let clock = self.clock.clone();
        let private_immich = std::mem::take(&mut self.private_immich);
        *self = Replica { device, clock, private_immich, ..Default::default() };
    }
}

#[cfg(test)]
mod tests;
