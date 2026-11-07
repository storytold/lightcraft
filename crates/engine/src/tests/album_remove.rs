//! Removing photos from an album changes the album only (issue #1805): the photos stay in the
//! library, in their library folder and in the folder view of their folder on disk. Deleting a
//! photo (`photo.delete`) still removes it from all of them.

use std::path::{Path, PathBuf};

use lightcraft_catalog::PhotoId;
use serde_json::json;

use crate::{Selection, Session};

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-album-remove-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn png(p: &Path, seed: u8) {
    let img = lightcraft_raster::Rgba8::from_fn(16, 12, |x, y| [(x * 9) as u8, (y * 11) as u8, seed, 255]);
    let b =
        crate::export::encode_image(&img, &crate::export::ExportOptions { format: crate::export::ExportFormat::Png, ..Default::default() }).unwrap();
    std::fs::write(p, b).unwrap();
}

/// A folder of three files imported into a new album "Trip": (session, folder, album id, photos).
fn imported(tag: &str) -> (Session, String, u64, Vec<PhotoId>) {
    let dir = temp_dir(tag);
    for (i, n) in ["a.png", "b.png", "c.png"].iter().enumerate() {
        png(&dir.join(n), i as u8 * 40);
    }
    let folder = dir.to_string_lossy().to_string();
    let mut s = Session::new().with_fs();
    let r = s.execute("library.import", &json!({"paths": [folder], "mode": "add", "albumName": "Trip"})).unwrap();
    let album = r["album"].as_u64().expect("the import made the album");
    let mut ids: Vec<PhotoId> = r["imported"].as_array().unwrap().iter().filter_map(|v| v.as_u64()).map(PhotoId).collect();
    ids.sort();
    assert_eq!(ids.len(), 3);
    (s, folder, album, ids)
}

/// How many photos the library folder view, the folder (disk) view and the album show.
fn counts(s: &mut Session, folder: &str, album: u64) -> (u64, u64, u64) {
    let lib = s.execute("library.source", &json!({"kind": "libraryFolder", "path": folder})).unwrap()["count"].as_u64().unwrap();
    let disk = s.execute("library.browse", &json!({"path": folder})).unwrap()["photos"].as_u64().unwrap();
    let disk_visible = s.visible_cloned().len() as u64;
    assert_eq!(disk, disk_visible, "browse reports what it shows");
    let al = s.execute("library.source", &json!({"kind": "album", "id": album})).unwrap()["count"].as_u64().unwrap();
    (lib, disk, al)
}

/// The reporter's steps: a folder added to an album, a photo removed in the album, then the folder
/// view: it still shows every photo of the folder.
#[test]
fn removing_from_an_album_keeps_the_photo_in_its_folder() {
    let (mut s, folder, album, ids) = imported("keep");
    assert_eq!(counts(&mut s, &folder, album), (3, 3, 3));

    // in the album (its source on screen), remove one photo; no `id`: the album on screen
    s.execute("library.source", &json!({"kind": "album", "id": album})).unwrap();
    s.selection = Selection::single(ids[1]);
    let r = s.execute("album.removePhotos", &json!({})).unwrap();
    assert_eq!(r["removed"], 1, "{r}");
    assert_eq!(r["album"], "Trip", "{r}");
    assert!(!s.catalog.photo(ids[1]).unwrap().deleted, "still in the library");
    assert_eq!(counts(&mut s, &folder, album), (3, 3, 2), "(library folder, folder on disk, album)");

    // removing what isn't in the album changes nothing (and makes no undo step)
    let undo = s.undo.len();
    s.execute("library.source", &json!({"kind": "album", "id": album})).unwrap();
    let r = s.execute("album.removePhotos", &json!({"ids": [ids[1].0]})).unwrap();
    assert_eq!(r["removed"], 0, "{r}");
    assert_eq!(s.undo.len(), undo);
    let _ = std::fs::remove_dir_all(&folder);
}

/// Delete (to Recently Deleted) is the way out of the library: the photo leaves its folder views
/// and the album with it.
#[test]
fn deleting_a_photo_still_removes_it_everywhere() {
    let (mut s, folder, album, ids) = imported("delete");
    s.execute("library.source", &json!({"kind": "album", "id": album})).unwrap();
    s.selection = Selection::single(ids[0]);
    s.execute("photo.delete", &json!({})).unwrap();
    assert!(s.catalog.photo(ids[0]).unwrap().deleted);
    assert_eq!(counts(&mut s, &folder, album), (2, 2, 2), "(library folder, folder on disk, album)");
    let n = s.execute("library.source", &json!({"kind": "recentlyDeleted"})).unwrap()["count"].as_u64().unwrap();
    assert_eq!(n, 1);
    let _ = std::fs::remove_dir_all(&folder);
}

/// Without an album on screen and without `id` there is nothing to remove from; smart albums
/// follow their rules.
#[test]
fn remove_from_album_refuses_what_it_cannot_do() {
    let (mut s, folder, _album, ids) = imported("refuse");
    s.execute("library.source", &json!({"kind": "all"})).unwrap();
    s.selection = Selection::single(ids[0]);
    assert!(s.execute("album.removePhotos", &json!({})).is_err(), "no album on screen");
    let smart = s.execute("album.createSmart", &json!({"name": "All", "rules": {}})).unwrap()["id"].as_u64().unwrap();
    assert!(s.execute("album.removePhotos", &json!({"id": smart})).is_err(), "smart album");
    assert!(s.execute("album.removePhotos", &json!({"id": 9999})).is_err(), "no such album");
    assert!(s.execute("album.removePhotos", &json!({"id": "x"})).is_err(), "bad id");
    let _ = std::fs::remove_dir_all(&folder);
}
