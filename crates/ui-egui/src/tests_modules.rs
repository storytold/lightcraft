//! Headless tests of the Classic module shell (P1.2): module switching keeps the selection and the
//! filmstrip, panel state is per module and survives a save/load of `ui.json`, every module draws,
//! and the panel, screen-mode and lights-out commands work.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::module::{Edge, LightsOut, ModuleId, ScreenMode};
use crate::state::{RightPanel, ViewMode};
use crate::{DacApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn demo() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = DacApp::new(dac_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    h.settle(SETTLE);
    run(&mut h, "module.library", json!({}));
    h
}

fn run(h: &mut Headless, id: &str, params: serde_json::Value) -> serde_json::Value {
    let r = h.request("engine.execute", json!({"command": id, "params": params}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    h.step();
    r["result"].clone()
}

fn has_widget(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

#[test]
fn switching_modules_keeps_selection_and_filmstrip() {
    let mut h = demo();
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(2).map(|p| p.0).collect();
    run(&mut h, "library.select", json!({"ids": ids}));
    let film = h.app.ui.filmstrip;
    assert_eq!(h.app.ui.module, ModuleId::Library);
    for m in ["develop", "map", "book", "slideshow", "print", "web", "library", "develop"] {
        run(&mut h, "module.switch", json!({"module": m}));
        assert_eq!(h.app.ui.module.key(), m);
        let sel: Vec<u64> = h.app.session.selection.ids.iter().map(|p| p.0).collect();
        assert_eq!(sel.len(), 2, "{m}: the selection stays");
        assert_eq!(h.app.ui.filmstrip, film, "{m}: the filmstrip stays");
        assert!(has_widget(&h, &format!("module:{m}")), "{m} is in the picker");
    }
    // Develop is the loupe with the Edit panel
    assert_eq!((h.app.ui.view, h.app.ui.right), (ViewMode::Detail, RightPanel::Edit));
    // ⌘⌥↑ goes back to the previous module
    run(&mut h, "module.previous", json!({}));
    assert_eq!(h.app.ui.module, ModuleId::Library);
    assert!(!h.app.ui.right.is_edit_tool());
    // a placeholder module says so
    run(&mut h, "module.map", json!({}));
    assert!(has_widget(&h, "view:module:map"));
    // a view command leaves it for the module the view belongs to
    run(&mut h, "view.photoGrid", json!({}));
    assert_eq!(h.app.ui.module, ModuleId::Library);
    // opening an editing panel from Library is entering Develop
    run(&mut h, "panel.crop", json!({}));
    assert_eq!(h.app.ui.module, ModuleId::Develop);
    // unknown modules are errors, not panics
    let r = h.request("engine.execute", json!({"command": "module.switch", "params": {"module": "darkroom"}}), T);
    assert_eq!(r["ok"], false, "{r}");
}

#[test]
fn panel_state_is_per_module_and_persists() {
    let mut h = demo();
    run(&mut h, "panel.left", json!({"show": true}));
    run(&mut h, "panel.order", json!({"order": ["activity", "info"]}));
    run(&mut h, "module.develop", json!({}));
    // Develop starts from what was on screen; hide its left and right groups and the toolbar
    run(&mut h, "panel.sides", json!({"show": false}));
    run(&mut h, "panel.toolbar", json!({}));
    run(&mut h, "panel.autoShow", json!({"edge": "left", "on": true}));
    assert!(!h.app.ui.left_panel && !h.app.ui.right_edge && !h.app.ui.toolbar);
    assert!(!has_widget(&h, "panel:left_panel"));
    run(&mut h, "module.library", json!({}));
    assert!(h.app.ui.left_panel && h.app.ui.right_edge && h.app.ui.toolbar, "Library keeps its own panels");
    assert_eq!(crate::module::right_group(&h.app).first(), Some(&crate::module::PanelId::Activity), "panel order is the user's");
    run(&mut h, "module.develop", json!({}));
    assert!(!h.app.ui.left_panel && !h.app.ui.right_edge && h.app.ui.auto_show.left);
    // across a save/load of ui.json
    let saved = serde_json::to_string(&h.app.ui).unwrap();
    let back = serde_json::from_str::<crate::UiState>(&saved).unwrap().sanitized();
    assert_eq!(back.module, ModuleId::Develop);
    assert_eq!(back.layouts.get(&ModuleId::Library).map(|l| (l.shown.left, l.toolbar)), Some((true, true)));
    assert!(!back.left_panel && back.auto_show.left);
    // auto show: the pointer at the left edge brings the hidden left panel back for a while
    h.request("ui.move", json!({"x": 1.0, "y": 450.0}), T);
    h.step();
    h.step();
    assert!(h.app.ui.peek.get(Edge::Left), "peeking");
    assert!(has_widget(&h, "panel:left_panel"));
    h.request("ui.move", json!({"x": 900.0, "y": 450.0}), T);
    h.step();
    h.step();
    assert!(!h.app.ui.peek.get(Edge::Left) && !has_widget(&h, "panel:left_panel"));
}

#[test]
fn every_module_draws_and_the_picker_hides_modules() {
    let mut h = demo();
    for m in ModuleId::ALL {
        run(&mut h, m.command(), json!({}));
        h.settle(SETTLE);
        let img = h.snapshot(T);
        assert!(img.width() > 0 && img.height() > 0);
        assert!(has_widget(&h, "region:moduleBar") && has_widget(&h, "region:identityPlate"), "{m:?}");
        if m.is_placeholder() {
            assert!(has_widget(&h, &format!("view:module:{}", m.key())));
        }
    }
    run(&mut h, "module.library", json!({}));
    run(&mut h, "module.setVisible", json!({"module": "web", "visible": false}));
    assert!(!has_widget(&h, "module:web"));
    // the module in use can't be hidden
    let r = h.request("engine.execute", json!({"command": "module.setVisible", "params": {"module": "library", "visible": false}}), T);
    assert_eq!(r["ok"], false, "{r}");
    run(&mut h, "module.setVisible", json!({"module": "web"}));
    assert!(has_widget(&h, "module:web"));
    // identity plate text, capped
    run(&mut h, "view.identityPlate", json!({"text": "x".repeat(500), "mark": false}));
    assert_eq!(h.app.ui.identity_plate.text.chars().count(), 80);
    // F5: the module bar goes
    run(&mut h, "panel.top", json!({}));
    assert!(!has_widget(&h, "region:moduleBar"));
}

#[test]
fn screen_modes_and_lights_out() {
    let mut h = demo();
    let r = run(&mut h, "view.screenMode", json!({}));
    assert_eq!(r["screenMode"], "fullScreenMenu");
    assert_eq!(h.app.ui.screen_mode, ScreenMode::FullScreenMenu);
    run(&mut h, "view.screenMode", json!({"mode": "fullScreenHidePanels"}));
    assert!(!has_widget(&h, "region:moduleBar") && !has_widget(&h, "panel:left_panel"));
    run(&mut h, "view.screenModeNormal", json!({}));
    assert_eq!(h.app.ui.screen_mode, ScreenMode::Normal);
    let r = h.request("engine.execute", json!({"command": "view.screenMode", "params": {"mode": "cinema"}}), T);
    assert_eq!(r["ok"], false);
    for want in [LightsOut::Dim, LightsOut::Black, LightsOut::Off] {
        run(&mut h, "view.lightsOut", json!({}));
        assert_eq!(h.app.ui.lights_out, want);
    }
    run(&mut h, "view.lightsOut", json!({"mode": "black"}));
    let img = h.snapshot(T);
    // the corner (chrome) is black
    assert_eq!(img.pixels.first().map(|c| (c.r(), c.g(), c.b())), Some((0, 0, 0)));
}

#[test]
fn rating_steps_and_flag_toggle() {
    let mut h = demo();
    let id = h.app.session.visible_cloned()[0];
    run(&mut h, "library.select", json!({"ids": [id.0]}));
    run(&mut h, "photo.rate", json!({"rating": 2}));
    run(&mut h, "photo.ratingUp", json!({}));
    assert_eq!(h.app.session.catalog.photo(id).unwrap().rating, 3);
    for _ in 0..5 {
        run(&mut h, "photo.ratingDown", json!({}));
    }
    assert_eq!(h.app.session.catalog.photo(id).unwrap().rating, 0);
    run(&mut h, "photo.flagToggle", json!({}));
    assert_eq!(h.app.session.catalog.photo(id).unwrap().flag, dac_catalog::Flag::Pick);
    run(&mut h, "photo.flagToggle", json!({}));
    assert_eq!(h.app.session.catalog.photo(id).unwrap().flag, dac_catalog::Flag::None);
}

#[test]
fn secondary_window_modes() {
    let mut h = demo();
    let id = h.app.session.visible_cloned()[1];
    run(&mut h, "library.select", json!({"ids": [id.0]}));
    let r = run(&mut h, "second.locked", json!({}));
    assert_eq!(r["locked"], id.0);
    assert!(h.app.ui.second_window);
    for m in ["second.grid", "second.loupe", "second.live", "second.compare", "second.survey", "second.slideshow"] {
        run(&mut h, m, json!({}));
        h.step();
        assert!(has_widget(&h, "view:secondWindow"), "{m}");
    }
}
