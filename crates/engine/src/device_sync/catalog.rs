//! The catalogue's side of sync: what changed here is told to the replica ([`scan`]), and what
//! other devices changed is written into the catalogue ([`apply`], [`reconcile`]).
//!
//! The catalogue leads for what this device owns or may add to; the replica leads for the rest.
//! A shared collection of the catalogue is compared with how it looked the last time
//! (`Lib::base`), so a photo another device added that isn't in the catalogue's collection yet is
//! never mistaken for one that was taken out here.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use lcc_catalog::{Album, AlbumId, Op, PhotoId, Source};
use lcc_sync::model::{Collection, CollectionKind, Grant, Mode, Take};
use lcc_sync::pill::Depth;
use lcc_sync::proto::{Batch, MAX_ITEMS};
use lcc_sync::replica::{Item, Outgoing, Ref};
use serde_json::{Value, json};

use super::{Lib, Want, now_ms, record};
use crate::Session;

/// How often the photos in no collection are worked out again.
const UNFILED_EVERY: Duration = Duration::from_secs(5);
/// A batch stays under this size (frames carry up to 64 MiB).
const MAX_BATCH_BYTES: usize = 24 << 20;
/// Files asked of a device at a time.
const WANTS: usize = 24;
/// How long a device that said it lacks a file isn't asked for it again.
const ASK_AGAIN: Duration = Duration::from_secs(600);

fn plain(a: &Album) -> bool {
    a.smart.is_none() && !a.quick
}

/// The catalogue's photos by key (and back), and the content-hash keys other devices may still
/// use for photos known here by SHA-1. A photo the replica still knows by its content hash moves
/// to its SHA-1 key (catalogues synced before photos were keyed by SHA-1, and photos hashed while
/// the catalogue was closed).
pub(super) fn index(s: &Session, lib: &mut Lib) {
    lib.album_sig.clear();
    lib.keys.clear();
    lib.ids.clear();
    lib.alias.clear();
    lib.hints.clear();
    for p in s.catalog.photos() {
        if let Some(k) = record::key_of(p, &s.catalog)
            && !lib.keys.contains_key(&k)
        {
            if let Some(old) = record::content_key(p).filter(|c| *c != k) {
                lib.alias.insert(old.to_string(), k.clone());
                lib.replica.rekey(old, &k);
            }
            lib.keys.insert(k.clone(), p.id);
            lib.ids.insert(p.id, k);
        }
    }
    wanting(lib);
}

fn reindex(s: &Session, lib: &mut Lib, id: PhotoId) {
    let p = s.catalog.photo(id);
    let key = p.and_then(|p| record::key_of(p, &s.catalog));
    let old = lib.ids.get(&id).cloned();
    if old == key {
        return;
    }
    // a photo got (or lost) its key: every collection's photos are looked at afresh
    lib.album_sig.clear();
    if let Some(o) = &old {
        // (another device's claim about it is settled by the hash made here)
        lib.hints.retain(|_, v| v != o);
        lib.ids.remove(&id);
        if lib.keys.get(o) == Some(&id) {
            lib.keys.remove(o);
        }
    }
    if let Some(k) = key
        && !lib.keys.contains_key(&k)
    {
        // its original's SHA-1 became known: what the replica holds under the old key moves
        if let Some(o) = old.as_deref().filter(|o| !o.starts_with(record::SHA1_KEY)) {
            lib.replica.rekey(o, &k);
            lib.alias.insert(o.to_string(), k.clone());
        }
        if let Some(c) = p.and_then(|p| record::content_key(p)).filter(|c| *c != k) {
            lib.alias.insert(c.to_string(), k.clone());
        }
        lib.keys.insert(k.clone(), id);
        lib.ids.insert(id, k);
    }
}

/// The key this catalogue uses for a key another device sent (it may still name the photo by
/// its content hash, which is known here by SHA-1).
pub(super) fn local_key<'a>(lib: &'a Lib, key: &'a str) -> &'a str {
    if lib.keys.contains_key(key) {
        return key;
    }
    lib.alias.get(key).or_else(|| lib.hints.get(key)).map_or(key, String::as_str)
}

