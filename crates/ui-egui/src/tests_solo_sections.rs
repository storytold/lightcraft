//! Headless tests of Solo Mode: opening one Edit section closes the others (Lightroom Classic's
//! panel Solo Mode), from the Window menu command, the section header's menu and ⌥-click.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

/// Detail view with the Edit panel open and Light (the default) expanded.
fn edit_open() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1200.0, 1400.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.app.ui.right = crate::state::RightPanel::Edit;
    h.app.ui.open_sections = vec!["light".into()];
    h.step();
    h.step();
    h
}

fn exec(h: &mut Headless, command: &str, params: serde_json::Value) -> serde_json::Value {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
    r["result"].clone()
}

/// A real click through the control channel: `{id, button?, alt?}` as `ui.clickWidget` takes them.
fn click(h: &mut Headless, params: serde_json::Value) {
    let r = h.request("ui.clickWidget", params.clone(), T);
    assert_eq!(r["ok"], true, "click {params}: {r}");
    h.step();
}

#[test]
fn solo_mode_closes_the_other_sections_when_one_opens() {
    let mut h = edit_open();
    click(&mut h, json!({"id": "section:color"}));
    assert_eq!(h.app.ui.open_sections, ["light", "color"], "off: sections open side by side");

    assert_eq!(exec(&mut h, "view.soloSections", json!({})), json!({"soloSections": true}));
    assert_eq!(h.app.ui.open_sections, ["color"], "turning it on keeps the section opened last");
    assert_eq!(crate::menubar::checked(&h.app, "view.soloSections"), Some(true));
    h.step();

    click(&mut h, json!({"id": "section:detail"}));
    assert_eq!(h.app.ui.open_sections, ["detail"]);
    // the Window > Edit Sections shortcuts follow it too
    exec(&mut h, "section.optics", json!({}));
    assert_eq!(h.app.ui.open_sections, ["optics"]);
    // closing the open one leaves none open
    exec(&mut h, "section.optics", json!({}));
    assert!(h.app.ui.open_sections.is_empty());

    assert_eq!(exec(&mut h, "view.soloSections", json!({"on": false})), json!({"soloSections": false}));
    exec(&mut h, "section.light", json!({}));
    exec(&mut h, "section.color", json!({}));
    assert_eq!(h.app.ui.open_sections, ["light", "color"]);
}

#[test]
fn the_section_header_menu_turns_solo_mode_on() {
    let mut h = edit_open();
    click(&mut h, json!({"id": "section:color"}));
    click(&mut h, json!({"id": "section:effects", "button": "right"}));
    click(&mut h, json!({"id": "sectionMenu:solo"}));
    assert!(h.app.ui.single_panel, "the header's Solo Mode item turns it on");
    assert_eq!(h.app.ui.open_sections, ["color"]);
}

#[test]
fn option_click_opens_just_that_section() {
    let mut h = edit_open();
    click(&mut h, json!({"id": "section:color"}));
    click(&mut h, json!({"id": "section:detail", "alt": true}));
    assert_eq!(h.app.ui.open_sections, ["detail"], "⌥-click closes the others");
    assert!(!h.app.ui.single_panel, "⌥-click doesn't turn Solo Mode on");
    // agents do the same with the section's command (Calibration has one, though not in the menu)
    exec(&mut h, "section.calibration", json!({"solo": true}));
    assert_eq!(h.app.ui.open_sections, ["calibration"]);
}

#[test]
fn negative_craft_stays_hidden_until_the_edit_menu_enables_it() {
    let mut h = edit_open();
    let widgets = |h: &mut Headless| h.request("ui.widgets", json!({}), T).to_string();
    h.step();
    let w = widgets(&mut h);
    assert!(!w.contains("\"section:film\""), "Negative Craft is off until the menu enables it: {w}");
    assert!(w.contains("\"section:light\""));
    assert_eq!(crate::menubar::checked(&h.app, "section.film"), Some(false));

    exec(&mut h, "section.film", json!({}));
    h.step();
    h.step();
    assert!(h.app.ui.negative_craft);
    assert!(h.app.ui.section_open("film"));
    assert_eq!(crate::menubar::checked(&h.app, "section.film"), Some(true));
    assert!(widgets(&mut h).contains("\"section:film\""), "the section sits above Light once enabled");

    exec(&mut h, "section.film", json!({"solo": true}));
    assert!(h.app.ui.negative_craft, "a header click folds the section and leaves Negative Craft on");
    assert!(h.app.ui.section_open("film"));

    exec(&mut h, "section.film", json!({}));
    h.step();
    assert!(!h.app.ui.negative_craft, "the menu item unchecks Negative Craft");
    assert!(!h.app.ui.section_open("film"));
    assert_eq!(crate::menubar::checked(&h.app, "section.film"), Some(false));
    assert!(!widgets(&mut h).contains("\"section:film\""), "unchecking the menu takes the section off an unconverted photo");

    exec(&mut h, "section.film", json!({}));
    h.step();
    assert!(h.app.ui.negative_craft);

    click(&mut h, json!({"id": "section:film", "button": "right"}));
    click(&mut h, json!({"id": "sectionMenu:hideFilm"}));
    h.step();
    assert!(!h.app.ui.negative_craft);
    assert!(!widgets(&mut h).contains("\"section:film\""), "Hide Negative Craft takes the section off photos that are not converted");

    exec(&mut h, "develop.merge", json!({"settings": {"film": {"enabled": true}}, "label": "Negative Craft"}));
    h.step();
    h.step();
    assert!(!h.app.ui.negative_craft, "a converted photo does not turn the panel on for every other photo");
    assert!(widgets(&mut h).contains("\"section:film\""), "a photo that is already converted still shows Negative Craft");
}

#[test]
fn bad_flags_and_unknown_sections_are_refused() {
    let mut h = edit_open();
    for (command, params) in
        [("view.soloSections", json!({"on": "yes"})), ("section.color", json!({"solo": 1})), ("section.typo", json!({"solo": true}))]
    {
        let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
        assert_eq!(r["ok"], false, "{command} {params}: {r}");
    }
    assert!(!h.app.ui.single_panel);
    assert_eq!(h.app.ui.open_sections, ["light"]);
}
