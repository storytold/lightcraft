//! Acting on several sidebar rows at once: the commands a context menu runs when several albums,
//! folders or keywords are chosen. Each is one step to undo, checks everything before it changes
//! anything, and a refusal names what it refused.
//!
//! Scenarios:
//! - Given three albums, when two are deleted at once (`album.delete {ids}`), then both are gone,
//!   the third stays, and one undo brings both back.
//! - Given a folder of albums and an album inside it both named, then deleting them deletes the
//!   folder with what is in it, once.
//! - An album that does not exist among those to delete is refused by name, and nothing is deleted.
//! - Given two albums, when both are moved into a folder (`album.move {ids, parent}`), then both
//!   are inside it, and one undo puts both back; a folder is not moved into itself, and then
//!   nothing moves.
//! - Given keywords on photos, when several are deleted at once (`keyword.delete {keywords}`),
//!   then every photo loses them and one undo gives them back; one that is no keyword refuses the
//!   whole call and nothing is deleted (one keyword that is no keyword still changes nothing).
//! - Given several keywords, when they are moved inside another (`keyword.move {keywords,
//!   parent}`), then each is nested there with its photos, as one undo step; a name that is taken
//!   there, or two of them with one name, is refused and nothing moves.
//! - Given two folders of the library, when both are removed at once (`library.removeFolder
//!   {paths}`), then their photos go to Recently Deleted as one undo step; one that is not a
//!   folder of the library refuses the whole call.
//! - Given two folders, when both get a colour label at once (`folder.label {paths}`), then both
//!   show it, and one undo takes both off.

use lightcraft_catalog::{AlbumId, Op, Photo, PhotoId, Source};
use serde_json::json;

use crate::Session;

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

fn album(s: &mut Session, params: serde_json::Value) -> u64 {
    s.execute("album.create", &params).unwrap()["id"].as_u64().unwrap()
}

fn has_album(s: &Session, id: u64) -> bool {
    s.catalog.album(AlbumId(id)).is_some()
}

fn parent_of(s: &Session, id: u64) -> Option<u64> {
    s.catalog.album(AlbumId(id)).unwrap().parent.map(|p| p.0)
}

fn tag(s: &mut Session, id: u64, keywords: &[&str]) {
    // the command wants a selection to be enabled, whatever `ids` says
    s.selection.ids = vec![PhotoId(id)];
    s.execute("photo.setMeta", &json!({"ids": [id], "addKeywords": keywords})).unwrap();
    s.selection.ids.clear();
}

fn keywords_of(s: &Session, id: u64) -> Vec<String> {
    let mut k = s.catalog.photo(PhotoId(id)).unwrap().meta.keywords.clone();
    k.sort();
    k
}

fn undo(s: &mut Session) {
    s.execute("edit.undo", &json!({})).unwrap();
}

#[test]
fn several_albums_are_deleted_as_one_step() {
    let mut s = Session::new();
    let (a, b, c) = (album(&mut s, json!({"name": "A"})), album(&mut s, json!({"name": "B"})), album(&mut s, json!({"name": "C"})));
    let steps = s.undo.len();
    let r = s.execute("album.delete", &json!({"ids": [a, c]})).unwrap();
    assert_eq!(r["deleted"], 2);
    assert_eq!((has_album(&s, a), has_album(&s, b), has_album(&s, c)), (false, true, false));
    assert_eq!(s.undo.len(), steps + 1, "one step");
    assert_eq!(s.undo.last().unwrap().label, "Delete 2 Albums");
    undo(&mut s);
    assert!(has_album(&s, a) && has_album(&s, c));
}

#[test]
fn a_folder_and_an_album_inside_it_are_deleted_once() {
    let mut s = Session::new();
    let folder = album(&mut s, json!({"name": "Trips", "folder": true}));
    let inside = album(&mut s, json!({"name": "Alps", "parent": folder}));
    let other = album(&mut s, json!({"name": "Other"}));
    s.execute("album.delete", &json!({"ids": [inside, folder, other]})).unwrap();
    assert!(!has_album(&s, folder) && !has_album(&s, inside) && !has_album(&s, other));
    undo(&mut s);
    assert_eq!(parent_of(&s, inside), Some(folder), "back where it was");
    assert!(has_album(&s, other));
}

