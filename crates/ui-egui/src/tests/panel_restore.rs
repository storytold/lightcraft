//! The panel a tool takes the place of comes back when the tool closes (item 12 of the evening
//! retest): open Edit → Masking (or Crop) → close it → Edit again, at the same width and scroll
//! position, with the photo where it was; a hidden panel stays hidden.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::RightPanel;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn detail() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail", "right": "edit"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h
}

fn exec(h: &mut Headless, command: &str) {
    let r = h.request("engine.execute", json!({"command": command, "params": {}}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
    h.step();
    h.step();
}

fn widget(h: &Headless, id: &str) -> Option<egui::Rect> {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r)
}

/// The Edit panel's first slider on show (its id and where it is: it moves when the panel scrolls).
fn first_slider(h: &Headless) -> (String, egui::Rect) {
    let panel = widget(h, "panel:right_panel").expect("the right panel");
    h.app
        .widgets
        .iter()
        .filter(|(w, r)| w.starts_with("slider:") && panel.contains_rect(*r))
        .min_by(|a, b| a.1.top().total_cmp(&b.1.top()))
        .map(|(w, r)| (w.clone(), *r))
        .expect("a slider in the Edit panel")
}

#[test]
fn the_edit_panel_comes_back_after_masking_and_crop_where_it_was() {
    let mut h = detail();
    // scroll the Edit panel down a little (the wheel scrolls what the pointer is over)
    let panel = widget(&h, "panel:right_panel").expect("the right panel");
    h.request("ui.move", json!({"x": panel.center().x, "y": panel.center().y}), T);
    let r = h.request("ui.scroll", json!({"dy": -300.0}), T);
    assert_eq!(r["ok"], true, "{r}");
    for _ in 0..60 {
        h.step();
    }
    let (slider, at) = first_slider(&h);
    let photo = widget(&h, "canvas:image").expect("the photo");
    for tool in ["panel.masking", "panel.crop", "panel.remove", "panel.redeye"] {
        exec(&mut h, tool);
        assert!(h.app.ui.right.is_tool(), "{tool}: {:?}", h.app.ui.right);
        assert_eq!(widget(&h, "panel:right_panel"), Some(panel), "{tool}: the panel keeps its place and width");
        // the same button again closes the tool: Edit is back, as it was
        exec(&mut h, tool);
        assert_eq!(h.app.ui.right, RightPanel::Edit, "{tool}: the Edit panel comes back");
        assert_eq!(widget(&h, "panel:right_panel"), Some(panel), "{tool}: same place and width");
        assert_eq!(widget(&h, "canvas:image"), Some(photo), "{tool}: the photo didn't move");
        assert_eq!(widget(&h, &slider), Some(at), "{tool}: Edit kept its scroll position");
    }
    // Masking, then Crop from it, then Crop closed: exactly what was there before, Masking (note 21); then Edit
    exec(&mut h, "panel.masking");
    exec(&mut h, "panel.crop");
    exec(&mut h, "panel.crop");
    assert_eq!(h.app.ui.right, RightPanel::Masking);
    exec(&mut h, "panel.masking");
    assert_eq!(h.app.ui.right, RightPanel::Edit);
    // Return (Done) in a tool also goes back
    exec(&mut h, "panel.masking");
    exec(&mut h, "tool.done");
    assert_eq!(h.app.ui.right, RightPanel::Edit);
}

#[test]
fn a_hidden_panel_stays_hidden_after_a_tool() {
    let mut h = detail();
    exec(&mut h, "panel.close");
    assert_eq!(h.app.ui.right, RightPanel::None);
    let photo = widget(&h, "canvas:image").expect("the photo");
    for tool in ["panel.masking", "panel.crop"] {
        exec(&mut h, tool);
        assert!(h.app.ui.right.is_tool(), "{tool} shows");
        exec(&mut h, tool);
        assert_eq!(h.app.ui.right, RightPanel::None, "{tool}: nothing shows again");
        h.settle(SETTLE);
        assert_eq!(widget(&h, "canvas:image"), Some(photo), "{tool}: the photo is where it was");
    }
    exec(&mut h, "panel.masking");
    exec(&mut h, "tool.done");
    assert_eq!(h.app.ui.right, RightPanel::None, "Done: hidden as before");
    // a panel picked meanwhile is what comes back: Info → Masking → closed = Info
    exec(&mut h, "panel.info");
    exec(&mut h, "panel.masking");
    exec(&mut h, "panel.masking");
    assert_eq!(h.app.ui.right, RightPanel::Info);
}

/// "I" toggles the Info panel; pressing it again brings back exactly what the right column showed before, closed
/// included (test notes 2026-10-24 note 21: the Edit sliders stayed gone).
#[test]
fn info_toggled_off_brings_back_exactly_what_was_there() {
    let mut h = detail();
    assert_eq!(h.app.ui.right, RightPanel::Edit);
    // the key, as the user pressed it
    let key = |h: &mut Headless| {
        let r = h.request("ui.key", json!({"key": "I"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        h.step();
    };
    key(&mut h);
    assert_eq!(h.app.ui.right, RightPanel::Info);
    key(&mut h);
    assert_eq!(h.app.ui.right, RightPanel::Edit, "Edit → I → I = Edit");
    exec(&mut h, "panel.masking");
    key(&mut h);
    assert_eq!(h.app.ui.right, RightPanel::Info);
    key(&mut h);
    assert_eq!(h.app.ui.right, RightPanel::Masking, "Masks → I → I = Masks");
    // and Masks still closes back to Edit
    exec(&mut h, "panel.masking");
    assert_eq!(h.app.ui.right, RightPanel::Edit);
    for panel in ["panel.keywords", "panel.versions", "panel.activity"] {
        exec(&mut h, panel);
        exec(&mut h, panel);
        assert_eq!(h.app.ui.right, RightPanel::Edit, "{panel} toggled off: Edit again");
    }
    exec(&mut h, "panel.close");
    key(&mut h);
    assert_eq!(h.app.ui.right, RightPanel::Info);
    key(&mut h);
    assert_eq!(h.app.ui.right, RightPanel::None, "closed → I → I = closed");
}

/// A `ui.set` that changes something else (here the panel width) keeps a pending toggle: I still puts Edit back
/// (CodeRabbit on #3031).
#[test]
fn a_ui_set_between_the_two_presses_keeps_the_toggle() {
    let mut h = detail();
    exec(&mut h, "panel.info");
    assert_eq!(h.app.ui.right, RightPanel::Info);
    let r = h.request("ui.set", json!({"rightWidth": 320.0}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    exec(&mut h, "panel.info");
    assert_eq!(h.app.ui.right, RightPanel::Edit, "Edit → I → ui.set → I = Edit");
}

/// A tool panel restored from the saved UI state (the panel before it unknown) closes to Edit, not to a hidden column.
#[test]
fn a_tool_restored_from_saved_state_closes_to_edit() {
    let mut ui: crate::state::UiState = serde_json::from_str(r#"{"right": "crop"}"#).unwrap();
    assert_eq!(ui.right, RightPanel::Crop);
    ui.note_right();
    assert_eq!(ui.leave_tool(), RightPanel::Edit);
}

/// F8 hiding a tool and F8 showing it again keeps what the tool closes to: Info → Crop → F8 → F8 → Done = Info, not a
/// hidden column (CodeRabbit on #3031).
#[test]
fn a_tool_hidden_and_shown_by_f8_still_closes_to_the_panel_before_it() {
    let mut h = detail();
    for close in ["tool.done", "panel.crop"] {
        exec(&mut h, "panel.close");
        exec(&mut h, "panel.info");
        assert_eq!(h.app.ui.right, RightPanel::Info);
        exec(&mut h, "panel.crop");
        assert_eq!(h.app.ui.right, RightPanel::Crop);
        exec(&mut h, "view.rightPanel");
        assert_eq!(h.app.ui.right, RightPanel::None, "F8 hides Crop");
        exec(&mut h, "view.rightPanel");
        assert_eq!(h.app.ui.right, RightPanel::Crop, "F8 shows Crop again");
        exec(&mut h, close);
        assert_eq!(h.app.ui.right, RightPanel::Info, "{close}: Info → Crop → F8 → F8 → closed = Info");
    }
    // a panel picked while the tool was hidden is not undone: Crop → F8 → Masking → closed = closed, as before
    exec(&mut h, "panel.close");
    exec(&mut h, "panel.crop");
    exec(&mut h, "view.rightPanel");
    exec(&mut h, "panel.info");
    exec(&mut h, "panel.masking");
    exec(&mut h, "panel.masking");
    assert_eq!(h.app.ui.right, RightPanel::Info, "a panel picked after F8 is what comes back");
}

/// A tool opened another way on the column F8 hid is not F8 showing a tool again: Info → Crop → F8 → Brush → Done
/// leaves the column hidden, as it was when the brush was picked (CodeRabbit on #3031).
#[test]
fn a_tool_opened_on_the_column_f8_hid_closes_to_the_hidden_column() {
    let mut h = detail();
    exec(&mut h, "panel.close");
    exec(&mut h, "panel.info");
    exec(&mut h, "panel.crop");
    exec(&mut h, "view.rightPanel");
    assert_eq!(h.app.ui.right, RightPanel::None, "F8 hides Crop");
    exec(&mut h, "tool.brush");
    assert_eq!(h.app.ui.right, RightPanel::Masking);
    exec(&mut h, "tool.done");
    assert_eq!(h.app.ui.right, RightPanel::None, "Info → Crop → F8 → Brush → Done = hidden");
}

/// A tool opened from another tool keeps where it closes to across F8: Masking → Crop → F8 → F8 → closed = Masking,
/// and Masking then closes to Edit (CodeRabbit on #3031).
#[test]
fn a_nested_tool_hidden_and_shown_by_f8_still_closes_to_the_tool_before_it() {
    let mut h = detail();
    for close in ["tool.done", "panel.crop"] {
        exec(&mut h, "panel.masking");
        exec(&mut h, "panel.crop");
        assert_eq!(h.app.ui.right, RightPanel::Crop);
        exec(&mut h, "view.rightPanel");
        assert_eq!(h.app.ui.right, RightPanel::None, "F8 hides Crop");
        exec(&mut h, "view.rightPanel");
        assert_eq!(h.app.ui.right, RightPanel::Crop, "F8 shows Crop again");
        exec(&mut h, close);
        assert_eq!(h.app.ui.right, RightPanel::Masking, "{close}: Masking → Crop → F8 → F8 → closed = Masking");
        exec(&mut h, "panel.masking");
        assert_eq!(h.app.ui.right, RightPanel::Edit, "{close}: then Masking closes to Edit");
    }
}

/// Toggles nest: Edit → I → K → K → I = Edit, each press taking back exactly the one before it (CodeRabbit on #3031).
#[test]
fn nested_panel_toggles_come_back_one_at_a_time() {
    let mut h = detail();
    exec(&mut h, "panel.info");
    exec(&mut h, "panel.keywords");
    assert_eq!(h.app.ui.right, RightPanel::Keywords);
    exec(&mut h, "panel.keywords");
    assert_eq!(h.app.ui.right, RightPanel::Info, "Keywords toggled off: Info again");
    exec(&mut h, "panel.info");
    assert_eq!(h.app.ui.right, RightPanel::Edit, "Info toggled off: Edit again");
    // pressing two keys in turn many times keeps a bounded history and still unwinds to a panel
    for _ in 0..crate::state::TOGGLE_HISTORY {
        exec(&mut h, "panel.info");
        exec(&mut h, "panel.keywords");
    }
    assert!(h.app.ui.toggle_return.len() <= crate::state::TOGGLE_HISTORY);
    exec(&mut h, "panel.keywords");
    assert_eq!(h.app.ui.right, RightPanel::Info);
}

/// A tool opened by a command from another tool closes back to that tool, and the panels before it still come back
/// in turn: Info → Crop → B → Done = Crop, Crop closed = Info, I = Edit (CodeRabbit on #3031).
#[test]
fn a_tool_opened_from_another_tool_closes_back_to_it() {
    let mut h = detail();
    exec(&mut h, "panel.info");
    exec(&mut h, "panel.crop");
    exec(&mut h, "tool.brush");
    assert_eq!(h.app.ui.right, RightPanel::Masking);
    exec(&mut h, "tool.done");
    assert_eq!(h.app.ui.right, RightPanel::Crop, "Info → Crop → B → Done = Crop");
    exec(&mut h, "tool.done");
    assert_eq!(h.app.ui.right, RightPanel::Info, "then Crop closes to Info");
    exec(&mut h, "panel.info");
    assert_eq!(h.app.ui.right, RightPanel::Edit, "and I gives back Edit");
    // a tool opened by a command from a panel closes to that panel
    exec(&mut h, "tool.brush");
    exec(&mut h, "tool.done");
    assert_eq!(h.app.ui.right, RightPanel::Edit);
}

/// Commands run back to back with no frame between them (a batch of control requests) see F8's hide at once:
/// Info → Crop → F8 → Brush → Done leaves the column hidden (CodeRabbit on #3031).
#[test]
fn f8_is_noted_before_the_next_command_of_the_same_frame() {
    let mut h = detail();
    exec(&mut h, "panel.close");
    for command in ["panel.info", "panel.crop", "view.rightPanel", "tool.brush", "tool.done"] {
        h.app.run(command, json!({})).unwrap_or_else(|e| panic!("{command}: {e}"));
    }
    assert_eq!(h.app.ui.right, RightPanel::None, "the brush was opened on the hidden column");
}

/// Info → Remove → F8 → A (Visualize Spots opens Remove again on the hidden column) → Done leaves the column
/// hidden: F8 kept Remove's record, but this opening is a new one, from the hidden column (CodeRabbit on #3031).
#[test]
fn a_tool_hidden_by_f8_and_opened_again_by_a_command_closes_to_the_hidden_column() {
    let mut h = detail();
    for command in ["panel.info", "view.visualizeSpots", "view.rightPanel", "view.visualizeSpots"] {
        h.app.run(command, json!({})).unwrap_or_else(|e| panic!("{command}: {e}"));
    }
    assert_eq!(h.app.ui.right, RightPanel::Remove);
    h.app.run("tool.done", json!({})).unwrap_or_else(|e| panic!("tool.done: {e}"));
    assert_eq!(h.app.ui.right, RightPanel::None, "Remove was opened again on the hidden column");
    // the same steps a frame apart
    let mut h = detail();
    for command in ["panel.info", "view.visualizeSpots", "view.rightPanel", "view.visualizeSpots", "tool.done"] {
        exec(&mut h, command);
    }
    assert_eq!(h.app.ui.right, RightPanel::None, "frame by frame");
}

/// Commands run in one frame (a script, queued shortcuts) each see the panel the one before left: Close Panel →
/// B → Done keeps the column hidden, and B → A → Done goes back to Masking (CodeRabbit on #3031).
#[test]
fn every_command_notes_the_panel_it_leaves_on_show() {
    let mut h = detail();
    for command in ["panel.close", "tool.brush", "tool.done"] {
        h.app.run(command, json!({})).unwrap_or_else(|e| panic!("{command}: {e}"));
    }
    assert_eq!(h.app.ui.right, RightPanel::None, "the brush was opened on the hidden column");
    let mut h = detail();
    for command in ["tool.brush", "view.visualizeSpots", "tool.done"] {
        h.app.run(command, json!({})).unwrap_or_else(|e| panic!("{command}: {e}"));
    }
    assert_eq!(h.app.ui.right, RightPanel::Masking, "Remove was opened from Masking");
}

/// `ui.set` changing the panel notes it at once: a tool opened before the next frame closes back to that panel
/// (CodeRabbit on #3031).
#[test]
fn a_panel_set_by_ui_set_is_noted_before_the_next_command() {
    let mut h = detail();
    let (req, _rx) = crate::control::ControlRequest::new("ui.set", json!({"right": "info"}));
    let ctx = h.view.ctx.clone();
    let _ = crate::control::handle(&mut h.app, &ctx, &req);
    assert_eq!(h.app.ui.right, RightPanel::Info);
    for command in ["tool.brush", "tool.done"] {
        h.app.run(command, json!({})).unwrap_or_else(|e| panic!("{command}: {e}"));
    }
    assert_eq!(h.app.ui.right, RightPanel::Info, "the brush was opened from Info");
}

/// A tool panel put back by a toggle brings its own on-canvas tool back: Masking's Brush, Remove's Heal or Clone
/// (CodeRabbit on #3031). The panels come back as before (Edit → I → K → K → I = Edit).
#[test]
fn a_panel_put_back_by_a_toggle_keeps_its_tool() {
    let mut h = detail();
    exec(&mut h, "panel.masking");
    h.app.ui.tool = "brush".into();
    exec(&mut h, "panel.info");
    assert_eq!(h.app.ui.right, RightPanel::Info);
    exec(&mut h, "panel.info");
    assert_eq!(h.app.ui.right, RightPanel::Masking, "Masks → I → I = Masks");
    assert_eq!(h.app.ui.tool, "brush", "Masks → I → I keeps the Brush");
    exec(&mut h, "panel.masking");
    assert_eq!(h.app.ui.right, RightPanel::Edit);
    assert_eq!(h.app.ui.tool, "", "Edit comes back with no tool");

    exec(&mut h, "panel.remove");
    assert_eq!(h.app.ui.right, RightPanel::Remove);
    for mode in ["heal", "clone"] {
        h.app.ui.tool = mode.into();
        exec(&mut h, "panel.info");
        exec(&mut h, "panel.info");
        assert_eq!(h.app.ui.right, RightPanel::Remove);
        assert_eq!(h.app.ui.tool, mode, "Remove → I → I keeps {mode}");
    }
    exec(&mut h, "panel.remove");
    assert_eq!(h.app.ui.right, RightPanel::Edit);

    // the panels themselves come back as before
    for (command, right) in [
        ("panel.info", RightPanel::Info),
        ("panel.keywords", RightPanel::Keywords),
        ("panel.keywords", RightPanel::Info),
        ("panel.info", RightPanel::Edit),
    ] {
        exec(&mut h, command);
        assert_eq!(h.app.ui.right, right, "Edit → I → K → K → I, after {command}");
    }
}

/// A toggle that opens a panel while the Tools column is behind another dock tab remembers the
/// column as hidden: toggled off, it doesn't come back with the panel or the on-canvas tool it held
/// out of sight (CodeRabbit on #3031: Remove in Heal behind another tab came back with Heal armed).
#[test]
fn a_toggle_over_a_hidden_tools_column_puts_back_a_hidden_column() {
    let mut ui = crate::state::UiState { right: RightPanel::Remove, tool: "heal".into(), ..Default::default() };
    assert!(ui.toggle_panel(RightPanel::Info, false), "Info comes up");
    assert_eq!(ui.right, RightPanel::Info);
    assert!(!ui.toggle_panel(RightPanel::Info, true), "and goes");
    assert_eq!(ui.right, RightPanel::None, "the column was out of sight: it stays out of sight");
    assert_eq!(ui.tool, "heal", "no tool put back (the command clears it)");
    let mut ui = crate::state::UiState { right: RightPanel::Edit, ..Default::default() };
    ui.toggle_panel(RightPanel::Info, false);
    ui.toggle_panel(RightPanel::Info, true);
    assert_eq!(ui.right, RightPanel::None, "Edit behind another tab isn't brought forward either");
}
