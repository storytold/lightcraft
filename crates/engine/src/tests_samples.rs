//! Sample photos: a new library starts empty and can take the procedural demo photos on request
//! (`library.addSamples`), and they leave together with the albums they filled
//! (`library.removeSamples`). Both are one undo step.

use std::path::{Path, PathBuf};

use lightcraft_catalog::{Op, Source};
use serde_json::json;

use crate::Session;

fn samples(s: &Session) -> usize {
    s.catalog.photos().filter(|p| matches!(p.source, Source::Demo { .. })).count()
}

fn album(s: &Session, name: &str) -> Option<lightcraft_catalog::AlbumId> {
    s.catalog.albums().find(|a| a.name == name).map(|a| a.id)
}

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-samples-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn png(p: &Path) {
    let img = lightcraft_raster::Rgba8::from_fn(16, 12, |x, y| [(x * 9) as u8, (y * 11) as u8, 40, 255]);
    let b =
        crate::export::encode_image(&img, &crate::export::ExportOptions { format: crate::export::ExportFormat::Png, ..Default::default() }).unwrap();
    std::fs::write(p, b).unwrap();
}

#[test]
fn sample_photos_come_into_an_empty_library_as_one_undo_step() {
    // Given a new, empty library
    let mut s = Session::new();
    assert_eq!(s.catalog.len(), 0);
    assert!(s.catalog.never_held_photos() && !s.has_samples());
    // When the person asks for the sample photos
    let r = s.execute("library.addSamples", &json!({})).unwrap();
    // Then the demo photos and their albums are in, and one is selected to look at
    let n = samples(&s);
    assert!(n > 0);
    assert_eq!(r["added"], n);
    assert!(album(&s, "Travel 2026").is_some() && album(&s, "Portfolio").is_some());
    assert!(s.active().is_some());
    assert!(s.has_samples());
    // and one Undo takes them all back out (the library has held photos now, though)
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.catalog.len(), 0);
    assert_eq!(s.catalog.albums().count(), 0);
    assert!(!s.has_samples() && !s.catalog.never_held_photos());
}

#[test]
fn sample_photos_are_added_once() {
    let mut s = Session::with_demo();
    let err = s.execute("library.addSamples", &json!({})).unwrap_err().to_string();
    assert!(err.contains("already"), "{err}");
}

#[test]
fn removing_the_samples_takes_their_albums_and_keeps_the_persons_own() {
    // Given the sample photos, and a photo of the person's own put in the sample album Portfolio
    let dir = temp_dir("remove");
    png(&dir.join("own.png"));
    let mut s = Session::with_demo().with_fs();
    s.execute("library.import", &json!({"paths": [dir.join("own.png").to_string_lossy()]})).unwrap();
    let own = s.catalog.photos().find(|p| matches!(p.source, Source::File { .. })).map(|p| p.id).unwrap();
    let portfolio = album(&s, "Portfolio").unwrap();
    let mut photos = s.catalog.album(portfolio).unwrap().photos.clone();
    photos.push(own);
    s.catalog.apply(Op::SetAlbumPhotos { id: portfolio, photos }).unwrap();
    let albums_before = s.catalog.albums().count();
    // When the samples are removed
    let r = s.execute("library.removeSamples", &json!({})).unwrap();
    // Then every sample photo is gone, with the albums only they filled (and the folder that held
    // them), and the person's photo and the album it is in stay
    assert_eq!(samples(&s), 0);
    assert!(!s.has_samples());
    assert_eq!(s.catalog.len(), 1);
    assert!(s.catalog.photo(own).is_some());
    assert_eq!(s.catalog.album(portfolio).map(|a| a.photos.clone()), Some(vec![own]));
    for gone in ["Travel 2026", "Mountains", "Coastlines", "Deserts", "Night Sky", "Garden"] {
        assert!(album(&s, gone).is_none(), "{gone} stays");
    }
    assert_eq!(r["albums"], albums_before - 1);
    assert_eq!(s.catalog.album(portfolio).and_then(|a| a.cover), Some(own), "the cover was a sample");
    // the sample photos' keywords went with them
    assert!(s.catalog.photos().all(|p| p.meta.keywords.is_empty()));
    // and one Undo brings them all back
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(samples(&s) > 0);
    assert_eq!(s.catalog.albums().count(), albums_before);
}