/// The key to merge an item another device sent under. Beyond [`local_key`]: a photo sent under
/// its SHA-1 key whose content hash names a photo here that isn't hashed yet is merged under
/// that photo's key for now, and the photo is hashed next (`remote::State::sha1_first`). The
/// other device's SHA-1 is only a hint: the key moves when this device's own hash is done, and
/// only to what it computed (a wrong claim leaves the photo as it is). A photo only told of
/// (no file here to hash) moves to the key the other device uses.
fn incoming_key(s: &mut Session, lib: &mut Lib, key: &str, info: &Value) -> String {
    let k = local_key(lib, key).to_string();
    if k != key || lib.keys.contains_key(&k) || lib.replica.photos.contains_key(&k) {
        return k;
    }
    let (Some(_), Some(h)) = (record::sha1_of_key(key), info.get("content_hash").and_then(Value::as_str)) else { return k };
    if h == key || h.len() > 128 {
        return k;
    }
    if let Some(id) = lib.keys.get(h).copied() {
        let unhashed = s.catalog.photo(id).is_some_and(|p| p.sha1.is_none() && record::content_key(p) == Some(h));
        if unhashed {
            lib.hints.insert(key.to_string(), h.to_string());
            #[cfg(not(target_arch = "wasm32"))]
            {
                s.remote.sha1_first.insert(id);
                s.remote.sha1_idle_at = None;
            }
            return h.to_string();
        }
    } else if lib.replica.photos.contains_key(h) {
        lib.replica.rekey(h, key);
        lib.alias.insert(h.to_string(), key.to_string());
    }
    key.to_string()
}

/// The photos this device should hold more of than it does.
fn wanting(lib: &mut Lib) {
    let r = &lib.replica;
    lib.wanting = r
        .photos
        .iter()
        .filter(|(k, e)| e.ghost.is_none() && lib.keys.contains_key(*k) && e.have < r.depth_wanted(k))
        .map(|(k, _)| k.clone())
        .collect();
}

fn in_group(lib: &Lib) -> Option<Mode> {
    let r = &lib.replica;
    r.group.as_ref().filter(|g| g.has(&r.device)).map(|g| g.mode)
}

/// Photos of the catalogue that can't sync yet: not hashed, or a second import of the same bytes.
pub(super) fn unkeyed(s: &Session) -> usize {
    s.catalog.photos().filter(|p| !p.deleted && p.copy_of.is_none() && record::key_of(p, &s.catalog).is_none()).count()
}

/// New collections of the catalogue the person hasn't said how to share.
pub(super) fn undecided(s: &Session, lib: &Lib) -> Vec<Value> {
    if in_group(lib).is_none() {
        return Vec::new();
    }
    s.catalog
        .albums()
        .filter(|a| plain(a) && !a.folder && !lib.replica.asked.contains(&a.id.0))
        .map(|a| json!({"album": a.id.0, "name": a.name}))
        .take(50)
        .collect()
}

/// Makes the catalogue's collection `album` a shared one (not allowed to anyone yet).
pub(super) fn register(s: &Session, lib: &mut Lib, album: AlbumId) -> Result<String, String> {
    if let Some(id) = lib.replica.collection_of_album(album.0) {
        return Ok(id.to_string());
    }
    let a = s.catalog.album(album).ok_or("no such collection")?;
    if !plain(a) {
        return Err("smart collections and the Quick Collection don't sync".into());
    }
    let id = lcc_peer::identity::random_id().map_err(|e| e.to_string())?;
    let kind = if a.folder { CollectionKind::Set } else { CollectionKind::Album };
    let parent = a.parent.and_then(|p| lib.replica.collection_of_album(p.0).map(str::to_string));
    lib.replica.add_collection(&id, kind, &a.name, parent, Grant::None, Some(album.0), now_ms());
    Ok(id)
}

