//! Modal application dialogs block background input while retaining their own controls.

use crate::headless::Headless;
use crate::{LccApp, Services};
use serde_json::json;
use std::time::Duration;

const T: Duration = Duration::from_secs(20);

fn demo(size: [f32; 2], ui: serde_json::Value) -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LccApp::new(lcc_engine::Session::with_demo(), services);
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
