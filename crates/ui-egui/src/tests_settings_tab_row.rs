//! The Settings window (OneSlips, LR-PREF-WINDOW): in the Classic layout on a desktop-sized window,
//! Settings is a window; with its categories along the top, one row of tabs. Every tab is reachable from the row or its More
//! menu, the window keeps its size and place on every tab, its controls stay inside it on a small
//! screen, Esc closes it, and it opens again on the tab it was left at.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);

fn demo(size: [f32; 2]) -> Headless {
    let services = Services { png: None, write_shared: Some(std::sync::Arc::new(|_: &str, _: &[u8]| Ok(()))), ..Default::default() };
    let mut app = LccApp::new(lcc_engine::Session::with_demo(), services);
    // (the row of tabs: Settings ▸ General ▸ Settings Categories: Top; Classic lists them on the left by default)
    app.ui.settings.settings_categories_classic = Some(crate::state::SettingsCategories::Top);
    let mut h = Headless::new(app, size, 1.0);
    h.settle(T);
    h
}

fn rect(h: &Headless, id: &str) -> Option<egui::Rect> {
    h.app.widgets.iter().rev().find(|(w, _)| w == id).map(|(_, r)| *r)
}

fn steps(h: &mut Headless, n: usize) {
    for _ in 0..n {
        h.step();
    }
}

fn settings(h: &mut Headless, params: serde_json::Value) {
    let r = h.request("engine.execute", json!({"command": "app.settings", "params": params}), T);
    assert_eq!(r["ok"], true, "{r}");
    steps(h, 6);
}

fn open_tab(h: &Headless) -> String {
    match &h.app.ui.dialog {
        Some(Dialog::Settings { tab }) => tab.clone(),
        other => panic!("Settings is not open: {other:?}"),
    }
}

fn shown_tabs() -> Vec<&'static str> {
    crate::panels::settings::shown_tabs().map(|(id, _)| *id).collect()
}

/// Clicks tab `id` in the row, or through More when it is in the menu.
fn pick(h: &mut Headless, id: &str) {
    let widget = format!("button:settingsTab-{id}");
    if rect(h, &widget).is_none() {
        let r = h.request("ui.clickWidget", json!({"id": "button:settingsTabMore"}), T);
        assert_eq!(r["ok"], true, "{id}: More: {r}");
        steps(h, 4);
    }
    let r = h.request("ui.clickWidget", json!({"id": widget}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    steps(h, 6);
}

#[test]
fn classic_settings_is_a_window_with_one_row_of_tabs() {
    let mut h = demo([1920.0, 1080.0]);
    settings(&mut h, json!({"tab": "general"}));
    assert!(rect(&h, "button:settingsTab-general").is_some(), "the General tab");
    assert!(rect(&h, "settings:section:general").is_none(), "no section rail in the window");
    let page = rect(&h, "settings:rows").expect("the page");
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, h.size);
    assert!(screen.contains_rect(page), "{page:?}");
    assert!(page.width() < screen.width() - 100.0, "a window, not the whole screen: {page:?}");
    // one line: every tab in the row at the same height
    let tops: Vec<f32> = shown_tabs().iter().filter_map(|id| rect(&h, &format!("button:settingsTab-{id}"))).map(|r| r.top()).collect();
    assert!(tops.len() > 4 && tops.iter().all(|t| (t - tops[0]).abs() < 0.5), "one row: {tops:?}");
    // every tab in the row (no More menu), no page wider than its room, the whole window on screen
    for id in shown_tabs() {
        pick(&mut h, id);
        assert_eq!(open_tab(&h), id, "{id} opened");
        assert!(rect(&h, "button:settingsTabMore").is_none(), "{id}: a tab went under More");
        for tab in shown_tabs() {
            assert!(rect(&h, &format!("button:settingsTab-{tab}")).is_some(), "{id}: {tab} is not in the row");
        }
        let room = rect(&h, "settings:rows").expect("the page's room");
        let page = rect(&h, "settings:page").expect("the page");
        assert!(page.right() <= room.right() + 0.5, "{id}: the page {page:?} is wider than its room {room:?}");
        for w in ["settings:rows", "settings:back", "dialog:title"] {
            let r = rect(&h, w).unwrap_or_else(|| panic!("{id}: no {w}"));
            assert!(screen.contains_rect(r), "{id}: {w} {r:?} runs off the {screen:?} screen");
        }
    }
    // the window opens as wide as the demo's own tabs, measured (the old default was too narrow for them)
    let big = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(2560.0, 1600.0));
    let tabs_w = crate::panels::settings::tab_row_width(&h.view.ctx);
    assert!(tabs_w > 0.0 && crate::panels::settings::window_size(big, None, tabs_w).default.x >= tabs_w);
}

