//! Exploring the library by folder: `library.folders` lists where the imported photos live and
//! `library.filter {libraryFolder}` shows one folder's photos (see `lightcraft_catalog::folders`);
//! `library.removeFolder` takes a folder's photos out of the library.
//!
//! * Given photos imported from two folders, an agent can list them with counts, then choose one
//!   and see exactly its photos; the filter shows as a chip that clears just that choice.
//! * A path that is not a library folder shows nothing rather than everything.
//! * Removing a folder from the library moves its photos to Recently Deleted (one undo step) and
//!   leaves every file where it is.
//! * Renaming or moving a folder on disk keeps the chosen folder chosen.
//! * Given a folder of the library, an agent gives it a colour label with `folder.label`; the
//!   folder list shows it, undo takes it back, and only folders the library holds photos in can
//!   be labelled.
//! * A labelled folder keeps its label when it is renamed or moved on disk, and so do the folders
//!   inside it; undo puts the labels back where they were.

use lightcraft_catalog::{Op, Photo, Source};
use serde_json::json;

use crate::{LibrarySource, Session, filter_chips};

fn source_folder(s: &mut Session, path: &str) -> serde_json::Value {
    s.execute("library.source", &json!({"kind": "libraryFolder", "path": path})).unwrap()
}

fn add(s: &mut Session, path: &str) {
    let id = s.catalog.alloc_photo_id();
    let p = Photo::new(id, Source::File { path: path.into() }, "x.jpg", "JPEG", 60, 40, "2026-01-01T10:00:00");
    s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
}

/// A scratch folder that goes away with the test, however it ends.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("lc-folders-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
    fn path(&self, rel: &str) -> String {
        // the host's separator throughout, as the code under test spells joined paths
        self.0.join(rel).components().collect::<std::path::PathBuf>().to_string_lossy().to_string()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn session() -> Session {
    let mut s = Session::new();
    for path in ["/pics/trip/a.jpg", "/pics/trip/b.jpg", "/pics/home/c.jpg"] {
        add(&mut s, path);
    }
    s
}

#[test]
fn the_folders_command_lists_where_the_photos_were_imported_from() {
    let mut s = session();
    let r = s.execute("library.folders", &json!({})).unwrap();
    assert_eq!((r[0]["name"].as_str(), r[0]["volume"].as_bool(), r[0]["count"].as_u64()), (Some("/"), Some(true), Some(3)));
    let pics = &r[0]["children"][0];
    assert_eq!(pics["name"], "pics");
    let kids: Vec<(&str, u64)> =
        pics["children"].as_array().unwrap().iter().map(|c| (c["name"].as_str().unwrap(), c["count"].as_u64().unwrap())).collect();
    assert_eq!(kids, [("home", 1), ("trip", 2)]);
}

#[test]
fn choosing_a_folder_shows_its_photos_and_a_chip_that_clears_it() {
    let mut s = session();
    assert_eq!(s.visible().len(), 3);
    let r = s.execute("library.filter", &json!({"libraryFolder": "/pics/trip"})).unwrap();
    assert_eq!(r["count"], 2);
    let chips = filter_chips(&s.filter, &s.catalog);
    assert_eq!(chips.len(), 1);
    assert_eq!(chips[0].label, "Folder: pics/trip", "two names, so two folders called trip are told apart");
    s.execute("library.filter", &chips[0].clear).unwrap();
    assert_eq!(s.visible().len(), 3, "clearing the chip shows everything again");
}

#[test]
fn a_path_that_is_not_a_library_folder_shows_nothing() {
    let mut s = session();
    let r = s.execute("library.filter", &json!({"libraryFolder": "/elsewhere"})).unwrap();
    assert_eq!(r["count"], 0);
}

#[test]
fn removing_a_folder_moves_its_photos_to_recently_deleted_and_undo_brings_them_back() {
    let mut s = session();
    let r = s.execute("library.removeFolder", &json!({"path": "/pics//trip/"})).unwrap();
    assert_eq!(r["removed"], 2);
    assert_eq!(s.visible().len(), 1, "only /pics/home is left");
    let tree = s.execute("library.folders", &json!({})).unwrap();
    assert_eq!(tree[0]["count"], 1);
    s.source = LibrarySource::RecentlyDeleted;
    assert_eq!(s.visible().len(), 2, "they wait in Recently Deleted");
    s.source = LibrarySource::All;
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.visible().len(), 3, "one undo step");
}

#[test]
fn removing_a_folder_drops_its_photos_from_stacks() {
    let mut s = session();
    let trip: Vec<_> =
        s.catalog.photos().filter(|p| matches!(&p.source, Source::File { path } if path.contains("/pics/trip/"))).map(|p| p.id).collect();
    assert_eq!(trip.len(), 2);
    s.execute("library.select", &json!({"ids": [trip[0].0, trip[1].0], "active": trip[0].0})).unwrap();
    s.execute("stack.group", &json!({})).unwrap();
    assert_eq!(s.catalog.stack_of(trip[0]).unwrap().photos.len(), 2);
    s.execute("library.removeFolder", &json!({"path": "/pics/trip"})).unwrap();
    assert!(s.catalog.stack_of(trip[0]).is_none() && s.catalog.stack_of(trip[1]).is_none());
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.catalog.stack_of(trip[0]).unwrap().photos.len(), 2);
}

#[test]
fn removing_a_folder_that_holds_nothing_or_a_whole_disk_is_refused() {
    let mut s = session();
    assert!(s.execute("library.removeFolder", &json!({"path": "/elsewhere"})).is_err());
    assert!(s.execute("library.removeFolder", &json!({"path": "/"})).is_err(), "choose a folder, not everything");
    assert!(s.execute("library.removeFolder", &json!({"path": " "})).is_err());
    assert!(s.execute("library.removeFolder", &json!({})).is_err());
    assert_eq!(s.visible().len(), 3, "nothing changed");
}

