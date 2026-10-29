//! Ordering albums by hand: `album.reorder` puts an album (or folder) before a sibling, or last of
//! its kind, in one undo step, also when it changes folder; `album.sort` goes back to by name.

use lightcraft_catalog::AlbumId;
use serde_json::{Value, json};

use crate::Session;

fn make(s: &mut Session, name: &str, parent: Option<u64>, folder: bool) -> u64 {
    let mut p = json!({"name": name, "folder": folder});
    if let Some(parent) = parent {
        p["parent"] = json!(parent);
    }
    s.execute("album.create", &p).unwrap()["id"].as_u64().unwrap()
}

fn names(s: &Session, parent: Option<u64>) -> Vec<String> {
    s.catalog.album_children(parent.map(AlbumId)).iter().map(|a| a.name.clone()).collect()
}

fn reorder(s: &mut Session, id: u64, params: Value) -> Result<Value, String> {
    let mut p = params;
    p["id"] = json!(id);
    s.execute("album.reorder", &p).map_err(|e| e.to_string())
}

/// Given three albums listed by name, when one is placed before another, then the list follows;
/// "no sibling" puts it last (among albums).
#[test]
fn an_album_is_placed_before_a_sibling_or_last() {
    let mut s = Session::new();
    let (a, b, c) = (make(&mut s, "A", None, false), make(&mut s, "B", None, false), make(&mut s, "C", None, false));
    reorder(&mut s, c, json!({"before": a})).unwrap();
    assert_eq!(names(&s, None), ["C", "A", "B"]);
    assert!(s.catalog.album_children_are_ordered(None));
    reorder(&mut s, c, json!({"before": null})).unwrap();
    assert_eq!(names(&s, None), ["A", "B", "C"]);
    reorder(&mut s, a, json!({})).unwrap();
    assert_eq!(names(&s, None), ["B", "C", "A"], "no `before`: last");
    assert_eq!(reorder(&mut s, b, json!({"before": b})).unwrap()["changed"], 0, "before itself is where it is");
    assert_eq!(names(&s, None), ["B", "C", "A"]);
}

/// Folders stay ahead of albums: a folder is placed among folders, an album among albums.
#[test]
fn folders_and_albums_keep_to_their_own_group() {
    let mut s = Session::new();
    let (f1, f2) = (make(&mut s, "F1", None, true), make(&mut s, "F2", None, true));
    let (a1, a2) = (make(&mut s, "A1", None, false), make(&mut s, "A2", None, false));
    reorder(&mut s, f2, json!({"before": f1})).unwrap();
    reorder(&mut s, a2, json!({"before": a1})).unwrap();
    assert_eq!(names(&s, None), ["F2", "F1", "A2", "A1"]);
    reorder(&mut s, f1, json!({"before": a1})).expect_err("a folder is not placed among albums");
    reorder(&mut s, a1, json!({"before": f1})).expect_err("an album is not placed among folders");
    reorder(&mut s, f2, json!({"before": null})).unwrap();
    assert_eq!(names(&s, None), ["F1", "F2", "A2", "A1"], "last of the folders");
}

/// Placing an album in another folder moves it there, and one undo undoes both.
#[test]
fn placing_across_folders_moves_it_and_undoes_in_one_step() {
    let mut s = Session::new();
    let folder = make(&mut s, "Trips", None, true);
    let (x, y) = (make(&mut s, "X", Some(folder), false), make(&mut s, "Y", Some(folder), false));
    let loose = make(&mut s, "Loose", None, false);
    reorder(&mut s, y, json!({"before": x})).unwrap();
    assert_eq!(names(&s, Some(folder)), ["Y", "X"]);
    reorder(&mut s, loose, json!({"parent": folder, "before": x})).unwrap();
    assert_eq!(names(&s, Some(folder)), ["Y", "Loose", "X"]);
    assert_eq!(s.catalog.album(AlbumId(loose)).unwrap().parent, Some(AlbumId(folder)));
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(names(&s, Some(folder)), ["Y", "X"]);
    assert_eq!(s.catalog.album(AlbumId(loose)).unwrap().parent, None);
    assert_eq!(s.catalog.album(AlbumId(loose)).unwrap().order, None);
}

