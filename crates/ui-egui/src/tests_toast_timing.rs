//! The HUD toast stays long enough to read, errors stay 6 s, a pointer resting on it holds it,
//! and it fades in. Time is the headless harness's simulated clock (1/60 s per frame).

use egui::Pos2;
use lightcraft_catalog::ColorLabel;
use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const LONG: &str = "Smart albums update automatically: change their rules, or use Photo ▸ Delete Photo";

fn app() -> Headless {
    let mut h =
        Headless::new(LightcraftApp::new(lightcraft_engine::Session::new(), Services { png: None, ..Default::default() }), [1000.0, 700.0], 1.0);
    h.step();
    h.step();
    h
}

fn ctx(h: &Headless) -> egui::Context {
    h.app.tasks.repaint.clone().expect("a frame has run")
}

/// Run frames for `secs` of simulated time.
fn run_for(h: &mut Headless, secs: f64) {
    let end = h.time() + secs;
    while h.time() < end {
        h.step();
    }
}

fn shown(h: &Headless) -> bool {
    h.app.ui.toast.is_some()
}

/// Where the toast sits: the middle of its box, 60 px above the canvas bottom.
fn toast_centre(h: &Headless) -> Pos2 {
    let canvas = h.app.canvas_rect.expect("canvas");
    egui::pos2(canvas.center().x, canvas.bottom() - 60.0)
}

fn point_at(h: &mut Headless, p: Pos2) {
    h.events.push(egui::Event::PointerMoved(p));
    h.step();
}

#[test]
fn a_long_instruction_outlasts_the_plain_toast_time() {
    let mut h = app();
    let c = ctx(&h);
    h.app.toast(&c, LONG);
    run_for(&mut h, 1.5);
    assert!(shown(&h), "still readable after 1.4 s");
    run_for(&mut h, 3.5);
    assert!(shown(&h), "about 5.3 s to read it");
    run_for(&mut h, 0.6);
    assert!(!shown(&h), "gone after its reading time");
}

#[test]
fn a_short_confirmation_still_goes_quickly() {
    let mut h = app();
    let c = ctx(&h);
    h.app.toast(&c, "Library saved");
    run_for(&mut h, 1.5);
    assert!(shown(&h));
    run_for(&mut h, 0.4);
    assert!(!shown(&h), "about 1.7 s");
}

#[test]
fn a_failing_command_says_why_for_six_seconds() {
    let mut h = app();
    let r = crate::menubar::run_item(&mut h.app, "file.importKeywords", json!({"path": "/no/such/keywords.txt"}));
    assert!(r.is_err());
    assert!(shown(&h));
    run_for(&mut h, 5.8);
    assert!(shown(&h), "an error is not gone after 1.4 s");
    run_for(&mut h, 0.4);
    assert!(!shown(&h), "but does not stay for ever");
}

#[test]
fn a_pointer_resting_on_the_toast_holds_it_and_leaving_gives_a_second_and_a_half() {
    let mut h = app();
    let c = ctx(&h);
    h.app.toast(&c, "Library saved");
    let p = toast_centre(&h);
    point_at(&mut h, p);
    run_for(&mut h, 5.0);
    assert!(shown(&h), "held while pointed at, long past its time");
    let alpha_painted = h.app.ui.toast.as_ref().map(|t| crate::toast::toast_alpha(h.time(), t.shown_at, t.until));
    assert_eq!(alpha_painted, Some(1.0), "and not fading");
    point_at(&mut h, egui::pos2(5.0, 5.0));
    run_for(&mut h, 1.4);
    assert!(shown(&h), "1.5 s more after the pointer leaves");
    run_for(&mut h, 0.5);
    assert!(!shown(&h), "then fades out and goes");
}

