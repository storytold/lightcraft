//! Headless tests of what a touch screen offers in place of hovering, right-clicking with
//! modifiers and keys: everything the desktop does has a way in by finger (`docs/parity.md`).

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::ViewMode;
use crate::{LightcraftApp, Services};

const SETTLE: Duration = Duration::from_secs(120);

/// The demo library in `view` on a touch screen of `size`, announced as the mobile shell does.
fn touch_app(view: ViewMode, size: [f32; 2]) -> Headless {
    let services = Services { png: None, ..Default::default() };
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    app.ui.view = view;
    let mut h = Headless::new(app, size, 1.0);
    h.input([finger(egui::TouchPhase::Cancel, egui::Pos2::ZERO, 0)]);
    h.settle(SETTLE);
    h
}

fn finger(phase: egui::TouchPhase, pos: egui::Pos2, id: u64) -> egui::Event {
    egui::Event::Touch { device_id: egui::TouchDeviceId(1), id: egui::TouchId(id), phase, pos, force: None }
}

fn button(pos: egui::Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }
}

/// A finger down and up at `at`, then lifted away.
fn tap(h: &mut Headless, at: egui::Pos2) {
    h.input([finger(egui::TouchPhase::Start, at, 1), egui::Event::PointerMoved(at), button(at, true)]);
    h.step();
    h.input([finger(egui::TouchPhase::End, at, 1), button(at, false)]);
    h.step();
    h.input([egui::Event::PointerGone]);
    h.step();
    // past a double tap
    for _ in 0..40 {
        h.step();
    }
}

fn widget(h: &Headless, id: &str) -> Option<egui::Rect> {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r)
}

/// Photos in the grid whose middle is on screen (between the header and `bottom`).
fn thumbs(h: &Headless, bottom: f32) -> Vec<(u64, egui::Rect)> {
    h.app
        .widgets
        .iter()
        .filter(|(w, r)| w.starts_with("thumb:") && r.center().y > 140.0 && r.center().y < bottom)
        .filter_map(|(w, r)| Some((w.trim_start_matches("thumb:").parse().ok()?, *r)))
        .collect()
}

/// A finger has no ⌘ or ⇧: Select Several makes taps add photos to the selection and take them
/// out again, and the grid shows a tick on each.
#[test]
fn select_several_makes_taps_add_and_remove_photos() {
    // (tall: the grid registers a row's first photo, and two rows have to fit)
    let mut h = touch_app(ViewMode::PhotoGrid, [1194.0, 1400.0]);
    let button = widget(&h, "button:selectMode").expect("Select in the bottom bar");
    tap(&mut h, button.center());
    assert!(h.app.ui.select_mode);
    // two photos not selected yet
    let t: Vec<(u64, egui::Rect)> =
        thumbs(&h, 1300.0).into_iter().filter(|(id, _)| !h.app.session.selection.contains(lightcraft_catalog::PhotoId(*id))).collect();
    let (a, b) = (t[0], t[1]);
    tap(&mut h, a.1.center());
    tap(&mut h, b.1.center());
    let sel: Vec<u64> = h.app.session.selection.ids.iter().map(|p| p.0).collect();
    assert!(sel.contains(&a.0) && sel.contains(&b.0), "both taps added: {sel:?}");
    let n = sel.len();
    tap(&mut h, a.1.center());
    assert!(!h.app.session.selection.contains(lightcraft_catalog::PhotoId(a.0)), "a second tap takes it out");
    assert_eq!(h.app.session.selection.ids.len(), n - 1);
    assert_eq!(h.app.ui.view, ViewMode::PhotoGrid, "taps in Select Several never open the photo");
    // Done: taps pick one photo again
    let done = widget(&h, "button:selectMode").unwrap();
    tap(&mut h, done.center());
    tap(&mut h, b.1.center());
    assert_eq!(h.app.session.selection.ids.len(), 1);
}

/// An iPad with a trackpad: the screen is a touch screen, but the trackpad's pointer drags photos
/// (to an album) as a mouse does; only a finger's drag scrolls.
#[test]
fn a_trackpad_on_a_touch_screen_still_drags_photos() {
    let mut h = touch_app(ViewMode::PhotoGrid, [1194.0, 834.0]);
    let (_, r) = thumbs(&h, 700.0)[0];
    let mut p = r.center();
    h.input([egui::Event::PointerMoved(p)]);
    h.step();
    h.input([button(p, true)]);
    h.step();
    for _ in 0..6 {
        p += egui::vec2(20.0, 10.0);
        h.input([egui::Event::PointerMoved(p)]);
        h.step();
    }
    assert!(h.app.ui.dragging_photos.is_some(), "the trackpad drags the photo");
    h.input([button(p, false)]);
    h.step();
}

