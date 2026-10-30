//! Every way into the sample photos lands in the same place: the welcome's thumbnails, its Try
//! Sample Photos button, Help ▸ Add Sample Photos and agents open a sample in Detail, ready to
//! edit, as its thumbnail showed it. There the banner walks the first edit through: an invitation
//! to drag a slider, how to compare the change with the original, what the comparison shows.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::panels::samples;
use crate::state::ViewMode;
use crate::{LightcraftApp, Services};

fn library(tag: &str) -> lightcraft_engine::Session {
    let dir = std::env::temp_dir().join(format!("lc-ui-landing-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut s = lightcraft_engine::Session::new().with_fs();
    s.open_library(dir.join("lib"), false).unwrap();
    s
}

fn headless(session: lightcraft_engine::Session) -> Headless {
    let mut a = LightcraftApp::new(session, Services::default());
    a.ui.view = ViewMode::PhotoGrid;
    let mut h = Headless::new(a, [1300.0, 900.0], 1.0);
    h.step();
    h.step();
    h
}

fn active_scene(h: &Headless) -> Option<u32> {
    let id = h.app.session.active()?;
    match h.app.session.catalog.photo(id)?.source {
        lightcraft_catalog::Source::Demo { scene } => Some(scene),
        _ => None,
    }
}

#[test]
fn samples_added_from_the_menu_land_in_detail_like_a_thumbnail_does() {
    // Given an empty library's welcome
    let mut h = headless(library("menu"));
    // When the samples come by Help ▸ Add Sample Photos (or the welcome's button, or an agent)
    h.app.run("library.addSamples", json!({})).unwrap();
    h.step();
    h.step();
    // Then the first of the welcome's samples is open in Detail
    assert_eq!(h.app.ui.view, ViewMode::Detail);
    let first = lightcraft_engine::demo::welcome_scenes().first().map(|(id, _)| *id);
    assert_eq!(active_scene(&h), first);
}

#[test]
fn samples_added_to_a_library_with_photos_leave_the_view_alone() {
    // (only the welcome's empty library is taken somewhere: a library in use stays as it is)
    // a photo of the person's own keeps the library from being empty
    let dir = std::env::temp_dir().join(format!("lc-ui-landing-photo-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let img = lightcraft_raster::Rgba8 { width: 8, height: 8, data: vec![[90, 120, 160, 255]; 64] };
    let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    std::fs::write(dir.join("own.png"), png).unwrap();
    let mut s = library("own");
    s.execute("library.import", &json!({"paths": [dir.join("own.png").to_string_lossy()]})).unwrap();
    let mut h = headless(s);
    assert!(!crate::panels::empty_library::is_empty(&h.app.session));
    h.app.run("library.addSamples", json!({})).unwrap();
    h.step();
    assert_eq!(h.app.ui.view, ViewMode::PhotoGrid);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_banner_walks_the_first_edit_through() {
    let mut h = headless(library("invite"));
    h.app.run("library.addSamples", json!({})).unwrap();
    h.step();
    h.step();
    // in Detail on a sample, nothing changed yet: an invitation to edit, with the compare key
    let text = samples::message(&h.app, true).unwrap_or_default();
    assert!(text.starts_with("Drag any slider"), "{text}");
    assert!(text.contains("Y"), "the key Before/After Side by Side is bound to: {text}");
    // after a first change: it says so, and how to see it beside the original
    let r = h.request(
        "engine.execute",
        json!({"command": "develop.set", "params": {"control": "light.exposure", "value": 0.5}}),
        Duration::from_secs(20),
    );
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    let text = samples::message(&h.app, true).unwrap_or_default();
    assert!(text.starts_with("That's your first edit."), "{text}");
    assert!(text.contains("Press Y to see it beside the original"), "{text}");
    // comparing: what is on screen
    h.app.run("view.beforeAfter", json!({})).unwrap();
    h.step();
    let text = samples::message(&h.app, true).unwrap_or_default();
    assert!(text.starts_with("The original beside your edit"), "{text}");
    // and back: the usual message from then on, whatever comes next
    h.app.run("view.beforeAfter", json!({})).unwrap();
    h.step();
    let text = samples::message(&h.app, true).unwrap_or_default();
    assert!(text.starts_with("These are sample photos"), "{text}");
    h.app.run("view.beforeAfter", json!({})).unwrap();
    h.step();
    let text = samples::message(&h.app, true).unwrap_or_default();
    assert!(text.starts_with("These are sample photos"), "compared once is enough: {text}");
}

#[test]
fn comparing_before_any_change_still_invites_one() {
    let mut h = headless(library("early"));
    h.app.run("library.addSamples", json!({})).unwrap();
    h.step();
    h.app.run("view.beforeAfter", json!({})).unwrap();
    h.step();
    let text = samples::message(&h.app, true).unwrap_or_default();
    assert!(text.starts_with("Drag any slider"), "nothing to compare yet: {text}");
}

#[test]
fn a_sample_that_comes_edited_is_invited_as_well_until_the_person_edits_it() {
    // a few samples come with settings of their own: opening one is not a first edit
    let mut h = headless(library("came-edited"));
    h.app.run("library.addSamples", json!({})).unwrap();
    h.step();
    let edited = h
        .app
        .session
        .catalog
        .photos()
        .find(|p| Some(p.id) != h.app.session.active() && *p.develop != lightcraft_develop::DevelopSettings::default())
        .map(|p| p.id)
        .expect("a sample that comes edited");
    h.app.session.selection = lightcraft_engine::Selection::single(edited);
    h.step();
    let text = samples::message(&h.app, true).unwrap_or_default();
    assert!(text.starts_with("Drag any slider"), "opening an edited sample is not a first edit: {text}");
    // the person's own change to it is
    set_exposure(&mut h, 0.5);
    assert!(samples::message(&h.app, true).unwrap_or_default().starts_with("That's your first edit."));
}

#[test]
fn comparing_ends_the_walk_even_if_the_edit_is_undone_meanwhile() {
    let mut h = headless(library("compare-undo"));
    h.app.run("library.addSamples", json!({})).unwrap();
    h.step();
    set_exposure(&mut h, 0.5);
    h.app.run("view.beforeAfter", json!({})).unwrap();
    h.step();
    assert!(samples::message(&h.app, true).unwrap_or_default().starts_with("The original beside your edit"));
    // the edit is undone while comparing, and then the comparison is closed
    h.app.run("edit.undo", json!({})).unwrap();
    h.step();
    h.app.run("view.beforeAfter", json!({})).unwrap();
    h.step();
    let text = samples::message(&h.app, true).unwrap_or_default();
    assert!(text.starts_with("These are sample photos"), "the walk is over: {text}");
}

#[test]
fn the_grid_of_samples_keeps_the_usual_message() {
    let mut h = headless(library("grid"));
    h.app.run("library.addSamples", json!({})).unwrap();
    h.app.ui.view = ViewMode::PhotoGrid;
    h.step();
    let text = samples::message(&h.app, true).unwrap_or_default();
    assert!(text.starts_with("These are sample photos"), "{text}");
}

#[test]
fn landing_opens_the_edit_panel_with_its_sliders() {
    // even when the panel put aside for the welcome was another, or none
    let mut h = headless(library("panel"));
    h.app.ui.right_before_welcome = Some(crate::state::RightPanel::Info);
    h.app.run("library.addSamples", json!({})).unwrap();
    h.step();
    h.step();
    assert_eq!(h.app.ui.right, crate::state::RightPanel::Edit);
}

#[test]
fn samples_added_while_a_local_folder_is_shown_leave_the_view_alone() {
    // the library has no photos of its own, but the welcome isn't what is on screen
    let mut h = headless(library("folder"));
    h.app.session.source = lightcraft_engine::LibrarySource::Folder;
    h.step();
    h.app.run("library.addSamples", json!({})).unwrap();
    h.step();
    assert_eq!(h.app.ui.view, ViewMode::PhotoGrid);
}

fn set_exposure(h: &mut Headless, value: f64) {
    let r = h.request(
        "engine.execute",
        json!({"command": "develop.set", "params": {"control": "light.exposure", "value": value}}),
        Duration::from_secs(20),
    );
    assert_eq!(r["ok"], true, "{r}");
    h.step();
}

#[test]
fn only_an_edit_on_the_sample_open_counts_as_one() {
    let mut h = headless(library("not-an-edit"));
    h.app.run("library.addSamples", json!({})).unwrap();
    h.step();
    // a rating is a change to the library, not an edit
    h.app.run("photo.rate", json!({"rating": 3})).unwrap();
    h.step();
    let text = samples::message(&h.app, true).unwrap_or_default();
    assert!(text.starts_with("Drag any slider"), "{text}");
    // an edit is
    set_exposure(&mut h, 0.5);
    assert!(samples::message(&h.app, true).unwrap_or_default().starts_with("That's your first edit."));
    // another sample, unedited, is invited again
    let other = h
        .app
        .session
        .catalog
        .photos()
        .find(|p| Some(p.id) != h.app.session.active() && *p.develop == lightcraft_develop::DevelopSettings::default())
        .map(|p| p.id)
        .unwrap();
    h.app.session.selection = lightcraft_engine::Selection::single(other);
    h.step();
    assert!(samples::message(&h.app, true).unwrap_or_default().starts_with("Drag any slider"));
    // and an edit undone is no edit any more
    set_exposure(&mut h, 0.5);
    h.app.run("edit.undo", json!({})).unwrap();
    h.step();
    assert!(samples::message(&h.app, true).unwrap_or_default().starts_with("Drag any slider"));
}

#[test]
fn show_original_alone_is_not_a_comparison() {
    let mut h = headless(library("original"));
    h.app.run("library.addSamples", json!({})).unwrap();
    h.step();
    set_exposure(&mut h, 0.5);
    h.app.ui.before_after = crate::state::BeforeAfter::Original;
    h.step();
    assert!(samples::message(&h.app, true).unwrap_or_default().starts_with("That's your first edit."));
}
