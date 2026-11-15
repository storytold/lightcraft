//! Settings ▸ General ▸ Settings Categories: the Settings window of the Classic and Compact
//! layouts lists its categories down its left side or along its top, by default Left in Classic
//! and Top in Compact; Modern has no choice (its full-window sheet always lists them on the left).
//! Settings ▸ Interface shows only the slider settings that apply to the layout drawn, and What's
//! New is wide enough to read.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::{Dialog, SettingsCategories};
use crate::theme::SliderLayout;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);

#[derive(Clone, Copy, PartialEq)]
enum Layout {
    Classic,
    Compact,
    Modern,
}

fn app(layout: Layout) -> LccApp {
    let services = Services { png: None, ..Default::default() };
    let mut app = LccApp::new(lcc_engine::Session::new(), services);
    app.ui.modern.enabled = layout == Layout::Modern;
    app.ui.settings.compact_layout = layout == Layout::Compact;
    app
}

fn steps(h: &mut Headless, n: usize) {
    for _ in 0..n {
        h.step();
    }
}

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

fn settings(h: &mut Headless, params: serde_json::Value) {
    let r = h.request("engine.execute", json!({"command": "app.settings", "params": params}), T);
    assert_eq!(r["ok"], true, "{r}");
    // (the first request switches the widget list on; a new window sizes itself on its first frames)
    h.request("ui.widgets", json!({}), T);
    steps(h, 6);
}

/// Opens Settings on `tab` in `layout` on a desktop-sized window.
fn open(layout: Layout, tab: &str, prepare: impl FnOnce(&mut LccApp)) -> Headless {
    let mut app = app(layout);
    prepare(&mut app);
    let mut h = Headless::new(app, [1600.0, 1400.0], 1.0);
    settings(&mut h, json!({"tab": tab}));
    h
}

/// The categories are listed down the left (the list's buttons are `settings:section:{id}`).
fn listed_left(h: &Headless) -> bool {
    has(h, "settings:section:general") && has(h, "settings:section:interface")
}

#[test]
fn categories_follow_each_layout_s_default() {
    let classic = open(Layout::Classic, "general", |_| {});
    assert!(listed_left(&classic), "Classic: down the left");
    assert!(has(&classic, "button:settingsTab-interface"), "the list answers to the tab row's ids too");
    assert!(has(&classic, "button:settingsCategories-0"), "Classic offers the choice");
    let compact = open(Layout::Compact, "general", |_| {});
    assert!(!listed_left(&compact), "Compact: along the top");
    assert!(has(&compact, "button:settingsTab-interface"), "one row of tabs");
    assert!(has(&compact, "button:settingsCategories-1"), "Compact offers the choice");
}

#[test]
fn the_choice_moves_the_categories_and_each_layout_keeps_its_own() {
    let mut h = open(Layout::Classic, "general", |_| {});
    assert!(listed_left(&h));
    // Top
    let r = h.request("ui.clickWidget", json!({"id": "button:settingsCategories-1"}), T);
    assert_eq!(r["ok"], true, "{r}");
    steps(&mut h, 6);
    assert_eq!(h.app.ui.settings.settings_categories_classic, Some(SettingsCategories::Top));
    assert!(!listed_left(&h), "the tabs moved to the top");
    assert!(matches!(&h.app.ui.dialog, Some(Dialog::Settings { tab }) if tab == "general"), "on the same category");
    assert_eq!(h.app.ui.settings.settings_categories_compact, None, "Compact keeps its own");
    // and back to Left
    let r = h.request("ui.clickWidget", json!({"id": "button:settingsCategories-0"}), T);
    assert_eq!(r["ok"], true, "{r}");
    steps(&mut h, 6);
    assert!(listed_left(&h), "the list is back");
    // saved with the UI state (ui.json)
    let saved = serde_json::to_string(&h.app.ui).unwrap();
    let back: crate::state::UiState = serde_json::from_str(&saved).unwrap();
    assert_eq!(back.settings.settings_categories_classic, Some(SettingsCategories::Left));
    // Compact chosen Left lists them down the left too
    let compact = open(Layout::Compact, "general", |a| a.ui.settings.settings_categories_compact = Some(SettingsCategories::Left));
    assert!(listed_left(&compact), "Compact with Left");
}

#[test]
fn a_category_picked_in_the_list_opens_and_ctrl_tab_steps() {
    let mut h = open(Layout::Classic, "general", |_| {});
    let r = h.request("ui.clickWidget", json!({"id": "settings:section:display"}), T);
    assert_eq!(r["ok"], true, "{r}");
    steps(&mut h, 4);
    assert!(matches!(&h.app.ui.dialog, Some(Dialog::Settings { tab }) if tab == "display"));
    h.request("ui.key", json!({"key": "Tab", "ctrl": true, "cmd": !cfg!(target_os = "macos")}), T);
    steps(&mut h, 4);
    assert!(matches!(&h.app.ui.dialog, Some(Dialog::Settings { tab }) if tab != "display"), "Ctrl+Tab moves on");
    // Esc closes it, as the tabbed window
    h.request("ui.key", json!({"key": "Escape"}), T);
    steps(&mut h, 4);
    assert!(h.app.ui.dialog.is_none());
}