#[test]
fn removing_a_folder_leaves_browsed_and_already_deleted_photos_alone() {
    let mut s = session();
    for (path, local, deleted) in [("/pics/trip/browsed.jpg", true, false), ("/pics/trip/gone.jpg", false, true)] {
        let id = s.catalog.alloc_photo_id();
        let mut p = Photo::new(id, Source::File { path: path.into() }, "x.jpg", "JPEG", 60, 40, "2026-01-01T10:00:00");
        (p.local, p.deleted) = (local, deleted);
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    let r = s.execute("library.removeFolder", &json!({"path": "/pics/trip"})).unwrap();
    assert_eq!(r["removed"], 2, "only the two library photos of the folder");
    let state: Vec<(bool, bool)> = s.catalog.photos().filter(|p| p.local || p.deleted).map(|p| (p.local, p.deleted)).collect();
    assert!(state.contains(&(true, false)) && state.contains(&(false, true)), "browsed stays browsed, the old deletion stays: {state:?}");
}

#[test]
fn removing_the_folder_that_is_chosen_clears_the_choice_but_a_parent_stays() {
    let mut s = session();
    s.execute("library.filter", &json!({"libraryFolder": "/pics/trip"})).unwrap();
    s.execute("library.removeFolder", &json!({"path": "/pics/trip"})).unwrap();
    assert_eq!(s.filter.library_folder, None, "no empty grid under a chip for a folder that is gone");
    let mut s = session();
    s.execute("library.filter", &json!({"libraryFolder": "/pics"})).unwrap();
    s.execute("library.removeFolder", &json!({"path": "/pics/trip"})).unwrap();
    assert_eq!(s.filter.library_folder.as_deref(), Some("/pics"));
}

#[test]
fn a_path_that_names_no_folder_removes_nothing() {
    let mut s = session();
    for path in [".", "./", "a/..", "../..", "x/../.."] {
        assert!(s.execute("library.removeFolder", &json!({"path": path})).is_err(), "{path:?}");
    }
    assert_eq!(s.visible().len(), 3);
}

#[test]
fn a_whole_disk_goes_only_when_asked_for_by_name_and_never_the_startup_disk() {
    let mut s = Session::new();
    for p in ["/Volumes/nas/a/1.jpg", "/Volumes/nas/2.jpg", "/Users/me/3.jpg", r"C:\x\4.jpg", r"\\srv\share\5.jpg"] {
        add(&mut s, p);
    }
    for path in ["/Volumes/nas", "C:", "C:\\", r"\\?\C:\", "C:\\..", r"\\srv\share"] {
        assert!(s.execute("library.removeFolder", &json!({"path": path})).is_err(), "{path:?} is a whole disk");
    }
    for path in ["/", "//"] {
        assert!(s.execute("library.removeFolder", &json!({"path": path, "disk": true})).is_err(), "{path:?}: never the startup disk");
    }
    assert_eq!(s.visible().len(), 5, "nothing changed");
    let r = s.execute("library.removeFolder", &json!({"path": "/Volumes/nas", "disk": true})).unwrap();
    assert_eq!(r["removed"], 2, "everything on the nas, nothing elsewhere");
    assert_eq!(s.visible().len(), 3);
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.visible().len(), 5);
}

#[test]
fn a_folder_that_spans_several_disks_is_removed_only_when_asked_for_by_name() {
    let mut s = Session::new();
    for p in ["/Volumes/nas/a/1.jpg", "/Volumes/tokyo/2.jpg", "/Volumes/3.jpg"] {
        add(&mut s, p);
    }
    assert!(s.execute("library.removeFolder", &json!({"path": "/Volumes"})).is_err(), "it would take two disks' photos at once");
    assert_eq!(s.visible().len(), 3);
    let r = s.execute("library.removeFolder", &json!({"path": "/Volumes", "disk": true})).unwrap();
    assert_eq!(r["removed"], 3);
}

#[test]
fn the_folder_filter_takes_only_a_text_path() {
    let mut s = session();
    assert!(s.execute("library.filter", &json!({"libraryFolder": 5})).is_err());
    assert_eq!(s.filter.library_folder, None, "refused, nothing changed");
    s.execute("library.filter", &json!({"libraryFolder": "   "})).unwrap();
    assert_eq!(s.visible().len(), 3, "a blank path is no choice");
    s.execute("library.filter", &json!({"libraryFolder": "."})).unwrap();
    assert_eq!(s.visible().len(), 0, "a path that names no folder shows nothing, not everything");
}

/// Folders are compared by identity: a path read back may be spelled with the platform's separator.
fn same_folder(actual: Option<&str>, expected: &str, why: &str) {
    let key = lightcraft_catalog::query::folder_key;
    assert_eq!(actual.map(key), Some(key(expected)), "{why}: {actual:?} vs {expected}");
}

#[test]
fn a_renamed_folder_stays_the_chosen_one_and_undo_follows_it_back() {
    let dir = Scratch::new("rename");
    std::fs::create_dir_all(dir.0.join("trip/day1")).unwrap();
    let (trip, renamed) = (dir.path("trip"), dir.path("holiday"));
    let mut s = Session::new();
    add(&mut s, &format!("{trip}/a.jpg"));
    add(&mut s, &format!("{trip}/day1/b.jpg"));
    s.execute("library.filter", &json!({"libraryFolder": trip})).unwrap();
    assert_eq!(s.visible().len(), 2);
    s.execute("folder.rename", &json!({"path": trip, "name": "holiday"})).unwrap();
    same_folder(s.filter.library_folder.as_deref(), &renamed, "the choice follows the folder");
    assert_eq!(s.visible().len(), 2, "and still shows its photos");
    s.execute("edit.undo", &json!({})).unwrap();
    same_folder(s.filter.library_folder.as_deref(), &trip, "the choice is the same folder");
    assert_eq!(s.visible().len(), 2);
}

#[test]
fn a_moved_folder_keeps_a_chosen_subfolder_chosen() {
    let dir = Scratch::new("move");
    std::fs::create_dir_all(dir.0.join("trip/day1")).unwrap();
    std::fs::create_dir_all(dir.0.join("archive")).unwrap();
    let (day1, moved) = (dir.path("trip/day1"), dir.path("archive/trip/day1"));
    let mut s = Session::new();
    add(&mut s, &format!("{day1}/b.jpg"));
    s.execute("library.filter", &json!({"libraryFolder": day1})).unwrap();
    s.execute("folder.move", &json!({"path": dir.path("trip"), "into": dir.path("archive")})).unwrap();
    same_folder(s.filter.library_folder.as_deref(), &moved, "the subfolder follows its parent");
    assert_eq!(s.visible().len(), 1);
    s.execute("edit.undo", &json!({})).unwrap();
    same_folder(s.filter.library_folder.as_deref(), &day1, "the choice is the same folder");
}

#[test]
fn following_a_folder_reads_both_spellings_the_same_way() {
    // the choice was typed with a `..` in it; the folder that moved was not
    let mut s = Session::new();
    s.filter.library_folder = Some("/a/x/../b/sub".into());
    crate::cmd::browse::follow_folder(&mut s, "/a/b", "/a/c");
    same_folder(s.filter.library_folder.as_deref(), "/a/c/sub", "a subfolder stays a subfolder, never widens to its parent");
    s.filter.library_folder = Some("/elsewhere/b".into());
    crate::cmd::browse::follow_folder(&mut s, "/a/b", "/a/c");
    assert_eq!(s.filter.library_folder.as_deref(), Some("/elsewhere/b"), "other folders are left alone");
}

#[test]
fn following_a_folder_keeps_the_case_of_the_subfolder_names() {
    // Windows compares folders in lower case; the name the user sees must not change with it
    let mut s = Session::new();
    s.filter.library_folder = Some("/A/Trip/Day1".into());
    crate::cmd::browse::follow_folder(&mut s, "/A/Trip", "/B");
    assert_eq!(s.filter.library_folder.as_deref(), Some(std::path::Path::new("/B").join("Day1").to_str().unwrap()));
}

