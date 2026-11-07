//! Edit Capture Time on a photo whose own capture time doesn't fully read (a Lightroom catalog's
//! `captureTime` is copied verbatim): the preview says so instead of asking for a date the user
//! never typed, and OK leaves the photo as it was and says why.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(60);

fn set_captured(h: &mut Headless, id: lightcraft_catalog::PhotoId, t: &str) {
    h.app.session.catalog.apply(lightcraft_catalog::Op::SetCaptured { id, captured: Some(t.into()) }).unwrap();
}

fn painted(h: &Headless, part: &str) -> bool {
    h.painted_text().iter().any(|t| t.contains(part))
}

fn toast(h: &Headless) -> String {
    h.app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default()
}

/// Select `ids` with `active` and open Edit Capture Time.
fn open(h: &mut Headless, ids: &[lightcraft_catalog::PhotoId], active: lightcraft_catalog::PhotoId) {
    let ids: Vec<u64> = ids.iter().map(|id| id.0).collect();
    h.request("engine.execute", json!({"command": "library.select", "params": {"ids": ids, "active": active.0}}), T);
    let r = h.request("ui.menu.invoke", json!({"id": "dialog.captureTime"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
}

#[test]
fn shift_preview_says_the_photos_capture_time_cant_be_read() {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    let id = h.app.session.visible_cloned()[0];
    set_captured(&mut h, id, "2026-01-01T10:00:00");
    open(&mut h, &[id], id);
    match &mut h.app.ui.dialog {
        Some(Dialog::CaptureTime { mode, hours, .. }) => {
            *mode = "shift".into();
            *hours = 1;
        }
        other => panic!("Edit Capture Time is not open: {other:?}"),
    }
    h.settle(SETTLE);
    assert!(painted(&h, "2026-01-01 10:00:00 → 2026-01-01 11:00:00"), "a readable time previews its shift: {:?}", h.painted_text());

    let bad = "2026-01-01T10:00:00 and more";
    set_captured(&mut h, id, bad);
    h.settle(SETTLE);
    assert!(painted(&h, "can't be read"), "the preview says the photo's time can't be read: {:?}", h.painted_text());
    assert!(!painted(&h, "Enter a date"), "no date was typed, so the preview doesn't ask for one: {:?}", h.painted_text());

    let r = h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T);
    assert_eq!(r["ok"], true, "{r}");
    // Inspect the notification before settling advances the simulated clock past its lifetime.
    assert!(h.app.ui.dialog.is_none(), "OK closes the dialog");
    let toast = toast(&h);
    assert!(toast.contains("can't be read"), "OK says the photo was left as it was: {toast:?}");
    assert_eq!(h.app.session.catalog.photo(id).unwrap().captured.as_deref(), Some(bad), "the unreadable time is left as it was");
}

#[test]
fn set_mode_refuses_moving_others_by_an_unreadable_time_and_repairs_it_alone() {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    let ids = h.app.session.visible_cloned();
    let (bad, other) = (ids[0], ids[1]);
    let broken = "2026-01-01T10:00:00 and more";
    set_captured(&mut h, bad, broken);
    set_captured(&mut h, other, "2026-01-01T12:00:00");
    let open_set = |h: &mut Headless, select: &[lightcraft_catalog::PhotoId]| {
        open(h, select, bad);
        match &mut h.app.ui.dialog {
            Some(Dialog::CaptureTime { mode, time, .. }) => {
                *mode = "set".into();
                *time = "2026-01-01 11:00:00".into();
            }
            other => panic!("Edit Capture Time is not open: {other:?}"),
        }
        h.settle(SETTLE);
    };

    // with another photo selected, OK can't move it by the unreadable time's change: it says so
    // and stays open
    open_set(&mut h, &[bad, other]);
    let r = h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T);
    assert_eq!(r["ok"], true, "{r}");
    // The click is consumed by request; unrelated rendering must not expire its notification.
    let toast = toast(&h);
    assert!(toast.contains("can't be read"), "OK says why nothing changed: {toast:?}");
    assert!(matches!(h.app.ui.dialog, Some(Dialog::CaptureTime { .. })), "the dialog stays open after the refusal");
    let captured = |h: &Headless, id| h.app.session.catalog.photo(id).unwrap().captured.clone();
    assert_eq!(captured(&h, bad).as_deref(), Some(broken));
    assert_eq!(captured(&h, other).as_deref(), Some("2026-01-01T12:00:00"), "the other photo doesn't move");
    h.request("ui.dialog.cancel", json!({}), T);

    // alone, the preview shows the time it will get, and OK sets it
    open_set(&mut h, &[bad]);
    let preview = h.painted_text().into_iter().find(|t| t.contains(" → ")).unwrap_or_default();
    assert!(preview.ends_with(" → 2026-01-01 11:00:00"), "the preview shows the time it will get, nothing after: {preview:?}");
    let r = h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(h.app.ui.dialog.is_none(), "OK closes the dialog");
    assert_eq!(captured(&h, bad).as_deref(), Some("2026-01-01T11:00:00"), "alone, it gets the time typed");
}

/// Errors from the command say why and keep the dialog (and what was typed) open, from its OK
/// button and from the control channel alike; they used to close it without a word.
#[test]
fn errors_are_shown_and_the_dialog_stays_open() {
    let files = crate::tests_fixtures::SyntheticLibrary::new("capture-time-errors");
    let app = files.app();
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    let id = h.app.session.visible_cloned()[0];
    set_captured(&mut h, id, "9999-12-31T12:00:00");
    open(&mut h, &[id], id);
    match &mut h.app.ui.dialog {
        Some(Dialog::CaptureTime { time, .. }) => *time = "yesterday".into(),
        other => panic!("Edit Capture Time is not open: {other:?}"),
    }
    assert!(h.settle(SETTLE), "capture-time dialog renders settled");
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T)["ok"], true);
    // The request consumes the click. Inspect its short-lived notification before waiting
    // for unrelated rendering: settling advances the simulated clock and can expire the toast.
    assert!(toast(&h).contains("not a date"), "OK says the typed time isn't a date: {:?}", toast(&h));
    assert!(matches!(&h.app.ui.dialog, Some(Dialog::CaptureTime { time, .. }) if time == "yesterday"), "the dialog stays open with what was typed");

    // a shift past year 9999, confirmed through the control channel
    match &mut h.app.ui.dialog {
        Some(Dialog::CaptureTime { mode, days, .. }) => {
            *mode = "shift".into();
            *days = 1;
        }
        other => panic!("Edit Capture Time is not open: {other:?}"),
    }
    assert!(h.settle(SETTLE), "capture-time dialog renders settled");
    let r = h.request("ui.dialog.confirm", json!({}), T);
    assert_ne!(r["ok"], true, "the shift is refused: {r}");
    assert!(r.to_string().contains("9999"), "{r}");
    assert!(matches!(h.app.ui.dialog, Some(Dialog::CaptureTime { .. })), "ui.dialog.confirm keeps it open too, as the button does");
    assert_eq!(h.app.session.catalog.photo(id).unwrap().captured.as_deref(), Some("9999-12-31T12:00:00"));
}

/// A capture time that doesn't fully read isn't offered as the time to set: `2026-01-01T10:0`
/// would read as midnight. The field starts empty and the preview asks for a date.
#[test]
fn set_mode_does_not_start_from_an_unreadable_time() {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    let id = h.app.session.visible_cloned()[0];
    set_captured(&mut h, id, "2026-01-01T10:00:00");
    open(&mut h, &[id], id);
    assert!(
        matches!(&h.app.ui.dialog, Some(Dialog::CaptureTime { time, .. }) if time == "2026-01-01 10:00:00"),
        "a readable time is offered: {:?}",
        h.app.ui.dialog
    );
    h.request("ui.dialog.cancel", json!({}), T);

    set_captured(&mut h, id, "2026-01-01T10:0");
    open(&mut h, &[id], id);
    assert!(
        matches!(&h.app.ui.dialog, Some(Dialog::CaptureTime { time, .. }) if time.is_empty()),
        "an unreadable time isn't offered: {:?}",
        h.app.ui.dialog
    );
    assert!(painted(&h, "Enter a date"), "alone, the photo will get the time typed, so the preview asks for one: {:?}", h.painted_text());
}
