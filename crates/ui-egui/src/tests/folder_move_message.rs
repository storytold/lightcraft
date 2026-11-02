//! Move Folder To… says where the folder went in plain words, in the person's language, and
//! that Edit → Undo puts it back. The Folders tree and Local share the one message.

use std::path::PathBuf;

use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

/// A fresh temporary folder holding `trip` (with one photo file) and an empty `archive`.
fn folders(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("lc-folder-move-msg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (trip, archive) = (root.join("trip"), root.join("archive"));
    for dir in [&trip, &archive] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let img = lightcraft_raster::Rgba8 { width: 8, height: 8, data: vec![[90, 120, 160, 255]; 64] };
    let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    std::fs::write(trip.join("beach.png"), png).unwrap();
    (root, trip, archive)
}

/// An app whose library holds the photo `beach.png` of `trip` (when `imported`).
fn app(trip: &std::path::Path, imported: bool) -> Headless {
    use lightcraft_catalog::{Op, Photo, Source};
    let mut session = lightcraft_engine::Session::new().with_fs();
    if imported {
        let id = session.catalog.alloc_photo_id();
        let path = trip.join("beach.png").to_string_lossy().to_string();
        let p = Photo::new(id, Source::File { path }, "beach.png", "PNG", 8, 8, "2026-01-01T10:00:00");
        session.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    Headless::new(LightcraftApp::new(session, Services { png: None, ..Default::default() }), [1300.0, 900.0], 1.0)
}

fn toast(h: &Headless) -> String {
    h.app.ui.toast.as_ref().map(|(t, _, _)| t.clone()).unwrap_or_default()
}

fn move_into(h: &mut Headless, trip: &std::path::Path, archive: &std::path::Path) -> String {
    let ctx = h.view.ctx.clone();
    crate::panels::left::move_folder_to(&mut h.app, &ctx, &trip.to_string_lossy(), &archive.to_string_lossy());
    toast(h)
}

#[test]
fn the_message_names_the_folder_where_it_went_the_photos_and_undo() {
    let (root, trip, archive) = folders("english");
    let mut h = app(&trip, true);
    let text = move_into(&mut h, &trip, &archive);
    assert_eq!(text, "Moved “trip” into “archive”. Photos: 1. Edit → Undo moves it back.");
    assert!(archive.join("trip").join("beach.png").is_file(), "moved on disk");
    // and Undo does what the message says
    h.app.run("edit.undo", json!({})).unwrap();
    assert!(trip.join("beach.png").is_file(), "back where it was");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_folder_without_library_photos_leaves_the_count_out() {
    let (root, trip, archive) = folders("nophotos");
    let mut h = app(&trip, false);
    let text = move_into(&mut h, &trip, &archive);
    assert_eq!(text, "Moved “trip” into “archive”. Edit → Undo moves it back.");
    let _ = std::fs::remove_dir_all(&root);
}

/// The folder's catalog photos that are not the library's (browsed in Local, in Recently Deleted) went along on disk
/// too, and a virtual copy shares its original's file: none of them is a library photo moved.
#[test]
fn the_count_is_of_library_photos_not_of_local_deleted_or_virtual_copies() {
    use lightcraft_catalog::{Op, Photo, Source};
    let (root, trip, archive) = folders("kinds");
    let mut h = app(&trip, true);
    let path = trip.join("beach.png").to_string_lossy().to_string();
    let original = h.app.session.catalog.photos().next().unwrap().id;
    let mut photos = Vec::new();
    for kind in ["local", "deleted", "copy"] {
        let id = h.app.session.catalog.alloc_photo_id();
        let mut p = Photo::new(id, Source::File { path: path.clone() }, "beach.png", "PNG", 8, 8, "2026-01-01T10:00:00");
        match kind {
            "local" => p.local = true,
            "deleted" => p.deleted = true,
            _ => p.copy_of = Some(original),
        }
        photos.push(Op::AddPhoto { photo: Box::new(p) });
    }
    h.app.session.catalog.apply(Op::Batch { ops: photos }).unwrap();
    let text = move_into(&mut h, &trip, &archive);
    assert_eq!(text, "Moved “trip” into “archive”. Photos: 1. Edit → Undo moves it back.");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_message_follows_the_language() {
    let (root, trip, archive) = folders("german");
    let mut h = app(&trip, true);
    crate::i18n::set_language(crate::i18n::Locale::De);
    let text = move_into(&mut h, &trip, &archive);
    crate::i18n::set_language(crate::i18n::Locale::En);
    assert!(!text.starts_with("Moved"), "{text}");
    assert!(text.contains("trip") && text.contains("archive"), "{text}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_refused_move_says_why_and_not_that_it_moved() {
    let (root, trip, archive) = folders("refused");
    std::fs::create_dir_all(archive.join("trip")).unwrap();
    let mut h = app(&trip, false);
    let text = move_into(&mut h, &trip, &archive);
    assert!(text.contains("already exists") && !text.contains("Moved"), "{text}");
    let _ = std::fs::remove_dir_all(&root);
}