#[test]
fn deleting_several_albums_checks_them_all_first() {
    let mut s = Session::new();
    let a = album(&mut s, json!({"name": "A"}));
    let steps = s.undo.len();
    let e = s.execute("album.delete", &json!({"ids": [a, 4242]})).unwrap_err().to_string();
    assert!(e.contains("4242"), "{e}");
    assert!(has_album(&s, a), "nothing was deleted");
    assert_eq!(s.undo.len(), steps);
    for bad in [json!([]), json!("a"), json!([a, "b"])] {
        assert!(s.execute("album.delete", &json!({"ids": bad})).is_err(), "{bad}");
    }
    assert!(has_album(&s, a));
    // one alone, the way it always was
    s.execute("album.delete", &json!({"id": a})).unwrap();
    assert!(!has_album(&s, a));
    assert_eq!(s.undo.last().unwrap().label, "Delete Album");
}

#[test]
fn several_albums_move_into_a_folder_as_one_step() {
    let mut s = Session::new();
    let (a, b) = (album(&mut s, json!({"name": "A"})), album(&mut s, json!({"name": "B"})));
    let folder = album(&mut s, json!({"name": "Trips", "folder": true}));
    let steps = s.undo.len();
    s.execute("album.move", &json!({"ids": [a, b], "parent": folder})).unwrap();
    assert_eq!((parent_of(&s, a), parent_of(&s, b)), (Some(folder), Some(folder)));
    assert_eq!(s.undo.len(), steps + 1);
    assert_eq!(s.undo.last().unwrap().label, "Move 2 Albums");
    undo(&mut s);
    assert_eq!((parent_of(&s, a), parent_of(&s, b)), (None, None));
    // back to the top level, both at once
    s.execute("album.move", &json!({"ids": [a, b], "parent": folder})).unwrap();
    s.execute("album.move", &json!({"ids": [a, b], "parent": null})).unwrap();
    assert_eq!((parent_of(&s, a), parent_of(&s, b)), (None, None));
}

#[test]
fn moving_several_albums_checks_them_all_first() {
    let mut s = Session::new();
    let a = album(&mut s, json!({"name": "A"}));
    let folder = album(&mut s, json!({"name": "Trips", "folder": true}));
    let inner = album(&mut s, json!({"name": "Inner", "folder": true, "parent": folder}));
    // a folder can't go inside itself or inside a folder it holds: then nothing moves
    for parent in [folder, inner] {
        let e = s.execute("album.move", &json!({"ids": [a, folder], "parent": parent})).unwrap_err().to_string();
        assert!(e.contains("Trips"), "the refusal names it: {e}");
        assert_eq!(parent_of(&s, a), None, "nothing moved");
    }
    // into something that is not a folder, or not there
    assert!(s.execute("album.move", &json!({"ids": [folder], "parent": a})).is_err());
    assert!(s.execute("album.move", &json!({"ids": [a], "parent": 4242})).is_err());
    assert!(s.execute("album.move", &json!({"ids": [a, 4242], "parent": folder})).is_err());
    assert_eq!(parent_of(&s, a), None);
    // a folder and an album already inside it: the folder moves, and the album with it
    let outer = album(&mut s, json!({"name": "Outer", "folder": true}));
    let within = album(&mut s, json!({"name": "Within", "parent": inner}));
    s.execute("album.move", &json!({"ids": [within, inner], "parent": outer})).unwrap();
    assert_eq!((parent_of(&s, inner), parent_of(&s, within)), (Some(outer), Some(inner)));
}

