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

#[test]
fn masking_thumbnails_show_composite_coverage_and_follow_edits() {
    fn thumbnail(h: &mut Headless, mask: u32) -> (u64, egui::ColorImage) {
        assert!(h.settle(SETTLE));
        let photo = h.app.session.active().unwrap();
        let key = h.app.session.mask_thumbnail_job(photo, mask, 96).unwrap().key;
        let tex = h.app.renderer.textures.get(&crate::render::Slot::Variant(key)).expect("mask coverage texture rendered");
        let pixels = tex.pixels.as_deref().unwrap().clone();
        assert!(pixels.pixels.iter().all(|p| p.r() == p.g() && p.g() == p.b() && p.a() == 255));
        let rect = h.app.widgets.iter().find(|(id, _)| id == &format!("maskThumbnailImage:{mask}")).unwrap().1;
        assert!(
            (rect.aspect_ratio() - pixels.size[0] as f32 / pixels.size[1] as f32).abs() < 0.01,
            "fit the whole photo without stretching/cropping"
        );
        // The row actually paints the coverage texture, rather than retaining a type icon.
        let screen = h.paint();
        let center = screen[(rect.center().x as usize, rect.center().y as usize)];
        let center_alpha = pixels[(pixels.size[0] / 2, pixels.size[1] / 2)].r();
        assert!(center.r().abs_diff(center_alpha) <= 3, "{center:?} vs {center_alpha}");
        (key, pixels)
    }
    let mut h = detail("panel.masking");
    exec(&mut h, "mask.add", json!({"kind":"radial", "center":[0.5,0.5], "rx":0.3,"ry":0.3,"feather":30}));
    let mask = h.app.session.active_mask.unwrap();
    let (key, radial) = thumbnail(&mut h, mask);
    assert_eq!(radial[(radial.size[0] / 2, radial.size[1] / 2)].r(), 255);
    assert_eq!(radial[(0, 0)].r(), 0);
    assert!(radial.pixels.iter().any(|p| p.r() > 10 && p.r() < 240), "feather has intermediate coverage");
    exec(&mut h, "mask.addComponent", json!({"kind":"brush", "op":"subtract"}));
    exec(&mut h, "mask.brushStroke", json!({"points":[[0.5,0.5]], "size":0.12,"feather":0,"flow":100,"density":100}));
    let (painted_key, painted) = thumbnail(&mut h, mask);
    assert_ne!(key, painted_key);
    assert_eq!(painted[(painted.size[0] / 2, painted.size[1] / 2)].r(), 0, "subtract brush removes the center");
    assert_ne!(painted, radial);
    exec(&mut h, "edit.undo", json!({}));
    assert_eq!(thumbnail(&mut h, mask).1, radial);
    exec(&mut h, "edit.redo", json!({}));
    assert_eq!(thumbnail(&mut h, mask).1, painted);
    exec(&mut h, "mask.addComponent", json!({"kind":"linear", "op":"intersect", "start":[0.0,0.3],"end":[0.0,0.7]}));
    let (_, intersected) = thumbnail(&mut h, mask);
    assert!(intersected.pixels.iter().zip(&painted.pixels).any(|(a, b)| a.r() < b.r()));
    assert!(intersected.pixels.iter().zip(&painted.pixels).all(|(a, b)| a.r() <= b.r()));
    exec(&mut h, "mask.invert", json!({}));
    let (_, inverted) = thumbnail(&mut h, mask);
    assert!(intersected.pixels.iter().zip(&inverted.pixels).all(|(a, b)| (u16::from(a.r()) + u16::from(b.r())).abs_diff(255) <= 1));
    exec(&mut h, "mask.visible", json!({"visible":false}));
    assert_eq!(thumbnail(&mut h, mask).1, inverted, "hidden mask still shows its range");
    let before = develop(&h);
    exec(&mut h, "ui.maskPanel", json!({"floating":true}));
    exec(&mut h, "view.maskOverlay", json!({"show":false}));
    exec(&mut h, "view.maskOverlayColor", json!({"color":"#00ff00","opacity":15}));
    assert_eq!(thumbnail(&mut h, mask).1, inverted, "layout and overlay colour do not alter the thumbnail");
    assert_eq!(develop(&h), before, "thumbnail requests never edit the photo");
    let done = h.app.renderer.completed;
    for _ in 0..10 {
        h.step();
    }
    assert_eq!(h.app.renderer.completed, done, "unchanged thumbnails reuse their textures");
}

/// The first mask has direct choices; subsequent masks use the compact creation menu.
#[test]
fn create_mask_menu_opens_dismisses_and_creates_undoable_masks() {
    let mut h = detail("panel.masking");
    let choices_visible = |h: &Headless| h.app.widgets.iter().filter(|(id, _)| id.starts_with("maskNew:")).count();
    assert_eq!(choices_visible(&h), 10, "the first mask needs no extra click");
    assert!(!h.app.widgets.iter().any(|(id, _)| id == "icon:maskCreate"));
    click(&mut h, "maskNew:linear");
    let first = develop(&h).masks;
    assert_eq!(first.len(), 1);
    assert_eq!(choices_visible(&h), 0, "creation collapses the choices");
    exec(&mut h, "edit.undo", json!({}));
    assert!(develop(&h).masks.is_empty());
    assert_eq!(choices_visible(&h), 10, "undoing the first mask restores the direct choices");
    exec(&mut h, "edit.redo", json!({}));
    assert_eq!(develop(&h).masks, first);
    assert_eq!(choices_visible(&h), 0);
    click(&mut h, "icon:maskCreate");
    assert_eq!(choices_visible(&h), 10);
    h.request("ui.key", json!({"key": "Escape"}), T);
    assert_eq!(choices_visible(&h), 0);
    assert_eq!(h.app.ui.right, RightPanel::Masking);
    assert_eq!(h.app.ui.view, crate::state::ViewMode::Detail);
    assert_eq!(develop(&h).masks, first);

    click(&mut h, "icon:maskCreate");
    // Click the header, outside the popup, without creating a mask.
    let plus = h.app.widgets.iter().find(|(id, _)| id == "icon:maskCreate").unwrap().1;
    h.request("ui.click", json!({"x": plus.left() - 80.0, "y": plus.center().y}), T);
    assert_eq!(choices_visible(&h), 0);
    assert_eq!(develop(&h).masks, first);

    for kind in ["linear", "radial", "brush", "luminanceRange", "colorRange"] {
        let before = develop(&h).masks;
        click(&mut h, "icon:maskCreate");
        click(&mut h, &format!("maskNew:{kind}"));
        assert_eq!(choices_visible(&h), 0, "choosing {kind} closes the menu");
        let after = develop(&h).masks;
        assert_eq!(after.len(), before.len() + 1);
        assert_eq!(&after[..before.len()], &before);
        let created = after.last().unwrap();
        assert_eq!(h.app.session.active_mask, Some(created.id));
        assert_eq!(created.components.len(), 1);
        assert_created_mask_kind(kind, &created.components[0].shape);
        if ["linear", "radial", "brush", "colorRange"].contains(&kind) {
            assert_eq!(h.app.ui.tool, kind);
        }
        exec(&mut h, "edit.undo", json!({}));
        assert_eq!(develop(&h).masks, before);
        exec(&mut h, "edit.redo", json!({}));
        assert_eq!(develop(&h).masks, after);
    }
    for mask in develop(&h).masks {
        exec(&mut h, "mask.delete", json!({"id": mask.id}));
    }
    assert!(develop(&h).masks.is_empty());
    assert_eq!(choices_visible(&h), 10, "deleting the last mask restores direct creation");
    assert!(!h.app.widgets.iter().any(|(id, _)| id == "icon:maskCreate"));
}

