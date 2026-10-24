//! Several albums, several folders, several keywords or several dates shown at once: what an
//! agent asks for today, and what Cmd/Ctrl+click and Shift+click in the sidebars will ask for
//! once that feature lands. The grid shows the photos of any of them, each once.
//!
//! Scenarios:
//! - Given two albums that share a photo, when both are shown (`library.source {kind: "album",
//!   ids}`), then the grid lists their photos once each, and `library.state` names both.
//! - Given several albums shown, when one album is shown the usual way, then only that one is.
//! - Given several albums shown, when one of them is deleted, then the others stay shown; when
//!   the last is deleted, the grid goes back to All Photos.
//! - An album that does not exist among those asked for is refused, and the view stays as it was.
//! - Given several albums shown, then the grid follows the sort, not one album's own order.
//! - Given two folders of the library, when both are shown (`paths`), then the grid lists the
//!   photos of both; removing one from the library leaves the other shown.
//! - Given several keywords in the filter (`keywords`), then photos with any of them are shown;
//!   a renamed keyword is followed, a deleted one leaves the filter, and one chip clears them all.
//! - Given several dates in the filter (`dates`), then photos taken on any of them are shown.
//! - Given several albums shown, when the view is saved as a smart album, then that album shows
//!   the same photos.
//! - Several albums are still the source after the library is closed and opened again.
//! - Given a smart album saved from a view of several albums, when its rules are replaced in
//!   the rules dialog, then it is still limited to all of those albums.
//! - Albums in the filter itself show as a chip that clears them, and are refused while an
//!   album is the source (they would be ignored); the one album of the filter goes with it.
//! - Given several folders in the filter, when one is renamed on disk, then the filter follows.

use lightcraft_catalog::{AlbumId, Op, Photo, PhotoId, Source};
use serde_json::{Value, json};

use crate::{LibrarySource, Session, filter_chips};

/// Photos `1..=n`, one a day from 2026-01-01 (so the newest has the highest id).
fn photos(s: &mut Session, n: u64) -> Vec<u64> {
    (1..=n)
        .map(|day| {
            let id = s.catalog.alloc_photo_id();
            let mut p = Photo::new(id, Source::Demo { scene: 1 }, &format!("p{day}.jpg"), "JPEG", 60, 40, "2026-02-01T10:00:00");
            p.captured = Some(format!("2026-01-{day:02}T10:00:00"));
            s.commit("Add Photo", Op::AddPhoto { photo: Box::new(p) }).unwrap();
            id.0
        })
        .collect()
}

fn album(s: &mut Session, name: &str, ids: &[u64]) -> u64 {
    let id = s.execute("album.create", &json!({"name": name})).unwrap()["id"].as_u64().unwrap();
    // the command wants a selection to be enabled, whatever `ids` says
    s.selection.ids = ids.iter().map(|i| PhotoId(*i)).collect();
    s.execute("album.addPhotos", &json!({"id": id, "ids": ids})).unwrap();
    s.selection.ids.clear();
    id
}

/// What the grid shows, in id order.
fn shown(s: &mut Session) -> Vec<u64> {
    let mut v: Vec<u64> = s.visible_cloned().iter().map(|p| p.0).collect();
    v.sort_unstable();
    v
}

fn state(s: &mut Session) -> Value {
    s.execute("library.state", &json!({})).unwrap()
}

/// Two albums sharing photo 2: `(ids, a, b)`.
fn two_albums(s: &mut Session) -> (Vec<u64>, u64, u64) {
    let ids = photos(s, 5);
    let a = album(s, "Alps", &[ids[0], ids[1]]);
    let b = album(s, "Beach", &[ids[1], ids[2]]);
    (ids, a, b)
}

