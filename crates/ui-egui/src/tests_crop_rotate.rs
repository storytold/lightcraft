//! Rotating in the crop tool (issue #534): outside the crop box the pointer shows that a drag
//! rotates, a drag shows the angle as it changes, and the Straighten value takes an exact angle.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn crop_tool() -> Headless {
    let mut session = lcc_engine::Session::with_demo();
    // newest first, the newest photo open: written around it
    session.execute("library.sort", &json!({"ascending": false})).unwrap();
    session.selection = lcc_engine::Selection::single(session.visible_cloned()[0]);
    let app = LccApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    for (method, params) in [("ui.set", json!({"view": "detail"})), ("engine.execute", json!({"command": "panel.crop"}))] {
        let r = h.request(method, params, T);
        assert_eq!(r["ok"], true, "{r}");
    }
    h.settle(SETTLE);
    h
}

/// Issue #297: a double-click on the photo finishes Guided Upright, as it finishes the crop tool.
#[test]
fn double_click_finishes_guided_upright() {
    let mut h = crop_tool();
    let r = h.request("engine.execute", json!({"command": "tool.guidedUpright"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.tool, "guidedUpright");
    let click = json!({"kind": "down", "x": 0.5, "y": 0.5});
    let up = json!({"kind": "up", "x": 0.5, "y": 0.5});
    let r = h.request("ui.pointer", json!({"events": [click, up, click, up]}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(h.app.ui.tool.is_empty(), "the tool finished");
    assert_eq!(h.app.ui.right, crate::state::RightPanel::Edit);
}

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

fn angle(h: &Headless) -> f64 {
    let id = h.app.session.active().expect("a photo");
    h.app.session.catalog.photo(id).expect("the photo").develop.crop.geometry.angle
}

/// The crop box lies inside the rotated photo (issue #742: a typed or slid angle shrinks it to fit,
/// as the rotate handle does).
fn crop_fits(h: &Headless) -> bool {
    let id = h.app.session.active().expect("a photo");
    let p = h.app.session.catalog.photo(id).expect("the photo");
    let (w, h) = (p.width as f64, p.height as f64);
    let (w, h) = if p.develop.orientation.swaps_axes() { (h, w) } else { (w, h) };
    let g = p.develop.crop.geometry;
    g.rect != lcc_geom::Rect::UNIT && g.is_within_image(w, h)
}

fn hover(h: &mut Headless, p: egui::Pos2) {
    let r = h.request("ui.move", json!({"x": p.x, "y": p.y}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
}

/// The pointer in the crop tool, in one session. Outside the crop box, where a drag rotates, it is a curved double
/// arrow drawn at the pointer (no system cursor has one); inside it is the move cursor. A drag outside rotates and
/// shows the angle next to the pointer until released; closing the tool mid-rotation (Return) leaves nothing
/// behind; with ⌘ held a drag draws a level line, so the pointer is the crosshair.
#[test]
fn the_pointer_shows_rotation_outside_the_box_and_the_angle_while_rotating() {
    let mut h = crop_tool();
    let img = h.app.image_rect.expect("the photo on screen");
    hover(&mut h, egui::pos2(img.left() - 24.0, img.center().y));
    assert_eq!(h.last_cursor, egui::CursorIcon::None, "the system cursor gives way to the glyph");
    assert!(has(&h, "cropRotateCursor"), "the rotation glyph is drawn at the pointer");
    hover(&mut h, img.center());
    assert_eq!(h.last_cursor, egui::CursorIcon::Move, "inside the box");
    assert!(!has(&h, "cropRotateCursor"), "no glyph inside the box");

    let rotate =
        json!({"events": [{"kind": "down", "x": -0.05, "y": 0.2}, {"kind": "drag", "x": -0.05, "y": 0.25}, {"kind": "drag", "x": -0.05, "y": 0.3}]});
    let r = h.request("ui.pointer", rotate.clone(), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(angle(&h) != 0.0, "the drag rotated the crop");
    assert!(has(&h, "cropAngleReadout"), "the angle is shown while rotating");
    let r = h.request("ui.pointer", json!({"events": [{"kind": "up", "x": -0.05, "y": 0.3}]}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(!has(&h, "cropAngleReadout"), "and gone once released");

    // a rotation cut short: reopened, the box shows the move pointer and no angle
    let r = h.request("ui.pointer", rotate, T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(has(&h, "cropAngleReadout"), "rotating again");
    for (method, params) in [
        ("engine.execute", json!({"command": "tool.done"})),
        ("ui.pointer", json!({"events": [{"kind": "up", "x": -0.05, "y": 0.3}]})),
        ("engine.execute", json!({"command": "panel.crop"})),
    ] {
        let r = h.request(method, params, T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
    }
    let img = h.app.image_rect.expect("the photo on screen");
    hover(&mut h, img.center());
    assert_eq!(h.last_cursor, egui::CursorIcon::Move, "the move pointer after reopening");
    assert!(!has(&h, "cropAngleReadout"), "no angle");
    assert!(!has(&h, "cropRotateCursor"), "no rotation glyph");

    h.events.push(egui::Event::ModifiersChanged(egui::Modifiers::COMMAND));
    h.events.push(egui::Event::PointerMoved(egui::pos2(img.left() - 24.0, img.center().y)));
    h.settle(SETTLE);
    assert_eq!(h.last_cursor, egui::CursorIcon::Crosshair, "⌘ held: the straighten crosshair");
    assert!(!has(&h, "cropRotateCursor"), "no rotation glyph with ⌘ held");
}

/// Click the Angle value, clear what is there and type `text` (not yet applied).
fn type_angle(h: &mut Headless, text: &[&str]) {
    let r = h.request("ui.clickWidget", json!({"id": "sliderValue:crop.angle"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    for key in ["a", "Backspace"] {
        let r = h.request("ui.key", json!({"key": key, "cmd": key == "a"}), T);
        assert_eq!(r["ok"], true, "{r}");
    }
    h.settle(SETTLE);
    for t in text {
        let r = h.request("ui.text", json!({"text": t}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
    }
}

fn key(h: &mut Headless, key: &str) {
    let r = h.request("ui.key", json!({"key": key}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
}

/// The Crop panel's Angle value, in one session: it takes an exact angle as people type it (a decimal comma, a
/// degree sign, past ±45° held at the limit) and shrinks the crop to fit (issue #742); typing applies once, on
/// Return or on leaving the field, as one undo step, and Esc gives it up; it writes a sign on a turned angle; a
/// drag on it is one undo step.
#[test]
fn the_angle_field_takes_typed_angles_and_applies_them_once() {
    let mut h = crop_tool();
    for (typed, want) in [("-3,25", -3.25), ("1.5°", 1.5), ("60", 45.0)] {
        type_angle(&mut h, &[typed]);
        key(&mut h, "Enter");
        assert!((angle(&h) - want).abs() < 1e-9, "{typed:?}: want {want}, got {}", angle(&h));
        assert!(crop_fits(&h), "{typed:?}: the Angle field shrinks the crop to fit the photo");
    }
    // one key at a time: no undo step or render per keystroke, no half-typed angles (-3° on the way to -3.25°)
    type_angle(&mut h, &[]);
    let (before, undo) = (angle(&h), h.app.session.undo.len());
    for ch in ["-", "3", ".", "2", "5"] {
        let r = h.request("ui.text", json!({"text": ch}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
    }
    assert_eq!((angle(&h), h.app.session.undo.len()), (before, undo), "nothing applied while typing");
    key(&mut h, "Enter");
    assert!((angle(&h) + 3.25).abs() < 1e-9, "applied on Return: {}", angle(&h));
    assert_eq!(h.app.session.undo.len(), undo + 1, "as one undo step");
    // Esc gives up the typed angle and only leaves the field
    type_angle(&mut h, &["7"]);
    let (before, undo) = (angle(&h), h.app.session.undo.len());
    key(&mut h, "Escape");
    assert_eq!((angle(&h), h.app.session.undo.len()), (before, undo), "Esc applied nothing");
    assert_eq!(h.app.ui.right, crate::state::RightPanel::Crop, "and only left the field");
    // leaving another way (Tab) applies, as Return does
    type_angle(&mut h, &["4.5"]);
    let undo = h.app.session.undo.len();
    key(&mut h, "Tab");
    assert!((angle(&h) - 4.5).abs() < 1e-9, "applied on leaving: {}", angle(&h));
    assert_eq!(h.app.session.undo.len(), undo + 1, "leaving is one undo step");
    // the field writes the angle as the readout does: its number follows its label (its "°" is painted on its own)
    let r = h.request("engine.execute", json!({"command": "crop.straighten", "params": {"angle": 2.5}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let texts = h.painted_text();
    let field = texts.iter().position(|t| t == "Angle").and_then(|i| texts.get(i + 1));
    assert_eq!(field.map(String::as_str), Some("+2.50"), "{texts:?}");
    // a drag on the value is one undo step, like the slider
    let undo = h.app.session.undo.len();
    let r = h.request("ui.dragWidget", json!({"id": "sliderValue:crop.angle", "dx": 40.0, "dy": 0.0, "steps": 6}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(angle(&h) > 2.5, "dragged right: {}", angle(&h));
    assert_eq!(h.app.session.undo.len(), undo + 1, "one step for the whole drag");
}

/// The angle next to the pointer reads like the Straighten value, in degrees.
#[test]
fn the_angle_reads_like_the_straighten_value() {
    use crate::panels::detail::crop_angle_label;
    assert_eq!(crop_angle_label(0.0), "0.00°", "as the Angle field shows it at rest");
    assert_eq!(crop_angle_label(2.5), "+2.50°");
    assert_eq!(crop_angle_label(-12.254), "-12.25°");
    assert_eq!(crop_angle_label(-0.001), "0.00°", "no minus zero, and every zero the same");
}

/// Rotating with the pointer at the canvas's corner keeps the angle readout on the canvas (and so
/// on screen), rather than spilling past its edge.
#[test]
fn the_readout_stays_on_the_canvas() {
    let mut h = crop_tool();
    let img = h.app.image_rect.expect("the photo on screen");
    let canvas = h.app.canvas_rect.expect("the canvas");
    let corner = canvas.right_bottom() - egui::vec2(4.0, 4.0);
    let (x, y) = ((corner.x - img.left()) / img.width(), (corner.y - img.top()) / img.height());
    let r = h.request(
        "ui.pointer",
        json!({"events": [{"kind": "down", "x": x, "y": y - 0.1}, {"kind": "drag", "x": x, "y": y - 0.05}, {"kind": "drag", "x": x, "y": y}]}),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let readout = h.app.widgets.iter().find(|(w, _)| w == "cropAngleReadout").map(|(_, r)| *r).expect("the readout while rotating");
    assert!(canvas.contains_rect(readout), "{readout:?} in {canvas:?}");
}

/// Rotating in the margin beside a narrow photo keeps the readout next to the pointer: it is kept
/// on the canvas, not pinned to the photo's edge.
#[test]
fn the_readout_follows_the_pointer_in_the_margin() {
    let mut h = crop_tool();
    let img = h.app.image_rect.expect("the photo on screen");
    let canvas = h.app.canvas_rect.expect("the canvas");
    assert!(img.left() - canvas.left() > 150.0, "this test needs a margin beside the photo");
    let at = egui::pos2(canvas.left() + 40.0, img.center().y);
    let x = (at.x - img.left()) / img.width();
    let r = h.request(
        "ui.pointer",
        json!({"events": [{"kind": "down", "x": x, "y": 0.4}, {"kind": "drag", "x": x, "y": 0.45}, {"kind": "drag", "x": x, "y": 0.5}]}),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let readout = h.app.widgets.iter().find(|(w, _)| w == "cropAngleReadout").map(|(_, r)| *r).expect("the readout while rotating");
    assert!(readout.distance_to_pos(at) < 60.0, "{readout:?} near {at:?}");
}

/// Placing the readout never panics, whatever the bounds (a NaN or an empty rect).
#[test]
fn placing_the_readout_never_panics() {
    use crate::panels::detail::readout_rect;
    let size = egui::vec2(50.0, 20.0);
    for bounds in [egui::Rect::NAN, egui::Rect::NOTHING, egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(10.0, 10.0))] {
        let r = readout_rect(egui::pos2(5.0, 5.0), size, bounds);
        assert_eq!(r.size(), size, "{bounds:?}");
    }
    let r = readout_rect(egui::pos2(f32::NAN, 5.0), size, egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0)));
    assert_eq!(r.size(), size);
}

/// A move or resize drag keeps its own pointer when it runs past the box (the box stops at the
/// photo's edge): the rotation glyph would say the drag rotates.
#[test]
fn moving_the_box_never_shows_rotation() {
    let mut h = crop_tool();
    let r = h.request(
        "ui.pointer",
        json!({"events": [{"kind": "down", "x": 0.5, "y": 0.5}, {"kind": "drag", "x": 0.8, "y": 0.5}, {"kind": "drag", "x": 1.2, "y": 0.5}]}),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.last_cursor, egui::CursorIcon::Move, "still moving");
    assert!(!has(&h, "cropRotateCursor"));
    assert_eq!(angle(&h), 0.0, "nothing rotated");
}

/// The crop tool is Lightroom Classic's Crop & Straighten drawer: Aspect and its lock, Angle with
/// the straighten tool and Auto, Constrain to Image, Reset and Close, plus rotate and flip buttons
/// (issue #705; also in the Photo menu). The guide overlays stay in the View menu.
#[test]
fn the_crop_tool_is_lightroom_classics_drawer() {
    let mut h = crop_tool();
    for id in ["cropLock", "straightenTool", "straightenAuto", "cropReset", "cropClose", "cropRotateLeft", "cropFlipV"] {
        assert!(has(&h, &format!("button:{id}")), "{id} is in the drawer");
    }
    assert!(!has(&h, "button:cropOverlay-grid"), "the overlays stay in the View menu");
    let id = h.app.session.active().expect("a photo");
    let flipped = |h: &Headless| h.app.session.catalog.photo(id).map(|p| p.develop.crop.flip_h);
    let before = flipped(&h);
    let r = h.request("ui.clickWidget", json!({"id": "button:cropFlipH"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_ne!(flipped(&h), before, "the drawer's Flip Horizontal flips the photo");
    let r = h.request("engine.execute", json!({"command": "view.cropOverlay"}), T);
    assert_eq!(r["ok"], true, "{r}");
    // Close leaves the crop tool for the Edit panel
    let r = h.request("ui.clickWidget", json!({"id": "button:cropClose"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.right, crate::state::RightPanel::Edit);
}

fn crop_rect(h: &Headless) -> lcc_geom::Rect {
    let id = h.app.session.active().expect("a photo");
    h.app.session.catalog.photo(id).expect("the photo").develop.crop.geometry.rect
}

/// Press where the pointer shows the resize cursor for the left edge handle, `dx` px from the
/// edge, and drag on by `step` px per event: the drag must resize, as the cursor said (issue #1930).
fn drag_left_handle(dx: f32, step: f32) -> Headless {
    let mut h = crop_tool();
    let img = h.app.image_rect.expect("the photo on screen");
    hover(&mut h, egui::pos2(img.left() + dx, img.center().y));
    assert_eq!(h.last_cursor, egui::CursorIcon::ResizeHorizontal, "the pointer shows a resize");
    let x = |px: f32| px / img.width();
    let events: Vec<_> = std::iter::once(json!({"kind": "down", "x": x(dx), "y": 0.5}))
        .chain((1..=4).map(|i| json!({"kind": "drag", "x": x(dx + step * i as f32), "y": 0.5})))
        .collect();
    let r = h.request("ui.pointer", json!({ "events": events }), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h
}

/// Just outside the box, inside the handle's reach: the drag resizes, it does not rotate.
#[test]
fn a_drag_from_just_outside_a_handle_resizes_not_rotates() {
    let h = drag_left_handle(-8.0, -3.0);
    assert_eq!(angle(&h), 0.0, "the resize cursor showed, so nothing rotates");
    assert!(!has(&h, "cropAngleReadout"), "no rotation under way");
}

/// Just inside the box, inside the handle's reach: the drag moves the left edge, not the box.
#[test]
fn a_drag_from_just_inside_a_handle_resizes_not_moves() {
    let h = drag_left_handle(8.0, 3.0);
    let r = crop_rect(&h);
    assert!(r.x0 > 0.0, "the left edge moved in: {r:?}");
    assert!((r.x1 - 1.0).abs() < 1e-6, "the right edge stays: {r:?}");
}

/// Upright and the transform sliders are the Edit panel's Transform section, as Lightroom Classic's
/// Transform panel, after the lens corrections: `section.geometry` opens it, and an Upright mode
/// set there applies.
#[test]
fn transform_is_a_section_of_the_edit_panel() {
    let mut h = crop_tool();
    assert!(!has(&h, "button:upright-auto"), "not in the crop tool");
    // (⌥-click's solo: only Transform open, so its rows are on screen)
    let r = h.request("engine.execute", json!({"command": "section.geometry", "params": {"solo": true}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.right, crate::state::RightPanel::Edit);
    // the section's rows can sit below the fold (more sections above it): scroll the panel to them
    if let Some((_, panel)) = h.app.widgets.iter().find(|(w, _)| w == "panel:right_panel").cloned() {
        h.request("ui.move", json!({"x": panel.center().x, "y": panel.center().y}), T);
        h.request("ui.scroll", json!({"dy": -300.0}), T);
        h.settle(SETTLE);
    }
    // (Guided: the other modes analyse the photo's pixels, which this harness doesn't decode)
    let r = h.request("ui.clickWidget", json!({"id": "button:upright-guided"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let id = h.app.session.active().expect("a photo");
    assert_eq!(h.app.session.develop_of(id).map(|d| d.geometry.upright), Some(lcc_develop::Upright::Guided));
}

/// Auto on a photo without a horizon or plumb lines says so, instead of leaving the photo as it is
/// without a word.
#[test]
fn auto_straighten_says_when_it_finds_nothing() {
    let dir = lcc_testkit::TempDir::new("lc-ui-auto-straighten");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let img = lcc_raster::Rgba8 { width: 64, height: 48, data: vec![[90, 120, 160, 255]; 64 * 48] };
    let bytes = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &lcc_codecs::EncodeMeta::default()).unwrap();
    let path = dir.join("flat.png");
    std::fs::write(&path, bytes).unwrap();
    let mut session = lcc_engine::Session::new().with_fs();
    session.execute("library.import", &json!({"paths": [path.to_string_lossy()]})).unwrap();
    let app = LccApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    for (method, params) in [("ui.set", json!({"view": "detail"})), ("engine.execute", json!({"command": "panel.crop"}))] {
        let r = h.request(method, params, T);
        assert_eq!(r["ok"], true, "{r}");
    }
    h.settle(SETTLE);
    let r = h.request("ui.clickWidget", json!({"id": "button:straightenAuto"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(h.app.ui.toast().is_some_and(|t| t.0 == "No horizon or vertical lines to level by"), "{:?}", h.app.ui.toasts);
    assert_eq!(angle(&h), 0.0);
    let _ = std::fs::remove_dir_all(dir);
}

/// Note 5 of 2026-10-31: every C (Crop on / off) rendered the loupe again, though the edit did
/// not change. The renderer keeps the last full-quality whole-frame renders: C off → on → off with
/// the edit unchanged shows them at once and asks for no render; an edit renders again.
#[test]
fn crop_on_and_off_with_an_unchanged_edit_renders_nothing_again() {
    use crate::render::Slot;
    let mut h = crop_tool();
    let c = |h: &mut Headless| {
        let r = h.request("engine.execute", json!({"command": "panel.crop"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
    };
    let key = |h: &Headless| h.app.renderer.textures.get(&Slot::Main).map(|t| t.key).expect("a loupe render");
    let r = h.request("engine.execute", json!({"command": "crop.aspect", "params": {"aspect": "4x5"}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let whole = key(&h);
    c(&mut h);
    let cropped = key(&h);
    assert_ne!(whole, cropped, "Crop off renders the cropped view");
    let before = h.app.renderer.main_requested;
    c(&mut h);
    assert_eq!(key(&h), whole, "Crop on shows the kept whole photo");
    c(&mut h);
    assert_eq!(key(&h), cropped, "Crop off shows the kept cropped view");
    assert_eq!(h.app.renderer.main_requested, before, "pressing C rendered again");
    let r = h.request("engine.execute", json!({"command": "develop.set", "params": {"control": "light.exposure", "value": 0.4}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(h.app.renderer.main_requested > before, "an edit renders");
}
