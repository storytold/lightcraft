//! Headless tests of the Tool Overlay (issue #224): when the Remove tool's spot pins and the mask
//! pins show (Auto / Always / Selected / Never), Shift+H and `view.toolOverlay` to change it.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::menubar::MenuNode;
use crate::state::{RightPanel, ToolOverlay};
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn detail(panel: &str) -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("engine.execute", json!({"command": panel}), T);
    assert_eq!(r["ok"], true, "{r}");
    h
}

fn exec(h: &mut Headless, command: &str, params: serde_json::Value) -> serde_json::Value {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
    r["result"].clone()
}

/// The Remove tool with three spots (the last one selected).
fn three_spots() -> Headless {
    let mut h = detail("panel.remove");
    assert_eq!(h.app.ui.right, RightPanel::Remove);
    for (x, y) in [(0.25, 0.3), (0.5, 0.6), (0.75, 0.3)] {
        exec(&mut h, "spot.add", json!({"mode": "heal", "points": [[x, y]], "size": 0.03}));
    }
    let id = h.app.session.active().expect("active photo");
    assert_eq!(h.app.session.develop_of(id).unwrap_or_default().spots.len(), 3);
    assert_eq!(h.app.session.active_spot, Some(2));
    h
}

/// Pointer over the photo (normalized image coordinates), or away from it (the top bar).
fn hover(h: &mut Headless, over: bool) {
    let r = if over {
        h.request("ui.pointer", json!({"events": [{"kind": "move", "x": 0.1, "y": 0.9}]}), T)
    } else {
        h.request("ui.move", json!({"x": 4.0, "y": 4.0}), T)
    };
    assert_eq!(r["ok"], true, "{r}");
    h.step();
}

/// The ids of the pins drawn this frame with `prefix` (`spotPin:`, `maskPin:`), sorted.
fn pins(h: &Headless, prefix: &str) -> Vec<String> {
    let mut v: Vec<String> = h.app.widgets.iter().filter(|(id, _)| id.starts_with(prefix)).map(|(id, _)| id.clone()).collect();
    v.sort();
    v.dedup();
    v
}

fn spot_pins(h: &Headless) -> Vec<String> {
    pins(h, "spotPin:")
}

fn all_three() -> Vec<String> {
    vec!["spotPin:0".to_string(), "spotPin:1".to_string(), "spotPin:2".to_string()]
}

#[test]
fn spot_pins_follow_each_tool_overlay_mode() {
    let mut h = three_spots();
    // Auto (the default): only while the pointer is over the photo
    assert_eq!(h.app.ui.tool_overlay, ToolOverlay::Auto);
    hover(&mut h, false);
    assert_eq!(spot_pins(&h), Vec::<String>::new(), "Auto: hidden with the pointer away");
    assert!(pins(&h, "spotSource:").is_empty());
    hover(&mut h, true);
    assert_eq!(spot_pins(&h), all_three(), "Auto: shown with the pointer over the photo");
    assert_eq!(pins(&h, "spotSource:"), vec!["spotSource:2".to_string()], "the selected spot's source");
    // Always: whatever the pointer does
    exec(&mut h, "view.toolOverlay", json!({"mode": "always"}));
    hover(&mut h, false);
    assert_eq!(spot_pins(&h), all_three(), "Always: shown with the pointer away");
    // Selected: the selected spot only
    exec(&mut h, "view.toolOverlay", json!({"mode": "selected"}));
    hover(&mut h, true);
    assert_eq!(spot_pins(&h), vec!["spotPin:2".to_string()]);
    exec(&mut h, "spot.select", json!({"index": 0}));
    h.step();
    assert_eq!(spot_pins(&h), vec!["spotPin:0".to_string()]);
    // Never: none, even over the photo
    exec(&mut h, "view.toolOverlay", json!({"mode": "never"}));
    hover(&mut h, true);
    assert_eq!(spot_pins(&h), Vec::<String>::new());
    assert!(pins(&h, "spotSource:").is_empty());
}

