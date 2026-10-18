//! Feature: the History panel has a cursor. Going to a step loads its settings without adding a
//! step; later steps stay listed until a new edit drops them; undo/redo stay coherent.

use serde_json::json;

use crate::Session;

fn exposure(s: &Session) -> f64 {
    s.develop_of(s.active().unwrap()).unwrap().light.exposure
}

fn list(s: &mut Session) -> (usize, Option<usize>) {
    let h = s.execute("history.list", &json!({})).unwrap();
    (h["history"].as_array().unwrap().len(), h["cursor"].as_u64().map(|c| c as usize))
}

/// A demo session with three exposure edits (1, 2, 3) as its latest History steps.
fn three_edits() -> (Session, usize) {
    let mut s = Session::with_demo();
    for v in [1.0, 2.0, 3.0] {
        s.execute("develop.set", &json!({"control": "light.exposure", "value": v})).unwrap();
    }
    let (n, cursor) = list(&mut s);
    assert_eq!(cursor, Some(n - 1), "the cursor starts at the latest step");
    (s, n)
}

#[test]
fn going_back_adds_no_step_and_forward_returns() {
    let (mut s, n) = three_edits();
    s.execute("history.restore", &json!({"index": n - 3})).unwrap();
    assert_eq!(exposure(&s), 1.0);
    assert_eq!(list(&mut s), (n, Some(n - 3)), "no new step; later steps kept");
    s.execute("history.next", &json!({})).unwrap();
    assert_eq!(exposure(&s), 2.0);
    s.execute("history.restore", &json!({"index": n - 1})).unwrap();
    assert_eq!(exposure(&s), 3.0);
    assert_eq!(list(&mut s), (n, Some(n - 1)));
    assert!(s.execute("history.next", &json!({})).is_err(), "nothing after the latest step");
    s.execute("history.previous", &json!({})).unwrap();
    assert_eq!((exposure(&s), list(&mut s)), (2.0, (n, Some(n - 2))));
    assert!(s.execute("history.restore", &json!({"index": n})).is_err());
    assert!(s.execute("history.restore", &json!({"index": -1})).is_err());
}

#[test]
fn a_new_edit_behind_the_end_drops_the_later_steps() {
    let (mut s, n) = three_edits();
    s.execute("history.restore", &json!({"index": n - 3})).unwrap();
    s.execute("develop.set", &json!({"control": "light.exposure", "value": -1.0})).unwrap();
    assert_eq!(list(&mut s), (n - 1, Some(n - 2)), "steps 2 and 3 dropped, the new one appended");
    assert_eq!(exposure(&s), -1.0);
    // undo brings the dropped steps and the cursor back
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!((exposure(&s), list(&mut s)), (1.0, (n, Some(n - 3))));
    s.execute("edit.redo", &json!({})).unwrap();
    assert_eq!((exposure(&s), list(&mut s)), (-1.0, (n - 1, Some(n - 2))));
}

#[test]
fn undo_and_redo_move_the_cursor_with_the_settings() {
    let (mut s, n) = three_edits();
    s.execute("history.restore", &json!({"index": n - 3})).unwrap();
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!((exposure(&s), list(&mut s)), (3.0, (n, Some(n - 1))), "undoing a step move");
    s.execute("edit.redo", &json!({})).unwrap();
    assert_eq!((exposure(&s), list(&mut s)), (1.0, (n, Some(n - 3))));
}

#[test]
fn clearing_history_resets_the_cursor() {
    let (mut s, n) = three_edits();
    s.execute("history.restore", &json!({"index": n - 2})).unwrap();
    s.execute("history.clear", &json!({})).unwrap();
    assert_eq!((exposure(&s), list(&mut s)), (2.0, (1, Some(0))));
    assert_eq!(s.catalog.photo(s.active().unwrap()).unwrap().history_cursor, None);
}

#[test]
fn the_cursor_survives_the_op_log_and_old_catalogs_load_at_the_end() {
    let mut s = Session::with_demo();
    let snap = s.catalog.to_snapshot();
    let _ = s.drain_log();
    for v in [1.0, 2.0] {
        s.execute("develop.set", &json!({"control": "light.exposure", "value": v})).unwrap();
    }
    let (n, _) = list(&mut s);
    s.execute("history.restore", &json!({"index": n - 2})).unwrap();
    let log: String = s.drain_log().iter().map(lightcraft_catalog::Catalog::op_to_log_line).collect();
    let mut c = lightcraft_catalog::Catalog::from_snapshot(&snap).unwrap();
    c.replay(&log).unwrap();
    assert_eq!(c.to_snapshot(), s.catalog.to_snapshot());
    let id = s.active().unwrap();
    assert_eq!(c.photo(id).unwrap().history_cursor, Some(n - 2));
    // a photo written before the cursor existed has no `history_cursor` key: at the latest step
    let mut v = serde_json::to_value(c.photo(id).unwrap()).unwrap();
    v.as_object_mut().unwrap().remove("history_cursor");
    let old: lightcraft_catalog::Photo = serde_json::from_value(v).unwrap();
    assert_eq!((old.history_cursor, old.history_index()), (None, Some(n - 1)));
}

/// A negative `index` must be an error on every indexed command, not element 0 (`-1.0 as usize`
/// saturates to 0): `history.restore` guards it, `version.*` (and the other `index` commands)
/// must too. A caller sending `index: -1` used to delete or restore version 0 silently.
#[test]
fn negative_indices_are_refused() {
    let mut s = Session::with_demo();
    let id = s.active().unwrap();
    let versions = |s: &Session| s.catalog.photo(id).unwrap().versions.len();
    s.execute("version.create", &json!({"name": "A"})).unwrap();
    assert_eq!(versions(&s), 1);
    assert!(s.execute("version.delete", &json!({"index": -1})).is_err(), "must not delete version 0");
    assert_eq!(versions(&s), 1, "nothing was deleted");
    assert!(s.execute("version.restore", &json!({"index": -1})).is_err());
    assert!(s.execute("version.rename", &json!({"index": -1, "name": "B"})).is_err());
    assert_eq!(s.catalog.photo(id).unwrap().versions[0].name, "A", "version 0 was not renamed");
}
