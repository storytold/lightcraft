//! Develop tool profiles (fork-owned): the Edit panel shows the groups and sliders of the profile in use, picked from
//! the row at its head; Settings ▸ Develop makes the user's own; the built-ins never change. Also the panel's head
//! (Browse Profiles in the tool row, Auto · B&W | HDR inside Basic) in the Classic, Compact and Modern layouts.

use std::time::Duration;

use serde_json::{Value, json};

use crate::headless::Headless;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn exec(h: &mut Headless, command: &str, params: Value) -> Value {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    h.settle(SETTLE);
    r
}

fn ok(h: &mut Headless, command: &str, params: Value) -> Value {
    let r = exec(h, command, params);
    assert_eq!(r["ok"], true, "{command}: {r}");
    r["result"].clone()
}

fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    h.settle(SETTLE);
}

fn rect(h: &Headless, id: &str) -> Option<egui::Rect> {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r)
}

fn count(h: &Headless, id: &str) -> usize {
    h.app.widgets.iter().filter(|(w, _)| w == id).count()
}

/// The Edit panel's sections top to bottom.
fn sections(h: &Headless) -> Vec<String> {
    let mut rows: Vec<(f32, String)> =
        h.app.widgets.iter().filter_map(|(w, r)| w.strip_prefix("section:").map(|id| (r.top(), id.to_string()))).collect();
    rows.sort_by(|a, b| a.0.total_cmp(&b.0));
    rows.into_iter().map(|(_, id)| id).collect()
}

/// Develop on the demo library's first photo, Eboda grading mode, every section folded but Basic.
fn develop() -> Headless {
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1300.0, 1600.0], 1.0);
    let r = h.request("ui.set", json!({"module": "develop", "view": "detail", "right": "edit"}), T);
    assert_eq!(r["ok"], true, "{r}");
    ok(&mut h, "view.gradingMode", json!({"mode": "eboda"}));
    h.app.ui.open_sections = vec!["light".into()];
    h.settle(SETTLE);
    h
}

#[test]
fn the_default_profile_shows_every_group_and_the_panel_head_has_no_edit_line() {
    let mut h = develop();
    assert_eq!(h.app.ui.settings.tool_profiles.active().id, crate::tool_profiles::DEFAULT);
    assert_eq!(
        sections(&h),
        ["light", "curve", "mixer", "grading", "detail", "retouch", "optics", "geometry", "effects", "calibration", "pluginFilters"],
        "every group, as the panel always had them"
    );
    // the head: the tool profile row, no profile row, Auto · B&W | HDR inside Basic, Browse Profiles in the tool row
    let menu = rect(&h, "dropdown:toolProfile").expect("the tool profile row");
    let basic = rect(&h, "section:light").expect("Basic");
    assert!(menu.bottom() <= basic.top() + 0.5, "the tool profile row {menu:?} is above Basic {basic:?}");
    assert!(rect(&h, "dropdown:profile").is_none(), "no profile row");
    let auto = rect(&h, "button:auto").expect("Auto");
    assert!(auto.top() >= basic.bottom() - 0.5, "Auto {auto:?} is inside Basic {basic:?}");
    let browse = rect(&h, "icon:profileBrowser").expect("Browse Profiles");
    let crop = rect(&h, "icon:crop").expect("Crop");
    assert!((browse.center().y - crop.center().y).abs() < 1.0, "Browse Profiles {browse:?} is in the tool row with Crop {crop:?}");
    // Browse Profiles opens the browser and a second click puts it away
    click(&mut h, "icon:profileBrowser");
    assert_eq!(h.app.ui.right, crate::state::RightPanel::Profiles);
    click(&mut h, "icon:profileBrowser");
    assert_eq!(h.app.ui.right, crate::state::RightPanel::Edit);
}

