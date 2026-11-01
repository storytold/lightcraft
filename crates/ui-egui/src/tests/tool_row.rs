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

/// FredApps on #3158: at the narrowest the panel goes (250 pt) every tool button keeps a hit
/// target of at least 24 × 24 pt: the row wraps onto a second line instead of shrinking them, and
/// every tool is still there, inside the panel, above the sections.
#[test]
fn a_narrow_panel_wraps_the_tool_row_and_keeps_every_button_24_pt() {
    let mut h = edit_panel();
    narrowest(&mut h);
    let panel = widget(&h, "panel:right_panel").expect("the panel");
    let row = widget(&h, "toolRow").expect("the tool row");
    assert!(panel.width() <= crate::state::RIGHT_WIDTH.min + 2.0, "the narrowest panel: {panel:?}");
    assert!(row.height() > crate::panels::strip::ROW_H, "two lines: {row:?}");
    let light = widget(&h, "section:light").expect("Light");
    assert!(row.bottom() <= light.top(), "above the sections");
    for id in ["edit", "crop", "remove", "masking", "versions", "activity", "keywords", "info", "presets", "more"] {
        let r = widget(&h, &format!("icon:{id}")).unwrap_or_else(|| panic!("icon:{id}"));
        assert!(r.width() >= 24.0 - 1e-3 && r.height() >= 24.0 - 1e-3, "{id}: a 24 pt target: {r:?}");
        assert!(row.contains_rect(r) && panel.contains_rect(r), "{id} in the row: {r:?}");
    }
}

/// The row's fit: one line while every button can be 24 pt or more, else even lines; never a
/// button under 24 pt or a line without one, whatever the width.
#[test]
fn the_tool_row_fits_its_buttons_at_any_width() {
    use crate::panels::strip::{ROW_BUTTON_MIN, row_fit};
    assert_eq!(row_fit(318.0, 10).0, 10, "the default panel: one line");
    let (per_line, lines, bw) = row_fit(250.0, 10);
    assert_eq!((per_line, lines), (5, 2), "the narrowest panel: two even lines");
    assert!(bw >= ROW_BUTTON_MIN);
    for w in [0.0, 1.0, 30.0, 100.0, 249.0, 274.0, 318.0, 520.0, 1e9, f32::NAN, f32::INFINITY, -50.0] {
        for count in [0, 1, 10] {
            let (per_line, lines, bw) = row_fit(w, count);
            assert!(per_line >= 1 && lines >= 1 && per_line * lines >= count, "{w} / {count}: {per_line} × {lines}");
            assert!(bw.is_finite() && bw >= ROW_BUTTON_MIN, "{w} / {count}: {bw}");
        }
    }
}

/// Drag the panel's inner edge as far right as it goes: the narrowest panel.
fn narrowest(h: &mut Headless) {
    let panel = widget(h, "panel:right_panel").expect("the panel");
    let r = h.request("ui.drag", json!({"x": panel.left() + 2.0, "y": 500.0, "toX": panel.left() + 300.0, "toY": 500.0, "steps": 12}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
}

/// The registered widget that has the keyboard focus.
fn focused(h: &Headless) -> Option<String> {
    let id = h.view.ctx.memory(|m| m.focused())?;
    let rect = h.view.ctx.read_response(id)?.rect;
    h.app.widgets.iter().find(|(_, r)| *r == rect).map(|(w, _)| w.clone())
}

/// FredApps on #3158: Tab reaches every tool in the row in the stated order, left to right and,
/// when the row wraps on the narrowest panel, line by line.
#[test]
fn tab_reaches_the_row_tools_in_order_on_one_line_and_wrapped() {
    let want = ["edit", "crop", "remove", "masking", "versions", "activity", "keywords", "info", "presets", "more"].map(|id| format!("icon:{id}"));
    for narrow in [false, true] {
        let mut h = edit_panel();
        if narrow {
            narrowest(&mut h);
        }
        let row = widget(&h, "toolRow").expect("the tool row");
        let mut order: Vec<String> = Vec::new();
        for _ in 0..400 {
            assert_eq!(h.request("ui.key", json!({"key": "Tab"}), T)["ok"], true);
            h.step();
            h.step();
            let Some(w) = focused(&h) else { continue };
            // (the top bar and the view toolbar have icons too: only the row's count here)
            if !w.starts_with("icon:") || !widget(&h, &w).is_some_and(|r| row.contains_rect(r)) {
                continue;
            }
            if order.first() == Some(&w) {
                break;
            }
            if order.last() != Some(&w) {
                order.push(w);
            }
        }
        assert_eq!(order, want, "narrow {narrow}: the row's Tab order");
    }
}
