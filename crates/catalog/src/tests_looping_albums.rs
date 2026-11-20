//! A smart album on a loop (it includes itself through the albums it tests) holds nothing,
//! whichever way it is asked: its photos, its count, whether it holds one photo, the grid's query
//! for it. The rule, stated once:
//!
//! * an album **on** a loop (a ring of albums testing one another, or one testing itself) holds no
//!   photos;
//! * an album **off** every loop is worked out as always, and one that tests a looping album sees
//!   it as the empty album it is (so "is in A" matches nothing and "isn't in A" matches all);
//! * mending the loop (changing rules, removing an album) gives each album its photos back.

use super::*;

/// A smart album with `any` of the rules in `rules` (a JSON array).
fn any_of(id: u64, rules: serde_json::Value) -> Album {
    let filter: Filter = serde_json::from_value(serde_json::json!({"ruleSet": {"match": "any", "rules": rules}})).unwrap();
    Album { smart: Some(Box::new(filter)), ..Album::new(AlbumId(id), format!("Smart {id}")) }
}

fn in_album(op: &str, id: u64) -> serde_json::Value {
    serde_json::json!({"field": "album", "op": op, "value": id})
}

fn picked() -> serde_json::Value {
    serde_json::json!({"field": "flag", "op": "is", "value": "pick"})
}

fn rated(n: u64) -> serde_json::Value {
    serde_json::json!({"field": "rating", "op": "gte", "value": n})
}

/// Three photos: `.0` is picked, `.1` is rated 5, `.2` is neither.
fn photos(c: &mut Catalog) -> (PhotoId, PhotoId, PhotoId) {
    let mut add = |name: &str| {
        let id = c.alloc_photo_id();
        let p = Photo::new(id, Source::Demo { scene: 1 }, name, "JPEG", 600, 400, "2026-09-30T10:00:00");
        c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        id
    };
    let ids = (add("picked.jpg"), add("rated.jpg"), add("plain.jpg"));
    c.apply(Op::SetFlag { id: ids.0, flag: Flag::Pick }).unwrap();
    c.apply(Op::SetRating { id: ids.1, rating: 5 }).unwrap();
    ids
}

fn add(c: &mut Catalog, album: Album) {
    c.apply(Op::AddAlbum { album }).unwrap();
}

/// What the grid shows for `album`: the query the sidebar click makes.
fn grid(c: &Catalog, album: u64) -> Vec<PhotoId> {
    c.query(&Filter { album: Some(AlbumId(album)), ..Default::default() }, &Sort::default())
}

/// Every way of asking what an album holds agrees it holds `want`.
#[track_caller]
fn assert_holds(c: &Catalog, album: u64, want: &[PhotoId]) {
    let mut want = want.to_vec();
    want.sort();
    let mut got = c.album_photos(AlbumId(album));
    got.sort();
    assert_eq!(got, want, "album_photos of {album}");
    assert_eq!(c.album_count(AlbumId(album)), want.len(), "album_count of {album}");
    let mut shown = grid(c, album);
    shown.sort();
    assert_eq!(shown, want, "the grid's source for {album}");
    for p in c.photos() {
        assert_eq!(c.album_contains(AlbumId(album), p), want.contains(&p.id), "album_contains of {album} for {:?}", p.id);
    }
}

/// 1 ⇄ 2: each holds the photos of its own rule, or those of the other. Without the loop, 1 holds
/// the picked and the rated photo, and so does 2.
fn two_albums_in_a_loop() -> (Catalog, (PhotoId, PhotoId, PhotoId)) {
    let mut c = Catalog::new();
    let p = photos(&mut c);
    add(&mut c, any_of(1, serde_json::json!([picked(), in_album("is", 2)])));
    add(&mut c, any_of(2, serde_json::json!([rated(4), in_album("is", 1)])));
    (c, p)
}

#[test]
fn two_albums_that_test_each_other_hold_nothing() {
    let (c, _) = two_albums_in_a_loop();
    assert_holds(&c, 1, &[]);
    assert_holds(&c, 2, &[]);
}

#[test]
fn an_album_that_tests_itself_holds_nothing() {
    let mut c = Catalog::new();
    photos(&mut c);
    add(&mut c, any_of(1, serde_json::json!([picked(), in_album("is", 1)])));
    assert_holds(&c, 1, &[]);
}

