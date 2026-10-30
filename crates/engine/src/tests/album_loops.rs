//! A smart album never comes to include itself by a new edit, whatever makes the edit
//! (`Session::commit` keeps the catalog's rule, `Catalog::apply_new`); a library that already
//! holds such a loop opens, works and can be mended, and undo and redo move across it freely.
//!
//! Scenarios:
//! - When an edit that no command checked would put a smart album on a loop, then it is
//!   refused, and there is nothing to undo.
//! - Given a library saved with a loop in it, when it is opened, then the album on the loop is
//!   reported and empty, the folder it is in shows its other albums, and moving the album out
//!   mends it; undoing that brings the loop back (it was there), and redoing mends it again.

use lightcraft_catalog::{Album, AlbumId, Filter, Op, Photo, PhotoId, Source};
use serde_json::json;

use crate::Session;

fn limited_to(album: u64) -> Filter {
    Filter { album: Some(AlbumId(album)), ..Default::default() }
}

fn make(s: &mut Session, params: serde_json::Value) -> u64 {
    s.execute("album.create", &params).unwrap()["id"].as_u64().unwrap()
}

#[test]
fn an_edit_no_command_checked_cant_make_a_loop() {
    let mut s = Session::new();
    let trips = make(&mut s, json!({"name": "Trips", "folder": true}));
    let view = s.execute("album.createSmart", &json!({"name": "Trips view", "rules": {"album": trips}})).unwrap()["id"].as_u64().unwrap();
    let (steps, before) = (s.undo.len(), s.catalog.to_snapshot());
    let next = s.catalog.alloc_album_id();
    for op in [
        Op::MoveAlbum { id: AlbumId(view), parent: Some(AlbumId(trips)) },
        Op::AddAlbum { album: Album { parent: Some(AlbumId(trips)), smart: Some(Box::new(limited_to(trips))), ..Album::new(next, "Inside") } },
        Op::Batch { ops: vec![Op::MoveAlbum { id: AlbumId(view), parent: Some(AlbumId(trips)) }] },
    ] {
        let e = s.commit("Edit", op).unwrap_err().to_string();
        assert!(e.contains("include itself"), "{e}");
        assert_eq!(s.undo.len(), steps, "nothing to undo");
        assert!(s.catalog.albums_on_a_loop().is_empty());
    }
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&s.catalog.to_snapshot()).unwrap()["albums"],
        serde_json::from_str::<serde_json::Value>(&before).unwrap()["albums"]
    );
}

#[test]
fn a_library_saved_with_a_loop_opens_and_is_mended_by_moving_the_album_out() {
    /// Gone with the test, however it ends.
    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let scratch = Scratch(std::env::temp_dir().join(format!("lc-album-loops-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&scratch.0);
    let mut s = Session::new();
    s.open_library(&scratch.0, false).unwrap();
    let id = s.catalog.alloc_photo_id();
    s.commit(
        "Add Photo",
        Op::AddPhoto { photo: Box::new(Photo::new(id, Source::Demo { scene: 1 }, "p.jpg", "JPEG", 60, 40, "2026-02-01T10:00:00")) },
    )
    .unwrap();
    let trips = make(&mut s, json!({"name": "Trips", "folder": true}));
    let rome = make(&mut s, json!({"name": "Rome", "parent": trips}));
    s.selection.ids = vec![PhotoId(id.0)];
    s.execute("album.addPhotos", &json!({"id": rome, "ids": [id.0]})).unwrap();
    let view = s.execute("album.createSmart", &json!({"name": "Trips view", "rules": {"album": trips}})).unwrap()["id"].as_u64().unwrap();
    // as a version without the rule recorded it: the view moved into the folder it shows
    let old_edit = Op::MoveAlbum { id: AlbumId(view), parent: Some(AlbumId(trips)) };
    s.catalog.apply(old_edit.clone()).unwrap();
    s.pending_log.push(old_edit);
    s.execute("album.rename", &json!({"id": rome, "name": "Roma"})).unwrap();
    drop(s);

    let mut s = Session::new();
    s.open_library(&scratch.0, false).unwrap();
    let on_a_loop = |s: &Session| s.catalog.albums_on_a_loop().into_iter().map(|a| a.0).collect::<Vec<_>>();
    assert_eq!(on_a_loop(&s), [view], "it opened, loop and all");
    assert!(!s.catalog.smart_album_problems(AlbumId(view)).is_empty(), "and says so");
    let shown = |s: &mut Session, album: u64| {
        s.execute("library.source", &json!({"kind": "album", "id": album})).unwrap();
        s.visible_cloned().len()
    };
    assert_eq!((shown(&mut s, view), shown(&mut s, trips)), (0, 1), "the album on the loop is empty; its folder shows Roma's photo");
    // other edits go on
    s.execute("album.rename", &json!({"id": view, "name": "Still looping"})).unwrap();
    // mended by moving it out; undo brings back what was, redo mends again
    s.execute("album.move", &json!({"id": view, "parent": null})).unwrap();
    assert!(on_a_loop(&s).is_empty());
    assert_eq!(shown(&mut s, view), 1);
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(on_a_loop(&s), [view], "undo is no new edit: it brings back the library as it was");
    s.execute("edit.redo", &json!({})).unwrap();
    assert!(on_a_loop(&s).is_empty());
}
