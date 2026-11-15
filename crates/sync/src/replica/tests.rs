use super::*;
use crate::model::{CollectionKind, Grant, Mode, Take};
use serde_json::json;

/// A device: its replica and a stand-in catalogue (photo key → field → value).
struct Dev {
    r: Replica,
    cat: HashMap<String, BTreeMap<String, Value>>,
    now: u64,
}

impl Dev {
    fn new(id: &str) -> Dev {
        Dev { r: Replica::new(id), cat: HashMap::new(), now: 1000 }
    }
    fn tick(&mut self) -> u64 {
        self.now += 10;
        self.now
    }
    /// The catalogue's photo as the replica is told of it.
    fn scan(&mut self, key: &str) {
        let now = self.tick();
        let fields: Vec<(String, u64, bool)> = self.cat[key].iter().map(|(n, v)| (n.clone(), crate::digest(v), v.is_null())).collect();
        let refs: Vec<(&str, u64, bool)> = fields.iter().map(|(n, d, def)| (n.as_str(), *d, *def)).collect();
        self.r.local_photo(key, &refs, Depth::Full, now);
    }
    fn import(&mut self, key: &str) {
        self.cat.insert(key.into(), [("rating".to_string(), Value::Null), ("develop".to_string(), Value::Null)].into());
        self.scan(key);
    }
    fn edit(&mut self, key: &str, field: &str, v: Value) {
        self.cat.get_mut(key).unwrap().insert(field.into(), v);
        self.scan(key);
    }
    fn value(&self, key: &str, field: &str) -> Value {
        self.cat.get(key).and_then(|f| f.get(field)).cloned().unwrap_or(Value::Null)
    }
}

/// Sends everything `from` has for `to`, as an exchange would, and returns how many items went.
fn send(from: &mut Dev, to: &mut Dev) -> usize {
    let (a, b) = (from.r.device.clone(), to.r.device.clone());
    let mut sent = 0;
    loop {
        let out = from.r.outgoing(&b, 3);
        for r in &out.refs {
            let item = from.r.item(r, |k| from.cat.get(k).map(|f| (json!({"name": k}), f.clone()))).unwrap();
            // through JSON, as on the wire
            let item: Item = serde_json::from_str(&serde_json::to_string(&item).unwrap()).unwrap();
            match item {
                Item::Group(g) => {
                    to.r.merge_group(&g, &a);
                }
                Item::Coll(c) => {
                    to.r.merge_collection(&c, &a);
                }
                Item::Member { coll, key, on } => {
                    to.r.merge_member(&coll, &key, &on, &a);
                }
                Item::Photo { key, info, fields, .. } => match to.cat.get_mut(&key) {
                    Some(row) => {
                        for (name, lww) in &fields {
                            if to.r.incoming_wins(&key, name, lww) {
                                row.insert(name.clone(), lww.v.clone());
                                to.r.accept_field(&key, name, crate::digest(&lww.v), lww.s, &a);
                            }
                        }
                    }
                    None => {
                        to.r.merge_ghost(&key, &info, &fields, &a);
                    }
                },
            }
            sent += 1;
        }
        from.r.confirmed(&b, &out);
        if !out.more {
            return sent;
        }
    }
}

fn both(a: &mut Dev, b: &mut Dev) {
    send(a, b);
    send(b, a);
}

/// `a` starts a group and `others` join it; `a` owns collection `c` holding photo `p`, which
/// every device has in its catalogue.
fn group(mode: Mode, grant: Grant, a: &mut Dev, others: &mut [&mut Dev]) {
    let now = a.tick();
    a.r.start_group("g", mode, Some(a.r.device.clone()), "A", now).unwrap();
    a.r.add_collection("c", CollectionKind::Album, "Trip", None, grant, Some(1), now);
    a.import("p");
    a.r.set_member("c", "p", true, now);
    for (i, d) in others.iter_mut().enumerate() {
        let now = d.tick();
        let g = a.r.group.clone().unwrap();
        d.r.join_group(&g, &format!("D{i}"), now).unwrap();
        d.import("p");
        // the joiner's own group item reaches the starter, and the starter's collection the joiner
        both(d, a);
        both(a, d);
    }
}

