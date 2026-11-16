//! Headless tests of the Modern layout (`crate::modern`): the floating chrome, the dock's groups and
//! their cards of pill sliders, the tools, the dock's two places, the menu bar, the Library /
//! Develop switch, and bad commands as errors.

use std::time::Duration;

use serde_json::{Value, json};

use crate::headless::Headless;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn ask(h: &mut Headless, method: &str, params: Value) -> Value {
    let r = h.request(method, params.clone(), T);
    h.settle(SETTLE);
    r
}

fn exec(h: &mut Headless, command: &str, params: Value) -> Value {
    ask(h, "engine.execute", json!({"command": command, "params": params}))
}

/// The demo library in Develop on its first photo, Modern layout on.
fn develop(size: [f32; 2]) -> Headless {
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, size, 1.0);
    let id = h.app.session.visible_cloned()[0].0;
    assert_eq!(exec(&mut h, "view.modernLayout", json!({"on": true}))["ok"], true);
    assert_eq!(exec(&mut h, "library.select", json!({"ids": [id]}))["ok"], true);
    assert_eq!(exec(&mut h, "module.develop", json!({}))["ok"], true);
    assert_eq!(exec(&mut h, "view.detail", json!({}))["ok"], true);
    exec(&mut h, "modern.chrome", json!({"on": false}));
    h
}

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

fn value(h: &Headless, control: &str) -> f64 {
    let id = h.app.session.selection.active.unwrap();
    lcc_develop::controls::get(&h.app.session.develop_of(id).unwrap_or_default(), control).unwrap()
}