/// A group was started or joined. Sharing the whole catalogue lists every collection on every
/// device (allowed to none until the person says); sharing collections only lists nothing yet.
/// Either way the collections there are now count as asked about: the list in Settings shows them.
pub(super) fn register_all(s: &Session, lib: &mut Lib) {
    let Some(mode) = in_group(lib) else { return };
    let mut albums: Vec<&Album> = s.catalog.albums().filter(|a| plain(a)).collect();
    // sets first, so a collection finds its set
    albums.sort_by_key(|a| (!a.folder, a.id));
    for a in albums {
        lib.replica.asked.insert(a.id.0);
        if mode == Mode::Catalogue {
            let _ = register(s, lib, a.id);
        }
    }
}

fn scan_photo(s: &Session, lib: &mut Lib, key: &str, now: u64) -> bool {
    let Some(p) = lib.keys.get(key).and_then(|id| s.catalog.photo(*id)) else { return false };
    let known = lib.replica.photos.get(key);
    if known.is_some_and(|e| e.ghost.is_some()) {
        // told of before it was imported here: `reconcile` joins the two
        return false;
    }
    // a photo imported here holds its original (a link-only one, Immich's preview)
    let link_only = p.preview_only.as_deref() == Some(crate::remote::LINK_ONLY);
    let have = if link_only { Depth::Preview } else { known.map_or(Depth::Full, |e| e.have) };
    lib.replica.local_photo(key, &record::digests(p, &s.catalog), have, now)
}

/// Compares one shared collection's photos with how they were, and tells the replica.
fn scan_members(s: &Session, lib: &mut Lib, id: &str, cur: BTreeSet<String>, now: u64) -> bool {
    let me = lib.replica.device.clone();
    let may_add = lib.replica.collection(id).is_some_and(|c| c.may_add(&me));
    let mut changed = false;
    if may_add {
        let (adds, removes): (Vec<String>, Vec<String>) = match lib.base.get(id) {
            Some(b) => (cur.difference(b).cloned().collect(), b.difference(&cur).cloned().collect()),
            // first look since the catalogue was opened: what is here and not known is new
            None => {
                let known: BTreeSet<&String> = lib.replica.members_of(id).collect();
                (cur.iter().filter(|k| !known.contains(k)).cloned().collect(), Vec::new())
            }
        };
        let immich = lib.replica.collection(id).is_some_and(|c| c.is_immich());
        for k in adds {
            changed |= scan_photo(s, lib, &k, now);
            changed |= lib.replica.set_member(id, &k, true, now);
            if immich {
                // to the server: uploaded unless Immich has it, then put into the album
                lib.immich_out.push((id.to_string(), k, true));
            }
        }
        for k in removes {
            changed |= lib.replica.set_member(id, &k, false, now);
            if immich {
                lib.immich_out.push((id.to_string(), k, false));
            }
        }
    }
    lib.base.insert(id.to_string(), cur);
    changed
}

fn album_keys(lib: &Lib, a: &Album) -> BTreeSet<String> {
    a.photos.iter().filter_map(|p| lib.ids.get(p).cloned()).collect()
}

fn signature(a: &Album) -> (usize, u64) {
    (a.photos.len(), a.photos.iter().fold(0u64, |h, p| h.wrapping_mul(31).wrapping_add(p.0)))
}