#[test]
fn choosing_a_folder_replaces_the_source_the_way_an_album_does() {
    let mut s = session();
    s.execute("library.source", &json!({"kind": "picks"})).unwrap();
    assert_eq!(s.visible().len(), 0, "no picks");
    let r = source_folder(&mut s, "/pics/trip");
    assert_eq!(r["count"], 2, "the folder's photos, whatever was shown before");
    assert_eq!((s.source, s.library_folder.as_deref()), (LibrarySource::LibraryFolder, Some("/pics/trip")));
    assert!(filter_chips(&s.filter, &s.catalog).is_empty(), "a source is not a filter chip");
    let state = s.execute("library.state", &json!({})).unwrap();
    assert_eq!((state["sourceLabel"].as_str(), state["libraryFolder"].as_str()), (Some("pics/trip"), Some("/pics/trip")), "agents see which folder");
    // the filter bar narrows it further, as it does an album
    s.execute("library.filter", &json!({"rating": 1})).unwrap();
    assert_eq!(s.visible().len(), 0);
    s.execute("library.clearFilter", &json!({})).unwrap();
    assert_eq!(s.visible().len(), 2);
    s.execute("library.source", &json!({"kind": "all"})).unwrap();
    assert_eq!(s.visible().len(), 3, "back to everything");
}

#[test]
fn the_folder_source_takes_a_library_folder_path_only() {
    let mut s = session();
    for params in [
        json!({"kind": "libraryFolder"}),
        json!({"kind": "libraryFolder", "path": 5}),
        json!({"kind": "libraryFolder", "path": "  "}),
        json!({"kind": "libraryFolder", "path": "."}),
    ] {
        assert!(s.execute("library.source", &params).is_err(), "{params}");
    }
    assert_eq!(s.source, LibrarySource::All, "refused, nothing changed");
    assert!(s.execute("library.source", &json!({"kind": "libraryFolder", "path": "/elsewhere"})).is_err(), "no photo was imported from it");
    assert_eq!(s.source, LibrarySource::All);
}

#[test]
fn removing_the_folder_being_shown_returns_to_all_photos() {
    let mut s = session();
    source_folder(&mut s, "/pics/trip");
    s.execute("library.removeFolder", &json!({"path": "/pics/trip"})).unwrap();
    assert_eq!((s.source, s.library_folder.as_deref()), (LibrarySource::All, None));
    assert_eq!(s.visible().len(), 1);
    // removing another folder leaves the source alone
    let mut s = session();
    source_folder(&mut s, "/pics/home");
    s.execute("library.removeFolder", &json!({"path": "/pics/trip"})).unwrap();
    assert_eq!(s.source, LibrarySource::LibraryFolder);
    assert_eq!(s.visible().len(), 1);
}

#[test]
fn a_renamed_folder_stays_the_one_shown_and_undo_follows_it_back() {
    let dir = Scratch::new("source-rename");
    std::fs::create_dir_all(dir.0.join("trip/day1")).unwrap();
    let (trip, day1, renamed) = (dir.path("trip"), dir.path("trip/day1"), dir.path("holiday"));
    let mut s = Session::new();
    add(&mut s, &format!("{trip}/a.jpg"));
    add(&mut s, &format!("{day1}/b.jpg"));
    source_folder(&mut s, &day1);
    s.execute("folder.rename", &json!({"path": trip, "name": "holiday"})).unwrap();
    same_folder(s.library_folder.as_deref(), &format!("{renamed}/day1"), "the subfolder follows its parent");
    assert_eq!(s.visible().len(), 1);
    s.execute("edit.undo", &json!({})).unwrap();
    same_folder(s.library_folder.as_deref(), &day1, "the choice is the same folder");
    assert_eq!(s.visible().len(), 1);
}

#[test]
fn the_shown_folder_follows_reading_both_spellings_the_same_way() {
    let mut s = Session::new();
    s.library_folder = Some("/a/x/../b/sub".into());
    crate::cmd::browse::follow_folder(&mut s, "/a/b", "/a/c");
    same_folder(s.library_folder.as_deref(), "/a/c/sub", "the shown folder reads both spellings alike");
}

#[test]
fn switching_from_one_folder_to_another_changes_what_is_shown() {
    let mut s = session();
    assert_eq!(source_folder(&mut s, "/pics/trip")["count"], 2);
    assert_eq!(s.source_total(), Some(2));
    assert_eq!(source_folder(&mut s, "/pics/home")["count"], 1, "same source, another folder");
    assert_eq!(s.visible().len(), 1);
    assert_eq!(s.source_total(), Some(1));
}

#[test]
fn a_smart_album_made_from_a_folder_view_keeps_the_folder() {
    let mut s = session();
    source_folder(&mut s, "/pics/trip");
    assert_eq!(s.visible().len(), 2);
    let r = s.execute("album.createSmart", &json!({"name": "Trip"})).unwrap();
    assert_eq!(r["count"], 2, "the album matches the folder's photos, not the whole library: {r}");
}

#[test]
fn a_folder_source_and_a_folder_filter_never_disagree() {
    let mut s = session();
    s.execute("library.filter", &json!({"libraryFolder": "/pics/home"})).unwrap();
    source_folder(&mut s, "/pics/trip");
    assert_eq!(s.filter.library_folder, None, "choosing a folder drops a folder filter left over");
    assert!(s.execute("library.filter", &json!({"libraryFolder": "/pics/home"})).is_err(), "one folder at a time");
    assert_eq!(s.visible().len(), 2);
    s.execute("library.filter", &json!({"libraryFolder": null, "rating": 0})).unwrap();
    s.execute("library.source", &json!({"kind": "all"})).unwrap();
    assert!(s.execute("library.filter", &json!({"libraryFolder": "/pics/home"})).is_ok(), "fine while no folder is the source");
}

#[test]
fn a_folder_source_with_no_folder_falls_back_to_all_photos() {
    let mut s = session();
    s.source = LibrarySource::LibraryFolder;
    s.library_folder = None;
    assert_eq!(s.visible().len(), 3, "everything, not an empty grid under no name");
    assert_eq!((s.source, s.library_folder.as_deref()), (LibrarySource::All, None));
    assert_eq!(s.source_total(), Some(3));
}

#[test]
fn only_a_folder_that_shows_exactly_its_own_photos_can_be_the_source() {
    let mut s = Session::new();
    for p in ["/Volumes/nas/a/1.jpg", "/Users/me/2.jpg", "/Volumes/3.jpg"] {
        add(&mut s, p);
    }
    for path in ["/", "//", "/elsewhere", "/Volumes"] {
        assert!(s.execute("library.source", &json!({"kind": "libraryFolder", "path": path})).is_err(), "{path:?}");
    }
    assert_eq!(s.source, LibrarySource::All, "refused, nothing changed");
    assert_eq!(source_folder(&mut s, "/Volumes/nas")["count"], 1, "a disk is a fine source");
    assert_eq!(source_folder(&mut s, "/Users/me")["count"], 1);
}

#[test]
fn removing_a_folder_above_a_disk_needs_asking_by_name_even_with_one_disk() {
    let mut s = Session::new();
    for p in ["/Volumes/nas/a/1.jpg", "/Users/me/2.jpg"] {
        add(&mut s, p);
    }
    assert!(s.execute("library.removeFolder", &json!({"path": "/Volumes"})).is_err(), "it would take the whole nas");
    assert_eq!(s.visible().len(), 2);
    assert_eq!(s.execute("library.removeFolder", &json!({"path": "/Volumes/nas/a"})).unwrap()["removed"], 1, "a folder on the disk is just a folder");
    assert_eq!(s.visible().len(), 1);
}

