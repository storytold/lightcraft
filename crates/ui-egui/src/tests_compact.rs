//! Headless tests of the compact (phone-width) layout: the photo keeps its room, the right panel
//! and Presets open in a full-width bottom sheet under it, My Photos is a full-width drawer, and a
//! desktop-sized window is unchanged.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::RightPanel;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);
const PHONE: [f32; 2] = [402.0, 874.0];

fn demo(size: [f32; 2], ui: serde_json::Value) -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, size, 1.0);
    let r = h.request("ui.set", ui, T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h
}

fn widget(h: &Headless, id: &str) -> Option<egui::Rect> {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r)
}

fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
}

#[test]
fn phone_width_puts_the_editor_in_a_sheet_under_the_photo() {
    let mut h = demo(PHONE, json!({"view": "detail", "right": "edit", "leftPanel": true}));
    assert!(crate::panels::compact::is_compact(&h.view.ctx));
    // entering the compact layout closed My Photos, which would cover the photo
    assert!(!h.app.ui.left_panel);
    let sheet = widget(&h, "panel:sheet").expect("the edit sheet is up");
    let right = widget(&h, "panel:right_panel").expect("the Edit panel is in the sheet");
    assert!(right.width() >= PHONE[0] - 2.0, "the Edit panel spans the width: {right:?}");
    assert!(sheet.top() > 300.0 && sheet.bottom() <= PHONE[1], "{sheet:?}");
    let canvas = h.app.canvas_rect.expect("the loupe is drawn");
    assert!(canvas.width() >= PHONE[0] - 2.0 && canvas.height() > 250.0, "the photo keeps its room: {canvas:?}");
    assert!(canvas.bottom() <= sheet.top() + 1.0, "the photo sits above the sheet");
    assert!(widget(&h, "slider:light.exposure").is_some(), "the Light sliders are in the sheet");
    // a tool in the sheet's row switches the panel; Presets replaces it at full width
    click(&mut h, "icon:crop");
    assert_eq!(h.app.ui.right, RightPanel::Crop);
    click(&mut h, "icon:presets");
    assert!(h.app.ui.presets);
    let presets = widget(&h, "panel:sheet").expect("still in the sheet");
    assert!(presets.width() >= PHONE[0] - 2.0);
    // Hide puts the sheet away and gives the photo the height back
    click(&mut h, "icon:sheetClose");
    assert!(!h.app.ui.presets && h.app.ui.right == RightPanel::None);
    h.step();
    assert!(widget(&h, "panel:sheet").is_none());
    assert!(h.app.canvas_rect.is_some_and(|c| c.height() > canvas.height() + 150.0));
    // the bottom bar's Edit button brings it back on the Edit panel
    click(&mut h, "icon:edit");
    assert_eq!(h.app.ui.right, RightPanel::Edit);
    assert!(widget(&h, "panel:sheet").is_some());
}

#[test]
fn phone_width_opens_my_photos_as_a_drawer() {
    let mut h = demo(PHONE, json!({"view": "photoGrid"}));
    click(&mut h, "icon:sidebar");
    assert!(h.app.ui.left_panel);
    let left = widget(&h, "panel:left_panel").expect("My Photos is open");
    assert!(left.width() >= PHONE[0] - 2.0, "full width: {left:?}");
    // picking a source closes it onto that source's photos
    click(&mut h, "source:picks");
    assert_eq!(h.app.session.source, lightcraft_engine::LibrarySource::Picks);
    assert!(!h.app.ui.left_panel);
}

#[test]
fn desktop_width_keeps_the_side_panels() {
    let h = demo([1400.0, 900.0], json!({"view": "detail", "right": "edit"}));
    assert!(!crate::panels::compact::is_compact(&h.view.ctx));
    assert!(widget(&h, "panel:sheet").is_none());
    let right = widget(&h, "panel:right_panel").expect("the right panel");
    assert!(right.width() < 600.0 && right.right() > 1300.0, "a column at the right: {right:?}");
    assert!(widget(&h, "icon:discord").is_some(), "the desktop top bar");
}

/// The dialog window's rectangle (`panels::dialogs` gives every dialog this id).
/// The open dialog's window (whichever window id it has: Settings and the import review keep
/// their own).
fn dialog_rect(h: &Headless) -> Option<egui::Rect> {
    h.app.widgets.iter().find(|(w, _)| w == "dialog:window").map(|(_, r)| *r)
}

