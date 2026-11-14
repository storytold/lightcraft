//! Headless tests of the Modern layout's gestures and edges: a swipe scrolls the grid (a tap still
//! opens, a long press picks), Back closes things in order and hands the root to the system, the
//! UI keeps clear of the screen's insets, the dock scrolls to the open group, and Develop's
//! histogram switches size, moves, snaps and has its menu.

use std::time::Duration;

use serde_json::{Value, json};

use crate::headless::Headless;
use crate::modern::HistogramPlace;
use crate::module::ModuleId;
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

fn rect(h: &Headless, id: &str) -> Option<egui::Rect> {
    h.app.widgets.iter().rev().find(|(w, _)| w == id).map(|(_, r)| *r)
}

/// The demo library's grid in the Modern layout.
fn library(size: [f32; 2]) -> Headless {
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, size, 1.0);
    assert_eq!(exec(&mut h, "view.modernLayout", json!({"on": true}))["ok"], true);
    exec(&mut h, "view.squareGrid", json!({}));
    h
}

/// The demo library in Develop on its first photo, Modern layout on.
fn develop(size: [f32; 2]) -> Headless {
    let mut h = library(size);
    let id = h.app.session.visible_cloned()[0].0;
    exec(&mut h, "library.select", json!({"ids": [id]}));
    exec(&mut h, "module.develop", json!({}));
    exec(&mut h, "view.detail", json!({}));
    exec(&mut h, "modern.chrome", json!({"on": true}));
    h
}

fn first_thumb(h: &Headless) -> (String, u64) {
    let grid = rect(h, "grid:scroll").expect("the grid");
    let visible = |r: &egui::Rect| r.intersect(grid).area();
    let (w, _) = h
        .app
        .widgets
        .iter()
        .filter(|(w, _)| w.starts_with("thumb:"))
        .max_by(|a, b| visible(&a.1).total_cmp(&visible(&b.1)))
        .expect("a grid cell on screen");
    (w.clone(), w["thumb:".len()..].parse().unwrap())
}

#[test]
fn a_swipe_scrolls_the_grid_without_picking_or_dragging_a_photo() {
    // narrow and short: the demo library is taller than the screen
    let mut h = library([360.0, 420.0]);
    let selected = h.app.session.selection.ids.clone();
    let top = h.app.grid_scroll.unwrap_or(0.0);
    let (cell, _) = first_thumb(&h);
    let r = ask(&mut h, "ui.dragWidget", json!({"id": cell, "dy": -160.0, "steps": 8}));
    assert_eq!(r["ok"], true, "{r}");
    let after = h.app.grid_scroll.unwrap_or(0.0);
    assert!(after > top + 40.0, "swiping up scrolls the grid down ({top} → {after})");
    assert_eq!(h.app.ui.module, ModuleId::Library, "a swipe opens nothing");
    assert_eq!(h.app.session.selection.ids, selected, "a swipe picks nothing");
    assert!(h.app.ui.dragging_photos.is_none(), "a swipe drags no photos");
    // a tap still opens the photo under it
    let (cell, id) = first_thumb(&h);
    ask(&mut h, "ui.clickWidget", json!({"id": cell}));
    assert_eq!(h.app.ui.module, ModuleId::Develop);
    assert_eq!(h.app.session.active().map(|p| p.0), Some(id));
}

/// The menu bar, open on a phone-sized window, scrolls sideways under a finger: the open menu
/// doesn't lock it in place (its title scrolls out of view and stays out), and a tap still picks
/// the menu that has scrolled into view.
#[test]
fn the_open_menu_bar_scrolls_sideways_under_a_finger() {
    let mut h = library([412.0, 960.0]);
    exec(&mut h, "modern.menu", json!({"on": true}));
    let bar = rect(&h, "modern:menubar").expect("the menu bar");
    let help = rect(&h, "modern:menu:Help").expect("Help's title");
    assert!(help.left() >= bar.right(), "Help starts off screen to the right ({help:?} in {bar:?})");
    ask(&mut h, "ui.dragWidget", json!({"id": "modern:menubar", "fx": 0.1, "dx": -400.0, "steps": 10}));
    let help = rect(&h, "modern:menu:Help").expect("Help's title");
    assert!(help.left() < 412.0, "the swipe brought Help into view ({help:?})");
    assert!(h.app.ui.modern.menu.is_some(), "the swipe closed nothing");
    // and it stays: the open menu doesn't pull the bar back to its own title
    for _ in 0..3 {
        h.step();
    }
    let stayed = rect(&h, "modern:menu:Help").unwrap();
    assert!((stayed.left() - help.left()).abs() < 2.0, "no snap-back ({help:?} → {stayed:?})");
    // a tap still picks the menu now in view
    ask(&mut h, "ui.clickWidget", json!({"id": "modern:menu:Help"}));
    assert_eq!(h.app.ui.modern.menu.as_deref(), Some("Help"), "the tap picked Help");
    ask(&mut h, "ui.key", json!({"key": "BrowserBack"}));
    assert!(h.app.ui.modern.menu.is_none(), "Back closes the bar");
}

