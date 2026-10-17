//! Headless tests of the Classic module shell (P1.2): module switching keeps the selection and the
//! filmstrip, panel state is per module and survives a save/load of `ui.json`, every module draws,
//! and the panel, screen-mode and lights-out commands work.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::module::{Edge, LightsOut, ModuleId, ScreenMode};
use crate::state::{RightPanel, ViewMode};
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn demo() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let mut session = lcc_engine::Session::with_demo();
    // newest first, the newest photo open: written around it
    session.execute("library.sort", &json!({"ascending": false})).unwrap();
    session.selection = lcc_engine::Selection::single(session.visible_cloned()[0]);
    let app = LccApp::new(session, services);
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

// Regression: returning to a grid from Develop by a view change (an import showing its photos,
// a grid key) entered Library with the Develop panel still on the right, instead of Library's
// Classic panels, until Develop and back.
#[test]
fn a_grid_view_from_develop_shows_library_panels() {
    let mut h = demo();
    run(&mut h, "module.develop", json!({}));
    run(&mut h, "module.library", json!({}));
    run(&mut h, "module.develop", json!({}));
    assert_eq!(h.app.ui.right, RightPanel::Edit);
    run(&mut h, "view.photoGrid", json!({}));
    assert_eq!(h.app.ui.module, ModuleId::Library);
    assert!(!h.app.ui.right.is_edit_tool(), "{:?}", h.app.ui.right);
    // and the first time too, before Library has a saved layout
    let mut h = demo();
    h.app.ui.layouts.clear();
    h.app.ui.module = ModuleId::Develop;
    h.app.ui.view = ViewMode::Detail;
    h.app.ui.right = RightPanel::Edit;
    h.step();
    run(&mut h, "view.photoGrid", json!({}));
    assert_eq!(h.app.ui.module, ModuleId::Library);
    assert!(!h.app.ui.right.is_edit_tool(), "{:?}", h.app.ui.right);
}

// Regression (#144): Library came back in the loupe after Develop, whatever view it was left in.
#[test]
fn library_returns_to_its_own_view_after_develop() {
    let mut h = demo();
    run(&mut h, "view.photoGrid", json!({}));
    run(&mut h, "module.develop", json!({}));
    assert_eq!(h.app.ui.view, ViewMode::Detail);
    run(&mut h, "module.library", json!({}));
    assert_eq!((h.app.ui.module, h.app.ui.view), (ModuleId::Library, ViewMode::PhotoGrid));
    // left in the loupe, it comes back in the loupe
    run(&mut h, "view.loupe", json!({}));
    run(&mut h, "module.develop", json!({}));
    run(&mut h, "module.library", json!({}));
    assert_eq!(h.app.ui.view, ViewMode::Detail);
    // and across a save/load of ui.json
    run(&mut h, "view.photoGrid", json!({}));
    run(&mut h, "module.develop", json!({}));
    let saved = serde_json::to_string(&h.app.ui).unwrap();
    let back = serde_json::from_str::<crate::UiState>(&saved).unwrap().sanitized();
    assert_eq!(back.layouts.get(&ModuleId::Library).and_then(|l| l.view), Some(ViewMode::PhotoGrid));
}

// Issue #227: Returning to Library from Develop preserves the non-default view (Square Grid, Compare, etc.)
// and redundant top-bar navigation arrows are removed.
#[test]
fn library_preserves_non_default_view_when_returning_from_develop() {
    let mut h = demo();
    // Top-bar navigation arrows are removed
    assert!(!has_widget(&h, "icon:back"));
    assert!(!has_widget(&h, "icon:forward"));

    // Square grid preserved via module.library
    run(&mut h, "view.squareGrid", json!({}));
    assert_eq!(h.app.ui.view, ViewMode::SquareGrid);
    run(&mut h, "module.develop", json!({}));
    assert_eq!(h.app.ui.view, ViewMode::Detail);
    run(&mut h, "module.library", json!({}));
    assert_eq!((h.app.ui.module, h.app.ui.view), (ModuleId::Library, ViewMode::SquareGrid));

    // Square grid preserved when returning via view.back (Escape)
    run(&mut h, "module.develop", json!({}));
    assert_eq!(h.app.ui.view, ViewMode::Detail);
    run(&mut h, "view.back", json!({}));
    assert_eq!((h.app.ui.module, h.app.ui.view), (ModuleId::Library, ViewMode::SquareGrid));

    // Compare view preserved when returning via module.library
    run(&mut h, "view.compare", json!({}));
    assert_eq!(h.app.ui.view, ViewMode::Compare);
    run(&mut h, "module.develop", json!({}));
    assert_eq!(h.app.ui.view, ViewMode::Detail);
    run(&mut h, "module.library", json!({}));
    assert_eq!((h.app.ui.module, h.app.ui.view), (ModuleId::Library, ViewMode::Compare));
}

