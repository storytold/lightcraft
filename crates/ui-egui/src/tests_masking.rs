//! Headless tests of the Masking and Remove tools on the photo: overlay keys, pins, spot editing.

use std::time::Duration;

use lightcraft_develop::MaskShape;
use serde_json::json;

use crate::headless::Headless;
use crate::state::RightPanel;
use crate::{LightcraftApp, Services};
use lightcraft_catalog::Rule;

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
    h
}

fn exec(h: &mut Headless, command: &str, params: serde_json::Value) -> serde_json::Value {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
    r["result"].clone()
}

fn pointer(h: &mut Headless, events: serde_json::Value) {
    let r = h.request("ui.pointer", json!({"events": events}), T);
    assert_eq!(r["ok"], true, "{r}");
}

fn develop(h: &Headless) -> lightcraft_develop::DevelopSettings {
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
    use lightcraft_pipeline::{
        geometry::Frame,
        masks::{MatteKind, Mattes, evaluate_one},
    };
    use lightcraft_raster::{Image, Plane, Rgb32f};
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
    assert_eq!(h.app.session.active_mask, Some(2));
    assert!(!h.app.ui.brush_erase);
    assert!(h.app.ui.mask_overlay);
    let r = h.request("ui.set", json!({"brushSize": 0.15, "brushFeather": 0.0, "brushFlow": 100.0}), T);
    assert_eq!(r["ok"], true, "{r}");
    brush_at(&mut h, 0.5, 0.5);
    assert!(mask_coverage(&h, 1, 50, 50) > 0.99);
    h.app.ui.brush_erase = true;
    brush_at(&mut h, 0.5, 0.5);
    assert!(mask_coverage(&h, 1, 50, 50) < 0.01);
    exec(&mut h, "edit.undo", json!({}));
    assert!(mask_coverage(&h, 1, 50, 50) > 0.99);
    assert_eq!(develop(&h).masks[0], original);
    h.settle(SETTLE);
}