#[test]
fn every_tab_is_reachable_the_window_keeps_its_size_and_reopens_where_it_was() {
    let mut h = demo([1300.0, 860.0]);
    settings(&mut h, json!({"tab": "general"}));
    let page = rect(&h, "settings:rows").expect("the page");
    let close = rect(&h, "settings:back").expect("Close");
    for id in shown_tabs() {
        pick(&mut h, id);
        assert_eq!(open_tab(&h), id, "{id} opened");
        assert_eq!(rect(&h, "settings:rows"), Some(page), "{id}: the page moved or changed size");
        assert_eq!(rect(&h, "settings:back"), Some(close), "{id}: Close moved");
    }
    // Esc closes, and Settings reopens on the last tab at the same size
    pick(&mut h, "performance");
    let r = h.request("ui.key", json!({"key": "Escape"}), T);
    assert_eq!(r["ok"], true, "{r}");
    steps(&mut h, 4);
    assert!(h.app.ui.dialog.is_none(), "Esc closes Settings");
    settings(&mut h, json!({}));
    assert_eq!(open_tab(&h), "performance", "Settings… opens on the tab shown last");
    let r = h.request("ui.clickWidget", json!({"id": "settings:back"}), T);
    assert_eq!(r["ok"], true, "{r}");
    steps(&mut h, 2);
    assert!(h.app.ui.dialog.is_none(), "Close closes");
    let kept = h.app.ui.settings.settings_size;
    // and it opens at that size again: it doesn't grow by its frame each time
    settings(&mut h, json!({}));
    steps(&mut h, 4);
    assert_eq!(h.app.ui.settings.settings_size, kept, "reopened at the size it was left at");
    assert_eq!(rect(&h, "settings:rows"), Some(page), "reopened at the same place and size");
    // the touch screen's section names still open Settings, on the first tab they hold
    settings(&mut h, json!({"tab": "library"}));
    assert_eq!(open_tab(&h), "folders");
    // the Modern layout keeps the full-window Settings with its section rail
    h.app.ui.dialog = None;
    h.app.ui.modern.enabled = true;
    settings(&mut h, json!({"tab": "general"}));
    assert!(rect(&h, "settings:section:general").is_some(), "the section rail");
}

#[test]
fn a_small_window_keeps_the_tabs_and_close_on_screen() {
    let mut h = demo([700.0, 480.0]);
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, h.size);
    for id in shown_tabs() {
        settings(&mut h, json!({"tab": id}));
        assert_eq!(open_tab(&h), id);
        for w in ["settings:rows".to_string(), "settings:back".to_string(), format!("button:settingsTab-{id}")] {
            let r = rect(&h, &w).unwrap_or_else(|| panic!("{id}: no {w}"));
            assert!(screen.contains_rect(r), "{id}: {w} {r:?} runs off the {screen:?} window");
        }
        // the tabs that don't fit are under More, not on a second row
        assert!(rect(&h, "button:settingsTabMore").is_some(), "{id}: More");
    }
}

