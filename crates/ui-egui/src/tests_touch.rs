//! Headless tests of touch (finger / pencil) input on the photo's tools: a touch reaches the
//! drag threshold in one long move, so tools must hit-test and start strokes at the press point,
//! and handles need finger-sized reach.

use std::time::Duration;

use lightcraft_develop::MaskShape;
use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn detail(panel: &str) -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("engine.execute", json!({"command": panel}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h
}

fn develop(h: &Headless) -> lightcraft_develop::DevelopSettings {
    let id = h.app.session.active().expect("active photo");
    (*h.app.session.develop_of(id).unwrap_or_default()).clone()
}

fn widget(h: &mut Headless, id: &str) -> egui::Rect {
    let r = h.request("ui.widgets", json!({"filter": id}), T);
    let w = r["result"].as_array().unwrap().iter().find(|w| w["id"] == id).cloned().unwrap_or_else(|| panic!("{id} on screen: {r}"));
    let v: Vec<f32> = (0..4).map(|i| w["rect"][i].as_f64().unwrap() as f32).collect();
    egui::Rect::from_min_size(egui::pos2(v[0], v[1]), egui::vec2(v[2], v[3]))
}

/// One finger pressed at `from`, moved through `path` (one move per frame), lifted at the end.
fn touch(h: &mut Headless, from: egui::Pos2, path: &[egui::Pos2]) {
    // a finger: no pressure (UIKit reports force for a pencil only)
    let t = |phase, pos| egui::Event::Touch { device_id: egui::TouchDeviceId(1), id: egui::TouchId(1), phase, pos, force: None };
    let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
    h.input([t(egui::TouchPhase::Start, from), egui::Event::PointerMoved(from), button(from, true)]);
    h.step();
    let mut last = from;
    for &p in path {
        h.input([t(egui::TouchPhase::Move, p), egui::Event::PointerMoved(p)]);
        h.step();
        last = p;
    }
    h.input([t(egui::TouchPhase::End, last), button(last, false), egui::Event::PointerGone]);
    h.step();
    h.step();
}

#[test]
fn a_fast_touch_drag_from_a_crop_corner_resizes_the_crop() {
    let mut h = detail("panel.crop");
    let corner = widget(&mut h, "cropHandle:0").center();
    let start = develop(&h).crop.geometry.rect;
    // a finger a few points off the corner, whose first move lands 30 points inside it
    let from = corner + egui::vec2(4.0, 3.0);
    let path: Vec<egui::Pos2> =
        (1..=6).map(|i| from + egui::vec2(30.0, 30.0) * (i as f32 / 2.0).min(1.0) + egui::vec2(10.0, 10.0) * i as f32).collect();
    touch(&mut h, from, &path);
    let r = develop(&h).crop.geometry.rect;
    assert!(r.x0 > start.x0 + 0.02 && r.y0 > start.y0 + 0.02, "the top-left corner moved in: {start:?} → {r:?}");
    assert!((r.x1 - start.x1).abs() < 1e-6 && (r.y1 - start.y1).abs() < 1e-6, "the opposite corner stays: {r:?}");
}

#[test]
fn a_touch_lands_a_crop_corner_from_a_finger_width_away() {
    let mut h = detail("panel.crop");
    let corner = widget(&mut h, "cropHandle:2").center();
    let start = develop(&h).crop.geometry.rect;
    // 18 points off the bottom-right corner: outside a mouse's reach, inside a finger's
    let from = corner + egui::vec2(-13.0, -12.0);
    let path: Vec<egui::Pos2> = (1..=6).map(|i| from + egui::vec2(-12.0, -12.0) * i as f32).collect();
    touch(&mut h, from, &path);
    let r = develop(&h).crop.geometry.rect;
    assert!(r.x1 < start.x1 - 0.02 && r.y1 < start.y1 - 0.02, "the bottom-right corner moved in: {start:?} → {r:?}");
    assert!((r.x0 - start.x0).abs() < 1e-6, "not a move of the whole crop: {r:?}");
}

#[test]
fn a_touch_brush_stroke_starts_where_the_finger_went_down() {
    let mut h = detail("panel.masking");
    let r = h.request("engine.execute", json!({"command": "tool.brush"}), T);
    assert_eq!(r["ok"], true, "{r}");
    let img = h.app.image_rect.expect("image on screen");
    let from = img.left_center() + egui::vec2(img.width() * 0.2, 0.0);
    // the first move is already 40 points along
    let path: Vec<egui::Pos2> = (1..=5).map(|i| from + egui::vec2(40.0 * i as f32, 0.0)).collect();
    touch(&mut h, from, &path);
    let d = develop(&h);
    let MaskShape::Brush { strokes } = &d.masks[0].components[0].shape else { panic!("a brush mask: {:?}", d.masks) };
    let first = strokes[0].points[0];
    let want = (from.x - img.left()) / img.width();
    assert!((first.x as f32 - want).abs() < 0.01, "starts at the press ({want}), not past the drag threshold: {:?}", strokes[0].points);
    h.settle(SETTLE);
}

/// A bare egui menu (Root ▸ Sub ▸ Item) driven by touch taps as egui-winit reports them: the
/// release and `PointerGone` arrive in the same frame. With `hook`, the app's input hook runs.
struct MenuProbe {
    ctx: egui::Context,
    time: f64,
    deferred: bool,
    hook: bool,
    chosen: u32,
    rects: [Option<egui::Rect>; 3],
}

impl MenuProbe {
    fn new(hook: bool) -> Self {
        Self { ctx: egui::Context::default(), time: 0.0, deferred: false, hook, chosen: 0, rects: [None; 3] }
    }

    fn frame(&mut self, events: Vec<egui::Event>) {
        let mut raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))),
            time: Some(self.time),
            events,
            ..Default::default()
        };
        if self.hook {
            crate::defer_touch_pointer_gone(&mut self.deferred, &mut raw);
        }
        self.time += 0.1;
        let mut rects = [None; 3];
        let mut chosen = 0;
        let mut out = self.ctx.run_ui(raw, |ui| {
            let root = ui.menu_button("Root", |ui| {
                let sub = ui.menu_button("Sub", |ui| {
                    let b = ui.button("Item");
                    rects[2] = Some(b.rect);
                    if b.clicked() {
                        chosen += 1;
                    }
                });
                rects[1] = Some(sub.response.rect);
            });
            rects[0] = Some(root.response.rect);
        });
        out.textures_delta.clear();
        self.rects = rects;
        self.chosen += chosen;
    }

    fn tap(&mut self, at: egui::Pos2) {
        let t = |phase| egui::Event::Touch { device_id: egui::TouchDeviceId(1), id: egui::TouchId(1), phase, pos: at, force: None };
        let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        self.frame(vec![t(egui::TouchPhase::Start), egui::Event::PointerMoved(at), button(true)]);
        self.frame(vec![t(egui::TouchPhase::End), button(false), egui::Event::PointerGone]);
        self.frame(vec![]);
        self.frame(vec![]);
    }

    /// Taps Root, Sub and Item; returns whether the menu is still open afterwards.
    fn choose(&mut self) -> bool {
        self.frame(vec![]);
        self.tap(self.rects[0].expect("Root").center());
        self.tap(self.rects[1].expect("Sub open").center());
        self.tap(self.rects[2].expect("Item open").center());
        self.rects[2].is_some()
    }
}