fn scan_albums(s: &Session, lib: &mut Lib, mode: Mode, now: u64) -> bool {
    let me = lib.replica.device.clone();
    let mut changed = false;
    if mode == Mode::Catalogue {
        let mut fresh: Vec<&Album> = s.catalog.albums().filter(|a| plain(a) && lib.replica.collection_of_album(a.id.0).is_none()).collect();
        fresh.sort_by_key(|a| (!a.folder, a.id));
        for a in fresh {
            changed |= register(s, lib, a.id).is_ok();
        }
    }
    let mapped: Vec<(String, u64)> = lib.replica.albums.iter().map(|(id, a)| (id.clone(), *a)).collect();
    for (id, album) in mapped {
        let Some(c) = lib.replica.collection(&id).cloned() else {
            lib.replica.albums.remove(&id);
            continue;
        };
        let Some(a) = s.catalog.album(AlbumId(album)) else {
            // removed here: the owner's removal goes to every device; another device just stops taking it
            if c.owner == me {
                let _ = lib.replica.set_collection_deleted(&id, true, now);
            } else if c.take_of(&me) != Take::None {
                let _ = lib.replica.set_take(&id, Take::None, now);
            }
            lib.replica.albums.remove(&id);
            lib.base.remove(&id);
            lib.album_sig.remove(&album);
            changed = true;
            continue;
        };
        if c.owner == me {
            let parent = if mode == Mode::Catalogue {
                a.parent.and_then(|p| lib.replica.collection_of_album(p.0).map(str::to_string))
            } else {
                c.meta.v.parent.clone()
            };
            changed |= lib.replica.set_collection_meta(&id, &a.name, parent, now).unwrap_or(false);
        }
        if a.folder || c.kind == CollectionKind::Set {
            continue;
        }
        let sig = signature(a);
        if lib.album_sig.get(&album) == Some(&sig) && lib.base.contains_key(&id) {
            continue;
        }
        lib.album_sig.insert(album, sig);
        let cur = album_keys(lib, a);
        changed |= scan_members(s, lib, &id, cur, now);
    }
    // the photos in no collection, when this device shares them
    let unfiled = format!("unfiled:{me}");
    if lib.replica.collection(&unfiled).is_some_and(|c| !c.gone() && c.grant.v != Grant::None)
        && lib.unfiled_at.is_none_or(|t| t.elapsed() >= UNFILED_EVERY)
    {
        lib.unfiled_at = Some(Instant::now());
        let cur: BTreeSet<String> = s
            .catalog
            .photos()
            .filter(|p| !p.deleted && !p.local && s.catalog.albums_of(p.id).is_empty())
            .filter_map(|p| lib.ids.get(&p.id).cloned())
            .collect();
        changed |= scan_members(s, lib, &unfiled, cur, now);
    }
    changed
}

/// Tells the replica what changed in the catalogue since it last looked. Says whether anything did.
pub(super) fn scan(s: &Session, lib: &mut Lib) -> bool {
    let rev = s.catalog.revision;
    if lib.revision == Some(rev) {
        return false;
    }
    let Some(mode) = in_group(lib) else {
        lib.revision = Some(rev);
        return false;
    };
    let now = now_ms();
    let touched = lib.revision.and_then(|r| s.catalog.touched_since(r));
    match &touched {
        Some(ids) => {
            for id in ids {
                reindex(s, lib, *id);
            }
        }
        None => index(s, lib),
    }
    let mut changed = scan_albums(s, lib, mode, now);
    match touched {
        Some(ids) => {
            for id in ids {
                // (a photo an Immich album lists comes into the replica when it is imported here)
                if let Some(key) = lib.ids.get(&id).cloned()
                    && (lib.replica.photos.contains_key(&key) || lib.replica.collections_of(&key).next().is_some())
                {
                    changed |= scan_photo(s, lib, &key, now);
                }
            }
        }
        None => {
            let members: BTreeSet<&String> = lib.replica.members.values().flat_map(|m| m.iter().filter(|(_, e)| e.on.v).map(|(k, _)| k)).collect();
            let keys: BTreeSet<String> = lib.replica.photos.keys().chain(members).filter(|k| lib.keys.contains_key(*k)).cloned().collect();
            for key in keys {
                changed |= scan_photo(s, lib, &key, now);
            }
        }
    }
    lib.revision = Some(rev);
    changed
}

