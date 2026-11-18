//! The panel a tool takes the place of comes back when the tool closes (item 12 of the evening
//! retest): open Edit → Masking (or Crop) → close it → Edit again, at the same width and scroll
//! position, with the photo where it was; a hidden panel stays hidden.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::RightPanel;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn detail() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LccApp::new(lcc_engine::Session::with_demo(), services);
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
fn the_panel_a_tool_replaced_comes_back_where_it_was_and_a_hidden_one_stays_hidden() {
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
    for tool in ["panel.masking", "panel.crop", "panel.remove"] {
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
    // Masking, then Crop from it, then Crop closed: still Edit
    exec(&mut h, "panel.masking");
    exec(&mut h, "panel.crop");
    exec(&mut h, "panel.crop");
    assert_eq!(h.app.ui.right, RightPanel::Edit);
    // Return (Done) in a tool also goes back
    exec(&mut h, "panel.masking");
    exec(&mut h, "tool.done");
    assert_eq!(h.app.ui.right, RightPanel::Edit);
    // a hidden panel stays hidden after a tool
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
