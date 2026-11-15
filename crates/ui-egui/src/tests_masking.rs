//! Headless tests of the Masking and Remove tools on the photo: overlay keys, pins, spot editing.

use std::time::Duration;

use lcc_develop::MaskShape;
use serde_json::json;

use crate::headless::Headless;
use crate::state::RightPanel;
use crate::{LccApp, Services};
use lcc_catalog::Rule;

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn detail(panel: &str) -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LccApp::new(lcc_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1200.0, 1000.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail", "moduleBar": false}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("engine.execute", json!({"command": panel}), T);
    assert_eq!(r["ok"], true, "{r}");
    h
}

fn exec(h: &mut Headless, command: &str, params: serde_json::Value) -> serde_json::Value {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
    r["result"].clone()
}

/// The loupe in Library, whose left column carries the folder tree and the collections the sidebar
/// tests browse: Develop's left column is its own Classic stack and has neither.
fn library_loupe() -> Headless {
    let mut h = detail("panel.edit");
    exec(&mut h, "module.library", json!({}));
    h
}

fn pointer(h: &mut Headless, events: serde_json::Value) {
    let r = h.request("ui.pointer", json!({"events": events}), T);
    assert_eq!(r["ok"], true, "{r}");
}

fn develop(h: &Headless) -> lcc_develop::DevelopSettings {
    let id = h.app.session.active().expect("active photo");
    (*h.app.session.develop_of(id).unwrap_or_default()).clone()
}

fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
}

fn brush_at(h: &mut Headless, x: f64, y: f64) {
    pointer(h, json!([{"kind": "down", "x": x, "y": y}, {"kind": "up", "x": x, "y": y}]));
}

/// Evaluate the real composite, with a deterministic embedded sky matte (no AI model required).
fn mask_coverage(h: &Headless, mask: usize, x: usize, y: usize) -> f32 {
    use lcc_pipeline::{
        geometry::Frame,
        masks::{MatteKind, Mattes, evaluate_one},
    };
    use lcc_raster::{Image, Plane, Rgb32f};
    let d = develop(h);
    let mut sky = Image::<u8>::new(100, 100);
    sky.data.fill(255);
    let mut mattes = Mattes::default();
    mattes.push(MatteKind::Sky, sky);
    let a = evaluate_one(
        &d.masks[mask],
        &Frame::new(100, 100, &Default::default(), true),
        100,
        100,
        &Rgb32f::new(100, 100),
        &Plane::new(100, 100),
        0.0,
        Some(&mattes),
    );
    a.get(x, y)
}

#[test]
fn subtract_brush_removes_sky_coverage_and_add_restores_it() {
    let mut h = detail("panel.masking");
    exec(&mut h, "mask.add", json!({"kind": "sky"}));
    let r = h.request("ui.set", json!({"brushSize": 0.15, "brushFeather": 0.0, "brushFlow": 100.0}), T);
    assert_eq!(r["ok"], true, "{r}");
    click(&mut h, "button:maskMinus:1");
    click(&mut h, "maskComp:subtract:brush");
    brush_at(&mut h, 0.5, 0.5);
    assert!(mask_coverage(&h, 0, 50, 50) < 0.01, "Subtract > Brush must remove sky coverage");
    assert!(mask_coverage(&h, 0, 10, 10) > 0.99, "unpainted sky stays selected");
    let subtracted = develop(&h);
    exec(&mut h, "edit.undo", json!({}));
    assert!(mask_coverage(&h, 0, 50, 50) > 0.99);
    exec(&mut h, "edit.redo", json!({}));
    assert_eq!(develop(&h).masks, subtracted.masks);

    // Renaming must not detach subsequent strokes from the subtract component.
    exec(&mut h, "mask.component", json!({"component": 1, "action": "rename", "name": "Sky cleanup"}));
    brush_at(&mut h, 0.25, 0.5);
    assert!(mask_coverage(&h, 0, 25, 50) < 0.01);
    assert_eq!(develop(&h).masks[0].components.len(), 2);

    // Erase removes paint from the subtract component, restoring the underlying sky.
    h.app.ui.brush_erase = true;
    // (the eraser keeps its own settings: the same as the brush's here)
    h.app.ui.erase_brush = h.app.ui.brush_set(false);
    brush_at(&mut h, 0.5, 0.5);
    assert!(mask_coverage(&h, 0, 50, 50) > 0.99);
    click(&mut h, "button:maskPlus:1");
    click(&mut h, "maskComp:add:brush");
    brush_at(&mut h, 0.25, 0.5);
    assert!(mask_coverage(&h, 0, 25, 50) > 0.99, "Add must be composed after Subtract");
    assert_eq!(develop(&h).masks[0].components.len(), 3);
    h.settle(SETTLE);
}

#[test]
fn new_brush_starts_a_separate_mask_in_paint_mode() {
    let mut h = detail("panel.masking");
    exec(&mut h, "mask.add", json!({"kind": "sky"}));
    let original = develop(&h).masks[0].clone();
    h.app.ui.brush_erase = true;
    h.app.ui.mask_overlay = false;
    click(&mut h, "maskNew:brush");
    assert_eq!(h.app.session.active_mask, None, "armed: the first stroke makes the mask");
    assert!(!h.app.ui.brush_erase);
    assert!(h.app.ui.mask_overlay);
    let r = h.request("ui.set", json!({"brushSize": 0.15, "brushFeather": 0.0, "brushFlow": 100.0}), T);
    assert_eq!(r["ok"], true, "{r}");
    brush_at(&mut h, 0.5, 0.5);
    assert_eq!(h.app.session.active_mask, Some(2));
    assert!(mask_coverage(&h, 1, 50, 50) > 0.99);
    h.app.ui.brush_erase = true;
    // (the eraser keeps its own settings: the same as the brush's here)
    h.app.ui.erase_brush = h.app.ui.brush_set(false);
    brush_at(&mut h, 0.5, 0.5);
    assert!(mask_coverage(&h, 1, 50, 50) < 0.01);
    exec(&mut h, "edit.undo", json!({}));
    assert!(mask_coverage(&h, 1, 50, 50) > 0.99);
    assert_eq!(develop(&h).masks[0], original);
    h.settle(SETTLE);
}

