//! Modal application dialogs block background input while retaining their own controls.

use crate::headless::Headless;
use crate::{LightcraftApp, Services};
use serde_json::json;
use std::time::Duration;

const T: Duration = Duration::from_secs(20);

fn demo(size: [f32; 2], ui: serde_json::Value) -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, size, 1.0);
    let r = h.request("ui.set", ui, T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(Duration::from_secs(120));
    h
}

/// Application dialogs block the toolbar, including the gear that would reopen Copy Settings
/// and discard its choices. Their own controls work, and closing restores background input.
#[test]
fn modal_dialogs_block_background_clicks_and_preserve_choices() {
    let mut h = demo([1400.0, 1000.0], json!({"view": "detail", "right": "edit"}));
    let active = h.app.session.active().unwrap();
    let rating = h.app.session.catalog.photo(active).unwrap().rating;
    h.app.run("dialog.copySettings", json!({})).unwrap();
    // Also exercise the first frame of a newly opened dialog.
    let r = h.request("ui.clickWidget", json!({"id": "star:bottom:5"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(h.app.session.catalog.photo(active).unwrap().rating, rating, "the dialog blocks background ratings");
    let r = h.request("ui.clickWidget", json!({"id": "copyGroup:none"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    let choices = h.app.ui.dialog.clone();
    assert!(matches!(&choices, Some(crate::state::Dialog::CopySettings { groups }) if groups.is_empty()));
    let r = h.request("ui.clickWidget", json!({"id": "icon:copyGear"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(h.app.ui.dialog, choices, "a background click cannot reopen the dialog and reset its choices");
    let r = h.request("ui.clickWidget", json!({"id": "button:dialogCancel"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(h.app.ui.dialog.is_none());
    let r = h.request("ui.clickWidget", json!({"id": "star:bottom:5"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(h.app.session.catalog.photo(active).unwrap().rating, 5, "closing restores toolbar input");
    // A different dialog uses the same barrier.
    h.app.run("app.settings", json!({})).unwrap();
    h.step();
    let r = h.request("ui.clickWidget", json!({"id": "star:bottom:1"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(h.app.session.catalog.photo(active).unwrap().rating, 5);
    let r = h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(h.app.ui.dialog.is_none());
}

#[test]
fn modal_dialogs_block_background_shortcuts_and_keep_escape() {
    let mut h = demo([1400.0, 1000.0], json!({"view": "detail", "right": "edit"}));
    let active = h.app.session.active().unwrap();
    let rating = h.app.session.catalog.photo(active).unwrap().rating;
    h.app.run("dialog.copySettings", json!({})).unwrap();
    h.step();
    for key in ["5", "ArrowRight", "J"] {
        let r = h.request("ui.key", json!({"key": key}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
    }
    assert_eq!(h.app.session.catalog.photo(active).unwrap().rating, rating);
    assert_eq!(h.app.session.active(), Some(active));
    assert!(!h.app.ui.show_clipping);
    let r = h.request("ui.key", json!({"key": "Escape"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(h.app.ui.dialog.is_none());
    let r = h.request("ui.key", json!({"key": "5"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(h.app.session.catalog.photo(active).unwrap().rating, 5, "closing restores shortcuts");
}

#[test]
fn modal_dialogs_close_background_menus() {
    let mut h = demo([1400.0, 1000.0], json!({"view": "detail", "right": "edit"}));
    let r = h.request("ui.clickWidget", json!({"id": "menu:Photo"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(egui::Popup::is_any_open(&h.view.ctx));
    h.app.run("dialog.copySettings", json!({})).unwrap();
    h.step();
    assert!(!egui::Popup::is_any_open(&h.view.ctx), "background menus cannot remain above the modal window");
}

fn capture_dialog(h: &mut Headless, mode: &str, time: &str) {
    h.app.ui.dialog = Some(crate::state::Dialog::CaptureTime { mode: mode.into(), time: time.into(), days: 0, hours: 0, minutes: 0, zone: 0.0 });
    h.step();
}

/// The Edit Capture Time preview says why nothing would move: a photo whose capture time can't be
/// read is named as such (not "Enter a date"), Set mode alone shows the time the photo will get,
/// and a shift past the years 0000–9999 says so (issue #2505).
#[test]
fn capture_time_preview_says_why_nothing_moves() {
    use crate::panels::dialogs::capture_time_preview;
    assert_eq!(capture_time_preview("shift", 2, "", Some(3600), "a.jpg", "2026-01-01T10:00:00"), "a.jpg: 2026-01-01 10:00:00 → 2026-01-01 11:00:00");
    assert_eq!(capture_time_preview("shift", 2, "", Some(3600), "a.jpg", "2026-01-01T10:0"), "a.jpg: the capture time can't be read");
    assert_eq!(capture_time_preview("shift", 2, "", Some(3600), "a.jpg", "2026-01-01T10:00:00 and more"), "a.jpg: the capture time can't be read");
    assert_eq!(capture_time_preview("set", 2, "", None, "a.jpg", "2026-01-01T10:00:00"), "Enter a date as YYYY-MM-DD HH:MM:SS");
    assert_eq!(
        capture_time_preview("set", 1, "2026-03-03 12:00:00", None, "a.jpg", "2026-01-01T10:0"),
        "a.jpg: 2026-01-01 10:0 → 2026-03-03 12:00:00"
    );
    assert_eq!(
        capture_time_preview("shift", 1, "", Some(86_400 * 365 * 9000), "a.jpg", "9999-12-31T23:00:00"),
        "The shift would go outside the years 0000–9999"
    );
}

/// Set mode doesn't prefill with a capture time it can't read: the field starts empty (issue #2505).
#[test]
fn capture_time_dialog_prefills_only_a_readable_time() {
    let mut h = demo([1400.0, 1000.0], json!({"view": "detail", "right": "edit"}));
    let active = h.app.session.active().unwrap();
    h.app.run("photo.setCaptureTime", json!({"time": "2020-01-02 03:04:05"})).unwrap();
    h.app.run("dialog.captureTime", json!({})).unwrap();
    h.step();
    let Some(crate::state::Dialog::CaptureTime { time, .. }) = h.app.ui.dialog.clone() else { panic!("no dialog") };
    assert_eq!(time, "2020-01-02 03:04:05");
    h.app.ui.dialog = None;
    h.app.session.catalog.apply(lightcraft_catalog::Op::SetCaptured { id: active, captured: Some("2026-01-01T10:0".into()) }).unwrap();
    h.app.run("dialog.captureTime", json!({})).unwrap();
    h.step();
    let Some(crate::state::Dialog::CaptureTime { time, .. }) = h.app.ui.dialog.clone() else { panic!("no dialog") };
    assert!(time.is_empty(), "an unreadable time would prefill a misread (`10:0` reads as midnight)");
}

/// Photos whose capture time can't be read are left as they were and the dialog says so; errors
/// keep the dialog open, from the OK button and the control channel alike (issue #2505).
#[test]
fn capture_time_leaves_unreadable_photos_and_stays_open_on_errors() {
    let mut h = demo([1400.0, 1000.0], json!({"view": "detail", "right": "edit"}));
    let active = h.app.session.active().unwrap();
    let peer = h.app.session.catalog.photos().map(|p| p.id).find(|id| *id != active).unwrap();
    h.app.session.catalog.apply(lightcraft_catalog::Op::SetCaptured { id: active, captured: Some("2026-01-01T10:00:00 and more".into()) }).unwrap();
    let peer_before = h.app.session.catalog.photo(peer).unwrap().captured.clone();
    h.app.run("library.select", json!({"ids": [active.0, peer.0], "active": peer.0})).unwrap();
    // shift both by an hour: the readable one moves, the unreadable one is left and counted
    h.app.ui.dialog = Some(crate::state::Dialog::CaptureTime { mode: "shift".into(), time: String::new(), days: 0, hours: 1, minutes: 0, zone: 0.0 });
    // the dialog's window settles its size over the first frames; click after it has
    h.settle(T);
    let r = h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(T);
    let toast = h.app.ui.toast.clone().map(|t| t.0).unwrap_or_default();
    assert!(h.app.ui.dialog.is_none(), "OK with unreadable photos closes the dialog (toast: {toast})");
    let toast = h.app.ui.toast.clone().expect("toast").0;
    assert!(toast.contains("left as they were"), "{toast}");
    assert_eq!(
        h.app.session.catalog.photo(active).unwrap().captured.as_deref(),
        Some("2026-01-01T10:00:00 and more"),
        "the unreadable one is untouched"
    );
    let peer_after = h.app.session.catalog.photo(peer).unwrap().captured.clone().unwrap();
    let secs = |t: &str| lightcraft_catalog::dates::iso_seconds(t).unwrap();
    assert_eq!(secs(&peer_after) - secs(peer_before.as_deref().unwrap()), 3600, "the readable one moved by the shift");
    // a typed time that isn't a date: the error shows and the dialog stays open
    capture_dialog(&mut h, "set", "banana");
    let r = h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    let toast = h.app.ui.toast.clone().expect("toast").0;
    assert!(toast.contains("is not a date"), "{toast}");
    assert!(h.app.ui.dialog.is_some(), "an error keeps the dialog open");
    // the same through the control channel
    let r = h.request("ui.dialog.confirm", json!({}), T);
    assert_eq!(r["ok"], false, "{r}");
    assert!(r["error"].as_str().is_some_and(|e| e.contains("is not a date")), "{r}");
    assert!(h.app.ui.dialog.is_some(), "the control channel keeps the dialog open too");
    // several photos and an unreadable active one: Set mode refuses and stays open
    h.app.session.catalog.apply(lightcraft_catalog::Op::SetCaptured { id: peer, captured: Some("2026-03-01T12:00:00".into()) }).unwrap();
    h.app.run("library.select", json!({"ids": [active.0, peer.0], "active": active.0})).unwrap();
    capture_dialog(&mut h, "set", "2026-03-03 12:00:00");
    let r = h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    let toast = h.app.ui.toast.clone().expect("toast").0;
    assert!(toast.contains("can't be read"), "{toast}");
    assert!(h.app.ui.dialog.is_some(), "the refusal keeps the dialog open");
    assert_eq!(h.app.session.catalog.photo(peer).unwrap().captured.as_deref(), Some("2026-03-01T12:00:00"), "nothing moved");
    // alone, the same photo gets the typed time as it is (repair)
    h.app.run("library.select", json!({"ids": [active.0], "active": active.0})).unwrap();
    capture_dialog(&mut h, "set", "2026-03-03 12:00:00");
    let r = h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(h.app.ui.dialog.is_none());
    assert_eq!(h.app.session.catalog.photo(active).unwrap().captured.as_deref(), Some("2026-03-03T12:00:00"));
}