#[test]
fn mask_adjustments_hide_overlay_without_disabling_the_mask() {
    use lightcraft_pipeline::Overlay;
    let mut h = detail("panel.masking");
    exec(&mut h, "mask.add", json!({"kind": "linear", "start": [0.5, 0.2], "end": [0.5, 0.7]}));
    assert!(h.app.ui.mask_overlay);

    // Changing the shape needs the overlay; changing the image needs an unobscured preview.
    exec(&mut h, "mask.refine", json!({"value": 10.0}));
    assert!(h.app.ui.mask_overlay);
    let r = h.request("ui.clickWidget", json!({"id": "slider:exposure", "fx": 0.7}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(!h.app.ui.mask_overlay, "the exposure slider hides the overlay after release");
    let d = develop(&h);
    assert!(d.masks[0].adjust.exposure > 0.0);
    assert!(d.masks[0].visible, "hiding the overlay must not disable the mask");
    assert_eq!(crate::panels::detail::view_overlay(&h.app, &d), Overlay::None);
    let mask = d.masks[0].clone();

    let r = h.request("ui.key", json!({"key": "o"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.app.ui.mask_overlay, "O can restore the overlay");
    assert_eq!(develop(&h).masks[0], mask, "toggling the overlay leaves the mask and its effect unchanged");
    assert!(matches!(crate::panels::detail::view_overlay(&h.app, &develop(&h)), Overlay::Mask { .. }));

    // Failed edits and global adjustments must not hide the mask overlay.
    assert!(h.app.run("mask.adjust", json!({"id": 999, "values": {"exposure": 1.0}})).is_err());
    assert!(h.app.ui.mask_overlay);
    exec(&mut h, "develop.set", json!({"control": "light.exposure", "value": 0.5}));
    assert!(h.app.ui.mask_overlay);

    // Numeric edits/resets and Amount use the same command path as the sliders.
    for values in [json!({"exposure": 0.0}), json!({"saturation": 20.0}), json!({"amount": 50.0})] {
        exec(&mut h, "view.maskOverlay", json!({"show": true}));
        exec(&mut h, "mask.adjust", json!({"values": values}));
        assert!(!h.app.ui.mask_overlay);
    }
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
    use lightcraft_pipeline::{MaskView, Overlay};
    let mut h = detail("panel.masking");
    assert_eq!(h.app.ui.right, RightPanel::Masking);
    exec(&mut h, "mask.add", json!({"kind": "radial", "center": [0.3, 0.4], "rx": 0.1, "ry": 0.1}));
    exec(&mut h, "mask.add", json!({"kind": "linear", "start": [0.7, 0.2], "end": [0.7, 0.6]}));
    assert_eq!(h.app.session.active_mask, Some(2));
    // the loupe asks the renderer for the selected mask's overlay
    let d = develop(&h);
    let o = crate::panels::detail::view_overlay(&h.app, &d);
    assert_eq!(o, Overlay::Mask { id: 2, view: MaskView::Color, color: [230, 30, 40], opacity: 50 });
    // O toggles it, Shift+O cycles the colour (and leaves the crop overlay alone)
    h.request("ui.key", json!({"key": "o"}), T);
    assert!(!h.app.ui.mask_overlay);
    assert_eq!(crate::panels::detail::view_overlay(&h.app, &d), Overlay::None);
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

/// Masks list: double-click renames in place, the hover eye hides a mask, the overlay colour
/// cycles through the swatches.
#[test]
fn mask_list_rename_hide_and_overlay_colour() {
    let mut h = detail("panel.masking");
    exec(&mut h, "mask.add", json!({"kind": "linear"}));
    exec(&mut h, "mask.add", json!({"kind": "radial"}));
    h.settle(SETTLE);
    let first = develop(&h).masks[0].id;
    let r = h.request("ui.clickWidget", json!({"id": format!("mask:{first}"), "count": 2}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(Duration::from_secs(5));
    assert_eq!(h.app.ui.renaming_mask.as_ref().map(|r| r.0), Some(first), "double-click starts renaming");
    h.request("ui.key", json!({"key": "A", "cmd": true}), T);
    h.request("ui.text", json!({"text": "Sky"}), T);
    h.request("ui.key", json!({"key": "Enter"}), T);
    h.settle(Duration::from_secs(5));
    assert!(h.app.ui.renaming_mask.is_none());
    assert_eq!(develop(&h).masks[0].name, "Sky");
    // hover the row, then click its eye
    let r = h.request("ui.hoverWidget", json!({"id": format!("mask:{first}")}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(Duration::from_secs(5));
    let r = h.request("ui.clickWidget", json!({"id": format!("maskVisible:{first}")}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(!develop(&h).masks[0].visible);
    // overlay colour: no params = next swatch
    let before = h.app.ui.mask_overlay_color;
    exec(&mut h, "view.maskOverlayColor", json!({}));
    let all = crate::panels::masking::OVERLAY_COLORS;
    let i = all.iter().position(|c| *c == before).unwrap();
    assert_eq!(h.app.ui.mask_overlay_color, all[(i + 1) % all.len()]);
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
    let n = h.app.session.catalog.photos().filter(|p| !p.deleted && p.rating >= 3 && p.flag != lightcraft_catalog::Flag::Reject).count();
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
    for (group, _) in lightcraft_catalog::rules::FIELD_GROUPS {
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

/// Drawing the editor never changes a rule: values it can't show as they are (in the last 0 days,
/// 20,000 days, a plain number of days, a "between" that isn't a pair) stay as stored, and the
/// ones the check refuses are marked.
#[test]
fn smart_album_editor_never_rewrites_what_it_draws() {
    let mut h = detail("panel.edit");
    exec(&mut h, "dialog.smartAlbum", json!({"name": "Odd"}));
    let rules = json!({"rules": [
        {"field": "captureDate", "op": "inLast", "value": {"n": 0, "unit": "days"}},
        {"field": "captureDate", "op": "inLast", "value": {"n": 20000, "unit": "Days"}},
        {"field": "captureDate", "op": "notInLast", "value": 7},
        {"field": "captureDate", "op": "between", "value": "2026"},
        {"field": "iso", "op": "between", "value": [100]}
    ]});
    let Some(crate::state::Dialog::SmartRules { rules: r, .. }) = &mut h.app.ui.dialog else { panic!("no rule editor") };
    *r = serde_json::from_value(rules.clone()).unwrap();
    h.settle(SETTLE);
    let Some(crate::state::Dialog::SmartRules { rules: r, .. }) = &h.app.ui.dialog else { panic!("no rule editor") };
    assert_eq!(serde_json::to_value(r).unwrap()["rules"], rules["rules"], "unchanged by drawing");
    let marked: Vec<&str> = h.app.widgets.iter().filter_map(|(w, _)| w.strip_prefix("ruleProblem:")).collect();
    assert_eq!(marked, ["rules-0", "rules-3", "rules-4"], "0 days and the half-made betweens");
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
    let album = h.app.session.catalog.album(lightcraft_catalog::AlbumId(id)).expect("album");
    assert_eq!(album.name, "Keep", "neither OK nor confirm renamed it");
    // fixed: OK renames it and sets the rules in one undo step
    let Some(crate::state::Dialog::SmartRules { rules, .. }) = &mut h.app.ui.dialog else { panic!("the editor stays open") };
    *rules = serde_json::from_value(json!({"rules": [{"field": "captureDate", "op": "is", "value": "2026"}]})).unwrap();
    h.settle(SETTLE);
    let undo = h.app.session.undo.len();
    let r = h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let album = h.app.session.catalog.album(lightcraft_catalog::AlbumId(id)).expect("album");
    assert_eq!((album.name.as_str(), h.app.session.undo.len()), ("Renamed", undo + 1));
    exec(&mut h, "edit.undo", json!({}));
    let album = h.app.session.catalog.album(lightcraft_catalog::AlbumId(id)).expect("album");
    assert_eq!(album.name, "Keep", "one undo takes back the name with the rules");
    assert!(album.smart.as_ref().is_some_and(|f| f.rule_set.is_none()));
}

/// The sidebar marks a smart album whose rules no longer check, so it can be fixed.
#[test]
fn sidebar_marks_smart_albums_with_problems() {
    let mut h = detail("panel.edit");
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
    assert_eq!(develop(&h).geometry.upright, lightcraft_develop::Upright::Guided);
    h.settle(SETTLE);
}

#[test]
fn luminance_range_controls_and_map() {
    let mut h = detail("panel.masking");
    exec(&mut h, "mask.add", json!({"kind": "luminanceRange", "lo": 0.6, "hi": 1.0}));
    h.settle(SETTLE);
    let lum = |h: &Headless| match &develop(h).masks[0].components[0].shape {
        MaskShape::LuminanceRange { lo, hi, lo_feather, .. } => (*lo, *hi, *lo_feather),
        s => panic!("{s:?}"),
    };
    let undo0 = h.app.session.undo.len();
    // drag the high handle from the right end to 80 %
    let r = h.request("ui.dragWidget", json!({"id": "lumRange:0", "fx": 1.0, "fy": 0.5, "dx": -48.0}), T);
    assert_eq!(r["ok"], true, "{r}");
    let (lo, hi, _) = lum(&h);
    assert_eq!(lo, 0.6, "the nearer handle moves");
    assert!(hi < 0.9 && hi > 0.6, "{hi}");
    assert_eq!(h.app.session.undo.len(), undo0 + 1, "one undo step per drag");
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
    let base = std::env::temp_dir().join(format!("lc-ui-tree-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("Trip/Day 1")).unwrap();
    std::fs::create_dir_all(base.join(".hidden")).unwrap();
    let mut h = detail("panel.edit");
    exec(&mut h, "view.leftPanel", json!({"show": true}));
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

#[test]
fn slideshow_advances_pauses_and_ends() {
    let mut h = detail("panel.edit");
    let first = h.app.session.active();
    exec(&mut h, "view.slideshow", json!({"interval": 0.5}));
    assert!(h.app.ui.fullscreen && h.app.ui.slideshow.is_some());
    // simulated time runs with the frames
    let mut moved = false;
    for _ in 0..200 {
        h.step();
        if h.app.session.active() != first {
            moved = true;
            break;
        }
    }
    assert!(moved, "the next photo comes up");
    // Space pauses: nothing moves
    let r = h.request("ui.key", json!({"key": "space"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.app.ui.slideshow.is_some_and(|s| s.2), "paused");
    let held = h.app.session.active();
    for _ in 0..120 {
        h.step();
    }
    assert_eq!(h.app.session.active(), held);
    // Esc ends it
    let r = h.request("ui.key", json!({"key": "escape"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(!h.app.ui.fullscreen && h.app.ui.slideshow.is_none());
    h.settle(SETTLE);
}

#[test]
fn geometry_slider_drag_marks_the_grid() {
    let mut h = detail("panel.crop");
    let spec = lightcraft_develop::controls::find("geometry.vertical").unwrap();
    let start = crate::widgets::SliderOut { value: None, drag_started: true, drag_stopped: false, reset: false };
    crate::panels::edit::apply_slider_out(&mut h.app, spec, start, |_, _| Ok(serde_json::Value::Null));
    assert_eq!(h.app.ui.dragging_control.as_deref(), Some("geometry.vertical"));
    h.step();
    let stop = crate::widgets::SliderOut { value: None, drag_started: false, drag_stopped: true, reset: false };
    crate::panels::edit::apply_slider_out(&mut h.app, spec, stop, |_, _| Ok(serde_json::Value::Null));
    assert!(h.app.ui.dragging_control.is_none());
    h.settle(SETTLE);
}

#[test]
fn edit_in_external_editor_opens_the_copy() {
    let dir = std::env::temp_dir().join(format!("lc-ui-ext-{}", std::process::id()));
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
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo().with_fs(), services);
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
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

/// Frame time of the photo grid on a 100k-photo library (ignored:
/// `cargo test --release -p lightcraft-ui-egui -- --ignored grid_frame_100k --nocapture`).
#[test]
#[ignore]
fn grid_frame_100k() {
    use lightcraft_catalog::{Op, Photo, PhotoId, Source};
    let mut session = lightcraft_engine::Session::new();
    let ops = (0..100_000u64)
        .map(|i| {
            let mut p = Photo::new(
                PhotoId(i + 1),
                Source::Demo { scene: (i % 20) as u32 },
                &format!("IMG_{i:06}.jpg"),
                "JPEG",
                6000,
                4000 - (i % 3) as u32 * 1000,
                "2026-01-01T00:00:00",
            );
            p.captured = Some(format!("20{:02}-{:02}-{:02}T10:00:00", 10 + i % 16, 1 + i % 12, 1 + i % 28));
            p.meta.keywords = vec![format!("kw{}", i % 300)];
            Op::AddPhoto { photo: Box::new(p) }
        })
        .collect();
    session.commit("Add", Op::Batch { ops }).unwrap();
    let app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1600.0, 1000.0], 1.0);
    h.app.ui.view = crate::state::ViewMode::PhotoGrid;
    h.app.ui.left_panel = true;
    for _ in 0..5 {
        h.step();
    }
    let t = std::time::Instant::now();
    let n = 30;
    for _ in 0..n {
        h.step();
    }
    let ms = t.elapsed().as_secs_f64() * 1e3 / n as f64;
    eprintln!("grid frame at 100k photos: {ms:.1} ms (UI thread, renders excluded)");
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
    assert!(h.app.ui.keyword_painter.is_none());
    h.settle(SETTLE);
}

#[test]
fn grid_info_cycles_caption() {
    let mut h = detail("panel.edit");
    assert_eq!(exec(&mut h, "view.gridInfo", json!({}))["info"], "exposure");
    assert_eq!(exec(&mut h, "view.gridInfo", json!({}))["info"], "date");
    assert_eq!(exec(&mut h, "view.gridInfo", json!({"info": "filename"}))["info"], "filename");
    let r = h.request("engine.execute", json!({"command": "view.gridInfo", "params": {"info": "lens"}}), T);
    assert_ne!(r["ok"], true);
    exec(&mut h, "view.squareGrid", json!({}));
    exec(&mut h, "view.gridInfo", json!({"info": "exposure"}));
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
    assert_eq!(st, json!({"on": true, "space": "srgb", "destWarning": true, "displayWarning": false}));
    h.settle(SETTLE);
    assert!(red(&h) > before, "out-of-gamut colours are painted red");
    let r = h.request("engine.execute", json!({"command": "view.softProof", "params": {"space": "cmyk"}}), T);
    assert_ne!(r["ok"], true);
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
    use lightcraft_engine::segment::Segmenter;
    let mut h = detail("panel.masking");
    let dir = std::env::temp_dir().join(format!("lc-ui-no-sam3-{}", std::process::id()));
    h.app.session.segmenter.dir = Some(dir.clone());
    h.app.session.segmenter.mirrors_file = Some(dir.join("none.txt"));
    let t = std::time::Instant::now();
    let r = h.request("ui.clickWidget", json!({"id": "maskNew:object"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(develop(&h).masks.is_empty());
    if !Segmenter::AVAILABLE {
        // a build without AI masks says so (a toast), no dialog
        assert_eq!(h.app.ui.dialog, None);
        return;
    }
    assert_eq!(h.app.ui.dialog, Some(Dialog::SamModel { then: Some(("object".into(), "new".into())), error: None }));
    assert!(!h.app.session.segmenter.download_status().running, "nothing downloads without a yes");
    if h.app.session.segmenter.mirrors().is_empty() {
        // no location configured in this build: no Download button (only Close), and an agent
        // confirming anyway gets the reason; the dialog stays
        h.step();
        // …but a way to install it by hand: the guide, and the model folder (created on demand)
        assert!(h.app.widgets.iter().any(|(id, _)| id == "link:samHelp"), "no installation guide link");
        let shown = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let log = shown.clone();
        h.app.services.reveal = Some(Box::new(move |p: &str| {
            log.lock().unwrap().push(p.to_string());
            Ok(())
        }));
        h.step();
        let r = h.request("ui.clickWidget", json!({"id": "button:samFolder"}), T);
        assert_eq!(r["ok"], true, "{r}");
        assert!(dir.is_dir(), "the model folder is created to be shown");
        assert_eq!(*shown.lock().unwrap(), vec![dir.to_string_lossy().to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
        let r = h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T);
        assert_eq!(r["ok"], false, "{r}");
        let r = h.request("ui.dialog.confirm", json!({}), T);
        assert_eq!(r["ok"], false, "{r}");
        assert!(r["error"].as_str().unwrap_or_default().contains("LIGHTCRAFT_SAM3_MIRRORS"), "{r}");
        assert!(matches!(h.app.ui.dialog, Some(Dialog::SamModel { .. })), "stays open");
    } else {
        // with a mirror: Download starts it in the background (here it fails: nothing listens)
        let r = h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T);
        assert_eq!(r["ok"], true, "{r}");
    }
    // (a frame with the message laid out, so the buttons are where they are drawn)
    h.step();
    h.step();
    let r = h.request("ui.clickWidget", json!({"id": "button:dialogCancel"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(h.app.ui.dialog, None);
    // Describe asks too
    let r = h.request("ui.clickWidget", json!({"id": "maskNew:prompt"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(h.app.ui.dialog, Some(Dialog::SamModel { then: Some(("prompt".into(), "new".into())), error: None }));
    assert_eq!(h.app.ui.describe, None);
    // a click command from an agent gets the not-installed error at once, and the app offers the model
    let e = h.app.run("mask.add", json!({"kind": "prompt", "text": "sky"})).unwrap_err();
    assert!(e.starts_with(lightcraft_engine::segment::NOT_INSTALLED), "{e}");
    assert!(develop(&h).masks.is_empty());
    assert!(t.elapsed() < SETTLE, "{:?}", t.elapsed());
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
    // along the centreline between the samples (away from the cursor ring at the last one), the
    // overlay colour at full coverage over the photo
    let mut gaps = vec![];
    for i in 1..=74 {
        let x = a.0 + (c.0 - a.0) * i as f32 / 80.0;
        let p = px(&photo, x, a.1);
        let want = [p[0] + (r0 - p[0]) * op, p[1] + (g0 - p[1]) * op, p[2] + (b0 - p[2]) * op];
        let got = px(&held, x, a.1);
        if (0..3).any(|k| (got[k] - want[k]).abs() > 12.0) {
            gaps.push((x, got, want));
        }
    }
    assert!(gaps.is_empty(), "the live stroke has gaps: {} of 74 points, first {:?}", gaps.len(), gaps.first());
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