#[test]
fn window_sizes_and_the_tab_row_fit_the_screen_and_ignore_nonsense() {
    use crate::panels::settings::{DEFAULT_SIZE, MIN_SIZE, window_size};
    let big = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1920.0, 1080.0));
    let fresh = window_size(big, None, 0.0);
    assert_eq!(fresh.default.x, DEFAULT_SIZE[0]);
    // a tall window, capped to the screen
    assert!(fresh.default.y <= 1080.0 && fresh.default.y == fresh.max.y.min(DEFAULT_SIZE[1]), "{fresh:?}");
    assert_eq!(window_size(big, Some([1000.0, 700.0]), 0.0).default, egui::vec2(1000.0, 700.0));
    assert_eq!(window_size(big, Some([f32::NAN, -3.0]), 0.0).default, fresh.default);
    assert_eq!(window_size(big, Some([10.0, 10.0]), 0.0).default, egui::vec2(MIN_SIZE[0], MIN_SIZE[1]));
    let small = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(500.0, 400.0));
    let s = window_size(small, Some([5000.0, 5000.0]), 5000.0);
    assert!(s.default.x <= 500.0 && s.default.y <= 400.0 && s.min.x <= s.max.x && s.min.y <= s.max.y, "{s:?}");
    let nan = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(f32::NAN, f32::INFINITY));
    let s = window_size(nan, None, f32::INFINITY);
    assert!(s.default.x.is_finite() && s.default.y.is_finite(), "{s:?}");
    // tabs that don't fit go under More, the chosen one stays
    {
        use crate::panels::settings::fit_tabs;
        let widths = [80.0; 6];
        assert_eq!(fit_tabs(&widths, 0, 1000.0, 50.0), ((0..6).collect(), vec![]));
        let (shown, hidden) = fit_tabs(&widths, 5, 300.0, 50.0);
        assert!(shown.contains(&5) && !hidden.contains(&5), "{shown:?} {hidden:?}");
        assert_eq!(shown.len() + hidden.len(), 6);
        assert!(shown.iter().map(|i| widths[*i]).sum::<f32>() <= 250.0);
        // no room at all: nothing panics, the chosen tab is still shown
        let (shown, _) = fit_tabs(&widths, 2, 0.0, 50.0);
        assert_eq!(shown, vec![2]);
        assert_eq!(fit_tabs(&[], 0, 100.0, 10.0), (vec![], vec![]));
    }
    // every tab and section name opens a tab
    {
        use crate::panels::settings::window_tab;
        for id in shown_tabs() {
            assert_eq!(window_tab(id), id);
        }
        for section in ["general", "library", "import", "editing", "ai", "display", "sync", "shortcuts", "controls", "slideshow"] {
            assert!(shown_tabs().contains(&window_tab(section)), "{section}");
        }
        assert_eq!(window_tab("nonsense"), "general");
    }
    // the label column gives way in a narrow window
    {
        use crate::dialog_frame::plain_label_width;
        assert_eq!(plain_label_width(170.0, 1200.0), 170.0);
        assert!(plain_label_width(170.0, 400.0) < 170.0);
        assert!(plain_label_width(170.0, 10.0) >= 90.0);
        assert_eq!(plain_label_width(170.0, f32::NAN), 170.0);
    }
    // a window nobody sized is as wide as the tabs; the old default counts as never chosen
    {
        use crate::panels::settings::{DEFAULT_SIZE, window_size};
        let big = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(2560.0, 1600.0));
        assert_eq!(window_size(big, None, 1180.0).default.x, 1180.0);
        assert_eq!(window_size(big, None, 300.0).default.x, DEFAULT_SIZE[0]);
        assert_eq!(window_size(big, Some([900.0, 640.0]), 1180.0).default.x, 1180.0, "the old default is not a choice");
        assert_eq!(window_size(big, Some([1000.0, 700.0]), 1180.0).default, egui::vec2(1000.0, 700.0));
        assert!(window_size(big, None, f32::NAN).default.x.is_finite());
        let laptop = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1366.0, 768.0));
        let s = window_size(laptop, None, 1180.0);
        assert!(s.default.y <= 768.0 && s.default.x <= 1366.0, "fits the screen: {s:?}");
    }
}