/// One rule for the mask overlay (upstream PR 2920, adapted): a mask shows its overlay by itself only
/// while it has no settings; one with settings is judged by its effect when picked, added to or
/// duplicated — after a short peek (issue #1109) so the user still sees which mask is on. O (or the
/// eye) shows it to stay, hovering its row shows it, and while a slider is at work nothing covers
/// the photo. Replaces issue #1800's re-show on every pick.
#[test]
fn a_mask_shows_its_overlay_by_itself_only_without_settings() {
    use lcc_pipeline::Overlay;
    let mut h = detail("panel.masking");
    let shown = |h: &Headless| matches!(crate::panels::detail::view_overlay(&h.app, &develop(h), false), Overlay::Mask { .. });
    let rest = |h: &mut Headless| h.app.ui.mask_adjusting_until = None;
    exec(&mut h, "mask.add", json!({"kind": "linear", "start": [0.5, 0.2], "end": [0.5, 0.7]}));
    let first = h.app.session.active_mask.unwrap();
    assert!(shown(&h), "a new mask without settings shows what it selects");
    exec(&mut h, "mask.adjust", json!({"values": {"exposure": 0.5}}));
    assert!(!shown(&h), "nothing covers the photo while a slider works");
    rest(&mut h);
    assert!(!shown(&h), "a mask with settings shows its effect");
    exec(&mut h, "mask.add", json!({"kind": "radial", "center": [0.4, 0.5], "rx": 0.2, "ry": 0.2}));
    assert!(shown(&h), "another new mask shows its overlay");
    let r = h.request("ui.clickWidget", json!({"id": format!("mask:{first}")}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(h.app.session.active_mask, Some(first));
    // (picking a mask with settings peeks its overlay for a moment first, issue #1109)
    rest(&mut h);
    assert!(shown(&h), "picking a mask with settings peeks its overlay");
    h.app.ui.mask_peek_until = Some(std::time::Instant::now());
    assert!(!shown(&h), "the peek over, a mask with settings shows its effect");
    pointer(&mut h, json!([{"kind": "move", "x": 0.1, "y": 0.9}])); // the pointer leaves the list
    assert_eq!(h.app.ui.mask_hover, None);
    for (cmd, params) in
        [("mask.addComponent", json!({"kind": "radial", "op": "add", "center": [0.6, 0.5], "rx": 0.1, "ry": 0.1})), ("mask.duplicate", json!({}))]
    {
        exec(&mut h, cmd, params);
        assert!(shown(&h), "{cmd} on a mask with settings peeks its overlay");
        h.app.ui.mask_peek_until = Some(std::time::Instant::now());
        assert!(!shown(&h), "{cmd} on a mask with settings shows its effect after the peek");
    }
    // the pointer on one of its component rows (or its pin) shows it
    h.app.ui.mask_hover = Some(first);
    assert!(shown(&h), "hovering a component row shows the overlay");
    h.app.ui.mask_hover = None;
    // O shows it to stay, through edits; a slider at work still hides it
    exec(&mut h, "view.maskOverlay", json!({"show": true}));
    assert!(shown(&h));
    exec(&mut h, "mask.adjust", json!({"values": {"exposure": 0.7}}));
    assert!(!shown(&h), "hidden while the slider works");
    rest(&mut h);
    assert!(shown(&h), "back when it rests: O keeps it on");
    exec(&mut h, "mask.select", json!({"id": first}));
    assert!(shown(&h), "O keeps it on for a picked mask too");
    h.settle(SETTLE);
}

/// #1042: a released brush stroke leaves the mask's overlay on show (even on a mask with settings),
/// so the painted area can be judged; the next mask slider hides it so the effect is seen.
#[test]
fn a_brush_stroke_leaves_its_overlay_until_a_slider_moves() {
    use lcc_pipeline::Overlay;
    let mut h = detail("panel.masking");
    let shown = |h: &Headless| matches!(crate::panels::detail::view_overlay(&h.app, &develop(h), false), Overlay::Mask { .. });
    exec(&mut h, "mask.add", json!({"kind": "brush"}));
    exec(&mut h, "mask.brushStroke", json!({"points": [[0.3, 0.3], [0.4, 0.3]], "size": 0.02}));
    exec(&mut h, "mask.adjust", json!({"values": {"exposure": 0.5}}));
    h.app.ui.mask_adjusting_until = None;
    assert!(!shown(&h), "a mask with settings shows its effect");
    exec(&mut h, "mask.brushStroke", json!({"points": [[0.5, 0.5], [0.6, 0.5]], "size": 0.02}));
    assert!(shown(&h), "the stroke's area stays on show after it is released");
    exec(&mut h, "mask.adjust", json!({"values": {"exposure": 0.6}}));
    h.app.ui.mask_adjusting_until = None;
    assert!(!shown(&h), "a slider hides it again");
    h.settle(SETTLE);
}

/// Issue #1109: picking a mask that has settings peeks its overlay for a moment (the Show Overlay
/// checkbox stays off), so the user immediately sees which mask the program has selected; the peek
/// ends by itself, and with the overlay off automatic (or pinned) there is no peek.
#[test]
fn picking_a_mask_with_settings_peeks_its_overlay_briefly() {
    use lcc_pipeline::{MaskView, Overlay};
    let mut h = detail("panel.masking");
    let overlay = |h: &Headless| crate::panels::detail::view_overlay(&h.app, &develop(h), false);
    exec(&mut h, "mask.add", json!({"kind": "linear", "start": [0.5, 0.2], "end": [0.5, 0.7]}));
    exec(&mut h, "mask.adjust", json!({"values": {"exposure": 0.5}}));
    h.app.ui.mask_adjusting_until = None;
    exec(&mut h, "mask.add", json!({"kind": "radial", "center": [0.4, 0.5], "rx": 0.2, "ry": 0.2}));
    exec(&mut h, "mask.select", json!({"id": 1}));
    assert!(!h.app.ui.mask_overlay, "not latched on");
    assert!(h.app.ui.mask_peek_until.is_some(), "the peek is armed");
    assert_eq!(overlay(&h), Overlay::Mask { id: 1, view: MaskView::Color, color: [230, 30, 40], opacity: 50 }, "the peek shows the mask");
    h.app.ui.mask_peek_until = Some(std::time::Instant::now());
    assert_eq!(overlay(&h), Overlay::None, "the peek ends by itself");
    // a mask without settings still latches the overlay on as before
    exec(&mut h, "mask.add", json!({"kind": "radial", "center": [0.6, 0.5], "rx": 0.2, "ry": 0.2}));
    assert!(h.app.ui.mask_overlay);
    assert!(h.app.ui.mask_peek_until.is_none());
    // with the overlay off automatic there is no peek
    exec(&mut h, "view.maskOverlay", json!({"auto": false}));
    exec(&mut h, "mask.select", json!({"id": 1}));
    assert!(h.app.ui.mask_peek_until.is_none());
    assert_eq!(overlay(&h), Overlay::None, "auto off: the effect is judged at once");
    h.settle(SETTLE);
}

#[test]
fn radial_body_drag_moves_rotated_components_and_undoes_once() {
    let mut h = detail("panel.masking");
    exec(&mut h, "mask.add", json!({"kind": "luminanceRange"}));
    exec(&mut h, "mask.addComponent", json!({"kind": "radial", "op": "add", "center": [0.45, 0.5], "rx": 0.22, "ry": 0.1, "angle": 40}));
    let before = develop(&h);
    // Well away from the pin and edge/rotation grips, inside a rotated ellipse.
    pointer(
        &mut h,
        json!([{"kind": "down", "x": 0.51, "y": 0.56}, {"kind": "drag", "x": 0.54, "y": 0.58},
        {"kind": "drag", "x": 0.61, "y": 0.66}, {"kind": "up", "x": 0.61, "y": 0.66}]),
    );
    let after = develop(&h);
    let MaskShape::Radial { center, rx, ry, angle, feather, invert } = &after.masks[0].components[1].shape else { panic!("radial") };
    assert!((center.x - 0.55).abs() < 0.005 && (center.y - 0.6).abs() < 0.005, "{center:?}");
    let MaskShape::Radial { rx: old_rx, ry: old_ry, angle: old_angle, feather: old_feather, invert: old_invert, .. } =
        &before.masks[0].components[1].shape
    else {
        panic!("radial")
    };
    assert_eq!((rx, ry, angle, feather, invert), (old_rx, old_ry, old_angle, old_feather, old_invert));
    assert_eq!(after.masks[0].components[0], before.masks[0].components[0], "only the hit component moves");
    exec(&mut h, "edit.undo", json!({}));
    assert_eq!(develop(&h), before, "one undo reverses the entire drag");
    exec(&mut h, "edit.redo", json!({}));
    assert_eq!(develop(&h), after);
    // The bounding-box corner is outside the ellipse, even for an inverted radial mask.
    exec(&mut h, "mask.component", json!({"component": 1, "action": "invert"}));
    let outside_before = develop(&h);
    pointer(
        &mut h,
        json!([{"kind": "down", "x": 0.35, "y": 0.75}, {"kind": "drag", "x": 0.4, "y": 0.8},
        {"kind": "up", "x": 0.4, "y": 0.8}]),
    );
    assert_eq!(develop(&h), outside_before, "outside the ellipse must not move it");
    // Hiding pins does not disable dragging the selected ellipse body.
    exec(&mut h, "view.maskPins", json!({"show": false}));
    pointer(
        &mut h,
        json!([{"kind": "down", "x": 0.61, "y": 0.66}, {"kind": "drag", "x": 0.66, "y": 0.71},
        {"kind": "up", "x": 0.66, "y": 0.71}]),
    );
    let MaskShape::Radial { center, .. } = develop(&h).masks[0].components[1].shape.clone() else { panic!("radial") };
    assert!((center.x - 0.6).abs() < 0.005 && (center.y - 0.65).abs() < 0.005, "{center:?}");
}

#[test]
fn radial_body_drag_keeps_resize_and_rotation_handles() {
    let mut h = detail("panel.masking");
    exec(&mut h, "mask.add", json!({"kind": "radial", "center": [0.5, 0.5], "rx": 0.18, "ry": 0.1}));
    for handle in [1, 5] {
        let widget = format!("maskHandle:1:0:{handle}");
        let rect = h.app.widgets.iter().find(|(id, _)| *id == widget).unwrap().1;
        let image = h.app.image_rect.unwrap();
        let q = rect.center();
        let x = (q.x - image.left()) / image.width();
        let y = (q.y - image.top()) / image.height();
        pointer(
            &mut h,
            json!([{"kind": "down", "x": x, "y": y}, {"kind": "drag", "x": x + 0.03, "y": y + 0.03},
            {"kind": "up", "x": x + 0.03, "y": y + 0.03}]),
        );
        let MaskShape::Radial { center, rx, angle, .. } = develop(&h).masks[0].components[0].shape.clone() else { panic!("radial") };
        assert!((center.x - 0.5).abs() < 0.005 && (center.y - 0.5).abs() < 0.005, "a handle must not translate the mask");
        if handle == 1 {
            assert!(rx > 0.19, "resize grip changes radius");
        } else {
            assert!(angle.abs() > 1.0, "rotation grip changes angle");
        }
    }
}

#[test]
fn mask_overlay_keys_and_pins() {
    use lcc_pipeline::{MaskView, Overlay};
    let mut h = detail("panel.masking");
    assert_eq!(h.app.ui.right, RightPanel::Masking);
    exec(&mut h, "mask.add", json!({"kind": "radial", "center": [0.3, 0.4], "rx": 0.1, "ry": 0.1}));
    exec(&mut h, "mask.add", json!({"kind": "linear", "start": [0.7, 0.2], "end": [0.7, 0.6]}));
    assert_eq!(h.app.session.active_mask, Some(2));
    // the loupe asks the renderer for the selected mask's overlay
    let d = develop(&h);
    let o = crate::panels::detail::view_overlay(&h.app, &d, false);
    assert_eq!(o, Overlay::Mask { id: 2, view: MaskView::Color, color: [230, 30, 40], opacity: 50 });
    // O toggles it, Shift+O cycles the colour (and leaves the crop overlay alone)
    h.request("ui.key", json!({"key": "o"}), T);
    assert!(!h.app.ui.mask_overlay);
    assert_eq!(crate::panels::detail::view_overlay(&h.app, &d, false), Overlay::None);
    h.request("ui.key", json!({"key": "o"}), T);
    let crop = h.app.ui.crop_overlay;
    let colour = h.app.ui.mask_overlay_color;
    h.request("ui.key", json!({"key": "o", "shift": true}), T);
    assert_ne!(h.app.ui.mask_overlay_color, colour, "the next overlay colour");
    assert_eq!(h.app.ui.mask_overlay_mode, "color", "the mode stays");
    assert_eq!(h.app.ui.crop_overlay, crop);
    exec(&mut h, "view.maskOverlayMode", json!({"mode": "whiteOnBlack"}));
    exec(&mut h, "view.maskOverlayColor", json!({"color": "#2870f0", "opacity": 80}));
    assert_eq!((h.app.ui.mask_overlay_color, h.app.ui.mask_overlay_opacity), ([0x28, 0x70, 0xf0], 80.0));
    for bad in ["#+f+f+f", "+12345", "#-0-0-0", "##ffffff"] {
        let r = h.request("engine.execute", json!({"command": "view.maskOverlayColor", "params": {"color": bad}}), T);
        assert_eq!(r["ok"], false, "{bad:?} is not a colour: {r}");
        assert_eq!(h.app.ui.mask_overlay_color, [0x28, 0x70, 0xf0], "{bad:?} leaves the overlay colour alone");
    }
    let r = h.request("engine.execute", json!({"command": "view.maskOverlayMode", "params": {"mode": "nope"}}), T);
    assert_eq!(r["ok"], false, "{r}");
    // clicking another mask's pin selects that mask
    pointer(&mut h, json!([{"kind": "down", "x": 0.3, "y": 0.4}, {"kind": "up", "x": 0.3, "y": 0.4}]));
    assert_eq!(h.app.session.active_mask, Some(1));
    // dragging a pin moves its component (one undo step)
    pointer(
        &mut h,
        json!([{"kind": "down", "x": 0.3, "y": 0.4}, {"kind": "drag", "x": 0.35, "y": 0.45}, {"kind": "drag", "x": 0.5, "y": 0.6}, {"kind": "up", "x": 0.5, "y": 0.6}]),
    );
    let MaskShape::Radial { center, .. } = develop(&h).masks[0].components[0].shape.clone() else { panic!("radial") };
    assert!((center.x - 0.5).abs() < 0.02 && (center.y - 0.6).abs() < 0.02, "{center:?}");
    // dragging the linear gradient's pin (a non-selected mask) selects and moves it
    pointer(
        &mut h,
        json!([{"kind": "down", "x": 0.7, "y": 0.4}, {"kind": "drag", "x": 0.72, "y": 0.4}, {"kind": "drag", "x": 0.8, "y": 0.4}, {"kind": "up", "x": 0.8, "y": 0.4}]),
    );
    assert_eq!(h.app.session.active_mask, Some(2));
    let MaskShape::Linear { start, end } = develop(&h).masks[1].components[0].shape.clone() else { panic!("linear") };
    assert!((start.x - 0.8).abs() < 0.02 && (end.x - 0.8).abs() < 0.02 && (start.y - 0.2).abs() < 0.02, "{start:?} {end:?}");
    // hidden pins can't be grabbed
    exec(&mut h, "view.maskPins", json!({"show": false}));
    pointer(&mut h, json!([{"kind": "down", "x": 0.5, "y": 0.6}, {"kind": "up", "x": 0.5, "y": 0.6}]));
    assert_eq!(h.app.session.active_mask, Some(2));
    h.settle(SETTLE);
}

/// Issue #3049: Show Pins draws a pin for an Object mask (at its first click) and a Describe
/// mask (on what its segmentation found). Clicking one selects its mask; dragging it leaves the
/// mask and the History alone, since neither can be moved.
#[test]
fn object_and_describe_masks_show_pins_that_select_but_do_not_move() {
    use lcc_develop::SegMask;
    use lcc_geom::Point;
    let mut h = detail("panel.masking");
    // a 16 × 16 segmentation found in columns 2..=5, rows 10..=13: centred on (0.25, 0.75)
    let blob: Vec<f32> = (0..16 * 16).map(|i| if (2..=5).contains(&(i % 16)) && (10..=13).contains(&(i / 16)) { 8.0 } else { -8.0 }).collect();
    let seg = SegMask::from_logits(16, &blob);
    let object = MaskShape::Object { hint: vec![Point::new(0.6, 0.5)], exclude: vec![], seg: Some(seg.clone()), detail: vec![], edge: 0.0 };
    let prompt = MaskShape::Prompt { text: "the boat".into(), seg: Some(seg), detail: vec![], edge: 0.0 };
    exec(&mut h, "mask.add", json!({"kind": "radial", "center": [0.5, 0.5], "rx": 0.1, "ry": 0.1}));
    exec(&mut h, "mask.update", json!({"id": 1, "shape": object}));
    exec(&mut h, "mask.add", json!({"kind": "radial", "center": [0.5, 0.5], "rx": 0.1, "ry": 0.1}));
    exec(&mut h, "mask.update", json!({"id": 2, "shape": prompt}));
    assert_eq!(h.app.session.active_mask, Some(2));
    // pins shown without the pointer over the photo
    exec(&mut h, "view.toolOverlay", json!({"mode": "always"}));
    h.settle(SETTLE);
    let pin_at = |h: &Headless, id: &str| h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| r.center());
    let (Some(object_pin), Some(prompt_pin)) = (pin_at(&h, "maskPin:1:0"), pin_at(&h, "maskPin:2:0")) else {
        panic!("both masks have a pin: {:?}", h.app.widgets.iter().filter(|(w, _)| w.starts_with("maskPin:")).collect::<Vec<_>>())
    };
    // the Describe pin sits on what was found, right of and below the Object's
    assert!(prompt_pin.x < object_pin.x && prompt_pin.y > object_pin.y, "{object_pin:?} {prompt_pin:?}");
    // clicking a pin selects its mask
    pointer(&mut h, json!([{"kind": "down", "x": 0.6, "y": 0.5}, {"kind": "up", "x": 0.6, "y": 0.5}]));
    assert_eq!(h.app.session.active_mask, Some(1));
    pointer(&mut h, json!([{"kind": "down", "x": 0.25, "y": 0.75}, {"kind": "up", "x": 0.25, "y": 0.75}]));
    assert_eq!(h.app.session.active_mask, Some(2));
    // dragging one changes nothing and adds no History step
    let id = h.app.session.active().expect("active photo");
    let before = h.app.session.catalog.photo(id).expect("photo").clone();
    pointer(
        &mut h,
        json!([{"kind": "down", "x": 0.6, "y": 0.5}, {"kind": "drag", "x": 0.65, "y": 0.55}, {"kind": "drag", "x": 0.8, "y": 0.7}, {"kind": "up", "x": 0.8, "y": 0.7}]),
    );
    assert_eq!(h.app.session.active_mask, Some(1), "the press still selects the mask");
    let after = h.app.session.catalog.photo(id).expect("photo").clone();
    assert_eq!((after.develop.clone(), after.history.len()), (before.develop.clone(), before.history.len()));
    h.settle(SETTLE);
}

/// Issue #3049: with the Object tool on, a click on another mask's pin selects that mask instead
/// of adding an Object click to the selected one. The tool stays on for an Object mask (its
/// clicks refine it) and goes off for a mask with no Object selection to refine.
#[test]
fn the_object_tool_selects_the_mask_whose_pin_is_clicked() {
    use lcc_geom::Point;
    let dir = lcc_testkit::TempDir::new("lc-ui-pin-object");
    let mut h = detail("panel.masking");
    // no SAM 3 model here: a stray Object click would offer the download or show an error
    h.app.session.segmenter.dir = Some(dir.to_path_buf());
    h.app.session.segmenter.mirrors_file = Some(dir.join("none.txt"));
    let object = |at: Point| MaskShape::Object { hint: vec![at], exclude: vec![], seg: None, detail: vec![], edge: 0.0 };
    let radial = json!({"kind": "radial", "center": [0.5, 0.5], "rx": 0.1, "ry": 0.1});
    exec(&mut h, "mask.add", radial.clone());
    exec(&mut h, "mask.update", json!({"id": 1, "shape": object(Point::new(0.6, 0.5))}));
    exec(&mut h, "mask.add", json!({"kind": "radial", "center": [0.2, 0.8], "rx": 0.05, "ry": 0.05}));
    exec(&mut h, "mask.add", radial);
    exec(&mut h, "mask.update", json!({"id": 3, "shape": object(Point::new(0.3, 0.3))}));
    assert_eq!(h.app.session.active_mask, Some(3));
    exec(&mut h, "view.toolOverlay", json!({"mode": "always"}));
    h.app.ui.tool = "object".into();
    h.settle(SETTLE);
    let toast = h.app.ui.toast().map(|t| t.0.clone());
    let before = develop(&h).masks;
    // the Object mask's pin: it is selected, its clicks and the selected mask's stay as they were
    pointer(&mut h, json!([{"kind": "down", "x": 0.6, "y": 0.5}, {"kind": "up", "x": 0.6, "y": 0.5}]));
    h.settle(SETTLE);
    assert_eq!(h.app.session.active_mask, Some(1), "the pinned mask is selected");
    assert_eq!(develop(&h).masks, before, "no Object click was added to any mask");
    assert!(h.app.ui.dialog.is_none(), "no Object click was tried");
    assert_eq!(h.app.ui.toast().map(|t| t.0.clone()).filter(|t| Some(t) != toast.as_ref()), None, "no error");
    assert_eq!(h.app.ui.tool, "object", "the tool stays on to refine the Object mask");
    // the Radial mask's pin: selected, and the Object tool goes off (nothing for it to refine)
    pointer(&mut h, json!([{"kind": "down", "x": 0.2, "y": 0.8}, {"kind": "up", "x": 0.2, "y": 0.8}]));
    h.settle(SETTLE);
    assert_eq!(h.app.session.active_mask, Some(2));
    assert_eq!(develop(&h).masks, before);
    assert_eq!(h.app.ui.tool, "", "the Object tool is off");
    assert!(h.app.ui.dialog.is_none());
}

/// Issue #3049: with the Object tool on, a click on the selected Object mask's own pin adds no
/// near-copy of its first click (which would use up one of its limited Object clicks): its
/// clicks stay as they were, it stays selected and no error is shown.
#[test]
fn the_object_tool_adds_no_click_on_the_selected_masks_own_pin() {
    use lcc_geom::Point;
    let dir = lcc_testkit::TempDir::new("lc-ui-own-pin-object");
    let mut h = detail("panel.masking");
    // no SAM 3 model here: a stray Object click would offer the download or show an error
    h.app.session.segmenter.dir = Some(dir.to_path_buf());
    h.app.session.segmenter.mirrors_file = Some(dir.join("none.txt"));
    exec(&mut h, "mask.add", json!({"kind": "radial", "center": [0.5, 0.5], "rx": 0.1, "ry": 0.1}));
    let object = MaskShape::Object { hint: vec![Point::new(0.6, 0.5)], exclude: vec![], seg: None, detail: vec![], edge: 0.0 };
    exec(&mut h, "mask.update", json!({"id": 1, "shape": object}));
    assert_eq!(h.app.session.active_mask, Some(1));
    exec(&mut h, "view.toolOverlay", json!({"mode": "always"}));
    h.app.ui.tool = "object".into();
    h.settle(SETTLE);
    let toast = h.app.ui.toast().map(|t| t.0.clone());
    let before = develop(&h).masks;
    // a little off the pin's centre, as a hand clicks it
    pointer(&mut h, json!([{"kind": "down", "x": 0.601, "y": 0.501}, {"kind": "up", "x": 0.601, "y": 0.501}]));
    h.settle(SETTLE);
    assert_eq!(develop(&h).masks, before, "no Object click was added");
    assert_eq!(h.app.session.active_mask, Some(1), "the mask stays selected");
    assert!(h.app.ui.dialog.is_none(), "no Object click was tried");
    assert_eq!(h.app.ui.toast().map(|t| t.0.clone()).filter(|t| Some(t) != toast.as_ref()), None, "no error");
    assert_eq!(h.app.ui.tool, "object", "the tool stays on");
}

/// #2210: Ctrl+Shift+O (the all-masks overlay) draws the white outline of every visible
/// Radial/Linear mask, not just the selected one — but only the selected mask registers
/// `maskHandle:` elements, so nothing else becomes draggable; the others' outlines are only painted.
#[test]
fn all_masks_overlay_outlines_every_visible_mask_but_only_the_selected_one_is_draggable() {
    let mut h = detail("panel.masking");
    // Two visible radial masks; mask 2 ends up selected (mask.add selects the new one).
    exec(&mut h, "mask.add", json!({"kind": "radial", "center": [0.3, 0.5], "rx": 0.12, "ry": 0.12}));
    exec(&mut h, "mask.add", json!({"kind": "radial", "center": [0.7, 0.5], "rx": 0.12, "ry": 0.12}));
    exec(&mut h, "view.maskOverlayAll", json!({}));
    assert!(h.app.ui.mask_overlay_all);
    assert_eq!(h.app.session.active_mask, Some(2));
    // Pins add their own <maskPin:…> elements; keep them out of the handle count.
    exec(&mut h, "view.maskPins", json!({"show": false}));
    h.settle(SETTLE);

    let handles_of = |h: &Headless, id: usize| h.app.widgets.iter().filter(|(w, _)| w.starts_with(&format!("maskHandle:{id}:"))).count();
    // The selected mask keeps its 4 edge handles, its rotation grip and its feather handle.
    assert_eq!(handles_of(&h, 2), 6, "the selected mask keeps its handles");
    // The other visible mask draws its outline but registers no handle: it is not draggable.
    assert_eq!(handles_of(&h, 1), 0, "an unselected mask must not become draggable");
    // Its outline is painted, not registered as a widget, so the way to observe it is the rendered
    // pixels: white (alpha 170) over the photo, so the overlay adds near-white pixels along its edge.
    let white_pixels = |h: &mut Headless, on: bool| -> usize {
        if h.app.ui.mask_overlay_all != on {
            exec(h, "view.maskOverlayAll", json!({}));
        }
        assert_eq!(h.app.ui.mask_overlay_all, on);
        h.settle(SETTLE);
        let px = h.view.paint(&Default::default());
        px.pixels.iter().filter(|p| p.r() > 200 && p.g() > 200 && p.b() > 200).count()
    };
    let off = white_pixels(&mut h, false);
    let on = white_pixels(&mut h, true);
    assert!(on > off, "the unselected mask's outline must add white pixels: off {off}, on {on}");
    // …while a mask with no components has nothing to outline and nothing to grab either.
    exec(&mut h, "mask.add", json!({"kind": "luminanceRange"}));
    exec(&mut h, "mask.addComponent", json!({"kind": "radial", "op": "add", "center": [0.5, 0.8], "rx": 0.08, "ry": 0.08}));
    assert_eq!(h.app.session.active_mask, Some(3));
    assert_eq!(handles_of(&h, 1), 0);
    h.settle(SETTLE);
}

/// #518: holding the brush still, however long, paints the same feathered spot as a quick press.
#[test]
fn holding_the_brush_still_does_not_harden_the_spot() {
    let mut h = detail("panel.masking");
    let r = h.request("ui.set", json!({"brushSize": 0.1, "brushFeather": 80.0, "brushFlow": 100.0}), T);
    assert_eq!(r["ok"], true, "{r}");
    exec(&mut h, "tool.brush", json!({"new": true}));
    // a quick press at the left, a long stationary hold (120 frames, ~2 s) at the right
    brush_at(&mut h, 0.3, 0.5);
    let mut hold = vec![json!({"kind": "down", "x": 0.7, "y": 0.5})];
    hold.extend((0..120).map(|_| json!({"kind": "move", "x": 0.7, "y": 0.5})));
    hold.push(json!({"kind": "up", "x": 0.7, "y": 0.5}));
    pointer(&mut h, json!(hold));
    let d = develop(&h);
    let MaskShape::Brush { strokes } = &d.masks[0].components[0].shape else { panic!("brush") };
    assert_eq!(strokes.len(), 2, "{strokes:?}");
    assert_eq!(strokes[1].points.len(), 1, "a stationary hold records one sample: {:?}", strokes[1].points);
    // the same edge profile around both spots
    for dx in [0usize, 4, 6, 8] {
        let (quick, held) = (mask_coverage(&h, 0, 30 + dx, 50), mask_coverage(&h, 0, 70 + dx, 50));
        assert!((quick - held).abs() < 0.01, "{dx} px from the centre: quick {quick}, held {held}");
    }
    assert!(mask_coverage(&h, 0, 70 + 6, 50) < 0.9, "the edge stays feathered");
    h.settle(SETTLE);
}

/// A ⇧-click with the brush paints a straight line from where the last stroke ended (issue #106 of the Classic
/// fork), one undo step; a plain click only paints a dab where it lands.
#[test]
fn shift_click_paints_a_straight_line_from_the_last_stroke() {
    let mut h = detail("panel.masking");
    exec(&mut h, "tool.brush", json!({}));
    brush_at(&mut h, 0.2, 0.5);
    brush_at(&mut h, 0.8, 0.5);
    assert!(mask_coverage(&h, 0, 50, 50) < 0.01, "two plain clicks leave the space between them alone");
    let r = h.request("ui.pointer", json!({"events": [{"kind": "down", "x": 0.5, "y": 0.2}, {"kind": "up", "x": 0.5, "y": 0.2}], "shift": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    let d = develop(&h);
    let MaskShape::Brush { strokes } = &d.masks[0].components[0].shape else { panic!("brush") };
    assert_eq!(strokes.len(), 3, "{strokes:?}");
    assert_eq!(strokes[2].points.len(), 2, "from the last stroke's end to the click: {strokes:?}");
    for (x, y) in [(65, 35), (58, 27)] {
        assert!(mask_coverage(&h, 0, x, y) > 0.5, "the line from (0.8, 0.5) to (0.5, 0.2) passes ({x}, {y})");
    }
    assert!(mask_coverage(&h, 0, 35, 35) < 0.01, "and nowhere else");
    exec(&mut h, "edit.undo", json!({}));
    let d = develop(&h);
    let MaskShape::Brush { strokes } = &d.masks[0].components[0].shape else { panic!("brush") };
    assert_eq!(strokes.len(), 2, "one undo step takes the line back");
    h.settle(SETTLE);
}

#[test]
fn brush_strokes_carry_auto_mask() {
    let mut h = detail("panel.masking");
    let r = h.request("ui.set", json!({"brushAutoMask": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    exec(&mut h, "tool.brush", json!({}));
    assert_eq!(h.app.ui.tool, "brush");
    pointer(
        &mut h,
        json!([{"kind": "down", "x": 0.3, "y": 0.5}, {"kind": "drag", "x": 0.4, "y": 0.5}, {"kind": "drag", "x": 0.5, "y": 0.5}, {"kind": "up", "x": 0.5, "y": 0.5}]),
    );
    let d = develop(&h);
    let MaskShape::Brush { strokes } = &d.masks[0].components[0].shape else { panic!("brush") };
    assert!(strokes[0].auto_mask && strokes[0].points.len() >= 2, "{strokes:?}");
    assert!(!strokes[0].erase);
    // holding ⌥ paints an erase stroke without switching the brush to Erase
    let r = h.request(
        "ui.pointer",
        json!({"events": [{"kind": "down", "x": 0.35, "y": 0.5}, {"kind": "drag", "x": 0.45, "y": 0.5}, {"kind": "up", "x": 0.45, "y": 0.5}], "alt": true}),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    let d = develop(&h);
    let MaskShape::Brush { strokes } = &d.masks[0].components[0].shape else { panic!("brush") };
    assert_eq!(strokes.len(), 2, "{strokes:?}");
    assert!(strokes[1].erase, "⌥ erases");
    assert!(!h.app.ui.brush_erase, "the brush mode is unchanged");
    h.settle(SETTLE);
}

/// The Auto Mask range slider sits under Auto Mask (shown while it is on), and
/// each stroke keeps the range it was painted with.
#[test]
fn each_stroke_keeps_the_auto_mask_range_it_was_painted_with() {
    let mut h = detail("panel.masking");
    exec(&mut h, "tool.brush", json!({"new": true}));
    h.step();
    h.step();
    let has = |h: &Headless| h.app.widgets.iter().any(|(w, _)| w == "slider:ui.brushAutoRange");
    assert!(!has(&h), "no range without Auto Mask");
    let r = h.request("ui.set", json!({"brushAutoMask": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert!(has(&h), "the range shows under Auto Mask");
    assert_eq!(h.app.ui.brush_auto_range, 0.0, "it starts at Lightroom's limits");
    let r = h.request("ui.dragWidget", json!({"id": "slider:ui.brushAutoRange", "dx": 40.0, "dy": 0.0, "steps": 6}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    let wide = h.app.ui.brush_auto_range;
    assert!(wide > 0.0 && wide <= 100.0, "dragged wider: {wide}");
    let stroke = |h: &mut Headless, y: f64| {
        pointer(h, json!([{"kind": "down", "x": 0.3, "y": y}, {"kind": "drag", "x": 0.4, "y": y}, {"kind": "up", "x": 0.4, "y": y}]));
        h.step();
    };
    stroke(&mut h, 0.4);
    // tighter for the next stroke
    let r = h.request("ui.set", json!({"brushAutoRange": -50.0}), T);
    assert_eq!(r["ok"], true, "{r}");
    stroke(&mut h, 0.6);
    let d = develop(&h);
    let MaskShape::Brush { strokes } = &d.masks[0].components[0].shape else { panic!("brush") };
    let got: Vec<f64> = strokes.iter().map(|s| s.auto_range).collect();
    assert_eq!(got.len(), 2, "{strokes:?}");
    assert!((got[0] - f64::from(wide)).abs() < 1e-3 && got[1] == -50.0, "each as painted: {got:?}");
    h.settle(SETTLE);
}

#[test]
fn remove_spots_by_pointer_and_keyboard() {
    let mut h = detail("panel.remove");
    assert_eq!(h.app.ui.right, RightPanel::Remove);
    let spots = |h: &Headless| develop(h).spots;
    // paint a spot: it's added with the brush's size/feather/opacity and selected
    let r = h.request("ui.set", json!({"removeFeather": 30.0, "removeOpacity": 80.0}), T);
    assert_eq!(r["ok"], true, "{r}");
    pointer(&mut h, json!([{"kind": "down", "x": 0.3, "y": 0.6}, {"kind": "up", "x": 0.3, "y": 0.6}]));
    assert_eq!(spots(&h).len(), 1);
    assert_eq!((spots(&h)[0].feather, spots(&h)[0].opacity), (30.0, 80.0));
    assert_eq!(h.app.session.active_spot, Some(0));
    // [ / ] resize the selected spot, Shift+[ / Shift+] feather it
    let s0 = spots(&h)[0].size;
    h.request("ui.key", json!({"key": "]"}), T);
    assert!(spots(&h)[0].size > s0 * 1.1, "{} vs {s0}", spots(&h)[0].size);
    h.request("ui.key", json!({"key": "["}), T);
    h.request("ui.key", json!({"key": "["}), T);
    assert!(spots(&h)[0].size < s0 * 0.9);
    h.request("ui.key", json!({"key": "[", "shift": true}), T);
    assert_eq!(spots(&h)[0].feather, 20.0);
    h.request("ui.key", json!({"key": "]", "shift": true}), T);
    assert_eq!(spots(&h)[0].feather, 30.0);
    // / picks another source (and leaves the filmstrip alone)
    let (src, film) = (spots(&h)[0].source_offset, h.app.ui.filmstrip);
    h.request("ui.key", json!({"key": "/"}), T);
    assert_ne!(spots(&h)[0].source_offset, src);
    assert_eq!(h.app.ui.filmstrip, film);
    // a second spot elsewhere; clicking the first one's pin selects it
    pointer(&mut h, json!([{"kind": "down", "x": 0.7, "y": 0.3}, {"kind": "up", "x": 0.7, "y": 0.3}]));
    assert_eq!((spots(&h).len(), h.app.session.active_spot), (2, Some(1)));
    pointer(&mut h, json!([{"kind": "down", "x": 0.3, "y": 0.6}, {"kind": "up", "x": 0.3, "y": 0.6}]));
    assert_eq!((spots(&h).len(), h.app.session.active_spot), (2, Some(0)));
    // drag its target: it moves, its source offset stays
    let src = spots(&h)[0].source_offset;
    pointer(
        &mut h,
        json!([{"kind": "down", "x": 0.3, "y": 0.6}, {"kind": "drag", "x": 0.32, "y": 0.6}, {"kind": "drag", "x": 0.4, "y": 0.6}, {"kind": "up", "x": 0.4, "y": 0.6}]),
    );
    let sp = spots(&h)[0].clone();
    assert!((sp.points[0].x - 0.4).abs() < 0.01 && (sp.points[0].y - 0.6).abs() < 0.01, "{:?}", sp.points);
    assert_eq!(sp.source_offset, src);
    // drag its source to a fixed place
    let o = sp.source_offset.unwrap();
    let (sx, sy) = (sp.points[0].x + o.x, sp.points[0].y + o.y);
    pointer(
        &mut h,
        json!([{"kind": "down", "x": sx, "y": sy}, {"kind": "drag", "x": sx + 0.01, "y": sy}, {"kind": "drag", "x": 0.5, "y": 0.8}, {"kind": "up", "x": 0.5, "y": 0.8}]),
    );
    let sp = spots(&h)[0].clone();
    let o = sp.source_offset.unwrap();
    assert!((sp.points[0].x + o.x - 0.5).abs() < 0.01 && (sp.points[0].y + o.y - 0.8).abs() < 0.01, "{o:?}");
    // ⌫ deletes the selected spot, not the photo
    let photos = h.app.session.visible_cloned().len();
    h.request("ui.key", json!({"key": "delete"}), T);
    assert_eq!((spots(&h).len(), h.app.session.active_spot), (1, None));
    assert_eq!(h.app.session.visible_cloned().len(), photos);
    h.request("ui.key", json!({"key": "delete"}), T);
    assert_eq!(spots(&h).len(), 1, "nothing selected: nothing deleted");
    h.settle(SETTLE);
}

/// Issue #82: "show all overlays" and Show Pins sit beside the overlay colours and work in every overlay mode;
/// Refine Edges and Feather sit under the overlay options and appear only with a mask selected.
#[test]
fn overlay_row_toggles_in_every_mode_and_edge_sliders_follow_the_selection() {
    let mut h = detail("panel.masking");
    exec(&mut h, "mask.add", json!({"kind": "radial", "center": [0.5, 0.5], "rx": 0.2, "ry": 0.2}));
    h.settle(Duration::from_secs(5));
    let has = |h: &mut Headless, id: &str| h.request("ui.widgets", json!({"filter": id}), T).to_string().contains(&format!("\"{id}\""));
    for mode in ["color", "whiteOnBlack"] {
        exec(&mut h, "view.maskOverlayMode", json!({"mode": mode}));
        h.settle(Duration::from_secs(5));
        let all = h.app.ui.mask_overlay_all;
        click(&mut h, "icon:maskOverlayAll");
        assert_eq!(h.app.ui.mask_overlay_all, !all, "show-all toggles in {mode}");
    }
    assert!(has(&mut h, "slider:refine") && has(&mut h, "slider:maskFeather"), "edge sliders with a selected mask");
    h.app.session.active_mask = None;
    h.settle(Duration::from_secs(5));
    assert!(!has(&mut h, "slider:refine") && !has(&mut h, "slider:maskFeather"), "no edge sliders without a selection");
}

/// Issue #694: Sub-actions (components) are directly under their parent mask, clickable to adjust,
/// collapsible via disclosure chevron, have individual delete bin icons, and the parent mask delete
/// bin icon appears only when hovering over the parent mask.
#[test]
fn issue_694_sub_actions_click_collapse_delete() {
    let mut h = detail("panel.masking");
    exec(&mut h, "mask.add", json!({"kind": "radial", "center": [0.5, 0.5], "rx": 0.2, "ry": 0.2}));
    h.settle(SETTLE);
    let has = |h: &mut Headless, id: &str| h.request("ui.widgets", json!({"filter": id}), T).to_string().contains(&format!("\"{id}\""));

    // Add a linear component to the mask
    exec(&mut h, "mask.addComponent", json!({"kind": "linear", "op": "add", "start": [0.3, 0.3], "end": [0.7, 0.7]}));
    h.settle(SETTLE);

    // Add a brush component to the mask
    exec(&mut h, "tool.brush", json!({"op": "subtract"}));
    h.settle(SETTLE);

    assert_eq!(develop(&h).masks[0].components.len(), 3);

    // Parent mask delete bin shows only on hover
    assert!(!has(&mut h, "maskDeleteRow:1"), "parent bin hidden without hover");
    h.request("ui.hoverWidget", json!({"id": "mask:1"}), T);
    h.settle(SETTLE);
    assert!(has(&mut h, "maskDeleteRow:1"), "parent bin visible when hovered");
    assert!(has(&mut h, "button:maskDelete"), "parent bin button registered");

    // Inline + and - buttons are present on the parent mask row
    assert!(has(&mut h, "button:maskPlus:1"));
    assert!(has(&mut h, "button:maskMinus:1"));

    // Sub-components are present directly under parent mask
    assert!(has(&mut h, "component:0"));
    assert!(has(&mut h, "component:1"));
    assert!(has(&mut h, "component:2"));
    assert!(has(&mut h, "button:componentDelete:1"));

    // Clicking a sub-component activates it and sets the tool
    click(&mut h, "component:1");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.active_mask_component, Some(1));
    assert_eq!(h.app.ui.tool, "linear");

    click(&mut h, "component:2");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.active_mask_component, Some(2));
    assert_eq!(h.app.ui.tool, "brush");

    // Deleting a sub-component via its bin icon
    click(&mut h, "button:componentDelete:1");
    h.settle(SETTLE);
    assert_eq!(develop(&h).masks[0].components.len(), 2);

    // Collapsing the parent mask hides components
    assert!(!h.app.ui.is_mask_collapsed(1));
    click(&mut h, "button:maskCollapse:1");
    h.settle(SETTLE);
    assert!(h.app.ui.is_mask_collapsed(1));
    assert!(!has(&mut h, "component:0"), "components hidden when collapsed");

    // Expanding it shows them again
    click(&mut h, "button:maskCollapse:1");
    h.settle(SETTLE);
    assert!(!h.app.ui.is_mask_collapsed(1));
    assert!(has(&mut h, "component:0"), "components visible when expanded");
}

#[test]
fn command_drag_straightens_in_crop() {
    let mut h = detail("panel.crop");
    let tool = h.app.ui.tool.clone();
    let before = develop(&h).crop.geometry.angle;
    let r = h.request(
        "ui.pointer",
        json!({"events": [{"kind": "down", "x": 0.3, "y": 0.5}, {"kind": "drag", "x": 0.45, "y": 0.51}, {"kind": "drag", "x": 0.6, "y": 0.53}, {"kind": "up", "x": 0.6, "y": 0.53}], "cmd": true}),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    let angle = develop(&h).crop.geometry.angle;
    assert!(angle != before && (1.0..15.0).contains(&angle.abs()), "a slightly tilted line straightens: {angle}");
    assert_eq!(h.app.ui.tool, tool, "the crop tool stays active");
    assert!(h.app.gesture.is_none());
    h.settle(SETTLE);
}

#[test]
fn crop_drag_activates_the_remembered_ratio_and_undo_restores_the_photo() {
    let mut h = detail("panel.crop");
    h.app.ui.crop_default_aspect = Some(json!([1, 1]));
    h.settle(SETTLE);
    let before = develop(&h);
    let undo = h.app.session.undo.len();
    assert!(before.crop.aspect.is_none());
    let r = h.request(
        "ui.pointer",
        json!({"events": [
            {"kind": "down", "x": 0.995, "y": 0.995},
            {"kind": "drag", "x": 0.85, "y": 0.85},
            {"kind": "drag", "x": 0.75, "y": 0.75},
            {"kind": "up", "x": 0.75, "y": 0.75}
        ]}),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(develop(&h).crop.aspect, Some((100, 100)));
    assert_eq!(h.app.session.undo.len(), undo + 1);
    exec(&mut h, "edit.undo", json!({}));
    assert_eq!(develop(&h), before);
}

/// Issue #991: with no aspect ever chosen, a crop starts locked to the photo's own shape (Lightroom's
/// default), so a handle drag keeps the proportions; after the Lock is switched off once, the next
/// photo's crop starts free again.
#[test]
fn a_fresh_crop_starts_locked_to_original_until_the_lock_is_switched_off() {
    let mut h = detail("panel.crop");
    h.app.ui.crop_default_aspect = None;
    h.settle(SETTLE);
    let before = develop(&h);
    assert!(before.crop.aspect.is_none());
    let p = h.app.session.catalog.photo(h.app.session.active().unwrap()).unwrap().clone();
    let own = (p.width * 100, p.height * 100);
    assert_eq!(h.app.crop_tool_aspect(), Some(own), "the Lock shows engaged on an untouched photo");
    let drag = json!({"events": [
        {"kind": "down", "x": 0.995, "y": 0.995},
        {"kind": "drag", "x": 0.85, "y": 0.85},
        {"kind": "drag", "x": 0.75, "y": 0.75},
        {"kind": "up", "x": 0.75, "y": 0.75}
    ]});
    let r = h.request("ui.pointer", drag.clone(), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(develop(&h).crop.aspect, Some(own), "the drag kept the photo's proportions");
    assert_eq!(h.app.ui.crop_default_aspect, None, "nothing was chosen explicitly");
    exec(&mut h, "edit.undo", json!({}));
    assert_eq!(develop(&h), before);
    // Lock off once: remembered as free, and from then on the same drag crops without a ratio
    let mut h = detail("panel.crop");
    h.app.ui.crop_default_aspect = None;
    h.settle(SETTLE);
    h.app.run("crop.aspect", json!({"aspect": "toggle"})).unwrap();
    h.settle(SETTLE);
    assert_eq!(h.app.ui.crop_default_aspect, Some(json!("free")));
    assert_eq!(h.app.crop_tool_aspect(), None);
    let before = develop(&h);
    // a rotating drag outside the box (any crop drag starts cropping); the release on its own
    for events in [
        json!([{"kind": "down", "x": -0.05, "y": 0.2}, {"kind": "drag", "x": -0.05, "y": 0.25}, {"kind": "drag", "x": -0.05, "y": 0.3}]),
        json!([{"kind": "up", "x": -0.05, "y": 0.3}]),
    ] {
        let r = h.request("ui.pointer", json!({ "events": events }), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
    }
    assert_ne!(develop(&h).crop, before.crop, "the drag cropped");
    assert_eq!(develop(&h).crop.aspect, None, "and left the ratio free");
    let _ = drag;
}

/// A remembered ratio the crop command refuses (a hand-edited settings file) is forgotten, and the
/// drag still crops: it must not block every crop of an untouched photo.
#[test]
fn a_bad_remembered_ratio_is_dropped_and_the_drag_still_crops() {
    let mut h = detail("panel.crop");
    h.app.ui.crop_default_aspect = Some(json!([0, -3]));
    h.settle(SETTLE);
    let before = develop(&h);
    // a rotating drag outside the box (any crop drag starts cropping); the release on its own
    for events in [
        json!([{"kind": "down", "x": -0.05, "y": 0.2}, {"kind": "drag", "x": -0.05, "y": 0.25}, {"kind": "drag", "x": -0.05, "y": 0.3}]),
        json!([{"kind": "up", "x": -0.05, "y": 0.3}]),
    ] {
        let r = h.request("ui.pointer", json!({ "events": events }), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
    }
    assert_eq!(h.app.ui.crop_default_aspect, None, "the bad value is forgotten");
    assert_ne!(develop(&h).crop, before.crop, "the drag cropped");
    // forgotten, the tool shows the default lock (the photo's own shape), and the crop keeps the
    // lock it shows (nothing but the lock button unlocks it)
    let shown = h.app.crop_tool_aspect().expect("shown locked");
    let lock = develop(&h).crop.aspect.expect("the crop carries it");
    assert_eq!(lock.0 as u64 * shown.1 as u64, lock.1 as u64 * shown.0 as u64, "{lock:?} is the ratio shown, {shown:?}");
}

#[test]
fn double_click_in_crop_box_applies_the_crop() {
    let mut h = detail("panel.crop");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.right, crate::state::RightPanel::Crop);
    let c = h.app.image_rect.expect("image on screen").center();
    let r = h.request("ui.click", json!({"x": c.x, "y": c.y, "count": 2}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.right, crate::state::RightPanel::Edit, "double-click leaves the crop tool");
}

#[test]
fn option_digit_toggles_keyword_from_set() {
    let mut h = detail("panel.keywords");
    exec(&mut h, "photo.setMeta", json!({"addKeywords": ["alpha"]}));
    exec(&mut h, "photo.setMeta", json!({"removeKeywords": ["alpha"]}));
    let has = |h: &Headless| develop_photo_keywords(h).iter().any(|k| k == "alpha");
    assert!(!has(&h));
    let r = h.request("ui.key", json!({"key": "1", "alt": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(has(&h), "⌥1 adds the first recent keyword");
    let r = h.request("ui.clickWidget", json!({"id": "kwSet:1"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(!has(&h), "its button removes it again");
    h.settle(SETTLE);
}

fn develop_photo_keywords(h: &Headless) -> Vec<String> {
    let id = h.app.session.active().expect("active photo");
    h.app.session.catalog.photo(id).unwrap().meta.keywords.clone()
}

#[test]
fn smart_album_rule_editor_creates_and_edits() {
    let mut h = detail("panel.edit");
    exec(&mut h, "dialog.smartAlbum", json!({"name": "Keepers"}));
    // the editor starts with Rating ≥ 3; "+" adds a second rule
    let r = h.request("ui.clickWidget", json!({"id": "button:ruleAdd-rules-0"}), T);
    assert_eq!(r["ok"], true, "{r}");
    let Some(crate::state::Dialog::SmartRules { rules, .. }) = &mut h.app.ui.dialog else { panic!("no rule editor") };
    assert_eq!(rules.rules.len(), 2);
    rules.rules[1] = serde_json::from_value(json!({"field": "flag", "op": "isNot", "value": "reject"})).unwrap();
    let r = h.request("ui.dialog.confirm", json!({}), T);
    assert_eq!(r["ok"], true, "{r}");
    let a = h.app.session.catalog.albums().find(|a| a.name == "Keepers").expect("album").clone();
    let n = h.app.session.catalog.photos().filter(|p| !p.deleted && p.rating >= 3 && p.flag != lcc_catalog::Flag::Reject).count();
    assert_eq!(h.app.session.catalog.album_count(a.id), n);
    // edit: back to one rule
    exec(&mut h, "dialog.smartAlbum", json!({"id": a.id.0}));
    let r = h.request("ui.clickWidget", json!({"id": "button:ruleRemove-rules-1"}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("ui.dialog.confirm", json!({}), T);
    assert_eq!(r["ok"], true, "{r}");
    let n3 = h.app.session.catalog.photos().filter(|p| !p.deleted && p.rating >= 3).count();
    assert_eq!(h.app.session.catalog.album_count(a.id), n3);
    h.settle(SETTLE);
}

/// Given the rule editor, the field menu shows the top-level fields and one submenu per group;
/// picking a field from a submenu changes the rule and gives it an operator of that field.
#[test]
fn smart_album_field_menu_groups_fields_in_submenus() {
    let mut h = detail("panel.edit");
    exec(&mut h, "dialog.smartAlbum", json!({"name": "Grouped"}));
    let has = |h: &Headless, id: &str| h.app.widgets.iter().any(|(w, _)| w == id);
    let r = h.request("ui.clickWidget", json!({"id": "ruleField:rules-0"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(has(&h, "ruleFieldItem:rating:rules-0"), "top-level fields are in the menu itself");
    for (group, _) in lcc_catalog::rules::FIELD_GROUPS {
        assert!(has(&h, &format!("ruleFieldGroup:{group}:rules-0")), "no {group} submenu");
    }
    assert!(!has(&h, "ruleFieldItem:filePath:rules-0"), "grouped fields wait in their submenu");
    // clicking a group row (a touch, or a click faster than hover) opens it, not closes the menu
    let r = h.request("ui.clickWidget", json!({"id": "ruleFieldGroup:File:rules-0"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let r = h.request("ui.clickWidget", json!({"id": "ruleFieldItem:filePath:rules-0"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let Some(crate::state::Dialog::SmartRules { rules, .. }) = &h.app.ui.dialog else { panic!("no rule editor") };
    let Rule::Field { field, op, .. } = &rules.rules[0] else { panic!("not a field rule") };
    assert_eq!((field.as_str(), op.as_str()), ("filePath", "contains"), "Rating's ≥ isn't a text operator");
    assert!(!has(&h, "ruleFieldItem:rating:rules-0") && !has(&h, "ruleFieldItem:filePath:rules-0"), "picking a field closes the menu");
}

/// A yes/no rule whose value isn't yes or no (written by an agent or an older version) stays as
/// it is in the editor: the dialog shows the problem and OK doesn't quietly save it as Yes.
#[test]
fn smart_album_editor_keeps_an_unreadable_yes_no_value() {
    let mut h = detail("panel.edit");
    exec(&mut h, "dialog.smartAlbum", json!({"name": "Odd"}));
    let Some(crate::state::Dialog::SmartRules { rules, .. }) = &mut h.app.ui.dialog else { panic!("no rule editor") };
    rules.rules[0] = serde_json::from_value(json!({"field": "edited", "op": "is", "value": "maybe"})).unwrap();
    h.settle(SETTLE);
    let Some(crate::state::Dialog::SmartRules { rules, .. }) = &h.app.ui.dialog else { panic!("no rule editor") };
    let Rule::Field { value, .. } = &rules.rules[0] else { panic!("not a field rule") };
    assert_eq!(value, &json!("maybe"), "drawing the editor doesn't rewrite the value");
    let r = h.request("ui.dialog.confirm", json!({}), T);
    assert!(h.app.session.catalog.albums().all(|a| a.name != "Odd"), "OK refuses it: {r}");
    // nor any other value it can't show as it is (in the last 0 days, 20,000 days, a plain number of
    // days, a "between" that isn't a pair): they stay as stored, and the ones the check refuses are marked
    let odd = json!({"rules": [
        {"field": "captureDate", "op": "inLast", "value": {"n": 0, "unit": "days"}},
        {"field": "captureDate", "op": "inLast", "value": {"n": 20000, "unit": "Days"}},
        {"field": "captureDate", "op": "notInLast", "value": 7},
        {"field": "captureDate", "op": "between", "value": "2026"},
        {"field": "iso", "op": "between", "value": [100]}
    ]});
    let Some(crate::state::Dialog::SmartRules { rules: r, .. }) = &mut h.app.ui.dialog else { panic!("the refused editor stays open") };
    *r = serde_json::from_value(odd.clone()).unwrap();
    h.settle(SETTLE);
    let Some(crate::state::Dialog::SmartRules { rules: r, .. }) = &h.app.ui.dialog else { panic!("no rule editor") };
    assert_eq!(serde_json::to_value(r).unwrap()["rules"], odd["rules"], "unchanged by drawing");
    let marked: Vec<&str> = h.app.widgets.iter().filter_map(|(w, _)| w.strip_prefix("ruleProblem:")).collect();
    assert_eq!(marked, ["rules-0", "rules-3", "rules-4"], "0 days and the half-made betweens");
}

/// The rule editor marks each rule that can't mean anything, inside groups too, and keeps OK
/// disabled until they are fixed; the control channel's confirm is refused the same way.
#[test]
fn smart_album_editor_marks_bad_rules_and_blocks_ok() {
    let mut h = detail("panel.edit");
    exec(&mut h, "dialog.smartAlbum", json!({"name": "Checked"}));
    let set = |h: &mut Headless, rules: serde_json::Value| {
        let Some(crate::state::Dialog::SmartRules { rules: r, .. }) = &mut h.app.ui.dialog else { panic!("no rule editor") };
        *r = serde_json::from_value(json!({"rules": rules})).unwrap();
        h.settle(SETTLE);
    };
    let has = |h: &Headless, id: &str| h.app.widgets.iter().any(|(w, _)| w == id);
    set(
        &mut h,
        json!([
            {"field": "rating", "op": "gte", "value": 3},
            {"field": "captureDate", "op": "is", "value": "banana"},
            {"group": {"match": "any", "rules": [{"field": "iso", "op": "is", "value": 100}, {"field": "rating", "op": "is", "value": 9}]}}
        ]),
    );
    assert!(!has(&h, "ruleProblem:rules-0"), "a good rule isn't marked");
    assert!(has(&h, "ruleProblem:rules-1"), "the bad date is marked");
    assert!(has(&h, "ruleProblem:rules-2-1") && !has(&h, "ruleProblem:rules-2-0"), "inside a group, the bad rule itself");
    let Some(crate::state::Dialog::SmartRules { rules, .. }) = &h.app.ui.dialog else { panic!("no rule editor") };
    assert!(serde_json::to_string(rules).unwrap().contains(r#""value":9"#), "drawing doesn't clamp the rating to 5");
    let r = h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(h.app.session.catalog.albums().all(|a| a.name != "Checked"), "OK is disabled");
    let r = h.request("ui.dialog.confirm", json!({}), T);
    assert!(h.app.session.catalog.albums().all(|a| a.name != "Checked"), "confirm refuses too: {r}");
    assert!(h.app.ui.dialog.is_some(), "and the dialog stays open with the rules");
    // fixed, the marks go and OK saves
    set(&mut h, json!([{"field": "rating", "op": "gte", "value": 3}, {"field": "captureDate", "op": "is", "value": "2026"}]));
    assert!(!h.app.widgets.iter().any(|(w, _)| w.starts_with("ruleProblem:")));
    let r = h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(h.app.session.catalog.albums().any(|a| a.name == "Checked"));
}

/// An Album rule picks its album from a list of the albums and smart albums, not by typing an id.
#[test]
fn smart_album_editor_picks_an_album_from_a_list() {
    let mut h = detail("panel.edit");
    let trip = exec(&mut h, "album.create", json!({"name": "Trip", "addSelected": false}))["id"].as_u64().unwrap();
    let smart = exec(&mut h, "album.createSmart", json!({"name": "Fives", "rules": {"rating": 5}}))["id"].as_u64().unwrap();
    exec(&mut h, "dialog.smartAlbum", json!({"name": "In Trip"}));
    let Some(crate::state::Dialog::SmartRules { rules, .. }) = &mut h.app.ui.dialog else { panic!("no rule editor") };
    rules.rules[0] = serde_json::from_value(json!({"field": "album", "op": "is", "value": 0})).unwrap();
    h.settle(SETTLE);
    assert!(h.app.widgets.iter().any(|(w, _)| w == "ruleProblem:rules-0"), "no album chosen yet");
    let r = h.request("ui.clickWidget", json!({"id": "albumPicker:rules-0"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let has = |h: &Headless, id: String| h.app.widgets.iter().any(|(w, _)| *w == id);
    assert!(has(&h, format!("albumPickerItem:{trip}:rules-0")), "plain albums are offered");
    assert!(has(&h, format!("albumPickerItem:{smart}:rules-0")), "smart albums too");
    let r = h.request("ui.clickWidget", json!({"id": format!("albumPickerItem:{trip}:rules-0")}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let Some(crate::state::Dialog::SmartRules { rules, .. }) = &h.app.ui.dialog else { panic!("no rule editor") };
    let Rule::Field { value, .. } = &rules.rules[0] else { panic!("not a field rule") };
    assert_eq!(value, &json!(trip));
    assert!(!h.app.widgets.iter().any(|(w, _)| w == "ruleProblem:rules-0"));
}

/// Editing an album with rules that need fixing changes nothing, not even its name.
#[test]
fn smart_album_edit_with_bad_rules_keeps_the_name() {
    let mut h = detail("panel.edit");
    let id = exec(&mut h, "album.createSmart", json!({"name": "Keep", "rules": {"rating": 3}}))["id"].as_u64().unwrap();
    exec(&mut h, "dialog.smartAlbum", json!({"id": id}));
    let Some(crate::state::Dialog::SmartRules { name, rules, .. }) = &mut h.app.ui.dialog else { panic!("no rule editor") };
    *name = "Renamed".into();
    *rules = serde_json::from_value(json!({"rules": [{"field": "captureDate", "op": "is", "value": "banana"}]})).unwrap();
    h.settle(SETTLE);
    let r = h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let _ = h.request("ui.dialog.confirm", json!({}), T);
    let album = h.app.session.catalog.album(lcc_catalog::AlbumId(id)).expect("album");
    assert_eq!(album.name, "Keep", "neither OK nor confirm renamed it");
    // fixed: OK renames it and sets the rules in one undo step
    let Some(crate::state::Dialog::SmartRules { rules, .. }) = &mut h.app.ui.dialog else { panic!("the editor stays open") };
    *rules = serde_json::from_value(json!({"rules": [{"field": "captureDate", "op": "is", "value": "2026"}]})).unwrap();
    h.settle(SETTLE);
    let undo = h.app.session.undo.len();
    let r = h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let album = h.app.session.catalog.album(lcc_catalog::AlbumId(id)).expect("album");
    assert_eq!((album.name.as_str(), h.app.session.undo.len()), ("Renamed", undo + 1));
    exec(&mut h, "edit.undo", json!({}));
    let album = h.app.session.catalog.album(lcc_catalog::AlbumId(id)).expect("album");
    assert_eq!(album.name, "Keep", "one undo takes back the name with the rules");
    assert!(album.smart.as_ref().is_some_and(|f| f.rule_set.is_none()));
}

/// The sidebar marks a smart album whose rules no longer check, so it can be fixed.
#[test]
fn sidebar_marks_smart_albums_with_problems() {
    let mut h = library_loupe();
    let trip = exec(&mut h, "album.create", json!({"name": "Trip", "addSelected": false}))["id"].as_u64().unwrap();
    let rules = json!({"ruleSet": {"rules": [{"field": "album", "op": "is", "value": trip}]}});
    let id = exec(&mut h, "album.createSmart", json!({"name": "In Trip", "rules": rules}))["id"].as_u64().unwrap();
    let r = h.request("ui.set", json!({"view": "photoGrid", "leftPanel": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(h.app.widgets.iter().any(|(w, _)| *w == format!("source:album:{id}")), "the album's row is on screen");
    let marked = |h: &Headless| h.app.widgets.iter().any(|(w, _)| *w == format!("albumProblem:{id}"));
    assert!(!marked(&h));
    exec(&mut h, "album.delete", json!({"id": trip}));
    h.settle(SETTLE);
    assert!(marked(&h), "its album is gone");
}

/// Editing "Excluded Photos" while "Travel" tests it: the list shows the album itself and Travel
/// greyed (testing Travel would loop back), and a rule set to Travel by other means is marked.
#[test]
fn smart_album_editor_keeps_albums_from_including_themselves() {
    let mut h = detail("panel.edit");
    let trip = exec(&mut h, "album.create", json!({"name": "Trip", "addSelected": false}))["id"].as_u64().unwrap();
    let excluded = exec(&mut h, "album.createSmart", json!({"name": "Excluded Photos", "rules": {"rating": 1}}))["id"].as_u64().unwrap();
    let rules = json!({"ruleSet": {"rules": [{"field": "album", "op": "isNot", "value": excluded}]}});
    let travel = exec(&mut h, "album.createSmart", json!({"name": "Travel", "rules": rules}))["id"].as_u64().unwrap();
    exec(&mut h, "dialog.smartAlbum", json!({"id": excluded}));
    let set = |h: &mut Headless, value: serde_json::Value| {
        let Some(crate::state::Dialog::SmartRules { rules, .. }) = &mut h.app.ui.dialog else { panic!("no rule editor") };
        rules.rules = vec![serde_json::from_value(json!({"field": "album", "op": "is", "value": value})).unwrap()];
        h.settle(SETTLE);
    };
    set(&mut h, json!(null));
    let r = h.request("ui.clickWidget", json!({"id": "albumPicker:rules-0"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let has = |h: &Headless, id: String| h.app.widgets.iter().any(|(w, _)| *w == id);
    assert!(has(&h, format!("albumPickerItem:{trip}:rules-0")));
    // itself and an album that tests it are shown, greyed: clicking them picks nothing
    for blocked in [excluded, travel] {
        let r = h.request("ui.clickWidget", json!({"id": format!("albumPickerItem:{blocked}:rules-0")}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        let Some(crate::state::Dialog::SmartRules { rules, .. }) = &h.app.ui.dialog else { panic!("no rule editor") };
        assert!(matches!(&rules.rules[0], Rule::Field { value, .. } if value.is_null()), "{blocked} wasn't picked");
    }
    let _ = h.request("ui.key", json!({"key": "Escape"}), T);
    h.settle(SETTLE);
    set(&mut h, json!(travel));
    assert!(has(&h, "ruleProblem:rules-0".to_string()), "a loop is marked");
}

#[test]
fn g_toggles_grids_and_shift_g_starts_guided_upright() {
    let mut h = detail("panel.edit");
    // the Alternative set's keys (tests_keymap covers Classic)
    h.app.ui.settings.keymap_set = crate::shortcuts::KeymapSet::Alternative;
    let key = |h: &mut Headless, shift: bool| {
        let r = h.request("ui.key", json!({"key": "g", "shift": shift}), T);
        assert_eq!(r["ok"], true, "{r}");
    };
    key(&mut h, false);
    assert_eq!(h.app.ui.view, crate::state::ViewMode::PhotoGrid);
    key(&mut h, false);
    assert_eq!(h.app.ui.view, crate::state::ViewMode::SquareGrid);
    key(&mut h, false);
    assert_eq!(h.app.ui.view, crate::state::ViewMode::PhotoGrid);
    key(&mut h, true);
    assert_eq!((h.app.ui.view, h.app.ui.right, h.app.ui.tool.as_str()), (crate::state::ViewMode::Detail, RightPanel::Crop, "guidedUpright"));
    assert_eq!(develop(&h).geometry.upright, lcc_develop::Upright::Guided);
    h.settle(SETTLE);
}

#[test]
fn luminance_range_drag_keeps_the_grabbed_handle() {
    let mut h = detail("panel.masking");
    exec(&mut h, "mask.add", json!({"kind": "luminanceRange", "lo": 0.2, "hi": 0.8}));
    h.settle(SETTLE);
    let original = develop(&h);
    for (start, dx, steps) in [(0.2, 180.0, 10), (0.8, -180.0, 10), (0.2, 120.0, 1), (0.8, -120.0, 1)] {
        let undo0 = h.app.session.undo.len();
        let r = h.request("ui.dragWidget", json!({"id": "lumRange:0", "fx": start, "fy": 0.5, "dx": dx, "steps": steps}), T);
        assert_eq!(r["ok"], true, "{r}");
        let d = develop(&h);
        let MaskShape::LuminanceRange { lo, hi, .. } = &d.masks[0].components[0].shape else { panic!() };
        if start < 0.5 {
            assert_eq!(*hi, 0.8, "dragging low must leave high fixed, including past high");
            assert!(*lo > 0.6 && *lo < *hi, "{lo}..{hi}");
        } else {
            assert_eq!(*lo, 0.2, "dragging high must leave low fixed, including past low");
            assert!(*hi < 0.4 && *hi > *lo, "{lo}..{hi}");
        }
        assert_eq!(h.app.session.undo.len(), undo0 + 1);
        exec(&mut h, "edit.undo", json!({}));
        assert_eq!(develop(&h), original);
    }
    // Show Luminance Map: black-and-white overlay, and back
    let r = h.request("ui.clickWidget", json!({"id": "check:lumMap0"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.app.ui.mask_overlay && h.app.ui.mask_overlay_mode == "colorOnBw");
    let r = h.request("ui.clickWidget", json!({"id": "check:lumMap0"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_ne!(h.app.ui.mask_overlay_mode, "colorOnBw");
    h.settle(SETTLE);
}

#[test]
fn b_adds_to_quick_collection_in_the_grid_and_brushes_in_edit() {
    let mut h = detail("panel.edit");
    // the Alternative set's keys (tests_keymap covers Classic)
    h.app.ui.settings.keymap_set = crate::shortcuts::KeymapSet::Alternative;
    // in the loupe B is the masking brush
    let r = h.request("ui.key", json!({"key": "b"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(h.app.ui.tool, "brush");
    assert!(h.app.session.catalog.quick_collection().is_none());
    // in the grid it adds the selection to the Quick Collection
    exec(&mut h, "view.photoGrid", json!({}));
    let r = h.request("ui.key", json!({"key": "b"}), T);
    assert_eq!(r["ok"], true, "{r}");
    let q = h.app.session.catalog.quick_collection().expect("quick collection");
    assert_eq!(h.app.session.catalog.album_count(q), 1);
    h.settle(SETTLE);
}

#[test]
fn local_folder_tree_expands_and_browses() {
    let base = lcc_testkit::TempDir::new("lc-ui-tree");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("Trip/Day 1")).unwrap();
    std::fs::create_dir_all(base.join(".hidden")).unwrap();
    let mut h = library_loupe();
    exec(&mut h, "view.leftPanel", json!({"show": true}));
    h.app.ui.toggle_sidebar_section("panel:navigator");
    // the Local Folders panel at the top of the column (it comes last by default)
    h.app.ui.panel_order = vec![crate::module::PanelId::Local];
    h.hide_home_above(&base);
    exec(&mut h, "local.addRoot", json!({"path": base.to_string_lossy()}));
    exec(&mut h, "library.browse", json!({"path": base.to_string_lossy()}));
    h.settle(SETTLE);
    let base_s = base.to_string_lossy().to_string();
    let r = h.request("ui.clickWidget", json!({"id": format!("folderToggle:{base_s}")}), T);
    assert_eq!(r["ok"], true, "{r}");
    let trip = base.join("Trip").to_string_lossy().to_string();
    // the folder listing can land a few frames later on a loaded machine (FreeBSD CI): wait for the row
    let row = format!("source:local:{trip}");
    h.step_until(SETTLE, |h| h.app.widgets.iter().any(|(w, _)| *w == row));
    let r = h.request("ui.clickWidget", json!({"id": format!("source:local:{trip}")}), T);
    assert_eq!(r["ok"], true, "the subfolder is listed: {r}");
    assert_eq!(h.app.session.browse.as_ref().map(|b| b.path.clone()), Some(trip.clone()), "clicking it browses it");
    let hidden = base.join(".hidden").to_string_lossy().to_string();
    let r = h.request("ui.clickWidget", json!({"id": format!("source:local:{hidden}")}), T);
    assert_ne!(r["ok"], true, "hidden folders are not listed");
    let _ = std::fs::remove_dir_all(&base);
    h.settle(SETTLE);
}

/// A folder listed twice in Local (Pictures as a place of its own and again inside Home) opens
/// and closes on each row by itself: both copies of its triangle used to share one egui id, so
/// the copy drawn later took the clicks of the one drawn first (issue #1093).
#[test]
fn local_folder_listed_twice_opens_and_closes_each_row() {
    let base = lcc_testkit::TempDir::new("lc-ui-twice");
    let _ = std::fs::remove_dir_all(&base);
    let inner = base.join("Inner");
    std::fs::create_dir_all(inner.join("Sub")).unwrap();
    let mut h = library_loupe();
    // tall enough that both rows sit above the left column's Import / Export bar (#118): a row
    // scrolled under it is still registered, and a click there lands on the bar
    assert_eq!(h.request("ui.resize", json!({"width": 1600, "height": 1400}), T)["ok"], true);
    exec(&mut h, "view.leftPanel", json!({"show": true}));
    h.hide_home_above(&base);
    let (base_s, inner_s) = (base.to_string_lossy().to_string(), inner.to_string_lossy().to_string());
    let sub = inner.join("Sub").to_string_lossy().to_string();
    exec(&mut h, "local.addRoot", json!({"path": base_s}));
    exec(&mut h, "local.addRoot", json!({"path": inner_s}));
    h.settle(SETTLE);
    let r = h.request("ui.clickWidget", json!({"id": format!("folderToggle:{base_s}")}), T);
    assert_eq!(r["ok"], true, "{r}");
    let toggle = format!("folderToggle:{inner_s}");
    h.step_until(SETTLE, |h| h.app.widgets.iter().filter(|(w, _)| *w == toggle).count() == 2);
    let rows = |h: &Headless, id: &str| h.app.widgets.iter().filter(|(w, _)| w == id).count();
    assert_eq!(rows(&h, &toggle), 2, "Inner is listed inside the base folder and as a place of its own");
    let sub_row = format!("source:local:{sub}");
    assert_eq!(rows(&h, &sub_row), 0);
    // each copy's triangle opens that copy only, and closes it again
    for copy in 0..2 {
        let at = h.app.widgets.iter().filter(|(w, _)| *w == toggle).nth(copy).map(|(_, r)| r.center()).unwrap();
        let r = h.request("ui.click", json!({"x": at.x, "y": at.y}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step_until(SETTLE, |h| h.app.widgets.iter().any(|(w, _)| *w == sub_row));
        assert_eq!(rows(&h, &sub_row), 1, "copy {copy} opens, the other stays closed");
        let at = h.app.widgets.iter().filter(|(w, _)| *w == toggle).nth(copy).map(|(_, r)| r.center()).unwrap();
        let r = h.request("ui.click", json!({"x": at.x, "y": at.y}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        assert_eq!(rows(&h, &sub_row), 0, "copy {copy} closes again");
    }
    let _ = std::fs::remove_dir_all(&base);
    h.settle(SETTLE);
}

#[test]
fn edit_in_external_editor_opens_the_copy() {
    let dir = lcc_testkit::TempDir::new("lc-ui-ext");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let opened: std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>> = Default::default();
    let o = opened.clone();
    let services = Services {
        png: None,
        open_with: Some(Box::new(move |path: &str, app: &str| {
            o.lock().unwrap().push((path.to_string(), app.to_string()));
            Ok(())
        })),
        ..Default::default()
    };
    let app = LccApp::new(lcc_engine::Session::with_demo().with_fs(), services);
    let mut h = Headless::new(app, [1200.0, 1000.0], 1.0);
    h.app.ui.settings.external_editor = "PhotoCraft".into();
    let r = h.request("engine.execute", json!({"command": "photo.editInExternal", "params": {"dir": dir.to_string_lossy()}}), T);
    assert_eq!(r["ok"], true, "{r}");
    let calls = opened.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].0.ends_with("-Edit.tif") && std::path::Path::new(&calls[0].0).exists(), "{calls:?}");
    assert_eq!(calls[0].1, "PhotoCraft");
    let _ = std::fs::remove_dir_all(&dir);
    h.settle(SETTLE);
}

/// An editor that won't open the copy: the copy is in the library already, so the command succeeds
/// with `failed` set and a toast saying why, instead of an error a script would retry (and make
/// another copy each time).
#[test]
fn edit_in_reports_an_editor_that_wont_open_without_failing() {
    let dir = lcc_testkit::TempDir::new("lc-ui-ext-fail");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let services = Services { png: None, open_with: Some(Box::new(|_: &str, _: &str| Err("the editor quit".into()))), ..Default::default() };
    let app = LccApp::new(lcc_engine::Session::with_demo().with_fs(), services);
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    h.app.ui.settings.external_editor = "PhotoCraft".into();
    let n = h.app.session.catalog.len();
    let r = h.request("engine.execute", json!({"command": "photo.editInExternal", "params": {"dir": dir.to_string_lossy()}}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["result"]["failed"], "the editor quit", "{r}");
    assert_eq!(h.app.session.catalog.len(), n + 1, "the copy stays in the library");
    let toast = h.app.ui.toast().map(|t| t.0.clone()).unwrap_or_default();
    assert!(toast.contains("the editor quit"), "{toast}");
    let _ = std::fs::remove_dir_all(&dir);
    h.settle(SETTLE);
}

/// Edit in PhotoCraft: a PSD copy (what PhotoCraft saves back to), handed to "PhotoCraft" for
/// the host to find; when the host can't find it, no copy is made.
#[test]
fn edit_in_photocraft_opens_a_psd_copy() {
    let dir = lcc_testkit::TempDir::new("lc-ui-pcraft");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let opened: std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>> = Default::default();
    let installed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (o, i) = (opened.clone(), installed.clone());
    let services = Services {
        png: None,
        open_with: Some(Box::new(move |path: &str, app: &str| {
            o.lock().unwrap().push((path.to_string(), app.to_string()));
            Ok(())
        })),
        check_editor: Some(Box::new(move |app: &str| {
            if app == crate::state::PHOTOCRAFT && !i.load(std::sync::atomic::Ordering::SeqCst) {
                Err("PhotoCraft isn't installed".into())
            } else {
                Ok(())
            }
        })),
        ..Default::default()
    };
    let app = LccApp::new(lcc_engine::Session::with_demo().with_fs(), services);
    let mut h = Headless::new(app, [1200.0, 1000.0], 1.0);
    let n = h.app.session.catalog.len();
    let params = json!({"command": "photo.editInPhotoCraft", "params": {"dir": dir.to_string_lossy()}});
    let r = h.request("engine.execute", params.clone(), T);
    assert_eq!(r["ok"], false, "{r}");
    assert_eq!((h.app.session.catalog.len(), opened.lock().unwrap().len()), (n, 0), "no copy without PhotoCraft");
    // and it says so, however it was asked for (menu, shortcut, context menu)
    let toast = h.app.ui.toast().map(|t| t.0.clone()).unwrap_or_default();
    assert!(!toast.is_empty(), "the missing editor is reported");
    h.app.ui.toasts.clear();
    let r = h.app.run("photo.editInPhotoCraft", json!({}));
    assert!(r.is_err() && h.app.ui.toast().is_some(), "{r:?} {:?}", h.app.ui.toasts);
    installed.store(true, std::sync::atomic::Ordering::SeqCst);
    let r = h.request("engine.execute", params, T);
    assert_eq!(r["ok"], true, "{r}");
    let calls = opened.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].0.ends_with("-Edit.psd") && std::path::Path::new(&calls[0].0).exists(), "{calls:?}");
    assert_eq!(calls[0].1, "PhotoCraft");
    let d = lcc_codecs::decode(&std::fs::read(&calls[0].0).unwrap(), Default::default()).unwrap();
    assert_eq!(d.bit_depth, 16, "16-bit by default");
    // a portable copy: its path is what the host is given; the file follows the settings
    h.app.ui.settings.photocraft_path = "/opt/pc/photocraft".into();
    h.app.ui.settings.photocraft_file.format = crate::state::EditFormat::Tiff;
    h.app.ui.settings.photocraft_file.bit_depth = 8;
    let r = h.request("engine.execute", json!({"command": "photo.editInPhotoCraft", "params": {"dir": dir.to_string_lossy()}}), T);
    assert_eq!(r["ok"], true, "{r}");
    let last = opened.lock().unwrap().last().cloned().unwrap();
    assert!(last.0.ends_with(".tif") && last.1 == "/opt/pc/photocraft", "{last:?}");
    assert!(crate::menus::ui_enabled(&h.app, "photo.editInPhotoCraft"));
    let _ = std::fs::remove_dir_all(&dir);
    h.settle(SETTLE);
}

/// Open Original in Other Editor: each selected photo's own file (a raw stays a raw) goes to the
/// configured editor, untouched and without a copy; demo photos and missing files say why.
#[test]
fn open_original_in_external_editor_hands_over_the_raw_itself() {
    use lcc_catalog::{Op, Photo, Source};
    let dir = lcc_testkit::TempDir::new("lc-ui-orig");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let opened: std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>> = Default::default();
    let o = opened.clone();
    let services = Services {
        png: None,
        open_with: Some(Box::new(move |path: &str, app: &str| {
            o.lock().unwrap().push((path.to_string(), app.to_string()));
            Ok(())
        })),
        check_editor: Some(Box::new(|_: &str| Ok(()))),
        ..Default::default()
    };
    let mut session = lcc_engine::Session::with_demo().with_fs();
    let demo = session.catalog.photos().map(|p| p.id).next().unwrap();
    let mut file = |name: &str, exists: bool| {
        let path = dir.join(name);
        if exists {
            std::fs::write(&path, b"raw sensor data").unwrap();
        }
        let id = session.catalog.alloc_photo_id();
        let p = Photo::new(id, Source::File { path: path.to_string_lossy().into_owned() }, name, "NEF", 60, 40, "2026-01-01T10:00:00");
        session.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        (id, path.to_string_lossy().into_owned())
    };
    let (a, a_path) = file("DSC_0001.NEF", true);
    let (b, b_path) = file("DSC_0002.NEF", true);
    let (gone, _) = file("DSC_0003.NEF", false);
    let app = LccApp::new(session, services);
    let mut h = Headless::new(app, [1200.0, 1000.0], 1.0);
    h.app.ui.settings.external_editor = "DxO PhotoLab".into();
    let n = h.app.session.catalog.len();

    exec(&mut h, "library.select", json!({"ids": [a.0, b.0, demo.0]}));
    let r = exec(&mut h, "photo.openOriginalInExternal", json!({}));
    let calls = opened.lock().unwrap().clone();
    assert_eq!(calls, vec![(a_path.clone(), "DxO PhotoLab".to_string()), (b_path.clone(), "DxO PhotoLab".to_string())]);
    assert_eq!(r["skipped"][0]["id"], demo.0, "{r}");
    assert_eq!(h.app.session.catalog.len(), n, "no copy is added to the library");
    assert_eq!(std::fs::read(&a_path).unwrap(), b"raw sensor data", "the original is left as it is");
    assert!(std::fs::read_dir(&dir).unwrap().count() == 2, "nothing is written next to the originals");

    // the app can name the editor per call, an agent over the control channel can't; the active
    // photo must have a file for the menu
    let r = h.app.run("photo.openOriginalInExternal", json!({"ids": [a.0], "app": "/opt/dxo"})).unwrap();
    assert_eq!(r["opened"][0]["path"], a_path.as_str());
    assert_eq!(opened.lock().unwrap().last().unwrap().1, "/opt/dxo");
    let r = h.request("engine.execute", json!({"command": "photo.openOriginalInExternal", "params": {"ids": [a.0], "app": "/bin/sh"}}), T);
    assert_eq!(r["ok"], false, "{r}");
    assert_eq!(opened.lock().unwrap().last().unwrap().1, "/opt/dxo", "nothing more opened");
    exec(&mut h, "library.select", json!({"ids": [a.0]}));
    assert!(crate::menus::ui_enabled(&h.app, "photo.openOriginalInExternal"));
    exec(&mut h, "library.select", json!({"ids": [demo.0]}));
    assert!(!crate::menus::ui_enabled(&h.app, "photo.openOriginalInExternal"));

    // a host that can start the editor once with every file does (DxO PhotoLab keeps one window
    // and drops the files that arrive while it starts)
    let batches: std::sync::Arc<std::sync::Mutex<Vec<(Vec<String>, String)>>> = Default::default();
    let bt = batches.clone();
    h.app.services.open_all_with = Some(Box::new(move |paths: &[String], app: &str| {
        bt.lock().unwrap().push((paths.to_vec(), app.to_string()));
        Ok(())
    }));
    let singles = opened.lock().unwrap().len();
    let r = exec(&mut h, "photo.openOriginalInExternal", json!({"ids": [a.0, b.0, demo.0]}));
    assert_eq!(r["opened"].as_array().map(Vec::len), Some(2), "{r}");
    assert_eq!(*batches.lock().unwrap(), vec![(vec![a_path.clone(), b_path.clone()], "DxO PhotoLab".to_string())]);
    assert_eq!(opened.lock().unwrap().len(), singles, "one start for all, not one per file");

    // a demo photo or a missing file: a clear error, nothing opened
    let calls = opened.lock().unwrap().len();
    for (ids, why) in [(json!([demo.0]), "demo"), (json!([gone.0]), "missing")] {
        let r = h.request("engine.execute", json!({"command": "photo.openOriginalInExternal", "params": {"ids": ids}}), T);
        assert_eq!(r["ok"], false, "{r}");
        assert!(r.to_string().contains(why), "{r}");
    }
    assert_eq!(opened.lock().unwrap().len(), calls);
    let _ = std::fs::remove_dir_all(&dir);
    h.settle(SETTLE);
}

/// Issue #2135: a library record whose original is a program (an imported `.lrcat` names the
/// paths) is never handed to the opener — the launcher is asked for nothing — and the user is
/// told why. The path the app does hand over is absolute, whatever the record's spelling.
#[test]
fn open_original_refuses_anything_that_is_not_a_photo_file() {
    use lcc_catalog::{Op, Photo, Source};
    let dir = lcc_testkit::TempDir::new("lc-ui-gate");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let opened: std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>> = Default::default();
    let batches: std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>> = Default::default();
    let (o, bt) = (opened.clone(), batches.clone());
    let services = Services {
        png: None,
        open_with: Some(Box::new(move |path: &str, app: &str| {
            o.lock().unwrap().push((path.to_string(), app.to_string()));
            Ok(())
        })),
        open_all_with: Some(Box::new(move |paths: &[String], _: &str| {
            bt.lock().unwrap().push(paths.to_vec());
            Ok(())
        })),
        check_editor: Some(Box::new(|_: &str| Ok(()))),
        ..Default::default()
    };
    let mut session = lcc_engine::Session::with_demo().with_fs();
    let mut record = |name: &str| {
        let path = dir.join(name);
        std::fs::write(&path, b"MZ").unwrap();
        let id = session.catalog.alloc_photo_id();
        let p = Photo::new(id, Source::File { path: path.to_string_lossy().into_owned() }, name, "ARW", 60, 40, "2026-01-01T10:00:00");
        session.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        id
    };
    // what a crafted catalog can name as a photo's original
    let programs = ["payload.exe", "start.lnk", "run.bat", "page.hta", "go.command", "noext"];
    let bad: Vec<_> = programs.iter().map(|n| record(n)).collect();
    let good = record("DSC_0001.NEF");
    let app = LccApp::new(session, services);
    let mut h = Headless::new(app, [1200.0, 1000.0], 1.0);
    h.app.ui.settings.external_editor = "DxO PhotoLab".into();

    for (id, name) in bad.iter().zip(programs) {
        let r = h.request("engine.execute", json!({"command": "photo.openOriginalInExternal", "params": {"ids": [id.0]}}), T);
        assert_eq!(r["ok"], false, "{name} was opened: {r}");
        assert!(r.to_string().contains("file types the app reads"), "{name}: {r}");
        // the menu item is greyed out for such a record, so it is never offered either
        exec(&mut h, "library.select", json!({"ids": [id.0]}));
        assert!(!crate::menus::ui_enabled(&h.app, "photo.openOriginalInExternal"), "{name} is offered in the menu");
    }
    assert!(opened.lock().unwrap().is_empty() && batches.lock().unwrap().is_empty(), "the launcher was asked to open a program");

    // a program selected together with a photo: only the photo is handed over, by absolute path
    let r = exec(&mut h, "photo.openOriginalInExternal", json!({"ids": [bad[0].0, good.0]}));
    assert_eq!(r["opened"].as_array().map(Vec::len), Some(1), "{r}");
    assert!(r["skipped"].to_string().contains("payload.exe"), "{r}");
    let handed = batches.lock().unwrap().clone();
    assert_eq!(handed.len(), 1);
    assert_eq!(handed[0].len(), 1, "{handed:?}");
    assert!(std::path::Path::new(&handed[0][0]).is_absolute() && handed[0][0].ends_with("DSC_0001.NEF"), "{handed:?}");
    let _ = std::fs::remove_dir_all(&dir);
    h.settle(SETTLE);
}

#[test]
fn second_window_shows_the_active_photo() {
    let mut h = detail("panel.edit");
    exec(&mut h, "view.photoGrid", json!({}));
    exec(&mut h, "view.secondWindow", json!({"show": true}));
    h.settle(SETTLE);
    // headless has no native windows: it's embedded, and renders the photo for its own slot
    let tex = h.app.renderer.textures.get(&crate::render::Slot::Second).map(|t| t.photo);
    assert_eq!(tex, h.app.session.active(), "the second window has its own render of the active photo");
    let r = h.request("ui.clickWidget", json!({"id": "view:secondWindow"}), T);
    assert_eq!(r["ok"], true, "on screen: {r}");
    exec(&mut h, "view.secondWindow", json!({}));
    assert!(!h.app.ui.second_window);
    h.settle(SETTLE);
}

#[test]
fn keyword_painter_toggles_on_click() {
    let mut h = detail("panel.edit");
    exec(&mut h, "tool.keywordPainter", json!({"keyword": "harbour"}));
    assert_eq!(h.app.ui.view, crate::state::ViewMode::PhotoGrid);
    h.settle(SETTLE);
    let target = h.app.session.visible_cloned()[0];
    let has = |h: &Headless| h.app.session.catalog.photo(target).unwrap().meta.keywords.iter().any(|k| k == "harbour");
    let sel = h.app.session.selection.clone();
    // a click: press and release on separate frames, no movement
    let click = |h: &mut Headless| {
        let w = h.request("ui.widgets", json!({}), T);
        let r = w["result"]
            .as_array()
            .and_then(|a| a.iter().find(|x| x["id"] == format!("thumb:{}", target.0)))
            .map(|x| x["rect"].clone())
            .expect("thumb on screen");
        let (x, y) = (r[0].as_f64().unwrap() + r[2].as_f64().unwrap() / 2.0, r[1].as_f64().unwrap() + r[3].as_f64().unwrap() / 2.0);
        let r = h.request("ui.drag", json!({"x": x, "y": y, "toX": x, "toY": y, "steps": 2}), T);
        assert_eq!(r["ok"], true, "{r}");
    };
    click(&mut h);
    assert!(has(&h), "painted");
    assert_eq!(h.app.session.selection, sel, "painting doesn't select");
    click(&mut h);
    assert!(!has(&h), "a second click takes it away");
    h.request("ui.key", json!({"key": "escape"}), T);
    assert!(h.app.ui.lib.painter.is_none());
    h.settle(SETTLE);
}

#[test]
fn reference_view_pins_a_photo_beside_the_active_one() {
    let mut h = detail("panel.edit");
    let first = h.app.session.active().unwrap();
    exec(&mut h, "photo.setReference", json!({}));
    let r = h.request("ui.key", json!({"key": "r", "shift": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(h.app.ui.view, crate::state::ViewMode::Reference);
    assert_eq!(h.app.ui.reference, Some(first.0));
    let active = h.app.session.active().unwrap();
    assert_ne!(active, first, "the next photo is the one being edited");
    // edits go to the active photo, the reference stays put
    exec(&mut h, "develop.set", json!({"control": "light.exposure", "value": 0.7}));
    assert_eq!(h.app.session.develop_of(active).unwrap().light.exposure, 0.7);
    assert_ne!(h.app.session.develop_of(first).unwrap().light.exposure, 0.7);
    h.settle(SETTLE);
    assert!(h.app.renderer.textures.get(&crate::render::Slot::Compare(0)).is_some_and(|t| t.photo == first), "the reference is drawn");
}

#[test]
fn soft_proofing_flags_out_of_gamut_colours_and_makes_proof_copies() {
    let dir = lcc_testkit::TempDir::new("proof-profile");
    let mut h = detail("panel.edit");
    if h.app.ui.right != RightPanel::Edit {
        exec(&mut h, "panel.edit", json!({}));
    }
    assert_eq!(h.app.ui.right, RightPanel::Edit);
    h.app.renderer.keep_pixels = true;
    exec(&mut h, "develop.set", json!({"control": "color.saturation", "value": 100}));
    exec(&mut h, "develop.set", json!({"control": "color.vibrance", "value": 100}));
    let red = |h: &Headless| {
        let t = h.app.renderer.textures.get(&crate::render::Slot::Main).expect("loupe render");
        t.pixels.as_ref().expect("pixels kept").pixels.iter().filter(|c| c.r() == 255 && c.g() == 0 && c.b() == 0).count()
    };
    h.settle(SETTLE);
    let before = red(&h);
    // S in the loupe: soft proofing on; the warning is the proof's own setting
    let r = h.request("ui.key", json!({"key": "s"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.app.ui.soft_proof);
    let st = exec(&mut h, "view.softProof", json!({"space": "srgb", "destWarning": true}));
    assert_eq!(
        st,
        json!({"on": true, "space": "srgb", "profile": null, "intent": "perceptual", "paperInk": false, "destWarning": true, "displayWarning": false})
    );
    h.settle(SETTLE);
    assert!(red(&h) > before, "out-of-gamut colours are painted red");
    let r = h.request("engine.execute", json!({"command": "view.softProof", "params": {"space": "cmyk"}}), T);
    assert_ne!(r["ok"], true);
    // a device (printer) profile, its intent and Simulate Paper & Ink
    std::fs::create_dir_all(&dir).unwrap();
    let icc = dir.join("printer.icc");
    std::fs::write(&icc, lcc_codecs::icc::write_named(lcc_codecs::NamedSpace::Srgb)).unwrap();
    let st = exec(&mut h, "view.softProof", json!({"profile": icc.to_string_lossy(), "intent": "relative", "paperInk": true}));
    assert_eq!((st["intent"].as_str(), st["paperInk"].as_bool()), (Some("relative"), Some(true)), "{st}");
    assert!(st["profile"]["name"].as_str().is_some_and(|n| !n.is_empty()), "{st}");
    assert_ne!(h.app.ui.proof.profile, 0);
    h.settle(SETTLE);
    assert!(red(&h) > before, "the device gamut is flagged too");
    let r = h.request("engine.execute", json!({"command": "view.softProof", "params": {"profile": "/nonexistent.icc"}}), T);
    assert_ne!(r["ok"], true);
    let st = exec(&mut h, "view.softProof", json!({"profile": null}));
    assert_eq!((st["profile"].clone(), h.app.ui.proof.profile), (json!(null), 0));
    let _ = std::fs::remove_dir_all(&dir);
    // Create Proof Copy: a virtual copy named after the proof
    let n = h.app.session.catalog.photos().count();
    let r = h.request("ui.clickWidget", json!({"id": "button:createProofCopy"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(h.app.session.catalog.photos().count(), n + 1);
    let copy = (**h.app.session.catalog.photos().max_by_key(|p| p.id.0).unwrap()).clone();
    assert_eq!(copy.copy_name.as_deref(), Some("Proof Copy (sRGB)"));
    // S again turns it off; in a grid S is Expand/Collapse Stack (the copy made a stack)
    h.request("ui.key", json!({"key": "s"}), T);
    assert!(!h.app.ui.soft_proof);
    exec(&mut h, "view.photoGrid", json!({}));
    let stack = h.app.session.catalog.stack_of(copy.id).cloned().expect("stacked with its original");
    h.request("ui.key", json!({"key": "s"}), T);
    assert!(!h.app.ui.soft_proof);
    assert_ne!(h.app.session.catalog.stack_of(copy.id).unwrap().collapsed, stack.collapsed, "S toggled the stack");
}

/// Object / Describe without the SAM 3 model: the download is offered (never started without a
/// yes), the dialog says why it can't start when no mirror is configured, nothing freezes and no
/// empty mask is left behind.
#[test]
fn ai_masks_without_the_model_offer_the_download() {
    use crate::state::Dialog;
    use lcc_engine::segment::Segmenter;
    let dir = lcc_testkit::TempDir::new("lc-ui-no-sam3");
    let mut h = detail("panel.masking");
    h.app.session.segmenter.dir = Some(dir.to_path_buf());
    h.app.session.segmenter.mirrors_file = Some(dir.join("none.txt"));
    let t = std::time::Instant::now();
    let r = h.request("ui.clickWidget", json!({"id": "maskNew:object"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(develop(&h).masks.is_empty());
    if !Segmenter::AVAILABLE {
        // a build without AI masks says so (a toast), no dialog
        assert!(h.app.ui.dialog.is_none());
        return;
    }
    assert_eq!(h.app.ui.dialog, Some(Dialog::Settings { tab: "ai".into() }));
    assert!(!h.app.session.segmenter.download_status().running, "nothing downloads without a yes");
    if h.app.session.segmenter.mirrors().is_empty() {
        // no location configured in this build: no Download button (only Close), and an agent
        // confirming anyway gets the reason; the dialog stays
        h.step();
        // …but a way to install it by hand: the guide, and the model folder (created on demand)
        assert!(h.app.widgets.iter().any(|(id, _)| id == "link:samHelp"), "no installation guide link");
        let shown = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let log = shown.clone();
        h.app.services.open_with = Some(Box::new(move |p: &str, editor: &str| {
            assert_eq!(editor, "", "open the model directory with the default application");
            log.lock().unwrap().push(p.to_string());
            Ok(())
        }));
        h.step();
        crate::settings_screen::reveal(&mut h, "button:samFolder");
        let r = h.request("ui.clickWidget", json!({"id": "button:samFolder"}), T);
        assert_eq!(r["ok"], true, "{r}");
        assert!(dir.is_dir(), "the model folder is created to be shown");
        assert_eq!(*shown.lock().unwrap(), vec![dir.to_string_lossy().to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
        // Settings ▸ AI has no download to confirm here: confirming just closes it
        assert!(matches!(&h.app.ui.dialog, Some(Dialog::Settings { tab }) if tab == "ai"), "stays open");
    }
    let r = h.request("ui.dialog.confirm", json!({}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.app.ui.dialog.is_none());
    // Describe asks too
    let r = h.request("ui.clickWidget", json!({"id": "maskNew:prompt"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(h.app.ui.dialog, Some(Dialog::Settings { tab: "ai".into() }));
    assert_eq!(h.app.ui.describe, None);
    // a click command from an agent gets the not-installed error at once, and the app offers the model
    let e = h.app.run("mask.add", json!({"kind": "prompt", "text": "sky"})).unwrap_err();
    assert!(e.starts_with(lcc_engine::segment::NOT_INSTALLED), "{e}");
    assert!(develop(&h).masks.is_empty());
    assert!(t.elapsed() < SETTLE, "{:?}", t.elapsed());
}

fn color_samples(h: &Headless) -> serde_json::Value {
    let d = develop(h);
    let m = d.masks.last().expect("a mask");
    let Some(MaskShape::ColorRange { samples, .. }) = m.components.iter().map(|c| &c.shape).find(|s| matches!(s, MaskShape::ColorRange { .. }))
    else {
        panic!("colour range")
    };
    serde_json::to_value(samples).expect("samples")
}

/// Same samples, up to sub-pixel rounding and the preview level sampled.
#[track_caller]
fn assert_close(a: &serde_json::Value, b: &serde_json::Value) {
    let flat = |v: &serde_json::Value| -> Vec<f64> {
        v.as_array().unwrap().iter().flat_map(|s| s.as_array().unwrap().iter().map(|x| x.as_f64().unwrap())).collect()
    };
    let (a, b) = (flat(a), flat(b));
    assert_eq!(a.len(), b.len());
    assert!(a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 0.05), "{a:?} vs {b:?}");
}

/// A gradient subtracted from (or intersected with) a brush mask while the brush is still armed must be
/// draggable: the brush took every press on the photo, so its handles were dead and each drag painted a stroke.
#[test]
fn adding_a_gradient_to_a_brush_mask_disarms_the_brush() {
    for op in ["subtract", "intersect"] {
        let mut h = detail("panel.masking");
        exec(&mut h, "tool.brush", json!({"new": true}));
        assert_eq!(h.app.ui.tool, "brush", "the brush is armed");
        brush_at(&mut h, 0.5, 0.5);
        exec(&mut h, "mask.addComponent", json!({"kind": "radial", "op": op, "center": [0.5, 0.5], "rx": 0.22, "ry": 0.22}));
        assert!(h.app.ui.tool.is_empty(), "{op}: adding a radial component disarms the brush");
    }

    // a brush component of the brush's own kind keeps it armed
    let mut h = detail("panel.masking");
    exec(&mut h, "tool.brush", json!({"new": true}));
    brush_at(&mut h, 0.5, 0.5);
    exec(&mut h, "mask.addComponent", json!({"kind": "brush", "op": "subtract"}));
    assert_eq!(h.app.ui.tool, "brush", "Subtract > Brush keeps the brush armed");

    // an omitted kind is a brush, as in the engine: the brush stays armed for painting
    let mut h = detail("panel.masking");
    exec(&mut h, "tool.brush", json!({"new": true}));
    brush_at(&mut h, 0.5, 0.5);
    exec(&mut h, "mask.addComponent", json!({"op": "subtract"}));
    assert_eq!(h.app.ui.tool, "brush", "an omitted kind is a brush component and keeps the brush armed");
    assert!(matches!(develop(&h).masks[0].components[1].shape, MaskShape::Brush { .. }), "the engine adds a brush component when kind is omitted");
}

/// The Colour Range eyedropper takes every press on the photo too: a gradient added while it is armed disarms it,
/// another colour range keeps it.
#[test]
fn adding_a_gradient_while_the_color_range_picker_is_armed_disarms_it() {
    let mut h = detail("panel.masking");
    exec(&mut h, "mask.add", json!({"kind": "colorRange"}));
    h.app.ui.tool = "colorRange".into();
    exec(&mut h, "mask.addComponent", json!({"kind": "colorRange", "op": "add"}));
    assert_eq!(h.app.ui.tool, "colorRange", "another colour range keeps the picker armed");
    exec(&mut h, "mask.addComponent", json!({"kind": "radial", "op": "subtract", "center": [0.5, 0.5], "rx": 0.22, "ry": 0.22}));
    assert!(h.app.ui.tool.is_empty(), "adding a radial component disarms the eyedropper");
}

/// Regression: the Colour Range "Pick" button armed the eyedropper, but with the Masking panel
/// open the loupe's clicks went to the mask overlay, so no sample was ever taken.
#[test]
fn color_range_pick_button_samples_the_photo() {
    let mut h = detail("panel.masking");
    exec(&mut h, "mask.add", json!({"kind": "colorRange"}));
    h.app.ui.tool.clear();
    h.settle(SETTLE);
    click(&mut h, "button:colorPick0");
    assert_eq!(h.app.ui.tool, "colorRange", "Pick arms the eyedropper");
    brush_at(&mut h, 0.25, 0.5);
    let ui_sample = color_samples(&h);
    assert_eq!(ui_sample.as_array().map(Vec::len), Some(1), "a click samples a colour");
    // the sample is the colour at that normalized point
    exec(&mut h, "mask.sampleColor", json!({"x": 0.25, "y": 0.5}));
    assert_close(&color_samples(&h), &ui_sample);
    // ⇧-click adds
    let r = h.request("ui.pointer", json!({"shift": true, "events": [{"kind": "down", "x": 0.7, "y": 0.2}, {"kind": "up", "x": 0.7, "y": 0.2}]}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(color_samples(&h).as_array().map(Vec::len), Some(2), "⇧-click adds a sample");

    // cropped and zoomed: the click samples the photo point under it
    exec(&mut h, "crop.set", json!({"rect": [0.2, 0.1, 0.9, 0.8]}));
    h.app.ui.zoom = crate::state::Zoom::Percent(200.0);
    h.app.ui.pan = (0.5, 0.5);
    h.settle(SETTLE);
    brush_at(&mut h, 0.5, 0.5);
    let ui_sample = color_samples(&h);
    assert_eq!(ui_sample.as_array().map(Vec::len), Some(1));
    // (the centre of the crop box, in normalized source coordinates)
    let c = develop(&h).crop.geometry.rect;
    exec(&mut h, "mask.sampleColor", json!({"x": (c.x0 + c.x1) / 2.0, "y": (c.y0 + c.y1) / 2.0}));
    assert_close(&color_samples(&h), &ui_sample); // a cropped, zoomed pick maps to the same frame point
    // Pick again ends picking
    click(&mut h, "button:colorPick0");
    assert!(h.app.ui.tool.is_empty());
    h.settle(SETTLE);
}

#[test]
fn a_selected_develop_slider_accepts_wheel_nudges() {
    let mut h = detail("panel.edit");
    // the demo may open on the Edit panel already, which `panel.edit` then toggles shut
    let r = h.request("ui.set", json!({"right": "edit"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let name = "slider:light.exposure";
    let panel = h.app.widgets.iter().find(|(id, _)| id == "panel:right_panel").unwrap().1;
    let track = h.app.widgets.iter().find(|(id, _)| id == name).unwrap().1;
    assert!(panel.contains(track.center()), "slider must be visible before interaction");

    let result = h.request("ui.clickWidget", json!({"id": name, "fx": 0.5}), T);
    assert_eq!(result["ok"], true, "{result}");
    let before = lcc_develop::controls::get(&develop(&h), "light.exposure").expect("exposure");

    // A wheel event over the selected control adjusts exposure instead of
    // scrolling the right sidebar (an unselected slider leaves scrolling alone).
    // (120 points is more than one notch, `theme::WHEEL_LINE_POINTS`; egui smooths it over a few frames)
    let result = h.request("ui.scroll", json!({"dy": 120.0}), T);
    assert_eq!(result["ok"], true, "{result}");
    for _ in 0..10 {
        h.step();
    }
    let after = lcc_develop::controls::get(&develop(&h), "light.exposure").expect("exposure");
    assert!(after > before, "selected exposure did not increase: {before} -> {after}");
    for _ in 0..40 {
        h.step();
    }
    let before_leaving = develop(&h).light.exposure;
    // Leaving the row disarms wheel edits, but must not take keyboard arrows away.
    h.request("ui.move", json!({"x": panel.left() - 10.0, "y": track.center().y}), T);
    h.request("ui.hoverWidget", json!({"id": name}), T);
    // More than a full notch: disarming dropped the wheel remainder, so this alone would edit it.
    h.request("ui.scroll", json!({"dy": -240.0}), T);
    for _ in 0..40 {
        h.step();
    }
    assert_eq!(develop(&h).light.exposure, before_leaving, "returning to a slider while scrolling must not edit it (issue #1355)");
    h.request("ui.move", json!({"x": panel.left() - 10.0, "y": track.center().y}), T);
    h.request("ui.key", json!({"key": "ArrowUp"}), T);
    assert!(develop(&h).light.exposure > before_leaving, "keyboard adjustment survives leaving the slider");

    // Scroll back to the top, then explicitly select it again to re-enable wheel nudges.
    h.request("ui.move", json!({"x": panel.right() - 8.0, "y": panel.center().y}), T);
    h.request("ui.scroll", json!({"dy": 600.0}), T);
    for _ in 0..40 {
        h.step();
    }
    click(&mut h, name);
    let before_reselecting = develop(&h).light.exposure;
    h.request("ui.scroll", json!({"dy": 120.0}), T);
    for _ in 0..40 {
        h.step();
    }
    assert!(develop(&h).light.exposure > before_reselecting, "a new click re-enables wheel adjustment");
}

/// Scrolling sideways over a slider adjusts it without selecting it first: a trackpad swipe
/// right raises the value, left lowers it, and Shift+wheel (sideways scrolling with a mouse) does
/// the same. A mostly vertical swipe still scrolls the panel and leaves the slider alone.
#[test]
fn sideways_scrolling_adjusts_the_slider_under_the_pointer() {
    let mut h = detail("panel.edit");
    let r = h.request("ui.set", json!({"right": "edit"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let exposure = |h: &Headless| lcc_develop::controls::get(&develop(h), "light.exposure").expect("exposure");
    let scroll = |h: &mut Headless, params: serde_json::Value| {
        // the pointer rests on the track, the slider is not clicked
        let r = h.request("ui.hoverWidget", json!({"id": "slider:light.exposure"}), T);
        assert_eq!(r["ok"], true, "{r}");
        let before = exposure(h);
        let r = h.request("ui.scroll", params, T);
        assert_eq!(r["ok"], true, "{r}");
        for _ in 0..10 {
            h.step();
        }
        exposure(h) - before
    };
    assert!(scroll(&mut h, json!({"dx": 120.0})) > 0.0, "a swipe right raises the value");
    assert!(scroll(&mut h, json!({"dx": -120.0})) < 0.0, "a swipe left lowers it");
    assert!(scroll(&mut h, json!({"dy": 120.0, "shift": true})) > 0.0, "Shift+wheel scrolls sideways");
    let undo = h.app.session.undo.len();
    assert_eq!(scroll(&mut h, json!({"dx": 30.0, "dy": -120.0})), 0.0, "a mostly vertical swipe leaves the slider alone");
    assert_eq!(h.app.session.undo.len(), undo);
}

/// Issue #517: a fast brush stroke (pointer samples several brush widths apart) shows on the photo
/// while the button is held as the continuous, feathered band it will be once committed, not as
/// separate dabs at the samples with gaps between them.
#[test]
fn a_fast_brush_stroke_shows_without_gaps_before_the_button_is_released() {
    let mut h = detail("panel.masking");
    h.settle(SETTLE);
    exec(&mut h, "tool.brush", json!({"new": true}));
    let r = h.request("ui.set", json!({"brushSize": 0.03, "brushFeather": 50.0, "brushFlow": 100.0}), T);
    assert_eq!(r["ok"], true, "{r}");
    // the photo before painting, and where a normalized image point is on screen
    pointer(&mut h, json!([{"kind": "move", "x": 0.02, "y": 0.02}]));
    h.settle(SETTLE);
    let photo = h.paint();
    let img = h.app.image_rect.expect("the photo is on screen");
    let ppp = h.pixels_per_point;
    let px = |im: &egui::ColorImage, x: f32, y: f32| {
        let (sx, sy) = (((img.left() + x * img.width()) * ppp) as usize, ((img.top() + y * img.height()) * ppp) as usize);
        let c = im.pixels[sy * im.size[0] + sx];
        [c.r() as f32, c.g() as f32, c.b() as f32]
    };
    // a drag that starts (past egui's drag threshold, at `a`) and then moves fast: samples far
    // apart, button held
    let (a, b, c) = ((0.17_f32, 0.4_f32), (0.5_f32, 0.4_f32), (0.85_f32, 0.4_f32));
    pointer(
        &mut h,
        json!([{"kind": "down", "x": 0.15, "y": a.1}, {"kind": "drag", "x": a.0, "y": a.1}, {"kind": "drag", "x": b.0, "y": b.1}, {"kind": "drag", "x": c.0, "y": c.1}]),
    );
    assert!(matches!(h.app.gesture, Some(crate::panels::detail::Gesture::Brush { .. })), "still painting");
    let held = h.paint();
    let [r0, g0, b0] = h.app.ui.mask_overlay_color.map(|v| v as f32);
    let op = h.app.ui.mask_overlay_opacity / 100.0;
    // along the centreline between the samples (away from the cursor's rings at the last one: the
    // halo's reaches 1.7 × Size out at Feather 50), the overlay colour at full coverage over the photo
    let mut gaps = vec![];
    for i in 1..=70 {
        let x = a.0 + (c.0 - a.0) * i as f32 / 80.0;
        let p = px(&photo, x, a.1);
        let want = [p[0] + (r0 - p[0]) * op, p[1] + (g0 - p[1]) * op, p[2] + (b0 - p[2]) * op];
        let got = px(&held, x, a.1);
        if (0..3).any(|k| (got[k] - want[k]).abs() > 12.0) {
            gaps.push((x, got, want));
        }
    }
    assert!(gaps.is_empty(), "the live stroke has gaps: {} of 70 points, first {:?}", gaps.len(), gaps.first());
    pointer(&mut h, json!([{"kind": "up", "x": c.0, "y": c.1}]));
    let d = develop(&h);
    let MaskShape::Brush { strokes } = &d.masks[0].components[0].shape else { panic!("brush") };
    assert!(strokes[0].points.len() >= 3, "{strokes:?}");
    // committed: the rendered overlay covers what the live stroke did, feather included
    h.settle(SETTLE);
    let done = h.paint();
    for (x, y) in [(0.3, a.1), (0.6, a.1), (0.3, a.1 + 0.02), (0.6, a.1 - 0.025), (0.45, a.1 + 0.06)] {
        let (l, k) = (px(&held, x, y), px(&done, x, y));
        assert!((0..3).all(|i| (l[i] - k[i]).abs() < 24.0), "live {l:?} vs committed {k:?} at ({x}, {y})");
    }
}

/// Fork #93: with the Masking brush active, a plain mouse-wheel notch over the photo resizes the brush (as
/// `[` / `]` do) instead of zooming; with the preference off, the wheel zooms again.
#[test]
fn a_wheel_notch_resizes_the_brush_instead_of_zooming() {
    let mut h = detail("panel.masking");
    h.settle(SETTLE);
    exec(&mut h, "tool.brush", json!({"new": true}));
    pointer(&mut h, json!([{"kind": "move", "x": 0.5, "y": 0.5}]));
    h.settle(SETTLE);
    let notch = |h: &mut Headless, dy: f32| {
        h.events.push(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: egui::vec2(0.0, dy),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
        });
        for _ in 0..30 {
            h.step();
        }
    };
    let (size0, zoom0) = (h.app.ui.brush_size, h.app.ui.zoom);
    notch(&mut h, 1.0);
    assert!(h.app.ui.brush_size > size0, "a notch up grows the brush: {size0} -> {}", h.app.ui.brush_size);
    assert_eq!(h.app.ui.zoom, zoom0, "and does not zoom");
    let size1 = h.app.ui.brush_size;
    notch(&mut h, -1.0);
    assert!(h.app.ui.brush_size < size1, "a notch down shrinks it");
    // the preference off: the wheel zooms, the brush keeps its size
    h.app.ui.settings.wheel_resizes_brush = false;
    let size2 = h.app.ui.brush_size;
    notch(&mut h, 1.0);
    assert_eq!(h.app.ui.brush_size, size2);
    assert_ne!(h.app.ui.zoom, zoom0, "with the preference off the wheel zooms");
}

/// Issue #808: with the brush active, a trackpad two-finger vertical scroll over the photo resizes the brush
/// instead of panning; holding Space bar lets the gesture move the frame around (pan).
#[test]
fn trackpad_scroll_resizes_brush_and_space_pans() {
    let mut h = detail("panel.masking");
    h.settle(SETTLE);
    exec(&mut h, "tool.brush", json!({"new": true}));
    pointer(&mut h, json!([{"kind": "move", "x": 0.5, "y": 0.5}]));
    h.settle(SETTLE);

    let scroll_trackpad = |h: &mut Headless, dy: f32| {
        h.events.push(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, dy),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
        });
        for _ in 0..10 {
            h.step();
        }
    };

    let (size0, zoom0) = (h.app.ui.brush_size, h.app.ui.zoom);
    scroll_trackpad(&mut h, 50.0);
    assert!(h.app.ui.brush_size > size0, "scrolling up on trackpad grows the brush: {size0} -> {}", h.app.ui.brush_size);
    assert_eq!(h.app.ui.zoom, zoom0, "and does not zoom");
    let size1 = h.app.ui.brush_size;
    scroll_trackpad(&mut h, -50.0);
    assert!(h.app.ui.brush_size < size1, "scrolling down shrinks it: {size1} -> {}", h.app.ui.brush_size);

    // Zoom in so panning is possible
    let r = h.app.run("view.navigate", json!({"zoom": {"percent": 200.0}, "pan": [0.5, 0.5]}));
    assert!(r.is_ok(), "{r:?}");
    for _ in 0..5 {
        h.step();
    }
    let pan0 = h.app.ui.pan;
    let size_before_space = h.app.ui.brush_size;

    // Holding Space bar + scroll moves the frame around (pan) instead of resizing the brush
    let r = h.request("ui.key", json!({"key": "Space", "action": "down"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    scroll_trackpad(&mut h, 50.0);
    assert_eq!(h.app.ui.brush_size, size_before_space, "holding Space does not resize the brush");
    assert_ne!(h.app.ui.pan, pan0, "holding Space moves the frame around (pans)");
    let r = h.request("ui.key", json!({"key": "Space", "action": "up"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();

    // With the preference off: trackpad scroll does not resize the brush
    h.app.ui.settings.wheel_resizes_brush = false;
    let size_off = h.app.ui.brush_size;
    scroll_trackpad(&mut h, 50.0);
    assert_eq!(h.app.ui.brush_size, size_off, "with preference off, trackpad scroll does not resize brush");
}

#[test]
fn trackpad_scroll_resizes_remove_brush() {
    let mut h = detail("panel.remove");
    h.settle(SETTLE);
    pointer(&mut h, json!([{"kind": "move", "x": 0.5, "y": 0.5}]));
    h.settle(SETTLE);

    let scroll_trackpad = |h: &mut Headless, dy: f32| {
        h.events.push(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, dy),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
        });
        for _ in 0..10 {
            h.step();
        }
    };

    let size0 = h.app.ui.remove_size;
    scroll_trackpad(&mut h, 50.0);
    assert!(h.app.ui.remove_size > size0, "trackpad scroll up grows remove brush: {size0} -> {}", h.app.ui.remove_size);
    let size1 = h.app.ui.remove_size;
    scroll_trackpad(&mut h, -50.0);
    assert!(h.app.ui.remove_size < size1, "trackpad scroll down shrinks remove brush: {size1} -> {}", h.app.ui.remove_size);
}

#[test]
fn mask_brush_toggles_and_disarms_with_tile_done_button_or_escape() {
    let mut h = detail("panel.masking");
    h.settle(SETTLE);

    // 1. Clicking the Brush tile arms it
    click(&mut h, "maskNew:brush");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.tool, "brush", "clicking Brush tile arms it");
    assert!(h.app.ui.mask_new.is_some(), "armed for a new mask");

    // 2. Clicking the active Brush tile again toggles it off
    click(&mut h, "maskNew:brush");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.tool, "", "clicking active Brush tile disarms it");
    assert!(h.app.ui.mask_new.is_none(), "mask_new cleared");

    // 3. Re-arm and paint a stroke to make a mask
    click(&mut h, "maskNew:brush");
    h.settle(SETTLE);
    pointer(&mut h, json!([{"kind": "down", "x": 0.5, "y": 0.5}, {"kind": "drag", "x": 0.55, "y": 0.55}, {"kind": "up", "x": 0.55, "y": 0.55}]));
    h.settle(SETTLE);
    assert_eq!(h.app.ui.tool, "brush", "brush active on the new mask");
    assert!(!develop(&h).masks.is_empty(), "mask was created");

    // 4. Clicking the 'Done' button in the brush header disarms the brush
    click(&mut h, "button:brushDone");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.tool, "", "clicking Done disarms the brush");
    assert_eq!(h.app.ui.right, crate::state::RightPanel::Masking, "stays in Masking panel");

    // 5. Re-arming via shortcut/command, then pressing Escape disarms it
    exec(&mut h, "tool.brush", json!({}));
    h.settle(SETTLE);
    assert_eq!(h.app.ui.tool, "brush", "re-armed brush");
    let r = h.request("ui.key", json!({"key": "Escape"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.tool, "", "Escape disarms the active brush");
    assert_eq!(h.app.ui.right, crate::state::RightPanel::Masking, "stays in Masking panel on first Escape");
}

#[test]
fn mask_object_toggles_and_disarms_with_tile_done_button_or_escape() {
    let mut h = detail("panel.masking");
    h.settle(SETTLE);

    // 1. Clicking Object tile arms it
    click(&mut h, "maskNew:object");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.tool, "object", "clicking Object tile arms it");

    // 2. Clicking active Object tile toggles it off
    click(&mut h, "maskNew:object");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.tool, "", "clicking active Object tile disarms it");

    // 3. Re-arm via command, verify Done button disarms it
    exec(&mut h, "tool.object", json!({}));
    h.settle(SETTLE);
    assert_eq!(h.app.ui.tool, "object", "armed object");
    click(&mut h, "button:objectDone");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.tool, "", "clicking Done disarms object tool");

    // 4. Re-arm and verify Escape disarms it
    exec(&mut h, "tool.object", json!({}));
    h.settle(SETTLE);
    assert_eq!(h.app.ui.tool, "object", "armed object");
    let r = h.request("ui.key", json!({"key": "Escape"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.tool, "", "Escape disarms object tool");
    assert_eq!(h.app.ui.right, crate::state::RightPanel::Masking, "stays in Masking panel");

    // 5. Each tool command, invoked with `toggle` while its tool is active, puts the tool away
    for tool in ["brush", "object", "linear", "radial"] {
        let command = format!("tool.{tool}");
        exec(&mut h, &command, json!({}));
        assert_eq!(h.app.ui.tool, tool);
        exec(&mut h, &command, json!({"toggle": true}));
        assert_eq!(h.app.ui.tool, "", "{command} with toggle toggles off");
    }
}

fn widgets_contain(h: &mut Headless, id: &str) -> bool {
    h.request("ui.widgets", json!({}), T).to_string().contains(&format!("\"{id}\""))
}

/// The mask's Point Color flyout: the eyedropper samples into the active mask (not the photo), the
/// swatch appears, and Delete removes it.
#[test]
fn mask_point_color_eyedropper_samples_into_the_mask() {
    let mut h = detail("panel.masking");
    exec(&mut h, "mask.add", json!({"kind": "linear"}));
    h.app.ui.toggle_flyout("maskPointColor");
    h.settle(SETTLE);
    assert!(widgets_contain(&mut h, "icon:maskPointColorPicker"), "the flyout shows the eyedropper");
    // (the eyedropper's button sets this tool)
    h.app.ui.tool = "maskPointColor".into();
    h.step();
    brush_at(&mut h, 0.5, 0.1);
    let d = develop(&h);
    assert_eq!(d.masks[0].point_colors.len(), 1, "sampled into the mask");
    assert!(d.point_colors.is_empty(), "not into the photo's Point Color");
    assert!(h.app.ui.tool.is_empty(), "one sample per pick");
    h.settle(SETTLE);
    assert!(widgets_contain(&mut h, "maskPointColor:0"), "the sample's swatch shows");
    exec(&mut h, "mask.pointColorDelete", json!({"index": 0}));
    assert!(develop(&h).masks[0].point_colors.is_empty());
    h.settle(SETTLE);
}

/// `ui.clickWidget` on the Point Color flyout's eyedropper arms the tool, as a real click does.
#[test]
fn mask_point_color_eyedropper_answers_click_widget() {
    let mut h = detail("panel.masking");
    exec(&mut h, "mask.add", json!({"kind": "linear"}));
    h.app.ui.toggle_flyout("maskPointColor");
    h.settle(SETTLE);
    // laid out below the window's edge here: refused with a reason, not a silent ok
    let r = h.request("ui.clickWidget", json!({"id": "icon:maskPointColorPicker"}), T);
    if r["ok"] == false {
        assert!(r.to_string().contains("outside the window"), "{r}");
        // scroll the panel until the eyedropper is in view, then click it
        let flyout = h.request("ui.widgets", json!({"filter": "flyout:maskPointColor"}), T);
        let x = flyout["result"][0]["rect"][0].as_f64().unwrap_or(1000.0) + 20.0;
        for _ in 0..20 {
            h.request("ui.move", json!({"x": x, "y": 400.0}), T);
            h.request("ui.scroll", json!({"dy": -200.0}), T);
            h.settle(SETTLE);
            let y = h.request("ui.widgets", json!({"filter": "icon:maskPointColorPicker"}), T)["result"][0]["rect"][1].as_f64().unwrap_or(f64::MAX);
            if y < 600.0 {
                break;
            }
        }
        h.request("ui.clickWidget", json!({"id": "icon:maskPointColorPicker"}), T);
        {}
    }
    h.settle(SETTLE);
    let rect = h.request("ui.widgets", json!({"filter": "maskPointColor"}), T);
    assert_eq!(h.app.ui.tool, "maskPointColor", "click answered {r}; widgets {rect}");
}

#[test]
fn a_selected_people_component_offers_who_it_is_about() {
    let mut h = detail("panel.masking");
    exec(&mut h, "mask.add", json!({"kind": "sky"}));
    exec(&mut h, "mask.update", json!({"component": 0, "shape": {"kind": "people", "person": 0, "parts": ["Clothes"]}}));
    let photo = h.app.session.active().expect("photo").0;
    exec(&mut h, "people.setRole", json!({"role": "Groom", "person": "Alex", "scope": {"ids": [photo]}}));
    h.settle(SETTLE);
    assert!(!widgets_contain(&mut h, "maskWho:0"), "Who picker is hidden by default");
    click(&mut h, "button:componentMenu:0");
    let r = h.request("ui.clickWidget", json!({"id": "maskMenu:toggleWho"}), T);
    assert_eq!(r["ok"], true, "component menu toggles Show Who & Roles: {r}");
    h.settle(SETTLE);
    let r = h.request("ui.clickWidget", json!({"id": "maskWho:0"}), T);
    assert_eq!(r["ok"], true, "the Who picker shows under a selected People component once enabled: {r}");
    // choosing a role goes into the recipe and is resolved here (no Groom face: everyone)
    exec(&mut h, "mask.update", json!({"component": 0, "shape": {"kind": "people", "person": 0, "parts": ["Clothes"], "who": {"role": "Groom"}}}));
    let done = exec(&mut h, "masks.recompute", json!({}));
    assert!(done["photos"][0]["notes"].as_array().is_some_and(|n| n.iter().any(|t| t.as_str().is_some_and(|t| t.contains("Alex")))), "{done}");
    assert!(matches!(develop(&h).masks[0].components[0].shape, MaskShape::People { person: 0, who: Some(_), .. }));
    // Set Role: a role for this folder, and the component is about it
    click(&mut h, "button:maskSetRole");
    h.app.ui.role_draft = Some(("Bride".into(), "Sam".into()));
    h.step();
    h.step();
    click(&mut h, "button:maskRoleSet");
    let roles = exec(&mut h, "people.roles", json!({}));
    assert!(roles["here"].as_array().is_some_and(|r| r.iter().any(|b| b["role"] == "Bride" && b["person"] == "Sam")), "{roles}");
    assert!(
        matches!(&develop(&h).masks[0].components[0].shape, MaskShape::People { who: Some(lcc_develop::recipe::Who::Role(r)), .. } if r == "Bride")
    );
}

/// The mask's Color Mixer flyout: a slider per colour of the shown group, the group tabs, and the
/// targeted adjustment, which edits the mask's mixer (not the photo's) on a vertical drag.
#[test]
fn mask_color_mixer_flyout_edits_the_masks_mixer() {
    let mut h = detail("panel.masking");
    exec(&mut h, "mask.add", json!({"kind": "linear"}));
    h.app.ui.toggle_flyout("maskMixer");
    h.settle(SETTLE);
    assert!(widgets_contain(&mut h, "slider:maskMixer.red.hue"), "the Hue group's sliders show");
    assert!(widgets_contain(&mut h, "icon:maskMixerTargeted"));
    h.app.ui.mask_mixer_group = "sat".into();
    h.settle(SETTLE);
    assert!(widgets_contain(&mut h, "slider:maskMixer.blue.sat"), "the Saturation group's sliders show");
    // (the targeted button sets this tool)
    h.app.ui.tool = "maskMixer:sat".into();
    h.step();
    pointer(
        &mut h,
        json!([{"kind": "down", "x": 0.5, "y": 0.2}, {"kind": "move", "x": 0.5, "y": 0.15}, {"kind": "move", "x": 0.5, "y": 0.1}, {"kind": "up", "x": 0.5, "y": 0.1}]),
    );
    let d = develop(&h);
    assert!(!d.masks[0].mixer.is_neutral(), "the drag moved the mask's mixer");
    assert!(d.mixer.is_neutral(), "not the photo's");
    assert!(d.masks[0].mixer.bands().iter().all(|b| b.hue == 0.0 && b.lum == 0.0), "only Saturation moved");
    h.settle(SETTLE);
    assert!(widgets_contain(&mut h, "button:maskMixerReset"), "Reset shows once something is set");
}

/// A stale AI mask (its selection not computed for this photo) shows a badge on its row; the mask
/// list's menu and the photo's menu offer Update AI Mask.
#[test]
fn a_stale_ai_mask_shows_a_badge_and_offers_update() {
    let mut h = detail("panel.masking");
    let id = h.app.session.active().unwrap();
    let mut d = develop(&h);
    d.masks.push(lcc_develop::Mask {
        id: 5,
        components: vec![lcc_develop::MaskComponent {
            anchor: None,
            name: None,
            op: lcc_develop::MaskOp::Add,
            invert: false,
            shape: lcc_develop::MaskShape::Prompt { text: "the sky".into(), seg: None, detail: vec![], edge: 0.0 },
        }],
        ..Default::default()
    });
    h.app.session.set_develop(id, d, "paste").unwrap();
    exec(&mut h, "mask.select", json!({"id": 5}));
    h.settle(SETTLE);
    assert!(widgets_contain(&mut h, "maskStale:5"), "the badge shows");
    // a mask with its selection: no badge
    let seg = serde_json::to_value(lcc_develop::SegMask::from_logits(4, &[5.0; 16])).unwrap();
    exec(&mut h, "mask.add", json!({"kind": "prompt", "text": "sky", "seg": seg}));
    h.settle(SETTLE);
    let m = develop(&h).masks.last().unwrap().id;
    assert!(!widgets_contain(&mut h, &format!("maskStale:{m}")));
    // the photo's right-click menu offers Update AI Mask for the selected AI mask
    exec(&mut h, "mask.select", json!({"id": 5}));
    h.settle(SETTLE);
    let canvas = h.app.canvas_rect.expect("the photo is on screen");
    let corner = canvas.left_top() + egui::vec2(30.0, 30.0);
    h.request("ui.click", json!({"x": corner.x, "y": corner.y, "button": "right"}), T);
    h.settle(SETTLE);
    assert!(widgets_contain(&mut h, "photoMenu:updateAiMask"), "{}", h.request("ui.widgets", json!({"filter": "Menu"}), T));
    h.request("ui.key", json!({"key": "Escape"}), T);
    h.settle(SETTLE);
    // a right-click on a mask's pin (the Describe mask's, at the centre) opens that mask's menu
    let c = canvas.center();
    h.request("ui.click", json!({"x": c.x, "y": c.y, "button": "right"}), T);
    h.settle(SETTLE);
    assert!(widgets_contain(&mut h, &format!("maskMenu:update:{m}")), "{}", h.request("ui.widgets", json!({"filter": "Menu"}), T));
}

/// The mask rows show thumbnails once the photo's small rendition is ready, and the Roles window
/// lists who has which role and clears one.
#[test]
fn mask_rows_show_thumbnails_and_the_roles_window_clears_a_role() {
    let mut h = detail("panel.masking");
    exec(&mut h, "mask.add", json!({"kind": "radial"}));
    let m = develop(&h).masks[0].id;
    let mut shown = false;
    for _ in 0..100 {
        h.settle(SETTLE);
        if widgets_contain(&mut h, &format!("maskThumb:{m}")) {
            shown = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(shown, "the mask's thumbnail shows");
    assert!(widgets_contain(&mut h, "componentThumb:0"), "and its component's");
    let mask_thumb = h.app.widgets.iter().find(|(w, _)| w == &format!("maskThumb:{m}")).map(|(_, r)| *r).unwrap();
    let comp_thumb = h.app.widgets.iter().find(|(w, _)| w == "componentThumb:0").map(|(_, r)| *r).unwrap();
    assert!(mask_thumb.width() >= 30.0 && mask_thumb.height() >= 30.0, "mask preview is large enough to read: {mask_thumb:?}");
    assert!(comp_thumb.width() >= 26.0 && comp_thumb.height() >= 26.0, "component preview is large enough to read: {comp_thumb:?}");
    // the Roles window
    let photo = h.app.session.active().expect("photo").0;
    exec(&mut h, "people.setRole", json!({"role": "Bride", "person": "Sam", "scope": {"ids": [photo]}}));
    h.app.ui.roles_window = true;
    exec(&mut h, "mask.update", json!({"component": 0, "shape": {"kind": "people", "person": 0, "parts": ["Clothes"]}}));
    h.settle(SETTLE);
    assert!(widgets_contain(&mut h, "window:roles"), "the window shows");
    click(&mut h, "button:roleClear:0");
    h.settle(SETTLE);
    let roles = exec(&mut h, "people.roles", json!({}));
    assert!(roles["roles"].as_array().is_some_and(Vec::is_empty), "{roles}");
}

/// Picking another mask or another photo while a Brush stroke is held ends the stroke: its release
/// can't add to the picked mask or start a mask on the new photo, and the pointer still held paints
/// nothing more until it is let go (ported from OneSlips 76d00a470, NOTES-1031-7).
#[test]
fn picking_another_mask_or_photo_mid_stroke_ends_the_stroke() {
    let strokes = |h: &Headless, id: u32| -> usize {
        develop(h)
            .masks
            .iter()
            .find(|m| m.id == id)
            .map(|m| m.components.iter().map(|c| if let MaskShape::Brush { strokes } = &c.shape { strokes.len() } else { 0 }).sum())
            .unwrap_or(0)
    };
    for pick in ["mask", "photo"] {
        let mut h = detail("panel.masking");
        h.settle(SETTLE);
        exec(&mut h, "tool.brush", json!({"new": true}));
        h.app.ui.brush_auto_mask = false;
        brush_at(&mut h, 0.2, 0.5);
        let first = h.app.session.active_mask.expect("the first mask");
        exec(&mut h, "tool.brush", json!({"new": true}));
        brush_at(&mut h, 0.6, 0.5);
        let second = h.app.session.active_mask.expect("the second mask");
        assert_ne!(first, second);
        pointer(&mut h, json!([{"kind": "down", "x": 0.6, "y": 0.7}, {"kind": "drag", "x": 0.65, "y": 0.7}, {"kind": "drag", "x": 0.7, "y": 0.7}]));
        h.step();
        assert!(matches!(h.app.gesture, Some(crate::panels::detail::Gesture::Brush { .. })), "{pick}: a stroke is held");
        let photo = h.app.session.active().expect("a photo");
        let (first_strokes, masks) = (strokes(&h, first), develop(&h).masks.len());
        if pick == "mask" {
            exec(&mut h, "mask.select", json!({"id": first}));
        } else {
            let other = h.app.session.visible_cloned().into_iter().find(|p| *p != photo).expect("another photo");
            exec(&mut h, "library.select", json!({"ids": [other.0]}));
        }
        assert!(h.app.gesture.is_none(), "{pick}: the stroke ended");
        pointer(&mut h, json!([{"kind": "drag", "x": 0.75, "y": 0.7}, {"kind": "drag", "x": 0.8, "y": 0.7}, {"kind": "up", "x": 0.8, "y": 0.7}]));
        h.step();
        h.step();
        if pick == "mask" {
            assert_eq!(strokes(&h, first), first_strokes, "the picked mask's strokes are untouched");
            assert_eq!(develop(&h).masks.len(), masks, "no new mask");
        } else {
            assert!(develop(&h).masks.is_empty(), "no mask started on the new photo");
        }
        h.settle(SETTLE);
    }
}

/// Leaving Masking puts its tools away: back in Masking no brush is armed (it hid the overlay and
/// made pin drags paint). Colour Range's eyedropper, used from Edit too, is left alone.
#[test]
fn leaving_masking_puts_the_brush_away() {
    let mut h = detail("panel.masking");
    exec(&mut h, "tool.brush", json!({"new": true}));
    h.step();
    assert_eq!(h.app.ui.tool, "brush");
    exec(&mut h, "panel.edit", json!({}));
    h.step();
    assert_eq!(h.app.ui.tool, "", "the brush is put away");
    exec(&mut h, "panel.masking", json!({}));
    h.step();
    assert_eq!(h.app.ui.tool, "", "and not back");
    // nor in a grid
    exec(&mut h, "tool.brush", json!({}));
    exec(&mut h, "view.gridToggle", json!({}));
    h.step();
    assert_eq!(h.app.ui.tool, "");
}

/// One masking mode at a time: a tool armed for a new mask selects none and makes its mask on the
/// first stroke or drag; selecting a mask, or another photo, puts the armed tool away.
#[test]
fn tools_arm_and_make_their_mask_on_the_first_stroke() {
    let mut h = detail("panel.masking");
    exec(&mut h, "mask.add", json!({"kind": "radial", "center": [0.2, 0.2], "rx": 0.05, "ry": 0.05}));
    let first = h.app.session.active_mask.expect("a mask");
    // Linear: armed, nothing made, then a drag draws it from the press to the release (one undo step)
    exec(&mut h, "tool.linear", json!({}));
    h.step();
    assert_eq!((h.app.session.active_mask, develop(&h).masks.len()), (None, 1), "armed: no mask selected, none made");
    pointer(
        &mut h,
        json!([{"kind": "down", "x": 0.5, "y": 0.3}, {"kind": "drag", "x": 0.5, "y": 0.4}, {"kind": "drag", "x": 0.5, "y": 0.6}, {"kind": "up", "x": 0.5, "y": 0.6}]),
    );
    let d = develop(&h);
    assert_eq!(d.masks.len(), 2);
    let MaskShape::Linear { start, end } = d.masks[1].components[0].shape.clone() else { panic!("linear") };
    assert!(end.y - start.y > 0.15, "drawn by the drag: {start:?} → {end:?}");
    exec(&mut h, "edit.undo", json!({}));
    assert_eq!(develop(&h).masks.len(), 1, "one undo step takes the drawn gradient back");
    // Radial: a click places one there
    exec(&mut h, "tool.radial", json!({}));
    brush_at(&mut h, 0.7, 0.6);
    let MaskShape::Radial { center, .. } = develop(&h).masks[1].components[0].shape.clone() else { panic!("radial") };
    assert!((center.x - 0.7).abs() < 0.05 && (center.y - 0.6).abs() < 0.05, "{center:?}");
    // selecting a mask while a tool waits for a new one puts the tool away
    exec(&mut h, "tool.brush", json!({"new": true}));
    h.step();
    exec(&mut h, "mask.select", json!({"id": first}));
    h.step();
    assert_eq!((h.app.ui.tool.as_str(), h.app.ui.mask_new.is_none()), ("", true), "the selected mask wins");
    // another photo puts an armed tool away
    exec(&mut h, "tool.brush", json!({"new": true}));
    h.step();
    let photo = h.app.session.active().expect("a photo");
    let other = h.app.session.visible_cloned().into_iter().find(|p| *p != photo).expect("another photo");
    exec(&mut h, "library.select", json!({"ids": [other.0]}));
    h.step();
    assert_eq!(h.app.ui.tool, "", "another photo disarms");
    h.settle(SETTLE);
}

/// `mask.editComponent` makes one component the one edited on the photo: a Brush component arms the
/// brush on it, so strokes go into it (not the mask's last Brush); bad arguments are errors.
#[test]
fn edit_component_arms_the_brush_on_that_component() {
    let mut h = detail("panel.masking");
    exec(&mut h, "tool.brush", json!({"new": true}));
    brush_at(&mut h, 0.3, 0.3);
    exec(&mut h, "mask.addComponent", json!({"kind": "linear", "op": "add", "start": [0.1, 0.5], "end": [0.9, 0.5]}));
    exec(&mut h, "mask.addComponent", json!({"kind": "brush", "op": "subtract"}));
    let mid = h.app.session.active_mask.expect("a mask");
    let r = exec(&mut h, "mask.editComponent", json!({"id": mid, "component": 0}));
    assert_eq!(r["brush"], true, "{r}");
    assert_eq!((h.app.ui.tool.as_str(), h.app.ui.active_mask_component), ("brush", Some(0)));
    brush_at(&mut h, 0.6, 0.6);
    let d = develop(&h);
    let MaskShape::Brush { strokes } = &d.masks[0].components[0].shape else { panic!("brush") };
    assert_eq!(strokes.len(), 2, "into the chosen component");
    let r = exec(&mut h, "mask.editComponent", json!({"id": mid, "component": 1}));
    assert_eq!((r["brush"].clone(), h.app.ui.tool.as_str()), (json!(false), "linear"), "the gradient's handles take the drags");
    for bad in [json!({"id": mid, "component": 9}), json!({"id": 999, "component": 0}), json!({"id": mid}), json!({"id": mid, "component": "x"})] {
        let r = h.request("engine.execute", json!({"command": "mask.editComponent", "params": bad}), T);
        assert_eq!(r["ok"], false, "{bad}: {r}");
    }
    h.settle(SETTLE);
}

/// Painting a mask that has settings, with Auto Mask and the overlay off, sends the stroke live: its
/// effect shows while the button is still held, and the whole stroke is one undo step.
#[test]
fn a_stroke_on_a_mask_with_settings_shows_live() {
    let mut h = detail("panel.masking");
    exec(&mut h, "tool.brush", json!({"new": true}));
    h.app.ui.brush_auto_mask = false;
    brush_at(&mut h, 0.2, 0.2);
    exec(&mut h, "mask.adjust", json!({"values": {"exposure": 1.0}}));
    h.app.ui.mask_overlay = false;
    h.app.ui.mask_overlay_pinned = false;
    let strokes = |h: &Headless| match &develop(h).masks[0].components[0].shape {
        MaskShape::Brush { strokes } => strokes.len(),
        _ => 0,
    };
    let before = strokes(&h);
    pointer(&mut h, json!([{"kind": "down", "x": 0.4, "y": 0.5}, {"kind": "drag", "x": 0.5, "y": 0.5}, {"kind": "drag", "x": 0.6, "y": 0.5}]));
    h.step();
    assert_eq!(strokes(&h), before + 1, "the stroke is on the mask before the button is let go");
    pointer(&mut h, json!([{"kind": "drag", "x": 0.7, "y": 0.5}, {"kind": "up", "x": 0.7, "y": 0.5}]));
    assert_eq!(strokes(&h), before + 1, "still one stroke");
    exec(&mut h, "edit.undo", json!({}));
    assert_eq!(strokes(&h), before, "one undo step");
    h.settle(SETTLE);
}

/// The eraser keeps its own Size, Feather, Flow and Density: Erase (or ⌥ held) shows and changes
/// them, the brush's stay as they were; `[` / `]` resize the set in use.
#[test]
fn the_eraser_keeps_its_own_settings() {
    let mut h = detail("panel.masking");
    exec(&mut h, "tool.brush", json!({"new": true}));
    h.app.ui.brush_size = 20.0;
    h.app.ui.erase_brush.size = 40.0;
    h.step();
    let size = |h: &Headless| h.app.ui.brush_set(h.app.ui.brush_erasing()).size;
    assert_eq!(size(&h), 20.0);
    click(&mut h, "button:brushErase");
    assert!(h.app.ui.brush_erasing());
    assert_eq!(size(&h), 40.0, "the eraser's own size");
    exec(&mut h, "brush.larger", json!({}));
    assert!((h.app.ui.erase_brush.size - 48.0).abs() < 1e-3, "] grows the eraser: {:?}", h.app.ui.erase_brush);
    assert_eq!(h.app.ui.brush_size, 20.0, "and leaves the brush");
    click(&mut h, "button:brushAdd");
    assert_eq!(size(&h), 20.0);
}

/// Size is a size on the screen: a stroke stores the core's screen radius over the photo's screen
/// size, and Sizes saved as photo fractions by older builds (settings, A / B, the eraser) migrate to
/// the Size that paints the same at Fit.
#[test]
fn brush_size_is_a_screen_size_and_old_sizes_migrate() {
    let mut h = detail("panel.masking");
    exec(&mut h, "tool.brush", json!({"new": true}));
    h.app.ui.brush_size = 20.0;
    h.app.ui.brush_auto_mask = false;
    brush_at(&mut h, 0.3, 0.3);
    let size_of_last = |h: &Headless| match &develop(h).masks[0].components[0].shape {
        MaskShape::Brush { strokes } => strokes.last().map(|s| s.size).unwrap_or(0.0),
        _ => 0.0,
    };
    let at_fit = size_of_last(&h);
    assert!(at_fit > 0.0);
    let core = f64::from(crate::state::brush_core_points(20.0));
    let img = h.app.image_rect.expect("the photo on screen");
    let long = f64::from(img.width().max(img.height()));
    assert!((at_fit - core / long).abs() < 1e-3, "the core's screen radius over the photo's screen size: {at_fit} vs {}", core / long);
    // a settings file and presets from before
    let mut old = crate::UiState { brush_size: 0.04, ..Default::default() };
    old.brush_presets[1].size = 0.012;
    old.erase_brush.size = 0.04;
    old.clamp_ranges();
    assert!((14.0..=16.5).contains(&old.brush_size), "{}", old.brush_size);
    assert!((4.0..=6.0).contains(&old.brush_presets[1].size), "{}", old.brush_presets[1].size);
    assert_eq!(old.erase_brush.size, old.brush_size);
    for bad in [f32::NAN, -3.0, 0.0, 1e9] {
        old.brush_size = bad;
        old.clamp_ranges();
        assert!((1.0..=100.0).contains(&old.brush_size), "{bad} → {}", old.brush_size);
    }
    h.settle(SETTLE);
}

/// The brush's centre says what a stroke does: a cross while it paints, a minus while it erases
/// (Erase picked or ⌥ held); the Object tool shows a minus while ⌥ leaves things out.
#[test]
fn brush_and_object_cursors_show_erasing() {
    let mut h = detail("panel.masking");
    exec(&mut h, "tool.brush", json!({"new": true}));
    let has = |h: &Headless, w: &str| h.app.widgets.iter().any(|(id, _)| id == w);
    pointer(&mut h, json!([{"kind": "move", "x": 0.5, "y": 0.5}]));
    h.step();
    assert!(has(&h, "brushCursor:paint") && !has(&h, "brushCursor:erase"));
    let r = h.request("ui.pointer", json!({"events": [{"kind": "down", "x": 0.52, "y": 0.5}], "alt": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(has(&h, "brushCursor:erase"), "⌥ held: the stroke would erase");
    pointer(&mut h, json!([{"kind": "up", "x": 0.52, "y": 0.5}]));
    h.app.ui.brush_erase = true;
    pointer(&mut h, json!([{"kind": "move", "x": 0.5, "y": 0.5}]));
    h.step();
    assert!(has(&h, "brushCursor:erase"), "Erase picked");
    h.app.ui.tool = "object".into();
    let r = h.request("ui.pointer", json!({"events": [{"kind": "down", "x": 0.5, "y": 0.5}], "alt": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(has(&h, "objectCursor:exclude"));
}

/// Clicking a mask's row edits its top component: a Brush mask arms the brush on it (a stroke goes
/// into that mask, no new one), a gradient's row shows its handles.
#[test]
fn the_mask_row_activates_its_top_component() {
    let mut h = detail("panel.masking");
    exec(&mut h, "mask.add", json!({"kind": "brush"}));
    let brush = h.app.session.active_mask.expect("the brush mask");
    exec(&mut h, "mask.brushStroke", json!({"points": [[0.3, 0.3], [0.4, 0.3]], "size": 0.02}));
    exec(&mut h, "mask.add", json!({"kind": "linear", "start": [0.5, 0.3], "end": [0.5, 0.6]}));
    let lin = h.app.session.active_mask.expect("the gradient");
    h.step();
    click(&mut h, &format!("mask:{brush}"));
    h.step();
    assert_eq!(h.app.session.active_mask, Some(brush));
    assert_eq!(h.app.ui.tool, "brush", "the row arms the brush on the mask's Brush");
    assert!(h.app.ui.mask_new.is_none(), "not for a new mask");
    pointer(
        &mut h,
        json!([{"kind": "down", "x": 0.7, "y": 0.75}, {"kind": "drag", "x": 0.75, "y": 0.75}, {"kind": "drag", "x": 0.8, "y": 0.75}, {"kind": "up", "x": 0.8, "y": 0.75}]),
    );
    let d = develop(&h);
    assert_eq!(d.masks.len(), 2, "no new mask");
    let n = d.masks.iter().find(|m| m.id == brush).map(|m| match &m.components[0].shape {
        MaskShape::Brush { strokes } => strokes.len(),
        _ => 0,
    });
    assert_eq!(n, Some(2), "the stroke went into the selected brush mask");
    click(&mut h, &format!("mask:{lin}"));
    h.step();
    assert_eq!(h.app.session.active_mask, Some(lin));
    assert_eq!(h.app.ui.active_mask_component, Some(0));
    assert_ne!(h.app.ui.tool, "brush", "a gradient on top arms no brush");
}

/// With no mask selected the panel shows the settings for the next mask: what is dialed in there
/// goes to the next new mask, whatever its tool.
#[test]
fn the_panel_sets_the_next_masks_settings_with_no_mask_selected() {
    let mut h = detail("panel.masking");
    h.step();
    assert!(h.app.widgets.iter().any(|(id, _)| id == "maskNextSettings"), "the next mask's settings show");
    click(&mut h, "maskResetNew");
    assert!(h.app.session.mask_reset_new, "Reset settings on new mask turns on");
    click(&mut h, "maskResetNew");
    exec(&mut h, "mask.adjust", json!({"values": {"exposure": 0.6}}));
    exec(&mut h, "mask.add", json!({"kind": "radial"}));
    assert_eq!(develop(&h).masks[0].adjust.exposure, 0.6);
}

/// The pointer on a mask's own row shows no overlay (that row is where its effect is judged with
/// the eye); on one of its component rows it shows the whole mask's.
#[test]
fn a_masks_own_row_shows_no_overlay_a_component_row_does() {
    let mut h = detail("panel.masking");
    exec(&mut h, "mask.add", json!({"kind": "radial", "center": [0.5, 0.5]}));
    exec(&mut h, "mask.adjust", json!({"values": {"exposure": 1.0}}));
    let mid = h.app.session.active_mask.expect("the mask");
    h.step();
    let at = |h: &Headless, id: &str| h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| r.center()).unwrap_or_else(|| panic!("{id}"));
    let row = at(&h, &format!("mask:{mid}"));
    let r = h.request("ui.move", json!({"x": row.x, "y": row.y}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert_eq!(h.app.ui.mask_hover, None, "the mask's own row: no overlay");
    let comp = at(&h, "component:0");
    let r = h.request("ui.move", json!({"x": comp.x, "y": comp.y}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert_eq!(h.app.ui.mask_hover, Some(mid), "a component row shows the whole mask");
}

/// The selected mask follows its linked copy to another photo: the component edited there stays
/// the one edited, and a brush armed on it stays armed; a photo without the copy has none.
#[test]
fn the_edited_component_and_armed_brush_carry_to_the_linked_copy() {
    let mut h = detail("panel.masking");
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(3).map(|p| p.0).collect();
    let (a, b, e) = (ids[0], ids[1], ids[2]);
    exec(&mut h, "library.select", json!({"ids": [b]}));
    exec(&mut h, "mask.add", json!({"kind": "radial"}));
    exec(&mut h, "library.select", json!({"ids": [a, b], "active": a}));
    exec(&mut h, "develop.autoSync", json!({"on": true}));
    exec(&mut h, "mask.add", json!({"kind": "linear", "start": [0.5, 0.3], "end": [0.5, 0.6]}));
    exec(&mut h, "mask.addComponent", json!({"kind": "brush"}));
    exec(&mut h, "develop.autoSync", json!({"on": false}));
    let on_a = h.app.session.active_mask.expect("selected");
    let link = develop(&h).masks.iter().find(|m| m.id == on_a).map(|m| m.link.clone()).expect("mask");
    assert!(!link.is_empty());
    exec(&mut h, "mask.editComponent", json!({"id": on_a, "component": 1}));
    assert_eq!((h.app.ui.tool.as_str(), h.app.ui.active_mask_component), ("brush", Some(1)));
    exec(&mut h, "library.select", json!({"ids": [b]}));
    h.step();
    let on_b = develop(&h).masks.iter().find(|m| m.link == link).map(|m| m.id).expect("b's copy");
    assert_eq!(h.app.session.active_mask, Some(on_b));
    assert_eq!((h.app.ui.tool.as_str(), h.app.ui.active_mask_component), ("brush", Some(1)), "the brush stays armed on the copy's Brush");
    exec(&mut h, "library.select", json!({"ids": [e]}));
    h.step();
    assert_eq!(h.app.session.active_mask, None);
    assert_ne!(h.app.ui.tool.as_str(), "brush", "no copy: the brush is put away");
    h.settle(SETTLE);
}

/// "Reset settings on new mask" is saved with the UI settings: a saved choice reaches the engine,
/// and a `mask.next` from a command comes back to be saved.
#[test]
fn reset_settings_on_new_mask_is_saved_with_the_ui_settings() {
    let mut h = detail("panel.masking");
    exec(&mut h, "mask.next", json!({"resetNew": true}));
    h.step();
    assert!(h.app.ui.mask_reset_new, "a command's choice is saved");
    let saved = serde_json::to_string(&h.app.ui).unwrap();
    let mut back: crate::UiState = serde_json::from_str(&saved).unwrap();
    assert!(back.mask_reset_new);
    back.mask_reset_new = true;
    let mut h2 = detail("panel.masking");
    h2.app.ui = back;
    h2.step();
    assert!(h2.app.session.mask_reset_new, "the saved choice reaches the engine");
    h.settle(SETTLE);
}

/// Create New Mask ▸ People opens its picker under the tiles: a person (counted from the left
/// without the face model) and parts make a People mask; Remembered opens the remembered objects.
#[test]
fn people_and_remembered_tiles_open_their_pickers() {
    let mut h = detail("panel.masking");
    click(&mut h, "maskNew:people");
    assert!(h.app.ui.people_pick.is_some(), "the People picker is open");
    h.settle(SETTLE);
    let mut prev: Option<egui::Rect> = None;
    for part in crate::panels::mask_pickers::PEOPLE_PARTS {
        let id = format!("maskPeople:part:{part}");
        let r = h.app.widgets.iter().find(|(w, _)| w == &id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no {id}"));
        if let Some(p) = prev {
            assert!(r.top() >= p.bottom(), "{part} should be below previous part: {r:?} vs {p:?}");
            assert!((r.left() - p.left()).abs() < 1.0, "{part} should align vertically: {r:?} vs {p:?}");
        }
        prev = Some(r);
    }
    h.app.ui.people_pick = Some((2, vec!["Clothes".into()]));
    click(&mut h, "button:maskPeople:create");
    assert!(h.app.ui.people_pick.is_none(), "closed after Create Mask");
    let d = develop(&h);
    let shape = d.masks.last().map(|m| m.components[0].shape.clone());
    assert!(matches!(&shape, Some(MaskShape::People { person: 2, parts, .. }) if parts == &["Clothes".to_string()]), "{shape:?}");
    click(&mut h, "maskNew:concept");
    assert!(h.app.ui.concept_pick && h.app.ui.people_pick.is_none(), "one picker at a time");
    click(&mut h, "button:maskConcept:cancel");
    assert!(!h.app.ui.concept_pick);
}

/// OneSlips' overlay rule: a brush stroke on a mask with settings counts as adjusting, so no
/// overlay covers the photo while it is painted, not even one switched on with O.
#[test]
fn no_mask_overlay_covers_the_photo_while_a_brush_stroke_is_painted() {
    use lcc_pipeline::Overlay;
    let mut h = detail("panel.masking");
    h.settle(SETTLE);
    exec(&mut h, "tool.brush", json!({"new": true}));
    pointer(
        &mut h,
        json!([{"kind": "down", "x": 0.3, "y": 0.4}, {"kind": "drag", "x": 0.32, "y": 0.4}, {"kind": "drag", "x": 0.4, "y": 0.4}, {"kind": "up", "x": 0.4, "y": 0.4}]),
    );
    h.settle(SETTLE);
    assert!(!develop(&h).masks.is_empty(), "the stroke made a mask");
    exec(&mut h, "mask.adjust", json!({"values": {"exposure": 0.6}}));
    h.app.ui.mask_adjusting_until = None;
    exec(&mut h, "view.maskOverlay", json!({"show": true}));
    exec(&mut h, "tool.brush", json!({}));
    let overlay = |h: &Headless| crate::panels::detail::view_overlay(&h.app, &develop(h), false);
    assert!(matches!(overlay(&h), Overlay::Mask { .. }), "O shows the overlay at rest");
    pointer(&mut h, json!([{"kind": "down", "x": 0.3, "y": 0.6}, {"kind": "drag", "x": 0.32, "y": 0.6}, {"kind": "drag", "x": 0.45, "y": 0.6}]));
    assert!(matches!(h.app.gesture, Some(crate::panels::detail::Gesture::Brush { .. })), "still painting");
    assert_eq!(overlay(&h), Overlay::None, "nothing covers the photo while brushing");
    pointer(&mut h, json!([{"kind": "up", "x": 0.45, "y": 0.6}]));
    h.app.ui.mask_adjusting_until = None;
    assert!(matches!(overlay(&h), Overlay::Mask { .. }), "the overlay O switched on comes back after the stroke");
}

/// O turns off whatever overlay shows, All Mask Overlays (Ctrl+Shift+O) included, and a pill on the
/// photo says while All shows.
#[test]
fn o_turns_off_the_all_masks_overlay() {
    use lcc_pipeline::Overlay;
    let mut h = detail("panel.masking");
    exec(&mut h, "mask.add", json!({"kind": "linear", "start": [0.5, 0.2], "end": [0.5, 0.7]}));
    exec(&mut h, "mask.adjust", json!({"values": {"exposure": 0.5}}));
    h.app.ui.mask_adjusting_until = None;
    exec(&mut h, "view.maskOverlayAll", json!({}));
    h.settle(SETTLE);
    let overlay = |h: &Headless| crate::panels::detail::view_overlay(&h.app, &develop(h), false);
    assert!(matches!(overlay(&h), Overlay::AllMasks { .. }), "Ctrl+Shift+O shows every mask");
    assert!(h.app.widgets.iter().any(|(w, _)| w == "loupe:allMasks"), "a pill says All shows");
    exec(&mut h, "view.maskOverlay", json!({}));
    h.settle(SETTLE);
    assert!(!h.app.ui.mask_overlay_all);
    assert_eq!(overlay(&h), Overlay::None, "O hid the All overlay");
    assert!(!h.app.widgets.iter().any(|(w, _)| w == "loupe:allMasks"), "the pill goes with it");
}

/// Issue #867: resizing the brush with a trackpad keeps the brush cursor (no flicker to the
/// pointer), and a short swipe changes the size noticeably.
#[test]
fn trackpad_brush_resize_keeps_cursor_and_is_quick() {
    let mut h = detail("panel.masking");
    h.settle(SETTLE);
    exec(&mut h, "tool.brush", json!({"new": true}));
    pointer(&mut h, json!([{"kind": "move", "x": 0.5, "y": 0.5}]));
    h.settle(SETTLE);
    let hover_cursor = h.last_cursor;
    let size0 = h.app.ui.brush_size;
    for _ in 0..5 {
        h.events.push(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, 20.0),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
        });
        h.step();
        assert_eq!(h.last_cursor, hover_cursor, "the brush cursor stays shown while it resizes");
    }
    assert!(h.app.ui.brush_size > size0 * 1.2, "one swipe grows the brush noticeably: {size0} -> {}", h.app.ui.brush_size);
}

/// Issue #1190: with no masks on the photo, the sliders under "Settings for the next mask" look
/// like the image's own develop sliders; they are dimmed and inert (a drag changes nothing) until
/// a mask exists, then the same sliders drive the photo again.
#[test]
fn the_next_mask_settings_are_inert_until_a_mask_exists() {
    let mut h = detail("panel.masking");
    h.step();
    assert!(h.app.widgets.iter().any(|(id, _)| id == "maskNextSettings"), "the next mask's settings show");
    let drag = |h: &mut Headless| {
        let r = h.request("ui.dragWidget", json!({"id": "slider:exposure", "fx": 0.1, "dx": 600.0, "steps": 8}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
    };
    drag(&mut h);
    assert_eq!(h.app.session.mask_next.exposure, 0.0, "no mask yet: the next-mask slider is inert");
    exec(&mut h, "mask.add", json!({"kind": "radial", "center": [0.5, 0.5]}));
    drag(&mut h);
    let id = h.app.session.active().expect("active photo");
    let d = h.app.session.develop_of(id).expect("develop");
    assert!(d.masks[0].adjust.exposure.abs() > 0.1, "a mask exists: the slider works again");
}

/// Issue #1161 (Part A): clicking another mask's pin while the Brush tool (or an armed new tool)
/// is active selects that mask and its component instead of painting a stroke or adding a new mask.
#[test]
fn the_brush_tool_selects_the_mask_whose_pin_is_clicked() {
    let mut h = detail("panel.masking");
    h.settle(SETTLE);
    let has = |h: &Headless, w: &str| h.app.widgets.iter().any(|(id, _)| id == w);
    // Mask 1: Brush stroke at (0.3, 0.3)
    exec(&mut h, "tool.brush", json!({"new": true}));
    brush_at(&mut h, 0.3, 0.3);
    h.settle(SETTLE);
    // Mask 2: Radial at (0.7, 0.6)
    exec(&mut h, "mask.add", json!({"kind": "radial", "center": [0.7, 0.6], "rx": 0.12, "ry": 0.12}));
    exec(&mut h, "view.toolOverlay", json!({"mode": "always"}));
    exec(&mut h, "mask.editComponent", json!({"id": 1, "component": 0}));
    h.settle(SETTLE);
    assert_eq!(h.app.session.active_mask, Some(1));
    assert_eq!(h.app.ui.tool, "brush");

    // Hovering the active mask's pin shows Move cursor and hides the brush cursor.
    pointer(&mut h, json!([{"kind": "move", "x": 0.3, "y": 0.3}]));
    h.step();
    assert_eq!(h.last_cursor, egui::CursorIcon::Move, "hovering active mask pin shows Move cursor");
    assert!(!has(&h, "brushCursor:paint"), "no brush cursor over active pin");

    // Hovering another mask's pin also shows Move cursor and hides the brush cursor.
    pointer(&mut h, json!([{"kind": "move", "x": 0.7, "y": 0.6}]));
    h.step();
    assert_eq!(h.last_cursor, egui::CursorIcon::Move, "hovering other mask pin shows Move cursor");
    assert!(!has(&h, "brushCursor:paint"), "no brush cursor over other pin");

    // Hovering away from any pin shows the brush cursor.
    pointer(&mut h, json!([{"kind": "move", "x": 0.5, "y": 0.5}]));
    h.step();
    assert_eq!(h.last_cursor, egui::CursorIcon::None, "canvas hover hides system cursor for brush");
    assert!(has(&h, "brushCursor:paint"), "brush cursor shown away from pins");

    // Clicking the active mask's own pin does not add a stroke / dab.
    pointer(&mut h, json!([{"kind": "down", "x": 0.3, "y": 0.3}, {"kind": "up", "x": 0.3, "y": 0.3}]));
    assert_eq!(h.app.session.active_mask, Some(1));
    assert_eq!(h.app.ui.tool, "brush");
    let MaskShape::Brush { strokes } = &develop(&h).masks[0].components[0].shape else { panic!("brush") };
    assert_eq!(strokes.len(), 1, "clicking active mask pin did not add a stroke");

    // Clicking mask 2's pin selects mask 2, clears the brush tool (radial is not brush), and adds
    // no extra stroke to mask 1.
    pointer(&mut h, json!([{"kind": "down", "x": 0.7, "y": 0.6}, {"kind": "up", "x": 0.7, "y": 0.6}]));
    assert_eq!(h.app.session.active_mask, Some(2));
    let MaskShape::Brush { strokes } = &develop(&h).masks[0].components[0].shape else { panic!("brush") };
    assert_eq!(strokes.len(), 1, "clicking mask 2's pin did not paint on mask 1");
    assert_eq!(h.app.ui.tool, "");

    // Arming the brush for a new mask and clicking mask 1's pin selects mask 1, keeps Brush active,
    // clears `mask_new`, and creates neither a new mask nor a new stroke.
    exec(&mut h, "tool.brush", json!({"new": true}));
    h.step();
    assert!(h.app.ui.mask_new.is_some());
    pointer(&mut h, json!([{"kind": "down", "x": 0.3, "y": 0.3}, {"kind": "up", "x": 0.3, "y": 0.3}]));
    assert_eq!(h.app.session.active_mask, Some(1));
    assert_eq!(develop(&h).masks.len(), 2, "no third mask was added");
    let MaskShape::Brush { strokes } = &develop(&h).masks[0].components[0].shape else { panic!("brush") };
    assert_eq!(strokes.len(), 1, "no stroke added to mask 1");
    assert!(h.app.ui.mask_new.is_none());
    assert_eq!(h.app.ui.tool, "brush");

    // Dragging the active Brush pin translates its strokes without adding a new stroke.
    let MaskShape::Brush { strokes } = &develop(&h).masks[0].components[0].shape else { panic!("brush") };
    let p0 = strokes[0].points[0];
    pointer(
        &mut h,
        json!([
            {"kind": "down", "x": p0.x, "y": p0.y},
            {"kind": "drag", "x": p0.x + 0.05, "y": p0.y + 0.05},
            {"kind": "up", "x": p0.x + 0.05, "y": p0.y + 0.05}
        ]),
    );
    h.settle(SETTLE);
    let MaskShape::Brush { strokes } = &develop(&h).masks[0].components[0].shape else { panic!("brush") };
    assert_eq!(strokes.len(), 1, "dragging the pin did not paint a stroke");
    let p1 = strokes[0].points[0];
    assert!((p1.x - (p0.x + 0.05)).abs() < 0.01 && (p1.y - (p0.y + 0.05)).abs() < 0.01, "stroke translated: {p0:?} -> {p1:?}");
    assert_eq!(h.app.ui.tool, "brush");
    assert!(h.app.gesture.is_none());

    // Armed Color Range also refuses to create a new mask when clicking another mask's pin.
    exec(&mut h, "tool.colorRange", json!({"new": true}));
    h.step();
    assert!(h.app.ui.mask_new.is_some());
    pointer(&mut h, json!([{"kind": "down", "x": 0.7, "y": 0.6}, {"kind": "up", "x": 0.7, "y": 0.6}]));
    assert_eq!(develop(&h).masks.len(), 2, "armed Color Range did not add a mask on pin click");
}

/// Clicking another component's pin on a multi-component mask while Brush is active selects that
/// component (switching tool if non-Brush, or keeping Brush if Brush) without painting.
#[test]
fn brush_tool_selects_another_component_pin_on_same_mask() {
    let mut h = detail("panel.masking");
    h.settle(SETTLE);
    // Mask 1 Component 0: Brush stroke at (0.3, 0.3)
    exec(&mut h, "tool.brush", json!({"new": true}));
    brush_at(&mut h, 0.3, 0.3);
    h.settle(SETTLE);
    // Add Component 1: Radial at (0.7, 0.7) to Mask 1
    exec(&mut h, "mask.addComponent", json!({"id": 1, "op": "add", "kind": "radial", "center": [0.7, 0.7], "rx": 0.1, "ry": 0.1}));
    exec(&mut h, "view.toolOverlay", json!({"mode": "always"}));
    exec(&mut h, "mask.editComponent", json!({"id": 1, "component": 0}));
    h.settle(SETTLE);
    assert_eq!(h.app.session.active_mask, Some(1));
    assert_eq!(h.app.ui.active_mask_component, Some(0));
    assert_eq!(h.app.ui.tool, "brush");

    // Clicking component 1's pin (0.7, 0.7) switches active component to 1 and clears tool (radial)
    pointer(&mut h, json!([{"kind": "down", "x": 0.7, "y": 0.7}, {"kind": "up", "x": 0.7, "y": 0.7}]));
    assert_eq!(h.app.session.active_mask, Some(1));
    assert_eq!(h.app.ui.active_mask_component, Some(1));
    assert_eq!(h.app.ui.tool, "");
    let MaskShape::Brush { strokes } = &develop(&h).masks[0].components[0].shape else { panic!("brush") };
    assert_eq!(strokes.len(), 1, "no extra stroke painted on component 0");

    // Clicking back to component 0's pin (0.3, 0.3) switches back to component 0 without painting
    pointer(&mut h, json!([{"kind": "down", "x": 0.3, "y": 0.3}, {"kind": "up", "x": 0.3, "y": 0.3}]));
    assert_eq!(h.app.session.active_mask, Some(1));
    assert_eq!(h.app.ui.active_mask_component, Some(0));
    assert_eq!(h.app.ui.tool, "");
    let MaskShape::Brush { strokes } = &develop(&h).masks[0].components[0].shape else { panic!("brush") };
    assert_eq!(strokes.len(), 1, "no extra stroke painted on component 0");
}

/// Issue #1161 (Part B): releasing a brush drag commits the stroke before the main render job is
/// built on the same `"up"` frame, so `view_overlay` and `Slot::Main` reflect `Overlay::Mask`
/// immediately without a 1-frame delay, while an additive live stroke bridges while `Slot::Main`
/// is pending and clears once settled.
#[test]
fn brush_release_requests_mask_overlay_immediately() {
    use crate::render::Slot;
    use lcc_pipeline::Overlay;

    let mut h = detail("panel.masking");
    h.settle(SETTLE);
    exec(&mut h, "tool.brush", json!({"new": true}));
    brush_at(&mut h, 0.3, 0.4);
    h.settle(SETTLE);
    exec(&mut h, "mask.adjust", json!({"values": {"exposure": 0.6}}));
    h.app.ui.mask_adjusting_until = None;
    exec(&mut h, "view.maskOverlay", json!({"show": true}));
    exec(&mut h, "tool.brush", json!({}));
    h.app.ui.brush_auto_mask = false;
    h.settle(SETTLE);

    let img = h.app.image_rect.expect("image rect");
    let pt = |nx: f32, ny: f32| egui::pos2(img.left() + img.width() * nx, img.top() + img.height() * ny);
    let overlay = |h: &Headless| crate::panels::detail::view_overlay(&h.app, &develop(h), false);

    // Part 1: live stroke (`sent > 0` during drag) -> single-step `"up"` frame queues the exact
    // post-stroke `Overlay::Mask` job on `Slot::Main` immediately.
    let (a, b, c) = (pt(0.3, 0.6), pt(0.34, 0.6), pt(0.45, 0.6));
    let m = egui::Modifiers::default();
    h.app.synthetic.push(egui::Event::PointerMoved(a));
    h.step();
    h.app.synthetic.push(egui::Event::PointerButton { pos: a, button: egui::PointerButton::Primary, pressed: true, modifiers: m });
    h.step();
    h.app.synthetic.push(egui::Event::PointerMoved(b));
    h.step();
    h.app.synthetic.push(egui::Event::PointerMoved(c));
    h.step();
    assert!(matches!(h.app.gesture, Some(crate::panels::detail::Gesture::Brush { .. })));
    assert_eq!(overlay(&h), Overlay::None, "no overlay while brushing a mask with settings");
    let drag_wanted = h.app.renderer.wanted(Slot::Main);

    h.app.synthetic.push(egui::Event::PointerButton { pos: c, button: egui::PointerButton::Primary, pressed: false, modifiers: m });
    h.step();
    assert!(h.app.gesture.is_none(), "brush gesture committed on the up frame");
    assert!(matches!(overlay(&h), Overlay::Mask { .. }), "mask overlay is active on the up frame itself");
    let release_wanted = h.app.renderer.wanted(Slot::Main).expect("main render requested or completed on release");
    assert_ne!(Some(release_wanted), drag_wanted, "release frame updates Slot::Main away from the drag render");
    h.settle(SETTLE);
    assert_eq!(
        h.app.renderer.textures.get(&Slot::Main).map(|t| t.key),
        Some(release_wanted),
        "release frame requested the exact settled overlay job"
    );

    // Part 2: additive non-live stroke (`sent == 0`, `brush_live`) -> kept while `Slot::Main` is
    // pending on release and subsequent pending frames, then cleared once settled.
    let open = std::rc::Rc::new(std::cell::Cell::new(false));
    h.app.renderer.set_offload(Box::new(crate::headless::RenderGate::new(open.clone())));
    h.app.ui.brush_auto_mask = true;
    let (d0, d1, d2) = (pt(0.3, 0.72), pt(0.34, 0.72), pt(0.45, 0.72));
    h.app.synthetic.push(egui::Event::PointerMoved(d0));
    h.step();
    h.app.synthetic.push(egui::Event::PointerButton { pos: d0, button: egui::PointerButton::Primary, pressed: true, modifiers: m });
    h.step();
    h.app.synthetic.push(egui::Event::PointerMoved(d1));
    h.step();
    h.app.synthetic.push(egui::Event::PointerMoved(d2));
    h.step();
    assert!(h.app.brush_live.is_some(), "live stroke buffer active during additive drag");

    h.app.synthetic.push(egui::Event::PointerButton { pos: d2, button: egui::PointerButton::Primary, pressed: false, modifiers: m });
    h.step();
    assert!(h.app.gesture.is_none());
    assert!(matches!(overlay(&h), Overlay::Mask { .. }));
    assert!(h.app.renderer.is_pending(Slot::Main), "RenderGate holds Slot::Main pending");
    assert!(h.app.brush_live.is_some(), "additive live stroke bridges while Slot::Main is pending");

    h.step();
    assert!(h.app.renderer.is_pending(Slot::Main));
    assert!(h.app.brush_live.is_some(), "additive live stroke stays drawn across pending frames");

    // Part 3: starting a new stroke after a 1-point dab while `Slot::Main` is still pending must
    // clear the bridged `brush_live` buffer instead of skipping/bridging from the old dab.
    let e0 = pt(0.2, 0.2);
    h.app.synthetic.push(egui::Event::PointerMoved(e0));
    h.step();
    h.app.synthetic.push(egui::Event::PointerButton { pos: e0, button: egui::PointerButton::Primary, pressed: true, modifiers: m });
    h.step();
    h.app.synthetic.push(egui::Event::PointerButton { pos: e0, button: egui::PointerButton::Primary, pressed: false, modifiers: m });
    h.step();
    assert!(h.app.renderer.is_pending(Slot::Main));
    assert!(h.app.brush_live.is_some(), "1-point dab bridges while Slot::Main is pending");

    let (f0, f1) = (pt(0.8, 0.8), pt(0.85, 0.8));
    h.app.synthetic.push(egui::Event::PointerMoved(f0));
    h.step();
    h.app.synthetic.push(egui::Event::PointerButton { pos: f0, button: egui::PointerButton::Primary, pressed: true, modifiers: m });
    h.step();
    h.app.synthetic.push(egui::Event::PointerMoved(f1));
    h.step();
    let live = h.app.brush_live.as_ref().expect("fresh live stroke buffer on second stroke");
    let ([w, h_px], _) = live.size();
    let near = |nx: f32, ny: f32| {
        let cx = (nx * w as f32).round() as isize;
        let cy = (ny * h_px as f32).round() as isize;
        (-2..=2)
            .flat_map(|dy| (-2..=2).map(move |dx| (cx + dx, cy + dy)))
            .any(|(x, y)| x >= 0 && y >= 0 && live.coverage(x as usize, y as usize) > 0.0)
    };
    assert!(!near(0.2, 0.2), "old 1-point dab was cleared before the new stroke");
    assert!(!near(0.5, 0.5), "no bridge segment drawn across the photo");
    assert!(near(0.85, 0.8), "new stroke painted at its own path");
    h.app.synthetic.push(egui::Event::PointerButton { pos: f1, button: egui::PointerButton::Primary, pressed: false, modifiers: m });
    h.step();

    open.set(true);
    h.settle(SETTLE);
    h.step();
    assert!(!h.app.renderer.is_pending(Slot::Main));
    assert!(h.app.brush_live.is_none(), "live stroke clears once Slot::Main settles");
}
