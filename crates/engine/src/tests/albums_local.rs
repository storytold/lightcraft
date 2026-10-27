//! Albums hold library photos (issue #684): browsed (Local) photos are skipped by
//! `album.create {addSelected}`, `album.addPhotos` and `album.toggleTarget` (B), which report how
//! many; a Local-only selection is refused and leaves Photo ▸ Add to Album off.

use std::path::{Path, PathBuf};

use lightcraft_catalog::{AlbumId, PhotoId};
use serde_json::json;

use crate::{Selection, Session};

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-albums-local-{tag}-{}", std::process::id()));
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

/// A session browsing a folder of two files, one of them added to My Photos: (session, local, library).
fn browsed(tag: &str) -> (Session, PhotoId, PhotoId) {
    let dir = temp_dir(tag);
    png(&dir.join("a.png"), 1);
    png(&dir.join("b.png"), 2);
    let mut s = Session::new().with_fs();
    s.execute("library.browse", &json!({"path": dir.to_string_lossy()})).unwrap();
    let mut ids: Vec<PhotoId> = s.catalog.photos().filter(|p| p.local).map(|p| p.id).collect();
    ids.sort();
    assert_eq!(ids.len(), 2, "both files are browsed");
    let (local, library) = (ids[0], ids[1]);
    s.execute("photo.addToLibrary", &json!({"ids": [library.0]})).unwrap();
    assert!(!s.catalog.photo(library).unwrap().local && s.catalog.photo(local).unwrap().local);
    (s, local, library)
}

fn enabled(s: &Session, id: &str) -> bool {
    s.commands().into_iter().find(|c| c.id == id).unwrap_or_else(|| panic!("no command {id}")).enabled
}

#[test]
fn a_local_only_selection_is_refused_and_leaves_add_to_album_off() {
    let (mut s, local, library) = browsed("refused");
    let album = AlbumId(s.execute("album.create", &json!({"name": "Trip", "addSelected": false})).unwrap()["id"].as_u64().unwrap());
    s.selection = Selection::single(local);
    assert!(!enabled(&s, "album.addPhotos"), "Photo ▸ Add to Album is off for a Local photo");
    for (command, params) in [
        ("album.addPhotos", json!({"id": album.0})),
        ("album.addPhotos", json!({"id": album.0, "ids": [local.0]})),
        ("album.toggleTarget", json!({})),
    ] {
        let e = s.execute(command, &params).unwrap_err().to_string();
        assert!(e.contains("Local") && e.contains("Add to My Photos"), "{command}: {e}");
    }
    assert!(s.catalog.album(album).unwrap().photos.is_empty(), "nothing invisible was added");
    assert!(s.catalog.quick_collection().is_none(), "no Quick Collection was made for nothing");
    // the album is still created from a Local-only selection, just without it
    let r = s.execute("album.create", &json!({"name": "Empty", "addSelected": true})).unwrap();
    assert_eq!(r["skipped"], 1, "{r}");
    assert!(s.catalog.album(AlbumId(r["id"].as_u64().unwrap())).unwrap().photos.is_empty());
    // once in My Photos the same photo goes in
    s.execute("photo.addToLibrary", &json!({"ids": [local.0]})).unwrap();
    assert!(enabled(&s, "album.addPhotos"));
    let r = s.execute("album.addPhotos", &json!({"id": album.0})).unwrap();
    assert_eq!((r["added"].as_u64(), r["skipped"].as_u64()), (Some(1), Some(0)), "{r}");
    assert_eq!(s.catalog.album(album).unwrap().photos, vec![local]);
    let _ = library;
}

#[test]
fn a_mixed_selection_skips_the_local_photos_and_says_how_many() {
    let (mut s, local, library) = browsed("mixed");
    s.selection = Selection { ids: vec![local, library], active: Some(library) };
    assert!(enabled(&s, "album.addPhotos"), "one library photo is enough");
    let r = s.execute("album.create", &json!({"name": "Trip", "addSelected": true})).unwrap();
    let album = AlbumId(r["id"].as_u64().unwrap());
    assert_eq!(r["skipped"], 1, "{r}");
    assert_eq!(s.catalog.album(album).unwrap().photos, vec![library]);
    assert_eq!(s.catalog.album(album).unwrap().cover, Some(library), "the cover is a photo the album shows");

    let other = AlbumId(s.execute("album.create", &json!({"name": "Other", "addSelected": false})).unwrap()["id"].as_u64().unwrap());
    let r = s.execute("album.addPhotos", &json!({"id": other.0})).unwrap();
    assert_eq!((r["added"].as_u64(), r["skipped"].as_u64()), (Some(1), Some(1)), "{r}");
    assert_eq!(s.catalog.album(other).unwrap().photos, vec![library]);

    let r = s.execute("album.toggleTarget", &json!({})).unwrap();
    assert_eq!((r["added"].as_bool(), r["skipped"].as_u64()), (Some(true), Some(1)), "{r}");
    let quick = s.catalog.quick_collection().expect("B made the Quick Collection");
    assert_eq!(s.catalog.album(quick).unwrap().photos, vec![library]);
    // B again: the library photo is the whole addable selection and it is in, so it comes out
    let r = s.execute("album.toggleTarget", &json!({})).unwrap();
    assert_eq!(r["added"], false, "{r}");
    assert!(s.catalog.album(quick).unwrap().photos.is_empty());
}