/// One Develop session through the chrome: a tap on the photo hides and shows it; a group opens
/// its card of pills and a drag is one undo step; the dock moves to the side and back; ☰ opens the
/// menus; the switch goes to Library and back.
#[test]
fn the_chrome_its_cards_the_dock_the_menu_and_the_module_switch() {
    let mut h = develop([1280.0, 800.0]);
    exec(&mut h, "modern.chrome", json!({"on": true}));
    for id in ["modern:canvas", "modern:dock", "modern:group:light", "modern:group:crop", "modern:menu", "modern:mode:library", "modern:histogram"] {
        assert!(has(&h, id), "{id}");
    }
    assert_eq!(ask(&mut h, "ui.clickWidget", json!({"id": "modern:canvas", "fx": 0.4, "fy": 0.4}))["ok"], true);
    assert!(!h.app.ui.modern.chrome, "a tap hides the chrome");
    assert!(!has(&h, "modern:dock"));
    ask(&mut h, "ui.clickWidget", json!({"id": "modern:canvas", "fx": 0.4, "fy": 0.4}));
    assert!(h.app.ui.modern.chrome && has(&h, "modern:dock"), "a second tap shows it");

    // a group's card of pills; one drag, one undo step
    let before = value(&h, "light.exposure");
    assert_eq!(ask(&mut h, "ui.clickWidget", json!({"id": "modern:group:light"}))["ok"], true);
    assert_eq!(h.app.ui.modern.group, Some(crate::modern_dock::Group::Light));
    assert!(has(&h, "modern:card") && has(&h, "slider:light.exposure"), "the card's pills");
    let undo = h.app.session.undo.len();
    let r = ask(&mut h, "ui.dragWidget", json!({"id": "slider:light.exposure", "fx": 0.2, "dx": 120.0, "steps": 12}));
    assert_eq!(r["ok"], true, "{r}");
    let after = value(&h, "light.exposure");
    assert!(after > before, "dragging right raises exposure ({before} → {after})");
    assert_eq!(h.app.session.undo.len(), undo + 1, "one drag, one undo step");
    // a second tap on the open group puts it away; a tap on the photo does too
    ask(&mut h, "ui.clickWidget", json!({"id": "modern:group:light"}));
    assert!(h.app.ui.modern.group.is_none() && !has(&h, "modern:card"));
    ask(&mut h, "ui.clickWidget", json!({"id": "modern:group:effects"}));
    assert!(has(&h, "slider:effects.clarity"));
    ask(&mut h, "ui.clickWidget", json!({"id": "modern:canvas", "fx": 0.5, "fy": 0.2}));
    assert!(h.app.ui.modern.group.is_none(), "a tap on the photo puts the card away");
    assert!(h.app.ui.modern.chrome, "and leaves the chrome");

    // the dock moves to the side and back
    let dock = |h: &Headless| h.app.widgets.iter().rev().find(|(w, _)| w == "modern:dock").map(|(_, r)| *r).unwrap();
    assert!(dock(&h).width() > dock(&h).height(), "along the bottom");
    ask(&mut h, "ui.clickWidget", json!({"id": "modern:dockSwitch"}));
    assert!(h.app.ui.modern.dock_side);
    assert!(dock(&h).height() > dock(&h).width() && dock(&h).left() < 40.0, "down the side");
    // clear of the top row's ☰ (and of the menu bar it opens), however short the window
    let menu = h.app.widgets.iter().rev().find(|(w, _)| w == "modern:menu").map(|(_, r)| *r).unwrap();
    assert!(dock(&h).top() > menu.bottom(), "below ☰: {:?} vs {menu:?}", dock(&h));
    ask(&mut h, "ui.resize", json!({"width": 900, "height": 420}));
    let menu = h.app.widgets.iter().rev().find(|(w, _)| w == "modern:menu").map(|(_, r)| *r).unwrap();
    assert!(dock(&h).top() > menu.bottom() && dock(&h).bottom() <= 420.0, "a short window: {:?}", dock(&h));
    ask(&mut h, "ui.resize", json!({"width": 1280, "height": 800}));
    ask(&mut h, "ui.clickWidget", json!({"id": "modern:dockSwitch"}));
    assert!(!h.app.ui.modern.dock_side);

    // ☰ opens on Shortcuts and reaches every menu; Esc closes it and nothing more
    ask(&mut h, "ui.clickWidget", json!({"id": "modern:menu"}));
    assert_eq!(h.app.ui.modern.menu.as_deref(), Some("Shortcuts"));
    assert!(has(&h, "modern:shortcut:import") && has(&h, "modern:shortcut:export") && has(&h, "modern:shortcut:settings"));
    // Settings opens the settings dialog, like Edit ▸ Settings
    ask(&mut h, "ui.clickWidget", json!({"id": "modern:shortcut:settings"}));
    assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::Settings { .. })), "settings dialog");
    ask(&mut h, "ui.key", json!({"key": "Escape"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "modern:menu"}));
    assert_eq!(h.app.ui.modern.menu.as_deref(), Some("Shortcuts"));
    for title in ["File", "Edit", "Photo", "View", "Help"] {
        assert!(has(&h, &format!("modern:menu:{title}")), "{title}");
    }
    ask(&mut h, "ui.clickWidget", json!({"id": "modern:menu:File"}));
    assert_eq!(h.app.ui.modern.menu.as_deref(), Some("File"));
    ask(&mut h, "ui.key", json!({"key": "Escape"}));
    assert!(h.app.ui.modern.menu.is_none(), "Esc closes it");
    assert_eq!(h.app.ui.module, crate::module::ModuleId::Develop, "and nothing more");

    // the switch goes between Library and Develop
    ask(&mut h, "ui.clickWidget", json!({"id": "modern:mode:library"}));
    assert_eq!(h.app.ui.module, crate::module::ModuleId::Library);
    ask(&mut h, "ui.clickWidget", json!({"id": "modern:mode:develop"}));
    assert_eq!(h.app.ui.module, crate::module::ModuleId::Develop);
}

#[test]
fn the_tools_open_their_panels_and_dodge_starts_a_lightening_brush() {
    let mut h = develop([1280.0, 800.0]);
    exec(&mut h, "modern.chrome", json!({"on": true}));
    ask(&mut h, "ui.clickWidget", json!({"id": "modern:group:crop"}));
    assert_eq!(h.app.ui.right, crate::state::RightPanel::Crop);
    // the card holds the crop's own controls, not Classic's histogram and Previous / Reset bar
    let card = h.app.widgets.iter().rev().find(|(w, _)| w == "modern:card").map(|(_, r)| *r).unwrap();
    assert!(!h.app.widgets.iter().any(|(w, r)| w.starts_with("histogram") && card.intersects(*r)), "no histogram in the crop card");
    ask(&mut h, "ui.clickWidget", json!({"id": "modern:group:crop"}));
    assert_eq!(h.app.ui.right, crate::state::RightPanel::Edit, "closing a tool leaves its overlay");
    ask(&mut h, "ui.clickWidget", json!({"id": "modern:group:dodge"}));
    assert_eq!(h.app.ui.right, crate::state::RightPanel::Masking);
    let id = h.app.session.selection.active.unwrap();
    let masks = h.app.session.develop_of(id).unwrap_or_default().masks.clone();
    let last = masks.last().expect("a new mask");
    assert!(last.adjust.exposure > 0.0, "Dodge lightens ({})", last.adjust.exposure);
}

