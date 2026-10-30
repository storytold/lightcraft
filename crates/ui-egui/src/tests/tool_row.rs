//! The tools stand in one row at the top of the right-hand panel (under the
//! fixed histogram), Red Eye in its "…" menu; the vertical strip only while the panel is closed;
//! and a section's slider names share a column fitted to its longest name, so the tracks are long.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::RightPanel;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn edit_panel() -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1600.0, 1000.0], 1.0);
    assert_eq!(h.request("ui.set", json!({"view": "detail", "right": "edit", "leftPanel": false}), T)["ok"], true);
    h.app.ui.open_sections = vec!["light".to_string(), "color".to_string(), "effects".to_string(), "detail".to_string()];
    h.settle(SETTLE);
    h
}

fn widget(h: &Headless, id: &str) -> Option<egui::Rect> {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r)
}

#[test]
fn the_tools_are_a_row_under_the_histogram_in_the_panel() {
    let mut h = edit_panel();
    let panel = widget(&h, "panel:right_panel").expect("the panel");
    let row = widget(&h, "toolRow").expect("the tool row");
    assert!(panel.contains_rect(row), "{row:?} in {panel:?}");
    assert!(row.height() <= 32.0, "one thin row: {row:?}");
    let light = widget(&h, "section:light").expect("Light");
    assert!(row.bottom() <= light.top(), "above the sections");
    for id in ["edit", "crop", "remove", "masking", "versions", "activity", "keywords", "info", "presets", "more"] {
        let r = widget(&h, &format!("icon:{id}")).unwrap_or_else(|| panic!("icon:{id}"));
        assert!(row.contains_rect(r), "{id} in the row: {r:?}");
    }
    assert!(widget(&h, "icon:redeye").is_none(), "Red Eye only in the … menu");
    // the panel takes the whole right side: no vertical strip beside it
    assert!(panel.right() >= 1599.0, "{panel:?}");
    // the tools work from the row; Red Eye from its menu
    let r = h.request("ui.clickWidget", json!({"id": "icon:crop"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.right, RightPanel::Crop);
    assert_eq!(h.request("ui.clickWidget", json!({"id": "icon:more"}), T)["ok"], true);
    h.settle(SETTLE);
    let r = h.request("ui.clickWidget", json!({"id": "menuItem:toolRow:redeye"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.right, RightPanel::RedEye);
}

#[test]
fn a_closed_panel_leaves_the_vertical_strip_to_open_it_again() {
    let mut h = edit_panel();
    assert_eq!(h.request("ui.set", json!({"right": "none"}), T)["ok"], true);
    h.settle(SETTLE);
    assert!(widget(&h, "toolRow").is_none());
    let edit = widget(&h, "icon:edit").expect("the strip's Edit");
    assert!(edit.right() >= 1590.0, "at the window's edge: {edit:?}");
    assert_eq!(h.request("ui.clickWidget", json!({"id": "icon:edit"}), T)["ok"], true);
    h.settle(SETTLE);
    assert_eq!(h.app.ui.right, RightPanel::Edit);
}

#[test]
fn a_sections_tracks_start_after_its_longest_name_and_are_long() {
    let h = edit_panel();
    let panel = widget(&h, "panel:right_panel").expect("the panel");
    let light: Vec<egui::Rect> = ["exposure", "contrast", "highlights", "shadows", "whites", "blacks"]
        .iter()
        .map(|c| widget(&h, &format!("slider:light.{c}")).unwrap_or_else(|| panic!("{c}")))
        .collect();
    for r in &light {
        assert!((r.left() - light[0].left()).abs() < 0.5, "one column: {light:?}");
        // as long as Lightroom's (58 % of its panel), at the default width
        assert!(r.width() >= 0.58 * panel.width(), "{r:?} of {panel:?}");
    }
    // the column ends just after the longest name ("Highlights"), not at a fixed share
    let name = widget(&h, "sliderLabel:light.highlights").expect("name");
    assert!(name.width() < 70.0, "{name:?}");
}
