//! The view never points at an album that is gone. What the session shows and acts on (the
//! source, the album a filter names, the target album) refers to albums of the library; whenever
//! the library or the view may have changed, by whatever way, those references are checked in
//! one place (`Session::reconcile_view`).
//!
//! Scenarios:
//! - Given an album, a smart album or a folder being shown, when it goes away (deleted, its
//!   creation undone, its deletion redone, the folder around it deleted, or removed by an edit
//!   that is no command), then the grid shows All Photos, titled so.
//! - Given an album that went away and came back (its deletion undone), then it is not shown
//!   again by itself, nor the filter's album again; it is the target again, as it was (the target
//!   is looked up where it is used, so nothing had to forget it).
//! - Given a library closed while showing an album that is gone when it is opened again, then
//!   it opens on All Photos.
//! - Given the filter names an album, when that album goes away, then the filter no longer names
//!   it; a saved filter that names an album since deleted applies without it; a filter can't be
//!   set to an album that doesn't exist, and such a call changes nothing else either.
//! - Given an album is the target, when it goes away, then photos go to the Quick Collection.
//! - Given photos are selected, when some go away by undo, then commands act on the ones that
//!   are left and write the others nowhere; when they come back by redo, they are selected as
//!   before (the selection is looked up where it is read, not pruned).
//! - Given another library is opened, then the filter's album and the target, which named albums
//!   of the library before, name nothing: the same number is another album there.
//! - Given a smart album saved from the view of an album or folder, when that album is deleted,
//!   then the smart album says so (it is marked, as for a rule naming a deleted album) instead of
//!   being quietly empty; and a smart album can't be limited to an album that doesn't exist.
//!   Pressing OK in its rules dialog, or updating its rules from the current view, lets go of the
//!   album that is gone and the mark with it; a new smart album saved from its view isn't born
//!   with the mark.
//! - Given every selected photo is gone, then commands on the selection are not available (and
//!   so record nothing and leave redo alone); photos named outright that aren't there are not
//!   written into an album.
//! - Given the shown album is only changed (renamed, moved, emptied), then it is still shown.

use lightcraft_catalog::{AlbumId, Op, Photo, PhotoId, Source};
use serde_json::{Value, json};

use crate::{LibrarySource, Session};