#[test]
fn several_keywords_are_deleted_as_one_step() {
    let mut s = Session::new();
    let ids = photos(&mut s, 2);
    tag(&mut s, ids[0], &["alps", "beach", "trips|italy"]);
    tag(&mut s, ids[1], &["beach", "town"]);
    let steps = s.undo.len();
    let r = s.execute("keyword.delete", &json!({"keywords": ["alps", "beach", "trips"]})).unwrap();
    assert_eq!(r["changed"], 2);
    assert_eq!(keywords_of(&s, ids[0]), Vec::<String>::new());
    assert_eq!(keywords_of(&s, ids[1]), vec!["town"]);
    assert_eq!(s.undo.len(), steps + 1);
    assert_eq!(s.undo.last().unwrap().label, "Delete 3 Keywords");
    undo(&mut s);
    assert_eq!(keywords_of(&s, ids[0]), vec!["alps", "beach", "trips|italy"]);
    assert_eq!(keywords_of(&s, ids[1]), vec!["beach", "town"]);
    // nothing named: refused, as is something that is no keyword
    for bad in [json!([]), json!([""]), json!(7), json!(["alps", 7])] {
        assert!(s.execute("keyword.delete", &json!({"keywords": bad})).is_err(), "{bad}");
    }
    assert_eq!(keywords_of(&s, ids[1]), vec!["beach", "town"]);
}

#[test]
fn deleting_several_keywords_checks_them_all_first() {
    let mut s = Session::new();
    let ids = photos(&mut s, 1);
    tag(&mut s, ids[0], &["alps", "trips|italy"]);
    let steps = s.undo.len();
    let e = s.execute("keyword.delete", &json!({"keywords": ["alps", "nowhere"]})).unwrap_err().to_string();
    assert!(e.contains("no keyword") && e.contains("nowhere"), "names the one that is missing: {e}");
    assert_eq!(keywords_of(&s, ids[0]), vec!["alps", "trips|italy"]);
    assert_eq!(s.undo.len(), steps);
    // a keyword that is only the upper level of one, in another case, is there
    s.execute("keyword.delete", &json!({"keywords": ["ALPS", "Trips"]})).unwrap();
    assert_eq!(keywords_of(&s, ids[0]), Vec::<String>::new());
    // one keyword that is not there is still a no-op, as before
    let steps = s.undo.len();
    s.execute("keyword.delete", &json!({"keyword": "nowhere"})).unwrap();
    assert_eq!(s.undo.len(), steps);
}

#[test]
fn several_keywords_move_inside_another_as_one_step() {
    let mut s = Session::new();
    let ids = photos(&mut s, 2);
    tag(&mut s, ids[0], &["alps", "beach"]);
    tag(&mut s, ids[1], &["beach", "places"]);
    let steps = s.undo.len();
    let r = s.execute("keyword.move", &json!({"keywords": ["alps", "beach"], "parent": "places"})).unwrap();
    assert_eq!(r["changed"], 2);
    assert_eq!(keywords_of(&s, ids[0]), vec!["places|alps", "places|beach"]);
    assert_eq!(keywords_of(&s, ids[1]), vec!["places", "places|beach"]);
    assert_eq!(s.undo.len(), steps + 1);
    assert_eq!(s.undo.last().unwrap().label, "Move 2 Keywords");
    undo(&mut s);
    assert_eq!(keywords_of(&s, ids[0]), vec!["alps", "beach"]);
    // …and back out to the top level
    s.execute("keyword.move", &json!({"keywords": ["alps", "beach"], "parent": "places"})).unwrap();
    s.execute("keyword.move", &json!({"keywords": ["places|alps", "places|beach"], "parent": null})).unwrap();
    assert_eq!(keywords_of(&s, ids[0]), vec!["alps", "beach"]);
}

#[test]
fn moving_several_keywords_checks_them_all_first() {
    let mut s = Session::new();
    let ids = photos(&mut s, 2);
    tag(&mut s, ids[0], &["alps", "europe|alps", "beach", "asia|beach", "places"]);
    let before = keywords_of(&s, ids[0]);
    let steps = s.undo.len();
    // a name that is taken where they go: moving would merge the two, which is not done in a batch
    let e = s.execute("keyword.move", &json!({"keywords": ["beach", "alps"], "parent": "europe"})).unwrap_err().to_string();
    assert!(e.contains("europe|alps"), "names the one in the way: {e}");
    // two of them with one name
    let e = s.execute("keyword.move", &json!({"keywords": ["beach", "asia|beach"], "parent": "places"})).unwrap_err().to_string();
    assert!(e.to_lowercase().contains("beach"), "{e}");
    // inside one of themselves; one that is no keyword
    assert!(s.execute("keyword.move", &json!({"keywords": ["places", "alps"], "parent": "places"})).is_err());
    assert!(s.execute("keyword.move", &json!({"keywords": ["alps", "nope"], "parent": "places"})).is_err());
    assert!(s.execute("keyword.move", &json!({"keywords": [], "parent": "places"})).is_err());
    assert_eq!(keywords_of(&s, ids[0]), before, "nothing moved");
    assert_eq!(s.undo.len(), steps);
    // a keyword and one below it: the upper one moves, and the lower with it
    s.execute("keyword.move", &json!({"keywords": ["europe|alps", "europe"], "parent": "places"})).unwrap();
    assert!(keywords_of(&s, ids[0]).contains(&"places|europe|alps".to_string()));
}