/// Full screen ends with Esc and a slideshow pauses with Space; a finger gets Exit in full screen,
/// and in a slideshow a tap brings up its controls (Pause, End).
#[test]
fn full_screen_and_slideshows_have_exit_and_pause_on_a_touch_screen() {
    let mut h = touch_app(ViewMode::Detail, [834.0, 1194.0]);
    h.app.run("view.fullScreenPreview", json!({})).unwrap();
    h.step();
    h.step();
    let exit = widget(&h, "button:fullscreenExit").expect("Exit on screen");
    tap(&mut h, exit.center());
    assert!(!h.app.ui.fullscreen, "Exit leaves full screen");
    h.app.run("view.slideshow", json!({})).unwrap();
    h.step();
    h.step();
    assert!(widget(&h, "button:fullscreenExit").is_none(), "the slideshow has its own controls");
    // a tap shows them (within their few seconds on screen)
    let c = egui::pos2(417.0, 597.0);
    h.input([finger(egui::TouchPhase::Start, c, 1), egui::Event::PointerMoved(c), button(c, true)]);
    h.step();
    h.input([finger(egui::TouchPhase::End, c, 1), button(c, false)]);
    h.step();
    let pause = widget(&h, "icon:slideshowPlayPause").expect("Pause on screen after a tap");
    let p = pause.center();
    h.input([finger(egui::TouchPhase::Start, p, 1), egui::Event::PointerMoved(p), button(p, true)]);
    h.step();
    h.input([finger(egui::TouchPhase::End, p, 1), button(p, false)]);
    h.step();
    assert!(h.app.ui.slideshow.as_ref().is_some_and(|r| r.play.is_paused()), "paused");
    let end = widget(&h, "icon:slideshowEnd").expect("End on screen");
    let e = end.center();
    h.input([finger(egui::TouchPhase::Start, e, 1), egui::Event::PointerMoved(e), button(e, true)]);
    h.step();
    h.input([finger(egui::TouchPhase::End, e, 1), button(e, false)]);
    h.step();
    assert!(h.app.ui.slideshow.is_none(), "ended");
}

/// A section's on/off eye shows on hover with a mouse; on a touch screen it's always there.
#[test]
fn edit_section_eyes_show_without_hovering_on_a_touch_screen() {
    let mut h = touch_app(ViewMode::Detail, [1194.0, 834.0]);
    h.app.ui.right = crate::state::RightPanel::Edit;
    h.settle(SETTLE);
    let eyes = h.app.widgets.iter().filter(|(w, _)| w.starts_with("sectionEye:")).count();
    assert!(eyes > 0, "eyes without a pointer over the headers");
}

/// The phone layout shows Info (and Keywords, Versions, History) in the bottom sheet over the
/// grid, so the selected photos' metadata can be edited there as on the desktop.
#[test]
fn the_phone_grid_shows_info_in_the_sheet() {
    let mut h = touch_app(ViewMode::PhotoGrid, [390.0, 844.0]);
    h.app.run("panel.info", json!({})).unwrap();
    h.settle(SETTLE);
    assert_eq!(h.app.ui.view, ViewMode::PhotoGrid, "the grid stays");
    assert!(widget(&h, "panel:sheet").is_some(), "the sheet is up");
}

/// The phone grid has no thumbnail slider: pinching out shows fewer photos across, in more.
#[test]
fn pinching_the_phone_grid_changes_how_many_photos_go_across() {
    let mut h = touch_app(ViewMode::PhotoGrid, [390.0, 844.0]);
    let cols = h.app.ui.grid_columns;
    let pinch = |h: &mut Headless, from: f32, to: f32| {
        let c = egui::pos2(195.0, 400.0);
        let at = |d: f32| [c - egui::vec2(d, 0.0), c + egui::vec2(d, 0.0)];
        let [a, b] = at(from);
        h.input([finger(egui::TouchPhase::Start, a, 1), finger(egui::TouchPhase::Start, b, 2)]);
        h.step();
        for i in 1..=8 {
            let [a, b] = at(from + (to - from) * i as f32 / 8.0);
            h.input([finger(egui::TouchPhase::Move, a, 1), finger(egui::TouchPhase::Move, b, 2), egui::Event::PointerMoved(a)]);
            h.step();
        }
        let [a, b] = at(to);
        h.input([finger(egui::TouchPhase::End, a, 1), finger(egui::TouchPhase::End, b, 2), egui::Event::PointerGone]);
        h.step();
    };
    pinch(&mut h, 40.0, 70.0);
    assert!(h.app.ui.grid_columns < cols, "pinching out: fewer across ({cols} → {})", h.app.ui.grid_columns);
    let fewer = h.app.ui.grid_columns;
    pinch(&mut h, 120.0, 40.0);
    assert!(h.app.ui.grid_columns > fewer, "pinching in: more across");
}

/// ← / → step the candidate in Compare; the View menu has them for touch screens.
#[test]
fn compare_candidates_step_from_the_menu() {
    let mut h = touch_app(ViewMode::PhotoGrid, [1194.0, 834.0]);
    assert!(!crate::menus::ui_enabled(&h.app, "compare.next"), "only in Compare");
    h.app.run("view.compare", json!({})).unwrap();
    h.settle(SETTLE);
    assert!(crate::menus::ui_enabled(&h.app, "compare.next"));
    let before = crate::panels::compare::compare_pair(&mut h.app);
    h.app.run("compare.next", json!({})).unwrap();
    let after = crate::panels::compare::compare_pair(&mut h.app);
    assert_eq!(before.map(|p| p.0), after.map(|p| p.0), "the select stays");
    assert_ne!(before.map(|p| p.1), after.map(|p| p.1), "the candidate moves on");
    h.app.run("compare.previous", json!({})).unwrap();
    assert_eq!(crate::panels::compare::compare_pair(&mut h.app), before, "and back");
}