#[test]
fn removing_the_samples_while_one_of_their_albums_is_shown_goes_back_to_all_photos() {
    let dir = temp_dir("source");
    png(&dir.join("own.png"));
    let mut s = Session::with_demo().with_fs();
    s.execute("library.import", &json!({"paths": [dir.join("own.png").to_string_lossy()]})).unwrap();
    let mountains = album(&s, "Mountains").unwrap();
    s.source = crate::LibrarySource::Album(mountains);
    s.execute("library.removeSamples", &json!({})).unwrap();
    assert_eq!(s.source, crate::LibrarySource::All);
    assert!(s.active().is_some(), "the photo left is selected");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_catalog_counts_its_sample_photos_as_they_come_and_go() {
    // the count the banner and the Help menu ask for each frame, without scanning the library
    let mut s = Session::with_demo();
    let n = samples(&s);
    assert_eq!(s.catalog.sample_photos(), n);
    // an edit doesn't change it; removing and undoing does, both ways
    let id = s.catalog.photos().next().map(|p| p.id).unwrap();
    s.catalog.apply(Op::SetRating { id, rating: 2 }).unwrap();
    assert_eq!(s.catalog.sample_photos(), n);
    s.execute("library.removeSamples", &json!({})).unwrap();
    assert_eq!(s.catalog.sample_photos(), 0);
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.catalog.sample_photos(), n);
    // and a catalog read back from its snapshot counts them again
    let back = lightcraft_catalog::Catalog::from_snapshot(&s.catalog.to_snapshot()).unwrap();
    assert_eq!(back.sample_photos(), n);
    assert_eq!(back.sample_photos_in_library(), n);
}

#[test]
fn samples_in_recently_deleted_are_counted_but_not_as_in_the_library() {
    let mut s = Session::with_demo();
    let n = samples(&s);
    let ids: Vec<_> = s.catalog.photos().map(|p| p.id).collect();
    s.selection.ids = ids.clone();
    s.execute("photo.delete", &json!({"ids": ids.iter().map(|i| i.0).collect::<Vec<_>>()})).unwrap();
    assert_eq!(s.catalog.sample_photos(), n, "still in the library's catalog, to remove or restore");
    assert_eq!(s.catalog.sample_photos_in_library(), 0);
    let back = lightcraft_catalog::Catalog::from_snapshot(&s.catalog.to_snapshot()).unwrap();
    assert_eq!(back.sample_photos_in_library(), 0);
    // restored, they are back in the library
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.catalog.sample_photos_in_library(), n);
}

#[test]
fn removing_samples_from_a_library_without_them_is_refused() {
    let mut s = Session::new();
    let err = s.execute("library.removeSamples", &json!({})).unwrap_err().to_string();
    assert!(err.contains("no sample photos"), "{err}");
}

#[test]
fn a_new_library_on_disk_starts_empty_unless_samples_are_asked_for() {
    let dir = temp_dir("new");
    let mut s = Session::new().with_fs();
    s.open_library(dir.join("plain"), false).unwrap();
    assert_eq!(s.catalog.len(), 0);
    // added samples are saved with the library
    s.execute("library.addSamples", &json!({})).unwrap();
    let n = samples(&s);
    s.close_library().unwrap();
    drop(s);
    let mut again = Session::new().with_fs();
    again.open_library(dir.join("plain"), false).unwrap();
    assert_eq!(samples(&again), n);
}

#[test]
fn the_sample_albums_come_in_one_folder() {
    // Given an empty library, when the samples come in
    let mut s = Session::new();
    let r = s.execute("library.addSamples", &json!({})).unwrap();
    // Then every album they bring is inside one folder, named in the reply
    assert_eq!(r["folder"], "Sample Photos");
    let folder = album(&s, "Sample Photos").unwrap();
    let top: Vec<String> = s.catalog.albums().filter(|a| a.parent.is_none()).map(|a| a.name.clone()).collect();
    assert_eq!(top, ["Sample Photos"]);
    let inside = s.catalog.albums().filter(|a| a.parent == Some(folder) && !a.folder).count();
    assert!(r["albums"].as_u64().unwrap() > inside as u64, "and more in Travel 2026, a folder inside it");
    // and removing the samples takes the folder too
    s.execute("library.removeSamples", &json!({})).unwrap();
    assert_eq!(s.catalog.albums().count(), 0);
}

#[test]
fn the_sample_folder_can_be_named() {
    let mut s = Session::new();
    s.execute("library.addSamples", &json!({"folder": "  Beispielfotos "})).unwrap();
    assert!(album(&s, "Beispielfotos").is_some());
    let mut s = Session::new();
    s.execute("library.addSamples", &json!({"folder": format!(" {} ", "x".repeat(200))})).unwrap();
    assert!(album(&s, &"x".repeat(200)).is_some(), "200 characters once trimmed");
    for bad in [json!(""), json!("   "), json!(3), json!("x".repeat(201)), json!("two\nlines")] {
        let mut s = Session::new();
        let err = s.execute("library.addSamples", &json!({"folder": bad})).unwrap_err().to_string();
        assert!(err.contains("folder") && err.len() < 200, "says what is wrong, without echoing it: {err}");
        assert_eq!(s.catalog.len(), 0, "nothing added");
    }
}