#[test]
fn an_edit_travels_down_a_chain_and_settles() {
    let (mut a, mut b, mut c) = (Dev::new("a"), Dev::new("b"), Dev::new("c"));
    group(Mode::Catalogue, Grant::Sync, &mut a, &mut [&mut b, &mut c]);
    for d in [&mut b, &mut c] {
        let now = d.tick();
        d.r.set_take("c", Take::FullSync, now).unwrap();
    }
    // A and C never talk: everything goes through B
    for _ in 0..3 {
        both(&mut a, &mut b);
        both(&mut b, &mut c);
    }
    a.edit("p", "rating", json!(4));
    c.edit("p", "develop", json!({"exposure": 1.0}));
    for _ in 0..3 {
        both(&mut a, &mut b);
        both(&mut b, &mut c);
    }
    for d in [&a, &b, &c] {
        assert_eq!(d.value("p", "rating"), json!(4), "{}", d.r.device);
        assert_eq!(d.value("p", "develop"), json!({"exposure": 1.0}), "{}", d.r.device);
    }
    // an edit made while another device's edit of the same photo is on its way in is not lost
    a.edit("p", "rating", json!(1));
    c.edit("p", "develop", json!({"exposure": 2.0}));
    both(&mut b, &mut c);
    both(&mut a, &mut b);
    both(&mut b, &mut c);
    both(&mut a, &mut b);
    for d in [&a, &b, &c] {
        assert_eq!((d.value("p", "rating"), d.value("p", "develop")), (json!(1), json!({"exposure": 2.0})), "{}", d.r.device);
    }
    // settled: nothing more to send in any direction, and nothing shows as ahead
    assert_eq!(send(&mut a, &mut b) + send(&mut b, &mut a) + send(&mut b, &mut c) + send(&mut c, &mut b), 0);
    assert!(!b.r.photo_ahead("p"));
    // A and C met once (when C joined) and not since: each still owes the other directly
    assert!(a.r.photo_ahead("p") && c.r.photo_ahead("p"));
    both(&mut a, &mut c);
    assert!(!a.r.photo_ahead("p") && !c.r.photo_ahead("p"));
}

#[test]
fn edits_of_the_same_field_on_two_devices_end_as_the_later_one_everywhere() {
    let (mut a, mut b) = (Dev::new("a"), Dev::new("b"));
    group(Mode::Catalogue, Grant::Sync, &mut a, &mut [&mut b]);
    let now = b.tick();
    b.r.set_take("c", Take::FullSync, now).unwrap();
    both(&mut a, &mut b);
    both(&mut a, &mut b);
    a.now = 5000;
    b.now = 6000;
    a.edit("p", "rating", json!(1));
    b.edit("p", "rating", json!(5));
    assert!(a.r.photo_ahead("p"), "an unsent edit shows as ahead");
    both(&mut a, &mut b);
    both(&mut a, &mut b);
    assert_eq!((a.value("p", "rating"), b.value("p", "rating")), (json!(5), json!(5)));
    // an edit made with a clock that runs behind still counts as later than what it has seen
    a.now = 100;
    a.edit("p", "rating", json!(2));
    both(&mut a, &mut b);
    assert_eq!(b.value("p", "rating"), json!(2));
}

