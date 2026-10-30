//! The welcome owns the window while the library is empty: the Tools panel steps aside (and comes
//! back as it was once photos arrive), the bottom bar's rating and flags hide, and a row of sample
//! photos under the buttons opens the samples on the one clicked. The window around it steps back:
//! no photo count, no bottom bar, one line in the sidebar for the sections still to fill.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::{RightPanel, ViewMode};
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

/// A new library on disk (the welcome's thumbnails are offered over a library, as the samples
/// banner is).
fn library(tag: &str) -> lightcraft_engine::Session {
    let dir = std::env::temp_dir().join(format!("lc-ui-welcome-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut s = lightcraft_engine::Session::new().with_fs();
    s.open_library(dir.join("lib"), false).unwrap();
    s
}

fn headless(session: lightcraft_engine::Session, size: [f32; 2]) -> Headless {
    let mut a = LightcraftApp::new(session, Services::default());
    a.ui.view = ViewMode::PhotoGrid;
    a.ui.right = RightPanel::Edit;
    let mut h = Headless::new(a, size, 1.0);
    h.step();
    h.step();
    h
}

/// Steps until a sample thumbnail has arrived from the worker that draws them (the tiles are
/// there from the first frame, as placeholders).
fn thumbnails_shown(h: &mut Headless) -> bool {
    let arrived = |h: &Headless| h.app.welcome_samples.as_ref().is_some_and(|s| s.has_thumbnail());
    h.step_until(T, |h| arrived(h));
    arrived(h) && h.app.widgets.iter().any(|(w, _)| w.starts_with("button:sample:"))
}

/// A photo of the person's own comes into the library, and the window shows it.
fn import_own_photo(h: &mut Headless, tag: &str) {
    let dir = std::env::temp_dir().join(format!("lc-ui-welcome-{tag}-photo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let img = lightcraft_raster::Rgba8 { width: 8, height: 8, data: vec![[90, 120, 160, 255]; 64] };
    let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    std::fs::write(dir.join("own.png"), png).unwrap();
    h.app.session.execute("library.import", &json!({"paths": [dir.join("own.png").to_string_lossy()]})).unwrap();
    h.step();
    h.step();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_empty_library_puts_the_tools_panel_aside_until_photos_arrive() {
    // Given an empty library opened with the Edit panel out
    let mut h = headless(library("aside"), [1300.0, 900.0]);
    // Then the welcome has the window: the panel is put aside, remembered
    assert_eq!(h.app.ui.right, RightPanel::None);
    assert_eq!(h.app.ui.right_before_welcome, Some(RightPanel::Edit));
    // When photos of the person's own arrive (samples open the Edit panel anyway), it is back as it was
    import_own_photo(&mut h, "aside");
    assert_eq!(h.app.ui.right, RightPanel::Edit);
    assert_eq!(h.app.ui.right_before_welcome, None);
}

#[test]
fn a_panel_opened_while_the_library_is_empty_stays_open() {
    let mut h = headless(library("open"), [1300.0, 900.0]);
    assert_eq!(h.app.ui.right, RightPanel::None);
    h.app.run("panel.info", json!({})).unwrap();
    h.step();
    h.step();
    assert_eq!(h.app.ui.right, RightPanel::Info, "only becoming empty puts it aside");
}

#[test]
fn the_panel_comes_back_after_a_restart_while_the_library_was_empty() {
    // quit while empty: ui.json held the panel put aside; the library has photos by the next launch
    let mut a = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
    a.ui.right = RightPanel::None;
    a.ui.right_before_welcome = Some(RightPanel::Info);
    let mut h = Headless::new(a, [1300.0, 900.0], 1.0);
    h.step();
    h.step();
    assert_eq!(h.app.ui.right, RightPanel::Info);
}

#[test]
fn an_editing_tool_put_aside_comes_back_as_the_edit_panel() {
    // the photo it was used on is gone: Masking on a newly arrived photo would be a surprise
    let mut a = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
    a.ui.right = RightPanel::None;
    a.ui.right_before_welcome = Some(RightPanel::Masking);
    let mut h = Headless::new(a, [1300.0, 900.0], 1.0);
    h.step();
    h.step();
    assert_eq!(h.app.ui.right, RightPanel::Edit);
}

#[test]
fn a_panel_opened_and_closed_during_the_welcome_stays_closed() {
    let mut h = headless(library("closed"), [1300.0, 900.0]);
    h.app.run("panel.info", json!({})).unwrap();
    h.step();
    h.step();
    h.app.ui.right = RightPanel::None;
    h.step();
    // photos of the person's own arrive (samples instead open the Edit panel, to edit one)
    import_own_photo(&mut h, "closed");
    assert_eq!(h.app.ui.right, RightPanel::None, "closing it was the person's choice");
}

#[test]
fn the_thumbnails_go_with_the_welcome_and_the_window_settles() {
    // photos arriving while the thumbnails are still drawing: nothing keeps waiting for them
    let mut h = headless(library("settle"), [1600.0, 1000.0]);
    assert!(h.app.welcome_samples.is_some());
    h.app.run("library.addSamples", json!({})).unwrap();
    h.step();
    h.step();
    assert!(h.app.welcome_samples.is_none());
    assert!(h.settle(T), "nothing left busy");
}

#[test]
fn the_thumbnails_go_when_another_view_replaces_the_welcome() {
    // the library is still empty, but People (or Compare, or a filter's message) is on screen
    let mut h = headless(library("people"), [1600.0, 1000.0]);
    assert!(h.app.welcome_samples.is_some());
    h.app.ui.view = ViewMode::People;
    h.step();
    h.step();
    assert!(h.app.welcome_samples.is_none());
    assert!(h.settle(T), "nothing left busy");
}

#[test]
fn a_row_out_of_room_for_a_moment_keeps_its_thumbnails() {
    // a resize past the threshold and back: no new worker, no placeholders flashing back
    let mut h = headless(library("resize"), [1600.0, 1000.0]);
    assert!(thumbnails_shown(&mut h));
    // 380 is below the room check (`a_short_view_offers_the_button_instead_of_the_row` shows the same height): 440 no longer is
    assert_eq!(h.request("ui.resize", json!({"width": 1600, "height": 380}), T)["ok"], true);
    h.step();
    h.step();
    assert!(has(&h, "button:emptySamples"), "the row is out of room: the button stands in for it");
    assert!(!h.app.widgets.iter().any(|(w, _)| w.starts_with("button:sample:")), "no thumbnail is drawn");
    assert!(h.app.welcome_samples.as_ref().is_some_and(|s| s.has_thumbnail()), "kept while the row is out of room");
    assert_eq!(h.request("ui.resize", json!({"width": 1600, "height": 1000}), T)["ok"], true);
    h.step();
    assert!(h.app.welcome_samples.as_ref().is_some_and(|s| s.has_thumbnail()));
}

#[test]
fn a_short_view_offers_the_button_instead_of_the_row() {
    // a wide but short window: the welcome's view is too low for the row under the message
    let mut h = headless(library("short"), [1300.0, 380.0]);
    h.step();
    assert!(has(&h, "button:emptySamples"));
    assert!(!h.app.widgets.iter().any(|(w, _)| w.starts_with("button:sample:")));
}

#[test]
fn the_bottom_bar_offers_no_rating_or_flags_with_nothing_to_rate() {
    let mut h = headless(library("bottom"), [1300.0, 900.0]);
    // (the Photo Grid hides the whole bar anyway: People shows it, so it is the view that reaches the check)
    h.app.ui.view = ViewMode::People;
    h.step();
    h.step();
    assert!(!crate::panels::empty_library::in_view(&h.app), "People is not the welcome");
    assert!(!has(&h, "star:bottom:1"));
    assert!(!has(&h, "flag:pick"));
    let h = headless(lightcraft_engine::Session::with_demo(), [1300.0, 900.0]);
    assert!(has(&h, "star:bottom:1"));
}

#[test]
fn the_welcome_shows_sample_photos_that_open_the_one_clicked() {
    // Given the welcome of a new library
    let mut h = headless(library("strip"), [1600.0, 1000.0]);
    assert!(thumbnails_shown(&mut h), "the sample thumbnails render and show");
    assert!(!has(&h, "button:emptySamples"), "the row replaces the button");
    // (a later one than the first, which adding the samples opens anyway)
    let id = h.app.widgets.iter().filter(|(w, _)| w.starts_with("button:sample:")).nth(2).map(|(w, _)| w.clone()).unwrap();
    let scene: u32 = id.trim_start_matches("button:sample:").parse().unwrap();
    // When one is clicked
    assert_eq!(h.request("ui.clickWidget", json!({"id": id}), T)["ok"], true);
    h.step();
    h.step();
    // Then the samples are in, and that photo is open in Detail, ready to edit
    assert!(h.app.session.has_samples());
    let active = h.app.session.active().and_then(|id| h.app.session.catalog.photo(id).cloned()).unwrap();
    assert!(matches!(active.source, lightcraft_catalog::Source::Demo { scene: s } if s == scene));
    assert_eq!(h.app.ui.view, ViewMode::Detail);
    assert_eq!(h.app.ui.right, RightPanel::Edit, "the panel put aside is back to edit with");
}

#[test]
fn a_column_too_narrow_for_the_row_offers_the_button() {
    // In the docked layout a 540 or 560 point window still leaves the center column 360 wide,
    // enough for three thumbnails, so the rule is checked where it is made. Under three
    // thumbnails wide there is no row (`show` then offers the button, as `a_short_view_offers_the_button_
    // instead_of_the_row` shows for a short view), at three there is one.
    use crate::panels::empty_library::strip_layout;
    assert!(strip_layout(300.0).is_none());
    assert!(strip_layout(307.0).is_none());
    assert_eq!(strip_layout(312.0).map(|(n, _)| n), Some(3));
}

#[test]
fn a_temporary_session_offers_no_samples() {
    // nothing done there is saved: samples would only be lost
    let mut a = LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
    a.ui.view = ViewMode::PhotoGrid;
    a.library_problem = Some(crate::panels::library_problem::LibraryProblem {
        dismissed: true,
        ..crate::panels::library_problem::LibraryProblem::new("/nowhere", "test")
    });
    let mut h = Headless::new(a, [1300.0, 900.0], 1.0);
    h.step();
    h.step();
    h.step();
    assert!(has(&h, "button:emptyImportPhotos"));
    assert!(!has(&h, "button:emptySamples"));
    assert!(!h.app.widgets.iter().any(|(w, _)| w.starts_with("button:sample:")));
}

#[test]
fn the_welcome_keeps_clear_of_the_grid_header() {
    // a short window: the welcome sits below "All Photos", never over it
    for size in [[1300.0, 440.0], [1300.0, 900.0]] {
        let h = headless(library(&format!("header-{}", size[1])), size);
        let title = h.app.widgets.iter().find(|(w, _)| w == "text:emptyTitle").map(|(_, r)| *r).unwrap();
        // the header is 44 points under the 42-point top bar
        assert!(title.top() >= 42.0 + 44.0, "{size:?}: the title starts at {}", title.top());
    }
}

#[test]
fn the_welcome_keeps_clear_of_the_filter_bar() {
    // the Filter Bar open (`\`) under the header: the welcome sits below it, whatever the height
    for height in (400..=560).step_by(20) {
        let mut h = headless(library(&format!("bar-{height}")), [1300.0, height as f32]);
        h.app.ui.filter_bar = true;
        h.step();
        h.step();
        let title = widget(&h, "text:emptyTitle");
        let bar = widget(&h, "filterbar");
        assert!(title.top() >= bar.bottom(), "{height}: the title starts at {}, the bar ends at {}", title.top(), bar.bottom());
    }
}

fn widget(h: &Headless, id: &str) -> egui::Rect {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no {id}"))
}

#[test]
fn the_window_steps_back_while_the_welcome_shows() {
    // Given a new library's welcome, with the sidebar out as an empty library opens
    let mut a = LightcraftApp::new(library("quiet"), Services::default());
    a.ui.view = ViewMode::PhotoGrid;
    a.ui.left_panel = true;
    let mut h = Headless::new(a, [1600.0, 1000.0], 1.0);
    h.step();
    h.step();
    // Then nothing around it competes: no "0 photos", no bottom bar, and one line in the sidebar
    // in place of four empty sections
    assert!(has(&h, "grid:title") && !has(&h, "grid:count"));
    assert!(!has(&h, "slider:thumbSize"), "the bottom bar is not drawn");
    assert!(has(&h, "sidebarEmpty:welcome"));
    for section in ["albums", "byDate", "folders", "keywords"] {
        assert!(!has(&h, &format!("sidebarSection:{section}")), "{section} is folded into the line");
    }
    // When photos arrive, the window is back as it was
    h.app.run("library.addSamples", json!({})).unwrap();
    h.app.ui.view = ViewMode::PhotoGrid;
    h.step();
    h.step();
    assert!(has(&h, "grid:count") && has(&h, "slider:thumbSize"));
    assert!(!has(&h, "sidebarEmpty:welcome"));
    assert!(has(&h, "sidebarSection:albums") && has(&h, "sidebarSection:keywords"));
}

#[test]
fn another_view_keeps_the_window_as_it_is() {
    // the welcome isn't what the People view shows: its way out stays
    let mut h = headless(library("people-keeps"), [1600.0, 1000.0]);
    h.app.ui.view = ViewMode::People;
    h.step();
    h.step();
    assert!(!crate::panels::empty_library::in_view(&h.app), "People keeps its bottom bar");
    h.app.ui.view = ViewMode::Detail;
    h.step();
    assert!(crate::panels::empty_library::in_view(&h.app), "Detail with nothing open shows the welcome");
    h.step();
    assert!(has(&h, "text:emptyTitle"));
    assert!(!has(&h, "slider:thumbSize"), "no bottom bar");
    let canvas = h.app.canvas_rect.unwrap();
    assert!(canvas.bottom() >= 999.0, "no empty filmstrip under the welcome: {canvas:?}");
}

#[test]
fn the_welcome_reads_as_a_short_paragraph() {
    // a wide window: the message breaks into even lines rather than running across it
    let h = headless(library("measure"), [1600.0, 1000.0]);
    let title = widget(&h, "text:emptyTitle");
    let body = widget(&h, "text:emptyBody");
    assert!(body.width() <= 540.0, "the body runs {} wide", body.width());
    assert!(body.height() > title.height(), "two lines: {body:?}");
    assert!(body.width() > 360.0, "the lines are even, not one long over one short: {body:?}");
    // still centred under the title
    assert!((body.center().x - title.center().x).abs() < 2.0);
}
