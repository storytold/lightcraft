//! Return presses OK in Copy Settings (⇧⌘C) and Paste Selected Settings (⇧⌘V); Esc cancels (#735).

use std::time::Duration;

use lcc_catalog::PhotoId;
use serde_json::{Value, json};

use crate::LccApp;
use crate::headless::Headless;
use crate::state::Dialog;

const SETTLE: Duration = Duration::from_secs(120);
const T: Duration = Duration::from_secs(10);

/// The demo library with an edited first photo selected; returns the first two photos' ids.
fn edited() -> (Headless, [u64; 2]) {
    let services = crate::Services { png: None, ..Default::default() };
    let mut app = LccApp::new(lcc_engine::Session::with_demo(), services);
    app.ui.view = crate::state::ViewMode::PhotoGrid;
    let mut h = Headless::new(app, [1200.0, 900.0], 1.0);
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(2).map(|p| p.0).collect();
    let ids = [ids[0], ids[1]];
    exec(&mut h, "library.select", json!({"ids": [ids[0]]}));
    exec(&mut h, "develop.set", json!({"values": {"light.exposure": 1.0, "color.vibrance": 30}}));
    (h, ids)
}

fn exec(h: &mut Headless, command: &str, params: Value) {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
}

fn key(h: &mut Headless, k: Value) {
    let r = h.request("ui.key", k.clone(), T);
    assert_eq!(r["ok"], true, "{k}: {r}");
    h.settle(SETTLE);
}

fn copy_dialog(h: &mut Headless) {
    key(h, json!({"key": "C", "cmd": true, "shift": true}));
    assert!(matches!(h.app.ui.dialog, Some(Dialog::CopySettings { .. })), "dialog not open: {:?}", h.app.ui.dialog);
}

/// Copies light and colour from the first photo, selects the second and opens Paste Selected
/// Settings… (⇧⌘V pastes at once in the Classic set, as in Lightroom Classic).
fn paste_dialog(h: &mut Headless, ids: [u64; 2]) {
    exec(h, "develop.copy", json!({"groups": ["light", "color"]}));
    exec(h, "library.select", json!({"ids": [ids[1]]}));
    exec(h, "dialog.pasteSettings", json!({}));
    h.settle(SETTLE);
    assert!(matches!(h.app.ui.dialog, Some(Dialog::PasteSettings { .. })), "dialog not open: {:?}", h.app.ui.dialog);
}

fn exposure(h: &Headless, id: u64) -> f64 {
    h.app.session.develop_of(PhotoId(id)).map(|d| d.light.exposure).unwrap_or_default()
}

/// Copy Settings and Paste Settings: Esc closes the dialog and does nothing, Return closes it and does the work.
#[test]
fn return_confirms_and_escape_cancels_copy_and_paste_settings() {
    let (mut h, ids) = edited();
    copy_dialog(&mut h);
    key(&mut h, json!({"key": "Escape"}));
    assert!(h.app.ui.dialog.is_none(), "Esc closes Copy Settings");
    assert!(h.app.session.clipboard.is_none(), "Esc copies nothing");
    copy_dialog(&mut h);
    key(&mut h, json!({"key": "Enter"}));
    assert!(h.app.ui.dialog.is_none(), "Return closes Copy Settings");
    assert!(h.app.session.clipboard.is_some(), "Return copies the settings");

    paste_dialog(&mut h, ids);
    key(&mut h, json!({"key": "Escape"}));
    assert!(h.app.ui.dialog.is_none(), "Esc closes Paste Settings");
    assert_eq!(exposure(&h, ids[1]), 0.0, "Esc pastes nothing");
    exec(&mut h, "library.select", json!({"ids": [ids[0]]}));
    paste_dialog(&mut h, ids);
    key(&mut h, json!({"key": "Enter"}));
    assert!(h.app.ui.dialog.is_none(), "Return closes Paste Settings");
    assert_eq!(exposure(&h, ids[1]), 1.0, "Return pastes the settings");
}

fn click(h: &mut Headless, id: &str) {
    h.settle(SETTLE);
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    h.settle(SETTLE);
}

/// Develop's Copy… / Paste buttons under the left column (issue #118) copy through the Copy
/// Settings dialog and paste into the selected photo; Library's Export… opens the Export module.
#[test]
fn the_left_bar_copies_pastes_and_opens_export() {
    let (mut h, ids) = edited();
    h.app.run("module.develop", json!({})).unwrap();
    h.app.run("panel.left", json!({"show": true})).unwrap();
    click(&mut h, "button:leftBar-copy");
    assert!(matches!(h.app.ui.dialog, Some(Dialog::CopySettings { .. })), "Copy… opens Copy Settings: {:?}", h.app.ui.dialog);
    key(&mut h, json!({"key": "Enter"}));
    assert!(h.app.session.clipboard.is_some(), "the dialog copies");
    exec(&mut h, "library.select", json!({"ids": [ids[1]]}));
    click(&mut h, "button:leftBar-paste");
    assert_eq!(exposure(&h, ids[1]), 1.0, "Paste pastes the copied settings");
    // Library's Import… / Export… buttons under the left column (issue #118)
    h.app.run("module.library", json!({})).unwrap();
    h.app.run("panel.left", json!({"show": true})).unwrap();
    click(&mut h, "button:leftBar-export");
    assert_eq!(h.app.ui.module, crate::module::ModuleId::Export, "Export… opens the Export module");
    assert!(h.app.ui.dialog.is_none(), "not the dialog: {:?}", h.app.ui.dialog);
}