/// What to send `peer` now.
pub(super) fn prepare(s: &Session, lib: &Lib, peer: &str) -> (Batch, Outgoing) {
    let r = &lib.replica;
    let left = !r.group.as_ref().is_some_and(|g| g.has(&r.device));
    let mut limit = MAX_ITEMS;
    loop {
        let mut out = r.outgoing(peer, limit);
        if left {
            // a device that left says so and nothing else
            out.refs.retain(|x| *x == Ref::Group);
        }
        let items: Vec<Item> = out
            .refs
            .iter()
            .filter_map(|x| {
                r.item(x, |key| lib.keys.get(key).and_then(|id| s.catalog.photo(*id)).map(|p| (record::info(p), record::values(p, &s.catalog))))
            })
            .collect();
        let size: usize = items.iter().map(|i| serde_json::to_vec(i).map_or(0, |b| b.len())).sum();
        if size > MAX_BATCH_BYTES && limit > 1 {
            limit = (limit / 4).max(1);
            continue;
        }
        // the Immich connections this device shares, once a session to each device of the group
        let accounts = match &lib.share_out {
            Some((_, a)) if !left && r.in_group_with(peer) && !lib.share_sent.contains(peer) => a.clone(),
            _ => Vec::new(),
        };
        return (Batch { items, upto: out.upto, more: out.more, ack: None, accounts }, out);
    }
}

/// Merges what `peer` sent. `Err` when it can't be taken now (it is sent again).
pub(super) fn apply(s: &mut Session, lib: &mut Lib, peer: &lcc_peer::Peer, batch: &Batch) -> Result<(), String> {
    let from = peer.device_id.as_str();
    // an edit in progress here is never changed under the hand: the batch waits
    if let Some(i) = &s.interaction
        && let Some(key) = lib.ids.get(&i.photo)
        && batch.items.iter().any(|it| matches!(it, Item::Photo { key: k, .. } if k == key))
    {
        return Err("a photo of the batch is being edited".into());
    }
    if lib.replica.in_group_with(from) {
        let room = lcc_sync::proto::MAX_SHARED.saturating_sub(lib.share_in.len());
        lib.share_in.extend(batch.accounts.iter().filter(|a| a.sane()).take(room).cloned());
    }
    let now = now_ms();
    // what this device held back from `peer` earlier is tried again first (older, so the batch's
    // own items still win where they are later)
    let earlier = lib.replica.take_held(from);
    let mut ahead: Option<u64> = None;
    for whole in earlier.iter().chain(batch.items.iter().take(MAX_ITEMS * 2)) {
        // a write stamped past this device's clock (plus the allowed skew) would beat every later
        // edit here: that part (field by field for a photo) is held back until it is due, so the
        // batch is still confirmed and nothing of it is lost
        let (ready, late) = whole.split_ahead(now);
        if let Some(late) = late {
            ahead = ahead.max(late.ahead_of(now).map(|st| st.t));
            let dropped = lib.replica.hold(from, late);
            if dropped > 0 {
                log::warn!("sync: {dropped} held-back change(s) from {} dropped (too many held)", peer.name);
            }
            lib.dirty = true;
        }
        let Some(item) = ready.as_ref() else { continue };
        if let Item::Group(g) = item {
            let asked = lib.replica.invited.contains(from) || lib.replica.group.as_ref().is_some_and(|mine| mine.devices.contains_key(from));
            if asked {
                lib.replica.merge_group(g, from, now);
            }
            continue;
        }
        if !lib.replica.in_group_with(from) {
            continue;
        }
        match item {
            Item::Group(_) => {}
            Item::Coll(c) => {
                lib.replica.merge_collection(c, from, now);
            }
            Item::Member { coll, key, on } => {
                let key = local_key(lib, key).to_string();
                lib.replica.merge_member(coll, &key, on, from, now);
            }
            Item::Photo { key, info, fields, .. } => {
                let key = &incoming_key(s, lib, key, info);
                match lib.keys.get(key).copied() {
                    Some(id) if lib.replica.photos.get(key).is_none_or(|e| e.ghost.is_none()) => {
                        for (name, lww) in fields.iter().take(32) {
                            if !record::FIELDS.contains(&name.as_str()) || !lib.replica.incoming_wins(key, name, lww) {
                                continue;
                            }
                            if name == "develop" && lib.replica.owes(from, key) {
                                record::keep_version(s, id, &peer.name);
                            }
                            if let Err(e) = record::apply(s, id, name, &lww.v) {
                                // this field stays as it is here (and is not confirmed as taken over)
                                log::warn!("sync: {e}");
                                continue;
                            }
                            let d = s.catalog.photo(id).map_or(0, |p| record::digest(p, &s.catalog, name));
                            lib.replica.accept_field(key, name, d, lww.s, from, now);
                        }
                    }
                    _ => {
                        lib.replica.merge_ghost(key, info, fields, from, now);
                    }
                }
            }
        }
    }
    reconcile(s, lib);
    if let Some(t) = ahead
        && lib.held_told.insert(from.to_string())
    {
        let minutes = t.saturating_sub(now) / 60_000;
        s.notify(crate::notice::Notice::SyncHeldFuture { device: peer.name.clone(), minutes });
    }
    Ok(())
}

