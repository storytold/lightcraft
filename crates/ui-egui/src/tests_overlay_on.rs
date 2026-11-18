//! Ported from OneSlips 0c58de7ad (MASK-TOOLS-OVERLAY, the parts engine PR 1 left out): in the Compact layout the
//! mask overlay has a switch with its own icon, lit while the overlay is on to stay, and the photo says "Overlay on ·
//! O" then; picking how the overlay looks never switches it on for good.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);

fn masking(compact: bool) -> Headless {
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 1000.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail", "moduleBar": false}), T);
    assert_eq!(r["ok"], true, "{r}");
    exec(&mut h, "view.compactLayout", json!({"on": compact}));
    exec(&mut h, "panel.masking", json!({}));
    exec(&mut h, "mask.add", json!({"kind": "radial", "center": [0.5, 0.5], "rx": 0.2, "ry": 0.2}));
    // (a new mask shows the overlay; each test starts with it off)
    exec(&mut h, "view.maskOverlay", json!({"show": false}));
    h.step();
    h.step();
    h
}

fn exec(h: &mut Headless, command: &str, params: serde_json::Value) -> serde_json::Value {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
    r["result"].clone()
}

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

/// Compact: the overlay switched on with O is named on the photo; while a slider adjusts the note steps aside with
/// the overlay, and comes back after; the switch is an icon button of its own, and a click turns the overlay on to
/// stay and another off. Classic draws no such note and keeps its checkbox. Choosing how the overlay looks (the
/// overlay menu) leaves it as it was, which is how it got stuck on; ⇧O, cycling the look from the keyboard, still
/// shows it.
#[test]
fn the_compact_overlay_switch_and_note_and_the_look_menu_never_latching_it_on() {
    let mut h = masking(true);
    assert!(!has(&h, "canvas:overlayOn"), "the overlay isn't on to stay yet");
    exec(&mut h, "view.maskOverlay", json!({"show": true}));
    h.step();
    assert!(has(&h, "canvas:overlayOn"), "the canvas says the overlay is on (O)");
    h.app.ui.dragging_control = Some("light.exposure".into());
    h.step();
    assert!(!has(&h, "canvas:overlayOn"), "no note while a slider adjusts");
    h.app.ui.dragging_control = None;
    h.step();
    assert!(has(&h, "canvas:overlayOn"), "back once the slider is let go");

    // the switch: a button of its own
    exec(&mut h, "view.maskOverlay", json!({"show": false}));
    h.step();
    h.step();
    assert!(has(&h, "icon:maskOverlayOn"), "the overlay's own button");
    assert!(!h.app.ui.mask_overlay);
    let r = h.request("ui.clickWidget", json!({"id": "icon:maskOverlayOn"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(h.app.ui.mask_overlay, "on to stay");
    let r = h.request("ui.clickWidget", json!({"id": "check:maskOverlay"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(!h.app.ui.mask_overlay, "the same switch turns it off");

    let mut classic = masking(false);
    assert!(!has(&classic, "icon:maskOverlayOn") && has(&classic, "check:maskOverlay"), "Classic keeps its checkbox");
    // the look menu does not latch the overlay on
    assert!(!classic.app.ui.mask_overlay);
    exec(&mut classic, "view.maskOverlayMode", json!({"mode": "whiteOnBlack", "show": false}));
    assert!(!classic.app.ui.mask_overlay, "picking a look leaves the overlay as it was");
    exec(&mut classic, "view.maskOverlayMode", json!({}));
    assert!(classic.app.ui.mask_overlay, "⇧O cycles the look and shows it");
    classic.step();
    assert!(!has(&classic, "canvas:overlayOn"), "Classic draws no note on the photo");
}