fn add_file(s: &mut Session, path: &str) -> u64 {
    let id = s.catalog.alloc_photo_id();
    let p = Photo::new(id, Source::File { path: path.into() }, "x.jpg", "JPEG", 60, 40, "2026-01-01T10:00:00");
    s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    id.0
}

fn deleted(s: &Session, id: u64) -> bool {
    s.catalog.photo(PhotoId(id)).unwrap().deleted
}

#[test]
fn several_folders_leave_the_library_as_one_step() {
    let mut s = Session::new();
    let alps = add_file(&mut s, "/pics/2025/alps/a.jpg");
    let beach = add_file(&mut s, "/pics/2026/beach/c.jpg");
    let town = add_file(&mut s, "/pics/2026/town/d.jpg");
    let steps = s.undo.len();
    // refused as a whole when one is not a folder of the library
    let e = s.execute("library.removeFolder", &json!({"paths": ["/pics/2025/alps", "/nowhere"]})).unwrap_err().to_string();
    assert!(e.contains("/nowhere"), "{e}");
    assert!(!deleted(&s, alps));
    let r = s.execute("library.removeFolder", &json!({"paths": ["/pics/2025/alps", "/pics/2026/beach"]})).unwrap();
    assert_eq!(r["removed"], 2);
    assert_eq!((deleted(&s, alps), deleted(&s, beach), deleted(&s, town)), (true, true, false));
    assert_eq!(s.undo.len(), steps + 1);
    assert_eq!(s.undo.last().unwrap().label, "Remove 2 Folders from Library");
    undo(&mut s);
    assert!(!deleted(&s, alps) && !deleted(&s, beach));
    // a folder and one inside it: each photo once
    add_file(&mut s, "/pics/2025/alps/day1/b.jpg");
    let r = s.execute("library.removeFolder", &json!({"paths": ["/pics/2025/alps/day1", "/pics/2025/alps"]})).unwrap();
    assert_eq!(r["removed"], 2);
}

#[test]
fn several_folders_get_a_colour_label_as_one_step() {
    let mut s = Session::new();
    add_file(&mut s, "/pics/2025/alps/a.jpg");
    add_file(&mut s, "/pics/2026/beach/c.jpg");
    let label = |s: &Session, path: &str| s.catalog.folder_record(path).and_then(|r| r.label);
    let steps = s.undo.len();
    let e = s.execute("folder.label", &json!({"paths": ["/pics/2025/alps", "/nowhere"], "label": "red"})).unwrap_err().to_string();
    assert!(e.contains("/nowhere"), "{e}");
    assert!(label(&s, "/pics/2025/alps").is_none(), "nothing was labelled");
    s.execute("folder.label", &json!({"paths": ["/pics/2025/alps", "/pics/2026/beach"], "label": "red"})).unwrap();
    assert!(label(&s, "/pics/2025/alps").is_some() && label(&s, "/pics/2026/beach").is_some());
    assert_eq!(s.undo.len(), steps + 1);
    undo(&mut s);
    assert!(label(&s, "/pics/2025/alps").is_none() && label(&s, "/pics/2026/beach").is_none());
    // taking the labels off, both at once
    s.execute("folder.label", &json!({"paths": ["/pics/2025/alps", "/pics/2026/beach"], "label": "blue"})).unwrap();
    s.execute("folder.label", &json!({"paths": ["/pics/2025/alps", "/pics/2026/beach"], "label": "none"})).unwrap();
    assert!(label(&s, "/pics/2025/alps").is_none() && label(&s, "/pics/2026/beach").is_none());
}