/// The loupe's shot-info strip never sits under the Modern layout's floating dock: it keeps clear
/// of it, and hides while a group's card is open above the dock.
#[test]
fn the_shot_info_strip_keeps_clear_of_the_modern_dock() {
    // wide: the photo fills the canvas to its bottom edge, where the dock floats
    let mut h = develop([1280.0, 800.0]);
    let strip = rect(&h, "loupe:shotInfo").expect("the shot-info strip");
    let dock = rect(&h, "modern:dock").expect("the dock");
    assert!(strip.bottom() <= dock.top() + 1.0, "the strip is above the dock ({strip:?} vs {dock:?})");
    exec(&mut h, "modern.group", json!({"group": "light"}));
    assert!(rect(&h, "loupe:shotInfo").is_none(), "a card is open over it: the strip hides");
    exec(&mut h, "modern.group", json!({"group": null}));
    assert!(rect(&h, "loupe:shotInfo").is_some(), "back with the card away");
}

/// A mouse is not a finger: its drag goes to the widget under it (a scroll bar, a photo), not to
/// the swipe scroller, so the grid's scroll bar can be dragged on the desktop.
#[test]
fn a_mouse_drag_is_not_a_swipe() {
    let mut h = library([1280.0, 800.0]);
    h.app.ui.modern.mouse_in_tests = true;
    let grid = rect(&h, "grid:scroll").expect("the grid");
    let at = grid.center();
    let (thumb, _) = first_thumb(&h);
    let before = rect(&h, &thumb).unwrap();
    for e in [
        egui::Event::PointerMoved(at),
        egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() },
        egui::Event::PointerMoved(at + egui::vec2(0.0, -40.0)),
    ] {
        h.app.synthetic.push(e);
        h.step();
    }
    h.app.synthetic.push(egui::Event::PointerButton {
        pos: at + egui::vec2(0.0, -40.0),
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: Default::default(),
    });
    h.settle(SETTLE);
    assert_eq!(rect(&h, &thumb), Some(before), "the grid didn't scroll as a finger's swipe would");
}