#[test]
fn the_toast_does_not_take_clicks_from_what_is_under_it() {
    use lightcraft_catalog::{Op, Photo, PhotoId, Source};
    let mut s = lightcraft_engine::Session::new();
    for id in 1..=12 {
        let p = Photo::new(PhotoId(id), Source::Demo { scene: 1 }, &format!("{id}.jpg"), "JPEG", 640, 480, "2026-10-08");
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    s.execute("library.sort", &json!({"key": "fileName", "ascending": true, "group": "none"})).unwrap();
    s.execute("library.select", &json!({"ids": [1]})).unwrap();
    let mut h = Headless::new(LightcraftApp::new(s, Services { png: None, ..Default::default() }), [1000.0, 700.0], 1.0);
    h.app.ui.view = crate::state::ViewMode::SquareGrid;
    h.app.ui.right = crate::state::RightPanel::None;
    h.app.ui.thumb_size = 300.0;
    for _ in 0..10 {
        h.step();
    }
    let c = ctx(&h);
    h.app.toast(&c, "Library saved");
    h.step();
    let p = toast_centre(&h);
    let (cell, rect) = h
        .app
        .widgets
        .iter()
        .find_map(|(w, r)| w.strip_prefix("thumb:").filter(|_| r.contains(p)).map(|id| (id.to_string(), *r)))
        .expect("a thumbnail lies under the toast");
    let id: u64 = cell.parse().unwrap();
    assert!(rect.contains(p));
    assert_ne!(h.app.session.selected(), vec![PhotoId(id)], "not selected yet");
    point_at(&mut h, p);
    h.events.push(egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
    h.step();
    h.events.push(egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
    h.step();
    h.step();
    assert_eq!(h.app.session.selected(), vec![PhotoId(id)], "the click went through to the thumbnail");
}

/// How far the painted pixel at the toast's left padding moved from the background.
fn tint(h: &Headless, bare: &egui::ColorImage) -> i32 {
    let p = toast_centre(h) - egui::vec2(24.0, 0.0);
    let at = |image: &egui::ColorImage| {
        let px = image.pixels[p.y as usize * image.size[0] + p.x as usize];
        i32::from(px.r()) + i32::from(px.g()) + i32::from(px.b())
    };
    (at(&h.paint()) - at(bare)).abs()
}

#[test]
fn a_new_toast_is_drawn_translucent_first_and_opaque_later() {
    let mut h = app();
    let bare = h.paint();
    let c = ctx(&h);
    h.app.toast(&c, "Hi");
    h.app.ui.toast.as_mut().unwrap().label = ColorLabel::parse("red");
    h.step();
    let first = tint(&h, &bare);
    for _ in 0..30 {
        h.step();
    }
    let later = tint(&h, &bare);
    assert!(later > 100, "the box is clearly visible once faded in: {later}");
    assert!(first < later / 2, "the first frame is translucent: first {first}, later {later}");
}

#[test]
fn the_same_text_shown_again_extends_without_restarting_the_fade() {
    let mut h = app();
    let c = ctx(&h);
    h.app.toast(&c, "Library saved");
    let first = h.app.ui.toast.clone().unwrap();
    run_for(&mut h, 1.0);
    h.app.toast(&c, "Library saved");
    let again = h.app.ui.toast.clone().unwrap();
    assert_eq!(again.shown_at, first.shown_at, "no fade-in flicker");
    assert!(again.until > first.until, "but it stays longer");
}

#[test]
fn a_different_text_replaces_it_and_fades_in_again() {
    let mut h = app();
    let c = ctx(&h);
    h.app.toast(&c, "Library saved");
    let first = h.app.ui.toast.clone().unwrap();
    run_for(&mut h, 0.5);
    h.app.toast(&c, "Undo");
    let next = h.app.ui.toast.clone().unwrap();
    assert_eq!(next.text, "Undo");
    assert!(next.shown_at > first.shown_at);
}

#[test]
fn a_hovered_toast_stays_without_a_repaint_timer_and_resumes_after() {
    let mut h = app();
    let c = ctx(&h);
    h.app.toast(&c, "Library saved");
    let p = toast_centre(&h);
    point_at(&mut h, p);
    run_for(&mut h, 1.0);
    let t = h.app.ui.toast.clone().unwrap();
    assert!(t.hovered);
    assert!(!crate::toast::needs_repaint(t.hovered, h.time(), t.shown_at), "static once faded in: no 30 ms timer");
    point_at(&mut h, egui::pos2(5.0, 5.0));
    let t = h.app.ui.toast.clone().unwrap();
    assert!(!t.hovered);
    assert!(crate::toast::needs_repaint(t.hovered, h.time(), t.shown_at), "the fade-out is animated again");
}

#[test]
fn a_toast_held_through_an_idle_gap_still_gets_its_hold_after_the_pointer_leaves() {
    let mut h = app();
    let c = ctx(&h);
    h.app.toast(&c, "Library saved");
    let p = toast_centre(&h);
    point_at(&mut h, p);
    run_for(&mut h, 0.2);
    // an idle app runs no frames while the pointer rests: `until` was not extended meanwhile
    h.app.ui.toast.as_mut().unwrap().until = h.time() - 0.1;
    assert!(h.app.ui.toast.as_ref().unwrap().hovered);
    point_at(&mut h, egui::pos2(5.0, 5.0));
    let t = h.app.ui.toast.clone().expect("still shown after leaving");
    assert!((t.until - (h.time() + crate::toast::HOVER_HOLD)).abs() < 0.1, "until {} time {}", t.until, h.time());
    run_for(&mut h, 1.9);
    assert!(!shown(&h));
}

#[test]
fn a_hidden_canvas_cannot_pin_a_hovered_toast() {
    let mut h = app();
    let c = ctx(&h);
    h.app.toast(&c, "Library saved");
    let p = toast_centre(&h);
    point_at(&mut h, p);
    assert!(h.app.ui.toast.as_ref().unwrap().hovered);
    let canvas = h.app.canvas_rect.take();
    crate::panels::toast(&mut h.app, &c);
    assert!(!h.app.ui.toast.as_ref().unwrap().hovered);
    h.app.canvas_rect = canvas;
    h.app.ui.toast.as_mut().unwrap().until = h.time() - 0.1;
    h.step();
    assert!(!shown(&h));
}