/// Undo and redo of placing across folders, of sorting, and of moving a folder that has children.
#[test]
fn placing_and_sorting_undo_and_redo() {
    let mut s = Session::new();
    let top = make(&mut s, "Top", None, true);
    let child = make(&mut s, "Child", Some(top), false);
    let (a, b) = (make(&mut s, "A", None, false), make(&mut s, "B", None, false));
    reorder(&mut s, b, json!({"before": a})).unwrap();
    // a folder with children, placed inside another folder
    let other = make(&mut s, "Other", None, true);
    reorder(&mut s, top, json!({"parent": other})).unwrap();
    assert_eq!(names(&s, Some(other)), ["Top"]);
    assert_eq!(names(&s, Some(top)), ["Child"], "its children came along");
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.catalog.album(AlbumId(top)).unwrap().parent, None);
    s.execute("edit.redo", &json!({})).unwrap();
    assert_eq!(s.catalog.album(AlbumId(top)).unwrap().parent, Some(AlbumId(other)));
    // sort, undo, redo
    s.execute("album.sort", &json!({})).unwrap();
    assert_eq!(names(&s, None), ["Other", "A", "B"]);
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(names(&s, None), ["Other", "B", "A"], "the hand order is back");
    s.execute("edit.redo", &json!({})).unwrap();
    assert_eq!(names(&s, None), ["Other", "A", "B"]);
    assert_eq!(s.catalog.album(AlbumId(child)).unwrap().parent, Some(AlbumId(top)));
}

/// Bad requests change nothing: a `before` that is not in the target folder, a target that is not
/// a folder or lies inside the album, an unknown album.
#[test]
fn bad_placements_are_refused_and_change_nothing() {
    let mut s = Session::new();
    let f = make(&mut s, "F", None, true);
    let inner = make(&mut s, "Inner", Some(f), true);
    let (a, b) = (make(&mut s, "A", None, false), make(&mut s, "B", Some(f), false));
    let before = s.catalog.to_snapshot();
    reorder(&mut s, a, json!({"before": b})).expect_err("B is in F, not at the top level");
    reorder(&mut s, a, json!({"parent": b})).expect_err("a plain album is no folder");
    reorder(&mut s, f, json!({"parent": inner})).expect_err("a folder into itself");
    reorder(&mut s, 987_654, json!({})).expect_err("no such album");
    reorder(&mut s, a, json!({"before": 987_654})).expect_err("no such sibling");
    assert_eq!(s.catalog.to_snapshot(), before, "nothing changed");
}

/// `album.sort` drops the hand order of one folder (and only that one): by name again.
#[test]
fn sorting_goes_back_to_by_name_for_one_folder() {
    let mut s = Session::new();
    let f = make(&mut s, "F", None, true);
    let (x, y) = (make(&mut s, "X", Some(f), false), make(&mut s, "Y", Some(f), false));
    let (a, b) = (make(&mut s, "A", None, false), make(&mut s, "B", None, false));
    reorder(&mut s, y, json!({"before": x})).unwrap();
    reorder(&mut s, b, json!({"before": a})).unwrap();
    assert_eq!(names(&s, Some(f)), ["Y", "X"]);
    assert_eq!(names(&s, None), ["F", "B", "A"]);
    let r = s.execute("album.sort", &json!({"parent": f})).unwrap();
    assert_eq!(r["changed"], 2);
    assert_eq!(names(&s, Some(f)), ["X", "Y"]);
    assert_eq!(names(&s, None), ["F", "B", "A"], "the top level keeps its order");
    assert!(!s.catalog.album_children_are_ordered(Some(AlbumId(f))));
    // nothing to do: no undo step is made
    let r = s.execute("album.sort", &json!({"parent": f})).unwrap();
    assert_eq!(r["changed"], 0);
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(names(&s, Some(f)), ["Y", "X"], "the one undo step was the sort");
    let r = s.execute("album.sort", &json!({})).unwrap();
    assert_eq!(r["changed"], 3, "every child of the top level had a place: F, B and A");
    assert_eq!(names(&s, None), ["F", "A", "B"]);
}