/// Hidden pins can't be grabbed: with Never a click on a spot paints a new spot (the tool still
/// works); with Selected, clicking a hidden spot does too, while the selected one can be dragged.
#[test]
fn hidden_spot_pins_are_not_grabbed_and_the_tool_still_works() {
    let mut h = three_spots();
    let spots = |h: &Headless| h.app.session.develop_of(h.app.session.active().unwrap()).unwrap_or_default().spots.clone();
    exec(&mut h, "view.toolOverlay", json!({"mode": "never"}));
    let r = h.request("ui.pointer", json!({"events": [{"kind": "down", "x": 0.25, "y": 0.3}, {"kind": "up", "x": 0.25, "y": 0.3}]}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(spots(&h).len(), 4, "Never: a click adds a spot");
    assert_eq!(h.app.session.active_spot, Some(3));
    exec(&mut h, "edit.undo", json!({}));
    assert_eq!(spots(&h).len(), 3);
    // Selected: the selected spot's pin still drags it
    exec(&mut h, "view.toolOverlay", json!({"mode": "selected"}));
    exec(&mut h, "spot.select", json!({"index": 1}));
    let r = h.request(
        "ui.pointer",
        json!({"events": [{"kind": "down", "x": 0.5, "y": 0.6}, {"kind": "drag", "x": 0.52, "y": 0.6}, {"kind": "drag", "x": 0.55, "y": 0.6}, {"kind": "up", "x": 0.55, "y": 0.6}]}),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    let moved = spots(&h)[1].points[0];
    assert!((moved.x - 0.55).abs() < 0.01 && (moved.y - 0.6).abs() < 0.01, "{moved:?}");
    assert_eq!(spots(&h).len(), 3);
    // …and a click on a hidden spot adds one rather than selecting it
    let r = h.request("ui.pointer", json!({"events": [{"kind": "down", "x": 0.75, "y": 0.3}, {"kind": "up", "x": 0.75, "y": 0.3}]}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(spots(&h).len(), 4);
}

#[test]
fn shift_h_and_the_command_cycle_the_tool_overlay() {
    let mut h = three_spots();
    // Shift+H steps Auto → Always → Selected → Never → Auto
    for want in [ToolOverlay::Always, ToolOverlay::Selected, ToolOverlay::Never, ToolOverlay::Auto] {
        let r = h.request("ui.key", json!({"key": "h", "shift": true}), T);
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(h.app.ui.tool_overlay, want);
        assert_eq!(h.app.ui.right, RightPanel::Remove, "Shift+H leaves the panel alone");
    }
    let toast = h.app.ui.toast.as_ref().map(|t| t.0.clone());
    assert_eq!(toast.as_deref(), Some("Tool Overlay: Auto"));
    // the pins follow the key: Never hides them over the photo
    for _ in 0..3 {
        h.request("ui.key", json!({"key": "h", "shift": true}), T);
    }
    hover(&mut h, true);
    assert_eq!(h.app.ui.tool_overlay, ToolOverlay::Never);
    assert!(spot_pins(&h).is_empty());
    // the command: no params steps, `mode` sets, a bad mode is refused and changes nothing
    assert_eq!(exec(&mut h, "view.toolOverlay", json!({})), json!({"mode": "auto"}));
    assert_eq!(exec(&mut h, "view.toolOverlay", json!({"mode": "Selected"})), json!({"mode": "selected"}));
    for bad in [json!({"mode": "sometimes"}), json!({"mode": 3})] {
        let r = h.request("engine.execute", json!({"command": "view.toolOverlay", "params": bad}), T);
        assert_eq!(r["ok"], false, "{r}");
    }
    assert_eq!(h.app.ui.tool_overlay, ToolOverlay::Selected);
    // plain H still opens Remove (from Edit), and leaves the mode alone
    exec(&mut h, "panel.edit", json!({}));
    h.request("ui.key", json!({"key": "h"}), T);
    assert_eq!(h.app.ui.right, RightPanel::Remove);
    assert_eq!(h.app.ui.tool_overlay, ToolOverlay::Selected);
    assert_eq!(crate::shortcuts::shortcut_of(&h.app.ui.settings.keymap, "view.toolOverlay"), Some("Shift+H"));
}

/// View ▸ Tool Overlay lists the modes (the current one checked) and the cycle item with its key;
/// the Remove panel's dropdown sets the mode.
#[test]
fn view_menu_and_remove_panel_choose_the_tool_overlay() {
    let mut h = three_spots();
    exec(&mut h, "view.toolOverlay", json!({"mode": "never"}));
    let bar = crate::menubar::menu_bar(&h.app);
    let view = &bar.iter().find(|(t, _)| t == "View").unwrap().1;
    let Some(MenuNode::Submenu { children, .. }) = view.iter().find(|n| matches!(n, MenuNode::Submenu { label, .. } if label == "Tool Overlay"))
    else {
        panic!("View ▸ Tool Overlay")
    };
    let items: Vec<(String, serde_json::Value, Option<bool>, Option<String>)> = children
        .iter()
        .filter_map(|n| match n {
            MenuNode::Item { id, params, checked, shortcut, .. } if id == "view.toolOverlay" => {
                Some((id.clone(), params.clone(), *checked, shortcut.clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(items.len(), 5, "{items:?}");
    let checked: Vec<&serde_json::Value> = items.iter().filter(|i| i.2 == Some(true)).map(|i| &i.1).collect();
    assert_eq!(checked, vec![&json!({"mode": "never"})]);
    assert_eq!(items[4].1, serde_json::Value::Null, "the last item cycles");
    assert_eq!(items[4].3.as_deref(), Some("Shift+H"));
    // the menu items run the command like the shortcut does
    let r = h.request("ui.menu.invoke", json!({"id": "view.toolOverlay", "params": {"mode": "always"}}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(h.app.ui.tool_overlay, ToolOverlay::Always);
    // the Remove panel's dropdown
    let r = h.request("ui.clickWidget", json!({"id": "dropdown:toolOverlay"}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("ui.clickWidget", json!({"id": "toolOverlay:selected"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(h.app.ui.tool_overlay, ToolOverlay::Selected);
}

/// Mask pins follow the same mode; Show Pins (`view.maskPins`) off still hides them all.
#[test]
fn mask_pins_follow_the_tool_overlay_and_show_pins() {
    let mut h = detail("panel.masking");
    exec(&mut h, "mask.add", json!({"kind": "radial", "center": [0.3, 0.4], "rx": 0.1, "ry": 0.1}));
    exec(&mut h, "mask.add", json!({"kind": "linear", "start": [0.7, 0.2], "end": [0.7, 0.6]}));
    assert_eq!(h.app.session.active_mask, Some(2));
    let both = vec!["maskPin:1:0".to_string(), "maskPin:2:0".to_string()];
    hover(&mut h, false);
    assert!(pins(&h, "maskPin:").is_empty(), "Auto: hidden with the pointer away");
    hover(&mut h, true);
    assert_eq!(pins(&h, "maskPin:"), both);
    exec(&mut h, "view.toolOverlay", json!({"mode": "always"}));
    hover(&mut h, false);
    assert_eq!(pins(&h, "maskPin:"), both);
    exec(&mut h, "view.toolOverlay", json!({"mode": "selected"}));
    h.step();
    assert_eq!(pins(&h, "maskPin:"), vec!["maskPin:2:0".to_string()]);
    exec(&mut h, "view.toolOverlay", json!({"mode": "never"}));
    hover(&mut h, true);
    assert!(pins(&h, "maskPin:").is_empty());
    // Show Pins off hides them in every mode; view.maskPins answers as before
    exec(&mut h, "view.toolOverlay", json!({"mode": "always"}));
    assert_eq!(exec(&mut h, "view.maskPins", json!({"show": false})), json!({"maskPins": false}));
    h.step();
    assert!(pins(&h, "maskPin:").is_empty());
    assert_eq!(exec(&mut h, "view.maskPins", json!({})), json!({"maskPins": true}));
    h.step();
    assert_eq!(pins(&h, "maskPin:"), both);
    assert_eq!(h.app.ui.tool_overlay, ToolOverlay::Always, "Show Pins leaves the mode alone");
}

/// The mode is saved with the UI state (`ui.json`); settings from before it open in Auto.
#[test]
fn tool_overlay_is_saved_and_old_settings_open_in_auto() {
    let ui = crate::UiState { tool_overlay: ToolOverlay::Selected, ..Default::default() };
    let saved = serde_json::to_value(&ui).unwrap();
    assert_eq!(saved["toolOverlay"], "selected");
    let back: crate::UiState = serde_json::from_value(saved.clone()).unwrap();
    assert_eq!(back.sanitized().tool_overlay, ToolOverlay::Selected);
    let mut old = saved;
    old.as_object_mut().unwrap().remove("toolOverlay");
    let back: crate::UiState = serde_json::from_value(old).unwrap();
    assert_eq!(back.tool_overlay, ToolOverlay::Auto);
    // through the control channel too
    let mut h = detail("panel.remove");
    let r = h.request("ui.set", json!({"toolOverlay": "never"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(h.app.ui.tool_overlay, ToolOverlay::Never);
    for m in ToolOverlay::ALL {
        assert_eq!(ToolOverlay::parse(m.name()), Some(m));
        assert_eq!(serde_json::to_value(m).unwrap(), m.name());
    }
    assert_eq!(ToolOverlay::parse("nope"), None);
}