fn photos(s: &mut Session, n: u64) {
    for day in 1..=n {
        let id = s.catalog.alloc_photo_id();
        let mut p = Photo::new(id, Source::Demo { scene: 1 }, &format!("p{day}.jpg"), "JPEG", 60, 40, "2026-02-01T10:00:00");
        p.captured = Some(format!("2026-01-{day:02}T10:00:00"));
        s.commit("Add Photo", Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
}

fn run(s: &mut Session, cmd: &str, params: Value) -> Value {
    s.execute(cmd, &params).unwrap()
}

fn show(s: &mut Session, id: u64) {
    run(s, "library.source", json!({"kind": "album", "id": id}));
    assert_eq!(s.source, LibrarySource::Album(AlbumId(id)));
}

fn shows_all(s: &mut Session, why: &str) {
    assert_eq!(s.source, LibrarySource::All, "{why}");
    assert_eq!(s.source.label(&s.catalog), "All Photos", "{why}");
    assert_eq!(s.visible_cloned().len(), 3, "{why}");
}

/// What can be shown: an album, a smart album, a folder of albums.
fn kinds() -> [(&'static str, &'static str, Value); 3] {
    [
        ("album", "album.create", json!({"name": "Shown"})),
        ("smart album", "album.createSmart", json!({"name": "Shown", "rules": {"rating": 3}})),
        ("folder", "album.create", json!({"name": "Shown", "folder": true})),
    ]
}

#[test]
fn a_shown_album_that_goes_away_gives_way_to_all_photos() {
    for (kind, create, params) in kinds() {
        // deleted
        let mut s = Session::new();
        photos(&mut s, 3);
        let id = run(&mut s, create, params.clone())["id"].as_u64().unwrap();
        show(&mut s, id);
        run(&mut s, "album.delete", json!({"id": id}));
        shows_all(&mut s, &format!("{kind} deleted"));
        // back by undo: not shown again by itself, and its deletion redone while shown
        run(&mut s, "edit.undo", json!({}));
        shows_all(&mut s, &format!("{kind} back by undo"));
        show(&mut s, id);
        run(&mut s, "edit.redo", json!({}));
        shows_all(&mut s, &format!("{kind}'s deletion redone"));

        // its creation undone
        let mut s = Session::new();
        photos(&mut s, 3);
        let id = run(&mut s, create, params.clone())["id"].as_u64().unwrap();
        show(&mut s, id);
        run(&mut s, "edit.undo", json!({}));
        assert!(s.catalog.album(AlbumId(id)).is_none());
        shows_all(&mut s, &format!("{kind}'s creation undone"));

        // removed by an edit that is no command (a background task, a merge): seen by the next look
        let mut s = Session::new();
        photos(&mut s, 3);
        let id = run(&mut s, create, params.clone())["id"].as_u64().unwrap();
        show(&mut s, id);
        s.commit("Remove", Op::RemoveAlbum { id: AlbumId(id) }).unwrap();
        assert_eq!(s.visible_cloned().len(), 3, "{kind} removed outside a command");
        shows_all(&mut s, &format!("{kind} removed outside a command"));
    }
}

#[test]
fn deleting_the_folder_around_the_shown_album_gives_way_too() {
    let mut s = Session::new();
    photos(&mut s, 3);
    let folder = run(&mut s, "album.create", json!({"name": "Trips", "folder": true}))["id"].as_u64().unwrap();
    let inner = run(&mut s, "album.create", json!({"name": "Rome", "parent": folder}))["id"].as_u64().unwrap();
    show(&mut s, inner);
    run(&mut s, "album.delete", json!({"id": folder}));
    shows_all(&mut s, "the folder around it deleted");
}

#[test]
fn an_album_that_comes_back_is_not_shown_again_by_itself() {
    let mut s = Session::new();
    photos(&mut s, 3);
    let id = run(&mut s, "album.create", json!({"name": "First"}))["id"].as_u64().unwrap();
    show(&mut s, id);
    run(&mut s, "library.filter", json!({"album": id}));
    // the filter is no undo step: this undoes the album
    run(&mut s, "edit.undo", json!({}));
    assert!(s.catalog.album(AlbumId(id)).is_none(), "the album's creation was undone");
    assert_eq!((s.source, s.filter.album), (LibrarySource::All, None));
    run(&mut s, "edit.redo", json!({}));
    assert!(s.catalog.album(AlbumId(id)).is_some(), "and redone");
    shows_all(&mut s, "the album came back");
    assert_eq!(s.filter.album, None);
    // nor does the next album made take its place
    run(&mut s, "edit.undo", json!({}));
    let second = run(&mut s, "album.create", json!({"name": "Second"}))["id"].as_u64().unwrap();
    assert_ne!(second, id, "ids are not handed out again");
    shows_all(&mut s, "another album was made");
}

/// The target album is looked up where it is used: gone, photos go to the Quick Collection; back
/// by undo, it is the target again.
#[test]
fn the_target_album_follows_the_album() {
    let mut s = Session::new();
    photos(&mut s, 3);
    let id = run(&mut s, "album.create", json!({"name": "Target"}))["id"].as_u64().unwrap();
    run(&mut s, "album.setTarget", json!({"id": id}));
    run(&mut s, "album.delete", json!({"id": id}));
    s.selection.ids = vec![PhotoId(1)];
    let r = run(&mut s, "album.toggleTarget", json!({}));
    assert_ne!(r["album"].as_u64(), Some(id));
    assert_eq!(s.catalog.quick_collection().map(|a| a.0), r["album"].as_u64(), "the Quick Collection took it");
    // undone as far as the deletion: the album is back, and is the target again
    for _ in 0..5 {
        if s.catalog.album(AlbumId(id)).is_none() {
            run(&mut s, "edit.undo", json!({}));
        }
    }
    assert!(s.catalog.album(AlbumId(id)).is_some());
    s.selection.ids = vec![PhotoId(2)];
    assert_eq!(run(&mut s, "album.toggleTarget", json!({}))["album"].as_u64(), Some(id));
}

#[test]
fn a_filter_lets_go_of_an_album_that_went_away() {
    let mut s = Session::new();
    photos(&mut s, 3);
    let id = run(&mut s, "album.create", json!({"name": "Named"}))["id"].as_u64().unwrap();
    let other = run(&mut s, "album.create", json!({"name": "Other"}))["id"].as_u64().unwrap();
    run(&mut s, "library.filter", json!({"album": id, "rating": 0}));
    assert_eq!(s.filter.album, Some(AlbumId(id)));
    // another album going away changes nothing
    run(&mut s, "album.delete", json!({"id": other}));
    assert_eq!(s.filter.album, Some(AlbumId(id)));
    run(&mut s, "album.delete", json!({"id": id}));
    assert_eq!(s.filter.album, None);
    assert_eq!(s.visible_cloned().len(), 3, "the filter hides nothing behind an album that is gone");
    // the step functions themselves check, not only the commands round them
    s.undo_step().unwrap();
    run(&mut s, "library.filter", json!({"album": id}));
    s.redo_step().unwrap();
    assert_eq!(s.filter.album, None);
}

/// The view can come to name a missing album with the library unchanged: a saved filter applied
/// after its album was deleted. And a filter is not set to an album that isn't there.
#[test]
fn a_filter_naming_a_missing_album_is_not_taken_up() {
    let mut s = Session::new();
    photos(&mut s, 3);
    let id = run(&mut s, "album.create", json!({"name": "Named"}))["id"].as_u64().unwrap();
    s.filter_presets.push(crate::cmd::filters::FilterPreset {
        name: "In Named".into(),
        filter: lightcraft_catalog::Filter { album: Some(AlbumId(id)), rating: 0, ..Default::default() },
    });
    run(&mut s, "album.delete", json!({"id": id}));
    // nothing changes in the library from here on
    let r = run(&mut s, "filter.applyPreset", json!({"name": "In Named"}));
    assert_eq!((s.filter.album, r["photos"].as_u64()), (None, Some(3)), "applied without the album that is gone");
    let e = s.execute("library.filter", &json!({"album": 77, "rating": 4})).unwrap_err().to_string();
    assert!(e.contains("no such album"), "{e}");
    assert_eq!((s.filter.album, s.filter.rating), (None, 0), "nothing of the call was taken up");
    assert_eq!(s.source_total(), Some(3));
    // an album that is there can be named, and cleared
    let kept = run(&mut s, "album.create", json!({"name": "Kept"}))["id"].as_u64().unwrap();
    run(&mut s, "library.filter", json!({"album": kept}));
    assert_eq!(s.filter.album, Some(AlbumId(kept)));
    run(&mut s, "library.filter", json!({"album": null}));
    assert_eq!(s.filter.album, None);
}

/// The counted total is a look at the grid too.
#[test]
fn the_total_is_of_what_is_there() {
    let mut s = Session::new();
    photos(&mut s, 3);
    let id = run(&mut s, "album.create", json!({"name": "Shown"}))["id"].as_u64().unwrap();
    show(&mut s, id);
    assert_eq!(s.source_total(), Some(0));
    s.commit("Remove", Op::RemoveAlbum { id: AlbumId(id) }).unwrap();
    assert_eq!(s.source_total(), Some(3));
    assert_eq!(s.source, LibrarySource::All);
}

/// Undo takes a selected photo away: commands on the selection act on the photos that are left,
/// and the one that is gone is written nowhere.
#[test]
fn a_selected_photo_that_is_gone_is_acted_on_nowhere() {
    let mut s = Session::new();
    photos(&mut s, 2);
    let album = run(&mut s, "album.create", json!({"name": "Kept"}))["id"].as_u64().unwrap();
    let late = s.catalog.alloc_photo_id();
    let p = Photo::new(late, Source::Demo { scene: 1 }, "late.jpg", "JPEG", 60, 40, "2026-02-01T10:00:00");
    s.commit("Add Photo", Op::AddPhoto { photo: Box::new(p) }).unwrap();
    s.selection.ids = vec![PhotoId(1), late];
    s.selection.active = Some(late);
    run(&mut s, "edit.undo", json!({}));
    assert!(s.catalog.photo(late).is_none());
    assert_eq!(s.active(), None, "no photo to act on alone");
    assert_eq!(s.targets(&json!({})), [PhotoId(1)], "the selection, as far as it is there");
    assert_eq!(run(&mut s, "album.addPhotos", json!({"id": album}))["added"], 1);
    assert_eq!(s.catalog.album(AlbumId(album)).unwrap().photos, [PhotoId(1)]);
    run(&mut s, "photo.rate", json!({"rating": 3}));
    assert_eq!(s.catalog.photo(PhotoId(1)).unwrap().rating, 3, "the photo that is left is rated");
    // ids a call names itself are the caller's to get right
    assert!(s.execute("photo.rate", &json!({"ids": [late.0], "rating": 1})).is_err());
}

/// Redo brings a selected photo back selected and active as before.
#[test]
fn photos_that_go_and_come_back_are_selected_as_before() {
    let mut s = Session::new();
    photos(&mut s, 3);
    s.selection.ids = vec![PhotoId(1), PhotoId(3)];
    s.selection.active = Some(PhotoId(3));
    run(&mut s, "edit.undo", json!({}));
    assert!(s.catalog.photo(PhotoId(3)).is_none());
    run(&mut s, "edit.redo", json!({}));
    assert_eq!((s.selection.ids.clone(), s.active()), (vec![PhotoId(1), PhotoId(3)], Some(PhotoId(3))));
}

/// Another library's albums have the same numbers: what named an album here names nothing there.
#[test]
fn another_library_starts_without_this_ones_album_filter_and_target() {
    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let dirs = ["a", "b"].map(|n| Scratch(std::env::temp_dir().join(format!("lc-view-reconcile-switch-{n}-{}", std::process::id()))));
    for d in &dirs {
        let _ = std::fs::remove_dir_all(&d.0);
    }
    // library B has an album 1 of its own
    let mut s = Session::new();
    s.open_library(&dirs[1].0, false).unwrap();
    photos(&mut s, 3);
    let theirs = run(&mut s, "album.create", json!({"name": "B's album"}))["id"].as_u64().unwrap();
    drop(s);
    let mut s = Session::new();
    s.open_library(&dirs[0].0, false).unwrap();
    photos(&mut s, 3);
    let ours = run(&mut s, "album.create", json!({"name": "A's album"}))["id"].as_u64().unwrap();
    assert_eq!(ours, theirs, "the same number in both libraries");
    run(&mut s, "album.setTarget", json!({"id": ours}));
    run(&mut s, "library.filter", json!({"album": ours, "rating": 0}));
    // and what else names this library's albums and photos by number
    run(&mut s, "library.filter", json!({"ruleSet": {"match": "all", "rules": [{"field": "album", "op": "is", "value": ours}]}}));
    s.filter.only = vec![PhotoId(2)];
    s.previous_active = Some(PhotoId(3));
    s.active_mask = Some(1);
    s.open_library(&dirs[1].0, false).unwrap();
    assert_eq!(s.catalog.album(AlbumId(theirs)).unwrap().name, "B's album");
    assert_eq!(s.target_album, None);
    assert_eq!(s.filter, lightcraft_catalog::Filter::default(), "no filter left over, as after a restart");
    assert_eq!((s.previous_active, s.active_mask), (None, None));
    assert_eq!(s.visible_cloned().len(), 3);
}

#[test]
fn a_smart_album_says_when_the_album_it_shows_is_gone() {
    let mut s = Session::new();
    photos(&mut s, 3);
    let folder = run(&mut s, "album.create", json!({"name": "Trips", "folder": true}))["id"].as_u64().unwrap();
    let inner = run(&mut s, "album.create", json!({"name": "Rome", "parent": folder}))["id"].as_u64().unwrap();
    s.selection.ids = vec![PhotoId(1), PhotoId(2)];
    run(&mut s, "album.addPhotos", json!({"id": inner, "ids": [1, 2]}));
    show(&mut s, folder);
    let saved = run(&mut s, "album.createSmart", json!({"name": "Trips view"}));
    assert_eq!(saved["count"], 2);
    let saved = AlbumId(saved["id"].as_u64().unwrap());
    assert!(s.catalog.smart_album_problems(saved).is_empty());
    run(&mut s, "album.delete", json!({"id": folder}));
    assert_eq!(s.catalog.album_count(saved), 0);
    let problems = s.catalog.smart_album_problems(saved);
    assert!(problems.iter().any(|p| p.issue == lightcraft_catalog::rules::Issue::NoSuchAlbum), "{problems:?}");
    // back with the folder
    run(&mut s, "edit.undo", json!({}));
    assert!(s.catalog.smart_album_problems(saved).is_empty());
    assert_eq!(s.catalog.album_count(saved), 2);
    // and none is made that way
    let e = s.execute("album.createSmart", &json!({"name": "Of nothing", "rules": {"album": 999}})).unwrap_err().to_string();
    assert!(e.contains("999"), "{e}");
}

/// The mark can be cleared where it sends the person: the rules dialog's OK (which replaces the
/// rules and keeps the album a view was saved from) keeps only an album that is still there, and
/// so does Update Rules from Current Filter. Other settings stay editable without clearing it.
#[test]
fn a_smart_album_lets_go_of_a_missing_album_when_its_rules_are_saved() {
    let saved_view_of_a_deleted_album = |s: &mut Session| -> AlbumId {
        let rome = run(s, "album.create", json!({"name": "Rome"}))["id"].as_u64().unwrap();
        show(s, rome);
        let saved = AlbumId(run(s, "album.createSmart", json!({"name": "Rome view"}))["id"].as_u64().unwrap());
        run(s, "album.delete", json!({"id": rome}));
        assert_eq!(s.catalog.smart_album_problems(saved).len(), 1);
        saved
    };
    let limit = |s: &Session, a: AlbumId| s.catalog.album(a).unwrap().smart.as_ref().unwrap().album;

    // the dialog's OK
    let mut s = Session::new();
    photos(&mut s, 3);
    let saved = saved_view_of_a_deleted_album(&mut s);
    // a patch that sets something else leaves the rest, the mark included
    run(&mut s, "album.setRules", json!({"id": saved.0, "rules": {"rating": 0}}));
    assert!(limit(&s, saved).is_some() && !s.catalog.smart_album_problems(saved).is_empty());
    let r = run(&mut s, "album.setRules", json!({"id": saved.0, "replace": true, "rules": {"ruleSet": {"match": "all", "rules": []}}}));
    assert_eq!(limit(&s, saved), None);
    assert!(s.catalog.smart_album_problems(saved).is_empty());
    assert_eq!(r["count"], 3, "what the dialog showed would match: its rules, without the album that is gone");

    // Update Rules from Current Filter, while it is shown
    let mut s = Session::new();
    photos(&mut s, 3);
    let saved = saved_view_of_a_deleted_album(&mut s);
    show(&mut s, saved.0);
    run(&mut s, "album.setRules", json!({"id": saved.0, "fromView": true}));
    assert!(s.catalog.smart_album_problems(saved).is_empty());

    // a new smart album from its view isn't born with the mark
    let mut s = Session::new();
    photos(&mut s, 3);
    let saved = saved_view_of_a_deleted_album(&mut s);
    show(&mut s, saved.0);
    let again = AlbumId(run(&mut s, "album.createSmart", json!({"name": "Again"}))["id"].as_u64().unwrap());
    assert!(s.catalog.smart_album_problems(again).is_empty());

    // an album that is still there is kept by the dialog's OK, as before
    let mut s = Session::new();
    photos(&mut s, 3);
    let rome = run(&mut s, "album.create", json!({"name": "Rome"}))["id"].as_u64().unwrap();
    show(&mut s, rome);
    let kept = AlbumId(run(&mut s, "album.createSmart", json!({"name": "Rome view"}))["id"].as_u64().unwrap());
    run(&mut s, "album.setRules", json!({"id": kept.0, "replace": true, "rules": {"ruleSet": {"match": "all", "rules": []}}}));
    assert_eq!(limit(&s, kept), Some(AlbumId(rome)));
}

/// Undo took away the only selected photo: nothing on the selection is available, so nothing is
/// recorded and redo still brings the photo back.
#[test]
fn a_selection_of_photos_that_are_gone_is_no_selection() {
    let mut s = Session::new();
    photos(&mut s, 2);
    let album = run(&mut s, "album.create", json!({"name": "Kept"}))["id"].as_u64().unwrap();
    let late = s.catalog.alloc_photo_id();
    let p = Photo::new(late, Source::Demo { scene: 1 }, "late.jpg", "JPEG", 60, 40, "2026-02-01T10:00:00");
    s.commit("Add Photo", Op::AddPhoto { photo: Box::new(p) }).unwrap();
    s.selection.ids = vec![late];
    s.selection.active = Some(late);
    run(&mut s, "edit.undo", json!({}));
    let (steps, redo) = (s.undo.len(), s.redo.len());
    assert_eq!(redo, 1);
    for (cmd, params) in [
        ("album.addPhotos", json!({"id": album})),
        ("album.removePhotos", json!({"id": album})),
        ("album.toggleTarget", json!({})),
        ("photo.rotateRight", json!({})),
        ("photo.flipHorizontal", json!({})),
        ("photo.deletePermanently", json!({})),
        ("photo.setMeta", json!({"title": "x"})),
        ("photo.rate", json!({"rating": 3})),
    ] {
        let e = s.execute(cmd, &params).map(|v| v.to_string()).unwrap_or_else(|e| e.to_string());
        assert_eq!((s.undo.len(), s.redo.len()), (steps, redo), "{cmd} recorded something: {e}");
    }
    assert!(s.catalog.quick_collection().is_none(), "no Quick Collection was made for nothing");
    run(&mut s, "edit.redo", json!({}));
    assert!(s.catalog.photo(late).is_some());
    assert_eq!(s.active(), Some(late));
    // photos named outright (a drag of the selection onto an album) that aren't there are left out
    run(&mut s, "edit.undo", json!({}));
    s.selection.ids = vec![PhotoId(1), late];
    let r = run(&mut s, "album.addPhotos", json!({"id": album, "ids": [1, late.0]}));
    assert_eq!(r["added"], 1);
    assert_eq!(s.catalog.album(AlbumId(album)).unwrap().photos, [PhotoId(1)]);
}

#[test]
fn a_shown_album_that_only_changes_stays_shown() {
    let mut s = Session::new();
    photos(&mut s, 3);
    let folder = run(&mut s, "album.create", json!({"name": "Trips", "folder": true}))["id"].as_u64().unwrap();
    let id = run(&mut s, "album.create", json!({"name": "Rome"}))["id"].as_u64().unwrap();
    show(&mut s, id);
    s.selection.ids = vec![PhotoId(1)];
    run(&mut s, "album.addPhotos", json!({"id": id, "ids": [1]}));
    run(&mut s, "album.rename", json!({"id": id, "name": "Roma"}));
    run(&mut s, "album.move", json!({"id": id, "parent": folder}));
    run(&mut s, "album.removePhotos", json!({"id": id, "ids": [1]}));
    run(&mut s, "edit.undo", json!({}));
    assert_eq!(s.source, LibrarySource::Album(AlbumId(id)));
    assert_eq!(s.source.label(&s.catalog), "Roma");
    assert_eq!(s.selection.ids, [PhotoId(1)]);
}

#[test]
fn a_library_opens_on_all_photos_when_its_last_album_is_gone() {
    /// Gone with the test, however it ends.
    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let scratch = Scratch(std::env::temp_dir().join(format!("lc-view-reconcile-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&scratch.0);
    let mut s = Session::new();
    s.open_library(&scratch.0, false).unwrap();
    photos(&mut s, 3);
    let id = run(&mut s, "album.create", json!({"name": "Shown"}))["id"].as_u64().unwrap();
    show(&mut s, id);
    s.save_view();
    // the view file still names the album; the library no longer has it
    s.source = LibrarySource::All;
    run(&mut s, "album.delete", json!({"id": id}));
    let view = scratch.0.join("view.json");
    let saved = std::fs::read_to_string(&view).unwrap();
    assert!(saved.contains("album"), "{saved}");
    drop(s);
    std::fs::write(&view, saved).unwrap();
    let mut back = Session::new();
    back.open_library(&scratch.0, false).unwrap();
    assert!(back.catalog.album(AlbumId(id)).is_none());
    shows_all(&mut back, "opened with a view of an album that is gone");
}
