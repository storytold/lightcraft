//! Property tests of the merge rules (completion plan 2.6): every merge here is a join, so copies
//! that saw the same writes agree whatever order (and however often) they were merged in, and no
//! merge lets a device that doesn't own a collection take more of it than its owner granted.
//!
//! Run with the crate's tests and by `cargo xtask sil sync` (docs/testing-sil.md);
//! `PROPTEST_CASES` raises the case count for a deeper run.

use proptest::prelude::*;

use crate::clock::{Lww, Stamp};
use crate::model::{Collection, CollectionKind, CollectionMeta, DeviceInfo, Grant, Group, Mode, Take};
use crate::pill::Depth;
use crate::replica::{Item, Ref, Replica};

const OWNER: &str = "o";
const PEERS: [&str; 3] = ["p", "q", "r"];

/// Few distinct stamps, so ties (same stamp, different values) come up often.
fn stamp() -> impl Strategy<Value = Stamp> {
    (0u64..4, 0u32..2, 0u32..2).prop_map(|(t, c, d)| Stamp { t, c, d })
}

fn lww<T: std::fmt::Debug + Clone + 'static>(v: impl Strategy<Value = T>) -> impl Strategy<Value = Lww<T>> {
    (v, stamp()).prop_map(|(v, s)| Lww { v, s })
}

fn grant() -> impl Strategy<Value = Grant> {
    prop::sample::select(Grant::ALL.to_vec())
}

fn take() -> impl Strategy<Value = Take> {
    prop::sample::select(Take::ALL.to_vec())
}

fn name() -> impl Strategy<Value = String> {
    prop::sample::select(vec!["Trip", "Trip 2", "Family", "B"]).prop_map(str::to_string)
}

/// One device's copy of the same collection (same id, owner and kind; names already clean, as
/// every copy a device keeps is).
fn collection() -> impl Strategy<Value = Collection> {
    let meta = (name(), prop::option::of(Just("set".to_string()))).prop_map(|(name, parent)| CollectionMeta { name, parent });
    let takes = prop::collection::btree_map(prop::sample::select(PEERS.to_vec()).prop_map(str::to_string), lww(take()), 0..=PEERS.len());
    (lww(meta), lww(grant()), takes, lww(any::<bool>())).prop_map(|(meta, grant, takes, deleted)| Collection {
        id: "c".into(),
        owner: OWNER.into(),
        kind: CollectionKind::Album,
        meta,
        grant,
        takes,
        deleted,
        immich: None,
    })
}

/// One device's copy of the same group.
fn group() -> impl Strategy<Value = Group> {
    let ids = prop::sample::select(vec![OWNER, "p", "q", "r"]).prop_map(str::to_string);
    let info = (name(), any::<bool>()).prop_map(|(name, left)| DeviceInfo { name, left });
    prop::collection::btree_map(ids, lww(info), 0..=4).prop_map(|devices| Group { id: "g".into(), mode: Mode::Collections, keeper: None, devices })
}

fn join<T: Clone>(a: &T, b: &T, merge: impl Fn(&mut T, &T) -> bool) -> T {
    let mut x = a.clone();
    merge(&mut x, b);
    x
}

fn lww_merge(a: &mut Lww<u8>, b: &Lww<u8>) -> bool {
    a.merge(b)
}
fn coll_merge(a: &mut Collection, b: &Collection) -> bool {
    a.merge(b)
}
fn group_merge(a: &mut Group, b: &Group) -> bool {
    a.merge(b)
}

/// What a device that doesn't own `c` may get of it never exceeds what its owner grants.
fn no_leak(c: &Collection) -> Result<(), TestCaseError> {
    let most = c.grant.v.most();
    for d in PEERS {
        prop_assert!(c.take_of(d) <= most, "{d} takes {:?} of a {:?} grant", c.take_of(d), c.grant.v);
        prop_assert!(c.depth_for(d) <= most.depth());
        prop_assert!(c.take_of(d) <= c.takes.get(d).map_or(Take::None, |t| t.v), "{d} got more than it chose");
        for depth in [Depth::Preview, Depth::Full] {
            prop_assert!(!c.may_fetch(d, depth) || depth <= most.depth());
        }
        if c.grant.v == Grant::None {
            prop_assert!(!c.shares_with(d) && !c.may_add(d), "{d} reaches an unshared collection");
        }
        if c.grant.v != Grant::Sync {
            prop_assert!(!c.may_add(d), "{d} may add under a {:?} grant", c.grant.v);
        }
    }
    Ok(())
}