#[test]
fn every_album_on_a_longer_ring_holds_nothing_and_the_ones_leading_into_it_do_not_count_as_on_it() {
    let mut c = Catalog::new();
    let (picked_photo, rated_photo, plain) = photos(&mut c);
    // 1 → 2 → 3 → 1
    add(&mut c, any_of(1, serde_json::json!([picked(), in_album("is", 2)])));
    add(&mut c, any_of(2, serde_json::json!([rated(4), in_album("is", 3)])));
    add(&mut c, any_of(3, serde_json::json!([rated(1), in_album("is", 1)])));
    // 4 tests the ring (a tail into it), 5 tests 4: neither is on it
    add(&mut c, any_of(4, serde_json::json!([rated(4), in_album("is", 1)])));
    add(&mut c, any_of(5, serde_json::json!([in_album("is", 4)])));
    for ring in [1, 2, 3] {
        assert_holds(&c, ring, &[]);
    }
    assert_holds(&c, 4, &[rated_photo]);
    assert_holds(&c, 5, &[rated_photo]);
    let _ = (picked_photo, plain);
}

#[test]
fn a_smart_album_off_every_loop_is_as_it_was() {
    let mut c = Catalog::new();
    let (picked_photo, rated_photo, _) = photos(&mut c);
    let (a, b) = (any_of(1, serde_json::json!([picked(), in_album("is", 2)])), any_of(2, serde_json::json!([rated(4), in_album("is", 1)])));
    add(&mut c, a);
    add(&mut c, b);
    // 3 and 4 form a chain, no loop: 4 holds what 3 holds
    add(&mut c, any_of(3, serde_json::json!([picked(), rated(4)])));
    add(&mut c, any_of(4, serde_json::json!([in_album("is", 3)])));
    assert_holds(&c, 3, &[picked_photo, rated_photo]);
    assert_holds(&c, 4, &[picked_photo, rated_photo]);
}

#[test]
fn an_album_that_tests_a_looping_album_sees_it_as_empty() {
    let (mut c, (a, b, d)) = two_albums_in_a_loop();
    // 3 isn't on the loop; it asks about album 1
    add(&mut c, any_of(3, serde_json::json!([in_album("is", 1)])));
    let mut not_in_one = any_of(4, serde_json::json!([in_album("isNot", 1)]));
    not_in_one.name = "Not in one".into();
    add(&mut c, not_in_one);
    assert_holds(&c, 3, &[]);
    assert_holds(&c, 4, &[a, b, d]);
}

#[test]
fn a_regular_album_is_not_a_loop() {
    let (mut c, (a, ..)) = two_albums_in_a_loop();
    add(&mut c, Album { photos: vec![a], ..Album::new(AlbumId(9), "Plain") });
    assert_holds(&c, 9, &[a]);
}

#[test]
fn mending_the_loop_gives_the_photos_back() {
    let (mut c, (picked_photo, rated_photo, _)) = two_albums_in_a_loop();
    assert_holds(&c, 1, &[]);
    // 2 stops testing 1
    let before = any_of(2, serde_json::json!([rated(4), in_album("is", 1)])).smart.unwrap();
    let inv = c.apply(Op::SetAlbumRules { id: AlbumId(2), rules: any_of(2, serde_json::json!([rated(4)])).smart.unwrap() }).unwrap();
    assert_holds(&c, 2, &[rated_photo]);
    assert_holds(&c, 1, &[picked_photo, rated_photo]);
    // undoing that brings the loop, and the emptiness, back
    c.apply(inv).unwrap();
    assert_eq!(c.album(AlbumId(2)).and_then(|a| a.smart.as_deref()), Some(&*before));
    assert_holds(&c, 1, &[]);
    assert_holds(&c, 2, &[]);
    // removing 2 leaves 1 testing an album that is gone
    c.apply(Op::RemoveAlbum { id: AlbumId(2) }).unwrap();
    assert_holds(&c, 1, &[picked_photo]);
    // adding it back closes the loop again
    add(&mut c, any_of(2, serde_json::json!([rated(4), in_album("is", 1)])));
    assert_holds(&c, 1, &[]);
}

