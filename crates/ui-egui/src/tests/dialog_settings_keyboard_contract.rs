//! Test-only native/candidate comparison of the actual Settings dialog.
//! Settings applies immediately: its only footer action is Close, not Apply or Cancel.
use std::time::Duration;

use serde_json::{Value, json};

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};

/// A generous settle budget: the point is that the dialog settles, and hosted runners are several times slower
/// than a developer machine.
const T: Duration = Duration::from_secs(90);

fn steps(h: &mut Headless) {
    for _ in 0..4 {
        h.step();
    }
}

fn demo() -> Headless {
    let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default()), [1400.0, 1100.0], 1.0);
    assert!(h.settle(T));
    h
}

fn ask(h: &mut Headless, method: &str, params: Value) {
    let r = h.request(method, params, T);
    assert_eq!(r["ok"], true, "{method}: {r}");
    steps(h);
}

fn show(h: &mut Headless, tab: &str) {
    ask(h, "engine.execute", json!({"command": "app.settings", "params": {"tab": tab}}));
    assert!(h.settle(T));
    steps(h);
    assert_eq!(h.app.ui.dialog, Some(Dialog::Settings { tab: tab.into() }));
}

fn click(h: &mut Headless, id: &str) {
    ask(h, "ui.clickWidget", json!({"id": id}));
}

fn key(h: &mut Headless, name: &str) {
    ask(h, "ui.key", json!({"key": name}));
}

fn replace(h: &mut Headless, field: &str, text: &str) {
    click(h, field);
    ask(h, "ui.key", json!({"key": "A", "cmd": true}));
    ask(h, "ui.text", json!({"text": text}));
    assert!(h.painted_text().iter().any(|s| s.contains(text)), "the draft must really be visible");
}

fn copyright(h: &mut Headless) -> Value {
    h.app.run("library.preferences", json!({})).unwrap()["import"]["copyright"].clone()
}

// Uses egui's actual focused response, not automation hover-probe ids or request_focus.
fn focused(h: &Headless) -> Option<String> {
    let id = h.view.ctx.memory(|m| m.focused())?;
    // SDK controls may publish only their indicator rect while egui senses indicator+label.
    if let Some((name, _)) = h.app.widget_hits.iter().find(|(_, hit)| hit.id == id) {
        return Some(name.clone());
    }
    let rect = h.view.ctx.read_response(id)?.rect;
    h.app.widgets.iter().find(|(_, r)| *r == rect).map(|(w, _)| w.clone())
}

fn clear_focus(h: &mut Headless) {
    if let Some(id) = h.view.ctx.memory(|m| m.focused()) {
        h.view.ctx.memory_mut(|m| m.surrender_focus(id));
    }
    steps(h);
    assert!(h.view.ctx.memory(|m| m.focused()).is_none(), "precondition: no widget focused");
}

#[test]
fn escape_with_no_focus_closes_real_settings() {
    let mut h = demo();
    show(&mut h, "import");
    clear_focus(&mut h);
    key(&mut h, "Escape");
    assert!(h.app.ui.dialog.is_none(), "one unfocused Escape closes Settings");
}

#[test]
fn escape_cancels_a_focused_library_text_draft_before_closing_settings() {
    let mut h = demo();
    show(&mut h, "import");
    let before = copyright(&mut h);
    replace(&mut h, "field:settings.copyright", "Uncommitted Settings Contract");
    assert_eq!(copyright(&mut h), before, "typing is still only a draft");
    key(&mut h, "Escape");
    assert_eq!(copyright(&mut h), before);
    assert!(!h.painted_text().iter().any(|s| s.contains("Uncommitted Settings Contract")));
    assert_eq!(h.app.ui.dialog, Some(Dialog::Settings { tab: "import".into() }));
    key(&mut h, "Escape");
    assert!(h.app.ui.dialog.is_none(), "the second Escape closes the dialog");
    show(&mut h, "import");
    assert_eq!(copyright(&mut h), before, "the cancelled draft did not return on reopening");
}