#[test]
fn bad_modern_commands_are_errors_not_panics() {
    let mut h = develop([400.0, 860.0]);
    for params in [json!({}), json!({"group": 3}), json!({"group": "nope"})] {
        let r = exec(&mut h, "modern.group", params.clone());
        assert_eq!(r["ok"], false, "modern.group {params}: {r}");
    }
    assert_eq!(exec(&mut h, "modern.group", json!({"group": null}))["ok"], true, "nothing open: nothing to close");
    assert_eq!(exec(&mut h, "modern.menu", json!({"menu": "Nope"}))["ok"], false);
    assert_eq!(exec(&mut h, "modern.menu", json!({"menu": 7}))["ok"], true, "not a name: the first menu");
    exec(&mut h, "modern.menu", json!({"on": false}));
    // the old touch command names are gone
    assert_eq!(exec(&mut h, "modern.tool", json!({"tool": "basic"}))["ok"], false);
    // the layout turned off leaves Classic's edges
    exec(&mut h, "view.modernLayout", json!({"on": false}));
    assert!(!has(&h, "modern:canvas"));
}

#[test]
fn a_tap_in_the_library_grid_opens_the_photo() {
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1280.0, 800.0], 1.0);
    exec(&mut h, "view.modernLayout", json!({"on": true}));
    exec(&mut h, "view.photoGrid", json!({}));
    assert!(has(&h, "modern:import"), "Library bar");
    ask(&mut h, "ui.clickWidget", json!({"id": "modern:import"}));
    assert!(has(&h, "modern:import:folder"), "Import offers a folder");
    ask(&mut h, "ui.clickWidget", json!({"id": "modern:import"}));
    let cell = h.app.widgets.iter().find(|(w, _)| w.starts_with("thumb:")).map(|(w, _)| w.clone());
    let Some(cell) = cell else { panic!("no grid cell on screen: {:?}", h.app.widgets.iter().map(|w| &w.0).take(40).collect::<Vec<_>>()) };
    ask(&mut h, "ui.clickWidget", json!({"id": cell}));
    assert_eq!(h.app.ui.module, crate::module::ModuleId::Develop);
    assert_eq!(h.app.ui.view, crate::state::ViewMode::Detail);
}

/// A tap opens the photo under the finger, wherever on its cell it lands, not the photo that was active before
/// (on a phone a tap near a cell's corner opened the photo edited last).
#[test]
fn a_tap_opens_the_photo_under_the_finger_not_the_one_active_before() {
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1280.0, 800.0], 1.0);
    exec(&mut h, "view.modernLayout", json!({"on": true}));
    exec(&mut h, "view.photoGrid", json!({}));
    h.settle(SETTLE);
    let cells = |h: &Headless| -> Vec<(u64, egui::Rect)> {
        h.app.widgets.iter().filter_map(|(w, r)| w.strip_prefix("thumb:").and_then(|n| n.parse().ok()).map(|id| (id, *r))).collect()
    };
    let on_screen = cells(&h);
    assert!(on_screen.len() >= 2, "two grid cells on screen");
    let last = on_screen.iter().map(|c| c.0).max().unwrap();
    exec(&mut h, "library.select", json!({"ids": [last]}));
    h.settle(SETTLE);
    // another photo, where it is now, tapped near its cell's bottom-right corner (where its badges are)
    let (other, rect) = cells(&h).into_iter().find(|c| c.0 != last).unwrap();
    let at = rect.max - egui::vec2(4.0, 4.0);
    h.request("ui.click", json!({"x": at.x, "y": at.y}), T);
    h.settle(SETTLE);
    assert_eq!(h.app.ui.module, crate::module::ModuleId::Develop);
    assert_eq!(h.app.session.active(), Some(lcc_catalog::PhotoId(other)), "the tapped photo opened");
}

#[test]
fn the_empty_library_offers_finger_sized_ways_in_without_desktop_hints() {
    // a folding phone's cover screen and its inner, near-square screen
    for size in [[412.0, 960.0], [1092.0, 984.0]] {
        let app = LccApp::new(lcc_engine::Session::new(), Services { png: None, ..Default::default() });
        let mut h = Headless::new(app, size, 1.0);
        assert_eq!(exec(&mut h, "view.modernLayout", json!({"on": true}))["ok"], true);
        h.settle(SETTLE);
        assert!(has(&h, "button:emptyImportPhotos") && has(&h, "button:emptyImportFolder"), "{size:?}");
        assert!(!has(&h, "button:emptyImportLightroom"), "no Lightroom catalog import on touch ({size:?})");
        let height = h.app.widgets.iter().find(|(w, _)| w == "button:emptyImportPhotos").map(|(_, r)| r.height()).unwrap();
        assert!(height >= 44.0, "finger-sized buttons ({size:?}: {height})");
    }
}