#[test]
fn the_loop_can_say_which_album_it_goes_through() {
    let (mut c, _) = two_albums_in_a_loop();
    assert!(c.album_on_a_loop(AlbumId(1)) && c.album_on_a_loop(AlbumId(2)));
    assert_eq!(c.album_loops_through(AlbumId(1)), Some(AlbumId(2)));
    assert_eq!(c.album_loops_through(AlbumId(2)), Some(AlbumId(1)));
    // 3 tests an album on the loop, and 4 tests itself as well as 3
    add(&mut c, any_of(3, serde_json::json!([in_album("is", 1)])));
    add(&mut c, any_of(4, serde_json::json!([in_album("is", 4)])));
    assert!(!c.album_on_a_loop(AlbumId(3)), "a tail into a loop is not on it");
    assert_eq!(c.album_loops_through(AlbumId(3)), None);
    assert_eq!(c.album_loops_through(AlbumId(4)), Some(AlbumId(4)), "an album that tests itself loops through itself");
    // a plain album and an album that does not exist
    add(&mut c, Album::new(AlbumId(5), "Plain"));
    assert!(!c.album_on_a_loop(AlbumId(5)) && !c.album_on_a_loop(AlbumId(99)));
    assert_eq!(c.album_loops_through(AlbumId(5)), None);
}

#[test]
fn on_a_longer_ring_the_album_named_is_one_that_leads_back() {
    let mut c = Catalog::new();
    // 1 → 2 → 3 → 1, and 1 also tests 7, which tests itself (a loop of its own, not 1's)
    add(&mut c, any_of(1, serde_json::json!([in_album("is", 7), in_album("is", 2)])));
    add(&mut c, any_of(2, serde_json::json!([in_album("is", 3)])));
    add(&mut c, any_of(3, serde_json::json!([in_album("is", 1)])));
    add(&mut c, any_of(7, serde_json::json!([in_album("is", 7)])));
    assert_eq!(c.album_loops_through(AlbumId(1)), Some(AlbumId(2)), "7 is on a loop, but not the one 1 is on");
    assert_eq!(c.album_loops_through(AlbumId(2)), Some(AlbumId(3)));
    assert_eq!(c.album_loops_through(AlbumId(3)), Some(AlbumId(1)));
}

/// Asking is cheap: the albums on a loop are worked out once, and again only after an op that adds
/// or removes an album or changes rules, not after the edits to photos the library sees all day.
#[test]
fn the_loops_are_remembered_until_an_album_changes() {
    let (mut c, (a, ..)) = two_albums_in_a_loop();
    assert!(!c.loops_known());
    assert_holds(&c, 1, &[]);
    assert!(c.loops_known());
    c.apply(Op::SetRating { id: a, rating: 3 }).unwrap();
    c.apply(Op::SetFlag { id: a, flag: Flag::Reject }).unwrap();
    assert!(c.loops_known(), "photo edits don't change which album tests which");
    c.apply(Op::SetAlbumRules { id: AlbumId(2), rules: any_of(2, serde_json::json!([rated(4)])).smart.unwrap() }).unwrap();
    assert!(!c.loops_known(), "new rules");
    assert!(!c.album_on_a_loop(AlbumId(1)));
    assert!(c.loops_known());
    add(&mut c, Album::new(AlbumId(9), "New"));
    assert!(!c.loops_known(), "a new album");
    assert!(!c.album_on_a_loop(AlbumId(1)));
    c.apply(Op::RemoveAlbum { id: AlbumId(9) }).unwrap();
    assert!(!c.loops_known(), "a removed album");
    // a copy builds its own when asked
    assert!(!c.album_on_a_loop(AlbumId(1)));
    assert!(!c.clone().loops_known());
}

/// A refused op leaves no half-forgotten state behind: the answer after it is still right.
#[test]
fn a_refused_edit_leaves_the_answers_as_they_were() {
    let (mut c, _) = two_albums_in_a_loop();
    assert_holds(&c, 1, &[]);
    assert!(c.apply(Op::SetAlbumRules { id: AlbumId(404), rules: any_of(1, serde_json::json!([rated(4)])).smart.unwrap() }).is_err());
    assert!(c.apply(Op::AddAlbum { album: any_of(1, serde_json::json!([rated(4)])) }).is_err(), "the id is taken");
    assert_holds(&c, 1, &[]);
    assert_holds(&c, 2, &[]);
}
