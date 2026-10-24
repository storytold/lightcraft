//! Smart albums that would include themselves, through the albums they test.
//!
//! The rule is the catalog's: a new edit that would put a smart album on such a loop is refused
//! (`Catalog::apply_new`), whoever makes it and however it is packed (alone, in a batch, as a
//! new album). A library that already holds a loop opens and keeps working (`Catalog::apply`,
//! `replay` and snapshots take it).
//!
//! Scenarios:
//! - When a new edit (adding a smart album, changing rules, alone or in a batch) would put a
//!   smart album on a loop, then it is refused, names the album, and nothing changed.
//! - When a new edit makes no loop, then it goes through, whatever else is in the library.
//! - Given a library that already holds a loop, then it opens, and edits that add no loop go
//!   through (also to the albums on it), and the loop can be mended.
//! - Given a ring of thousands of smart albums, then the edit that closes it is refused without
//!   running out of stack.

use crate::*;

fn smart(c: &mut Catalog, name: &str, rules: Filter) -> AlbumId {
    let id = c.alloc_album_id();
    c.apply(Op::AddAlbum { album: Album { smart: Some(Box::new(rules)), ..Album::new(id, name) } }).unwrap();
    id
}

fn plain(c: &mut Catalog, name: &str) -> AlbumId {
    let id = c.alloc_album_id();
    c.apply(Op::AddAlbum { album: Album::new(id, name) }).unwrap();
    id
}

/// Rules of Album rules: `(op, album)` joined by `all`.
fn tests(rules: &[(&str, AlbumId)]) -> Filter {
    let rules: Vec<serde_json::Value> = rules.iter().map(|(op, a)| serde_json::json!({"field": "album", "op": op, "value": a.0})).collect();
    serde_json::from_value(serde_json::json!({"ruleSet": {"match": "all", "rules": rules}})).unwrap()
}

/// The same, with the albums in a group of the rules.
fn tests_in_a_group(album: AlbumId) -> Filter {
    serde_json::from_value(serde_json::json!({"ruleSet": {"match": "all", "rules": [
        {"field": "rating", "op": "is", "value": 3},
        {"group": {"match": "any", "rules": [{"field": "album", "op": "is", "value": album.0}]}}
    ]}}))
    .unwrap()
}

fn set_rules(id: AlbumId, rules: Filter) -> Op {
    Op::SetAlbumRules { id, rules: Box::new(rules) }
}

fn rated(rating: u8) -> Filter {
    Filter { rating, ..Default::default() }
}

/// What the albums are: the whole library as saved.
fn saved(c: &Catalog) -> String {
    c.to_snapshot()
}

#[test]
fn a_new_edit_that_would_make_a_loop_is_refused() {
    let mut c = Catalog::new();
    let a = smart(&mut c, "A", rated(1));
    let b = smart(&mut c, "B", tests(&[("is", a)]));
    let chain = smart(&mut c, "Chain", tests(&[("isNot", b)]));
    let (x, y) = (smart(&mut c, "X", rated(2)), smart(&mut c, "Y", rated(3)));
    // F tests E; E is removed, so F tests an album that is not there (a saved rule may)
    let e = smart(&mut c, "E", rated(4));
    let f = smart(&mut c, "F", tests(&[("is", e)]));
    c.apply(Op::RemoveAlbum { id: e }).unwrap();
    let (before, revision) = (saved(&c), c.revision);
    let refused: Vec<(&str, Op, &str)> = vec![
        ("tests an album that tests it", set_rules(a, tests(&[("is", b)])), "A"),
        ("tests itself", set_rules(a, tests(&[("is", a)])), "A"),
        ("tests itself through a group", set_rules(a, tests_in_a_group(a)), "A"),
        ("tests the end of a chain that leads to it", set_rules(a, tests(&[("isNot", chain)])), "A"),
        ("limited to itself by its album field", set_rules(a, Filter { album: Some(a), ..Default::default() }), "A"),
        ("two edits, each alone without a loop", Op::Batch { ops: vec![set_rules(x, tests(&[("is", y)])), set_rules(y, tests(&[("is", x)]))] }, "X"),
        ("a rename and a loop", Op::Batch { ops: vec![Op::RenameAlbum { id: a, name: "Renamed".into() }, set_rules(a, tests(&[("is", b)]))] }, "A"),
        (
            "a batch in a batch",
            Op::Batch {
                ops: vec![Op::Batch { ops: vec![set_rules(x, tests(&[("is", y)]))] }, Op::Batch { ops: vec![set_rules(y, tests(&[("is", x)]))] }],
            },
            "X",
        ),
        (
            "a new album that takes the id an album was tested by",
            Op::AddAlbum { album: Album { smart: Some(Box::new(tests(&[("is", f)]))), ..Album::new(e, "Back") } },
            "Back",
        ),
        (
            "a new album and its own rules in one batch",
            Op::Batch {
                ops: vec![
                    Op::AddAlbum { album: Album { smart: Some(Box::new(rated(1))), ..Album::new(AlbumId(90), "Fresh") } },
                    set_rules(AlbumId(90), tests(&[("is", AlbumId(90))])),
                ],
            },
            "Fresh",
        ),
    ];
    for (what, op, name) in refused {
        let e = c.apply_new(op).unwrap_err().to_string();
        assert!(e.contains(&format!("“{name}”")) && e.contains("include itself"), "{what}: {e}");
        assert_eq!(saved(&c), before, "{what}: nothing changed");
        assert_eq!(c.revision, revision, "{what}: not even looked at as changed");
    }
}