/// Moving an album with `album.move` out of an ordered folder leaves the old neighbours' order.
#[test]
fn moving_out_of_an_ordered_folder_keeps_the_others_in_place() {
    let mut s = Session::new();
    let f = make(&mut s, "F", None, true);
    let (x, y, z) = (make(&mut s, "X", Some(f), false), make(&mut s, "Y", Some(f), false), make(&mut s, "Z", Some(f), false));
    reorder(&mut s, z, json!({"before": x})).unwrap();
    assert_eq!(names(&s, Some(f)), ["Z", "X", "Y"]);
    s.execute("album.move", &json!({"id": x, "parent": null})).unwrap();
    assert_eq!(names(&s, Some(f)), ["Z", "Y"]);
    assert_eq!(names(&s, None), ["F", "X"]);
    let _ = y;
}

/// Given albums chosen together in other folders, when one of them is placed before a sibling with the others named
/// in `with`, then it lands there and the others come to the same folder, as one undo step.
#[test]
fn placing_one_of_a_group_moves_the_others_along_in_one_step() {
    let mut s = Session::new();
    let f = make(&mut s, "F", None, true);
    let (x, y) = (make(&mut s, "X", Some(f), false), make(&mut s, "Y", Some(f), false));
    let (a, b) = (make(&mut s, "A", None, false), make(&mut s, "B", None, false));
    let undo = s.undo.len();
    reorder(&mut s, y, json!({"parent": null, "before": b, "with": [x, y]})).unwrap();
    let top = names(&s, None);
    let at = |n: &str| top.iter().position(|x| x == n).unwrap();
    assert_eq!(at("Y") + 1, at("B"), "Y is placed before B: {top:?}");
    assert!(top.contains(&"X".to_string()), "X came along: {top:?}");
    assert!(names(&s, Some(f)).is_empty());
    assert_eq!(s.undo.len(), undo + 1, "one step");
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(names(&s, Some(f)), ["X", "Y"], "one undo puts both back");
    assert_eq!(names(&s, None), ["F", "A", "B"]);
    let _ = a;
}

/// A group that is already in the folder is not moved again, and the placement is all that is left.
#[test]
fn placing_one_of_a_group_that_is_there_already_only_places_it() {
    let mut s = Session::new();
    let (a, b, c) = (make(&mut s, "A", None, false), make(&mut s, "B", None, false), make(&mut s, "C", None, false));
    reorder(&mut s, c, json!({"before": a, "with": [a, c]})).unwrap();
    assert_eq!(names(&s, None), ["C", "A", "B"]);
    let _ = b;
}

#[test]
fn placing_a_group_refuses_what_a_move_refuses_and_changes_nothing() {
    let mut s = Session::new();
    let f = make(&mut s, "F", None, true);
    let g = make(&mut s, "G", Some(f), true);
    let a = make(&mut s, "A", None, false);
    let undo = s.undo.len();
    reorder(&mut s, a, json!({"parent": g, "with": [a, f]})).expect_err("a folder can't go inside what is inside it");
    reorder(&mut s, a, json!({"with": [a, 987_654]})).expect_err("an album that is gone");
    assert_eq!((s.undo.len(), names(&s, None)), (undo, vec!["F".to_string(), "A".to_string()]));
}

#[test]
fn a_bad_with_is_reported_under_its_own_name() {
    let mut s = Session::new();
    let a = make(&mut s, "A", None, false);
    let e = reorder(&mut s, a, json!({"with": 3})).expect_err("`with` is a list");
    assert!(e.contains("`with` must be a list of album ids"), "{e}");
    let e = reorder(&mut s, a, json!({"with": ["x"]})).expect_err("a list of ids");
    assert!(e.contains("`with`") && !e.contains("`ids`"), "{e}");
}
