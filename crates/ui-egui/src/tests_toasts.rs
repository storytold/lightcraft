//! Toasts, the short messages at the bottom of the photo view: one no longer hides another.
//!
//! Scenarios, in the words of someone doing two things in a row:
//!
//! * A label is set, then a long task ends: both messages show, one above the other, the newest at the bottom, where
//!   a toast always was; each goes at its own time.
//! * The same message again (a label set twice) is one toast, there for longer, not two.
//! * A burst of messages shows the last three; a fourth pushes the oldest out.
//! * During a slideshow the toasts sit at the top, clear of the playback controls, the newest at the top.

use std::time::Duration;

use lightcraft_catalog::ColorLabel;

use crate::headless::Headless;
use crate::state::{MAX_TOASTS, UiState};
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(60);

fn demo() -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    h.settle(T);
    h
}

fn texts(ui: &UiState) -> Vec<&str> {
    ui.toasts.iter().map(|t| t.0.as_str()).collect()
}

/// Given a toast showing, when another comes, then both are kept, oldest first, and the newest is the one
/// `toast()` gives; each leaves at its own time.
#[test]
fn a_new_toast_joins_the_ones_showing_instead_of_replacing_them() {
    let mut ui = UiState::default();
    ui.push_toast("Red Label", 10.0, Some(ColorLabel::Red));
    ui.push_toast("Previews ready for 24 photos", 6.0, None);
    assert_eq!(texts(&ui), vec!["Red Label", "Previews ready for 24 photos"]);
    assert_eq!(ui.toast().map(|t| t.0.as_str()), Some("Previews ready for 24 photos"));
    // the newer one is gone first: the older one stays until its own time
    ui.expire_toasts(7.0);
    assert_eq!(texts(&ui), vec!["Red Label"]);
    ui.expire_toasts(10.5);
    assert!(ui.toast().is_none());
}

/// The same message again (same text, same label) is one toast, moved to the newest place, and stays until the
/// later of its two times.
#[test]
fn the_same_message_again_is_one_toast_that_stays_longer() {
    let mut ui = UiState::default();
    ui.push_toast("Red Label", 4.0, Some(ColorLabel::Red));
    ui.push_toast("Library saved", 5.0, None);
    ui.push_toast("Red Label", 8.0, Some(ColorLabel::Red));
    assert_eq!(texts(&ui), vec!["Library saved", "Red Label"]);
    assert_eq!(ui.toast().map(|t| t.1), Some(8.0));
    // a shorter one doesn't cut the time it already had
    ui.push_toast("Red Label", 6.0, Some(ColorLabel::Red));
    assert_eq!(ui.toast().map(|t| t.1), Some(8.0));
    // the same words in another label are another message
    ui.push_toast("Red Label", 8.0, None);
    assert_eq!(ui.toasts.len(), 3);
}

/// A burst keeps the newest few: one more than fit pushes the oldest out.
#[test]
fn a_burst_of_toasts_keeps_the_newest_few() {
    let mut ui = UiState::default();
    for i in 0..MAX_TOASTS + 2 {
        ui.push_toast(format!("message {i}"), 10.0, None);
    }
    assert_eq!(ui.toasts.len(), MAX_TOASTS);
    assert_eq!(ui.toasts.first().map(|t| t.0.clone()), Some("message 2".to_string()));
    assert_eq!(ui.toast().map(|t| t.0.clone()), Some(format!("message {}", MAX_TOASTS + 1)));
}

/// A message that says the state of something takes the place of the one before it on that topic, which no longer
/// holds: "Library saved" replaces the warning that changes weren't written, Reject replaces Pick. Messages on other
/// topics stay.
#[test]
fn a_new_state_takes_the_place_of_the_old_one_on_its_topic() {
    let mut ui = UiState::default();
    ui.push_toast_on("save", "2 changes saved in memory but not written to disk", 6.0, None);
    ui.push_toast("Exported 3 photos", 6.0, None);
    ui.push_toast_on("flag", "Flagged as Pick", 6.0, None);
    ui.push_toast_on("save", "Library saved", 2.0, None);
    ui.push_toast_on("flag", "Flagged as Reject", 6.0, None);
    assert_eq!(texts(&ui), vec!["Exported 3 photos", "Library saved", "Flagged as Reject"]);
}

/// Given two toasts from two commands, then both are on screen at the bottom of the photo view, the newest below,
/// apart and inside the view; in a slideshow they are at the top, the newest on top.
#[test]
fn toasts_stack_on_screen_the_newest_where_a_toast_always_was() {
    let mut h = demo();
    let r = h.app.run("photo.label", serde_json::json!({"label": "red"}));
    assert!(r.is_ok(), "{r:?}");
    let r = h.app.run("library.clearPreviews", serde_json::json!({}));
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(texts(&h.app.ui), vec!["Red Label", "✓ Clear Preview Cache"]);
    h.step();
    h.step();
    let rect = |h: &Headless, id: &str| h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no {id}"));
    let (newest, older) = (rect(&h, "toast:0"), rect(&h, "toast:1"));
    let canvas = h.app.canvas_rect.expect("a photo view");
    assert!(newest.top() >= older.bottom(), "the newest is below: {newest:?} {older:?}");
    assert!(canvas.contains_rect(newest) && canvas.contains_rect(older), "{canvas:?}");
    assert!((newest.bottom() - (canvas.bottom() - 60.0 + newest.height() / 2.0)).abs() < 1.0, "the newest is where a toast always was");
    // a slideshow: at the top, clear of the playback controls, the newest on top
    let r = h.app.run("view.slideshow", serde_json::json!({}));
    assert!(r.is_ok(), "{r:?}");
    h.app.ui.push_toast("Slideshow paused", h.app.last_time + 10.0, None);
    h.step();
    h.step();
    let (top, below) = (rect(&h, "toast:0"), rect(&h, "toast:1"));
    assert!(top.bottom() <= below.top(), "the newest is on top: {top:?} {below:?}");
}