#[test]
fn a_tap_on_a_submenu_item_chooses_it_and_closes_the_menu() {
    let mut m = MenuProbe::new(true);
    let still_open = m.choose();
    assert_eq!(m.chosen, 1, "the item was chosen");
    assert!(!still_open, "the menu closes after a tap on an item, as after a click");
    // and the pointer does go away once the finger is up
    assert!(m.ctx.pointer_hover_pos().is_none());
}

#[test]
fn without_the_hook_egui_keeps_the_menu_open_after_a_tap() {
    // documents the egui behaviour the hook works around (if egui fixes it, drop the hook)
    let mut m = MenuProbe::new(false);
    assert!(m.choose() && m.chosen == 1);
}

#[test]
fn settings_fits_an_ipad_screen_with_a_long_library_path() {
    // an iOS library lives deep in the app container: a long path, with no spaces to wrap at
    let dir = std::env::temp_dir()
        .join(format!("lc-ui-longpath-{}", std::process::id()))
        .join("Containers-Data-Application-CD08FC52-6DA6-4AEA-8F6B-561AF120B000-Documents-LightCraft-Library");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut session = lightcraft_engine::Session::new().with_fs();
    session.open_library(&dir, false).unwrap();
    let app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1194.0, 834.0], 1.0);
    h.app.ui.dialog = Some(crate::state::Dialog::Settings { tab: "general".into() });
    for _ in 0..4 {
        h.step();
    }
    let rect = h.app.widgets.iter().find(|(w, _)| w == "dialog:window").map(|(_, r)| *r).expect("the dialog is open");
    assert!(rect.left() >= 0.0 && rect.right() <= 1194.0, "the dialog stays on screen: {rect:?}");
    assert!(rect.width() <= 700.0, "the path doesn't stretch the dialog: {rect:?}");
    let _ = h.app.session.close_library();
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
}