#[test]
fn a_folder_name_with_spaces_at_its_edge_is_never_mistaken_for_its_neighbour() {
    let mut s = Session::new();
    for p in ["/p/shoot /a.jpg", "/p/shoot/b.jpg", "/p/other/c.jpg"] {
        add(&mut s, p);
    }
    assert_eq!(source_folder(&mut s, "/p/shoot ")["count"], 1);
    let shown: Vec<String> = s.visible().to_vec().iter().filter_map(|id| s.catalog.photo(*id)).map(|p| p.file_name.clone()).collect();
    assert_eq!(s.library_folder.as_deref(), Some("/p/shoot "), "kept as the tree gave it");
    assert_eq!(shown.len(), 1);
    let before: Vec<_> = s.catalog.photos().filter(|p| p.deleted).map(|p| p.id).collect();
    assert!(before.is_empty());
    let r = s.execute("library.removeFolder", &json!({"path": "/p/shoot "})).unwrap();
    assert_eq!(r["removed"], 1);
    let gone: Vec<String> = s
        .catalog
        .photos()
        .filter(|p| p.deleted)
        .map(|p| match &p.source {
            Source::File { path } => path.clone(),
            Source::Demo { .. } => String::new(),
        })
        .collect();
    assert_eq!(gone, ["/p/shoot /a.jpg"], "the folder named, not its neighbour");
}

#[test]
fn editing_a_smart_album_made_from_a_folder_view_keeps_the_folder() {
    let mut s = session();
    source_folder(&mut s, "/pics/trip");
    let r = s.execute("album.createSmart", &json!({"name": "Trip"})).unwrap();
    let id = r["id"].as_u64().unwrap();
    // the rules dialog saves what it shows (no folder field) with `replace`
    let r = s.execute("album.setRules", &json!({"id": id, "replace": true, "rules": {"ruleSet": {"rules": []}}})).unwrap();
    assert_eq!(r["count"], 2, "the folder is not something the dialog can show, so it is kept: {r}");
    let r = s.execute("album.setRules", &json!({"id": id, "replace": true, "rules": {"libraryFolder": null}})).unwrap();
    assert_eq!(r["count"], 3, "and can be dropped on purpose");
}

#[test]
fn a_shown_folder_that_loses_its_last_photo_gives_way_to_all_photos() {
    let mut s = session();
    source_folder(&mut s, "/pics/home");
    s.execute("photo.delete", &json!({})).unwrap();
    assert_eq!(s.visible().len(), 2, "back to everything that is left");
    assert_eq!((s.source, s.library_folder.as_deref()), (LibrarySource::All, None));
    // a folder whose photos are only hidden by the filter bar stays
    let mut s = session();
    source_folder(&mut s, "/pics/trip");
    s.execute("library.filter", &json!({"rating": 5})).unwrap();
    assert_eq!(s.visible().len(), 0);
    assert_eq!(s.source, LibrarySource::LibraryFolder, "the filters hide them, the folder is still there");
}

#[test]
fn the_state_names_a_folder_only_while_it_is_shown() {
    let mut s = session();
    source_folder(&mut s, "/pics/trip");
    assert_eq!(s.execute("library.state", &json!({})).unwrap()["libraryFolder"], "/pics/trip");
    s.execute("library.source", &json!({"kind": "all"})).unwrap();
    assert!(s.execute("library.state", &json!({})).unwrap()["libraryFolder"].is_null(), "not a stale leftover");
}

#[test]
fn a_blank_folder_filter_is_no_filter() {
    let mut s = session();
    s.execute("library.filter", &json!({"libraryFolder": "  "})).unwrap();
    assert_eq!(s.filter, lightcraft_catalog::Filter::default(), "no hidden 'filters active' state");
}

/// The label `library.folders` reports for the row at `path`.
fn listed_label(s: &mut Session, path: &str) -> Option<String> {
    fn find(rows: &serde_json::Value, key: &str) -> Option<serde_json::Value> {
        rows.as_array()?.iter().find_map(|r| {
            if r["path"].as_str().map(lightcraft_catalog::query::folder_key).as_deref() == Some(key) {
                Some(r.clone())
            } else {
                find(&r["children"], key)
            }
        })
    }
    let rows = s.execute("library.folders", &json!({})).unwrap();
    let row = find(&rows, &lightcraft_catalog::query::folder_key(path)).unwrap_or_else(|| panic!("no row for {path}: {rows}"));
    row["label"].as_str().map(str::to_string)
}

#[test]
fn a_labelled_folder_shows_its_label_in_the_folder_list_until_undone() {
    let mut s = session();
    s.execute("folder.label", &json!({"path": "/pics/trip", "label": "green"})).unwrap();
    assert_eq!(listed_label(&mut s, "/pics/trip").as_deref(), Some("green"));
    assert_eq!(listed_label(&mut s, "/pics/home"), None, "its neighbour has none");
    s.execute("folder.label", &json!({"path": "/pics/trip", "label": "none"})).unwrap();
    assert_eq!(listed_label(&mut s, "/pics/trip"), None);
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(listed_label(&mut s, "/pics/trip").as_deref(), Some("green"), "undo puts the label back");
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(listed_label(&mut s, "/pics/trip"), None);
}

#[test]
fn only_a_folder_of_the_library_takes_a_known_label() {
    let mut s = session();
    let before = s.catalog.to_snapshot();
    for (p, why) in [
        (json!({"path": "/pics/trip", "label": "mauve"}), "unknown label"),
        (json!({"path": "/pics/trip"}), "missing label"),
        (json!({"label": "red"}), "missing path"),
        (json!({"path": "  ", "label": "red"}), "blank path"),
        (json!({"path": "/elsewhere", "label": "red"}), "no photo was imported from it"),
        (json!({"path": "pics/trip", "label": "red"}), "a relative path"),
    ] {
        assert!(s.execute("folder.label", &p).is_err(), "{why}");
    }
    assert_eq!(s.catalog.to_snapshot(), before, "nothing changed");
}

#[test]
fn labels_follow_a_renamed_folder_and_undo_puts_them_back() {
    let dir = Scratch::new("label-rename");
    std::fs::create_dir_all(dir.0.join("trip/day1")).unwrap();
    let (trip, day1) = (dir.path("trip"), dir.path("trip/day1"));
    let mut s = Session::new();
    add(&mut s, &format!("{trip}/a.jpg"));
    add(&mut s, &format!("{day1}/b.jpg"));
    s.execute("folder.label", &json!({"path": trip, "label": "red"})).unwrap();
    s.execute("folder.label", &json!({"path": day1, "label": "blue"})).unwrap();
    s.execute("folder.rename", &json!({"path": trip, "name": "holiday"})).unwrap();
    assert_eq!(listed_label(&mut s, &dir.path("holiday")).as_deref(), Some("red"));
    assert_eq!(listed_label(&mut s, &dir.path("holiday/day1")).as_deref(), Some("blue"), "the folder inside goes along");
    assert_eq!(s.catalog.folder_record(&trip), None, "nothing stays behind");
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(listed_label(&mut s, &trip).as_deref(), Some("red"));
    assert_eq!(listed_label(&mut s, &day1).as_deref(), Some("blue"));
    s.execute("edit.redo", &json!({})).unwrap();
    assert_eq!(listed_label(&mut s, &dir.path("holiday")).as_deref(), Some("red"), "redo carries them again");
}

