//! Issue #781: a dialog taller than the window keeps its title and buttons on screen; its options
//! scroll. On a 1366×768 screen the Export dialog ran off both ends and couldn't be confirmed.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn demo(size: [f32; 2]) -> Headless {
    let services = Services { png: None, write_shared: Some(std::sync::Arc::new(|_: &str, _: &[u8]| Ok(()))), ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, size, 1.0);
    // let the first renders finish (a render still running at exit can crash in GPU teardown)
    h.settle(T);
    h
}

fn rect(h: &Headless, id: &str) -> egui::Rect {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no widget {id}"))
}

fn open(h: &mut Headless, command: &str) {
    let r = h.request("engine.execute", json!({"command": command, "params": {}}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
    assert!(h.app.ui.dialog.is_some(), "{command} opens a dialog");
    h.settle(T);
}

/// The dialog lies inside the window, and so do its buttons (`button:dialogCancel` when it has one).
fn assert_fits(h: &Headless, what: &str) {
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, h.size);
    let dialog = rect(h, "dialog:window");
    assert!(screen.contains_rect(dialog), "{what}: dialog {dialog:?} runs off the {screen:?} window");
    let mut buttons = vec!["button:dialogOk"];
    if h.app.widgets.iter().any(|(w, _)| w == "button:dialogCancel") {
        buttons.push("button:dialogCancel");
    }
    for b in buttons {
        let r = rect(h, b);
        assert!(dialog.contains_rect(r), "{what}: {b} {r:?} is outside the dialog {dialog:?}");
    }
}

/// The Export dialog in a window of `size`, set to write small JPEGs to a made-up folder.
fn export_dialog(size: [f32; 2]) -> Headless {
    let mut h = demo(size);
    open(&mut h, "dialog.export");
    if let Some(crate::state::Dialog::Export { full_size, resize, dir, .. }) = &mut h.app.ui.dialog {
        *full_size = false;
        *resize = lightcraft_engine::export::Resize::long_edge(64);
        *dir = "/lc-test-out".into();
    }
    h.settle(T);
    h
}

// Scenario: on a 1366×768 screen (and a shorter window) the Export dialog's buttons are on screen and work
#[test]
fn export_dialog_fits_a_short_window_and_its_button_exports() {
    for size in [[1366.0, 690.0], [1366.0, 520.0]] {
        let mut h = export_dialog(size);
        assert_fits(&h, &format!("Export at {size:?}"));
        let r = h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        h.step();
        assert!(h.app.ui.dialog.is_none(), "Export closed the dialog at {size:?}");
        assert!(h.app.export.is_some() || h.app.last_export_result.is_some(), "Export started at {size:?}");
        assert!(h.step_until(Duration::from_secs(60), |h| h.app.export.is_none()));
    }
}

// Scenario: Cancel is on screen too, and closes the dialog
#[test]
fn export_dialog_cancel_is_on_screen_in_a_short_window() {
    let mut h = export_dialog([1366.0, 690.0]);
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, h.size);
    assert!(screen.contains_rect(rect(&h, "button:dialogCancel")));
    h.request("ui.clickWidget", json!({"id": "button:dialogCancel"}), T);
    h.step();
    h.step();
    assert!(h.app.ui.dialog.is_none());
    assert!(h.app.export.is_none() && h.app.last_export_result.is_none(), "nothing exported");
}

// Scenario: a window with room for it shows the Export dialog as before (no scrolling, same size)
#[test]
fn export_dialog_is_unchanged_in_a_tall_window() {
    let h = export_dialog([1600.0, 1000.0]);
    assert_fits(&h, "Export at 1600×1000");
    let dialog = rect(&h, "dialog:window");
    // (846 × 500.6 before #781: the whole dialog, nothing scrolled away)
    assert!((dialog.height() - 846.0).abs() <= 1.0, "{dialog:?}");
    assert!((dialog.width() - 500.6).abs() <= 1.0, "{dialog:?}");
}

// Scenario: other long dialogs fit a short window too, with their buttons on screen
#[test]
fn long_dialogs_fit_a_short_window() {
    let mut h = demo([1000.0, 320.0]);
    for command in [
        "dialog.copySettings",
        "dialog.pasteSettings",
        "dialog.createPreset",
        "dialog.contactSheet",
        "app.settings",
        "app.shortcuts",
        "app.whatsNew",
        "app.about",
        "app.systemInfo",
        "dialog.smartAlbum",
    ] {
        open(&mut h, command);
        assert_fits(&h, command);
        h.app.ui.dialog = None;
        h.settle(T);
    }
}

// Scenario: a list that scrolls on its own (the shortcuts) gets shorter instead of scrolling inside a scrolling dialog
#[test]
fn a_dialog_list_shrinks_to_fit_a_short_window() {
    let mut h = demo([1000.0, 320.0]);
    open(&mut h, "app.shortcuts");
    let shrink = h.view.ctx.data(|d| d.get_temp::<f32>(egui::Id::new("dialog-list-shrink"))).unwrap_or(0.0);
    assert!(shrink > 0.0, "the list gave up room");
    let dialog = rect(&h, "dialog:window");
    assert!(dialog.height() <= 320.0, "{dialog:?}");
}
