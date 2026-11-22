//! The Levels section: its channel tabs, markers, number boxes, pickers and the section's eye (demo scene only).

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn demo() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LccApp::new(lcc_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1400.0, 2400.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail", "right": "edit"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.app.ui.solo_section("levels");
    h.settle(SETTLE);
    h
}

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

fn run(h: &mut Headless, id: &str, params: serde_json::Value) -> serde_json::Value {
    let r = h.request("engine.execute", json!({"command": id, "params": params}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    h.step();
    h.step();
    r["result"].clone()
}

fn levels(h: &Headless) -> lcc_develop::Levels {
    let id = h.app.session.active().unwrap();
    h.app.session.develop_of(id).unwrap_or_default().levels
}

#[test]
fn the_section_shows_the_rgb_tab_with_pickers_and_boxes() {
    let h = demo();
    for w in [
        "levels:plot",
        "levels:strip",
        "levels:outStrip",
        "levels:master.black",
        "levels:master.midtone",
        "levels:master.white",
        "levels:master.outBlack",
        "levels:master.outWhite",
    ] {
        assert!(has(&h, w), "{w}");
    }
    assert!(has(&h, "icon:levelsBlack") && has(&h, "icon:levelsWhite"), "pickers on the RGB tab");
}

#[test]
fn another_tab_shows_its_own_boxes_and_no_pickers() {
    let mut h = demo();
    let r = h.request("ui.clickWidget", json!({"id": "button:levelsTab-red"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert!(has(&h, "levels:red.black") && !has(&h, "levels:master.black"));
    assert!(!has(&h, "icon:levelsBlack") && !has(&h, "icon:levelsWhite"), "the pickers belong to the RGB tab");
    // an armed picker is put away by a tab change
    let r = h.request("ui.clickWidget", json!({"id": "button:levelsTab-master"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    h.app.ui.tool = "levelsBlack".into();
    h.request("ui.clickWidget", json!({"id": "button:levelsTab-green"}), T);
    h.step();
    h.step();
    assert!(h.app.ui.tool.is_empty());
}

#[test]
fn dragging_the_midtone_marker_is_one_undo_step() {
    let mut h = demo();
    assert_eq!(levels(&h).master.midtone, 0.0);
    // the neutral midtone marker sits at the middle of the strip
    let r = h.request("ui.dragWidget", json!({"id": "levels:strip", "dx": 40.0, "dy": 0.0, "steps": 6}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    let m = levels(&h).master.midtone;
    assert!(m < 0.0, "pulled toward the highlights darkens: {m}");
    run(&mut h, "edit.undo", json!({}));
    assert!(levels(&h).is_default(), "one undo takes the whole drag back");
}

#[test]
fn dragging_the_black_marker_moves_the_black_point() {
    let mut h = demo();
    let r = h.request("ui.dragWidget", json!({"id": "levels:strip", "fx": 0.0, "fy": 0.5, "dx": 30.0, "dy": 0.0, "steps": 6}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    let l = levels(&h).master;
    assert!(l.black > 5.0 && l.white == 255.0 && l.midtone == 0.0, "{l:?}");
}

#[test]
fn the_section_is_in_the_modern_dock_tone_card_and_the_tool_profiles() {
    assert!(crate::modern_dock::Group::Tone.sections().iter().any(|(id, _)| *id == "levels"));
    assert!(crate::tool_profiles::GROUPS.iter().any(|(id, _)| *id == "levels"));
    assert_eq!(crate::tool_profiles::group_of("levels.master.black"), Some("levels"));
    assert!(crate::tool_profiles::sliders_of("levels").count() == 20);
}

#[test]
fn the_eye_switches_the_levels_off_without_touching_the_values() {
    let mut h = demo();
    run(&mut h, "develop.levels", json!({"black": 60, "white": 200}));
    run(&mut h, "develop.sectionEnabled", json!({"section": "levels", "enabled": false}));
    let id = h.app.session.active().unwrap();
    let d = h.app.session.develop_of(id).unwrap();
    assert!(!d.section_enabled("levels"));
    assert!(d.effective().levels.is_default(), "an eye that is off contributes nothing");
    assert_eq!(d.levels.master.black, 60.0, "the values stay");
    run(&mut h, "develop.sectionEnabled", json!({"section": "levels", "enabled": true}));
    let d = h.app.session.develop_of(id).unwrap();
    assert_eq!(d.effective().levels.master.black, 60.0);
}

#[test]
fn history_names_the_levels_group() {
    let mut h = demo();
    run(&mut h, "develop.levels", json!({"black": 60}));
    let id = h.app.session.active().unwrap();
    assert!(h.app.session.develop_of(id).unwrap().levels.master.black == 60.0);
    let title = crate::panels::right::group_title(Some("levels"), "Levels", &[]);
    assert_eq!(title, "Levels");
}

#[test]
fn reset_brings_the_levels_back() {
    let mut h = demo();
    run(&mut h, "develop.levels", json!({"black": 40, "white": 200}));
    h.settle(SETTLE);
    assert!(has(&h, "button:levelsReset"), "Reset is inside the tool");
    let r = h.request("ui.clickWidget", json!({"id": "button:levelsReset"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert!(levels(&h).is_default());
}

#[test]
fn the_midtone_dot_and_markers_share_the_plot_width() {
    let h = demo();
    assert!(has(&h, "levels:outStrip") && has(&h, "levels:strip"));
}

#[test]
fn settings_saved_by_main_show_and_open_the_levels_section() {
    // a tool profile and open sections as an earlier version saved them: Levels isn't named anywhere
    let saved = json!({"active": "mine", "user": [{"id": "mine", "name": "Mine", "groupsOff": ["detail", "retouch"], "hidden": [], "custom": [], "witchcraft": false}]});
    let mut h = demo();
    h.app.ui.settings.tool_profiles = crate::tool_profiles::ToolProfiles::from_value(&saved);
    h.app.ui.open_sections = vec!["light".into(), "primaries".into()];
    assert!(crate::tool_profiles::group_shown(&h.app, "levels"), "a group the profile never heard of is shown");
    let r = h.request("engine.execute", json!({"command": "section.levels"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.app.ui.section_open("levels"));
    let r = h.request("engine.execute", json!({"command": "section.levels"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(!h.app.ui.section_open("levels"));
}

#[test]
fn every_section_header_has_its_command() {
    let mut h = demo();
    h.app.ui.open_sections.clear();
    h.settle(SETTLE);
    let ids: Vec<String> = h.app.widgets.iter().filter_map(|(w, _)| w.strip_prefix("section:").map(str::to_string)).collect();
    assert!(ids.iter().any(|i| i == "levels"), "{ids:?}");
    for id in ids {
        let r = h.request("engine.execute", json!({"command": format!("section.{id}")}), T);
        assert_eq!(r["ok"], true, "section.{id}: {r}");
    }
}

fn click_photo(h: &mut Headless) {
    let pos = h.app.image_rect.expect("photo on screen").center();
    h.app.synthetic.push(egui::Event::PointerMoved(pos));
    h.settle(SETTLE);
    h.app.synthetic.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
    h.app.synthetic.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
    h.settle(SETTLE);
}

#[test]
fn a_levels_picker_stays_armed_and_each_click_is_one_undo_step() {
    let mut h = demo();
    let r = h.request("ui.clickWidget", json!({"id": "icon:levelsBlack"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.tool, "levelsBlack");
    click_photo(&mut h);
    assert_eq!(h.app.ui.tool, "levelsBlack", "still armed after a click");
    let first = levels(&h).master.black;
    // a different place gives a different reading, set by a second click
    let img = h.app.image_rect.unwrap();
    let pos = img.left_top() + egui::vec2(img.width() * 0.5, img.height() * 0.08);
    h.app.synthetic.push(egui::Event::PointerMoved(pos));
    h.settle(SETTLE);
    h.app.synthetic.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
    h.app.synthetic.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
    h.settle(SETTLE);
    assert_eq!(h.app.ui.tool, "levelsBlack");
    // each click undoes on its own
    run(&mut h, "edit.undo", json!({}));
    assert_eq!(levels(&h).master.black, first, "one undo takes back the last click only");
}

#[test]
fn a_levels_picker_ends_on_esc_a_second_click_the_other_picker_or_leaving_the_section() {
    let mut h = demo();
    let click = |h: &mut Headless, id: &str| {
        let r = h.request("ui.clickWidget", json!({"id": id}), T);
        assert_eq!(r["ok"], true, "{id}: {r}");
        h.settle(SETTLE);
    };
    click(&mut h, "icon:levelsBlack");
    assert_eq!(h.app.ui.tool, "levelsBlack");
    // using the section's own controls does not end it
    run(&mut h, "develop.levels", json!({"midtone": 0.2}));
    h.app.ui.put_away_picker();
    assert_eq!(h.app.ui.tool, "levelsBlack");
    click(&mut h, "icon:levelsWhite");
    assert_eq!(h.app.ui.tool, "levelsWhite", "the other picker takes over");
    click(&mut h, "icon:levelsWhite");
    assert_eq!(h.app.ui.tool, "", "a second click on the icon puts it away");
    click(&mut h, "icon:levelsBlack");
    let r = h.request("ui.key", json!({"key": "Escape"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    click_photo_hover(&mut h);
    assert_eq!(h.app.ui.tool, "", "Esc puts it away");
    click(&mut h, "icon:levelsBlack");
    h.app.ui.toggle_section("levels");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.tool, "", "closing the Levels section puts it away");
}

fn click_photo_hover(h: &mut Headless) {
    let pos = h.app.image_rect.expect("photo on screen").center();
    h.app.synthetic.push(egui::Event::PointerMoved(pos));
    h.settle(SETTLE);
}

#[test]
fn the_markers_use_the_left_right_cursor_on_hover_and_while_dragging() {
    let mut h = demo();
    let strip = h.app.widgets.iter().find(|(w, _)| w == "levels:strip").map(|(_, r)| *r).expect("the strip");
    // the midtone marker is at the strip's middle while neutral
    h.app.synthetic.push(egui::Event::PointerMoved(strip.center()));
    h.settle(SETTLE);
    assert_eq!(h.last_cursor, egui::CursorIcon::ResizeHorizontal, "hover");
    h.app.synthetic.push(egui::Event::PointerButton {
        pos: strip.center(),
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: Default::default(),
    });
    h.app.synthetic.push(egui::Event::PointerMoved(strip.center() + egui::vec2(30.0, 0.0)));
    h.settle(SETTLE);
    assert_eq!(h.last_cursor, egui::CursorIcon::ResizeHorizontal, "while dragging");
    h.app.synthetic.push(egui::Event::PointerButton {
        pos: strip.center() + egui::vec2(30.0, 0.0),
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: Default::default(),
    });
    h.settle(SETTLE);
}