#[test]
fn a_device_is_told_only_what_its_owner_allows_and_it_takes() {
    let (mut a, mut b) = (Dev::new("a"), Dev::new("b"));
    group(Mode::Collections, Grant::None, &mut a, &mut [&mut b]);
    a.edit("p", "rating", json!(3));
    both(&mut a, &mut b);
    assert!(b.r.collection("c").is_none(), "an unshared collection isn't even listed in collections mode");
    assert_eq!(b.value("p", "rating"), Value::Null);
    // allowed, but not taken: listed, nothing more
    let now = a.tick();
    a.r.set_grant("c", Grant::Preview, now).unwrap();
    both(&mut a, &mut b);
    assert_eq!(b.r.collection("c").map(|c| c.grant.v), Some(Grant::Preview));
    assert_eq!(b.r.members_of("c").count(), 0);
    assert_eq!(b.value("p", "rating"), Value::Null);
    // more than allowed is refused; what is allowed brings the photos and their records
    let now = b.tick();
    assert_eq!(b.r.set_take("c", Take::FullQuality, now), Err(Refused::NotAllowed));
    assert_eq!(b.r.set_grant("c", Grant::Sync, now), Err(Refused::NotOwner));
    b.r.set_take("c", Take::Previews, now).unwrap();
    both(&mut b, &mut a);
    both(&mut a, &mut b);
    assert_eq!(b.r.members_of("c").collect::<Vec<_>>(), ["p"]);
    assert_eq!(b.value("p", "rating"), json!(3));
    assert_eq!(b.r.depth_wanted("p"), Depth::Preview);
    assert!(a.r.may_fetch("b", "p", Depth::Preview) && !a.r.may_fetch("b", "p", Depth::Full));
    assert!(!a.r.may_fetch("stranger", "p", Depth::Preview));
    // the owner stops sharing: later edits stay home
    let now = a.tick();
    a.r.set_grant("c", Grant::None, now).unwrap();
    both(&mut a, &mut b);
    a.edit("p", "rating", json!(1));
    both(&mut a, &mut b);
    assert_eq!(b.value("p", "rating"), json!(3));
    assert!(!a.r.photo_ahead("p"), "nobody is owed a photo that isn't shared");
}

#[test]
fn a_photo_put_into_a_shared_collection_later_brings_its_record() {
    let (mut a, mut b) = (Dev::new("a"), Dev::new("b"));
    group(Mode::Collections, Grant::Sync, &mut a, &mut [&mut b]);
    let now = b.tick();
    b.r.set_take("c", Take::FullSync, now).unwrap();
    both(&mut b, &mut a);
    both(&mut a, &mut b);
    // an old photo of A's, edited long before it is put into the collection
    a.import("q");
    a.edit("q", "rating", json!(5));
    both(&mut a, &mut b);
    assert!(!b.r.photos.contains_key("q"), "a photo in no shared collection isn't told");
    let now = a.tick();
    a.r.set_member("c", "q", true, now);
    both(&mut a, &mut b);
    // B doesn't hold it: kept with its facts and record until B's catalogue takes it
    let ghost = b.r.photos.get("q").and_then(|e| e.ghost.clone()).expect("told as a photo to fetch");
    assert_eq!(ghost.info, json!({"name": "q"}));
    assert_eq!(ghost.values.get("rating"), Some(&json!(5)));
    assert_eq!(b.r.depth_wanted("q"), Depth::Full);
    // and B adds one of its own, which the owner is told
    b.import("r");
    let now = b.tick();
    b.r.set_member("c", "r", true, now);
    both(&mut b, &mut a);
    assert!(a.r.members_of("c").any(|k| k == "r") && a.r.photos.get("r").is_some_and(|e| e.ghost.is_some()));
}

#[test]
fn what_is_still_to_send_survives_saving_and_loading() {
    let (mut a, mut b) = (Dev::new("a"), Dev::new("b"));
    group(Mode::Catalogue, Grant::Sync, &mut a, &mut [&mut b]);
    let now = b.tick();
    b.r.set_take("c", Take::FullSync, now).unwrap();
    both(&mut b, &mut a);
    both(&mut a, &mut b);
    a.edit("p", "rating", json!(2));
    let before = a.r.outgoing("b", 100);
    assert!(!before.refs.is_empty());
    let mut loaded: Replica = serde_json::from_str(&serde_json::to_string(&a.r).unwrap()).unwrap();
    loaded.rebuild();
    assert_eq!(loaded.outgoing("b", 100), before);
    // and new changes get numbers past the old ones
    let now = a.tick();
    assert!(loaded.set_member("c", "z", true, now));
    assert!(loaded.outgoing("b", 100).upto > before.upto);
}

