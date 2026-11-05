//! Headless tests of the White Balance Selector's button.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::ViewMode;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// The White Balance Selector button in the Edit panel does what W does: from Compare it opens
/// the active photo in Detail with the selector armed (it used to arm a tool Compare ignores).
/// Clicking it again disarms it.
#[test]
fn wb_selector_button_in_compare_opens_detail_like_w() {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let vis: Vec<u64> = h.app.session.visible_cloned().iter().map(|p| p.0).collect();
    h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [vis[0], vis[1]], "active": vis[0]}}), T);
    let r = h.request("engine.execute", json!({"command": "view.compare"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.app.ui.right = crate::state::RightPanel::Edit;
    h.settle(SETTLE);
    // open the Color section, where the selector sits
    if !h.app.widgets.iter().any(|(w, _)| w == "icon:wbPicker") {
        h.request("ui.clickWidget", json!({"id": "section:color"}), T);
        h.settle(SETTLE);
    }
    assert_eq!(h.app.ui.view, ViewMode::Compare);
    let active = h.app.session.selection.active;
    let r = h.request("ui.clickWidget", json!({"id": "icon:wbPicker"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.view, ViewMode::Detail, "the button leaves Compare like W");
    assert_eq!(h.app.ui.tool, "wbPicker");
    assert_eq!(h.app.session.selection.active, active, "on the photo that was active in Compare");
    let r = h.request("ui.clickWidget", json!({"id": "icon:wbPicker"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.tool, "", "a second click disarms it");
}