fn display_name(lib: &Lib, c: &Collection) -> String {
    match c.kind {
        CollectionKind::Unfiled => {
            let owner = lib.replica.group.as_ref().and_then(|g| g.name_of(&c.owner)).unwrap_or("another device");
            format!("{} ({owner})", c.meta.v.name)
        }
        _ if c.meta.v.name.is_empty() => "Collection".to_string(),
        _ => c.meta.v.name.clone(),
    }
}

/// Where a photo from another device goes: `<catalogue>/Synced/<file name>`, with part of its
/// key in the name when that is taken.
fn synced_path(s: &Session, lib: &Lib, name: &str, key: &str) -> PathBuf {
    let dir = lib.dir.join("Synced");
    let plain = dir.join(name);
    let taken = |p: &PathBuf| {
        p.exists() || s.catalog.photos().any(|q| matches!(&q.source, Source::File { path } if std::path::Path::new(path) == p.as_path()))
    };
    if !taken(&plain) {
        return plain;
    }
    let tag: String = key.chars().filter(char::is_ascii_alphanumeric).take(8).collect();
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, format!(".{ext}")),
        _ => (name, String::new()),
    };
    dir.join(format!("{stem}-{tag}{ext}"))
}

/// A photo another device told of comes into the catalogue (its file follows).
fn adopt(s: &mut Session, lib: &mut Lib, key: &str) {
    let Some(ghost) = lib.replica.photos.get(key).and_then(|e| e.ghost.clone()) else { return };
    let (id, have) = match lib.keys.get(key).copied() {
        // imported here meanwhile: the same photo
        Some(id) => (id, Depth::Full),
        None => {
            let name = ghost.info.get("file_name").and_then(Value::as_str).unwrap_or_default().to_string();
            let id = s.catalog.alloc_photo_id();
            let imported = (s.clock)();
            let photo = match record::photo_from(&ghost.info, key, id, "", &imported) {
                Ok(mut p) => {
                    p.source = Source::File { path: synced_path(s, lib, &p.file_name, key).to_string_lossy().to_string() };
                    p
                }
                Err(e) => {
                    log::warn!("sync: `{name}` from another device can't be listed: {e}");
                    // (not tried again every time: the record stays as it was told)
                    return;
                }
            };
            if let Err(e) = s.apply_system(Op::AddPhoto { photo: Box::new(photo) }) {
                log::warn!("sync: `{name}` from another device can't be listed: {e}");
                return;
            }
            lib.keys.insert(key.to_string(), id);
            lib.ids.insert(id, key.to_string());
            // a connected Immich server that has the same original links it (no upload, no second asset)
            #[cfg(not(target_arch = "wasm32"))]
            s.remote.link_queue.push(id);
            (id, Depth::None)
        }
    };
    for (name, v) in &ghost.values {
        if record::FIELDS.contains(&name.as_str())
            && let Err(e) = record::apply(s, id, name, v)
        {
            log::warn!("sync: {e}");
        }
    }
    let digests: Vec<(&str, u64)> =
        s.catalog.photo(id).map(|p| record::digests(p, &s.catalog).into_iter().map(|(n, d, _)| (n, d)).collect()).unwrap_or_default();
    lib.replica.adopt(key, &digests, have);
}