#[test]
fn a_profile_hides_groups_sliders_and_options_and_its_custom_group_comes_first_in_order() {
    let mut h = develop();
    let r = ok(&mut h, "view.toolProfileEdit", json!({"op": "new", "name": "Mine"}));
    let id = r["active"].as_str().unwrap().to_string();
    assert!(!crate::tool_profiles::is_builtin(&id), "{r}");
    // a new profile shows everything
    assert!(sections(&h).contains(&"curve".to_string()));
    assert!(rect(&h, "slider:light.exposure").is_some());
    // a group switched off
    ok(&mut h, "view.toolProfileEdit", json!({"op": "group", "group": "curve", "on": false}));
    assert!(!sections(&h).contains(&"curve".to_string()), "{:?}", sections(&h));
    // a slider and an option hidden in their group
    ok(&mut h, "view.toolProfileEdit", json!({"op": "item", "item": "light.highlights", "on": false}));
    ok(&mut h, "view.toolProfileEdit", json!({"op": "item", "item": "option:treatment", "on": false}));
    assert!(rect(&h, "slider:light.highlights").is_none(), "Highlights hidden");
    assert!(rect(&h, "button:auto").is_none(), "Auto · B&W | HDR hidden");
    assert!(rect(&h, "slider:light.shadows").is_some(), "the rest of Basic stays");
    // the Custom group: the chosen sliders in the chosen order, first, and not drawn twice
    ok(&mut h, "view.toolProfileEdit", json!({"op": "custom", "items": ["detail.sharpenAmount", "light.exposure"]}));
    ok(&mut h, "view.toolProfileEdit", json!({"op": "custom", "add": "vignette.amount"}));
    ok(&mut h, "view.toolProfileEdit", json!({"op": "custom", "up": "vignette.amount"}));
    assert_eq!(h.app.ui.settings.tool_profiles.active().custom, ["detail.sharpenAmount", "vignette.amount", "light.exposure"]);
    assert!(h.app.ui.section_open("custom"), "the Custom group opens with its first slider");
    assert_eq!(sections(&h).first().map(String::as_str), Some("custom"), "{:?}", sections(&h));
    let tops: Vec<f32> = ["detail.sharpenAmount", "vignette.amount", "light.exposure"]
        .iter()
        .map(|c| rect(&h, &format!("slider:{c}")).unwrap_or_else(|| panic!("{c}")).top())
        .collect();
    assert!(tops.windows(2).all(|w| w[0] < w[1]), "in the chosen order: {tops:?}");
    let basic = rect(&h, "section:light").unwrap();
    assert!(tops.iter().all(|t| *t < basic.top()), "above Basic");
    assert_eq!(count(&h, "slider:light.exposure"), 1, "Exposure moved to the Custom group, not drawn twice");
    // bad ids are refused, never stored
    assert_ne!(exec(&mut h, "view.toolProfileEdit", json!({"op": "custom", "add": "no.slider"}))["ok"], true);
    assert_ne!(exec(&mut h, "view.toolProfileEdit", json!({"op": "group", "group": "nope", "on": false}))["ok"], true);
    // back to Default: everything again
    ok(&mut h, "view.toolProfile", json!({"id": "default"}));
    assert!(sections(&h).contains(&"curve".to_string()) && !sections(&h).contains(&"custom".to_string()));
    assert!(rect(&h, "slider:light.highlights").is_some() && rect(&h, "button:auto").is_some());
    // the profile is kept, and deleting it falls back to Default
    ok(&mut h, "view.toolProfile", json!({"id": id}));
    ok(&mut h, "view.toolProfileEdit", json!({"op": "delete"}));
    assert_eq!(h.app.ui.settings.tool_profiles.active().id, "default");
    assert!(h.app.ui.settings.tool_profiles.user.is_empty());
}

#[test]
fn the_witchcraft_profile_hides_ebodas_colour_grading_and_brings_the_palettes() {
    let mut h = develop();
    let r = ok(&mut h, "view.toolProfile", json!({"id": "witchcraft"}));
    assert_eq!(r["active"], "witchcraft");
    let s = sections(&h);
    assert!(!s.contains(&"grading".to_string()), "no Color Grading: {s:?}");
    assert!(s.contains(&"primaries".to_string()) && s.contains(&"hsl".to_string()), "the Witchcraft palettes, in Eboda mode too: {s:?}");
    assert!(s.contains(&"mixer".to_string()) && s.contains(&"curve".to_string()), "the other groups stay: {s:?}");
    // the panel's row picks it too
    click(&mut h, "dropdown:toolProfile");
    click(&mut h, "toolProfileMenu:default");
    assert_eq!(h.app.ui.settings.tool_profiles.active().id, "default");
    assert!(sections(&h).contains(&"grading".to_string()));
}