#[test]
fn hostile_items_change_nothing_and_never_panic() {
    let mut a = Dev::new("a");
    let now = a.tick();
    a.r.start_group("g", Mode::Collections, None, "A", now).unwrap();
    let long = "x".repeat(10_000);
    assert!(!a.r.merge_member(&long, "k", &Lww::new(true, Stamp { t: 1, c: 0, d: 0 }), "b"));
    assert!(!a.r.merge_member("c", "", &Lww::new(true, Stamp { t: 1, c: 0, d: 0 }), "b"));
    assert!(!a.r.merge_ghost("\u{0}", &json!({}), &BTreeMap::new(), "b"));
    let mut c = Collection::new(&long, "b", CollectionKind::Album, "n", None, Grant::Sync, Stamp::ZERO);
    assert!(!a.r.merge_collection(&c, "b"));
    c.id = "ok".into();
    c.owner = String::new();
    assert!(!a.r.merge_collection(&c, "b"));
    // a group with another id is not this group
    let other = Group { id: "other".into(), mode: Mode::Catalogue, keeper: None, devices: BTreeMap::new() };
    assert!(!a.r.merge_group(&other, "b"));
    // a record with far too many fields is cut, not kept whole
    let fields: BTreeMap<String, Lww<Value>> = (0..500).map(|i| (format!("f{i}"), Lww::new(json!(i), Stamp { t: 9, c: 0, d: 0 }))).collect();
    a.r.merge_ghost("k", &json!({}), &fields, "b");
    assert!(a.r.photos["k"].fields.len() <= 32);
    // nothing of a device outside the group is ever told or served
    assert!(a.r.outgoing("stranger", 10).refs.is_empty());
    assert!(!a.r.may_fetch("stranger", "k", Depth::Preview));
}

#[test]
fn a_second_group_is_refused_and_names_are_unique() {
    let (mut a, mut b) = (Dev::new("a"), Dev::new("b"));
    let now = a.tick();
    a.r.start_group("g1", Mode::Collections, None, "Studio", now).unwrap();
    assert_eq!(a.r.start_group("g2", Mode::Collections, None, "Studio", now), Err(Refused::AlreadyInGroup));
    let g = a.r.group.clone().unwrap();
    assert_eq!(b.r.join_group(&g, "studio", 5), Err(Refused::NameTaken("studio".into())));
    b.r.join_group(&g, "Tablet", 5).unwrap();
    both(&mut b, &mut a);
    assert_eq!(a.r.rename_me("TABLET", 9000), Err(Refused::NameTaken("TABLET".into())));
    // joined elsewhere: another group's invitation is refused until this one is left
    let other = Group { id: "g9".into(), mode: Mode::Catalogue, keeper: None, devices: BTreeMap::new() };
    assert_eq!(b.r.join_group(&other, "Tablet", 6), Err(Refused::AlreadyInGroup));
    b.r.leave("b", 7000).unwrap();
    both(&mut b, &mut a);
    assert!(!a.r.in_group_with("b"), "the others learn that a device left");
    assert!(a.r.outgoing("b", 10).refs.iter().all(|r| *r == Ref::Group), "a device that left is told no more than that");
}

#[test]
fn a_photo_rekeyed_to_its_sha1_keeps_its_record_and_collections() {
    let (mut a, mut b) = (Dev::new("a"), Dev::new("b"));
    group(Mode::Catalogue, Grant::Sync, &mut a, &mut [&mut b]);
    let now = b.tick();
    b.r.set_take("c", Take::FullSync, now).unwrap();
    both(&mut a, &mut b);
    both(&mut b, &mut a);
    a.edit("p", "rating", json!(3));
    both(&mut a, &mut b);
    // A learns the original's SHA-1 first: its key moves; B still uses the old one
    let row = a.cat.remove("p").unwrap();
    a.cat.insert("sha1:x".into(), row);
    assert!(a.r.rekey("p", "sha1:x"));
    assert!(!a.r.photos.contains_key("p") && a.r.members_of("c").any(|k| k == "sha1:x"));
    assert_eq!(a.r.collections_of("sha1:x").count(), 1);
    // B edits under the old key meanwhile, and is told of the new key before it rekeys
    b.edit("p", "develop", json!({"exposure": 1.0}));
    send(&mut a, &mut b);
    assert!(b.r.photos.get("sha1:x").is_some_and(|e| e.ghost.is_some()));
    let row = b.cat.remove("p").unwrap();
    b.cat.insert("sha1:x".into(), row);
    assert!(b.r.rekey("p", "sha1:x"));
    // B's own later edit isn't overwritten by what it was told
    let g = b.r.photos["sha1:x"].ghost.clone().unwrap();
    assert!(!g.values.contains_key("develop"));
    // the catalogue takes what it was told that is later (as the engine's adopt does)
    for (f, v) in &g.values {
        b.cat.get_mut("sha1:x").unwrap().insert(f.clone(), v.clone());
    }
    let digests: Vec<(String, u64)> = b.cat["sha1:x"].iter().map(|(n, v)| (n.clone(), crate::digest(v))).collect();
    let refs: Vec<(&str, u64)> = digests.iter().map(|(n, d)| (n.as_str(), *d)).collect();
    b.r.adopt("sha1:x", &refs, Depth::Full);
    for _ in 0..3 {
        both(&mut a, &mut b);
    }
    for d in [&a, &b] {
        assert_eq!((d.value("sha1:x", "rating"), d.value("sha1:x", "develop")), (json!(3), json!({"exposure": 1.0})), "{}", d.r.device);
    }
    assert!(!a.r.has_outgoing("b") && !b.r.has_outgoing("a"), "settled");
    assert!(!a.r.photos.contains_key("p") && !b.r.photos.contains_key("p"));
    assert!(b.r.members_of("c").any(|k| k == "sha1:x") && !b.r.members_of("c").any(|k| k == "p"));
    // a rekey to nothing, to itself or of an unknown key changes nothing
    assert!(!a.r.rekey("sha1:x", "sha1:x") && !a.r.rekey("nope", "sha1:y") && !a.r.rekey("sha1:x", ""));
    assert!(!a.r.rekey("sha1:x", &"k".repeat(500)));
}