#[test]
fn several_albums_show_the_photos_of_any_of_them_once() {
    let mut s = Session::new();
    let (ids, a, b) = two_albums(&mut s);
    let r = s.execute("library.source", &json!({"kind": "album", "ids": [a, b]})).unwrap();
    assert_eq!(r["count"], 3);
    assert_eq!(shown(&mut s), vec![ids[0], ids[1], ids[2]]);
    // the last one named is the one a single-album action takes
    assert_eq!(s.source, LibrarySource::Album(AlbumId(b)));
    assert_eq!(s.source_albums(), vec![AlbumId(a), AlbumId(b)]);
    let st = state(&mut s);
    assert_eq!(st["albums"], json!([a, b]));
    assert_eq!(st["sourceLabel"], "2 albums");
    assert_eq!(s.source_total(), Some(3), "the count before the filter bar narrows it");
}

#[test]
fn showing_one_album_again_lets_go_of_the_others() {
    let mut s = Session::new();
    let (ids, a, b) = two_albums(&mut s);
    s.execute("library.source", &json!({"kind": "album", "ids": [a, b]})).unwrap();
    s.execute("library.source", &json!({"kind": "album", "id": a})).unwrap();
    assert_eq!(s.source_albums(), vec![AlbumId(a)]);
    assert_eq!(shown(&mut s), vec![ids[0], ids[1]]);
    // …and so does another kind of source
    s.execute("library.source", &json!({"kind": "album", "ids": [a, b]})).unwrap();
    s.execute("library.source", &json!({"kind": "all"})).unwrap();
    assert!(s.source_albums().is_empty());
    assert_eq!(shown(&mut s).len(), 5);
    assert_eq!(state(&mut s)["albums"], json!([]));
}

#[test]
fn an_album_named_twice_is_shown_once_and_one_alone_is_the_usual_source() {
    let mut s = Session::new();
    let (_, a, b) = two_albums(&mut s);
    s.execute("library.source", &json!({"kind": "album", "ids": [a, b, a]})).unwrap();
    assert_eq!(s.source_albums(), vec![AlbumId(b), AlbumId(a)], "the last naming is the one that counts");
    s.execute("library.source", &json!({"kind": "album", "ids": [a]})).unwrap();
    assert_eq!((s.source, s.source_albums()), (LibrarySource::Album(AlbumId(a)), vec![AlbumId(a)]));
}

#[test]
fn a_deleted_album_leaves_the_others_shown() {
    let mut s = Session::new();
    let (ids, a, b) = two_albums(&mut s);
    s.execute("library.source", &json!({"kind": "album", "ids": [a, b]})).unwrap();
    // the one single-album actions take goes: the other takes its place
    s.execute("album.delete", &json!({"id": b})).unwrap();
    assert_eq!(s.source, LibrarySource::Album(AlbumId(a)));
    assert_eq!(s.source_albums(), vec![AlbumId(a)]);
    assert_eq!(shown(&mut s), vec![ids[0], ids[1]]);
    s.execute("album.delete", &json!({"id": a})).unwrap();
    assert_eq!(s.source, LibrarySource::All);
    assert!(s.source_albums().is_empty());
    // undo brings an album back, not the view of it
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.source, LibrarySource::All);
}

#[test]
fn a_saved_view_of_albums_that_are_all_gone_shows_all_photos_not_an_empty_grid() {
    let mut s = Session::new();
    let (ids, _, _) = two_albums(&mut s);
    // what a restored view.json can hold when every album it names was deleted meanwhile
    s.source = LibrarySource::Album(AlbumId(9998));
    s.also_albums = vec![AlbumId(9999)];
    assert_eq!(shown(&mut s).len(), ids.len());
    assert_eq!(s.source, LibrarySource::All);
    assert!(s.source_albums().is_empty());
    // and the same for one album that is gone with none beside it
    s.source = LibrarySource::Album(AlbumId(9999));
    s.also_albums.clear();
    assert_eq!(shown(&mut s).len(), ids.len());
    assert_eq!(s.source, LibrarySource::All);
}