#[test]
fn labels_follow_a_moved_folder() {
    let dir = Scratch::new("label-move");
    std::fs::create_dir_all(dir.0.join("trip")).unwrap();
    std::fs::create_dir_all(dir.0.join("archive")).unwrap();
    let trip = dir.path("trip");
    let mut s = Session::new();
    add(&mut s, &format!("{trip}/a.jpg"));
    s.execute("folder.label", &json!({"path": trip, "label": "yellow"})).unwrap();
    s.execute("folder.move", &json!({"path": trip, "into": dir.path("archive")})).unwrap();
    assert_eq!(listed_label(&mut s, &dir.path("archive/trip")).as_deref(), Some("yellow"));
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(listed_label(&mut s, &trip).as_deref(), Some("yellow"), "undo moves it back with the folder");
    assert_eq!(s.catalog.folder_record(&dir.path("archive/trip")), None);
    s.execute("edit.redo", &json!({})).unwrap();
    assert_eq!(listed_label(&mut s, &dir.path("archive/trip")).as_deref(), Some("yellow"));
}

#[test]
fn a_label_comes_off_a_folder_whose_photos_are_gone() {
    let mut s = session();
    s.execute("folder.label", &json!({"path": "/pics/trip", "label": "red"})).unwrap();
    s.execute("library.removeFolder", &json!({"path": "/pics/trip"})).unwrap();
    s.execute("folder.label", &json!({"path": "/pics/trip", "label": "none"})).unwrap();
    assert_eq!(s.catalog.folder_record("/pics/trip"), None);
    assert!(s.execute("folder.label", &json!({"path": "/pics/trip", "label": "red"})).is_err(), "but a new one needs photos");
}

// ---- folder.create and photo.moveToFolder (issue #765)

/// A library photo of the real file `path` (written with `body`); returns its id.
fn add_file(s: &mut Session, path: &str, body: &str) -> lightcraft_catalog::PhotoId {
    std::fs::write(path, body).unwrap();
    let id = s.catalog.alloc_photo_id();
    let name = std::path::Path::new(path).file_name().unwrap().to_string_lossy().to_string();
    let p = Photo::new(id, Source::File { path: path.into() }, &name, "JPEG", 60, 40, "2026-01-01T10:00:00");
    s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    id
}

fn file_of(s: &Session, id: lightcraft_catalog::PhotoId) -> String {
    match &s.catalog.photo(id).unwrap().source {
        Source::File { path } => path.clone(),
        _ => panic!("not a file"),
    }
}

#[test]
fn a_folder_is_created_on_disk_and_undo_removes_it() {
    let dir = Scratch::new("create");
    let mut s = Session::new();
    let r = s.execute("folder.create", &json!({"parent": dir.path(""), "name": "picks"})).unwrap();
    assert_eq!(r["path"], dir.path("picks"));
    assert!(dir.0.join("picks").is_dir());
    assert!(s.execute("folder.create", &json!({"parent": dir.path(""), "name": "picks"})).unwrap_err().to_string().contains("already exists"));
    assert!(s.execute("folder.create", &json!({"parent": dir.path(""), "name": "../x"})).is_err(), "one name, no slashes");
    assert!(s.execute("folder.create", &json!({"parent": dir.path("nope"), "name": "x"})).unwrap_err().to_string().contains("not a folder"));
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(!dir.0.join("picks").exists(), "undo removes it");
    s.execute("edit.redo", &json!({})).unwrap();
    assert!(dir.0.join("picks").is_dir(), "redo makes it again");
    std::fs::write(dir.0.join("picks/note.txt"), "x").unwrap();
    assert!(s.execute("edit.undo", &json!({})).is_err(), "a folder with something in it is never removed");
    assert!(dir.0.join("picks/note.txt").is_file());
}

#[test]
fn photos_move_to_another_folder_with_their_sidecars_and_back_on_undo() {
    let dir = Scratch::new("move-photos");
    std::fs::create_dir_all(dir.0.join("in")).unwrap();
    std::fs::create_dir_all(dir.0.join("out")).unwrap();
    let mut s = Session::new();
    let a = add_file(&mut s, &dir.path("in/a.jpg"), "A");
    std::fs::write(dir.path("in/a.xmp"), "<x/>").unwrap();
    let b = add_file(&mut s, &dir.path("in/b.jpg"), "B");
    // a virtual copy shares its master's file and goes along
    let vc = s.catalog.alloc_photo_id();
    let p = Photo::new(vc, Source::File { path: dir.path("in/a.jpg") }, "a.jpg", "JPEG", 60, 40, "2026-01-01T10:00:00");
    s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    let r = s.execute("photo.moveToFolder", &json!({"ids": [a.0, b.0], "folder": dir.path("out")})).unwrap();
    assert_eq!(r["moved"], 2);
    assert_eq!(std::fs::read_to_string(dir.path("out/a.jpg")).unwrap(), "A");
    assert!(dir.0.join("out/a.xmp").is_file(), "the sidecar goes along");
    assert!(!dir.0.join("in/a.jpg").exists() && !dir.0.join("in/a.xmp").exists());
    assert_eq!(file_of(&s, a), dir.path("out/a.jpg"));
    assert_eq!(file_of(&s, b), dir.path("out/b.jpg"));
    assert_eq!(file_of(&s, vc), dir.path("out/a.jpg"), "the virtual copy follows");
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(file_of(&s, a), dir.path("in/a.jpg"));
    assert_eq!(file_of(&s, vc), dir.path("in/a.jpg"));
    assert!(dir.0.join("in/a.jpg").is_file() && dir.0.join("in/a.xmp").is_file() && dir.0.join("in/b.jpg").is_file());
    assert!(!dir.0.join("out/a.jpg").exists());
    s.execute("edit.redo", &json!({})).unwrap();
    assert!(dir.0.join("out/b.jpg").is_file());
    assert_eq!(file_of(&s, b), dir.path("out/b.jpg"));
}

#[test]
fn a_name_taken_in_the_destination_refuses_the_move_and_overwrites_nothing() {
    let dir = Scratch::new("move-clash");
    std::fs::create_dir_all(dir.0.join("in")).unwrap();
    std::fs::create_dir_all(dir.0.join("out")).unwrap();
    let mut s = Session::new();
    let a = add_file(&mut s, &dir.path("in/a.jpg"), "A");
    let b = add_file(&mut s, &dir.path("in/b.jpg"), "B");
    std::fs::write(dir.path("out/b.jpg"), "OTHER").unwrap();
    let e = s.execute("photo.moveToFolder", &json!({"ids": [a.0, b.0], "folder": dir.path("out")})).unwrap_err().to_string();
    assert!(e.contains("b.jpg already in") && e.contains("nothing was moved"), "{e}");
    assert_eq!(std::fs::read_to_string(dir.path("out/b.jpg")).unwrap(), "OTHER");
    assert!(dir.0.join("in/a.jpg").is_file() && !dir.0.join("out/a.jpg").exists(), "all or nothing");
    assert_eq!(file_of(&s, a), dir.path("in/a.jpg"));
}