proptest! {
    #[test]
    fn lww_merge_is_a_join(a in lww(0u8..4), b in lww(0u8..4), c in lww(0u8..4)) {
        prop_assert_eq!(join(&a, &b, lww_merge), join(&b, &a, lww_merge));
        prop_assert_eq!(join(&join(&a, &b, lww_merge), &c, lww_merge), join(&a, &join(&b, &c, lww_merge), lww_merge));
        let mut again = a.clone();
        prop_assert!(!again.merge(&a));
        prop_assert_eq!(again, a);
    }

    #[test]
    fn collection_merge_is_a_join(a in collection(), b in collection(), c in collection()) {
        prop_assert_eq!(join(&a, &b, coll_merge), join(&b, &a, coll_merge));
        prop_assert_eq!(join(&join(&a, &b, coll_merge), &c, coll_merge), join(&a, &join(&b, &c, coll_merge), coll_merge));
        let mut again = a.clone();
        prop_assert!(!again.merge(&a), "merging a copy into itself changed it");
        prop_assert_eq!(&again, &a);
        // merging what was already merged in changes nothing
        let ab = join(&a, &b, coll_merge);
        prop_assert_eq!(join(&ab, &b, coll_merge), ab);
    }

    #[test]
    fn group_merge_is_a_join(a in group(), b in group(), c in group()) {
        prop_assert_eq!(join(&a, &b, group_merge), join(&b, &a, group_merge));
        prop_assert_eq!(join(&join(&a, &b, group_merge), &c, group_merge), join(&a, &join(&b, &c, group_merge), group_merge));
        let mut again = a.clone();
        prop_assert!(!again.merge(&a));
        prop_assert_eq!(again, a);
    }

    /// Devices that got the same copies, in different orders and with repeats, hold the same
    /// collection, and none of the states on the way leaks more than is granted.
    #[test]
    fn copies_converge_in_any_order_without_leaking(
        copies in prop::collection::vec(collection(), 1..6),
        order in Just((0..12usize).collect::<Vec<_>>()).prop_shuffle(),
    ) {
        let n = copies.len();
        let mut forward = copies[0].clone();
        for c in &copies {
            forward.merge(c);
            no_leak(&forward)?;
        }
        let mut shuffled = copies[order[0] % n].clone();
        for i in &order {
            shuffled.merge(&copies[i % n]);
            no_leak(&shuffled)?;
        }
        // (the shuffled order may skip some copies: merge them in last, twice)
        for c in copies.iter().chain(&copies) {
            shuffled.merge(c);
        }
        prop_assert_eq!(forward, shuffled);
    }

    /// Through the replica, as a sync session runs: whatever the owner grants (and takes back)
    /// and whatever the peer asks to take, the owner never tells the peer which photos a
    /// collection holds, nor sends their records, unless the collection is shared with it now.
    #[test]
    fn the_owner_tells_a_peer_only_what_it_was_granted(
        steps in prop::collection::vec((grant(), take(), any::<bool>()), 1..12),
    ) {
        let mut owner = Replica::new(OWNER);
        let mut peer = Replica::new("p");
        owner.start_group("g", Mode::Collections, None, "Owner", 1000).map_err(|e| TestCaseError::fail(e.to_string()))?;
        let Some(Item::Group(g)) = owner.item(&Ref::Group, |_| None) else { return Err(TestCaseError::fail("no group")) };
        peer.join_group(&g, "Peer", 1001).map_err(|e| TestCaseError::fail(e.to_string()))?;
        let Some(Item::Group(pg)) = peer.item(&Ref::Group, |_| None) else { return Err(TestCaseError::fail("no group")) };
        owner.merge_group(&pg, "p", 1001);
        owner.add_collection("c", CollectionKind::Album, "Trip", None, Grant::None, Some(1), 1002);
        let mut now = 1010;
        for (i, (grant, take, on)) in steps.iter().enumerate() {
            now += 10;
            let _ = owner.set_grant("c", *grant, now);
            let key = format!("k{}", i % 3);
            owner.local_photo(&key, &[("rating", 1, false)], Depth::Full, now);
            owner.set_member("c", &key, *on, now);
            // the peer learns of the collection and says what it takes
            if let Some(c) = owner.collection("c").cloned() {
                peer.merge_collection(&c, OWNER, now);
                let _ = peer.set_take("c", *take, now + 1);
                if let Some(back) = peer.collection("c").cloned() {
                    owner.merge_collection(&back, "p", now + 1);
                }
            }
            let c = owner.collection("c").cloned().ok_or_else(|| TestCaseError::fail("collection lost"))?;
            no_leak(&c)?;
            let shared = c.shares_with("p");
            let out = owner.outgoing("p", 1000);
            for r in &out.refs {
                match r {
                    Ref::Member(coll, _) => prop_assert!(coll != "c" || shared, "membership told under {:?}/{:?}", c.grant.v, c.take_of("p")),
                    Ref::Photo(k) => prop_assert!(shared, "photo {k} sent under {:?}/{:?}", c.grant.v, c.take_of("p")),
                    _ => {}
                }
            }
            if steps.len() % 2 == 0 {
                owner.confirmed("p", &out);
            }
        }
        // what is left to tell, once the dust settles, follows the final grant as well
        let c = owner.collection("c").cloned().ok_or_else(|| TestCaseError::fail("collection lost"))?;
        if !c.shares_with("p") {
            let out = owner.outgoing("p", 1000);
            prop_assert!(out.refs.iter().all(|r| !matches!(r, Ref::Member(..) | Ref::Photo(_))), "{:?}", out.refs);
        }
    }
}