#[test]
fn an_album_that_is_not_there_is_refused_and_the_view_stays() {
    let mut s = Session::new();
    let (ids, a, _) = two_albums(&mut s);
    s.execute("library.source", &json!({"kind": "album", "id": a})).unwrap();
    let e = s.execute("library.source", &json!({"kind": "album", "ids": [a, 9999]})).unwrap_err().to_string();
    assert!(e.contains("9999"), "the refusal names the album: {e}");
    assert_eq!(s.source_albums(), vec![AlbumId(a)]);
    assert_eq!(shown(&mut s), vec![ids[0], ids[1]]);
    for bad in [json!([]), json!("x"), json!([a, "x"]), json!([-1])] {
        assert!(s.execute("library.source", &json!({"kind": "album", "ids": bad})).is_err(), "{bad}");
    }
    assert_eq!(s.source_albums(), vec![AlbumId(a)]);
}

#[test]
fn several_albums_follow_the_sort_not_an_album_s_own_order() {
    let mut s = Session::new();
    let ids = photos(&mut s, 4);
    // by hand: newest first in A
    let a = album(&mut s, "A", &[ids[3], ids[0]]);
    let b = album(&mut s, "B", &[ids[2], ids[1]]);
    s.execute("library.source", &json!({"kind": "album", "ids": [a, b]})).unwrap();
    s.execute("library.sort", &json!({"key": "captureDate", "ascending": true})).unwrap();
    let order: Vec<u64> = s.visible_cloned().iter().map(|p| p.0).collect();
    assert_eq!(order, ids);
}

fn add_file(s: &mut Session, path: &str) -> u64 {
    let id = s.catalog.alloc_photo_id();
    let p = Photo::new(id, Source::File { path: path.into() }, "x.jpg", "JPEG", 60, 40, "2026-01-01T10:00:00");
    s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    id.0
}

#[test]
fn several_folders_show_the_photos_of_any_of_them() {
    let mut s = Session::new();
    let alps = add_file(&mut s, "/pics/2025/alps/a.jpg");
    let deep = add_file(&mut s, "/pics/2025/alps/day1/b.jpg");
    let beach = add_file(&mut s, "/pics/2026/beach/c.jpg");
    let other = add_file(&mut s, "/pics/2026/town/d.jpg");
    let r = s.execute("library.source", &json!({"kind": "libraryFolder", "paths": ["/pics/2025/alps", "/pics/2026/beach"]})).unwrap();
    assert_eq!(r["count"], 3);
    assert_eq!(shown(&mut s), vec![alps, deep, beach]);
    assert_eq!(s.source, LibrarySource::LibraryFolder);
    assert_eq!(s.source_library_folders(), vec!["/pics/2025/alps".to_string(), "/pics/2026/beach".to_string()]);
    let st = state(&mut s);
    assert_eq!(st["libraryFolders"], json!(["/pics/2025/alps", "/pics/2026/beach"]));
    assert_eq!(st["libraryFolder"], "/pics/2026/beach", "the last one named is the one a single-folder action takes");
    assert_eq!(st["sourceLabel"], "2 folders");
    assert_eq!(s.source_total(), Some(3));
    // a folder with no photo of the library is refused, by name, and the view stays
    let e = s.execute("library.source", &json!({"kind": "libraryFolder", "paths": ["/pics/2026/town", "/nowhere"]})).unwrap_err().to_string();
    assert!(e.contains("/nowhere"), "{e}");
    assert_eq!(shown(&mut s), vec![alps, deep, beach]);
    // one of the folders shown leaves the library: the other stays
    s.execute("library.removeFolder", &json!({"path": "/pics/2026/beach"})).unwrap();
    assert_eq!(s.source_library_folders(), vec!["/pics/2025/alps".to_string()]);
    assert_eq!(shown(&mut s), vec![alps, deep]);
    // one folder the usual way lets go of the rest
    s.execute("library.source", &json!({"kind": "libraryFolder", "paths": ["/pics/2025/alps", "/pics/2026/town"]})).unwrap();
    s.execute("library.source", &json!({"kind": "libraryFolder", "path": "/pics/2026/town"})).unwrap();
    assert_eq!(shown(&mut s), vec![other]);
    assert_eq!(s.source_library_folders(), vec!["/pics/2026/town".to_string()]);
}