#[test]
fn panel_state_is_per_module_and_persists() {
    let mut h = demo();
    run(&mut h, "panel.left", json!({"show": true}));
    run(&mut h, "panel.order", json!({"order": ["keywordList", "quickDevelop"]}));
    run(&mut h, "module.develop", json!({}));
    // Develop starts from what was on screen; hide its left and right groups and the toolbar
    run(&mut h, "panel.sides", json!({"show": false}));
    run(&mut h, "panel.toolbar", json!({}));
    run(&mut h, "panel.autoShow", json!({"edge": "left", "on": true}));
    assert!(!h.app.ui.left_panel && !h.app.ui.right_edge && !h.app.ui.toolbar);
    assert!(!has_widget(&h, "panel:left_panel"));
    run(&mut h, "module.library", json!({}));
    assert!(h.app.ui.left_panel && h.app.ui.right_edge && h.app.ui.toolbar, "Library keeps its own panels");
    assert_eq!(crate::module::right_group(&h.app).first(), Some(&crate::module::PanelId::KeywordList), "panel order is the user's");
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
    // styled text: size (clamped), colour, bold; bad values are errors
    let r = run(&mut h, "view.identityPlate", json!({"size": 100, "color": "#ff8000", "bold": true}));
    assert_eq!((r["size"].as_f64(), r["color"].clone(), r["bold"].clone()), (Some(32.0), json!([255, 128, 0]), json!(true)));
    for bad in [json!({"color": "orange"}), json!({"color": 3}), json!({"size": "big"}), json!({"image": "/no/such/plate.svg"})] {
        let r = h.request("engine.execute", json!({"command": "view.identityPlate", "params": bad}), T);
        assert_eq!(r["ok"], false, "{r}");
    }
    // a graphic plate from an SVG file, drawn in the bar; "" goes back to text
    let svg = std::env::temp_dir().join(format!("plate-{}.svg", std::process::id()));
    std::fs::write(&svg, r#"<svg xmlns="http://www.w3.org/2000/svg" width="120" height="30"><circle cx="15" cy="15" r="14" fill="teal"/></svg>"#)
        .unwrap();
    run(&mut h, "view.identityPlate", json!({"image": svg.to_string_lossy()}));
    h.step();
    h.step();
    assert!(has_widget(&h, "region:identityPlate"));
    run(&mut h, "view.identityPlate", json!({"image": ""}));
    assert!(h.app.ui.identity_plate.image.is_empty());
    let _ = std::fs::remove_file(&svg);
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
    run(&mut h, "photo.increaseRating", json!({}));
    assert_eq!(h.app.session.catalog.photo(id).unwrap().rating, 3);
    for _ in 0..5 {
        run(&mut h, "photo.decreaseRating", json!({}));
    }
    assert_eq!(h.app.session.catalog.photo(id).unwrap().rating, 0);
    run(&mut h, "photo.flagToggle", json!({}));
    assert_eq!(h.app.session.catalog.photo(id).unwrap().flag, lcc_catalog::Flag::Pick);
    run(&mut h, "photo.flagToggle", json!({}));
    assert_eq!(h.app.session.catalog.photo(id).unwrap().flag, lcc_catalog::Flag::None);
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

/// The secondary window's own controls (here in egui's embedded window, as on the web): the mode
/// switcher, the filter bar in Grid, the filmstrip under the loupe; clicks select photos.
#[test]
fn secondary_window_has_its_own_switcher_filter_and_filmstrip() {
    let mut h = demo();
    run(&mut h, "second.grid", json!({}));
    h.step();
    for m in ["grid", "loupe", "live", "locked", "compare", "survey", "slideshow"] {
        assert!(has_widget(&h, &format!("secondMode:{m}")), "{m}");
    }
    assert!(has_widget(&h, "field:secondFilterText") && has_widget(&h, "secondFilterRating:3"));
    // the filter narrows the grid: rate one photo ★★★★★, ask for 5 stars
    let all = h.app.session.visible_cloned();
    let star = all[2];
    run(&mut h, "photo.rate", json!({"ids": [star.0], "rating": 5}));
    let r = h.request("ui.clickWidget", json!({"id": "secondFilterRating:5"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert_eq!(h.app.ui.second_filter.rating, 5);
    assert!(has_widget(&h, &format!("secondTile:{}", star.0)));
    assert!(!has_widget(&h, &format!("secondTile:{}", all[0].0)), "filtered out");
    // a tile click selects; the switcher's Normal button goes to the loupe with a filmstrip
    let r = h.request("ui.clickWidget", json!({"id": format!("secondTile:{}", star.0)}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(h.app.session.active(), Some(star));
    let r = h.request("ui.clickWidget", json!({"id": "secondMode:loupe"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert_eq!(h.app.ui.second_mode, crate::module::SecondMode::Loupe);
    assert!(has_widget(&h, &format!("secondFilm:{}", star.0)));
    run(&mut h, "second.filmstrip", json!({"show": false}));
    assert!(!has_widget(&h, &format!("secondFilm:{}", star.0)));
    // bad filter values are errors
    let r = h.request("engine.execute", json!({"command": "second.filter", "params": {"rating": 9}}), T);
    assert_eq!(r["ok"], false, "{r}");
    run(&mut h, "second.filter", json!({"clear": true}));
    assert_eq!(h.app.ui.second_filter, crate::panels::second::SecondFilter::default());
}

/// The tool in use is radio-style (Lightroom Classic): clicking its strip button leaves the
/// module as it is. It used to close the tool panel, and Develop with no editing panel is
/// Library's, so the click threw the user back into the grid.
#[test]
fn clicking_the_tool_in_use_keeps_develop() {
    let mut h = demo();
    run(&mut h, "module.develop", json!({}));
    h.step();
    for (cmd, icon, panel) in [
        ("panel.edit", "edit", RightPanel::Edit),
        ("panel.crop", "crop", RightPanel::Crop),
        ("panel.masking", "masking", RightPanel::Masking),
        ("panel.remove", "remove", RightPanel::Remove),
    ] {
        if h.app.ui.right != panel {
            run(&mut h, cmd, json!({}));
        }
        assert_eq!(h.app.ui.right, panel, "{cmd} is the tool in use");
        let r = h.request("ui.clickWidget", json!({"id": format!("icon:{icon}")}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        h.step();
        assert_eq!((h.app.ui.module, h.app.ui.right), (ModuleId::Develop, panel), "clicking the {icon} tool in use leaves it on screen");
    }
    // another tool's button still switches
    let r = h.request("ui.clickWidget", json!({"id": "icon:edit"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert_eq!((h.app.ui.module, h.app.ui.right), (ModuleId::Develop, RightPanel::Edit));
}

/// The tool strip's shortcuts and menu items keep toggling the tool off (Classic's `R`, `Q`…),
/// but closing Develop's editing panel leaves Develop: it used to hand the Detail view, with no
/// editing panel left, back to Library.
#[test]
fn closing_develops_tool_panel_stays_in_develop() {
    let mut h = demo();
    run(&mut h, "module.develop", json!({}));
    h.step();
    for (cmd, panel) in [
        ("panel.edit", RightPanel::Edit),
        ("panel.crop", RightPanel::Crop),
        ("panel.masking", RightPanel::Masking),
        ("panel.remove", RightPanel::Remove),
        ("panel.redeye", RightPanel::RedEye),
    ] {
        if h.app.ui.right != panel {
            run(&mut h, cmd, json!({}));
        }
        assert_eq!(h.app.ui.right, panel, "{cmd} is the tool in use");
        run(&mut h, cmd, json!({}));
        assert_eq!((h.app.ui.module, h.app.ui.right), (ModuleId::Develop, RightPanel::None), "{cmd} closes {panel:?} without leaving Develop");
        run(&mut h, cmd, json!({}));
        assert_eq!((h.app.ui.module, h.app.ui.right), (ModuleId::Develop, panel), "{cmd} opens {panel:?} again");
    }
    // the strip menu's hide leaves Develop too
    run(&mut h, "panel.edit", json!({}));
    run(&mut h, "panel.show", json!({"panel": "edit", "visible": false}));
    assert_eq!((h.app.ui.module, h.app.ui.right), (ModuleId::Develop, RightPanel::None));
    // a grid view is still Library's
    run(&mut h, "view.photoGrid", json!({}));
    assert_eq!(h.app.ui.module, ModuleId::Library);
}

/// #45, #108: each module's tool strip shows its own panels. Develop's are its editing tools plus
/// the Presets column — the Library panels it used to list there (Versions, Activity, Keywords,
/// Info) looked like Develop's tools. Library's strip is Versions and History & Activity, which open
/// over its Classic right column; keywords and info are still commands.
#[test]
fn tool_strip_shows_only_the_active_modules_tools() {
    let mut h = demo();
    run(&mut h, "module.develop", json!({}));
    h.step();
    for id in ["presets", "edit", "crop", "remove", "masking", "redeye", "more"] {
        assert!(has_widget(&h, &format!("icon:{id}")), "Develop strip has {id}");
    }
    for id in ["versions", "activity", "keywords", "info"] {
        assert!(!has_widget(&h, &format!("icon:{id}")), "Develop strip lacks {id}");
    }
    run(&mut h, "panel.info", json!({}));
    assert_eq!(h.app.ui.right, RightPanel::Info, "Info still opens by command");
    run(&mut h, "module.library", json!({}));
    h.step();
    for id in ["versions", "activity", "more"] {
        assert!(has_widget(&h, &format!("icon:{id}")), "Library strip has {id}");
    }
    for id in ["presets", "edit", "crop", "remove", "masking", "redeye", "keywords", "info"] {
        assert!(!has_widget(&h, &format!("icon:{id}")), "Library strip lacks {id}");
    }

    // Library's strip is the tab strip over its Classic column: clicking the panel in use closes it
    // and the column comes back.
    let r = h.request("ui.clickWidget", json!({"id": "icon:activity"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(h.app.ui.right, RightPanel::Activity, "clicking History & Activity opens it over the column");
    let r = h.request("ui.clickWidget", json!({"id": "icon:activity"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(h.app.ui.right, RightPanel::None, "clicking it again puts Library's Classic column back");
}

#[test]
fn identity_plate_setup_edits_the_plate_and_cancel_restores_it() {
    let mut h = demo();
    // Edit ▸ Identity Plate Setup… lists the dialog
    let bar = crate::menubar::menu_bar(&h.app);
    let edit = &bar.iter().find(|(t, _)| t == "Edit").unwrap().1;
    assert!(format!("{edit:?}").contains("dialog.identityPlateSetup"), "{edit:?}");
    let r = run(&mut h, "dialog.identityPlateSetup", json!({}));
    assert_eq!((r["open"].clone(), r["personalized"].clone()), (json!(true), json!(false)));
    h.step();
    assert!(has_widget(&h, "combo:identityPlateKind"));
    // personalise: the text options show, and edits reach the module bar while the dialog is open
    run(&mut h, "dialog.identityPlateSetup", json!({"personalized": true}));
    h.step();
    assert!(has_widget(&h, "field:identityPlateText") && has_widget(&h, "region:identityPlatePreview"));
    run(&mut h, "view.identityPlate", json!({"text": "Studio North", "bold": true, "showStatus": false}));
    assert_eq!(h.app.ui.identity_plate.text, "Studio North");
    // Cancel puts back the plate it opened with
    let r = h.request("ui.clickWidget", json!({"id": "button:identityPlateCancel"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert!(h.app.ui.identity_plate.setup.is_none() && h.app.ui.identity_plate.is_default(), "{:?}", h.app.ui.identity_plate);
    // OK keeps the edits, and they survive a save/load of the settings
    run(&mut h, "dialog.identityPlateSetup", json!({}));
    run(&mut h, "dialog.identityPlateSetup", json!({"personalized": true}));
    run(&mut h, "view.identityPlate", json!({"text": "Studio North", "kind": "graphic"}));
    h.step();
    assert!(has_widget(&h, "button:identityPlateLocate") && !has_widget(&h, "field:identityPlateText"));
    run(&mut h, "view.identityPlate", json!({"kind": "text"}));
    run(&mut h, "dialog.identityPlateSetup", json!({"close": "ok"}));
    h.step();
    assert!(!has_widget(&h, "combo:identityPlateKind"));
    let saved: crate::UiState = serde_json::from_str(&serde_json::to_string(&h.app.ui).unwrap()).unwrap();
    assert_eq!(saved.identity_plate.text, "Studio North");
    assert!(saved.identity_plate.setup.is_none());
    // choosing the app's own plate resets it
    run(&mut h, "dialog.identityPlateSetup", json!({}));
    run(&mut h, "dialog.identityPlateSetup", json!({"personalized": false}));
    assert!(h.app.ui.identity_plate.is_default());
    // bad arguments are errors, not panics
    for bad in [json!({"close": "maybe"}), json!({"close": 1}), json!({"personalized": "yes"})] {
        let r = h.request("engine.execute", json!({"command": "dialog.identityPlateSetup", "params": bad}), T);
        assert_eq!(r["ok"], false, "{r}");
    }
    let r = h.request("engine.execute", json!({"command": "view.identityPlate", "params": {"kind": "banner"}}), T);
    assert_eq!(r["ok"], false, "{r}");
}
