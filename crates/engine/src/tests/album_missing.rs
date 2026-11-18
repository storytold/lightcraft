//! Album commands only take photos the catalog has, and a call that changes nothing records
//! nothing.
//!
//! Scenarios:
//! - Given ids the catalog doesn't have (named outright, or still selected after an undo took the
//!   photos away), when they are added to an album, toggled into the target album, or used for a
//!   new album, then they reach no album.
//! - Given nothing is left to add, or every photo is in the album already, then there is no undo
//!   step and redo is left alone.
//! - Given a normal add or toggle, then one step is recorded and one undo restores the album.

use lightcraft_catalog::{AlbumId, PhotoId};
use serde_json::json;

use crate::{Selection, Session};

const GONE: PhotoId = PhotoId(987_654);

fn album(s: &mut Session, name: &str) -> AlbumId {
    AlbumId(s.execute("album.create", &json!({"name": name})).unwrap()["id"].as_u64().unwrap())
}

/// A demo session, an album, and a redo entry waiting (a rating set, then undone).
fn with_redo() -> (Session, AlbumId, PhotoId) {
    let mut s = Session::with_demo();
    let photo = s.catalog.photos().next().unwrap().id;
    let a = album(&mut s, "Trip");
    s.execute("photo.rate", &json!({"ids": [photo.0], "rating": 4})).unwrap();
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.redo.len(), 1);
    (s, a, photo)
}

#[test]
fn photos_the_catalog_lacks_reach_no_album() {
    let (mut s, a, photo) = with_redo();
    let (undo, redo) = (s.undo.len(), s.redo.len());
    let r = s.execute("album.addPhotos", &json!({"id": a.0, "ids": [GONE.0]})).unwrap();
    assert_eq!((r["added"].as_u64(), r["skipped"].as_u64()), (Some(0), Some(0)), "{r}");
    assert!(s.catalog.album(a).unwrap().photos.is_empty());
    assert_eq!((s.undo.len(), s.redo.len()), (undo, redo), "no step, redo kept");

    // a mix: the one that is there goes in, the other doesn't
    let r = s.execute("album.addPhotos", &json!({"id": a.0, "ids": [GONE.0, photo.0]})).unwrap();
    assert_eq!(r["added"], 1, "{r}");
    assert_eq!(s.catalog.album(a).unwrap().photos, vec![photo]);
    assert_eq!(s.undo.len(), undo + 1);
    assert_eq!(s.redo.len(), 0, "a real change clears redo");

    // toggling: nothing is made or changed for ids that are gone
    let r = s.execute("album.toggleTarget", &json!({"ids": [GONE.0]})).unwrap();
    assert!(r["added"].is_null() && r["toggled"] == 0, "{r}");
    assert!(s.catalog.quick_collection().is_none(), "no Quick Collection for nothing");
    assert_eq!(s.undo.len(), undo + 1);

    // a new album from a selection that is gone starts empty
    s.selection = Selection::single(GONE);
    let r = s.execute("album.create", &json!({"name": "New", "addSelected": true})).unwrap();
    let new = s.catalog.album(AlbumId(r["id"].as_u64().unwrap())).unwrap();
    assert!(new.photos.is_empty() && new.cover.is_none(), "{r}");
}

/// Leaves one redo entry waiting: a rating set, then undone.
fn leave_redo(s: &mut Session, photo: PhotoId) {
    s.execute("photo.rate", &json!({"ids": [photo.0], "rating": 4})).unwrap();
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.redo.len(), 1);
}

#[test]
fn toggling_gone_photos_leaves_the_target_album_and_redo_alone() {
    let mut s = Session::with_demo();
    let photo = s.catalog.photos().next().unwrap().id;
    let a = album(&mut s, "Trip");
    s.execute("album.setTarget", &json!({"id": a.0})).unwrap();
    s.execute("album.toggleTarget", &json!({"ids": [photo.0]})).unwrap();
    leave_redo(&mut s, photo);
    let undo = s.undo.len();
    let r = s.execute("album.toggleTarget", &json!({"ids": [GONE.0]})).unwrap();
    assert_eq!(r["album"].as_u64(), Some(a.0), "{r}");
    assert!(r["added"].is_null() && r["toggled"] == 0, "{r}");
    assert_eq!(s.catalog.album(a).unwrap().photos, vec![photo]);
    assert_eq!((s.undo.len(), s.redo.len()), (undo, 1));
}