#[test]
fn return_applies_text_and_close_is_not_a_transaction_cancel() {
    let mut h = demo();
    show(&mut h, "import");
    assert!(!h.app.widgets.iter().any(|(w, _)| w == "button:dialogCancel"), "Settings has Close, not Cancel");
    replace(&mut h, "field:settings.copyright", "Committed Settings Contract");
    key(&mut h, "Enter");
    assert_eq!(copyright(&mut h), json!("Committed Settings Contract"));
    assert_eq!(h.app.ui.dialog, Some(Dialog::Settings { tab: "import".into() }));
    let app_before = h.app.ui.settings.import_dropped_directly;
    let library_before = h.app.session.import_defaults.per_camera;
    click(&mut h, "check:settings.importDropped");
    click(&mut h, "check:settings.perCamera");
    assert_eq!(h.app.ui.settings.import_dropped_directly, !app_before);
    assert_eq!(h.app.session.import_defaults.per_camera, !library_before);
    click(&mut h, "button:dialogOk"); // Its visible label is Close for Settings.
    assert!(h.app.ui.dialog.is_none());
    show(&mut h, "import");
    assert_eq!(copyright(&mut h), json!("Committed Settings Contract"));
    assert_eq!(h.app.ui.settings.import_dropped_directly, !app_before);
    assert_eq!(h.app.session.import_defaults.per_camera, !library_before);
}

#[test]
fn tab_and_shift_tab_reach_real_import_controls_and_the_close_footer() {
    let mut h = demo();
    show(&mut h, "import");
    clear_focus(&mut h);
    let mut order = Vec::new();
    for _ in 0..64 {
        key(&mut h, "Tab");
        let name = focused(&h).unwrap_or_else(|| format!("<unregistered:{:?}>", h.view.ctx.memory(|m| m.focused())));
        order.push(name.clone());
        if name == "button:dialogOk" {
            break;
        }
    }
    eprintln!("SETTINGS_FORWARD_FOCUS={order:?}");
    for required in [
        "check:settings.perCamera",
        "field:settings.copyright",
        "field:settings.creator",
        "check:settings.autoWriteXmp",
        "check:settings.importDropped",
        "check:settings.importOnCard",
        "button:dialogOk",
    ] {
        assert!(order.iter().any(|s| s == required), "Tab must reach {required}: {order:?}");
    }
    assert_eq!(order.last().map(String::as_str), Some("button:dialogOk"));
    assert!(!order.iter().any(|s| s.starts_with('<')), "every focus stop has a real registered control: {order:?}");
    // Stay within the dialog, then walk back over precisely the real stops reached above.
    for expected in order.iter().rev().skip(1) {
        ask(&mut h, "ui.key", json!({"key": "Tab", "shift": true}));
        assert_eq!(focused(&h).as_deref(), Some(expected.as_str()), "Shift-Tab reverses the actual forward order: {order:?}");
    }
    // The field is reached by keys, not by request_focus; Tab commits its draft to the owner.
    for _ in 0..64 {
        if focused(&h).as_deref() == Some("field:settings.copyright") {
            break;
        }
        key(&mut h, "Tab");
    }
    assert_eq!(focused(&h).as_deref(), Some("field:settings.copyright"));
    ask(&mut h, "ui.key", json!({"key": "A", "cmd": true}));
    ask(&mut h, "ui.text", json!({"text": "Keyboard Applied Contract"}));
    key(&mut h, "Tab");
    assert_eq!(focused(&h).as_deref(), Some("field:settings.creator"));
    assert_eq!(copyright(&mut h), json!("Keyboard Applied Contract"));
}

#[test]
fn escape_after_a_keyboard_reached_checkbox_closes_settings() {
    let mut h = demo();
    show(&mut h, "import");
    clear_focus(&mut h);
    for _ in 0..64 {
        key(&mut h, "Tab");
        if focused(&h).as_deref() == Some("check:settings.perCamera") {
            break;
        }
    }
    assert_eq!(focused(&h).as_deref(), Some("check:settings.perCamera"), "the checkbox must actually be reached by Tab");
    let before = h.app.session.import_defaults.per_camera;
    key(&mut h, "Space");
    assert_eq!(h.app.session.import_defaults.per_camera, !before, "Space activates once");
    key(&mut h, "Escape");
    assert!(h.app.ui.dialog.is_none(), "Escape after keyboard activation closes Settings");
    assert_eq!(h.app.session.import_defaults.per_camera, !before, "closing does not undo immediate changes");
}
