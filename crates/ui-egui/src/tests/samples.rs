//! A new library asks how to begin: its first screen offers Import Photos…, Import Lightroom
//! Catalog… and the sample photos. While sample photos are in the library a banner says so and
//! offers to import or to remove them; once photos of the person's own are in, it asks whether to
//! remove the samples or keep them.

use std::path::Path;
use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::panels::empty_library;
use crate::state::ViewMode;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

fn headless(session: lightcraft_engine::Session) -> Headless {
    let mut a = LightcraftApp::new(session, Services::default());
    a.ui.view = ViewMode::PhotoGrid;
    let mut h = Headless::new(a, [1300.0, 900.0], 1.0);
    h.step();
    h.step();
    h
}

fn click(h: &mut Headless, id: &str) {
    assert_eq!(h.request("ui.clickWidget", json!({"id": id}), T)["ok"], true, "{id}");
    h.step();
    h.step();
}

/// A new library on disk (the banner is for a library, not an in-memory demo session), with the
/// sample photos if asked.
fn library(tag: &str, samples: bool) -> lightcraft_engine::Session {
    let dir = std::env::temp_dir().join(format!("lc-ui-samples-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut s = lightcraft_engine::Session::new().with_fs();
    s.open_library(dir.join("lib"), false).unwrap();
    if samples {
        s.execute("library.addSamples", &json!({})).unwrap();
    }
    s
}

/// A library with the sample photos and one photo of the person's own.
fn samples_and_own(tag: &str) -> lightcraft_engine::Session {
    let mut s = library(tag, true);
    let dir = std::env::temp_dir().join(format!("lc-ui-samples-{tag}-{}", std::process::id()));
    let img = lightcraft_raster::Rgba8 { width: 8, height: 8, data: vec![[90, 120, 160, 255]; 64] };
    let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    let own = dir.join("own.png");
    std::fs::write(&own, png).unwrap();
    s.execute("library.import", &json!({"paths": [Path::new(&own).to_string_lossy()]})).unwrap();
    s
}

#[test]
fn a_new_library_welcomes_and_offers_the_sample_photos() {
    // Given a library just created
    let s = library("welcome", false);
    assert_eq!(empty_library::title(&s, false), "Welcome to LightCraft");
    assert_eq!(empty_library::title(&s, true), "Your library is empty", "a temporary session isn't welcomed");
    let mut h = headless(s);
    for b in ["button:emptyImportPhotos", "button:emptySamples"] {
        assert!(has(&h, b), "{b}");
    }
    // When the person chooses the sample photos
    click(&mut h, "button:emptySamples");
    // Then they are in the library, one is selected, and the banner says what they are
    assert!(h.app.session.has_samples());
    assert!(h.app.session.active().is_some());
    assert!(has(&h, "banner:samples"));
    assert!(!has(&h, "button:emptySamples"));
}

#[test]
fn an_emptied_library_is_not_welcomed_again() {
    let mut s = lightcraft_engine::Session::with_demo();
    s.execute("library.removeSamples", &json!({})).unwrap();
    assert_eq!(empty_library::title(&s, false), "Your library is empty");
}

#[test]
fn the_banner_over_the_samples_offers_import_and_removal() {
    let mut h = headless(library("banner", true));
    for b in ["banner:samples", "button:samplesImport", "button:samplesRemove", "button:samplesHide"] {
        assert!(has(&h, b), "{b}");
    }
    // Remove Samples: the photos and their albums go, and the empty library leads to Import
    click(&mut h, "button:samplesRemove");
    assert!(!h.app.session.has_samples());
    assert!(!has(&h, "banner:samples"));
    assert!(has(&h, "button:emptyImportPhotos"));
}

#[test]
fn once_the_persons_photos_are_in_the_banner_asks_to_remove_or_keep_the_samples() {
    let mut h = headless(samples_and_own("keep"));
    assert!(has(&h, "button:samplesRemove"));
    assert!(has(&h, "button:samplesKeep"));
    assert!(!has(&h, "button:samplesImport"), "import is done");
    // Keep Them: the banner goes, for good
    click(&mut h, "button:samplesKeep");
    assert!(h.app.ui.samples_banner_hidden);
    assert!(!has(&h, "banner:samples"));
    assert!(h.app.session.has_samples());
}

#[test]
fn removing_the_samples_keeps_the_persons_photo_and_says_so() {
    let mut h = headless(samples_and_own("remove"));
    click(&mut h, "button:samplesRemove");
    assert!(!h.app.session.has_samples());
    assert_eq!(h.app.session.catalog.len(), 1);
    assert!(!has(&h, "banner:samples"));
}

#[test]
fn an_in_memory_demo_session_has_no_banner() {
    // `--memory`, the showcase scripts and the CLI's `--demo` are demos by design
    let h = headless(lightcraft_engine::Session::with_demo());
    assert!(h.app.session.has_samples());
    assert!(!has(&h, "banner:samples"));
}

#[test]
fn adding_the_samples_again_brings_the_banner_back() {
    let mut a = LightcraftApp::new(library("again", false), Services::default());
    a.ui.view = ViewMode::PhotoGrid;
    a.ui.samples_banner_hidden = true;
    let mut h = Headless::new(a, [1300.0, 900.0], 1.0);
    h.step();
    h.step();
    click(&mut h, "button:emptySamples");
    assert!(!h.app.ui.samples_banner_hidden);
    assert!(has(&h, "banner:samples"));
}

#[test]
fn samples_in_recently_deleted_are_not_offered_again() {
    // adding them would be refused: they are still in the library
    let mut s = lightcraft_engine::Session::with_demo();
    let ids: Vec<u64> = s.catalog.photos().map(|p| p.id.0).collect();
    s.selection.ids = s.catalog.photos().map(|p| p.id).collect();
    s.execute("photo.delete", &json!({"ids": ids})).unwrap();
    let h = headless(s);
    assert!(has(&h, "button:emptyImportPhotos"));
    assert!(!has(&h, "button:emptySamples"));
}

#[test]
fn samples_in_recently_deleted_dont_keep_the_banner_saying_they_are_here() {
    // the grid says the library is empty: a banner saying "these are sample photos" with a second
    // Import button contradicted it
    let mut s = library("trashed", true);
    let ids: Vec<u64> = s.catalog.photos().map(|p| p.id.0).collect();
    s.selection.ids = s.catalog.photos().map(|p| p.id).collect();
    s.execute("photo.delete", &json!({"ids": ids})).unwrap();
    let h = headless(s);
    assert!(h.app.session.has_samples(), "still removable from the Help menu");
    assert!(!has(&h, "banner:samples"));
    assert!(!has(&h, "button:samplesImport"));
    assert!(has(&h, "button:emptyImportPhotos"), "one Import, the empty state's");
}

#[test]
fn samples_added_from_the_help_menu_bring_the_banner_back() {
    let mut a = LightcraftApp::new(library("help", false), Services::default());
    a.ui.view = ViewMode::PhotoGrid;
    a.ui.samples_banner_hidden = true;
    let mut h = Headless::new(a, [1300.0, 900.0], 1.0);
    // (the first frame only waits for the fonts)
    h.step();
    h.step();
    assert_eq!(h.app.samples_present, Some(false));
    h.app.run("library.addSamples", json!({})).unwrap();
    h.step();
    h.step();
    assert!(!h.app.ui.samples_banner_hidden);
    assert!(has(&h, "banner:samples"));
}

/// Given a banner the person closed with Keep Them in a library with samples,
/// When they visit a library without samples and come back,
/// Then the banner stays closed: only samples that appear in a library bring it back.
#[test]
fn a_closed_banner_stays_closed_after_a_visit_to_a_library_without_samples() {
    let dirs = |tag: &str| std::env::temp_dir().join(format!("lc-ui-samples-{tag}-{}", std::process::id())).join("lib");
    let mut a = LightcraftApp::new(library("visit-a", true), Services::default());
    let without = library("visit-b", false);
    drop(without);
    a.ui.view = ViewMode::PhotoGrid;
    a.ui.samples_banner_hidden = true;
    let mut h = Headless::new(a, [1300.0, 900.0], 1.0);
    h.step();
    h.step();
    h.app.run("app.openLibrary", json!({"path": dirs("visit-b").to_string_lossy()})).unwrap();
    h.step();
    h.step();
    assert_eq!(h.app.samples_present, Some(false));
    h.app.run("app.openLibrary", json!({"path": dirs("visit-a").to_string_lossy()})).unwrap();
    h.step();
    h.step();
    assert!(h.app.ui.samples_banner_hidden, "closed for good");
    assert!(!has(&h, "banner:samples"));
}

#[test]
fn the_banners_buttons_stay_in_a_narrow_window_with_a_long_message() {
    // the longest translations (Ukrainian, Russian) are what pushed them out; the message gives way
    for (lang, own) in [("uk", false), ("ru", true)] {
        let s = if own { samples_and_own(&format!("narrow-{lang}")) } else { library(&format!("narrow-{lang}"), true) };
        let mut a = LightcraftApp::new(s, Services::default());
        a.ui.view = ViewMode::PhotoGrid;
        let mut h = Headless::new(a, [640.0, 640.0], 1.0);
        assert_eq!(h.request("ui.set", json!({"language": lang}), T)["ok"], true);
        h.step();
        h.step();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(640.0, 640.0));
        for b in ["button:samplesRemove", if own { "button:samplesKeep" } else { "button:samplesHide" }] {
            let r = h.app.widgets.iter().find(|(w, _)| w == b).map(|(_, r)| *r).unwrap_or_else(|| panic!("{b}"));
            assert!(screen.contains_rect(r), "{lang} {b} {r:?} is outside the window");
        }
        assert_eq!(h.request("ui.set", json!({"language": "en"}), T)["ok"], true);
    }
}

#[test]
fn adding_the_samples_says_what_came_and_where() {
    let mut h = headless(library("said", false));
    h.app.run("library.addSamples", json!({})).unwrap();
    h.step();
    let n = h.app.session.catalog.sample_photos();
    let toast = h.app.ui.toast.as_ref().map(|(text, _, _)| text.clone()).unwrap_or_default();
    assert!(toast.contains(&format!("Added {n} sample photos")) && toast.contains("“Sample Photos”"), "{toast}");
    // long enough to read, as a toast saying where to look is
    let (_, until, _) = h.app.ui.toast.clone().unwrap();
    assert!(until - h.app.last_time >= 3.0, "{until} at {}", h.app.last_time);
    // their albums are together, in the folder the toast names
    let top: Vec<String> = h.app.session.catalog.albums().filter(|a| a.parent.is_none()).map(|a| a.name.clone()).collect();
    assert_eq!(top, ["Sample Photos"]);
}

#[test]
fn a_caller_can_name_the_folder_and_the_toast_follows() {
    let mut h = headless(library("named", false));
    h.app.run("library.addSamples", json!({"folder": "Try these"})).unwrap();
    h.step();
    let toast = h.app.ui.toast.as_ref().map(|(text, _, _)| text.clone()).unwrap_or_default();
    assert!(toast.contains("“Try these”"), "{toast}");
}