#[test]
fn moving_to_a_missing_folder_or_a_missing_original_is_refused() {
    let dir = Scratch::new("move-missing");
    std::fs::create_dir_all(dir.0.join("in")).unwrap();
    let mut s = Session::new();
    let a = add_file(&mut s, &dir.path("in/a.jpg"), "A");
    let e = s.execute("photo.moveToFolder", &json!({"ids": [a.0], "folder": dir.path("nope")})).unwrap_err().to_string();
    assert!(e.contains("not a folder"), "{e}");
    std::fs::create_dir_all(dir.0.join("out")).unwrap();
    std::fs::remove_file(dir.path("in/a.jpg")).unwrap();
    let e = s.execute("photo.moveToFolder", &json!({"ids": [a.0], "folder": dir.path("out")})).unwrap_err().to_string();
    assert!(e.contains("missing or offline"), "{e}");
    assert_eq!(file_of(&s, a), dir.path("in/a.jpg"));
}

#[cfg(unix)]
#[test]
fn a_destination_without_write_permission_reports_an_error_and_keeps_everything() {
    use std::os::unix::fs::PermissionsExt;
    let dir = Scratch::new("move-perm");
    std::fs::create_dir_all(dir.0.join("in")).unwrap();
    std::fs::create_dir_all(dir.0.join("ro")).unwrap();
    let mut s = Session::new();
    let a = add_file(&mut s, &dir.path("in/a.jpg"), "A");
    std::fs::set_permissions(dir.0.join("ro"), std::fs::Permissions::from_mode(0o555)).unwrap();
    // running as root ignores permissions: nothing to check there
    let root = std::fs::write(dir.0.join("ro/probe"), "x").is_ok();
    if !root {
        assert!(s.execute("photo.moveToFolder", &json!({"ids": [a.0], "folder": dir.path("ro")})).is_err());
        assert!(dir.0.join("in/a.jpg").is_file());
        assert_eq!(file_of(&s, a), dir.path("in/a.jpg"));
        assert!(s.execute("folder.create", &json!({"parent": dir.path("ro"), "name": "x"})).is_err());
    }
    std::fs::set_permissions(dir.0.join("ro"), std::fs::Permissions::from_mode(0o755)).unwrap();
}

// ── Moving a folder refuses what it can't do, in plain words ─────────────────────────────────
//
// * A folder goes into a folder that is there: a missing one is refused and never created.
// * A folder or destination on a disk that isn't connected says so, rather than "not a folder".
// * No permission, another disk and other OS errors name the folder, with no `os error` number.
// * A folder that is, or holds, the open library says which.

/// A folder to move (`trip`, with a photo in it) and the session that knows it.
fn trip_session(dir: &Scratch) -> Session {
    std::fs::create_dir_all(dir.0.join("trip")).unwrap();
    std::fs::write(dir.0.join("trip/a.jpg"), b"x").unwrap();
    let mut s = Session::new();
    add(&mut s, &format!("{}/a.jpg", dir.path("trip")));
    s
}

/// A refusal: the words the person reads, which never show the command or an OS error number.
fn refusal(s: &mut Session, command: &str, params: serde_json::Value) -> String {
    let e = s.execute(command, &params).unwrap_err().to_string();
    assert!(!e.contains("invalid parameters") && !e.contains('`') && !e.contains("os error"), "plain words for the person: {e}");
    assert!(e.contains("Nothing was moved"), "{e}");
    e
}

/// A path on a disk that is not there (`/Volumes/<name>` on every platform's reading of it).
fn absent_disk() -> String {
    format!("/Volumes/lc-absent-disk-{}", std::process::id())
}

#[test]
fn a_move_into_a_folder_that_is_not_there_is_refused_and_creates_nothing() {
    let dir = Scratch::new("nodest");
    let mut s = trip_session(&dir);
    let undo = s.undo.len();
    let e = refusal(&mut s, "folder.move", json!({"path": dir.path("trip"), "into": dir.path("Archive")}));
    assert!(e.contains("“Archive” isn't there") && e.contains("is its disk connected?"), "{e}");
    assert!(!dir.0.join("Archive").exists(), "nothing is created");
    assert!(dir.0.join("trip/a.jpg").exists(), "nothing is moved");
    assert_eq!(s.undo.len(), undo, "and no undo step is left");
    // however deep the missing folders go
    let e = refusal(&mut s, "folder.move", json!({"path": dir.path("trip"), "into": dir.path("Archive/2026")}));
    assert!(e.contains("“2026” isn't there"), "{e}");
    assert!(!dir.0.join("Archive").exists());
}

#[test]
fn a_folder_that_is_not_there_is_not_called_offline() {
    let dir = Scratch::new("nosrc");
    let mut s = trip_session(&dir);
    std::fs::create_dir_all(dir.0.join("Archive")).unwrap();
    let e = refusal(&mut s, "folder.move", json!({"path": dir.path("gone"), "into": dir.path("Archive")}));
    assert!(e.contains("“gone” isn't there (moved or deleted?)") && !e.contains("connected"), "{e}");
    // a file is not a folder either
    let e = refusal(&mut s, "folder.rename", json!({"path": dir.path("trip/a.jpg"), "name": "b"}));
    assert!(e.contains("“a.jpg” is not a folder"), "{e}");
}

#[test]
fn a_folder_on_a_disk_that_is_not_connected_says_so() {
    let dir = Scratch::new("offsrc");
    let mut s = trip_session(&dir);
    std::fs::create_dir_all(dir.0.join("Archive")).unwrap();
    let off = absent_disk();
    let e = refusal(&mut s, "folder.move", json!({"path": format!("{off}/photos/Trip"), "into": dir.path("Archive")}));
    assert!(e.contains("“Trip” is on “lc-absent-disk-") && e.contains("which isn't connected") && e.contains("connect it"), "{e}");
    assert!(!e.contains("not a folder"), "{e}");
    let e = refusal(&mut s, "folder.rename", json!({"path": format!("{off}/photos/Trip"), "name": "Holiday"}));
    assert!(e.contains("which isn't connected"), "{e}");
}

#[test]
fn a_destination_on_a_disk_that_is_not_connected_says_so() {
    let dir = Scratch::new("offdest");
    let mut s = trip_session(&dir);
    let e = refusal(&mut s, "folder.move", json!({"path": dir.path("trip"), "into": format!("{}/Archive", absent_disk())}));
    assert!(e.contains("“Archive” is on “lc-absent-disk-") && e.contains("which isn't connected"), "{e}");
    assert!(!e.contains("isn't there"), "{e}");
    assert!(dir.0.join("trip/a.jpg").exists());
}

