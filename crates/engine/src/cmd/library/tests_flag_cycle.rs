//! `photo.cycleFlag` / `photo.increaseFlag` / `photo.decreaseFlag` through the real engine.
use lightcraft_catalog::{Flag, Op, Photo, PhotoId, Source};
use serde_json::json;

use crate::{Result, Selection, Session};

fn session() -> Result<Session> {
    let mut s = Session::new();
    for (id, flag) in [(1, Flag::None), (2, Flag::Pick), (3, Flag::Reject), (4, Flag::None)] {
        let mut photo = Photo::new(
            PhotoId(id),
            Source::File { path: format!("/lightcraft-flag-cycle/{id}.jpg") },
            &format!("{id}.jpg"),
            "JPEG",
            40,
            30,
            "2026-10-13",
        );
        photo.flag = flag;
        s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) })?;
    }
    s.execute("library.sort", &json!({"key": "fileName", "ascending": true}))?;
    s.execute("library.select", &json!({"ids": [1, 2, 3], "active": 2}))?;
    Ok(s)
}

fn flags(s: &Session) -> Vec<Flag> {
    (1..=4).map(|id| s.catalog.photo(PhotoId(id)).unwrap().flag).collect()
}

#[test]
fn cycle_steps_unflagged_pick_reject_and_wraps() {
    let mut s = session().unwrap();
    s.execute("library.select", &json!({"ids": [1]})).unwrap();
    let seen: Vec<Flag> = (0..4)
        .map(|_| {
            s.execute("photo.cycleFlag", &json!({})).unwrap();
            flags(&s)[0]
        })
        .collect();
    assert_eq!(seen, [Flag::Pick, Flag::Reject, Flag::None, Flag::Pick]);
}

#[test]
fn cycle_uses_each_photos_own_flag_in_one_undo_step() {
    let mut s = session().unwrap();
    let undo = s.undo.len();
    assert_eq!(s.execute("photo.cycleFlag", &json!({})).unwrap()["changed"], 3);
    assert_eq!(flags(&s), [Flag::Pick, Flag::Reject, Flag::None, Flag::None]);
    assert_eq!(s.undo.len(), undo + 1);
    assert_eq!(s.active(), Some(PhotoId(2)));
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(flags(&s), [Flag::None, Flag::Pick, Flag::Reject, Flag::None]);
    s.execute("edit.redo", &json!({})).unwrap();
    assert_eq!(flags(&s), [Flag::Pick, Flag::Reject, Flag::None, Flag::None]);
}

#[test]
fn explicit_ids_active_fallback_advance_and_errors() {
    let mut s = session().unwrap();
    s.execute("photo.cycleFlag", &json!({"ids": [4]})).unwrap();
    assert_eq!(flags(&s), [Flag::None, Flag::Pick, Flag::Reject, Flag::Pick]);
    s.selection = Selection { ids: vec![], active: Some(PhotoId(2)) };
    s.execute("photo.cycleFlag", &json!({})).unwrap();
    assert_eq!(flags(&s)[1], Flag::Reject);
    s.execute("photo.cycleFlag", &json!({"advance": true})).unwrap();
    assert_eq!(flags(&s)[1], Flag::None);
    assert_eq!(s.active(), Some(PhotoId(3)));
    let undo = s.undo.len();
    assert!(s.execute("photo.cycleFlag", &json!({"ids": [1, 999]})).is_err());
    assert_eq!(flags(&s)[0], Flag::None);
    assert_eq!(s.undo.len(), undo);
    s.selection = Selection::default();
    assert!(s.execute("photo.cycleFlag", &json!({})).is_err());
    assert_eq!(s.undo.len(), undo);
}

#[test]
fn increase_and_decrease_clamp_without_wrapping() {
    let mut s = session().unwrap();
    assert_eq!(s.execute("photo.increaseFlag", &json!({})).unwrap()["changed"], 2);
    assert_eq!(flags(&s), [Flag::Pick, Flag::Pick, Flag::None, Flag::None]);
    assert_eq!(s.execute("photo.increaseFlag", &json!({"ids": [2]})).unwrap()["changed"], 0);
    s.execute("photo.decreaseFlag", &json!({})).unwrap();
    assert_eq!(flags(&s), [Flag::None, Flag::None, Flag::Reject, Flag::None]);
    s.execute("photo.decreaseFlag", &json!({})).unwrap();
    assert_eq!(flags(&s), [Flag::Reject, Flag::Reject, Flag::Reject, Flag::None]);
    let undo = s.undo.len();
    assert_eq!(s.execute("photo.decreaseFlag", &json!({})).unwrap()["changed"], 0);
    assert_eq!(s.undo.len(), undo, "no change, no history step");
}

#[test]
fn registered_with_classic_shortcuts() {
    let specs = crate::command_specs();
    let key = |id: &str| specs.iter().find(|c| c.id == id).and_then(|c| c.shortcut);
    assert_eq!(key("photo.cycleFlag"), Some("`"));
    assert_eq!(key("photo.increaseFlag"), Some("Cmd+Up"));
    assert_eq!(key("photo.decreaseFlag"), Some("Cmd+Down"));
}