fn tag(s: &mut Session, id: u64, keywords: &[&str]) {
    // the command wants a selection to be enabled, whatever `ids` says
    s.selection.ids = vec![PhotoId(id)];
    s.execute("photo.setMeta", &json!({"ids": [id], "addKeywords": keywords})).unwrap();
    s.selection.ids.clear();
}

#[test]
fn several_keywords_show_the_photos_with_any_of_them() {
    let mut s = Session::new();
    let ids = photos(&mut s, 4);
    tag(&mut s, ids[0], &["alps"]);
    tag(&mut s, ids[1], &["beach", "alps"]);
    tag(&mut s, ids[2], &["trips|italy"]);
    let r = s.execute("library.filter", &json!({"keywords": ["alps", "trips"]})).unwrap();
    assert_eq!(r["count"], 3, "a keyword stands for the keywords below it too");
    assert_eq!(shown(&mut s), vec![ids[0], ids[1], ids[2]]);
    // the single keyword is one more of them, not a second condition
    s.execute("library.filter", &json!({"keyword": "beach", "keywords": ["trips"]})).unwrap();
    assert_eq!(shown(&mut s), vec![ids[1], ids[2]]);
    // one chip, and clearing it clears them all
    let chips = filter_chips(&s.filter, &s.catalog);
    assert_eq!(chips.len(), 1);
    assert_eq!(chips[0].label, "Keyword: beach or trips");
    s.execute("library.filter", &chips[0].clear.clone()).unwrap();
    assert_eq!(shown(&mut s).len(), 4);
    assert!(s.filter.keyword.is_none() && s.filter.keywords.is_empty());
}

#[test]
fn several_keywords_follow_a_rename_and_lose_a_deleted_one() {
    let mut s = Session::new();
    let ids = photos(&mut s, 3);
    tag(&mut s, ids[0], &["alps"]);
    tag(&mut s, ids[1], &["beach"]);
    s.execute("library.filter", &json!({"keywords": ["alps", "beach"]})).unwrap();
    s.execute("keyword.rename", &json!({"from": "alps", "to": "mountains"})).unwrap();
    assert_eq!(s.filter.keywords, vec!["mountains".to_string(), "beach".to_string()]);
    assert_eq!(shown(&mut s), vec![ids[0], ids[1]]);
    s.execute("keyword.delete", &json!({"keyword": "beach"})).unwrap();
    assert_eq!(s.filter.keywords, vec!["mountains".to_string()]);
    assert_eq!(shown(&mut s), vec![ids[0]]);
}

#[test]
fn several_dates_show_the_photos_of_any_of_them() {
    let mut s = Session::new();
    let ids = photos(&mut s, 5);
    s.execute("library.filter", &json!({"dates": ["2026-01-02", "2026-01-04"]})).unwrap();
    assert_eq!(shown(&mut s), vec![ids[1], ids[3]]);
    s.execute("library.filter", &json!({"date": "2026-01-05", "dates": ["2026-01-02"]})).unwrap();
    assert_eq!(shown(&mut s), vec![ids[1], ids[4]]);
    let chips = filter_chips(&s.filter, &s.catalog);
    assert_eq!(chips.len(), 1);
    assert_eq!(chips[0].label, "Date: January 5, 2026 or January 2, 2026");
    s.execute("library.filter", &chips[0].clear.clone()).unwrap();
    assert_eq!(shown(&mut s).len(), 5);
}

#[test]
fn several_albums_saved_as_a_smart_album_show_the_same_photos() {
    let mut s = Session::new();
    let (ids, a, b) = two_albums(&mut s);
    s.execute("library.source", &json!({"kind": "album", "ids": [a, b]})).unwrap();
    let smart = s.execute("album.createSmart", &json!({"name": "Both"})).unwrap()["id"].as_u64().unwrap();
    s.execute("library.source", &json!({"kind": "album", "id": smart})).unwrap();
    assert_eq!(shown(&mut s), vec![ids[0], ids[1], ids[2]]);
    // …and follows what the albums hold
    s.selection.ids = vec![PhotoId(ids[4])];
    s.execute("album.addPhotos", &json!({"id": a, "ids": [ids[4]]})).unwrap();
    assert_eq!(shown(&mut s), vec![ids[0], ids[1], ids[2], ids[4]]);
}

