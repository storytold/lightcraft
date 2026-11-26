//! A smart album never comes to include itself by a new edit, whatever makes the edit
//! (`Session::commit` keeps the catalog's rule, `Catalog::apply_new`); a library that already
//! holds such a loop opens, works and can be mended, and undo and redo move across it freely.
//!
//! Scenarios:
//! - When an edit that no command checked (an op committed on its own, in a batch of edits that
//!   are fine one by one, or as a new album) would put a smart album on a loop, then it is
//!   refused, and there is nothing to undo or to journal.
//! - When commands make the edits, then every way to a loop stays refused and ordinary edits go
//!   on, one undo step each.
//! - Given a library that holds a loop, then edits that add none go through, and undo and redo
//!   bring the loop back and take it away without being refused.

use lightcraft_catalog::{Album, AlbumId, Filter, Op};
use serde_json::json;

use crate::Session;

fn tests(albums: &[(&str, u64)]) -> Filter {
    let rules: Vec<_> = albums.iter().map(|(op, a)| json!({"field": "album", "op": op, "value": a})).collect();
    serde_json::from_value(json!({"ruleSet": {"match": "all", "rules": rules}})).unwrap()
}

fn smart(s: &mut Session, name: &str, rules: serde_json::Value) -> u64 {
    s.execute("album.createSmart", &json!({"name": name, "rules": rules})).unwrap()["id"].as_u64().unwrap()
}

fn set(id: u64, rules: Filter) -> Op {
    Op::SetAlbumRules { id: AlbumId(id), rules: Box::new(rules) }
}

#[test]
fn an_edit_no_command_checked_cant_make_a_loop() {
    let mut s = Session::new();
    let a = smart(&mut s, "A", json!({"rating": 1}));
    let b = smart(&mut s, "B", json!({"ruleSet": {"rules": [{"field": "album", "op": "is", "value": a}]}}));
    let (x, y) = (smart(&mut s, "X", json!({"rating": 2})), smart(&mut s, "Y", json!({"rating": 3})));
    let gone = smart(&mut s, "Gone", json!({"rating": 4}));
    let waits = smart(&mut s, "Waits", json!({"ruleSet": {"rules": [{"field": "album", "op": "is", "value": gone}]}}));
    s.execute("album.delete", &json!({"id": gone})).unwrap();
    let (steps, logged, snapshot) = (s.undo.len(), s.pending_log.len(), s.catalog.to_snapshot());
    let back = Album { smart: Some(Box::new(tests(&[("is", waits)]))), ..Album::new(AlbumId(gone), "Back") };
    for (what, op) in [
        ("rules that test an album that tests it", set(a, tests(&[("is", b)]))),
        ("two edits, each fine alone", Op::Batch { ops: vec![set(x, tests(&[("is", y)])), set(y, tests(&[("is", x)]))] }),
        ("a new album that takes the id of one a rule tests", Op::AddAlbum { album: back }),
    ] {
        let e = s.commit("Edit", op).unwrap_err().to_string();
        assert!(e.contains("include itself"), "{what}: {e}");
        assert_eq!((s.undo.len(), s.pending_log.len()), (steps, logged), "{what}: nothing to undo, nothing journaled");
        assert_eq!(s.catalog.to_snapshot(), snapshot, "{what}: nothing changed");
    }
}

#[test]
fn commands_refuse_every_way_to_a_loop_and_ordinary_edits_go_on() {
    let mut s = Session::with_demo();
    let a = smart(&mut s, "A", json!({"rating": 1}));
    let b = smart(&mut s, "B", json!({"ruleSet": {"rules": [{"field": "album", "op": "is", "value": a}]}}));
    let steps = s.undo.len();
    let looping = |s: &mut Session, params: serde_json::Value| {
        let e = s.execute("album.setRules", &params).unwrap_err().to_string();
        assert!(e.contains("include itself"), "{e}");
    };
    looping(&mut s, json!({"id": a, "rules": {"ruleSet": {"rules": [{"field": "album", "op": "is", "value": b}]}}}));
    looping(&mut s, json!({"id": a, "name": "Renamed too", "rules": {"ruleSet": {"rules": [{"field": "album", "op": "is", "value": a}]}}}));
    assert_eq!((s.undo.len(), s.catalog.album(AlbumId(a)).map(|x| x.name.clone())), (steps, Some("A".into())));
    // ordinary edits: a rename together with new rules is one undo step
    s.execute("album.setRules", &json!({"id": b, "name": "B2", "rules": {"rating": 2}})).unwrap();
    s.execute("album.rename", &json!({"id": a, "name": "A2"})).unwrap();
    assert_eq!(s.undo.len(), steps + 2);
    s.execute("edit.undo", &json!({})).unwrap();
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.catalog.album(AlbumId(b)).map(|x| x.name.clone()), Some("B".into()));
}

#[test]
fn a_library_that_holds_a_loop_goes_on_and_undo_and_redo_cross_it() {
    let mut s = Session::with_demo();
    let a = smart(&mut s, "A", json!({"rating": 1}));
    let b = smart(&mut s, "B", json!({"ruleSet": {"rules": [{"field": "album", "op": "is", "value": a}]}}));
    // as a version without the rule saved it
    s.catalog.apply(set(a, tests(&[("is", b)]))).unwrap();
    let on_a_loop = |s: &Session| {
        [a, b]
            .into_iter()
            .filter(|id| s.catalog.smart_album_problems(AlbumId(*id)).iter().any(|p| p.issue == lightcraft_catalog::rules::Issue::AlbumLoop))
            .count()
    };
    assert_eq!(on_a_loop(&s), 2);
    // edits that add no loop: a rename, a change that leaves the rule as it is, a new album
    s.execute("album.rename", &json!({"id": a, "name": "Still looping"})).unwrap();
    s.execute("album.setRules", &json!({"id": a, "rules": {"rating": 3}})).unwrap();
    smart(&mut s, "Reads the loop", json!({"ruleSet": {"rules": [{"field": "album", "op": "is", "value": a}]}}));
    // mending it is one step; undo brings back what was and is not refused; redo mends again
    s.execute("album.setRules", &json!({"id": b, "replace": true, "rules": {"rating": 2}})).unwrap();
    assert_eq!(on_a_loop(&s), 0);
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(on_a_loop(&s), 2, "undo is no new edit");
    s.execute("edit.redo", &json!({})).unwrap();
    assert_eq!(on_a_loop(&s), 0);
}
