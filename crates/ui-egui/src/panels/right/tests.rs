// Feature: the History panel greys out the steps after the cursor
#[test]
fn history_rows_after_the_cursor_are_greyed() {
    use super::{HistoryRow, history_row};
    let rows: Vec<_> = (0..4).map(|i| history_row(i, Some(1))).collect();
    assert_eq!(rows, [HistoryRow::Done, HistoryRow::Current, HistoryRow::Undone, HistoryRow::Undone]);
    assert_eq!(history_row(3, Some(3)), HistoryRow::Current, "at the latest step nothing is greyed");
    assert_eq!(history_row(0, None), HistoryRow::Done);
}

// Feature: the aspect button tells what is locked
#[test]
fn aspect_label_names_presets_in_either_orientation() {
    assert_eq!(super::aspect_label(None, Some(1.5)), "Free");
    assert_eq!(super::aspect_label(Some((1600, 900)), None), "16 × 9");
    assert_eq!(super::aspect_label(Some((900, 1600)), None), "16 × 9", "portrait 16:9");
    assert_eq!(super::aspect_label(Some((400, 500)), None), "4 × 5 / 8 × 10");
}

#[test]
fn aspect_label_prefers_original_over_a_coinciding_preset_and_never_shows_pixel_sizes() {
    assert_eq!(super::aspect_label(Some((300, 200)), Some(1.5)), "Original");
    assert_eq!(super::aspect_label(Some((600_000, 400_000)), Some(1.5)), "Original");
    assert_eq!(super::aspect_label(Some((137, 100)), Some(1.5)), "1.37 : 1");
    assert_eq!(super::aspect_label(Some((0, 5)), None), "Free");
}

#[test]
fn short_times() {
    assert_eq!(super::short_time("2026-09-30T12:00:05"), "Sep 30, 2026, 12:00 PM");
    assert_eq!(super::short_time("2026-01-02"), "Jan 2, 2026");
}

// Feature: the Keywords panel keeps its scroll position when a photo is selected (CodeRabbit on #3031: the
// no-selection list scrolled under another id)
#[test]
fn the_keywords_panel_scrolls_under_one_id_with_or_without_a_selection() {
    use crate::state::RightPanel;
    // (`show` names the no-selection list's scroll area by `scroll_salt(RightPanel::Keywords)`, as it names the
    // panel's with a selection by `scroll_salt(app.ui.right)`)
    assert_eq!(super::scroll_salt(RightPanel::Keywords), ("right-scroll", RightPanel::Keywords));
    assert_ne!(super::scroll_salt(RightPanel::Keywords), super::scroll_salt(RightPanel::Edit), "each panel keeps its own");
}

// The Keywords panel scrolled with no photo selected is the same scroll area as with one: the offset it was left at
// is there when a photo is selected (CodeRabbit on #3031)
#[test]
fn the_keywords_panel_keeps_its_offset_from_no_selection_to_a_selection() {
    use crate::headless::Headless;
    use serde_json::json;
    use std::time::Duration;
    const T: Duration = Duration::from_secs(20);
    let services = crate::Services { png: None, ..Default::default() };
    let mut h = Headless::new(crate::LightcraftApp::new(lightcraft_engine::Session::with_demo(), services), [1200.0, 420.0], 1.0);
    // a Keyword List longer than the panel
    for i in 0..80 {
        h.app.session.execute("keyword.create", &json!({"name": format!("keyword {i:02}")})).unwrap();
    }
    let r = h.request("ui.set", json!({"view": "detail", "right": "keywords"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(Duration::from_secs(120));
    // with a selection: the panel's scroll area (as the panel records it)
    let area = h.view.ctx.data(|d| d.get_temp::<egui::Id>(egui::Id::unique(super::AREA))).expect("the panel's scroll area");
    let offset = |h: &Headless| egui::scroll_area::State::load(&h.view.ctx, area).map_or(0.0, |s| s.clamped_offset().y);
    assert_eq!(offset(&h), 0.0);
    // no selection: the list scrolled down
    let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": []}}), T);
    assert_eq!(r["ok"], true, "{r}");
    for _ in 0..4 {
        h.step();
    }
    assert!(h.app.session.active().is_none());
    let panel = h.app.widgets.iter().find(|(w, _)| w == "panel:right_panel").map(|(_, r)| *r).expect("the right panel");
    h.request("ui.move", json!({"x": panel.center().x, "y": panel.center().y}), T);
    let r = h.request("ui.scroll", json!({"dy": -300.0}), T);
    assert_eq!(r["ok"], true, "{r}");
    for _ in 0..60 {
        h.step();
    }
    let scrolled = offset(&h);
    assert!(scrolled > 50.0, "the no-selection list scrolls the panel's own area: {scrolled}");
    // a photo selected: the area keeps the offset (moved down only by the Keywords box put above the list)
    let id = h.app.session.visible_cloned()[0];
    let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [id.0]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    for _ in 0..10 {
        h.step();
    }
    assert!(offset(&h) >= scrolled - 0.5, "the offset is kept: {} after {scrolled}", offset(&h));
}