fn open(h: &mut Headless, command: &str) {
    let r = h.request("engine.execute", json!({"command": command, "params": {}}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
    for _ in 0..4 {
        h.step();
    }
}

#[test]
fn phone_width_dialogs_fit_the_screen() {
    let mut h = demo(PHONE, json!({"view": "detail"}));
    let r = h.request("engine.execute", json!({"command": "library.selectAll", "params": {}}), T);
    assert_eq!(r["ok"], true, "{r}");
    // the wide ones on the desktop: Settings (560 pt), Export (label column + button rows),
    // Shortcuts (a three-column table), Merge (a 520 pt preview beside its options), and two
    // ordinary ones
    for command in ["app.settings", "dialog.export", "app.shortcuts", "dialog.mergeHdr", "dialog.labelNames", "dialog.copySettings"] {
        open(&mut h, command);
        assert!(h.app.ui.dialog.is_some(), "{command} opened a dialog");
        let r = dialog_rect(&h).unwrap_or_else(|| panic!("{command}: no dialog window"));
        assert!(r.left() >= 0.0 && r.right() <= PHONE[0], "{command} fits across: {r:?}");
        assert!(r.top() >= 0.0 && r.bottom() <= PHONE[1], "{command} fits down: {r:?}");
        assert!(r.width() > PHONE[0] - 40.0, "{command} uses the width: {r:?}");
        h.request("ui.dialog.cancel", json!({}), T);
        h.step();
    }
    h.app.ui.dialog = Some(crate::state::Dialog::ConfirmDelete { count: 1 });
    for _ in 0..4 {
        h.step();
    }
    let r = dialog_rect(&h).expect("the delete confirmation");
    assert!(r.left() >= 0.0 && r.right() <= PHONE[0], "{r:?}");
}

#[test]
fn desktop_width_dialogs_keep_their_size() {
    let mut h = demo([1400.0, 900.0], json!({"view": "detail"}));
    open(&mut h, "app.settings");
    let r = dialog_rect(&h).expect("Settings");
    assert!(r.width() >= 560.0 && r.width() < 900.0, "Settings at its desktop width: {r:?}");
    // (in a fresh session: dialogs share one window, which keeps the last one's width)
    let mut h = demo([1400.0, 900.0], json!({"view": "detail"}));
    open(&mut h, "dialog.labelNames");
    let r = dialog_rect(&h).expect("label names");
    assert!(r.width() < 600.0, "an ordinary dialog stays compact on the desktop: {r:?}");
}

fn exposure(h: &Headless) -> f64 {
    let id = h.app.session.active().expect("a photo in Detail");
    h.app.session.develop_of(id).unwrap_or_default().light.exposure
}

/// The edit sheet scrolled (mouse wheel) so the Light sliders are clear of the bottom bar; the
/// Exposure track's rectangle.
fn exposure_in_view(h: &mut Headless) -> egui::Rect {
    let sheet = widget(h, "panel:sheet").expect("the edit sheet");
    let r = h.request("ui.move", json!({"x": sheet.center().x, "y": sheet.center().y}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("ui.scroll", json!({"dy": -220.0}), T);
    assert_eq!(r["ok"], true, "{r}");
    for _ in 0..30 {
        h.step();
    }
    let track = widget(h, "slider:light.exposure").expect("the Exposure slider");
    assert!(track.bottom() < sheet.bottom() - 60.0, "Exposure scrolled into view: {track:?} in {sheet:?}");
    track
}

/// A finger touches the screen (no press: nothing is clicked), so the layout takes its touch-screen
/// sizes before a test measures where things are.
fn touch_screen(h: &mut Headless) {
    let p = egui::pos2(1.0, 1.0);
    let touch = |phase| egui::Event::Touch { device_id: egui::TouchDeviceId(1), id: egui::TouchId(9), phase, pos: p, force: None };
    h.input([touch(egui::TouchPhase::Start), touch(egui::TouchPhase::End)]);
    for _ in 0..3 {
        h.step();
    }
}

/// One finger from `from` to `to` in `steps` moves, then lifted.
fn touch_drag(h: &mut Headless, from: egui::Pos2, to: egui::Pos2, steps: usize) {
    let touch = |phase, pos| egui::Event::Touch { device_id: egui::TouchDeviceId(1), id: egui::TouchId(1), phase, pos, force: None };
    let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
    h.input([touch(egui::TouchPhase::Start, from), egui::Event::PointerMoved(from), button(from, true)]);
    h.step();
    for i in 1..=steps {
        let p = from + (to - from) * (i as f32 / steps as f32);
        h.input([touch(egui::TouchPhase::Move, p), egui::Event::PointerMoved(p)]);
        h.step();
    }
    h.input([touch(egui::TouchPhase::End, to), button(to, false), egui::Event::PointerGone]);
    h.step();
    h.step();
}

#[test]
fn touch_swipe_up_on_a_slider_scrolls_the_sheet_instead() {
    let mut h = demo(PHONE, json!({"view": "detail", "right": "edit"}));
    let before = exposure(&h);
    touch_screen(&mut h);
    let track = exposure_in_view(&mut h);
    // start on the track, left of the thumb, and swipe up
    let from = egui::pos2(track.left() + 20.0, track.center().y);
    touch_drag(&mut h, from, from - egui::vec2(4.0, 160.0), 16);
    assert_eq!(exposure(&h), before, "the value didn't move");
    let moved = widget(&h, "slider:light.exposure").expect("still on screen or scrolled");
    assert!(moved.top() < track.top() - 100.0, "the sheet scrolled: {track:?} → {moved:?}");
}

#[test]
fn touch_swipe_sideways_on_a_slider_adjusts_it_by_the_distance() {
    let mut h = demo(PHONE, json!({"view": "detail", "right": "edit"}));
    let before = exposure(&h);
    touch_screen(&mut h);
    let track = exposure_in_view(&mut h);
    // from near the left end, 40 pt right: by touch the value moves by 40 pt's worth, rather
    // than jumping to the finger (which would set it near the minimum)
    let from = egui::pos2(track.left() + 10.0, track.center().y);
    touch_drag(&mut h, from, from + egui::vec2(40.0, 2.0), 8);
    let after = exposure(&h);
    let spec = lightcraft_develop::controls::find("light.exposure").expect("exposure spec");
    let expected = 40.0 / track.width() as f64 * (spec.max - spec.min);
    assert!(after > before, "it went up: {before} → {after}");
    assert!((after - before - expected).abs() < expected * 0.4, "by about {expected}: {before} → {after}");
    // the slider stayed put: no scrolling
    assert_eq!(widget(&h, "slider:light.exposure").map(|r| r.top()), Some(track.top()));
}

#[test]
fn touch_tap_on_a_slider_leaves_the_value() {
    let mut h = demo(PHONE, json!({"view": "detail", "right": "edit"}));
    let before = exposure(&h);
    touch_screen(&mut h);
    let track = exposure_in_view(&mut h);
    let at = egui::pos2(track.left() + 10.0, track.center().y);
    touch_drag(&mut h, at, at, 0);
    assert_eq!(exposure(&h), before);
}

#[test]
fn mouse_click_on_a_slider_still_sets_the_value_there() {
    // no touch input in this session: the desktop behaviour
    let mut h = demo(PHONE, json!({"view": "detail", "right": "edit"}));
    let before = exposure(&h);
    let track = exposure_in_view(&mut h);
    let r = h.request("ui.click", json!({"x": track.left() + 2.0, "y": track.center().y}), T);
    assert_eq!(r["ok"], true, "{r}");
    for _ in 0..4 {
        h.step();
    }
    assert!(exposure(&h) < before - 1.0, "set near the minimum: {before} → {}", exposure(&h));
}

#[test]
fn mouse_click_on_a_slider_sets_the_value_on_a_touch_screen_too() {
    // the screen has been touched (as the mobile shells announce on their first frame), but the
    // click comes from a trackpad or mouse: it sets the value as on the desktop
    let mut h = demo(PHONE, json!({"view": "detail", "right": "edit"}));
    let before = exposure(&h);
    touch_screen(&mut h);
    let track = exposure_in_view(&mut h);
    let r = h.request("ui.click", json!({"x": track.left() + 2.0, "y": track.center().y}), T);
    assert_eq!(r["ok"], true, "{r}");
    for _ in 0..4 {
        h.step();
    }
    assert!(exposure(&h) < before - 1.0, "set near the minimum: {before} → {}", exposure(&h));
}

/// The soft keyboard, as a phone shell reports it: 330 pt of the screen's bottom.
const KEYBOARD: f32 = 330.0;

fn keyboard_up(h: &mut Headless) {
    h.set_safe_area(egui::SafeAreaInsets(egui::epaint::MarginF32 { bottom: KEYBOARD, ..Default::default() }));
    for _ in 0..8 {
        h.step();
    }
}

fn focus(h: &mut Headless, field: &str) {
    let r = h.request("ui.clickWidget", json!({"id": field}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert!(h.view.ctx.memory(|m| m.focused()).is_some(), "{field} has focus");
}

#[test]
fn keyboard_lifts_the_layout_and_keeps_a_sheet_field_in_view() {
    let mut h = demo(PHONE, json!({"view": "detail", "right": "info"}));
    let sheet = widget(&h, "panel:sheet").expect("the Info sheet");
    // the lowest text field visible in the sheet, which a 330 pt keyboard would cover
    let (field, rect) = h
        .app
        .widgets
        .iter()
        .filter(|(id, r)| id.starts_with("field:") && sheet.contains_rect(*r))
        .max_by(|a, b| a.1.bottom().total_cmp(&b.1.bottom()))
        .map(|(id, r)| (id.clone(), *r))
        .expect("a text field in the Info panel");
    assert!(rect.bottom() > PHONE[1] - KEYBOARD, "{field} starts where the keyboard goes: {rect:?}");
    focus(&mut h, &field);
    keyboard_up(&mut h);
    let content = h.view.ctx.content_rect();
    assert_eq!(content.bottom(), PHONE[1] - KEYBOARD, "the content area ends at the keyboard");
    let bar = widget(&h, "icon:photoGrid").expect("the bottom bar");
    assert!(bar.bottom() <= content.bottom(), "the bottom bar sits on the keyboard: {bar:?}");
    let now = widget(&h, &field).expect("the field");
    assert!(
        now.bottom() <= content.bottom() && now.top() >= h.app.canvas_rect.map_or(0.0, |c| c.bottom()),
        "{field} in view above the keyboard: {now:?}"
    );
}

#[test]
fn keyboard_keeps_a_dialog_field_in_view() {
    let mut h = demo(PHONE, json!({"view": "detail"}));
    open(&mut h, "dialog.rename");
    focus(&mut h, "field:renameTemplate");
    keyboard_up(&mut h);
    let content = h.view.ctx.content_rect();
    let dialog = dialog_rect(&h).expect("the Rename dialog");
    assert!(dialog.bottom() <= content.bottom() && dialog.top() >= 0.0, "the dialog sits above the keyboard: {dialog:?}");
    let field = widget(&h, "field:renameTemplate").expect("the template field");
    assert!(field.bottom() <= content.bottom(), "the field is in view: {field:?}");
}

// ------------------------------------------------------------------ loupe gestures

fn finger(id: u64, phase: egui::TouchPhase, pos: egui::Pos2) -> egui::Event {
    egui::Event::Touch { device_id: egui::TouchDeviceId(1), id: egui::TouchId(id), phase, pos, force: None }
}

/// Two fingers about `centre`, from `from` to `to` points apart (horizontally), in `steps` moves.
fn pinch(h: &mut Headless, centre: egui::Pos2, from: f32, to: f32, steps: usize) {
    let at = |d: f32| (centre - egui::vec2(d / 2.0, 0.0), centre + egui::vec2(d / 2.0, 0.0));
    let (a, b) = at(from);
    let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
    h.input([finger(1, egui::TouchPhase::Start, a), egui::Event::PointerMoved(a), button(a, true), finger(2, egui::TouchPhase::Start, b)]);
    h.step();
    for i in 1..=steps {
        let (a, b) = at(from + (to - from) * i as f32 / steps as f32);
        h.input([finger(1, egui::TouchPhase::Move, a), egui::Event::PointerMoved(a), finger(2, egui::TouchPhase::Move, b)]);
        h.step();
    }
    let (a, b) = at(to);
    h.input([finger(2, egui::TouchPhase::End, b), finger(1, egui::TouchPhase::End, a), button(a, false), egui::Event::PointerGone]);
    h.step();
    h.step();
}

fn loupe(h: &Headless) -> egui::Rect {
    widget(h, "canvas:image").expect("the loupe image")
}

#[test]
fn pinch_zooms_the_loupe_about_the_fingers_and_back_to_fit() {
    let mut h = demo(PHONE, json!({"view": "detail", "right": "none"}));
    let fit = loupe(&h);
    // spread two fingers over a point left of the centre: the image point there stays put
    let centre = fit.center() - egui::vec2(fit.width() / 4.0, 0.0);
    let u = (centre.x - fit.left()) / fit.width();
    pinch(&mut h, centre, 60.0, 180.0, 12);
    assert!(matches!(h.app.ui.zoom, crate::state::Zoom::Percent(_)), "zoomed: {:?}", h.app.ui.zoom);
    let zoomed = loupe(&h);
    assert!(zoomed.width() > fit.width() * 2.5, "about 3× wider: {fit:?} → {zoomed:?}");
    let u_after = (centre.x - zoomed.left()) / zoomed.width();
    assert!((u_after - u).abs() < 0.03, "the point under the fingers stayed: {u} → {u_after}");
    // pinching back in returns to Fit
    pinch(&mut h, centre, 200.0, 40.0, 12);
    assert_eq!(h.app.ui.zoom, crate::state::Zoom::Fit);
    assert_eq!(loupe(&h), fit);
}

#[test]
fn trackpad_pinch_over_the_loupe_zooms() {
    let mut h = demo([1400.0, 900.0], json!({"view": "detail", "right": "none"}));
    let fit = loupe(&h);
    h.input([egui::Event::PointerMoved(fit.center()), egui::Event::Zoom(1.5)]);
    h.step();
    h.step();
    assert!(loupe(&h).width() > fit.width() * 1.4, "{fit:?} → {:?}", loupe(&h));
}

fn swipe_by(h: &mut Headless, delta: egui::Vec2) {
    let start = loupe(h).center();
    touch_drag(h, start, start + delta, 10);
}

#[test]
fn touch_swipe_at_fit_steps_through_the_photos() {
    let mut h = demo(PHONE, json!({"view": "detail", "right": "none"}));
    let first = h.app.session.active();
    swipe_by(&mut h, egui::vec2(-160.0, 6.0));
    let second = h.app.session.active();
    assert_ne!(second, first, "a swipe left shows the next photo");
    swipe_by(&mut h, egui::vec2(160.0, -4.0));
    assert_eq!(h.app.session.active(), first, "a swipe right goes back");
    // too short, or mostly vertical: no change
    swipe_by(&mut h, egui::vec2(-30.0, 0.0));
    swipe_by(&mut h, egui::vec2(-90.0, 160.0));
    assert_eq!(h.app.session.active(), first);
    // the photo is back in place once the finger lifts
    assert_eq!(loupe(&h).center().x, h.app.canvas_rect.expect("canvas").center().x);
}

#[test]
fn mouse_drag_at_fit_doesnt_change_the_photo() {
    let mut h = demo(PHONE, json!({"view": "detail", "right": "none"}));
    let first = h.app.session.active();
    let c = loupe(&h).center();
    let r = h.request("ui.drag", json!({"x": c.x, "y": c.y, "toX": c.x - 160.0, "toY": c.y, "steps": 10}), T);
    assert_eq!(r["ok"], true, "{r}");
    for _ in 0..4 {
        h.step();
    }
    assert_eq!(h.app.session.active(), first);
}

#[test]
fn a_short_dialog_keeps_its_own_height_on_the_desktop() {
    let mut h = demo([1400.0, 900.0], json!({"view": "detail"}));
    open(&mut h, "dialog.labelNames");
    let r = dialog_rect(&h).expect("label names");
    assert!(r.height() < 600.0, "sized to its contents, not the screen: {r:?}");
}

/// A phone held sideways (about 400 pt tall, from the safe area).
const PHONE_LANDSCAPE: [f32; 2] = [874.0, 402.0];

#[test]
fn phone_landscape_puts_the_editor_beside_the_photo() {
    let mut h = demo(PHONE_LANDSCAPE, json!({"view": "detail", "right": "edit"}));
    assert!(crate::panels::compact::is_compact(&h.view.ctx), "a short window is compact too");
    let sheet = widget(&h, "panel:sheet").expect("the edit sheet is up");
    assert!(sheet.width() < PHONE_LANDSCAPE[0] / 2.0 && sheet.right() >= PHONE_LANDSCAPE[0] - 1.0, "a column at the right: {sheet:?}");
    let canvas = h.app.canvas_rect.expect("the loupe is drawn");
    assert!(canvas.right() <= sheet.left() + 1.0, "the photo sits left of the sheet: {canvas:?}");
    assert!(canvas.height() > 250.0 && canvas.width() > 400.0, "the photo keeps its room: {canvas:?}");
    assert!(widget(&h, "slider:light.exposure").is_some(), "the Light sliders are in the sheet");
    // without the sheet the filmstrip is lower than the desktop's, so the photo keeps most of the height
    click(&mut h, "icon:sheetClose");
    h.step();
    let canvas = h.app.canvas_rect.expect("the loupe");
    let t = crate::theme::Tokens::get(&h.view.ctx);
    let cell = h.app.widgets.iter().find(|(w, _)| w.starts_with("film:")).map(|(_, r)| *r).expect("a filmstrip cell");
    assert!(cell.height() < t.film_h - 30.0 && canvas.height() > 2.0 * cell.height(), "{cell:?} under {canvas:?}");
    // My Photos opens as a column beside the photos, not over them
    let mut h = demo(PHONE_LANDSCAPE, json!({"view": "photoGrid"}));
    click(&mut h, "icon:sidebar");
    let left = widget(&h, "panel:left_panel").expect("My Photos is open");
    assert!(left.width() < PHONE_LANDSCAPE[0] / 2.0, "a column: {left:?}");
    // the grid shows more than one row of photos
    let cells: Vec<egui::Rect> = h.app.widgets.iter().filter(|(w, _)| w.starts_with("thumb:")).map(|(_, r)| *r).collect();
    let tops: std::collections::BTreeSet<i32> = cells.iter().map(|r| r.top() as i32).collect();
    assert!(tops.len() >= 2, "two rows of photos at least: {cells:?}");
}

#[test]
fn phone_landscape_dialogs_keep_a_readable_width() {
    let mut h = demo(PHONE_LANDSCAPE, json!({"view": "photoGrid"}));
    open(&mut h, "dialog.export");
    let r = dialog_rect(&h).expect("Export");
    assert!(r.width() <= 570.0 && r.top() >= 0.0 && r.bottom() <= PHONE_LANDSCAPE[1], "{r:?}");
}

#[test]
fn phone_width_filter_bar_wraps_every_control_onto_the_screen() {
    let mut h = demo(PHONE, json!({"view": "photoGrid", "filterBar": true}));
    for _ in 0..4 {
        h.step();
    }
    let controls: Vec<(String, egui::Rect)> = h
        .app
        .widgets
        .iter()
        .filter(|(w, _)| w.starts_with("filter:") || w.starts_with("dropdown:filter-") || w.starts_with("button:filter"))
        .cloned()
        .collect();
    // stars, flags, labels, kind, edited, camera, lens, keyword, save, clear, presets
    assert!(controls.len() >= 20, "{controls:?}");
    for (id, r) in &controls {
        assert!(r.left() >= 0.0 && r.right() <= PHONE[0], "{id} is on screen: {r:?}");
    }
    let bar = widget(&h, "filterbar").expect("the bar");
    assert!(controls.iter().all(|(_, r)| bar.contains_rect(*r)), "inside the bar {bar:?}: {controls:?}");
}

#[test]
fn phone_touch_screen_gets_finger_sized_controls() {
    let mut h = demo(PHONE, json!({"view": "photoGrid"}));
    let mouse = h.view.ctx.global_style().spacing.interact_size.y;
    let p = egui::pos2(200.0, 400.0);
    let touch = |phase| egui::Event::Touch { device_id: egui::TouchDeviceId(1), id: egui::TouchId(1), phase, pos: p, force: None };
    h.input([touch(egui::TouchPhase::Start), touch(egui::TouchPhase::End)]);
    for _ in 0..3 {
        h.step();
    }
    let finger = h.view.ctx.global_style().spacing.interact_size.y;
    assert!(finger >= mouse + 6.0, "{mouse} → {finger}");
    // a desktop-sized window keeps the mouse sizes, touch screen or not
    let mut h = demo([1400.0, 900.0], json!({"view": "photoGrid"}));
    h.input([touch(egui::TouchPhase::Start), touch(egui::TouchPhase::End)]);
    for _ in 0..3 {
        h.step();
    }
    assert_eq!(h.view.ctx.global_style().spacing.interact_size.y, mouse);
}