#[test]
fn modern_offers_no_choice() {
    let h = open(Layout::Modern, "general", |a| a.ui.settings.settings_categories_classic = Some(SettingsCategories::Top));
    assert!(listed_left(&h), "the sheet's rail");
    assert!(has(&h, "button:settingsLayout-0"), "the General rows show");
    assert!(!has(&h, "button:settingsCategories-0") && !has(&h, "button:settingsCategories-1"), "no Settings Categories row");
}

#[test]
fn settings_opens_at_a_page() {
    let mut h = Headless::new(app(Layout::Compact), [1600.0, 1400.0], 1.0);
    settings(&mut h, json!({"page": "interface"}));
    assert!(matches!(&h.app.ui.dialog, Some(Dialog::Settings { tab }) if tab == "interface"));
    let r = h.request("engine.execute", json!({"command": "app.settings", "params": {"page": "nonsense"}}), T);
    assert_ne!(r["ok"], true, "an unknown page is refused: {r}");
}

/// The slider rows Settings ▸ Interface shows: (Sliders, Slider text, Slider control).
fn slider_rows(layout: Layout, chosen: SliderLayout) -> (bool, bool, bool) {
    let h = open(layout, "interface", |a| a.ui.settings.slider_layout = chosen);
    (has(&h, "button:settingsSliderLayout-0"), has(&h, "button:settingsSliderText-0"), has(&h, "button:settingsSliderControl-0"))
}

#[test]
fn only_the_slider_settings_that_apply_show() {
    // Classic: the slider layout is chosen here; acceleration needs a thumb, which a pill has not
    assert_eq!(slider_rows(Layout::Classic, SliderLayout::OneRow), (true, true, true));
    assert_eq!(slider_rows(Layout::Classic, SliderLayout::TwoRows), (true, true, true));
    assert_eq!(slider_rows(Layout::Classic, SliderLayout::Pill), (true, true, false));
    // Compact always draws one dense row: no layout choice, acceleration applies
    assert_eq!(slider_rows(Layout::Compact, SliderLayout::Pill), (false, true, true));
    // Modern always draws pills: neither
    assert_eq!(slider_rows(Layout::Modern, SliderLayout::OneRow), (false, true, false));
}

#[test]
fn slider_rows_follow_the_layout_drawn() {
    use crate::panels::settings::SliderRows;
    let rows = |forced, chosen| SliderRows::for_layout(forced, chosen);
    assert_eq!(rows(None, SliderLayout::OneRow), SliderRows { layout: true, control: true });
    assert_eq!(rows(None, SliderLayout::Pill), SliderRows { layout: true, control: false });
    assert_eq!(rows(Some(SliderLayout::OneRow), SliderLayout::Pill), SliderRows { layout: false, control: true });
    assert_eq!(rows(Some(SliderLayout::Pill), SliderLayout::OneRow), SliderRows { layout: false, control: false });
}

#[test]
fn whats_new_is_three_dialogs_wide_unless_the_window_is_narrower() {
    use crate::panels::dialogs::whats_new_width;
    let margin = crate::dialog_frame::SCREEN_MARGIN;
    assert!(whats_new_width(2400.0) >= 3.0 * 380.0, "three times a dialog's width");
    assert!(whats_new_width(2400.0) <= 2400.0 - 2.0 * margin);
    assert_eq!(whats_new_width(900.0), 900.0 - 2.0 * margin, "a narrower window: its width less the margins");
    assert!(whats_new_width(1300.0) > whats_new_width(900.0), "it grows with the window up to its width");
    for hostile in [0.0, -10.0, f32::NAN, f32::INFINITY] {
        let w = whats_new_width(hostile);
        assert!(w.is_finite() && w > 0.0, "{hostile}: {w}");
    }
    // in the app: the dialog is drawn that wide, and inside the window
    for size in [[1800.0, 1000.0], [900.0, 800.0]] {
        let mut h = Headless::new(app(Layout::Classic), size, 1.0);
        h.request("ui.widgets", json!({}), T);
        h.app.ui.dialog = Some(Dialog::WhatsNew);
        steps(&mut h, 8);
        let w = h.app.widgets.iter().rev().find(|(id, _)| id == "dialog:window").map(|(_, r)| *r).expect("What's New is drawn");
        let want = whats_new_width(size[0]);
        assert!(w.width() >= want - 1.0, "{size:?}: {} wide, wants {want}", w.width());
        assert!(w.left() >= 0.0 && w.right() <= size[0], "{size:?}: inside the window: {w:?}");
    }
}
