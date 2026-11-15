//! The quick settings bar: it fades in while the pointer is in the module bar's blank stretch and
//! out when it leaves, dismisses the status line's message on arrival, runs its entries' commands,
//! flashes the eye entry alone when the eye focus strip is hidden from its ✕, and carries plug-ins'
//! quick actions.

use std::time::Duration;

use serde_json::{Value, json};

use crate::headless::Headless;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(60);

fn demo() -> Headless {
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 860.0], 1.0);
    h.settle(T);
    h
}

fn widget(h: &Headless, id: &str) -> Option<egui::Rect> {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r)
}

fn frames(h: &mut Headless, n: usize) {
    for _ in 0..n {
        h.step();
    }
}

/// A point in the blank stretch: past the identity plate, before the module names.
fn blank_spot(h: &Headless) -> egui::Pos2 {
    let bar = widget(h, "region:moduleBar").expect("the module bar");
    let plate = widget(h, "region:identityPlate").map_or(bar.left() + 200.0, |r| r.right());
    let picker = h.app.widgets.iter().filter(|(w, _)| w.starts_with("module:")).map(|(_, r)| r.left()).fold(f32::INFINITY, f32::min);
    assert!(picker.is_finite() && picker > plate + 100.0, "a blank stretch between {plate} and {picker}");
    egui::pos2(picker - 60.0, bar.center().y)
}

fn quick(h: &mut Headless) -> Value {
    let r = h.request("engine.execute", json!({"command": "view.quickSettings"}), T);
    assert_eq!(r["ok"], true, "{r}");
    r["result"].clone()
}

fn move_to(h: &mut Headless, p: egui::Pos2) {
    h.request("ui.move", json!({"x": p.x, "y": p.y}), T);
}

// Scenario: the bar fades in over the blank stretch, right of the plate's room for progress bars,
// and fades out again when the pointer leaves
#[test]
fn the_bar_shows_while_the_pointer_is_in_the_blank_stretch() {
    let mut h = demo();
    frames(&mut h, 2);
    assert!(widget(&h, "quick:info").is_none(), "hidden at first");
    let spot = blank_spot(&h);
    move_to(&mut h, spot);
    frames(&mut h, 20);
    let bar = widget(&h, "region:quickBar").expect("the bar fades in on hover");
    for id in ["info", "overlay", "eyes", "clipping", "focusPoint", "faceBoxes", "filmstrip"] {
        let r = widget(&h, &format!("quick:{id}")).unwrap_or_else(|| panic!("entry {id}"));
        assert!(bar.contains_rect(r), "{id} inside the bar");
    }
    let plate = widget(&h, "region:identityPlate").unwrap();
    let picker = h.app.widgets.iter().filter(|(w, _)| w.starts_with("module:")).map(|(_, r)| r.left()).fold(f32::INFINITY, f32::min);
    assert!(bar.right() < picker && bar.left() > plate.right() + 120.0, "at the right of the stretch: {bar:?}");
    assert_eq!(quick(&mut h)["shown"], true);
    // off the stretch (over the photos)
    move_to(&mut h, egui::pos2(700.0, 450.0));
    frames(&mut h, 30);
    assert!(widget(&h, "quick:info").is_none(), "faded out when the pointer left");
    assert_eq!(quick(&mut h)["shown"], false);
}

// Scenario: a status-line message is dismissed the moment the pointer arrives in the stretch
#[test]
fn hovering_the_stretch_dismisses_the_status_message() {
    let mut h = demo();
    h.app.ui.status = "Something went wrong".into();
    frames(&mut h, 2);
    assert!(widget(&h, "status:message").is_some());
    let spot = blank_spot(&h);
    move_to(&mut h, spot);
    frames(&mut h, 2);
    assert!(h.app.ui.status.is_empty(), "dismissed on hover");
    assert!(widget(&h, "status:message").is_none());
}

// Scenario: a click on an entry runs its command; the state follows
#[test]
fn entries_run_their_commands() {
    let mut h = demo();
    let spot = blank_spot(&h);
    move_to(&mut h, spot);
    frames(&mut h, 20);
    let before = h.app.ui.show_clipping;
    assert_eq!(h.request("ui.clickWidget", json!({"id": "quick:clipping"}), T)["ok"], true);
    frames(&mut h, 3);
    assert_eq!(h.app.ui.show_clipping, !before, "clipping toggled");
    let overlay_before = crate::shortcuts::overlay_reports_changes(&h.app);
    assert_eq!(h.request("ui.clickWidget", json!({"id": "quick:overlay"}), T)["ok"], true);
    frames(&mut h, 3);
    assert_eq!(crate::shortcuts::overlay_reports_changes(&h.app), !overlay_before, "the on-screen overlay toggled");
    let entries = quick(&mut h)["entries"].clone();
    let clipping = entries.as_array().unwrap().iter().find(|e| e["id"] == "clipping").cloned().unwrap();
    assert_eq!(clipping["on"], !before);
    assert!(widget(&h, "region:quickBar").is_some(), "still shown while the pointer stays");
}