#[test]
fn a_long_press_picks_photos_and_taps_then_add_until_back() {
    let mut h = library([600.0, 900.0]);
    let cells: Vec<(String, egui::Rect)> = h.app.widgets.iter().filter(|(w, _)| w.starts_with("thumb:")).take(3).cloned().collect();
    assert!(cells.len() >= 3);
    let at = cells[0].1.center();
    h.app.synthetic.push(egui::Event::PointerMoved(at));
    h.app.synthetic.push(egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    for _ in 0..60 {
        h.step();
    }
    h.app.synthetic.push(egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.settle(SETTLE);
    assert!(h.app.ui.modern.selecting, "a long press starts picking");
    assert_eq!(h.app.ui.module, ModuleId::Library, "and opens nothing");
    assert_eq!(h.app.session.selection.ids.len(), 1);
    assert!(rect(&h, "modern:endSelect").is_some(), "the bar counts the picked photos");
    for (w, _) in &cells[1..] {
        ask(&mut h, "ui.clickWidget", json!({"id": w}));
    }
    assert_eq!(h.app.session.selection.ids.len(), 3, "taps add photos while picking");
    assert_eq!(h.app.ui.module, ModuleId::Library);
    let r = exec(&mut h, "modern.back", json!({}));
    assert_eq!(r["result"]["closed"], "selection", "{r}");
    assert!(!h.app.ui.modern.selecting);
    assert!(h.app.session.selection.ids.len() <= 1);
}

#[test]
fn back_closes_dialog_menu_card_and_develop_then_leaves_the_root_to_the_system() {
    let mut h = develop([800.0, 1280.0]);
    let back = |h: &mut Headless| {
        let r = exec(h, "modern.back", json!({}));
        assert_eq!(r["ok"], true, "{r}");
        r["result"].clone()
    };
    // the Back key runs the same steps, one a press
    exec(&mut h, "modern.group", json!({"group": "light"}));
    ask(&mut h, "ui.key", json!({"key": "BrowserBack"}));
    assert!(h.app.ui.modern.group.is_none(), "the Back key put the card away");
    assert_eq!(h.app.ui.module, ModuleId::Develop, "one press, one step");

    exec(&mut h, "app.settings", json!({}));
    assert!(h.app.ui.dialog.is_some());
    assert_eq!(back(&mut h)["closed"], "dialog");
    assert!(h.app.ui.dialog.is_none(), "Back closes the dialog as Esc does");

    exec(&mut h, "modern.menu", json!({"on": true}));
    assert_eq!(back(&mut h)["closed"], "menuBar");
    assert!(h.app.ui.modern.menu.is_none());

    exec(&mut h, "modern.group", json!({"group": "light"}));
    ask(&mut h, "ui.dragWidget", json!({"id": "slider:light.exposure", "fx": 0.2, "dx": 150.0, "steps": 8}));
    let id = h.app.session.active().unwrap();
    let changed = lcc_develop::controls::get(&h.app.session.develop_of(id).unwrap_or_default(), "light.exposure").unwrap();
    assert!(changed > 0.0);
    assert_eq!(back(&mut h)["closed"], "card");
    assert!(h.app.ui.modern.group.is_none());
    let kept = lcc_develop::controls::get(&h.app.session.develop_of(id).unwrap_or_default(), "light.exposure").unwrap();
    assert_eq!(kept, changed, "Back puts the card away and keeps the change");

    assert_eq!(back(&mut h)["closed"], "develop");
    assert_eq!(h.app.ui.module, ModuleId::Library);

    assert!(!crate::modern::take_system_back(&mut h.app));
    let root = back(&mut h);
    assert_eq!(root["handled"], false, "{root}");
    assert!(crate::modern::take_system_back(&mut h.app), "the root's Back goes to the system");
}

#[test]
fn the_ui_keeps_clear_of_the_insets_and_a_narrow_dock_scrolls_to_the_open_group() {
    // a folding phone's cover screen with the camera cutout at the top, and a gesture bar
    let (top, bottom) = (38.0, 18.0);
    let mut h = library([412.0, 960.0]);
    assert_eq!(exec(&mut h, "modern.insets", json!({"top": top, "bottom": bottom}))["ok"], true);
    let import = rect(&h, "modern:import").expect("library bar");
    assert!(import.top() >= top, "the Library bar is below the cutout ({import:?})");
    let mut h = develop([412.0, 960.0]);
    exec(&mut h, "modern.insets", json!({"top": top, "bottom": bottom}));
    let menu = rect(&h, "modern:menu").expect("the menu button");
    assert!(menu.top() >= top, "Develop's buttons are below the cutout ({menu:?})");
    let dock = rect(&h, "modern:dock").expect("the dock");
    assert!(dock.bottom() <= 960.0 - bottom, "the dock is above the gesture bar ({dock:?})");
    exec(&mut h, "modern.group", json!({"group": "light"}));
    let card = rect(&h, "modern:card").expect("the card");
    assert!(card.bottom() <= dock.top() && card.top() >= top, "the card sits above the dock ({card:?})");
    // nonsense is refused or cleaned, never a panic
    assert_eq!(exec(&mut h, "modern.insets", json!({"top": "lots"}))["ok"], false);
    let r = exec(&mut h, "modern.insets", json!({"top": -5.0, "left": 1e30}));
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(h.app.ui.modern.insets.top, 0.0);
    assert!(h.app.ui.modern.insets.left <= 400.0);
    // on this narrow window the dock scrolls to the open group
    exec(&mut h, "modern.insets", json!({"top": 0.0, "left": 0.0, "bottom": 0.0}));
    let dock = rect(&h, "modern:dock").expect("the dock");
    assert!(dock.width() <= 412.0, "the dock fits the window ({dock:?})");
    for group in ["effects", "heal", "presets"] {
        exec(&mut h, "modern.group", json!({"group": group}));
        let b = rect(&h, &format!("modern:group:{group}")).expect("its button");
        assert!(b.left() >= dock.left() - 1.0 && b.right() <= dock.right() + 1.0, "{group} is in view ({b:?} in {dock:?})");
    }
}

#[test]
fn the_histogram_switches_size_moves_snaps_and_has_a_menu() {
    let mut h = develop([1280.0, 800.0]);
    assert_eq!(h.app.ui.modern.histogram, HistogramPlace::Small, "on, small, by default");
    let small = rect(&h, "modern:histogram").expect("the histogram");
    ask(&mut h, "ui.clickWidget", json!({"id": "modern:histogram"}));
    assert_eq!(h.app.ui.modern.histogram, HistogramPlace::Large, "a tap makes it large");
    let large = rect(&h, "modern:histogram").unwrap();
    assert!(large.area() > small.area() * 2.0, "{small:?} → {large:?}");
    ask(&mut h, "ui.clickWidget", json!({"id": "modern:histogram"}));
    assert_eq!(h.app.ui.modern.histogram, HistogramPlace::Small, "and small again");
    // a drag moves it (a fraction of the room, saved), and near a corner it snaps there
    let before = rect(&h, "modern:histogram").unwrap();
    ask(&mut h, "ui.dragWidget", json!({"id": "modern:histogram", "dx": -400.0, "dy": 200.0, "steps": 10}));
    let after = rect(&h, "modern:histogram").unwrap();
    assert!(after.left() < before.left() - 200.0 && after.top() > before.top() + 100.0, "{before:?} → {after:?}");
    let at = h.app.ui.modern.histogram_at;
    assert!(at.iter().all(|v| (0.0..=1.0).contains(v)), "kept as a fraction ({at:?})");
    ask(&mut h, "ui.dragWidget", json!({"id": "modern:histogram", "dx": -2000.0, "dy": 2000.0, "steps": 10}));
    assert_eq!(h.app.ui.modern.histogram_at, [0.0, 1.0], "snapped into the bottom-left corner");
    let saved: crate::modern::ModernUi = serde_json::from_value(serde_json::to_value(&h.app.ui.modern).unwrap()).unwrap();
    assert_eq!((saved.histogram, saved.histogram_at), (HistogramPlace::Small, [0.0, 1.0]));
    // right-click: its menu; Back to the Corner, then Hide
    ask(&mut h, "ui.clickWidget", json!({"id": "modern:histogram", "button": "right"}));
    assert!(rect(&h, "modern:histogram:corner").is_some(), "its menu");
    ask(&mut h, "ui.clickWidget", json!({"id": "modern:histogram:corner"}));
    assert_eq!(h.app.ui.modern.histogram_at, [1.0, 0.0]);
    // its type and scale: the same setting as Classic's panel
    ask(&mut h, "ui.clickWidget", json!({"id": "modern:histogram", "button": "right"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "modern:histogram:mode:parade"}));
    assert_eq!(h.app.ui.histogram_mode, "parade");
    ask(&mut h, "ui.clickWidget", json!({"id": "modern:histogram", "button": "right"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "modern:histogram:scale:logarithmic"}));
    assert_eq!(h.app.ui.histogram_scale, "logarithmic");
    ask(&mut h, "ui.clickWidget", json!({"id": "modern:histogram", "button": "right"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "modern:histogram:hide"}));
    assert_eq!(h.app.ui.modern.histogram, HistogramPlace::Off);
    assert!(rect(&h, "modern:histogram").is_none());
    // the way back: ☰ ▸ Shortcuts ▸ Show Histogram (and Window ▸ Panels ▸ Histogram)
    exec(&mut h, "modern.menu", json!({"on": true}));
    ask(&mut h, "ui.clickWidget", json!({"id": "modern:shortcut:histogram"}));
    assert_eq!(h.app.ui.modern.histogram, HistogramPlace::Small);
    exec(&mut h, "view.histogram", json!({}));
    assert_eq!(h.app.ui.modern.histogram, HistogramPlace::Off);
    exec(&mut h, "view.histogram", json!({}));
    assert!(rect(&h, "modern:histogram").is_some());
    for bad in [json!({"mode": "sideways"}), json!({"mode": 3}), json!({"mode": "off", "at": [0.5]}), json!({"at": "x"})] {
        assert_eq!(exec(&mut h, "modern.histogram", bad.clone())["ok"], false, "{bad}");
    }
    // the touch layout's names still work
    assert_eq!(exec(&mut h, "modern.histogram", json!({"mode": "floating"}))["ok"], true);
    assert_eq!(h.app.ui.modern.histogram, HistogramPlace::Small);
}

#[test]
fn activity_cards_sit_below_the_top_buttons_inside_the_insets() {
    // a folding phone's cover screen with a status bar and a side cutout
    let mut h = library([412.0, 960.0]);
    exec(&mut h, "modern.insets", json!({"top": 28.0, "left": 12.0}));
    let g = h.app.session.activity.start("faces", "Finding faces", lcc_engine::activity::Cancel::No);
    std::thread::sleep(Duration::from_millis(600));
    h.step();
    h.step();
    let id = h.app.session.activity.list()[0].id;
    let card = rect(&h, &format!("activity:row:{id}")).expect("the activity card");
    assert!(card.left() >= 12.0 && card.top() >= 28.0, "inside the insets: {card:?}");
    let covered = |h: &Headless, card: egui::Rect| -> Vec<String> {
        h.app
            .widgets
            .iter()
            .filter(|(w, r)| w.starts_with("modern:") && w != "modern:canvas" && r.top() < card.top() && r.intersects(card))
            .map(|(w, _)| w.clone())
            .collect()
    };
    assert!(covered(&h, card).is_empty(), "the card covers {:?}", covered(&h, card));
    // Develop's top buttons too
    let first = h.app.session.visible_cloned()[0].0;
    exec(&mut h, "library.select", json!({"ids": [first]}));
    exec(&mut h, "module.develop", json!({}));
    exec(&mut h, "modern.chrome", json!({"on": true}));
    let menu = rect(&h, "modern:menu").expect("Develop's menu button");
    let card = rect(&h, &format!("activity:row:{id}")).expect("the activity card");
    assert!(!menu.intersects(card), "the menu button stays visible: {menu:?} vs {card:?}");
    assert!(covered(&h, card).iter().all(|w| w == "modern:histogram"), "the card covers {:?}", covered(&h, card));
    drop(g);
}

/// Press `id` at `at`, keep the finger still for `frames` frames calling `during` after each, then
/// let go.
fn hold_at(h: &mut Headless, at: egui::Pos2, frames: usize, mut during: impl FnMut(&Headless, usize)) {
    let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    h.app.synthetic.push(egui::Event::PointerMoved(at));
    h.app.synthetic.push(button(true));
    for i in 0..frames {
        h.step();
        during(h, i);
    }
    h.app.synthetic.push(button(false));
    h.settle(SETTLE);
}

/// #1035: a press and hold shows the original for as long as the finger stays down, on the photo
/// (a phone) and on the Before button (a tablet), and the edit comes back on release.
#[test]
fn holding_shows_the_original_until_the_finger_lifts() {
    use crate::state::BeforeAfter;
    for (size, id) in [([400.0, 800.0], "modern:canvas"), ([1200.0, 900.0], "modern:before")] {
        let mut h = develop(size);
        let at = rect(&h, id).unwrap_or_else(|| panic!("{id} on screen")).center();
        let mut shown_at = None;
        hold_at(&mut h, at, 90, |h, i| {
            let original = h.app.ui.before_after == BeforeAfter::Original;
            match shown_at {
                None if original => shown_at = Some(i),
                Some(first) => assert!(original, "{id}: the original went away at frame {i} (shown at {first}) while still held"),
                None => {}
            }
        });
        assert!(shown_at.is_some(), "{id}: holding shows the original");
        assert_eq!(h.app.ui.before_after, BeforeAfter::Off, "{id}: letting go shows the edit again");
    }
}
