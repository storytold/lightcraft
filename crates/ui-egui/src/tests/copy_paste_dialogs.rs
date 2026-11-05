//! Return presses OK in Copy Settings (⇧⌘C) and Paste Selected Settings (⇧⌘V); Esc cancels (#735).

use std::time::Duration;

use lightcraft_catalog::PhotoId;
use serde_json::{Value, json};

use crate::LightcraftApp;
use crate::headless::Headless;
use crate::state::Dialog;

const SETTLE: Duration = Duration::from_secs(120);
const T: Duration = Duration::from_secs(10);

/// The demo library with an edited first photo selected; returns the first two photos' ids.
fn edited() -> (Headless, [u64; 2]) {
    let services = crate::Services { png: None, ..Default::default() };
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
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

/// Copies light and colour from the first photo, selects the second and opens ⇧⌘V.
fn paste_dialog(h: &mut Headless, ids: [u64; 2]) {
    exec(h, "develop.copy", json!({"groups": ["light", "color"]}));
    exec(h, "library.select", json!({"ids": [ids[1]]}));
    key(h, json!({"key": "V", "cmd": true, "shift": true}));
    assert!(matches!(h.app.ui.dialog, Some(Dialog::PasteSettings { .. })), "dialog not open: {:?}", h.app.ui.dialog);
}

fn exposure(h: &Headless, id: u64) -> f64 {
    h.app.session.develop_of(PhotoId(id)).map(|d| d.light.exposure).unwrap_or_default()
}

#[test]
fn return_copies_the_chosen_settings() {
    let (mut h, _) = edited();
    copy_dialog(&mut h);
    assert!(h.app.session.clipboard.is_none());
    key(&mut h, json!({"key": "Enter"}));
    assert!(h.app.ui.dialog.is_none(), "Return closes the dialog");
    assert!(h.app.session.clipboard.is_some(), "Return copies the settings");
}

#[test]
fn escape_closes_copy_settings_without_copying() {
    let (mut h, _) = edited();
    copy_dialog(&mut h);
    key(&mut h, json!({"key": "Escape"}));
    assert!(h.app.ui.dialog.is_none(), "Esc closes the dialog");
    assert!(h.app.session.clipboard.is_none(), "Esc copies nothing");
}

#[test]
fn return_pastes_into_the_selected_photos() {
    let (mut h, ids) = edited();
    paste_dialog(&mut h, ids);
    key(&mut h, json!({"key": "Enter"}));
    assert!(h.app.ui.dialog.is_none(), "Return closes the dialog");
    assert_eq!(exposure(&h, ids[1]), 1.0, "Return pastes the settings");
}

#[test]
fn escape_closes_paste_settings_without_pasting() {
    let (mut h, ids) = edited();
    paste_dialog(&mut h, ids);
    key(&mut h, json!({"key": "Escape"}));
    assert!(h.app.ui.dialog.is_none(), "Esc closes the dialog");
    assert_eq!(exposure(&h, ids[1]), 0.0, "Esc pastes nothing");
}