#[test]
fn the_bottom_bar_groups_never_overlap_on_an_ipad_in_portrait() {
    let services = Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    // iPad 11" portrait with My Photos open: the narrowest non-compact layout
    let mut h = Headless::new(app, [834.0, 1194.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.app.ui.left_panel = true;
    for _ in 0..3 {
        h.step();
    }
    // the left group ends with Sort, or with the one View button that holds the modes and Sort
    // where the canvas is too narrow for them
    let id = if h.app.widgets.iter().any(|(w, _)| w == "icon:sort") { "icon:sort" } else { "icon:viewOptions" };
    let sort = widget(&mut h, id);
    let right: Vec<(String, egui::Rect)> = h
        .app
        .widgets
        .iter()
        .filter(|(id, r)| {
            ["dropdown:zoom", "dropdown:clickZoom", "icon:faceBoxes", "icon:filmstrip", "icon:beforeAfter"].contains(&id.as_str())
                && r.center().y > sort.top()
        })
        .cloned()
        .collect();
    assert!(right.iter().any(|(id, _)| id == "dropdown:zoom"), "the zoom stays: {right:?}");
    for (id, r) in &right {
        assert!(r.left() >= sort.right(), "{id} {r:?} overlaps the left group {sort:?}");
    }
}

#[test]
fn clearing_filters_from_the_photo_empties_the_search_field() {
    let services = Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1194.0, 834.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    let field = widget(&mut h, "field:search").center();
    touch(&mut h, field, &[]);
    let r = h.request("ui.text", json!({"text": "desert"}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("ui.key", json!({"key": "Enter"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(h.app.ui.search, "desert");
    // the "Filtered: …" notice on the photo clears every filter
    let x = widget(&mut h, "loupe:filters").center();
    touch(&mut h, x, &[]);
    for _ in 0..3 {
        h.step();
    }
    assert_eq!(h.app.session.filter, Default::default(), "the filter is gone");
    assert_eq!(h.app.ui.search, "", "and so is the text in the field");
}

#[test]
fn a_tap_on_a_compare_caption_star_rates_that_photo() {
    let services = Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1194.0, 834.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("engine.execute", json!({"command": "view.compare"}), T);
    assert_eq!(r["ok"], true, "{r}");
    // the mobile shell announces the touch screen at launch with a cancelled touch
    h.input([egui::Event::Touch {
        device_id: egui::TouchDeviceId(0),
        id: egui::TouchId(0),
        phase: egui::TouchPhase::Cancel,
        pos: egui::Pos2::ZERO,
        force: None,
    }]);
    h.settle(SETTLE);
    let stars: Vec<String> =
        h.app.widgets.iter().filter(|(id, _)| id.starts_with("cullStar:") && id.ends_with(":4")).map(|(id, _)| id.clone()).collect();
    assert_eq!(stars.len(), 2, "Select and Candidate each have stars: {stars:?}");
    for sid in stars {
        let photo = lightcraft_catalog::PhotoId(sid.split(':').nth(1).unwrap().parse().unwrap());
        let before = h.app.session.catalog.photo(photo).unwrap().rating;
        // re-read: rating one photo makes it the active one, which can move the captions
        let rect = widget(&mut h, &sid);
        touch(&mut h, rect.center(), &[]);
        let want = if before == 4 { 0 } else { 4 };
        assert_eq!(h.app.session.catalog.photo(photo).unwrap().rating, want, "{sid} rates its own photo (from {before})");
    }
}

#[test]
fn a_new_radial_or_linear_gradient_is_drawn_by_dragging_on_the_photo() {
    let mut h = detail("panel.masking");
    let img = h.app.image_rect.expect("image on screen");
    let at = |x: f32, y: f32| egui::pos2(img.left() + x * img.width(), img.top() + y * img.height());
    let r = h.request("ui.clickWidget", json!({"id": "maskNew:radial"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    // centre at (0.3, 0.3), dragged out to (0.4, 0.45)
    let path: Vec<egui::Pos2> = (1..=6).map(|i| at(0.3 + 0.1 * i as f32 / 6.0, 0.3 + 0.15 * i as f32 / 6.0)).collect();
    touch(&mut h, at(0.3, 0.3), &path);
    let d = develop(&h);
    let MaskShape::Radial { center, rx, ry, .. } = d.masks[0].components[0].shape.clone() else { panic!("radial: {:?}", d.masks) };
    assert!((center.x - 0.3).abs() < 0.01 && (center.y - 0.3).abs() < 0.01, "centred where the drag began: {center:?}");
    assert!(rx > 0.03 && ry > 0.05, "reaching the finger: {rx} {ry}");
    // a second drag edits it like any mask (the pin moves it), it isn't drawn again
    touch(&mut h, at(0.3, 0.3), &[at(0.35, 0.3), at(0.5, 0.3)]);
    let MaskShape::Radial { center: c2, rx: rx2, .. } = develop(&h).masks[0].components[0].shape.clone() else { panic!() };
    assert!((c2.x - 0.5).abs() < 0.01 && (rx2 - rx).abs() < 1e-9, "moved by its pin: {c2:?}");

    let r = h.request("ui.clickWidget", json!({"id": "maskNew:linear"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    touch(&mut h, at(0.6, 0.2), &[at(0.6, 0.3), at(0.6, 0.5), at(0.6, 0.7)]);
    let d = develop(&h);
    let MaskShape::Linear { start, end } = d.masks[1].components[0].shape.clone() else { panic!("linear: {:?}", d.masks) };
    assert!((start.y - 0.2).abs() < 0.01 && (end.y - 0.7).abs() < 0.01, "from press to release: {start:?} {end:?}");
    h.settle(SETTLE);
}

#[test]
fn the_export_dialog_fits_an_ipad_mini_in_landscape() {
    let services = Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1133.0, 744.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("ui.menu.invoke", json!({"command": "dialog.export"}), T);
    assert_eq!(r["ok"], true, "{r}");
    for _ in 0..4 {
        h.step();
    }
    let rect = h.app.widgets.iter().find(|(w, _)| w == "dialog:window").map(|(_, r)| *r).expect("the dialog is open");
    assert!(rect.top() >= 0.0 && rect.bottom() <= 744.0, "the whole dialog is on screen (it scrolls inside): {rect:?}");
}

#[test]
fn pencil_pressure_sizes_the_brush_and_a_finger_paints_evenly() {
    let mut h = detail("panel.masking");
    let r = h.request("engine.execute", json!({"command": "tool.brush"}), T);
    assert_eq!(r["ok"], true, "{r}");
    let img = h.app.image_rect.expect("image on screen");
    let at = |x: f32| egui::pos2(img.left() + x * img.width(), img.center().y);
    let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
    let pen = |phase, pos, force| egui::Event::Touch { device_id: egui::TouchDeviceId(1), id: egui::TouchId(7), phase, pos, force: Some(force) };
    let undo_before = h.app.session.undo.len();
    // light, then firm
    h.input([pen(egui::TouchPhase::Start, at(0.2), 0.1), egui::Event::PointerMoved(at(0.2)), button(at(0.2), true)]);
    h.step();
    for (i, f) in [0.1, 0.1, 0.1, 1.0, 1.0, 1.0].into_iter().enumerate() {
        let q = at(0.25 + 0.05 * i as f32);
        h.input([pen(egui::TouchPhase::Move, q, f), egui::Event::PointerMoved(q)]);
        h.step();
    }
    let end = at(0.25 + 0.05 * 5.0);
    h.input([pen(egui::TouchPhase::End, end, 1.0), button(end, false), egui::Event::PointerGone]);
    h.step();
    h.step();
    let d = develop(&h);
    let MaskShape::Brush { strokes } = &d.masks[0].components[0].shape else { panic!("a brush mask: {:?}", d.masks) };
    assert_eq!(strokes.len(), 1, "{strokes:?}");
    let p = &strokes[0].pressure;
    assert_eq!(p.len(), strokes[0].points.len(), "a pressure per point: {strokes:?}");
    assert!(p[0] < 0.5 && p[p.len() - 1] > 0.9, "light, then firm: {p:?}");
    assert_eq!(h.app.session.undo.len(), undo_before + 1, "one undo step for the stroke");
    // a finger has no pressure: the stroke paints evenly
    touch(&mut h, at(0.2), &[at(0.3), at(0.4)]);
    let d = develop(&h);
    let MaskShape::Brush { strokes: after } = &d.masks[0].components[0].shape else { panic!() };
    assert_eq!(after.len(), strokes.len() + 1);
    assert!(after[after.len() - 1].pressure.is_empty(), "{after:?}");
    h.settle(SETTLE);
}

#[test]
fn a_pencil_double_tap_swaps_the_brush_between_add_and_erase() {
    let mut h = detail("panel.masking");
    let ctx = h.view.ctx.clone();
    assert!(!h.app.pencil_tap(&ctx), "nothing to switch without the brush");
    let r = h.request("engine.execute", json!({"command": "tool.brush"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(!h.app.ui.brush_erase);
    assert!(h.app.pencil_tap(&ctx));
    assert!(h.app.ui.brush_erase, "erases");
    assert!(h.app.pencil_tap(&ctx));
    assert!(!h.app.ui.brush_erase, "and back");
}

#[test]
fn pencil_pressure_survives_frames_without_touch_events_and_a_zero_first_contact() {
    // on a device the app redraws between the Pencil's reports (renders in flight), and a Pencil's
    // first contact can report no force yet
    let mut h = detail("panel.masking");
    let r = h.request("engine.execute", json!({"command": "tool.brush"}), T);
    assert_eq!(r["ok"], true, "{r}");
    let img = h.app.image_rect.expect("image on screen");
    let at = |x: f32| egui::pos2(img.left() + x * img.width(), img.center().y);
    let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
    let pen = |phase, pos, force| egui::Event::Touch { device_id: egui::TouchDeviceId(1), id: egui::TouchId(7), phase, pos, force: Some(force) };
    h.input([pen(egui::TouchPhase::Start, at(0.2), 0.0), egui::Event::PointerMoved(at(0.2)), button(at(0.2), true)]);
    h.step();
    h.step();
    for (i, f) in [0.1, 0.1, 0.1, 1.0, 1.0, 1.0].into_iter().enumerate() {
        let q = at(0.25 + 0.05 * i as f32);
        h.input([pen(egui::TouchPhase::Move, q, f), egui::Event::PointerMoved(q)]);
        h.step();
        // a redraw with no new touch
        h.step();
    }
    let end = at(0.25 + 0.05 * 5.0);
    h.input([pen(egui::TouchPhase::End, end, 1.0), button(end, false), egui::Event::PointerGone]);
    h.step();
    h.step();
    let d = develop(&h);
    let MaskShape::Brush { strokes } = &d.masks[0].components[0].shape else { panic!("a brush mask: {:?}", d.masks) };
    let p = &strokes[0].pressure;
    assert_eq!(p.len(), strokes[0].points.len(), "every point has a pressure, redraws included: {strokes:?}");
    assert!(p.iter().all(|&f| f > 0.0), "the zero first contact counts as no reading: {p:?}");
    assert!(p[0] < 0.5 && p[p.len() - 1] > 0.9, "light, then firm: {p:?}");
    h.settle(SETTLE);
}

#[test]
fn a_menu_taller_than_the_screen_scrolls_to_its_last_item() {
    let services = Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    // an 11" iPad in landscape: the View menu is taller than the room below the menu bar
    let (w, hgt) = (1194.0, 834.0);
    let mut h = Headless::new(app, [w, hgt], 1.0);
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("ui.clickWidget", json!({"id": "menu:View"}), T);
    assert_eq!(r["ok"], true, "the in-window menu bar: {r}");
    h.step();
    let list = widget(&mut h, "menu-level:1");
    assert!(list.bottom() <= hgt, "the menu stays on screen: {list:?}");
    assert!(list.bottom() > hgt - 60.0, "and uses the room below the menu bar: {list:?}");
    // scroll the list down to its last item
    let last = "menuItem:view.filterBar";
    for _ in 0..40 {
        if widget(&mut h, last).bottom() <= list.bottom() {
            break;
        }
        let r = h.request("ui.move", json!({"x": list.center().x, "y": list.center().y}), T);
        assert_eq!(r["ok"], true, "{r}");
        let r = h.request("ui.scroll", json!({"dx": 0.0, "dy": -80.0}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
    }
    let item = widget(&mut h, last);
    assert!(item.top() >= list.top() && item.bottom() <= list.bottom() + 0.5, "the last item scrolled into view: {item:?} in {list:?}");
    let before = h.app.ui.filter_bar;
    let r = h.request("ui.clickWidget", json!({"id": last}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_ne!(h.app.ui.filter_bar, before, "and it works");
}

/// A finger dragged up the photo grid scrolls it, even when it lands on a photo: on a touch
/// screen the photos don't take the drag (that would drag them to an album), so the grid gets it;
/// a long press opens the photo's menu instead (Add to Album …).
#[test]
fn a_touch_drag_on_a_grid_photo_scrolls_the_grid() {
    let services = Services { png: None, ..Default::default() };
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    app.ui.view = crate::state::ViewMode::PhotoGrid;
    let mut h = Headless::new(app, [1194.0, 834.0], 1.0);
    // the mobile shell announces the touch screen before the first touch
    h.input([egui::Event::Touch {
        device_id: egui::TouchDeviceId(0),
        id: egui::TouchId(0),
        phase: egui::TouchPhase::Cancel,
        pos: egui::Pos2::ZERO,
        force: None,
    }]);
    h.settle(SETTLE);
    let first = h.app.widgets.iter().find(|(w, _)| w.starts_with("thumb:")).map(|(_, r)| *r).expect("a photo in the grid");
    let before = h.app.grid_scroll.unwrap_or_default();
    let from = first.center() + egui::vec2(0.0, 300.0).min(egui::vec2(0.0, 834.0 - first.center().y - 60.0));
    let t = |phase, pos| egui::Event::Touch { device_id: egui::TouchDeviceId(1), id: egui::TouchId(1), phase, pos, force: None };
    let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
    h.input([t(egui::TouchPhase::Start, from), egui::Event::PointerMoved(from), button(from, true)]);
    h.step();
    let mut p = from;
    for _ in 0..10 {
        p -= egui::vec2(0.0, 25.0);
        h.input([t(egui::TouchPhase::Move, p), egui::Event::PointerMoved(p)]);
        h.step();
        assert!(h.app.ui.dragging_photos.is_none(), "the finger doesn't drag the photos");
    }
    h.input([t(egui::TouchPhase::End, p), button(p, false), egui::Event::PointerGone]);
    h.step();
    h.step();
    let after = h.app.grid_scroll.unwrap_or_default();
    assert!(after >= before + 150.0, "the grid scrolled with the finger: {before} → {after}");
}

/// A tap in the grid: finger down and up where it landed, then the pointer leaves (a lifted
/// finger), as the mobile shell reports it.
fn tap(h: &mut Headless, at: egui::Pos2) {
    let t = |phase| egui::Event::Touch { device_id: egui::TouchDeviceId(1), id: egui::TouchId(1), phase, pos: at, force: None };
    let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
    h.input([t(egui::TouchPhase::Start), egui::Event::PointerMoved(at), button(true)]);
    h.step();
    h.input([t(egui::TouchPhase::End), button(false)]);
    h.step();
    h.input([egui::Event::PointerGone]);
    h.step();
}

/// Frames for `secs` of headless time.
fn wait(h: &mut Headless, secs: f64) {
    for _ in 0..(secs / crate::headless::FRAME_DT).ceil() as usize {
        h.step();
    }
}

/// Two taps on a photo in the grid open it in Detail, as a double click does. Fingers land a few
/// points apart and tap slower than a mouse double-clicks: the second tap counts when it lands on
/// the same photo soon enough, wherever on it.
#[test]
fn a_double_tap_on_a_grid_photo_opens_it() {
    let services = Services { png: None, ..Default::default() };
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    app.ui.view = crate::state::ViewMode::PhotoGrid;
    let mut h = Headless::new(app, [1194.0, 834.0], 1.0);
    h.input([egui::Event::Touch {
        device_id: egui::TouchDeviceId(0),
        id: egui::TouchId(0),
        phase: egui::TouchPhase::Cancel,
        pos: egui::Pos2::ZERO,
        force: None,
    }]);
    h.settle(SETTLE);
    let (id, r) =
        h.app.widgets.iter().find(|(w, r)| w.starts_with("thumb:") && r.top() > 120.0 && r.bottom() < 700.0).cloned().expect("a photo on screen");
    let id: u64 = id.trim_start_matches("thumb:").parse().unwrap();
    // too slow: two single taps
    tap(&mut h, r.center());
    wait(&mut h, 0.8);
    tap(&mut h, r.center());
    assert_eq!(h.app.ui.view, crate::state::ViewMode::PhotoGrid, "two slow taps only select");
    assert_eq!(h.app.session.active().map(|p| p.0), Some(id));
    wait(&mut h, 0.8);
    tap(&mut h, r.center());
    wait(&mut h, 0.2);
    tap(&mut h, r.center() + egui::vec2(12.0, -8.0));
    h.step();
    assert_eq!(h.app.ui.view, crate::state::ViewMode::Detail, "the second tap opens it");
    assert_eq!(h.app.session.active().map(|p| p.0), Some(id));
}