#[cfg(unix)]
#[test]
fn a_destination_without_permission_is_refused_naming_both_folders() {
    use std::os::unix::fs::PermissionsExt;
    let dir = Scratch::new("perm");
    let mut s = trip_session(&dir);
    std::fs::create_dir_all(dir.0.join("Archive")).unwrap();
    std::fs::set_permissions(dir.0.join("Archive"), std::fs::Permissions::from_mode(0o555)).unwrap();
    let writable = std::fs::write(dir.0.join("Archive/probe"), b"").is_ok();
    let r = s.execute("folder.move", &json!({"path": dir.path("trip"), "into": dir.path("Archive")}));
    std::fs::set_permissions(dir.0.join("Archive"), std::fs::Permissions::from_mode(0o755)).unwrap();
    if writable {
        return; // running as root: nothing is denied
    }
    let e = r.unwrap_err().to_string();
    assert!(e.contains("isn't allowed to move “trip” into “Archive”") && e.contains("permissions") && e.contains("Nothing was moved"), "{e}");
    assert!(!e.contains("os error") && !e.contains("invalid parameters"), "{e}");
    assert!(dir.0.join("trip/a.jpg").exists());
}

#[test]
fn a_move_to_another_disk_is_refused_and_says_what_to_do() {
    let dir = Scratch::new("exdev");
    std::fs::create_dir_all(dir.0.join("trip")).unwrap();
    std::fs::create_dir_all(dir.0.join("Archive")).unwrap();
    let denied = |_: &std::path::Path, _: &std::path::Path| Err(std::io::Error::from(std::io::ErrorKind::CrossesDevices));
    let e = crate::cmd::browse::rename_folder_with(&dir.path("trip"), &dir.path("Archive/trip"), &denied).unwrap_err();
    assert!(e.contains("“trip” is on a different disk than “Archive”") && e.contains("doesn't copy"), "{e}");
    assert!(e.contains("Finder") && e.contains("copy it there yourself") && e.contains("Nothing was moved"), "{e}");
    assert!(!e.contains("os error") && !e.contains("→"), "{e}");
}

#[test]
fn another_os_error_becomes_a_sentence_without_its_number() {
    let dir = Scratch::new("oserr");
    std::fs::create_dir_all(dir.0.join("trip")).unwrap();
    let fail = |_: &std::path::Path, _: &std::path::Path| Err(std::io::Error::from_raw_os_error(2));
    let e = crate::cmd::browse::rename_folder_with(&dir.path("trip"), &dir.path("holiday"), &fail).unwrap_err();
    assert!(e.starts_with("LightCraft couldn't rename “trip” to “holiday”: ") && e.ends_with(". Nothing was moved."), "{e}");
    assert!(!e.contains("os error") && !e.contains('→'), "{e}");
    // a reason that already ends in a full stop (Windows spells its errors so) doesn't double it
    let sentence = |_: &std::path::Path, _: &std::path::Path| Err(std::io::Error::other("The system cannot find the file specified."));
    let e = crate::cmd::browse::rename_folder_with(&dir.path("trip"), &dir.path("holiday"), &sentence).unwrap_err();
    assert!(e.ends_with("the system cannot find the file specified. Nothing was moved.") && !e.contains(".."), "{e}");
}

#[test]
fn a_folder_that_holds_the_open_library_names_the_library() {
    let dir = Scratch::new("holds");
    std::fs::create_dir_all(dir.0.join("Pictures/Lib")).unwrap();
    std::fs::create_dir_all(dir.0.join("Archive")).unwrap();
    let mut s = Session::new();
    s.open_library(dir.0.join("Pictures/Lib"), false).unwrap();
    let e = refusal(&mut s, "folder.move", json!({"path": dir.path("Pictures"), "into": dir.path("Archive")}));
    assert!(e.contains("“Pictures” holds the open library “Lib”"), "{e}");
    assert!(dir.0.join("Pictures/Lib").exists());
}

#[test]
fn a_folder_inside_the_open_library_is_said_to_be_part_of_it() {
    let dir = Scratch::new("inside");
    std::fs::create_dir_all(dir.0.join("Lib")).unwrap();
    std::fs::create_dir_all(dir.0.join("Archive")).unwrap();
    let mut s = Session::new();
    s.open_library(dir.0.join("Lib"), false).unwrap();
    std::fs::create_dir_all(dir.0.join("Lib/extra")).unwrap();
    let e = refusal(&mut s, "folder.move", json!({"path": dir.path("Lib/extra"), "into": dir.path("Archive")}));
    assert!(e.contains("“extra” is part of the open library “Lib”"), "{e}");
}

#[test]
fn the_library_folder_itself_and_a_rename_to_the_same_name_are_told_apart() {
    let dir = Scratch::new("same");
    std::fs::create_dir_all(dir.0.join("Lib")).unwrap();
    let mut s = Session::new();
    s.open_library(dir.0.join("Lib"), false).unwrap();
    let e = refusal(&mut s, "folder.rename", json!({"path": dir.path("Lib"), "name": "Renamed"}));
    assert!(e.contains("“Lib” is the open library's own folder") && e.contains("open another library first"), "{e}");
    // renaming a folder to the name it has says it is taken, not that it can't go into itself
    std::fs::create_dir_all(dir.0.join("trip")).unwrap();
    let e = s.execute("folder.rename", &json!({"path": dir.path("trip"), "name": "trip"})).unwrap_err().to_string();
    assert!(e.contains("already exists") && !e.contains("into itself"), "{e}");
}

#[test]
fn a_destination_that_is_a_file_or_a_whole_offline_disk_is_worded_for_what_it_is() {
    let dir = Scratch::new("dest-kinds");
    let mut s = trip_session(&dir);
    std::fs::write(dir.0.join("note.txt"), b"x").unwrap();
    let e = refusal(&mut s, "folder.move", json!({"path": dir.path("trip"), "into": dir.path("note.txt")}));
    assert!(e.contains("“note.txt” is not a folder") && !e.contains("isn't there"), "{e}");
    let off = absent_disk();
    let e = refusal(&mut s, "folder.move", json!({"path": dir.path("trip"), "into": off}));
    assert!(e.contains("lc-absent-disk-") && e.contains("” isn't connected.") && !e.contains("is on"), "{e}");
    // a folder in the disk that is named like the disk is a folder, not the disk
    let same = format!("{off}/{}", off.rsplit('/').next().unwrap());
    let e = refusal(&mut s, "folder.move", json!({"path": dir.path("trip"), "into": same}));
    assert!(e.contains("is on “lc-absent-disk-") && e.contains("which isn't connected"), "{e}");
}

#[test]
fn a_missing_folder_on_a_disk_that_answers_with_an_error_is_not_called_deleted() {
    use crate::cmd::browse::source_refusal;
    let from = format!("{}/photos/Trip", absent_disk());
    // the mount point is there but the folder isn't (or the mount is stale and answers with an error):
    // it may be gone or its disk may not be answering, and the sentence says both
    for answer in [Some(true), None] {
        let e = source_refusal(&from, &|_| answer);
        assert!(e.contains("“Trip” isn't there (moved or deleted, or is its disk connected?)") && !e.contains("which isn't connected"), "{e}");
    }
    // only the file system saying "not there" for the mount point makes the disk offline
    let e = source_refusal(&from, &|_| Some(false));
    assert!(e.contains("“Trip” is on “lc-absent-disk-") && e.contains("which isn't connected"), "{e}");
    // a folder that is not on a disk or share keeps the short wording
    let e = source_refusal(&Scratch::new("plainsrc").path("gone"), &|_| None);
    assert!(e.contains("isn't there (moved or deleted?)"), "{e}");
}