#[test]
fn a_new_edit_that_makes_no_loop_goes_through() {
    let mut c = Catalog::new();
    let a = smart(&mut c, "A", rated(1));
    let b = smart(&mut c, "B", tests(&[("is", a)]));
    let list = plain(&mut c, "List");
    let c1 = smart(&mut c, "C", tests(&[("isNot", b)]));
    let allowed = vec![
        ("a chain gets longer", set_rules(a, rated(2))),
        ("testing a plain album", set_rules(a, tests(&[("is", list)]))),
        ("a smart album testing another, the other way round", set_rules(c1, tests(&[("is", a), ("is", b)]))),
        (
            "a new smart album testing others",
            Op::AddAlbum { album: Album { smart: Some(Box::new(tests(&[("is", c1)]))), ..Album::new(AlbumId(50), "New") } },
        ),
        ("a new plain album", Op::AddAlbum { album: Album::new(AlbumId(51), "Plain") }),
        ("a rename", Op::RenameAlbum { id: b, name: "B2".into() }),
        ("a removal", Op::RemoveAlbum { id: c1 }),
        ("photos of a plain album", Op::SetAlbumPhotos { id: list, photos: vec![] }),
        ("the album field of the rules", set_rules(b, Filter { album: Some(list), ..Default::default() })),
        (
            "a batch of edits that don't meet",
            Op::Batch { ops: vec![set_rules(a, rated(3)), Op::RenameAlbum { id: a, name: "A3".into() }, set_rules(b, tests(&[("is", a)]))] },
        ),
        (
            "a batch that passes through a loop and ends without one",
            Op::Batch { ops: vec![set_rules(a, tests(&[("is", b)])), set_rules(b, rated(1))] },
        ),
    ];
    for (what, op) in allowed {
        let revision = c.revision;
        let undo = c.apply_new(op).unwrap_or_else(|e| panic!("{what}: {e}"));
        assert_eq!(c.revision, revision + 1, "{what}");
        assert!(c.albums().all(|al| !c.smart_album_problems(al.id).iter().any(|p| p.issue == rules::Issue::AlbumLoop)), "{what}: no loop");
        // and it is undone by what it returned
        c.apply(undo).unwrap();
    }
}

/// The refusal is the only difference between the two ways in: an edit that makes no loop does
/// the same through either.
#[test]
fn apply_new_changes_what_apply_changes() {
    let mut a = Catalog::new();
    let first = smart(&mut a, "A", rated(1));
    let second = smart(&mut a, "B", tests(&[("is", first)]));
    let mut b = Catalog::from_snapshot(&saved(&a)).unwrap();
    let ops = vec![set_rules(first, rated(4)), Op::RenameAlbum { id: second, name: "B2".into() }, set_rules(second, tests(&[("isNot", first)]))];
    for op in ops {
        let (ia, ib) = (a.apply(op.clone()).unwrap(), b.apply_new(op).unwrap());
        assert_eq!(Catalog::op_to_log_line(&ia), Catalog::op_to_log_line(&ib));
    }
    assert_eq!(saved(&a), saved(&b));
}