/// Brings the catalogue in line with the replica: the collections other devices own, the photos
/// they told of that this device takes, and which photos are in each shared collection.
pub(crate) fn reconcile(s: &mut Session, lib: &mut Lib) {
    let Some(mode) = in_group(lib) else { return };
    let me = lib.replica.device.clone();
    // collections of other devices → collections of the catalogue (sets first)
    let mut ids: Vec<(bool, String)> = lib.replica.collections.iter().map(|(id, e)| (e.c.kind != CollectionKind::Set, id.clone())).collect();
    ids.sort();
    for (_, id) in ids {
        let Some(c) = lib.replica.collection(&id).cloned() else { continue };
        if c.owner == me {
            continue;
        }
        let mapped = lib.replica.albums.get(&id).copied().filter(|a| s.catalog.album(AlbumId(*a)).is_some());
        let listed = !c.gone() && c.listed_for(&me, mode) && (mode == Mode::Catalogue || c.grant.v != Grant::None);
        let want = if c.is_immich() {
            // an Immich album is in the catalogue where the account is and the device takes it;
            // its set ("Immich") as long as one of its albums is
            let here = c.immich.as_ref().is_some_and(|r| lib.immich_here.contains(&r.account));
            !c.gone()
                && here
                && match c.kind {
                    CollectionKind::Set => lib
                        .replica
                        .collections
                        .values()
                        .any(|e| e.c.meta.v.parent.as_deref() == Some(id.as_str()) && !e.c.gone() && e.c.take_of(&me) != Take::None),
                    _ => c.take_of(&me) != Take::None,
                }
        } else {
            listed
                && match mode {
                    Mode::Catalogue => true,
                    Mode::Collections => c.kind != CollectionKind::Set && c.take_of(&me) != Take::None,
                }
        };
        match (want, mapped) {
            (true, None) => {
                let aid = s.catalog.alloc_album_id();
                let mut a = Album::new(aid, display_name(lib, &c));
                a.folder = c.kind == CollectionKind::Set;
                if mode == Mode::Catalogue || c.is_immich() {
                    a.parent = c
                        .meta
                        .v
                        .parent
                        .as_ref()
                        .and_then(|p| lib.replica.albums.get(p))
                        .map(|p| AlbumId(*p))
                        .filter(|p| s.catalog.album(*p).is_some_and(|x| x.folder));
                }
                if s.apply_system(Op::AddAlbum { album: a }).is_ok() {
                    lib.replica.albums.insert(id.clone(), aid.0);
                    lib.replica.asked.insert(aid.0);
                    lib.base.insert(id.clone(), BTreeSet::new());
                }
            }
            (false, Some(a)) => {
                let _ = s.apply_system(Op::RemoveAlbum { id: AlbumId(a) });
                lib.replica.albums.remove(&id);
                lib.base.remove(&id);
                lib.album_sig.remove(&a);
            }
            (true, Some(a)) => {
                let name = display_name(lib, &c);
                if s.catalog.album(AlbumId(a)).is_some_and(|x| x.name != name) {
                    let _ = s.apply_system(Op::RenameAlbum { id: AlbumId(a), name });
                }
            }
            (false, None) => {}
        }
    }
    // photos told of that this device takes (or imported meanwhile)
    let ghosts: Vec<String> =
        lib.replica.ghosts().filter(|k| lib.keys.contains_key(*k) || lib.replica.depth_wanted(k) >= Depth::Preview).cloned().collect();
    for key in ghosts {
        adopt(s, lib, &key);
    }
    // which photos are in each shared collection
    let mapped: Vec<(String, u64)> = lib.replica.albums.iter().map(|(id, a)| (id.clone(), *a)).collect();
    for (id, album) in mapped {
        let Some(a) = s.catalog.album(AlbumId(album)) else { continue };
        if a.folder {
            continue;
        }
        let cur = album_keys(lib, a);
        let mut photos = a.photos.clone();
        let before = photos.len();
        let off = |k: &String| lib.replica.members.get(&id).and_then(|m| m.get(k)).is_some_and(|e| !e.on.v);
        photos.retain(|p| lib.ids.get(p).is_none_or(|k| !off(k)));
        let mut changed = photos.len() != before;
        for k in lib.replica.members_of(&id) {
            if !cur.contains(k)
                && let Some(p) = lib.keys.get(k)
            {
                photos.push(*p);
                changed = true;
            }
        }
        if changed && s.apply_system(Op::SetAlbumPhotos { id: AlbumId(album), photos }).is_err() {
            continue;
        }
        // (a collection `scan` hasn't looked at yet keeps its first look: what is in it now is new to the replica)
        if lib.base.contains_key(&id)
            && let Some(a) = s.catalog.album(AlbumId(album))
        {
            let now = album_keys(lib, a);
            lib.album_sig.insert(album, signature(a));
            lib.base.insert(id, now);
        }
    }
    wanting(lib);
}