#[test]
fn a_folder_moved_into_itself_is_named_in_the_refusal() {
    let dir = Scratch::new("itself");
    let mut s = trip_session(&dir);
    std::fs::create_dir_all(dir.0.join("trip/inner")).unwrap();
    let e = refusal(&mut s, "folder.move", json!({"path": dir.path("trip"), "into": dir.path("trip/inner")}));
    assert!(e.contains("“trip” can't be moved into itself"), "{e}");
}

#[cfg(unix)]
#[test]
fn a_path_through_a_link_to_the_open_librarys_folder_is_refused_like_the_real_one() {
    let dir = Scratch::new("alias");
    std::fs::create_dir_all(dir.0.join("real/Lib")).unwrap();
    std::fs::create_dir_all(dir.0.join("Archive")).unwrap();
    std::os::unix::fs::symlink(dir.0.join("real"), dir.0.join("alias")).unwrap();
    let mut s = Session::new();
    s.open_library(dir.0.join("real/Lib"), false).unwrap();
    // the library's own folder, and the folder that holds it, spelled through the link
    let e = refusal(&mut s, "folder.rename", json!({"path": dir.path("alias/Lib"), "name": "Renamed"}));
    assert!(e.contains("is the open library's own folder"), "{e}");
    std::os::unix::fs::symlink(&dir.0, dir.0.join("up")).unwrap();
    let e = refusal(&mut s, "folder.move", json!({"path": dir.path("up/real"), "into": dir.path("Archive")}));
    assert!(e.contains("holds the open library"), "{e}");
    assert!(dir.0.join("real/Lib").is_dir(), "the library stayed where it is");
}

#[test]
fn a_case_only_rename_that_cannot_be_put_back_says_where_the_folder_is() {
    let dir = Scratch::new("caseback");
    std::fs::create_dir_all(dir.0.join("trip")).unwrap();
    let calls = std::cell::Cell::new(0);
    // the first step (to the temporary name) works; the second and the way back fail
    let flaky = |a: &std::path::Path, b: &std::path::Path| {
        calls.set(calls.get() + 1);
        if calls.get() == 1 { std::fs::rename(a, b) } else { Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)) }
    };
    let e = crate::cmd::browse::rename_folder_with(&dir.path("trip"), &dir.path("TRIP"), &flaky).unwrap_err();
    assert!(!e.contains("Nothing was moved") && !e.contains("os error"), "{e}");
    let tmp = dir.0.join(format!(".lc-rename-{}", std::process::id()));
    assert!(tmp.is_dir(), "the folder is under the temporary name");
    assert!(e.contains(&tmp.display().to_string()) && e.contains("rename it by hand"), "{e}");
}

#[test]
fn a_folder_that_cannot_be_looked_at_is_not_called_missing() {
    // the disk answers with an error (a stale share): not "isn't there"
    let e = crate::cmd::browse::look_refusal("/Volumes/nas/Trip", &std::io::Error::from(std::io::ErrorKind::TimedOut)).unwrap();
    assert!(e.contains("“Trip” can't be reached right now") && e.contains("check that its disk is connected") && !e.contains("isn't there"), "{e}");
    assert!(!e.contains("os error"), "{e}");
    // no permission to look
    let e = crate::cmd::browse::look_refusal("/x/Trip", &std::io::Error::from(std::io::ErrorKind::PermissionDenied)).unwrap();
    assert!(e.contains("isn't allowed to open “Trip”") && e.contains("permissions"), "{e}");
    // simply not there
    assert!(crate::cmd::browse::look_refusal("/x/Trip", &std::io::Error::from(std::io::ErrorKind::NotFound)).is_none());
    // or runs through a file
    assert!(crate::cmd::browse::look_refusal("/x/Trip", &std::io::Error::from(std::io::ErrorKind::NotADirectory)).is_none());
}

#[cfg(unix)]
#[test]
fn a_source_or_destination_without_permission_to_look_is_not_called_missing() {
    use std::os::unix::fs::PermissionsExt;
    let dir = Scratch::new("nolook");
    let mut s = trip_session(&dir);
    std::fs::create_dir_all(dir.0.join("locked/Trip")).unwrap();
    std::fs::create_dir_all(dir.0.join("locked/Archive")).unwrap();
    std::fs::set_permissions(dir.0.join("locked"), std::fs::Permissions::from_mode(0o000)).unwrap();
    let readable = std::fs::metadata(dir.0.join("locked/Trip")).is_ok();
    let src = s.execute("folder.move", &json!({"path": dir.path("locked/Trip"), "into": dir.path("trip")}));
    let dst = s.execute("folder.move", &json!({"path": dir.path("trip"), "into": dir.path("locked/Archive")}));
    std::fs::set_permissions(dir.0.join("locked"), std::fs::Permissions::from_mode(0o755)).unwrap();
    if readable {
        return; // running as root: nothing is denied
    }
    for (what, r) in [("source", src), ("destination", dst)] {
        let e = r.unwrap_err().to_string();
        assert!(e.contains("isn't allowed to open") && e.contains("Nothing was moved") && !e.contains("isn't there"), "{what}: {e}");
    }
}

#[cfg(unix)]
#[test]
fn a_link_the_library_was_opened_through_cannot_be_renamed_away_from_it() {
    let dir = Scratch::new("openlink");
    std::fs::create_dir_all(dir.0.join("real/Lib")).unwrap();
    std::os::unix::fs::symlink(dir.0.join("real"), dir.0.join("lnk")).unwrap();
    let mut s = Session::new();
    s.open_library(dir.0.join("lnk/Lib"), false).unwrap();
    let e = refusal(&mut s, "folder.rename", json!({"path": dir.path("lnk"), "name": "gone"}));
    assert!(e.contains("“lnk” holds the open library “Lib”"), "{e}");
    assert!(dir.0.join("lnk").exists(), "the link stayed");
}

#[test]
fn a_destination_that_runs_through_a_file_is_not_called_unreachable() {
    let dir = Scratch::new("thruFile");
    let mut s = trip_session(&dir);
    std::fs::write(dir.0.join("notes.txt"), b"x").unwrap();
    let e = refusal(&mut s, "folder.move", json!({"path": dir.path("trip"), "into": dir.path("notes.txt/sub")}));
    assert!(e.contains("“sub” isn't there") && !e.contains("can't be reached"), "{e}");
}

#[cfg(unix)]
#[test]
fn a_link_the_library_was_opened_through_is_found_when_reached_through_another_link() {
    let dir = Scratch::new("twolinks");
    std::fs::create_dir_all(dir.0.join("real/Lib")).unwrap();
    std::os::unix::fs::symlink(dir.0.join("real"), dir.0.join("lnk")).unwrap();
    std::os::unix::fs::symlink(&dir.0, dir.0.join("up")).unwrap();
    let mut s = Session::new();
    s.open_library(dir.0.join("lnk/Lib"), false).unwrap();
    let e = refusal(&mut s, "folder.rename", json!({"path": dir.path("up/lnk"), "name": "gone"}));
    assert!(e.contains("holds the open library"), "{e}");
    assert!(dir.0.join("lnk").exists(), "the link stayed");
}