/// A library saved by a version without the rule: it may hold a loop. It opens (a snapshot, a
/// replayed log), its albums still list and count, edits that make no loop go through, also
/// those to the albums on the loop, and an edit that closes another loop is still refused.
#[test]
fn a_library_that_already_holds_a_loop_opens_and_can_be_mended() {
    let mut old = Catalog::new();
    let mut log = String::new();
    let mut record = |c: &mut Catalog, op: Op| {
        log.push_str(&Catalog::op_to_log_line(&op));
        c.apply(op).unwrap();
    };
    let (first, second, other, third) = (AlbumId(1), AlbumId(2), AlbumId(3), AlbumId(4));
    record(&mut old, Op::AddAlbum { album: Album { smart: Some(Box::default()), ..Album::new(first, "First") } });
    record(&mut old, Op::AddAlbum { album: Album { smart: Some(Box::new(tests(&[("is", first)]))), ..Album::new(second, "Second") } });
    record(&mut old, set_rules(first, tests(&[("is", second)])));
    record(&mut old, Op::AddAlbum { album: Album { smart: Some(Box::default()), ..Album::new(other, "Other") } });
    record(&mut old, Op::AddAlbum { album: Album { smart: Some(Box::new(tests(&[("is", other)]))), ..Album::new(third, "Third") } });
    let mut c = Catalog::new();
    assert_eq!(c.replay(&log).unwrap(), 5, "the log replays, loop and all");
    let mut from_snapshot = Catalog::from_snapshot(&c.to_snapshot()).unwrap();
    for lib in [&mut c, &mut from_snapshot] {
        // they still answer
        for id in [first, second, other, third] {
            let _ = (lib.album_photos(id), lib.album_count(id), lib.smart_album_problems(id));
        }
        // edits that make no loop go through: elsewhere, and to the albums on the loop
        lib.apply_new(Op::RenameAlbum { id: first, name: "First, renamed".into() }).unwrap();
        lib.apply_new(set_rules(other, rated(2))).unwrap();
        lib.apply_new(set_rules(third, tests(&[("isNot", other)]))).unwrap();
        lib.apply_new(Op::AddAlbum { album: Album { smart: Some(Box::new(tests(&[("is", second)]))), ..Album::new(AlbumId(60), "Reads the loop") } })
            .unwrap();
        lib.apply_new(set_rules(first, Filter { rating: 3, rule_set: tests(&[("is", second)]).rule_set, ..Default::default() })).unwrap();
        // not one that closes another
        let e = lib.apply_new(set_rules(other, tests(&[("is", third)]))).unwrap_err().to_string();
        assert!(e.contains("“Other”") && e.contains("include itself"), "{e}");
        // mended by an edit that makes no loop
        lib.apply_new(set_rules(second, rated(1))).unwrap();
        assert!(!lib.smart_album_problems(first).iter().any(|p| p.issue == rules::Issue::AlbumLoop), "mended");
        assert!(!lib.smart_album_problems(second).iter().any(|p| p.issue == rules::Issue::AlbumLoop), "mended");
    }
}

/// What an album on a loop holds is whatever it held before the refusal existed: the rule keeps
/// new loops out and decides nothing about the photos of old ones.
#[test]
fn what_an_album_on_a_loop_holds_is_left_as_it_was() {
    let mut c = Catalog::new();
    let id = c.alloc_photo_id();
    c.apply(Op::AddPhoto { photo: Box::new(Photo::new(id, Source::Demo { scene: 1 }, "p.jpg", "JPEG", 60, 40, "2026-02-01T10:00:00")) }).unwrap();
    c.apply(Op::SetRating { id, rating: 3 }).unwrap();
    let first = smart(&mut c, "First", Filter::default());
    let second = smart(&mut c, "Second", tests(&[("is", first)]));
    c.apply(set_rules(first, tests(&[("is", second)]))).unwrap();
    let held = |c: &Catalog| [first, second].map(|a| (c.album_photos(a), c.album_count(a)));
    let looped = held(&c);
    // edits that go through the check, one of them to an album on the loop
    c.apply_new(set_rules(first, Filter { rule_set: tests(&[("is", second)]).rule_set, ..Default::default() })).unwrap();
    let id = c.alloc_album_id();
    c.apply_new(Op::AddAlbum { album: Album { smart: Some(Box::new(tests(&[("is", second)]))), ..Album::new(id, "Reads the loop") } }).unwrap();
    assert_eq!(held(&c), looped);
}

/// The search for loops walks the albums with its own stack: a ring of thousands of smart albums
/// is found, not overflowed.
#[test]
fn a_very_long_ring_is_refused_without_running_out_of_stack() {
    const N: usize = 20_000;
    let mut c = Catalog::new();
    let ids: Vec<AlbumId> = (0..N).map(|i| smart(&mut c, &format!("S{i}"), Filter::default())).collect();
    // each tests the next; the last one is left open
    for pair in ids.windows(2) {
        c.apply(set_rules(pair[0], tests(&[("is", pair[1])]))).unwrap();
    }
    let (first, last) = (ids[0], ids[N - 1]);
    let e = c.apply_new(set_rules(last, tests(&[("is", first)]))).unwrap_err().to_string();
    assert!(e.contains("include itself"), "{e}");
    // and what makes no ring goes through
    c.apply_new(set_rules(last, rated(5))).unwrap();
}
