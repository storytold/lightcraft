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
    assert!(!s.has_samples());
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
    assert!(!s.has_samples());
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
    assert_eq!(r["albums"], albums_before - 2, "the five albums only samples filled, not the Travel 2026 folder");
    assert_eq!(s.catalog.album(portfolio).and_then(|a| a.cover), Some(own), "the cover was a sample");
    // the sample photos' keywords went with them
    assert!(s.catalog.photos().all(|p| p.meta.keywords.is_empty()));
    // and one Undo brings them all back
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(samples(&s) > 0);
    assert_eq!(s.catalog.albums().count(), albums_before);
}

#[test]
fn removing_the_samples_during_an_open_drag_records_the_drag_as_its_own_undo_step() {
    // Given a drag open on a sample photo's exposure (its value already changed, no history step yet)
    let mut s = Session::with_demo().with_fs();
    let id = s.active().unwrap();
    s.execute("develop.beginInteraction", &json!({"label": "Exposure"})).unwrap();
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 1.2})).unwrap();
    // When the samples are removed from the control channel
    s.execute("library.removeSamples", &json!({})).unwrap();
    assert_eq!(samples(&s), 0);
    // Then one Undo brings the sample back with the dragged value, and the next undoes the drag itself
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.develop_of(id).unwrap().light.exposure, 1.2, "the photo comes back with its edit");
    // (refused with "nothing to undo" when the drag was never recorded)
    let _ = s.execute("edit.undo", &json!({}));
    assert_eq!(s.develop_of(id).unwrap().light.exposure, 0.0, "the drag was not its own undo step");
}

#[test]
fn removing_the_samples_clears_a_filter_on_an_album_that_goes() {
    // Given the person's own photo in the sample album Portfolio (which stays), and the filter and the one put aside
    // on albums while All is shown: one on Mountains (only samples; it goes), the other on Portfolio, in both orders
    for filter_on_the_removed_one in [true, false] {
        let dir = temp_dir("filter-album");
        png(&dir.join("own.png"));
        let mut s = Session::with_demo().with_fs();
        s.execute("library.import", &json!({"paths": [dir.join("own.png").to_string_lossy()]})).unwrap();
        let own = s.catalog.photos().find(|p| matches!(p.source, Source::File { .. })).map(|p| p.id).unwrap();
        let portfolio = album(&s, "Portfolio").unwrap();
        let mut photos = s.catalog.album(portfolio).unwrap().photos.clone();
        photos.push(own);
        s.catalog.apply(Op::SetAlbumPhotos { id: portfolio, photos }).unwrap();
        let mountains = album(&s, "Mountains").unwrap();
        let (now, aside) = if filter_on_the_removed_one { (mountains, portfolio) } else { (portfolio, mountains) };
        s.filter.album = Some(now);
        s.shelved_filter = Some(lightcraft_catalog::Filter { album: Some(aside), ..Default::default() });
        // When the samples are removed
        s.execute("library.removeSamples", &json!({})).unwrap();
        // Then a filter naming the album that went is cleared (the filter bar has no album chip, so it would narrow the
        // view unseen), and one naming an album that stays is left alone
        let keep = |gone: bool, id| if gone { None } else { Some(id) };
        assert_eq!(s.filter.album, keep(filter_on_the_removed_one, portfolio), "the filter's album after removal");
        assert_eq!(
            s.shelved_filter.as_ref().and_then(|f| f.album),
            keep(!filter_on_the_removed_one, portfolio),
            "the shelved filter's album after removal"
        );
        // (a filter put aside that the removal emptied is dropped, not left as an empty one)
        assert_eq!(s.shelved_filter.is_none(), !filter_on_the_removed_one, "an emptied filter is left put aside");
    }
}

#[test]
fn removing_the_samples_clears_the_mask_and_spot_chosen_on_a_photo_that_goes() {
    // Given a mask and a spot chosen on the active sample photo, and a photo of the person's own
    let dir = temp_dir("tool-selection");
    png(&dir.join("own.png"));
    let mut s = Session::with_demo().with_fs();
    s.execute("library.import", &json!({"paths": [dir.join("own.png").to_string_lossy()]})).unwrap();
    let own = s.catalog.photos().find(|p| matches!(p.source, Source::File { .. })).map(|p| p.id).unwrap();
    let sample = s.catalog.photos().find(|p| matches!(p.source, Source::Demo { .. })).map(|p| p.id).unwrap();
    s.selection = crate::Selection::single(sample);
    s.active_mask = Some(0);
    s.active_spot = Some(0);
    // When the samples are removed, the person's photo becomes the active one
    s.execute("library.removeSamples", &json!({})).unwrap();
    assert_eq!(s.active(), Some(own));
    // Then nothing chosen on the old photo is left pointing into the new one
    assert_eq!(s.active_mask, None, "the mask chosen on the removed photo stays chosen");
    assert_eq!(s.active_spot, None, "the spot chosen on the removed photo stays chosen");
}

#[test]
fn removing_the_samples_takes_a_chain_of_folders_however_deep() {
    // Given an album of sample photos at the bottom of 70 folders, each inside the one above
    let mut s = Session::with_demo().with_fs();
    let photos: Vec<_> = s.catalog.photos().filter(|p| matches!(p.source, Source::Demo { .. })).map(|p| p.id).take(2).collect();
    let mut parent = None;
    for level in 0..70 {
        let id = s.catalog.alloc_album_id();
        let mut folder = lightcraft_catalog::Album::new(id, format!("deep-{level:02}"));
        folder.folder = true;
        folder.parent = parent;
        s.catalog.apply(Op::AddAlbum { album: folder }).unwrap();
        parent = Some(id);
    }
    let id = s.catalog.alloc_album_id();
    let mut bottom = lightcraft_catalog::Album::new(id, "bottom");
    bottom.parent = parent;
    bottom.photos = photos;
    s.catalog.apply(Op::AddAlbum { album: bottom }).unwrap();
    // When the samples are removed
    s.execute("library.removeSamples", &json!({})).unwrap();
    // Then no folder of the chain is left behind, empty
    let left: Vec<_> = s.catalog.albums().filter(|a| a.name.starts_with("deep-") || a.name == "bottom").map(|a| a.name.clone()).collect();
    assert!(left.is_empty(), "left behind: {left:?}");
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
    // the count the Help menu asks for each frame, without scanning the library
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
}