#[test]
fn several_albums_are_still_the_source_after_reopening() {
    /// Gone with the test, however it ends.
    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let scratch = Scratch(std::env::temp_dir().join(format!("lc-multi-source-{}", std::process::id())));
    let dir = scratch.0.clone();
    let _ = std::fs::remove_dir_all(&dir);
    let mut s = Session::new();
    s.open_library(&dir, false).unwrap();
    let (_, a, b) = two_albums(&mut s);
    s.execute("library.source", &json!({"kind": "album", "ids": [a, b]})).unwrap();
    let want = shown(&mut s);
    s.save_view();
    drop(s);
    let mut back = Session::new();
    back.open_library(&dir, false).unwrap();
    assert_eq!(back.source_albums(), vec![AlbumId(a), AlbumId(b)]);
    assert_eq!(shown(&mut back), want);
}

#[test]
fn replacing_the_rules_of_a_saved_view_keeps_all_its_albums() {
    let mut s = Session::new();
    let (ids, a, b) = two_albums(&mut s);
    s.execute("library.source", &json!({"kind": "album", "ids": [a, b]})).unwrap();
    let smart = s.execute("album.createSmart", &json!({"name": "Both"})).unwrap()["id"].as_u64().unwrap();
    // what the rules dialog sends: its rules, in place of the ones before
    s.execute("album.setRules", &json!({"id": smart, "replace": true, "rules": {"rating": 0}})).unwrap();
    s.execute("library.source", &json!({"kind": "album", "id": smart})).unwrap();
    assert_eq!(shown(&mut s), vec![ids[0], ids[1], ids[2]], "still both albums");
    // …unless the call says to drop them
    s.execute("album.setRules", &json!({"id": smart, "replace": true, "rules": {"albums": []}})).unwrap();
    assert_eq!(shown(&mut s).len(), 2, "the one album left");
}

#[test]
fn albums_in_the_filter_show_as_a_chip_and_are_refused_beside_an_album_source() {
    let mut s = Session::new();
    let (ids, a, b) = two_albums(&mut s);
    s.execute("library.filter", &json!({"albums": [a, b]})).unwrap();
    assert_eq!(shown(&mut s), vec![ids[0], ids[1], ids[2]]);
    let chips = filter_chips(&s.filter, &s.catalog);
    assert_eq!(chips.iter().map(|c| c.label.as_str()).collect::<Vec<_>>(), vec!["Album: Alps or Beach"]);
    s.execute("library.filter", &chips[0].clear.clone()).unwrap();
    assert_eq!(shown(&mut s).len(), 5);
    // while an album is the source its albums are what is shown: more in the filter would be ignored
    s.execute("library.source", &json!({"kind": "album", "id": a})).unwrap();
    let e = s.execute("library.filter", &json!({"albums": [b]})).unwrap_err().to_string();
    assert!(e.contains("already shown"), "{e}");
    assert!(s.filter.albums.is_empty());
    // the single `album` is ignored the same way; null still clears it
    let e = s.execute("library.filter", &json!({"album": b})).unwrap_err().to_string();
    assert!(e.contains("already shown"), "{e}");
    assert_eq!(s.filter.album, None);
    s.execute("library.filter", &json!({"album": null})).unwrap();
    // albums left in the filter go when an album becomes the source, and other filters still work
    s.execute("library.source", &json!({"kind": "all"})).unwrap();
    s.execute("library.filter", &json!({"albums": [b]})).unwrap();
    s.execute("library.source", &json!({"kind": "album", "id": a})).unwrap();
    assert!(s.filter.albums.is_empty());
    s.execute("library.filter", &json!({"rating": 0})).unwrap();
    assert_eq!(shown(&mut s), vec![ids[0], ids[1]]);
}