/// The files to ask `peer` for.
pub(super) fn wants(s: &Session, lib: &mut Lib, peer: &str) -> Vec<Want> {
    lib.missing.retain(|_, at| at.elapsed() < ASK_AGAIN);
    let smart_dir = s.media.smart_dir.clone();
    let mut out = Vec::new();
    for key in &lib.wanting {
        if out.len() >= WANTS {
            break;
        }
        if lib.missing.contains_key(&(peer.to_string(), key.clone())) {
            continue;
        }
        let Some(p) = lib.keys.get(key).and_then(|id| s.catalog.photo(*id)) else { continue };
        let Source::File { path } = &p.source else { continue };
        match lib.replica.depth_from_peers(key) {
            // (a link-only photo's original comes from Immich, and its file here is the preview)
            Depth::Full if !path.is_empty() && p.preview_only.as_deref() != Some(crate::remote::LINK_ONLY) => {
                out.push(Want { key: key.clone(), depth: Depth::Full, dest: PathBuf::from(path) })
            }
            Depth::Preview => {
                if let Some(dir) = &smart_dir {
                    out.push(Want { key: key.clone(), depth: Depth::Preview, dest: crate::smart::sharded_file_path(dir, p) });
                }
            }
            _ => {}
        }
    }
    out
}

/// Where the files `peer` asks for are. `Err(true)`: being made, ask again; `Err(false)`: not
/// here, or `peer` may not have it.
pub(super) fn locate(s: &mut Session, lib: &mut Lib, peer: &str, wants: &[(String, Depth)]) -> Vec<Result<PathBuf, bool>> {
    let smart_dir = s.media.smart_dir.clone();
    let mut build: Vec<u64> = Vec::new();
    let found: Vec<Result<PathBuf, bool>> = wants
        .iter()
        .map(|(key, depth)| {
            let key = local_key(lib, key);
            if !lib.replica.may_fetch(peer, key, *depth) {
                return Err(false);
            }
            let have = lib.replica.photos.get(key).map_or(Depth::None, |e| e.have);
            let p = lib.keys.get(key).and_then(|id| s.catalog.photo(*id)).ok_or(false)?;
            let Source::File { path } = &p.source else { return Err(false) };
            match depth {
                Depth::Full if have >= Depth::Full => Ok(PathBuf::from(path)),
                Depth::Preview => {
                    let dir = smart_dir.as_ref().ok_or(false)?;
                    let sp = crate::smart::sharded_file_path(dir, p);
                    if sp.is_file() {
                        Ok(sp)
                    } else if have >= Depth::Full {
                        // made from the original now; the other device asks again
                        build.push(p.id.0);
                        Err(true)
                    } else {
                        Err(false)
                    }
                }
                _ => Err(false),
            }
        })
        .collect();
    if !build.is_empty() {
        #[cfg(not(target_arch = "wasm32"))]
        if let Err(e) = s.execute("library.smartPreviews", &json!({"ids": build, "background": true})) {
            log::info!("sync: previews for another device wait: {e}");
        }
    }
    found
}
