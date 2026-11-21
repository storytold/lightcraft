//! Photo ▸ Delete Rejected Photos (⌘⌫ / Ctrl+Backspace, as in Lightroom Classic): the view's rejects go to
//! Recently Deleted in one undo step; picks, unflagged photos and rejects the filter hides stay.
use lightcraft_catalog::{Flag, Op, Photo, PhotoId, Source};
use serde_json::json;

use crate::{Result, Session};

/// Photos 1..=6: 2 and 4 rejected, 5 picked, 6 rejected but given one star.
fn library() -> Result<Session> {
    let mut s = Session::new();
    for id in 1..=6 {
        let mut photo =
            Photo::new(PhotoId(id), Source::File { path: format!("/lightcraft-rejects/{id}.jpg") }, &format!("{id}.jpg"), "JPEG", 40, 30, "2026-11-14");
        photo.flag = match id {
            2 | 4 | 6 => Flag::Reject,
            5 => Flag::Pick,
            _ => Flag::None,
        };
        photo.rating = u8::from(id == 6);
        s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) })?;
    }
    s.execute("library.sort", &json!({"key": "fileName", "ascending": true}))?;
    Ok(s)
}

fn deleted(s: &Session) -> Vec<u64> {
    s.catalog.photos().filter(|p| p.deleted).map(|p| p.id.0).collect()
}

#[test]
fn rejects_go_to_recently_deleted_in_one_undo_step() {
    let mut s = library().unwrap();
    let r = s.execute("library.deleteRejected", &json!({})).unwrap();
    assert_eq!(r["changed"], 3);
    let mut d = deleted(&s);
    d.sort_unstable();
    assert_eq!(d, vec![2, 4, 6], "only the rejects, never a pick or an unflagged photo");
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(deleted(&s).is_empty(), "one undo brings them all back");
}

#[test]
fn rejects_the_filter_hides_stay() {
    let mut s = library().unwrap();
    s.execute("library.filter", &json!({"rating": 1, "ratingOp": "atLeast"})).unwrap();
    let r = s.execute("library.deleteRejected", &json!({})).unwrap();
    assert_eq!(r["changed"], 1);
    assert_eq!(deleted(&s), vec![6]);
}

#[test]
fn no_rejects_is_no_undo_step() {
    let mut s = library().unwrap();
    s.execute("library.deleteRejected", &json!({})).unwrap();
    let r = s.execute("library.deleteRejected", &json!({})).unwrap();
    assert_eq!(r["changed"], 0, "the rejects already in Recently Deleted are not counted again");
}

#[test]
fn the_default_shortcut_is_cmd_delete() {
    let spec = crate::cmd::find_command("library.deleteRejected").unwrap();
    assert_eq!(spec.shortcut, Some("Cmd+Delete"));
}