#[test]
fn samples_in_recently_deleted_are_still_counted() {
    let mut s = Session::with_demo();
    let n = samples(&s);
    let ids: Vec<_> = s.catalog.photos().map(|p| p.id).collect();
    s.selection.ids = ids.clone();
    s.execute("photo.delete", &json!({"ids": ids.iter().map(|i| i.0).collect::<Vec<_>>()})).unwrap();
    assert_eq!(s.catalog.sample_photos(), n, "still in the library's catalog, to remove or restore");
    let back = lightcraft_catalog::Catalog::from_snapshot(&s.catalog.to_snapshot()).unwrap();
    assert_eq!(back.sample_photos(), n);
    // restored, they are as before
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.catalog.sample_photos(), n);
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
    // and removing the samples takes the folder too, counting the albums as adding them did
    let removed = s.execute("library.removeSamples", &json!({})).unwrap();
    assert_eq!(removed["albums"], r["albums"]);
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
    for bad in [json!(""), json!("   "), json!(3), json!("x".repeat(201)), json!("two\nlines"), json!("a\tb")] {
        let mut s = Session::new();
        let err = s.execute("library.addSamples", &json!({"folder": bad})).unwrap_err().to_string();
        assert!(err.contains("folder") && err.len() < 200, "says what is wrong, without echoing it: {err}");
        assert_eq!(s.catalog.len(), 0, "nothing added");
    }
}

#[test]
fn a_name_with_a_trailing_newline_is_trimmed_not_refused() {
    let mut s = Session::new();
    s.execute("library.addSamples", &json!({"folder": "Try these\n"})).unwrap();
    assert!(album(&s, "Try these").is_some());
}

/// Two photos of the person's own, imported and both selected.
fn two_own_selected(tag: &str, mut s: Session) -> (Session, Vec<lightcraft_catalog::PhotoId>) {
    let dir = temp_dir(tag);
    png(&dir.join("a.png"));
    let img = lightcraft_raster::Rgba8::from_fn(16, 12, |x, y| [(x * 5) as u8, (y * 3) as u8, 90, 255]);
    let b =
        crate::export::encode_image(&img, &crate::export::ExportOptions { format: crate::export::ExportFormat::Png, ..Default::default() }).unwrap();
    std::fs::write(dir.join("b.png"), b).unwrap();
    s.execute("library.import", &json!({"paths": [dir.join("a.png").to_string_lossy(), dir.join("b.png").to_string_lossy()]})).unwrap();
    let own: Vec<_> = s.catalog.photos().filter(|p| matches!(p.source, Source::File { .. })).map(|p| p.id).collect();
    assert_eq!(own.len(), 2);
    s.selection.ids = own.clone();
    s.selection.active = own.first().copied();
    (s, own)
}

#[test]
fn removing_the_samples_keeps_the_persons_selection() {
    let (mut s, own) = two_own_selected("keep-selection", Session::with_demo().with_fs());
    s.execute("library.removeSamples", &json!({})).unwrap();
    assert_eq!(s.selection.ids, own, "the photos that were selected still are");
}

#[test]
fn adding_the_samples_keeps_a_selection_the_person_made() {
    let (mut s, own) = two_own_selected("keep-selection-add", Session::new().with_fs());
    s.execute("library.addSamples", &json!({})).unwrap();
    assert_eq!(s.selection.ids, own);
}

/// An active photo with nothing selected (`library.select {ids: [], active}`) is the person's
/// choice too: the samples that come in don't replace it.
#[test]
fn adding_the_samples_keeps_an_active_photo_chosen_without_a_selection() {
    let (mut s, own) = two_own_selected("keep-active-add", Session::new().with_fs());
    s.execute("library.select", &json!({"ids": [], "active": own[1].0})).unwrap();
    assert!(s.selection.ids.is_empty() && s.selection.active == Some(own[1]));
    s.execute("library.addSamples", &json!({})).unwrap();
    assert_eq!(s.selection.active, Some(own[1]));
    assert!(s.selection.ids.is_empty());
}

/// With nothing chosen, the photo that comes up is one of the samples just added, not whichever
/// photo of the library happens to be shown first.
#[test]
fn adding_the_samples_selects_a_sample_not_the_first_photo_of_the_library() {
    let (mut s, _own) = two_own_selected("select-sample-add", Session::new().with_fs());
    s.selection = crate::Selection::default();
    // the person's own photos come first in the view
    s.execute("library.sort", &json!({"key": "fileName", "ascending": true})).unwrap();
    s.execute("library.addSamples", &json!({})).unwrap();
    let first = s.visible_cloned()[0];
    assert!(matches!(s.catalog.photo(first).unwrap().source, Source::File { .. }), "the view starts with a photo of the person's own");
    let active = s.selection.active.expect("a photo is shown");
    assert!(matches!(s.catalog.photo(active).unwrap().source, Source::Demo { .. }), "the selected photo is a sample");
    assert_eq!(s.selection.ids, vec![active]);
}