#[test]
fn built_in_profiles_never_change() {
    let mut h = develop();
    for id in ["default", "witchcraft"] {
        for params in [
            json!({"op": "rename", "id": id, "name": "Other"}),
            json!({"op": "delete", "id": id}),
            json!({"op": "group", "id": id, "group": "detail", "on": false}),
            json!({"op": "item", "id": id, "item": "light.exposure", "on": false}),
            json!({"op": "custom", "id": id, "add": "light.exposure"}),
            json!({"op": "witchcraft", "id": id, "on": false}),
        ] {
            let r = exec(&mut h, "view.toolProfileEdit", params.clone());
            assert_ne!(r["ok"], true, "{params}: {r}");
        }
    }
    let p = &h.app.ui.settings.tool_profiles;
    assert!(p.user.is_empty());
    assert_eq!(p.find("witchcraft").map(|w| w.groups_off.clone()), Some(vec!["grading".to_string()]));
    // duplicating a built-in makes a profile of the user's that can change
    let r = ok(&mut h, "view.toolProfileEdit", json!({"op": "duplicate", "id": "witchcraft"}));
    let copy = r["active"].as_str().unwrap().to_string();
    assert!(!crate::tool_profiles::is_builtin(&copy));
    ok(&mut h, "view.toolProfileEdit", json!({"op": "group", "group": "grading", "on": true}));
    ok(&mut h, "view.toolProfileEdit", json!({"op": "rename", "name": "Colourist"}));
    let p = &h.app.ui.settings.tool_profiles;
    assert_eq!(p.active().name, "Colourist");
    assert!(p.active().witchcraft && p.active().groups_off.is_empty());
    assert_eq!(p.find("witchcraft").map(|w| w.groups_off.clone()), Some(vec!["grading".to_string()]), "the built-in is untouched");
    assert_ne!(exec(&mut h, "view.toolProfile", json!({"id": "no-such"}))["ok"], true, "an unknown profile is refused");
}

#[test]
fn stored_profiles_with_unknown_ids_load_and_draw() {
    let stored = json!({"toolProfiles": {"active": "user-1", "user": [
        {"id": "user-1", "name": "Old", "groupsOff": ["curve", "removedGroup"], "hidden": ["light.blacks", "gone.slider", "option:gone"],
         "custom": ["light.exposure", "removed.slider", 3, null]}
    ]}});
    let settings: crate::state::AppSettings = serde_json::from_value(stored).expect("settings load");
    let p = &settings.tool_profiles;
    assert_eq!(p.active().id, "user-1");
    assert_eq!(p.active().groups_off, ["curve"]);
    assert_eq!(p.active().hidden, ["light.blacks"]);
    assert_eq!(p.active().custom, ["light.exposure"]);
    let mut h = develop();
    h.app.ui.settings.tool_profiles = settings.tool_profiles;
    h.settle(SETTLE);
    assert!(rect(&h, "section:custom").is_some() && rect(&h, "section:curve").is_none() && rect(&h, "slider:light.blacks").is_none());
    // garbage instead of profiles reads as Default
    let settings: crate::state::AppSettings = serde_json::from_value(json!({"toolProfiles": "garbage"})).expect("settings load");
    assert_eq!(settings.tool_profiles.active().id, "default");
}

#[test]
fn the_gear_opens_settings_at_develop_where_profiles_are_made() {
    let mut h = develop();
    click(&mut h, "icon:toolProfileSettings");
    match &h.app.ui.dialog {
        Some(crate::state::Dialog::Settings { tab }) => assert_eq!(tab, "develop"),
        other => panic!("Settings ▸ Develop isn't open: {other:?}"),
    }
    // the page: a new profile, then a group switched off from its checkbox
    click(&mut h, "button:toolProfileNew");
    let active = h.app.ui.settings.tool_profiles.active().clone();
    assert!(!active.builtin(), "the new profile is in use");
    click(&mut h, "check:toolGroup:effects");
    assert!(!h.app.ui.settings.tool_profiles.active().shows_group("effects"), "Effects switched off");
    click(&mut h, "button:toolProfileDelete");
    assert_eq!(h.app.ui.settings.tool_profiles.active().id, "default");
}

#[test]
fn compact_and_modern_layouts_keep_the_tool_row_and_the_profile_browser() {
    let mut h = develop();
    ok(&mut h, "view.compactLayout", json!({"on": true}));
    for id in ["icon:profileBrowser", "dropdown:toolProfile", "icon:toolProfileSettings", "button:auto", "icon:crop"] {
        assert!(rect(&h, id).is_some(), "Compact: {id}");
    }
    let basic = rect(&h, "section:light").unwrap();
    assert!(rect(&h, "button:auto").unwrap().top() >= basic.bottom() - 0.5, "Compact: Auto inside Basic");
    ok(&mut h, "view.compactLayout", json!({"on": false}));
    ok(&mut h, "view.modernLayout", json!({"on": true}));
    ok(&mut h, "modern.group", json!({"group": "basic"}));
    click(&mut h, "modern:more:profile");
    assert!(rect(&h, "icon:profileBrowser").is_some(), "Modern: Profile & Treatment offers Browse Profiles");
    assert!(rect(&h, "button:auto").is_some(), "Modern: and the treatment buttons");
}