fn assert_created_mask_kind(kind: &str, shape: &MaskShape) {
    assert!(matches!(
        (kind, shape),
        ("linear", MaskShape::Linear { .. })
            | ("radial", MaskShape::Radial { .. })
            | ("brush", MaskShape::Brush { .. })
            | ("luminanceRange", MaskShape::LuminanceRange { .. })
            | ("colorRange", MaskShape::ColorRange { .. })
    ));
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
fn masking_component_menus_offer_all_creation_kinds_and_preserve_the_operation() {
    let mut h = detail("panel.masking");
    exec(&mut h, "mask.add", json!({"kind": "brush"}));
    click(&mut h, "icon:maskCreate");
    let mut creation_kinds: Vec<_> = h.app.widgets.iter().filter_map(|(id, _)| id.strip_prefix("maskNew:").map(str::to_owned)).collect();
    creation_kinds.sort();
    assert_eq!(creation_kinds.len(), 10);
    h.request("ui.key", json!({"key": "Escape"}), T);
    let original = develop(&h);
    for op in ["add", "subtract", "intersect"] {
        for kind in ["brush", "background", "colorRange", "linear", "radial", "sky", "subject", "luminanceRange"] {
            match op {
                "add" => click(&mut h, "button:maskPlus:1"),
                "subtract" => click(&mut h, "button:maskMinus:1"),
                _ => {
                    click(&mut h, "button:componentMenu0");
                    click(&mut h, "maskComponentMenu:0:intersect");
                }
            }
            let prefix = format!("maskComp:{op}:");
            let mut kinds: Vec<_> = h.app.widgets.iter().filter_map(|(id, _)| id.strip_prefix(&prefix).map(str::to_owned)).collect();
            kinds.sort();
            assert_eq!(kinds, creation_kinds, "{op} must offer every creatable kind");
            click(&mut h, &format!("maskComp:{op}:{kind}"));
            let after = develop(&h);
            assert_eq!(after.masks.len(), 1);
            assert_eq!(after.masks[0].components.len(), 2, "{op}/{kind} creates a component in the existing mask");
            assert_eq!(after.masks[0].components[0], original.masks[0].components[0]);
            let component = &after.masks[0].components[1];
            assert_eq!(serde_json::to_value(component.op).unwrap(), op);
            assert_eq!(serde_json::to_value(&component.shape).unwrap()["kind"], kind);
            assert_eq!(h.app.ui.mask_component.map(|(_, _, i)| i), Some(1));
            if ["brush", "linear", "radial", "colorRange"].contains(&kind) {
                assert_eq!(h.app.ui.tool, kind, "the new component is ready for canvas input");
            }
            assert!(!h.app.widgets.iter().any(|(id, _)| id.starts_with("maskComp:")));
            exec(&mut h, "edit.undo", json!({}));
            assert_eq!(develop(&h), original);
            exec(&mut h, "edit.redo", json!({}));
            assert_eq!(develop(&h), after);
            exec(&mut h, "edit.undo", json!({}));
        }
    }
    h.settle(SETTLE);
}

#[test]
fn masking_parent_menus_combine_with_the_clicked_mask() {
    for floating in [false, true] {
        let mut h = detail("panel.masking");
        exec(&mut h, "mask.add", json!({"kind": "linear"}));
        exec(&mut h, "mask.add", json!({"kind": "radial"}));
        exec(&mut h, "ui.maskPanel", json!({"floating": floating}));
        let before = develop(&h);
        for context in [true, false] {
            for op in ["intersect", "subtract"] {
                for kind in ["brush", "colorRange"] {
                    exec(&mut h, "mask.select", json!({"id": 2}));
                    let undo = h.app.session.undo.len();
                    let r = h.request(
                        "ui.clickWidget",
                        if context { json!({"id": "mask:1", "button": "right"}) } else { json!({"id": "maskMenu:1"}) },
                        T,
                    );
                    assert_eq!(r["ok"], true, "{r}");
                    click(&mut h, &format!("maskCombineMenu:1:{op}"));
                    for kind in ["object", "prompt", "subject", "sky", "background", "brush", "linear", "radial", "luminanceRange", "colorRange"] {
                        assert!(h.app.widgets.iter().any(|(id, _)| id == &format!("maskComp:{op}:{kind}")), "missing {op}/{kind}");
                    }
                    assert_eq!(develop(&h), before, "opening the submenu never edits the photo");
                    assert_eq!(h.app.session.active_mask, Some(2), "opening another mask's menu preserves selection");
                    click(&mut h, &format!("maskComp:{op}:{kind}"));
                    let after = develop(&h);
                    assert_eq!(after.masks.len(), 2);
                    assert_eq!(after.masks[1], before.masks[1], "the previously selected mask stays unchanged");
                    assert_eq!(after.masks[0].components.len(), 2);
                    assert_eq!(after.masks[0].components[0], before.masks[0].components[0]);
                    let added = &after.masks[0].components[1];
                    assert_eq!(serde_json::to_value(added.op).unwrap(), op);
                    assert_eq!(serde_json::to_value(&added.shape).unwrap()["kind"], kind);
                    assert_eq!(h.app.session.active_mask, Some(1));
                    assert_eq!(h.app.ui.mask_component.map(|(_, mask, i)| (mask, i)), Some((1, 1)));
                    assert_eq!(h.app.ui.tool, kind);
                    assert_eq!(h.app.session.undo.len(), undo + 1);
                    assert!(!h.app.widgets.iter().any(|(id, _)| id.starts_with("maskComp:")), "choosing a type closes the menu");
                    exec(&mut h, "edit.undo", json!({}));
                    assert_eq!(develop(&h), before);
                    exec(&mut h, "edit.redo", json!({}));
                    assert_eq!(develop(&h), after);
                    exec(&mut h, "edit.undo", json!({}));
                }
            }
        }
    }
}

#[test]
fn masking_intersection_color_range_samples_the_selected_component_on_canvas() {
    let mut h = detail("panel.masking");
    exec(&mut h, "mask.add", json!({"kind": "colorRange", "samples": [[50, 0, 0]]}));
    click(&mut h, "button:componentMenu0");
    click(&mut h, "maskComponentMenu:0:intersect");
    click(&mut h, "maskComp:intersect:colorRange");
    let before = develop(&h);
    let samples = |d: &lightcraft_develop::DevelopSettings| {
        let MaskShape::ColorRange { samples, .. } = &d.masks[0].components[1].shape else { panic!("colour range") };
        samples.clone()
    };
    brush_at(&mut h, 0.4, 0.3);
    let picked = develop(&h);
    assert_eq!(samples(&picked).len(), 1, "clicking the photo must sample the new intersection");
    assert_eq!(picked.masks[0].components[0], before.masks[0].components[0], "the earlier colour range stays unchanged");
    let r = h.request("ui.pointer", json!({"shift": true, "events": [{"kind": "down", "x": 0.7, "y": 0.7}, {"kind": "up", "x": 0.7, "y": 0.7}]}), T);
    assert_eq!(r["ok"], true, "{r}");
    let added = develop(&h);
    assert_eq!(samples(&added).len(), 2, "Shift-click adds a sample to the selected component");
    assert_eq!(added.masks[0].components[0], before.masks[0].components[0]);
    exec(&mut h, "edit.undo", json!({}));
    assert_eq!(develop(&h), picked);
    exec(&mut h, "edit.undo", json!({}));
    assert_eq!(develop(&h), before);
    exec(&mut h, "edit.redo", json!({}));
    exec(&mut h, "edit.redo", json!({}));
    assert_eq!(develop(&h), added);
    h.settle(SETTLE);
}

#[test]
fn intersect_brush_keeps_only_painted_overlap_and_supports_erase_and_undo() {
    let mut h = detail("panel.masking");
    exec(&mut h, "mask.add", json!({"kind": "radial", "rx": 0.2, "ry": 0.2, "feather": 0}));
    let original = develop(&h);
    h.app.ui.brush_erase = true;
    h.app.ui.mask_overlay = false;
    exec(&mut h, "mask.brushSettings", json!({"size": 0.1, "feather": 0, "flow": 100, "density": 100}));
    click(&mut h, "button:componentMenu0");
    click(&mut h, "maskComponentMenu:0:intersect");
    click(&mut h, "maskComp:intersect:brush");
    assert_eq!(h.app.ui.tool, "brush");
    assert!(!h.app.ui.brush_erase);
    assert!(h.app.ui.mask_overlay);
    assert_eq!(h.app.ui.mask_component.map(|(_, _, i)| i), Some(1));
    assert_eq!(develop(&h).masks[0].components[1].op, lightcraft_develop::MaskOp::Intersect);
    assert!(!h.app.widgets.iter().any(|(id, _)| id.starts_with("maskComp:")), "selection closes the menu");
    brush_at(&mut h, 0.5, 0.5);
    assert!(mask_coverage(&h, 0, 50, 50) > 0.99, "paint inside the original mask stays selected");
    assert!(mask_coverage(&h, 0, 62, 50) < 0.01, "unpainted area of the original mask is excluded");
    let painted = develop(&h);
    brush_at(&mut h, 0.1, 0.1);
    assert!(mask_coverage(&h, 0, 10, 10) < 0.01, "painting outside the original mask cannot expand it");
    exec(&mut h, "edit.undo", json!({}));
    assert_eq!(develop(&h), painted);
    exec(&mut h, "mask.brushSettings", json!({"erase": true}));
    brush_at(&mut h, 0.5, 0.5);
    assert!(mask_coverage(&h, 0, 50, 50) < 0.01, "Erase removes paint within the intersection");
    exec(&mut h, "edit.undo", json!({}));
    assert_eq!(develop(&h), painted);
    exec(&mut h, "edit.undo", json!({}));
    exec(&mut h, "edit.undo", json!({}));
    assert_eq!(develop(&h), original, "undo removes the stroke and new component separately");
    exec(&mut h, "edit.redo", json!({}));
    exec(&mut h, "edit.redo", json!({}));
    assert_eq!(develop(&h), painted);
    h.settle(SETTLE);
}

#[test]
fn new_brush_starts_a_separate_mask_in_paint_mode() {
    let mut h = detail("panel.masking");
    exec(&mut h, "mask.add", json!({"kind": "sky"}));
    let original = develop(&h).masks[0].clone();
    h.app.ui.brush_erase = true;
    h.app.ui.mask_overlay = false;
    click(&mut h, "icon:maskCreate");
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
    // Removing the creation grid makes the exposure adjustment visible without scrolling.
    let panel = h.app.widgets.iter().find(|(id, _)| id == "panel:right_panel").unwrap().1;
    let slider = h.app.widgets.iter().find(|(id, _)| id == "slider:exposure").unwrap().1;
    assert!(panel.contains_rect(slider), "exposure is visible before clicking: {slider:?} in {panel:?}");
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

    // Ctrl+Shift+O / Cmd+Shift+O shows every visible mask's region at once (the new command), ahead of the selected
    // mask overlay so it is not shadowed by the always-on single-mask overlay.
    exec(&mut h, "view.maskOverlayAll", json!({}));
    assert!(h.app.ui.mask_overlay_all, "the all-masks overlay command turns it on");
    assert!(matches!(crate::panels::detail::view_overlay(&h.app, &develop(&h)), Overlay::AllMasks { .. }));
    exec(&mut h, "view.maskOverlayAll", json!({}));
    assert!(!h.app.ui.mask_overlay_all, "the all-masks overlay command toggles it off");
    assert!(!matches!(crate::panels::detail::view_overlay(&h.app, &develop(&h)), Overlay::AllMasks { .. }));

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

/// A slider hides the overlay (above); a new mask, an added part, a duplicate or picking a mask in
/// the list shows it again, so the Overlay menu's mode is what the photo shows (issue #1800).
#[test]
fn a_new_or_picked_mask_shows_the_overlay_hidden_by_a_slider() {
    use lightcraft_pipeline::Overlay;
    let mut h = detail("panel.masking");
    exec(&mut h, "mask.add", json!({"kind": "linear", "start": [0.5, 0.2], "end": [0.5, 0.7]}));
    let first = h.app.session.active_mask.unwrap();
    let hide = |h: &mut Headless| {
        exec(h, "mask.adjust", json!({"values": {"exposure": 0.5}}));
        assert!(!h.app.ui.mask_overlay);
    };
    let shown = |h: &Headless| matches!(crate::panels::detail::view_overlay(&h.app, &develop(h)), Overlay::Mask { .. });
    hide(&mut h);
    exec(&mut h, "mask.add", json!({"kind": "radial", "center": [0.4, 0.5], "rx": 0.2, "ry": 0.2}));
    assert!(shown(&h), "a new mask shows its overlay");
    hide(&mut h);
    let r = h.request("ui.clickWidget", json!({"id": format!("mask:{first}")}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(h.app.session.active_mask, Some(first));
    assert!(shown(&h), "picking a mask in the list shows its overlay");
    for (cmd, params) in
        [("mask.addComponent", json!({"kind": "radial", "op": "add", "center": [0.6, 0.5], "rx": 0.1, "ry": 0.1})), ("mask.duplicate", json!({}))]
    {
        hide(&mut h);
        exec(&mut h, cmd, params);
        assert!(shown(&h), "{cmd} shows the overlay");
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
fn mask_overlay_color_refuses_anything_but_six_hex_digits() {
    let mut h = detail("panel.masking");
    exec(&mut h, "view.maskOverlayColor", json!({"color": "#2870f0"}));
    for bad in ["#+f+f+f", "+12345", "#-0-0-0", "##ffffff"] {
        let r = h.request("engine.execute", json!({"command": "view.maskOverlayColor", "params": {"color": bad}}), T);
        assert_eq!(r["ok"], false, "{bad:?} is not a colour: {r}");
        assert_eq!(h.app.ui.mask_overlay_color, [0x28, 0x70, 0xf0], "{bad:?} leaves the overlay colour alone");
    }
    exec(&mut h, "view.maskOverlayColor", json!({"color": "e63228"}));
    assert_eq!(h.app.ui.mask_overlay_color, [0xe6, 0x32, 0x28]);
    exec(&mut h, "view.maskOverlayColor", json!({"color": [10, 20, 30]}));
    assert_eq!(h.app.ui.mask_overlay_color, [10, 20, 30]);
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

/// Issue #3049: Show Pins draws a pin for an Object mask (at its first click) and a Describe
/// mask (on what its segmentation found). Clicking one selects its mask; dragging it leaves the
/// mask and the History alone, since neither can be moved.
#[test]
fn object_and_describe_masks_show_pins_that_select_but_do_not_move() {
    use lightcraft_develop::SegMask;
    use lightcraft_geom::Point;
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
    use lightcraft_geom::Point;
    let mut h = detail("panel.masking");
    // no SAM 3 model here: a stray Object click would offer the download or show an error
    let dir = std::env::temp_dir().join(format!("lc-ui-pin-object-{}", std::process::id()));
    h.app.session.segmenter.dir = Some(dir.clone());
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
    let toast = h.app.ui.toast.as_ref().map(|t| t.0.clone());
    let before = develop(&h).masks;
    // the Object mask's pin: it is selected, its clicks and the selected mask's stay as they were
    pointer(&mut h, json!([{"kind": "down", "x": 0.6, "y": 0.5}, {"kind": "up", "x": 0.6, "y": 0.5}]));
    h.settle(SETTLE);
    assert_eq!(h.app.session.active_mask, Some(1), "the pinned mask is selected");
    assert_eq!(develop(&h).masks, before, "no Object click was added to any mask");
    assert_eq!(h.app.ui.dialog, None, "no Object click was tried");
    assert_eq!(h.app.ui.toast.as_ref().map(|t| t.0.clone()).filter(|t| Some(t) != toast.as_ref()), None, "no error");
    assert_eq!(h.app.ui.tool, "object", "the tool stays on to refine the Object mask");
    // the Radial mask's pin: selected, and the Object tool goes off (nothing for it to refine)
    pointer(&mut h, json!([{"kind": "down", "x": 0.2, "y": 0.8}, {"kind": "up", "x": 0.2, "y": 0.8}]));
    h.settle(SETTLE);
    assert_eq!(h.app.session.active_mask, Some(2));
    assert_eq!(develop(&h).masks, before);
    assert_eq!(h.app.ui.tool, "", "the Object tool is off");
    assert_eq!(h.app.ui.dialog, None);
}

/// Issue #3049: with the Object tool on, a click on the selected Object mask's own pin adds no
/// near-copy of its first click (which would use up one of its limited Object clicks): its
/// clicks stay as they were, it stays selected and no error is shown.
#[test]
fn the_object_tool_adds_no_click_on_the_selected_masks_own_pin() {
    use lightcraft_geom::Point;
    let mut h = detail("panel.masking");
    // no SAM 3 model here: a stray Object click would offer the download or show an error
    let dir = std::env::temp_dir().join(format!("lc-ui-own-pin-object-{}", std::process::id()));
    h.app.session.segmenter.dir = Some(dir.clone());
    h.app.session.segmenter.mirrors_file = Some(dir.join("none.txt"));
    exec(&mut h, "mask.add", json!({"kind": "radial", "center": [0.5, 0.5], "rx": 0.1, "ry": 0.1}));
    let object = MaskShape::Object { hint: vec![Point::new(0.6, 0.5)], exclude: vec![], seg: None, detail: vec![], edge: 0.0 };
    exec(&mut h, "mask.update", json!({"id": 1, "shape": object}));
    assert_eq!(h.app.session.active_mask, Some(1));
    exec(&mut h, "view.toolOverlay", json!({"mode": "always"}));
    h.app.ui.tool = "object".into();
    h.settle(SETTLE);
    let toast = h.app.ui.toast.as_ref().map(|t| t.0.clone());
    let before = develop(&h).masks;
    // a little off the pin's centre, as a hand clicks it
    pointer(&mut h, json!([{"kind": "down", "x": 0.601, "y": 0.501}, {"kind": "up", "x": 0.601, "y": 0.501}]));
    h.settle(SETTLE);
    assert_eq!(develop(&h).masks, before, "no Object click was added");
    assert_eq!(h.app.session.active_mask, Some(1), "the mask stays selected");
    assert_eq!(h.app.ui.dialog, None, "no Object click was tried");
    assert_eq!(h.app.ui.toast.as_ref().map(|t| t.0.clone()).filter(|t| Some(t) != toast.as_ref()), None, "no error");
    assert_eq!(h.app.ui.tool, "object", "the tool stays on");
}

/// #2210: Ctrl+Shift+O (the all-masks overlay) draws the white outline of every visible
/// Radial/Linear mask, not just the selected one — but only the selected mask registers
/// `maskHandle:` elements, so nothing else becomes draggable.
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
    // The selected mask keeps its 4 edge handles and its rotation grip.
    assert_eq!(handles_of(&h, 2), 5, "the selected mask keeps its handles");
    // The other visible mask draws its outline but registers no handle: it is not draggable.
    assert_eq!(handles_of(&h, 1), 0, "an unselected mask must not become draggable");
    // …while a mask with no components has nothing to outline and nothing to grab either.
    exec(&mut h, "mask.add", json!({"kind": "luminanceRange"}));
    exec(&mut h, "mask.addComponent", json!({"kind": "radial", "op": "add", "center": [0.5, 0.8], "rx": 0.08, "ry": 0.08}));
    assert_eq!(h.app.session.active_mask, Some(3));
    assert_eq!(handles_of(&h, 1), 0);
    h.settle(SETTLE);
}

/// #2210: the outlines of *unselected* masks are painted, not registered as widgets, so the way to
/// observe them is the rendered pixels. With the all-masks overlay on, the unselected mask's
/// ellipse edge must carry white stroke pixels the photo alone does not have.
#[test]
fn all_masks_overlay_paints_the_outline_of_an_unselected_mask() {
    let mut h = detail("panel.masking");
    exec(&mut h, "mask.add", json!({"kind": "radial", "center": [0.3, 0.5], "rx": 0.12, "ry": 0.12}));
    exec(&mut h, "mask.add", json!({"kind": "radial", "center": [0.7, 0.5], "rx": 0.12, "ry": 0.12}));
    assert_eq!(h.app.session.active_mask, Some(2));
    exec(&mut h, "view.maskPins", json!({"show": false}));

    // Sample the whole canvas: the outline is white (alpha 170) over the photo, so the overlay adds
    // near-white pixels along the unselected mask's ellipse edge.
    let white_pixels = |h: &mut Headless, on: bool| -> usize {
        let overlay = h.app.ui.mask_overlay_all;
        if overlay != on {
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
    assert_eq!(develop(&h).crop.aspect, None);
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

/// A folder listed twice in Local (Pictures as a place of its own and again inside Home) opens
/// and closes on each row by itself: both copies of its triangle used to share one egui id, so
/// the copy drawn later took the clicks of the one drawn first (issue #1093).
#[test]
fn local_folder_listed_twice_opens_and_closes_each_row() {
    let base = std::env::temp_dir().join(format!("lc-ui-twice-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let inner = base.join("Inner");
    std::fs::create_dir_all(inner.join("Sub")).unwrap();
    let mut h = detail("panel.edit");
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

/// An editor that won't open the copy: the copy is in the library already, so the command succeeds
/// with `failed` set and a toast saying why, instead of an error a script would retry (and make
/// another copy each time).
#[test]
fn edit_in_reports_an_editor_that_wont_open_without_failing() {
    let dir = std::env::temp_dir().join(format!("lc-ui-ext-fail-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let services = Services { png: None, open_with: Some(Box::new(|_: &str, _: &str| Err("the editor quit".into()))), ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo().with_fs(), services);
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    h.app.ui.settings.external_editor = "PhotoCraft".into();
    let n = h.app.session.catalog.len();
    let r = h.request("engine.execute", json!({"command": "photo.editInExternal", "params": {"dir": dir.to_string_lossy()}}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["result"]["failed"], "the editor quit", "{r}");
    assert_eq!(h.app.session.catalog.len(), n + 1, "the copy stays in the library");
    let toast = h.app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default();
    assert!(toast.contains("the editor quit"), "{toast}");
    let _ = std::fs::remove_dir_all(&dir);
    h.settle(SETTLE);
}

/// Edit in PhotoCraft: a PSD copy (what PhotoCraft saves back to), handed to "PhotoCraft" for
/// the host to find; when the host can't find it, no copy is made.
#[test]
fn edit_in_photocraft_opens_a_psd_copy() {
    let dir = std::env::temp_dir().join(format!("lc-ui-pcraft-{}", std::process::id()));
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
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo().with_fs(), services);
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    let n = h.app.session.catalog.len();
    let params = json!({"command": "photo.editInPhotoCraft", "params": {"dir": dir.to_string_lossy()}});
    let r = h.request("engine.execute", params.clone(), T);
    assert_eq!(r["ok"], false, "{r}");
    assert_eq!((h.app.session.catalog.len(), opened.lock().unwrap().len()), (n, 0), "no copy without PhotoCraft");
    // and it says so, however it was asked for (menu, shortcut, context menu)
    let toast = h.app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default();
    assert!(!toast.is_empty(), "the missing editor is reported");
    h.app.ui.toast = None;
    let r = h.app.run("photo.editInPhotoCraft", json!({}));
    assert!(r.is_err() && h.app.ui.toast.is_some(), "{r:?} {:?}", h.app.ui.toast);
    installed.store(true, std::sync::atomic::Ordering::SeqCst);
    let r = h.request("engine.execute", params, T);
    assert_eq!(r["ok"], true, "{r}");
    let calls = opened.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].0.ends_with("-Edit.psd") && std::path::Path::new(&calls[0].0).exists(), "{calls:?}");
    assert_eq!(calls[0].1, "PhotoCraft");
    let d = lightcraft_codecs::decode(&std::fs::read(&calls[0].0).unwrap(), Default::default()).unwrap();
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
    use lightcraft_catalog::{Op, Photo, Source};
    let dir = std::env::temp_dir().join(format!("lc-ui-orig-{}", std::process::id()));
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
    let mut session = lightcraft_engine::Session::with_demo().with_fs();
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
    let app = LightcraftApp::new(session, services);
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
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

    // the editor can be named per call; the active photo must have a file for the menu
    let r = exec(&mut h, "photo.openOriginalInExternal", json!({"ids": [a.0], "app": "/opt/dxo"}));
    assert_eq!(r["opened"][0]["path"], a_path.as_str());
    assert_eq!(opened.lock().unwrap().last().unwrap().1, "/opt/dxo");
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
/// told why. The path LightCraft does hand over is absolute, whatever the record's spelling.
#[test]
fn open_original_refuses_anything_that_is_not_a_photo_file() {
    use lightcraft_catalog::{Op, Photo, Source};
    let dir = std::env::temp_dir().join(format!("lc-ui-gate-{}", std::process::id()));
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
    let mut session = lightcraft_engine::Session::with_demo().with_fs();
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
    let app = LightcraftApp::new(session, services);
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    h.app.ui.settings.external_editor = "DxO PhotoLab".into();

    for (id, name) in bad.iter().zip(programs) {
        let r = h.request("engine.execute", json!({"command": "photo.openOriginalInExternal", "params": {"ids": [id.0]}}), T);
        assert_eq!(r["ok"], false, "{name} was opened: {r}");
        assert!(r.to_string().contains("file types LightCraft reads"), "{name}: {r}");
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
    // Photo Grid has no captions: picking one shows it in Square Grid
    exec(&mut h, "view.photoGrid", json!({}));
    exec(&mut h, "view.gridInfo", json!({"info": "date"}));
    assert_eq!(h.app.ui.view, crate::state::ViewMode::SquareGrid);
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
    assert_eq!(
        st,
        json!({"on": true, "space": "srgb", "profile": null, "intent": "perceptual", "paperInk": false, "destWarning": true, "displayWarning": false})
    );
    h.settle(SETTLE);
    assert!(red(&h) > before, "out-of-gamut colours are painted red");
    let r = h.request("engine.execute", json!({"command": "view.softProof", "params": {"space": "cmyk"}}), T);
    assert_ne!(r["ok"], true);
    // a device (printer) profile, its intent and Simulate Paper & Ink
    let dir = std::env::temp_dir().join(format!("proof-profile-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let icc = dir.join("printer.icc");
    std::fs::write(&icc, lightcraft_codecs::icc::write_named(lightcraft_codecs::NamedSpace::Srgb)).unwrap();
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
        h.app.services.open_with = Some(Box::new(move |p: &str, editor: &str| {
            assert_eq!(editor, "", "open the model directory with the default application");
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

#[test]
fn masking_drag_handles_reorder_rows_once_and_preserve_selection() {
    fn drag(h: &mut Headless, source: &str, target: &str, after: bool) {
        let rect = h.app.widgets.iter().find(|(id, _)| id == target).unwrap().1;
        let r = h.request(
            "ui.dragWidget",
            json!({"id": source, "toX": rect.center().x, "toY": if after { rect.bottom() - 3.0 } else { rect.top() + 3.0 }, "steps": 12}),
            T,
        );
        assert_eq!(r["ok"], true, "{r}");
    }
    for floating in [false, true] {
        let mut h = detail("panel.masking");
        exec(&mut h, "mask.add", json!({"kind": "linear"}));
        exec(&mut h, "mask.addComponent", json!({"kind": "radial", "op": "subtract"}));
        exec(&mut h, "mask.addComponent", json!({"kind": "brush", "op": "intersect"}));
        exec(&mut h, "ui.maskPanel", json!({"floating": floating}));
        let before = develop(&h);
        let undo = h.app.session.undo.len();
        let menu = h.app.widgets.iter().find(|(id, _)| id == "button:componentMenu2").unwrap().1;
        let grip = h.app.widgets.iter().find(|(id, _)| id == "componentDrag:2").unwrap().1;
        assert!(grip.left() >= menu.right(), "the grip is to the right of the ellipsis");
        drag(&mut h, "componentDrag:2", "component:0", false);
        let mut moved = before.clone();
        moved.masks[0].components.rotate_right(1);
        assert_eq!(develop(&h), moved);
        assert_eq!(h.app.ui.mask_component.map(|(_, _, i)| i), Some(0));
        assert_eq!(h.app.ui.tool, "brush");
        assert_eq!(h.app.session.undo.len(), undo + 1, "one drag, one undo entry");
        exec(&mut h, "edit.undo", json!({}));
        assert_eq!(develop(&h), before);
        assert_eq!(h.app.ui.mask_component.map(|(_, _, i)| i), Some(2));
        exec(&mut h, "edit.redo", json!({}));
        assert_eq!(develop(&h), moved);
        assert_eq!(h.app.ui.mask_component.map(|(_, _, i)| i), Some(0));
        drag(&mut h, "componentDrag:0", "component:2", true);
        assert_eq!(develop(&h), before);
        assert_eq!(h.app.ui.mask_component.map(|(_, _, i)| i), Some(2));
        let undo = h.app.session.undo.len();
        drag(&mut h, "componentDrag:2", "component:2", false);
        assert_eq!(h.app.session.undo.len(), undo, "dropping on itself is a no-op");
        let canvas = h.app.image_rect.unwrap().center();
        h.request("ui.dragWidget", json!({"id": "componentDrag:2", "toX": canvas.x, "toY": canvas.y}), T);
        assert_eq!(develop(&h), before, "outside drop never paints through or moves a component");
        exec(&mut h, "mask.add", json!({"kind": "radial", "name": "Other"}));
        let before = develop(&h);
        let undo = h.app.session.undo.len();
        drag(&mut h, "maskDrag:1", "maskGroup:2", true);
        let mut moved = before.clone();
        moved.masks.swap(0, 1);
        assert_eq!(develop(&h), moved, "mask drag moves the whole group");
        assert_eq!(h.app.session.active_mask, Some(2));
        assert_eq!(h.app.session.undo.len(), undo + 1);
        exec(&mut h, "edit.undo", json!({}));
        assert_eq!(develop(&h), before);
        exec(&mut h, "edit.redo", json!({}));
        assert_eq!(develop(&h), moved);
    }
}

#[test]
fn masking_drag_scroll_cancel_and_stale_drop_preserve_edits() {
    fn rect(h: &Headless, key: &str) -> egui::Rect {
        h.app.widgets.iter().find(|(id, _)| id == key).unwrap().1
    }
    fn event(h: &mut Headless, kind: &str, point: egui::Pos2) {
        let image = h.app.image_rect.unwrap();
        let p = (point - image.min) / image.size();
        pointer(h, json!([{"kind": kind, "x": p.x, "y": p.y}]));
    }
    let mut h = detail("panel.masking");
    exec(&mut h, "mask.add", json!({"kind": "linear"}));
    for _ in 0..8 {
        exec(&mut h, "mask.addComponent", json!({"kind": "brush"}));
    }
    let before = develop(&h);
    let undo = h.app.session.undo.len();
    let source = rect(&h, "componentDrag:0").center();
    let viewport = rect(&h, "maskReorderViewport");
    let edge = egui::pos2(source.x, viewport.bottom() - 3.0);
    event(&mut h, "down", source);
    event(&mut h, "move", edge);
    for _ in 0..100 {
        h.step();
    }
    let last = rect(&h, "component:8");
    assert!(viewport.contains(last.center()), "dragging near the edge scrolls the last row into view");
    assert_eq!(develop(&h), before, "previewing an insertion never changes the document");
    event(&mut h, "move", last.center());
    h.request("ui.key", json!({"key": "Escape"}), T);
    event(&mut h, "up", last.center());
    assert_eq!(develop(&h), before);
    assert_eq!(h.app.session.undo.len(), undo);
    assert_eq!(h.app.ui.view, crate::state::ViewMode::Detail);
    // Grab a visible row, edit the mask through a command, then release at a new position.
    let source = rect(&h, "componentDrag:8").center();
    let target = rect(&h, "component:5").center();
    event(&mut h, "down", source);
    event(&mut h, "move", target);
    exec(&mut h, "mask.component", json!({"component": 8, "action": "rename", "name": "Changed while dragging"}));
    let changed = develop(&h);
    event(&mut h, "up", target);
    assert_eq!(develop(&h), changed, "stale source indices are never applied after an edit");
    assert_eq!(h.app.session.undo.len(), undo + 1);
}

#[test]
fn masking_component_move_menu_keeps_selection_and_undo_targets() {
    for floating in [false, true] {
        let mut h = detail("panel.masking");
        exec(&mut h, "mask.add", json!({"kind": "sky"}));
        exec(&mut h, "mask.addComponent", json!({"kind": "brush", "op": "subtract"}));
        exec(&mut h, "mask.brushSettings", json!({"size": 0.1, "feather": 0, "flow": 100, "density": 100}));
        brush_at(&mut h, 0.5, 0.5);
        exec(&mut h, "mask.addComponent", json!({"kind": "radial", "op": "intersect"}));
        exec(&mut h, "ui.maskPanel", json!({"floating": floating}));
        click(&mut h, "component:1");
        let before = develop(&h);
        assert!(mask_coverage(&h, 0, 50, 50) < 0.01);
        click(&mut h, "button:componentMenu1");
        click(&mut h, "maskComponentMove:1:up");
        let mut moved = before.clone();
        moved.masks[0].components.swap(0, 1);
        assert_eq!(develop(&h), moved);
        assert_eq!(h.app.ui.mask_component.map(|(_, _, i)| i), Some(0));
        assert_eq!(h.app.ui.tool, "brush");
        assert!(mask_coverage(&h, 0, 50, 50) > 0.99, "move changes the actual composition order");
        exec(&mut h, "edit.undo", json!({}));
        assert_eq!(develop(&h), before);
        assert_eq!(h.app.ui.mask_component.map(|(_, _, i)| i), Some(1));
        exec(&mut h, "edit.redo", json!({}));
        assert_eq!(develop(&h), moved);
        assert_eq!(h.app.ui.mask_component.map(|(_, _, i)| i), Some(0));
        brush_at(&mut h, 0.25, 0.5);
        let painted = develop(&h);
        let MaskShape::Brush { strokes } = &painted.masks[0].components[0].shape else { panic!("brush") };
        assert_eq!(strokes.len(), 2, "canvas edits follow the moved component");
        assert_eq!(painted.masks[0].components[1..], moved.masks[0].components[1..]);
        exec(&mut h, "edit.undo", json!({}));
        click(&mut h, "button:componentMenu0");
        click(&mut h, "maskComponentMove:0:up");
        assert_eq!(develop(&h), moved, "Move Up is disabled at the top");
        h.request("ui.key", json!({"key": "Escape"}), T);
        for from in [0, 1] {
            click(&mut h, &format!("button:componentMenu{from}"));
            click(&mut h, &format!("maskComponentMove:{from}:down"));
            assert_eq!(h.app.ui.mask_component.map(|(_, _, i)| i), Some(from + 1));
            assert_eq!(h.app.ui.tool, "brush");
        }
        let bottom = develop(&h);
        click(&mut h, "button:componentMenu2");
        click(&mut h, "maskComponentMove:2:down");
        assert_eq!(develop(&h), bottom, "Move Down is disabled at the bottom");
        h.request("ui.key", json!({"key": "Escape"}), T);
        // Moving another component across the selection preserves the selected brush.
        exec(&mut h, "mask.component", json!({"component": 1, "action": "move", "delta": 1}));
        assert_eq!(h.app.ui.mask_component.map(|(_, _, i)| i), Some(1));
        assert_eq!(h.app.ui.tool, "brush");
        h.settle(SETTLE);
    }
}

#[test]
fn masking_delete_keys_respect_component_and_mask_selection() {
    for floating in [false, true] {
        for key in ["Backspace", "Delete"] {
            let mut h = detail("panel.masking");
            exec(&mut h, "mask.add", json!({"kind": "radial", "name": "Keep"}));
            exec(&mut h, "mask.add", json!({"kind": "linear", "name": "Sky"}));
            exec(&mut h, "mask.addComponent", json!({"kind": "brush", "op": "subtract"}));
            exec(&mut h, "mask.addComponent", json!({"kind": "radial", "op": "intersect"}));
            exec(&mut h, "ui.maskPanel", json!({"floating": floating}));
            let before = develop(&h);
            // Middle, first and last components must each be independently deletable.
            for component in [1, 0, 2] {
                click(&mut h, &format!("component:{component}"));
                let r = h.request("ui.key", json!({"key": key}), T);
                assert_eq!(r["ok"], true, "{r}");
                let mut expected = before.clone();
                expected.masks[1].components.remove(component);
                assert_eq!(develop(&h), expected, "{key}, floating={floating}: delete only component {component}");
                assert_eq!(h.app.session.active_mask, Some(2));
                exec(&mut h, "edit.undo", json!({}));
                assert_eq!(develop(&h), before);
                exec(&mut h, "edit.redo", json!({}));
                assert_eq!(develop(&h), expected);
                exec(&mut h, "edit.undo", json!({}));
            }
            click(&mut h, "mask:2");
            assert!(h.app.ui.mask_component.is_none(), "clicking the parent selects the whole mask");
            h.request("ui.key", json!({"key": key}), T);
            assert_eq!(develop(&h).masks, vec![before.masks[0].clone()]);
            assert!(h.app.ui.mask_component.is_none(), "whole-mask deletion keeps the remaining mask selected as a whole");
            exec(&mut h, "edit.undo", json!({}));
            assert_eq!(develop(&h), before);
            exec(&mut h, "mask.select", json!({"id": 1}));
            click(&mut h, "component:0");
            h.request("ui.key", json!({"key": key}), T);
            assert_eq!(develop(&h).masks, vec![before.masks[1].clone()], "deleting a sole component removes only its empty mask");
            exec(&mut h, "edit.undo", json!({}));
            assert_eq!(develop(&h), before);
            exec(&mut h, "mask.select", json!({"id": null}));
            h.request("ui.key", json!({"key": key}), T);
            assert_eq!(develop(&h), before);
            let photo = h.app.session.active().unwrap();
            assert!(!h.app.session.catalog.photo(photo).unwrap().deleted, "retouching never deletes the photo");
            h.settle(SETTLE);
        }
    }
}

#[test]
fn masking_backspace_edits_a_component_name_without_deleting_it() {
    let mut h = detail("panel.masking");
    exec(&mut h, "mask.add", json!({"kind": "linear"}));
    exec(&mut h, "mask.addComponent", json!({"kind": "brush", "op": "subtract"}));
    let before = develop(&h);
    let r = h.request("ui.clickWidget", json!({"id": "component:1", "count": 2}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.app.ui.renaming_component.is_some());
    h.request("ui.key", json!({"key": "End"}), T);
    h.request("ui.key", json!({"key": "Backspace"}), T);
    assert_eq!(develop(&h), before, "the text field owns Backspace");
    h.request("ui.key", json!({"key": "Escape"}), T);
    assert_eq!(develop(&h), before, "cancelling the rename preserves all components");
    h.settle(SETTLE);
}

#[test]
fn masking_component_selection_routes_brushes_and_settings_without_editing_the_photo() {
    let mut h = detail("panel.masking");
    exec(&mut h, "mask.add", json!({"kind": "brush"}));
    exec(&mut h, "mask.addComponent", json!({"kind": "brush", "op": "subtract"}));
    click(&mut h, "component:0");
    assert_eq!(h.app.ui.mask_component.map(|(_, _, i)| i), Some(0));
    assert_eq!(h.app.ui.tool, "brush");
    let before = develop(&h);
    exec(&mut h, "mask.brushSettings", json!({"density": 38, "feather": 0, "flow": 100}));
    h.request("ui.set", json!({"leftPanel": false}), T);
    assert_eq!(h.app.ui.mask_component.map(|(_, _, i)| i), Some(0), "ui.set retains component selection");
    for (command, params) in [
        ("mask.brushSettings", json!({"size": 0.05, "density": 101})),
        ("mask.selectComponent", json!({"id": 1, "component": 9})),
        ("ui.maskPanel", json!({"floating": "yes"})),
    ] {
        let state = serde_json::to_value(&h.app.ui).unwrap();
        let selected = h.app.ui.mask_component;
        assert!(h.app.run(command, params).is_err());
        assert_eq!(serde_json::to_value(&h.app.ui).unwrap(), state);
        assert_eq!(h.app.ui.mask_component, selected);
    }
    assert_eq!(develop(&h), before);
    brush_at(&mut h, 0.4, 0.5);
    let after = develop(&h);
    let MaskShape::Brush { strokes } = &after.masks[0].components[0].shape else { panic!("brush") };
    assert_eq!(strokes.len(), 1);
    assert_eq!(strokes[0].density, 38.0);
    assert_eq!(after.masks[0].components[1], before.masks[0].components[1]);
    exec(&mut h, "edit.undo", json!({}));
    assert_eq!(develop(&h), before);
    exec(&mut h, "edit.redo", json!({}));
    assert_eq!(develop(&h), after);
    exec(&mut h, "mask.addComponent", json!({"kind": "luminanceRange", "op": "intersect"}));
    assert!(h.app.widgets.iter().any(|(id, _)| id == "lumRange:2"));
    assert!(!h.app.widgets.iter().any(|(id, _)| id == "slider:ui.brushDensity"));
    click(&mut h, "component:0");
    assert!(!h.app.widgets.iter().any(|(id, _)| id == "lumRange:2"));
    assert!(h.app.widgets.iter().any(|(id, _)| id == "slider:ui.brushDensity"));
    // Deleting an earlier component keeps the selected logical component.
    exec(&mut h, "mask.selectComponent", json!({"component": 2}));
    exec(&mut h, "mask.component", json!({"component": 0, "action": "delete"}));
    assert_eq!(h.app.ui.mask_component.map(|(_, _, i)| i), Some(1));
    assert!(h.app.widgets.iter().any(|(id, _)| id == "lumRange:1"));
    h.settle(SETTLE);
}

#[test]
fn masking_overlay_mode_menu_stays_open_and_selects_from_both_footers() {
    use lightcraft_pipeline::MaskView;
    let mut h = detail("panel.masking");
    exec(&mut h, "mask.add", json!({"kind": "brush"}));
    let before = develop(&h);
    let has = |h: &Headless, key: &str| h.app.widgets.iter().any(|(id, _)| id == key);
    for floating in [false, true] {
        exec(&mut h, "ui.maskPanel", json!({"floating": floating}));
        for launcher in ["icon:maskOverlayOptions", "maskOverlaySwatch"] {
            click(&mut h, launcher);
            for mode in MaskView::ALL {
                click(&mut h, "dropdown:maskOverlayMode");
                for _ in 0..4 {
                    h.step();
                }
                assert!(has(&h, "dropdown:maskOverlayMode"), "opening the mode menu must preserve its parent");
                for option in MaskView::ALL {
                    assert!(has(&h, &format!("maskOverlayMode:{}", option.name())), "all modes remain available");
                }
                click(&mut h, &format!("maskOverlayMode:{}", mode.name()));
                h.step();
                assert_eq!(h.app.ui.mask_overlay_mode, mode.name());
                assert!(has(&h, "dropdown:maskOverlayMode"), "selection keeps overlay settings open");
            }
            exec(&mut h, "view.maskOverlayMode", json!({"mode": "color"}));
            exec(&mut h, "view.maskOverlayColor", json!({"color": [230, 30, 40]}));
            click(&mut h, "maskOverlayCustomColor");
            for _ in 0..4 {
                h.step();
            }
            assert!(has(&h, "maskOverlayColorPicker"), "custom colour picker also preserves its parent");
            let r = h.request("ui.clickWidget", json!({"id": "maskOverlayColorPicker", "fx": 0.4, "fy": 0.4}), T);
            assert_eq!(r["ok"], true, "{r}");
            assert_ne!(h.app.ui.mask_overlay_color, [230, 30, 40]);
            assert!(has(&h, "maskOverlayColorPicker"), "colour editing keeps the picker open");
            assert!(has(&h, "dropdown:maskOverlayMode"));
            h.request("ui.key", json!({"key": "Escape"}), T);
            assert!(!has(&h, "dropdown:maskOverlayMode"));
            assert_eq!(h.app.ui.view, crate::state::ViewMode::Detail, "Escape dismisses the popup, not the photo");
            click(&mut h, launcher);
            click(&mut h, "dropdown:maskOverlayMode");
            h.step();
            click(&mut h, "label:maskAdjustments");
            h.step();
            assert!(!has(&h, "dropdown:maskOverlayMode"), "outside click dismisses both menus");
            assert_eq!(develop(&h), before, "overlay controls do not paint through or edit the photo");
        }
    }
}

#[test]
fn masking_list_floats_moves_docks_and_preserves_overlay_preferences_without_photo_edits() {
    let mut h = detail("panel.masking");
    let has = |h: &Headless, key: &str| h.app.widgets.iter().any(|(id, _)| id == key);
    let rect = |h: &Headless, key: &str| h.app.widgets.iter().find(|(id, _)| id == key).unwrap().1;
    assert!(!h.app.ui.mask_panel_floating);
    click(&mut h, "maskNew:brush");
    assert!(has(&h, "maskList:embedded"));
    assert!(!has(&h, "maskList:floating"));
    let before = develop(&h);
    click(&mut h, "icon:maskPanelArrange");
    h.step();
    h.step();
    assert!(has(&h, "maskList:floating"));
    assert!(!has(&h, "maskList:embedded"));
    let initial = rect(&h, "panel:maskFloating");
    for _ in 0..100 {
        h.step();
    }
    let window = rect(&h, "panel:maskFloating");
    assert!((initial.height() - window.height()).abs() < 1.0, "idle frames must not grow the window: {initial:?} -> {window:?}");
    let right = rect(&h, "panel:right_panel");
    assert!(window.right() <= right.left() + 1.0, "{window:?} {right:?}");
    assert!(window.contains_rect(rect(&h, "component:0")));
    assert!(right.contains_rect(rect(&h, "slider:ui.brushDensity")));
    // Clicking controls above the canvas must never start a brush stroke underneath.
    click(&mut h, "component:0");
    click(&mut h, "icon:maskOverlay");
    assert!(!h.app.ui.mask_overlay);
    click(&mut h, "icon:maskOverlay");
    click(&mut h, "maskOverlaySwatch");
    click(&mut h, "maskOverlayColor:28c846");
    let r = h.request("ui.clickWidget", json!({"id": "slider:ui.maskOverlayOpacity", "fx": 0.75}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(h.app.ui.mask_overlay_color, [40, 200, 70]);
    assert!(h.app.ui.mask_overlay_opacity > 60.0);
    h.request("ui.key", json!({"key": "Escape"}), T);
    let title = rect(&h, "maskPanelTitle").center();
    let img = h.app.image_rect.unwrap();
    let from = (title - img.min) / img.size();
    let to = (title + egui::vec2(-90.0, 60.0) - img.min) / img.size();
    let r = h.request(
        "ui.pointer",
        json!({"events": [
            {"kind": "down", "x": from.x, "y": from.y},
            {"kind": "move", "x": to.x, "y": to.y},
            {"kind": "up", "x": to.x, "y": to.y}
        ]}),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    let moved = rect(&h, "panel:maskFloating");
    assert!((moved.left() - window.left() + 90.0).abs() < 3.0, "{window:?} -> {moved:?}");
    assert_eq!(develop(&h), before, "list interactions never paint through the floating window");
    let saved = serde_json::to_value(&h.app.ui).unwrap();
    let restored: crate::UiState = serde_json::from_value(saved).unwrap();
    assert!(restored.mask_panel_floating);
    assert_eq!(restored.mask_panel_rect, h.app.ui.mask_panel_rect);
    assert_eq!(restored.mask_overlay_color, h.app.ui.mask_overlay_color);
    assert_eq!(restored.mask_overlay_opacity, h.app.ui.mask_overlay_opacity);
    assert_restored_mask_panel_placement(restored, moved);
    click(&mut h, "button:maskPanelDock");
    assert!(has(&h, "maskList:embedded"));
    assert!(!has(&h, "maskList:floating"));
    exec(&mut h, "edit.undo", json!({}));
    assert!(develop(&h).masks.is_empty(), "panel and overlay changes do not consume photo undo");
    exec(&mut h, "ui.maskPanel", json!({"floating": true}));
    assert!(has(&h, "maskNew:brush"), "first mask choices remain embedded even with the float preference");
    assert!(!has(&h, "maskList:floating"));
    assert_crowded_mask_panel_stays_bounded(&mut h);
}

fn assert_crowded_mask_panel_stays_bounded(h: &mut Headless) {
    let rect = |h: &Headless, key: &str| h.app.widgets.iter().find(|(id, _)| id == key).unwrap().1;
    for _ in 0..12 {
        exec(h, "mask.add", json!({"kind": "radial"}));
    }
    let crowded = rect(h, "panel:maskFloating");
    for _ in 0..100 {
        h.step();
    }
    let stable = rect(h, "panel:maskFloating");
    assert!((crowded.height() - stable.height()).abs() < 1.0, "scrolling list remains bounded: {crowded:?} -> {stable:?}");
    assert!(stable.contains_rect(rect(h, "icon:maskOverlay")), "overlay controls remain outside the list scroll area");
    h.settle(SETTLE);
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

#[test]
fn adding_radial_component_disarms_color_range_picker() {
    let mut h = detail("panel.masking");
    exec(&mut h, "mask.add", json!({"kind": "colorRange"}));
    h.app.ui.tool.clear();
    click(&mut h, "button:colorPick0");
    brush_at(&mut h, 0.25, 0.5);
    assert_eq!(color_samples(&h).as_array().map(Vec::len), Some(1));

    exec(&mut h, "mask.addComponent", json!({"kind": "radial", "op": "intersect", "center": [0.5, 0.5], "rx": 0.22, "ry": 0.22}));
    assert_eq!(h.app.ui.tool, "radial", "the new radial selection replaces the colour picker");
    assert_eq!(crate::panels::masking::selected_component(&h.app), Some((1, 1)));

    let widget = "maskHandle:1:1:1";
    let rect = h.app.widgets.iter().find(|(id, _)| *id == widget).unwrap().1;
    let image = h.app.image_rect.unwrap();
    let q = rect.center();
    let (x, y) = ((q.x - image.left()) / image.width(), (q.y - image.top()) / image.height());
    pointer(&mut h, json!([{"kind": "down", "x": x, "y": y}, {"kind": "drag", "x": x + 0.05, "y": y}, {"kind": "up", "x": x + 0.05, "y": y}]));
    let MaskShape::Radial { rx, .. } = develop(&h).masks[0].components[1].shape else { panic!("radial") };
    assert!(rx > 0.22, "dragging a radial handle resizes the component: {rx}");
    assert_eq!(color_samples(&h).as_array().map(Vec::len), Some(1), "radial dragging adds no colour samples");
}

fn assert_restored_mask_panel_placement(restored: crate::UiState, moved: egui::Rect) {
    let rect = |h: &Headless, key: &str| h.app.widgets.iter().find(|(id, _)| id == key).unwrap().1;
    // A fresh egui context restores the saved placement without frame-size growth.
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    app.ui = restored;
    app.run("mask.add", json!({"kind": "brush"})).unwrap();
    let mut reopened = Headless::new(app, [1200.0, 800.0], 1.0);
    for _ in 0..5 {
        reopened.step();
    }
    let back = rect(&reopened, "panel:maskFloating");
    assert!((back.left() - moved.left()).abs() < 2.0 && (back.top() - moved.top()).abs() < 2.0, "{moved:?} -> {back:?}");
    assert!((back.width() - moved.width()).abs() < 2.0 && (back.height() - moved.height()).abs() < 2.0, "{moved:?} -> {back:?}");
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
    let before = lightcraft_develop::controls::get(&develop(&h), "light.exposure").expect("exposure");

    // A wheel event over the selected control adjusts exposure instead of
    // scrolling the right sidebar (an unselected slider leaves scrolling alone).
    // (120 points is more than one notch, `theme::WHEEL_LINE_POINTS`; egui smooths it over a few frames)
    let result = h.request("ui.scroll", json!({"dy": 120.0}), T);
    assert_eq!(result["ok"], true, "{result}");
    for _ in 0..10 {
        h.step();
    }
    let after = lightcraft_develop::controls::get(&develop(&h), "light.exposure").expect("exposure");
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
    let exposure = |h: &Headless| lightcraft_develop::controls::get(&develop(h), "light.exposure").expect("exposure");
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

/// What the photo shows along the centreline of [`hold_fast_stroke`]'s stroke at height `y`, away
/// from the cursor ring at its end.
fn stroke_centreline(h: &Headless, y: f32) -> Vec<[f32; 3]> {
    let im = h.paint();
    let img = h.app.image_rect.expect("the photo is on screen");
    let ppp = h.pixels_per_point;
    (1..=74)
        .map(|i| {
            let x = 0.17 + (0.85 - 0.17) * i as f32 / 80.0;
            let (sx, sy) = (((img.left() + x * img.width()) * ppp) as usize, ((img.top() + y * img.height()) * ppp) as usize);
            let c = im.pixels[sy * im.size[0] + sx];
            [c.r() as f32, c.g() as f32, c.b() as f32]
        })
        .collect()
}

/// The fast stroke of the #517 test at height `y`, with the button still held: its centreline
/// before painting and while painting.
fn hold_fast_stroke(h: &mut Headless, y: f32) -> (Vec<[f32; 3]>, Vec<[f32; 3]>) {
    pointer(h, json!([{"kind": "move", "x": 0.02, "y": 0.02}]));
    h.settle(SETTLE);
    let photo = stroke_centreline(h, y);
    pointer(
        h,
        json!([{"kind": "down", "x": 0.15, "y": y}, {"kind": "drag", "x": 0.17, "y": y}, {"kind": "drag", "x": 0.5, "y": y}, {"kind": "drag", "x": 0.85, "y": y}]),
    );
    assert!(matches!(h.app.gesture, Some(crate::panels::detail::Gesture::Brush { .. })), "still painting");
    (photo, stroke_centreline(h, y))
}

/// The centreline points (photo, held) where the stroke being painted changes the photo.
fn tinted(photo: &[[f32; 3]], held: &[[f32; 3]]) -> Vec<([f32; 3], [f32; 3])> {
    photo.iter().zip(held).filter(|(p, l)| (0..3).any(|k| (p[k] - l[k]).abs() > 6.0)).map(|(p, l)| (*p, *l)).collect()
}

/// Issue #2549: with the mask overlay hidden (a mask slider hid it, or O) or a colour overlay at
/// opacity 0, the stroke being painted doesn't tint the photo either, so dodging and burning is
/// judged on the photo; the effect shows once the stroke is committed. The all-masks overlay still
/// shows it, as it will the committed stroke.
#[test]
fn a_brush_stroke_is_not_tinted_while_the_mask_overlay_is_hidden() {
    let mut h = detail("panel.masking");
    h.settle(SETTLE);
    exec(&mut h, "tool.brush", json!({"new": true}));
    let r = h.request("ui.set", json!({"brushSize": 0.03, "brushFeather": 50.0, "brushFlow": 100.0}), T);
    assert_eq!(r["ok"], true, "{r}");
    // dodging: brighten the new, still empty mask first (which hides the overlay), then paint
    exec(&mut h, "mask.adjust", json!({"values": {"exposure": 1.0}}));
    assert!(!h.app.ui.mask_overlay, "a mask slider hides the overlay");
    let (photo, held) = hold_fast_stroke(&mut h, 0.3);
    let t = tinted(&photo, &held);
    assert!(t.is_empty(), "the hidden overlay tints the stroke being painted: {} of 74 points, first {:?}", t.len(), t.first());
    assert!(h.app.brush_live.is_none(), "nothing is drawn over the photo while painting");
    pointer(&mut h, json!([{"kind": "up", "x": 0.85, "y": 0.3}]));
    let d = develop(&h);
    let MaskShape::Brush { strokes } = &d.masks[0].components[0].shape else { panic!("brush") };
    assert!(strokes[0].points.len() >= 3, "{strokes:?}");
    assert!(!h.app.ui.mask_overlay, "painting leaves the overlay hidden");
    // committed: the stroke brightens the photo
    h.settle(SETTLE);
    let luma = |c: &[[f32; 3]]| c.iter().map(|p| p[0] + p[1] + p[2]).sum::<f32>() / c.len() as f32;
    let done = stroke_centreline(&h, 0.3);
    assert!(luma(&done) > luma(&photo) + 30.0, "committed {} vs before {}", luma(&done), luma(&photo));

    // O shows the overlay again, but a colour overlay at opacity 0 shows nothing of the committed
    // strokes, nor of the one being painted
    let r = h.request("ui.key", json!({"key": "o"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.app.ui.mask_overlay, "O shows the overlay");
    exec(&mut h, "view.maskOverlayColor", json!({"opacity": 0}));
    let (photo, held) = hold_fast_stroke(&mut h, 0.45);
    let t = tinted(&photo, &held);
    assert!(t.is_empty(), "an overlay at opacity 0 tints the stroke being painted: {} of 74 points, first {:?}", t.len(), t.first());
    pointer(&mut h, json!([{"kind": "up", "x": 0.85, "y": 0.45}]));

    // O hides it again, at its usual opacity
    exec(&mut h, "view.maskOverlayColor", json!({"opacity": 50}));
    let r = h.request("ui.key", json!({"key": "o"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(!h.app.ui.mask_overlay, "O hides the overlay");
    let (photo, held) = hold_fast_stroke(&mut h, 0.6);
    let t = tinted(&photo, &held);
    assert!(t.is_empty(), "the overlay hidden with O tints the stroke being painted: {} of 74 points, first {:?}", t.len(), t.first());
    pointer(&mut h, json!([{"kind": "up", "x": 0.85, "y": 0.6}]));

    // the all-masks overlay shows every mask's region, so the stroke being painted is tinted as it
    // will be once committed
    exec(&mut h, "view.maskOverlayAll", json!({}));
    assert!(h.app.ui.mask_overlay_all && !h.app.ui.mask_overlay);
    let (photo, held) = hold_fast_stroke(&mut h, 0.75);
    let [r0, g0, b0] = h.app.ui.mask_overlay_color.map(|v| v as f32);
    let op = h.app.ui.mask_overlay_opacity / 100.0;
    for (p, got) in photo.iter().zip(&held) {
        let want = [p[0] + (r0 - p[0]) * op, p[1] + (g0 - p[1]) * op, p[2] + (b0 - p[2]) * op];
        assert!((0..3).all(|k| (got[k] - want[k]).abs() <= 12.0), "the all-masks overlay tints the stroke: {got:?} vs {want:?}");
    }
    pointer(&mut h, json!([{"kind": "up", "x": 0.85, "y": 0.75}]));
    assert_eq!(develop(&h).masks.len(), 1, "every stroke went into the one mask");

    // a hidden mask isn't in the all-masks overlay: nor is the stroke being painted into it
    let id = develop(&h).masks[0].id;
    exec(&mut h, "mask.visible", json!({"id": id, "visible": false}));
    let (photo, held) = hold_fast_stroke(&mut h, 0.9);
    let t = tinted(&photo, &held);
    assert!(t.is_empty(), "the all-masks overlay tints a stroke into a hidden mask: {} of 74 points, first {:?}", t.len(), t.first());
    pointer(&mut h, json!([{"kind": "up", "x": 0.85, "y": 0.9}]));
}

#[test]
fn effect_dropdown_applies_saves_and_shows_the_matching_effect() {
    let mut h = detail("panel.masking");
    exec(&mut h, "mask.add", json!({"kind": "radial"}));
    exec(&mut h, "mask.adjust", json!({"values": {"exposure": 0.7, "shadows": 25, "amount": 80}}));
    h.step();
    // save the mask's sliders from the menu (the New Effect Preset dialog)
    click(&mut h, "dropdown:maskEffect");
    click(&mut h, "maskEffectMenu:save");
    let mut dlg = h.app.ui.dialog.take().expect("the save dialog opens");
    match &mut dlg {
        crate::state::Dialog::SaveEffect { name, .. } => *name = "Lift".into(),
        d => panic!("the save dialog opens: {d:?}"),
    }
    let r = crate::panels::dialogs::confirm_dialog(&mut h.app, &dlg);
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(h.app.session.mask_effects.len(), 1);
    // change the sliders, then pick the effect again: sliders back, Amount kept
    exec(&mut h, "mask.adjust", json!({"values": {"exposure": -1, "clarity": 40, "amount": 50}}));
    h.step();
    click(&mut h, "dropdown:maskEffect");
    click(&mut h, "maskEffect:Lift");
    let a = develop(&h).masks.last().expect("a mask").adjust;
    assert_eq!((a.exposure, a.shadows, a.clarity, a.amount), (0.7, 25.0, 0.0, 50.0));
    assert_eq!(exec(&mut h, "mask.effects", json!({}))["current"], "Lift");
}

#[test]
fn masking_review_history_preserves_surviving_component_after_insert_and_delete() {
    for floating in [false, true] {
        let mut h = detail("panel.masking");
        exec(&mut h, "mask.add", json!({"kind": "linear"}));
        exec(&mut h, "mask.addComponent", json!({"kind": "radial"}));
        exec(&mut h, "mask.addComponent", json!({"kind": "brush"}));
        exec(&mut h, "ui.maskPanel", json!({"floating": floating}));
        let original = develop(&h);
        // Use explicit selection to avoid repeated row clicks entering double-click rename.
        // Delete an earlier, unselected row through the same command as its component menu.
        for removed in [0, 1] {
            exec(&mut h, "mask.selectComponent", json!({"component": 2}));
            exec(&mut h, "mask.component", json!({"component": removed, "action": "delete"}));
            assert_eq!(crate::panels::masking::selected_component(&h.app), Some((1, 1)));
            exec(&mut h, "edit.undo", json!({}));
            assert_eq!(develop(&h), original);
            assert_eq!(crate::panels::masking::selected_component(&h.app), Some((1, 2)), "the same brush survives undo");
            assert_eq!(h.app.ui.tool, "brush");
            exec(&mut h, "edit.redo", json!({}));
            assert_eq!(crate::panels::masking::selected_component(&h.app), Some((1, 1)));
            exec(&mut h, "edit.undo", json!({}));
        }
        exec(&mut h, "mask.component", json!({"component": 0, "action": "duplicate"}));
        exec(&mut h, "mask.selectComponent", json!({"component": 3}));
        exec(&mut h, "edit.undo", json!({}));
        assert_eq!(develop(&h), original);
        assert_eq!(crate::panels::masking::selected_component(&h.app), Some((1, 2)));
        exec(&mut h, "edit.redo", json!({}));
        assert_eq!(crate::panels::masking::selected_component(&h.app), Some((1, 3)));
        exec(&mut h, "edit.undo", json!({}));
        let response = h.request("ui.key", json!({"key": "Backspace"}), T);
        assert_eq!(response["ok"], true);
        assert_eq!(develop(&h).masks[0].components, original.masks[0].components[..2], "Delete still targets C, not B");
        h.settle(SETTLE);
    }
}

#[test]
fn masking_review_brush_shortcut_synchronizes_controls_painting_and_delete() {
    for floating in [false, true] {
        let mut h = detail("panel.masking");
        exec(&mut h, "mask.add", json!({"kind": "brush"}));
        exec(&mut h, "mask.addComponent", json!({"kind": "brush", "op": "subtract"}));
        exec(&mut h, "mask.addComponent", json!({"kind": "radial"}));
        exec(&mut h, "ui.maskPanel", json!({"floating": floating}));
        let before = develop(&h);
        let undo = h.app.session.undo.len();
        let response = h.request("ui.key", json!({"key": "B"}), T);
        assert_eq!(response["ok"], true);
        assert_eq!(h.app.ui.tool, "brush");
        assert_eq!(crate::panels::masking::selected_component(&h.app), Some((1, 1)));
        assert!(h.app.widgets.iter().any(|(id, _)| id == "slider:ui.brushSize"));
        assert_eq!(develop(&h), before, "activating an existing brush is not a photo edit");
        assert_eq!(h.app.session.undo.len(), undo);
        brush_at(&mut h, 0.25, 0.3);
        let painted = develop(&h);
        assert_eq!(painted.masks[0].components[0], before.masks[0].components[0]);
        assert_eq!(painted.masks[0].components[2], before.masks[0].components[2]);
        let MaskShape::Brush { strokes } = &painted.masks[0].components[1].shape else { panic!("brush") };
        assert_eq!(strokes.len(), 1);
        // An explicitly selected earlier brush wins over the fallback to the last brush.
        click(&mut h, "component:0");
        exec(&mut h, "tool.brush", json!({}));
        assert_eq!(crate::panels::masking::selected_component(&h.app), Some((1, 0)));
        click(&mut h, "component:2");
        h.request("ui.key", json!({"key": "B"}), T);
        h.request("ui.key", json!({"key": "Backspace"}), T);
        assert_eq!(develop(&h).masks[0].components, vec![before.masks[0].components[0].clone(), before.masks[0].components[2].clone()]);
        h.settle(SETTLE);
    }
}

#[test]
fn masking_review_pending_brush_selects_its_first_stroke_without_an_extra_edit() {
    for existing_mask in [false, true] {
        let mut h = detail("panel.masking");
        if existing_mask {
            exec(&mut h, "mask.add", json!({"kind": "radial"}));
        }
        let before = develop(&h);
        let undo = h.app.session.undo.len();
        exec(&mut h, "tool.brush", json!({}));
        assert_eq!(develop(&h), before);
        assert_eq!(h.app.session.undo.len(), undo);
        assert_eq!(crate::panels::masking::selected_component(&h.app), None, "a pending brush must not retain a radial selection");
        assert!(h.app.widgets.iter().any(|(id, _)| id == "slider:ui.brushSize"));
        brush_at(&mut h, 0.25, 0.3);
        let index = usize::from(existing_mask);
        assert_eq!(crate::panels::masking::selected_component(&h.app), Some((1, index)));
        assert_eq!(h.app.session.undo.len(), undo + 1, "the first stroke and its brush are one undo step");
        let painted = develop(&h);
        assert!(matches!(painted.masks[0].components[index].shape, MaskShape::Brush { .. }));
        exec(&mut h, "edit.undo", json!({}));
        assert_eq!(develop(&h), before);
        exec(&mut h, "edit.redo", json!({}));
        assert_eq!(develop(&h), painted);
        h.settle(SETTLE);
    }
}

/// A retained component selection follows history even while another panel owns the tools.
#[test]
fn masking_review_hidden_panel_history_keeps_delete_target() {
    let mut h = detail("panel.masking");
    exec(&mut h, "mask.add", json!({"kind":"linear"}));
    exec(&mut h, "mask.addComponent", json!({"kind":"radial"}));
    exec(&mut h, "mask.addComponent", json!({"kind":"brush"}));
    let original = develop(&h);
    exec(&mut h, "mask.component", json!({"component":1,"action":"delete"}));
    exec(&mut h, "panel.edit", json!({}));
    for (command, index) in [("edit.undo", 2), ("edit.redo", 1), ("edit.undo", 2)] {
        exec(&mut h, command, json!({}));
        assert_eq!(crate::panels::masking::selected_component(&h.app), Some((1, index)));
        assert!(h.app.ui.tool.is_empty(), "history must not arm the brush in Edit");
    }
    exec(&mut h, "panel.masking", json!({}));
    h.request("ui.key", json!({"key":"Backspace"}), T);
    assert_eq!(develop(&h).masks[0].components, original.masks[0].components[..2]);
}

/// A queued description hasn't created a row; a pending Object already has one.
#[test]
fn masking_review_pending_ai_creation_waits_for_a_real_component() {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
    app.ui.right = RightPanel::Masking;
    app.run("mask.add", json!({"kind":"radial"})).unwrap();
    app.run("mask.addComponent", json!({"kind":"brush"})).unwrap();
    app.run("mask.selectComponent", json!({"component":0})).unwrap();
    let selected = app.ui.mask_component;
    for command in ["mask.add", "mask.addComponent"] {
        // Worker-independent delivery of the documented queued Describe response.
        crate::panels::masking::selection_changed(&mut app, command, &json!({"kind":"prompt"}), &json!({"pending":true}), None);
        assert_eq!(app.ui.mask_component, selected);
        assert_eq!(app.ui.tool, "radial");
    }
    let photo = app.session.active().unwrap();
    // Use stored coverage to create an Object without a model, then deliver the same
    // pending response as a worker-backed Object that already created its placeholder.
    let seg = lightcraft_develop::SegMask::from_logits(2, &[8.0, -8.0, 8.0, -8.0]);
    let mut result = app.session.execute("mask.addComponent", &json!({"kind":"object","seg":seg})).unwrap();
    result["pending"] = json!(true);
    crate::panels::masking::selection_changed(&mut app, "mask.addComponent", &json!({"kind":"object"}), &result, None);
    assert_eq!(app.ui.mask_component, Some((photo, 1, 2)));
    assert_eq!(app.ui.tool, "object");
    // A completed Describe uses stored segmentation and then selects the created row.
    app.session.execute("mask.addComponent", &json!({"kind":"prompt","text":"tree","seg":seg})).unwrap();
    crate::panels::masking::selection_completed(&mut app, photo, 1, 3);
    assert_eq!(app.ui.mask_component, Some((photo, 1, 3)));
    assert!(app.ui.tool.is_empty());
    app.run("mask.add", json!({"kind":"radial"})).unwrap();
    let selected = app.ui.mask_component;
    crate::panels::masking::selection_completed(&mut app, photo, 1, 3);
    assert_eq!(app.ui.mask_component, selected, "an unrelated completion cannot select another mask");
}

/// Report write failures while following the edits actually applied in memory.
#[test]
fn masking_review_unsaved_edits_keep_component_selection_in_sync() {
    use lightcraft_catalog::{MemStore, Store};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    #[derive(Clone, Default)]
    struct FailingAppend {
        files: MemStore,
        fail: Arc<AtomicBool>,
    }
    impl Store for FailingAppend {
        fn read(&mut self, name: &str) -> std::io::Result<Option<Vec<u8>>> {
            self.files.read(name)
        }
        fn write_atomic(&mut self, name: &str, data: &[u8]) -> std::io::Result<()> {
            self.files.write_atomic(name, data)
        }
        fn append(&mut self, name: &str, data: &[u8]) -> std::io::Result<()> {
            if self.fail.load(Ordering::SeqCst) {
                return Err(std::io::Error::other("disk full"));
            }
            self.files.append(name, data)
        }
        fn truncate(&mut self, name: &str, len: u64) -> std::io::Result<()> {
            self.files.truncate(name, len)
        }
        fn describe(&self) -> String {
            "mask review failing journal".into()
        }
    }
    let store = FailingAppend::default();
    let mut session = lightcraft_engine::Session::new();
    session
        .open_library_in(
            lightcraft_engine::library::LibraryStores {
                dir: "mask-review".into(),
                catalog: Box::new(store.clone()),
                files: Box::new(MemStore::new()),
                on_disk: false,
            },
            true,
        )
        .unwrap();
    let mut app = LightcraftApp::new(session, Services::default());
    app.ui.right = RightPanel::Masking;
    for kind in ["linear", "radial", "brush"] {
        let command = if kind == "linear" { "mask.add" } else { "mask.addComponent" };
        app.run(command, json!({"kind":kind})).unwrap();
    }
    store.fail.store(true, Ordering::SeqCst);
    for (command, params, index) in [
        ("mask.component", json!({"component":1,"action":"delete"}), 1),
        ("edit.undo", json!({}), 2),
        ("edit.redo", json!({}), 1),
        ("mask.component", json!({"component":1,"action":"move","to":0}), 0),
        ("mask.component", json!({"component":0,"action":"move","delta":1}), 1),
        ("mask.component", json!({"component":0,"action":"duplicate"}), 1),
        ("mask.addComponent", json!({"kind":"radial"}), 3),
    ] {
        let e = app.run_typed(command, params).unwrap_err();
        assert_eq!(e.kind, lightcraft_engine::ErrorKind::NotSaved);
        assert_eq!(crate::panels::masking::selected_component(&app), Some((1, index)), "{command}");
    }
    let before = app.ui.mask_component;
    let e = app.run_typed("mask.component", json!({"component":99,"action":"delete"})).unwrap_err();
    assert_ne!(e.kind, lightcraft_engine::ErrorKind::NotSaved);
    assert_eq!(app.ui.mask_component, before, "rejected edits cannot change selection");
    assert!(app.session.unsaved().is_some());
    store.fail.store(false, Ordering::SeqCst);
    app.session.persist().unwrap();
    assert!(app.session.unsaved().is_none());
}

/// Away from radial bodies and handles, the first linear is a fallback even after other kinds.
#[test]
fn masking_review_linear_canvas_fallback_finds_later_components() {
    let mut h = detail("panel.masking");
    exec(&mut h, "mask.add", json!({"kind":"radial","center":[0.2,0.2],"rx":0.1,"ry":0.1}));
    exec(&mut h, "mask.addComponent", json!({"kind":"linear","start":[0.3,0.3],"end":[0.7,0.3]}));
    exec(&mut h, "mask.addComponent", json!({"kind":"linear","start":[0.3,0.6],"end":[0.7,0.6]}));
    exec(&mut h, "mask.addComponent", json!({"kind":"luminanceRange"}));
    for index in [1, 2] {
        if index == 1 {
            exec(&mut h, "mask.select", json!({"id":1}));
        } else {
            exec(&mut h, "mask.selectComponent", json!({"component":2}));
        }
        let before = develop(&h);
        pointer(
            &mut h,
            json!([
                {"kind":"down","x":0.85,"y":0.85}, {"kind":"drag","x":0.87,"y":0.83},
                {"kind":"drag","x":0.92,"y":0.78}, {"kind":"up","x":0.92,"y":0.78}
            ]),
        );
        let after = develop(&h);
        for i in 0..4 {
            if i == index {
                assert_ne!(after.masks[0].components[i], before.masks[0].components[i]);
            } else {
                assert_eq!(after.masks[0].components[i], before.masks[0].components[i]);
            }
        }
        assert_eq!(crate::panels::masking::selected_component(&h.app), Some((1, index)));
        exec(&mut h, "edit.undo", json!({}));
    }
}