#[test]
fn immich_albums_are_collections_every_device_shares_but_members_come_from_the_server() {
    let (mut a, mut b) = (Dev::new("a"), Dev::new("b"));
    group(Mode::Collections, Grant::None, &mut a, &mut [&mut b]);
    // both devices see the same album on the server: the same ids, one collection
    for d in [&mut a, &mut b] {
        let now = d.tick();
        assert!(d.r.immich_collection("immich:u1", "u1", None, "Immich", None, now));
        assert!(d.r.immich_collection("immich:u1:al1", "u1", Some("al1"), "Holiday", Some("immich:u1".into()), now));
        assert!(!d.r.immich_collection("immich:u1:al1", "u1", Some("al1"), "Holiday", Some("immich:u1".into()), now), "nothing new");
    }
    both(&mut a, &mut b);
    let c = b.r.collection("immich:u1:al1").unwrap().clone();
    assert!(c.is_immich() && c.grant.v == Grant::Sync && c.kind == CollectionKind::Album);
    assert!(b.r.collection("immich:u1").is_some_and(|c| c.kind == CollectionKind::Set));
    // takes: none, previews or full quality; never full sync
    let now = b.tick();
    assert_eq!(b.r.set_take("immich:u1:al1", Take::FullSync, now), Err(Refused::NotAllowed));
    b.r.set_take("immich:u1:al1", Take::Previews, now).unwrap();
    // A's membership (from its own listing) isn't told; the photo's record is, and no file is served
    let now = a.tick();
    a.r.set_member("immich:u1:al1", "p", true, now);
    both(&mut a, &mut b);
    assert!(b.r.members_of("immich:u1:al1").next().is_none(), "B lists the album's photos itself");
    assert!(a.r.tells("b", &Ref::Photo("p".into())), "the record travels to a device that takes it");
    assert!(!a.r.may_fetch("b", "p", Depth::Preview), "files come from Immich");
    assert_eq!(a.r.depth_from_peers("p"), Depth::Full, "p is also in A's own collection c");
    // a private connection's collections are never told
    a.r.private_immich.insert("u1".into());
    let now = a.tick();
    a.r.immich_collection("immich:u1:al2", "u1", Some("al2"), "Secret", Some("immich:u1".into()), now);
    both(&mut a, &mut b);
    assert!(b.r.collection("immich:u1:al2").is_none());
    assert!(!a.r.tells("b", &Ref::Coll("immich:u1:al1".into())));
    // hostile ids change nothing
    assert!(!a.r.immich_collection("", "u1", None, "x", None, now));
    assert!(!a.r.immich_collection(&"x".repeat(500), "u1", None, "x", None, now));
    assert!(!a.r.immich_collection("immich:x", "u\u{0}", None, "x", None, now));
}