// Scenario: hiding the eye focus strip from its ✕ turns it off and flashes only the eye entry of the
// otherwise hidden bar, a couple of times, then nothing
#[test]
fn dismissing_the_eye_strip_flashes_its_entry_alone() {
    let mut h = demo();
    h.app.ui.eye_focus = true;
    move_to(&mut h, egui::pos2(700.0, 450.0));
    frames(&mut h, 2);
    let r = h.request("engine.execute", json!({"command": "view.dismissEyeFocus"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(!h.app.ui.eye_focus && !h.app.eye_focus_on(), "the strip is off");
    frames(&mut h, 12);
    assert!(widget(&h, "quick:eyes").is_some(), "the eye entry blinks");
    assert!(widget(&h, "quick:info").is_none() && widget(&h, "region:quickBar").is_none(), "the rest of the bar stays hidden");
    assert_eq!(quick(&mut h)["flashing"], "eyes");
    frames(&mut h, (crate::quick_bar::FLASH_SECS * 60.0) as usize + 10);
    assert!(widget(&h, "quick:eyes").is_none(), "and stops");
    assert_eq!(quick(&mut h)["flashing"], Value::Null);
    // the bar's eye entry turns it back on
    let spot = blank_spot(&h);
    move_to(&mut h, spot);
    frames(&mut h, 20);
    assert_eq!(h.request("ui.clickWidget", json!({"id": "quick:eyes"}), T)["ok"], true);
    frames(&mut h, 2);
    assert!(h.app.eye_focus_on());
}

#[test]
fn a_bad_flash_or_eye_request_is_an_error() {
    let mut h = demo();
    let r = h.request("engine.execute", json!({"command": "view.quickSettings", "params": {"flash": "nope"}}), T);
    assert_eq!(r["ok"], false, "{r}");
    let r = h.request("engine.execute", json!({"command": "view.zoomToEye", "params": {"index": -1}}), T);
    assert_eq!(r["ok"], false, "{r}");
    let r = h.request("engine.execute", json!({"command": "view.zoomToEye"}), T);
    assert_eq!(r["ok"], false, "no eyes found yet: {r}");
    let r = h.request("engine.execute", json!({"command": "view.changeOverlay", "params": {"on": "yes"}}), T);
    assert_eq!(r["ok"], false, "{r}");
}

/// A plug-in `id` whose command `go` ("Go Quick") is a quick action and replies `{message: "ran"}`.
fn quick_module(id: &str) -> Vec<u8> {
    let manifest = format!(
        r#"{{"id":"{id}","name":"Quick Test","version":"1.0.0","requires":["quickActions"],"commands":[{{"id":"go","label":"Go Quick","quickAction":true}}]}}"#
    );
    let reply = r#"{"ok":{"message":"ran"}}"#;
    let esc = |s: &str| s.replace('"', "\\\"");
    let src = format!(
        r#"(module (memory (export "memory") 2)
  (data (i32.const 16) "{m}") (data (i32.const 4096) "{r}")
  (func (export "lcc_abi_version") (result i32) (i32.const 1))
  (func (export "lcc_manifest") (result i64) (i64.or (i64.shl (i64.const {ml}) (i64.const 32)) (i64.const 16)))
  (func (export "lcc_alloc") (param i32) (result i32) (i32.const 65536))
  (func (export "lcc_handle") (param i32 i32) (result i64) (i64.or (i64.shl (i64.const {rl}) (i64.const 32)) (i64.const 4096))))"#,
        m = esc(&manifest),
        r = esc(reply),
        ml = manifest.len(),
        rl = reply.len()
    );
    wat::parse_str(&src).unwrap()
}

// Scenario: a plug-in's quick action is a button in the bar, and a click runs its command
#[test]
fn a_plugin_adds_a_button_that_runs_its_command() {
    lcc_engine::cmd::plugins::use_in_memory_manager();
    lcc_engine::cmd::plugins::manager().as_mut().unwrap().install_bytes(&quick_module("test.quickbar"), None, false).unwrap();
    let mut h = demo();
    let entries = quick(&mut h)["entries"].clone();
    let entry = entries.as_array().unwrap().iter().find(|e| e["id"] == "plugin:test.quickbar/go").cloned().expect("listed");
    assert_eq!(entry["label"], "Go Quick");
    let spot = blank_spot(&h);
    move_to(&mut h, spot);
    frames(&mut h, 20);
    let id = "quick:plugin:test.quickbar/go";
    assert!(widget(&h, id).is_some(), "the plug-in's button is in the bar");
    assert_eq!(h.request("ui.clickWidget", json!({"id": id}), T)["ok"], true);
    let ran = h.step_until(Duration::from_secs(20), |h| h.app.ui.toasts.iter().any(|t| t.0 == "ran"));
    assert!(ran, "the command ran and its message shows: {:?}", h.app.ui.toasts);
}