#[test]
fn several_folders_in_the_filter_and_as_the_source_follow_a_renamed_folder() {
    /// Gone with the test, however it ends.
    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let dir = Scratch(std::env::temp_dir().join(format!("lc-multi-source-rename-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&dir.0);
    std::fs::create_dir_all(dir.0.join("trip")).unwrap();
    std::fs::create_dir_all(dir.0.join("home")).unwrap();
    let at = |rel: &str| dir.0.join(rel).to_string_lossy().to_string();
    let key = lightcraft_catalog::query::folder_key;
    let mut s = Session::new();
    add_file(&mut s, &format!("{}/a.jpg", at("trip")));
    add_file(&mut s, &format!("{}/b.jpg", at("home")));
    // as a filter
    s.execute("library.filter", &json!({"libraryFolders": [at("trip"), at("home")]})).unwrap();
    s.execute("folder.rename", &json!({"path": at("trip"), "name": "holiday"})).unwrap();
    assert_eq!(s.filter.library_folders.iter().map(|f| key(f)).collect::<Vec<_>>(), vec![key(&at("holiday")), key(&at("home"))]);
    assert_eq!(s.visible().len(), 2, "still the photos of both");
    // as the source
    s.execute("library.clearFilter", &json!({})).unwrap();
    s.execute("library.source", &json!({"kind": "libraryFolder", "paths": [at("holiday"), at("home")]})).unwrap();
    s.execute("folder.rename", &json!({"path": at("holiday"), "name": "trip"})).unwrap();
    assert_eq!(s.source_library_folders().iter().map(|f| key(f)).collect::<Vec<_>>(), vec![key(&at("trip")), key(&at("home"))]);
    assert_eq!(s.visible().len(), 2);
}

#[test]
fn several_albums_follow_a_small_edit_instead_of_being_worked_out_whole() {
    let mut s = Session::new();
    let (ids, a, b) = two_albums(&mut s);
    s.execute("library.source", &json!({"kind": "album", "ids": [a, b]})).unwrap();
    assert_eq!(shown(&mut s), vec![ids[0], ids[1], ids[2]]);
    let (followed, whole) = (s.view_followed, s.view_worked_out);
    s.selection.ids = [PhotoId(ids[1])].into_iter().collect();
    s.execute("photo.rate", &json!({"ids": [ids[1]], "rating": 5})).unwrap();
    s.selection.ids.clear();
    assert_eq!(shown(&mut s), vec![ids[0], ids[1], ids[2]]);
    assert_eq!((s.view_followed, s.view_worked_out), (followed + 1, whole), "several albums have no order of their own");
}

#[test]
fn a_single_album_left_in_the_filter_does_not_outlive_an_album_source() {
    let mut s = Session::new();
    let (_, a, b) = two_albums(&mut s);
    s.execute("library.filter", &json!({"album": a})).unwrap();
    s.execute("library.source", &json!({"kind": "album", "id": b})).unwrap();
    assert_eq!(s.filter.album, None, "no chip for an album that limits nothing");
}

#[test]
fn albums_in_a_filter_put_aside_do_not_come_back_beside_an_album_source() {
    let mut s = Session::new();
    let (ids, a, b) = two_albums(&mut s);
    // Given albums in the filter, put aside by Enable Filters, and an album then shown as the source
    s.execute("library.filter", &json!({"albums": [b]})).unwrap();
    s.execute("library.toggleFilters", &json!({})).unwrap();
    s.execute("library.source", &json!({"kind": "album", "id": a})).unwrap();
    // When the filter is brought back
    s.execute("library.toggleFilters", &json!({})).unwrap();
    // Then the albums stay out of it, as they do for a filter set while an album is shown
    assert!(s.filter.albums.is_empty(), "{:?}", s.filter.albums);
    assert_eq!(shown(&mut s), vec![ids[0], ids[1]]);
    assert!(filter_chips(&s.filter, &s.catalog).is_empty());
}