/// Every develop setting is reachable in Modern: a slider with more behind it carries a mark and a
/// right-click (or long press) opens its whole section; each group's section buttons open the rest.
#[test]
fn every_setting_is_reachable_through_the_groups_sections() {
    let mut h = develop([1280.0, 800.0]);
    exec(&mut h, "modern.chrome", json!({"on": true}));
    exec(&mut h, "modern.group", json!({"group": "effects"}));
    assert!(has(&h, "sliderMore:vignette.amount"), "Vignette says it has more");
    ask(&mut h, "ui.clickWidget", json!({"id": "slider:vignette.amount", "button": "right"}));
    assert_eq!(h.app.ui.modern.more.as_deref(), Some("effects"));
    for id in ["vignette.midpoint", "vignette.feather", "vignette.roundness", "vignette.highlights", "grain.size", "grain.roughness"] {
        assert!(has(&h, &format!("slider:{id}")), "{id} in Effects");
    }
    // Back: to the group's sliders first
    let r = exec(&mut h, "modern.back", json!({}));
    assert_eq!(r["result"]["closed"], "section", "{r}");
    assert!(h.app.ui.modern.group.is_some() && h.app.ui.modern.more.is_none());
    for group in crate::modern_dock::Group::ADJUST {
        exec(&mut h, "modern.group", json!({"group": group.label()}));
        for (key, _) in group.sections() {
            assert_eq!(ask(&mut h, "ui.clickWidget", json!({"id": format!("modern:more:{key}")}))["ok"], true, "{group:?} offers {key}");
            assert_eq!(h.app.ui.modern.more.as_deref(), Some(*key));
            assert!(has(&h, "modern:card"));
            ask(&mut h, "ui.clickWidget", json!({"id": "modern:moreBack"}));
            assert!(h.app.ui.modern.more.is_none(), "‹ goes back");
        }
    }
    // the tone curve and colour grading are there
    exec(&mut h, "modern.group", json!({"group": "colour"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "modern:more:grading"}));
    assert_eq!(h.app.ui.modern.more.as_deref(), Some("grading"));
    // and that card follows the grading mode (Settings ▸ General). Witchcraft (the default): the
    // colourist's Primaries palette and the HSL curves
    assert!(has(&h, "wheel:primaries-lift"), "the colourist wheels");
    assert!(has(&h, "curve:hsl"), "the HSL curves");
    assert!(!has(&h, "wheel:shadows"), "not Lightroom's 3-way");
    // Eboda: Lightroom's own 3-way wheels
    exec(&mut h, "view.gradingMode", json!({"mode": "eboda"}));
    assert!(has(&h, "wheel:shadows"), "the 3-way wheels");
    assert!(!has(&h, "wheel:primaries-lift"));
}

/// While a pill is dragged the chrome fades away (the slider and the histogram stay), and it comes
/// back when the drag ends.
#[test]
fn the_chrome_fades_while_a_slider_is_dragged() {
    let mut h = develop([1280.0, 800.0]);
    exec(&mut h, "modern.chrome", json!({"on": true}));
    exec(&mut h, "modern.group", json!({"group": "light"}));
    let pill = h.app.widgets.iter().rev().find(|(w, _)| w == "slider:light.exposure").map(|(_, r)| *r).unwrap();
    let at = pill.left_center() + egui::vec2(30.0, 0.0);
    let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
    for e in [egui::Event::PointerMoved(at), button(at, true)] {
        h.app.synthetic.push(e);
        h.step();
    }
    for i in 1..=30 {
        h.app.synthetic.push(egui::Event::PointerMoved(at + egui::vec2(i as f32 * 3.0, 0.0)));
        h.step();
    }
    assert!(crate::modern::chrome_alpha(&h.view.ctx) < 0.2, "faded while dragging: {}", crate::modern::chrome_alpha(&h.view.ctx));
    assert!(has(&h, "modern:histogram"), "the histogram stays");
    h.app.synthetic.push(button(at + egui::vec2(90.0, 0.0), false));
    h.settle(SETTLE);
    assert!((crate::modern::chrome_alpha(&h.view.ctx) - 1.0).abs() < 1e-3, "back after the drag");
}