#[test]
fn adding_photos_that_are_all_there_already_records_nothing() {
    let mut s = Session::with_demo();
    let photo = s.catalog.photos().next().unwrap().id;
    let a = album(&mut s, "Trip");
    s.execute("album.addPhotos", &json!({"id": a.0, "ids": [photo.0]})).unwrap();
    leave_redo(&mut s, photo);
    let undo = s.undo.len();
    let r = s.execute("album.addPhotos", &json!({"id": a.0, "ids": [photo.0]})).unwrap();
    assert_eq!(r["added"], 0, "{r}");
    assert_eq!((s.undo.len(), s.redo.len()), (undo, 1), "no step, redo kept");
}

#[test]
fn a_normal_add_or_toggle_is_one_step_and_one_undo_restores_it() {
    let (mut s, a, photo) = with_redo();
    let undo = s.undo.len();
    s.execute("album.addPhotos", &json!({"id": a.0, "ids": [photo.0]})).unwrap();
    assert_eq!(s.undo.len(), undo + 1);
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(s.catalog.album(a).unwrap().photos.is_empty());

    s.execute("album.setTarget", &json!({"id": a.0})).unwrap();
    let undo = s.undo.len();
    let r = s.execute("album.toggleTarget", &json!({"ids": [GONE.0, photo.0]})).unwrap();
    assert_eq!((r["added"].as_bool(), r["toggled"].as_u64(), s.undo.len()), (Some(true), Some(1), undo + 1), "{r}");
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(s.catalog.album(a).unwrap().photos.is_empty());
    s.execute("edit.redo", &json!({})).unwrap();
    // toggled off: also one step, one undo puts the photo back
    let undo = s.undo.len();
    let r = s.execute("album.toggleTarget", &json!({"ids": [photo.0]})).unwrap();
    assert_eq!((r["added"].as_bool(), r["toggled"].as_u64(), s.undo.len()), (Some(false), Some(1), undo + 1), "{r}");
    assert!(s.catalog.album(a).unwrap().photos.is_empty());
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.catalog.album(a).unwrap().photos, vec![photo]);
}

#[test]
fn a_toggle_counts_only_the_photos_it_added() {
    let mut s = Session::with_demo();
    let ids: Vec<PhotoId> = s.catalog.photos().take(3).map(|p| p.id).collect();
    let a = album(&mut s, "Trip");
    s.execute("album.setTarget", &json!({"id": a.0})).unwrap();
    s.execute("album.addPhotos", &json!({"id": a.0, "ids": [ids[0].0, ids[1].0]})).unwrap();
    let all: Vec<u64> = ids.iter().map(|p| p.0).collect();
    let r = s.execute("album.toggleTarget", &json!({"ids": all})).unwrap();
    assert_eq!((r["added"].as_bool(), r["toggled"].as_u64()), (Some(true), Some(1)), "two were in already: {r}");
    assert_eq!(s.catalog.album(a).unwrap().photos.len(), 3);
}

#[test]
fn photos_named_outright_reach_the_album_commands_with_nothing_selected() {
    let mut s = Session::with_demo();
    let photo = s.catalog.photos().next().unwrap().id;
    let a = album(&mut s, "Trip");
    s.execute("album.setTarget", &json!({"id": a.0})).unwrap();
    s.selection = Selection::default();

    // ids the library lacks: the no-op result, not a refusal
    let r = s.execute("album.addPhotos", &json!({"id": a.0, "ids": [GONE.0]})).unwrap();
    assert_eq!((r["added"].as_u64(), r["skipped"].as_u64()), (Some(0), Some(0)), "{r}");
    let r = s.execute("album.toggleTarget", &json!({"ids": [GONE.0]})).unwrap();
    assert!(r["added"].is_null(), "{r}");

    // ids the library has: they go in
    let r = s.execute("album.addPhotos", &json!({"id": a.0, "ids": [photo.0]})).unwrap();
    assert_eq!(r["added"], 1, "{r}");
    let r = s.execute("album.toggleTarget", &json!({"ids": [photo.0]})).unwrap();
    assert_eq!((r["added"].as_bool(), r["toggled"].as_u64()), (Some(false), Some(1)), "{r}");
}

#[test]
fn the_album_an_add_names_is_not_a_photo_that_skips_the_selection_check() {
    let mut s = Session::with_demo();
    let a = album(&mut s, "Trip");
    s.selection = Selection::default();
    // no `ids`: the selection is what is added, and it is empty
    assert!(matches!(s.execute("album.addPhotos", &json!({"id": a.0})), Err(crate::EngineError::Disabled(..))));
    assert!(matches!(s.execute("album.addPhotos", &json!({"id": a.0, "ids": null})), Err(crate::EngineError::Disabled(..))));
    assert!(matches!(s.execute("album.toggleTarget", &json!({})), Err(crate::EngineError::Disabled(..))));
}
