//! The Lens Blur section: Apply, the sliders, what stays in focus (the subject, a mask or a fixed
//! shape that can be moved on the photo), Find Subject and Brush Refinement.

use super::{
    Color32, ControlSpec, Icon, LightcraftApp, Pos2, Rect, RichText, Section, Sense, Stroke, Tokens, Track, control, paint, pos2, register, slider,
    sub_title, text_button, vec2,
};
use lightcraft_develop::DevelopSettings;
use serde_json::json;

/// The Lens Blur section: Apply, Blur Amount, Boost, and what stays in focus (the photo's subject,
/// or one of its masks, e.g. an Object mask that selected it).
pub(super) fn lens_blur(app: &mut LightcraftApp, ui: &mut egui::Ui, d: &DevelopSettings) {
    let t = Tokens::get(ui.ctx());
    let b = &d.lens_blur;
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 4, bottom: 4 }).show(ui, |ui| {
        let mut on = b.enabled;
        let r = ui.checkbox(&mut on, crate::i18n::tr("Apply"));
        register(ui, "check:lensBlur.enabled", r.rect);
        if r.on_hover_text(crate::i18n::tr("Blur what is out of focus, as a wide aperture would")).changed() {
            let mut patch = json!({"enabled": on});
            // switched on the first time: a visible starting amount, as in Lightroom
            if on && b.amount == 0.0 {
                patch["amount"] = json!(50.0);
            }
            let _ = app.run("develop.merge", json!({"settings": {"lens_blur": patch}, "label": "Lens Blur"}));
        }
    });
    control(app, ui, d, "lensBlur.amount", b.enabled);
    control(app, ui, d, "lensBlur.boost", b.enabled);
    control(app, ui, d, "lensBlur.transition", b.enabled);
    // the focus controls do nothing while the blur is off: shown, but not usable
    let has_mask = b.focus_mask.and_then(|id| d.masks.iter().find(|m| m.id == id)).is_some();
    if app.ui.tool == "lensBlurShape" && (!b.enabled || has_mask || b.shape == lightcraft_develop::FocusShape::Auto) {
        app.ui.tool.clear();
    }
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 6, bottom: 4 }).show(ui, |ui| ui.add_enabled_ui(b.enabled, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(crate::i18n::tr("In Focus")).font(t.font(13.0)).color(t.text_dim));
            let chosen = b.focus_mask.and_then(|id| d.masks.iter().find(|m| m.id == id));
            // without a mask: the subject estimate, or the fixed shape chosen below
            let auto = b.shape == lightcraft_develop::FocusShape::Auto;
            let none = if auto { crate::i18n::tr("Subject") } else { crate::i18n::tr(b.shape.label()) };
            let shown = chosen.map_or_else(|| none.to_string(), |m| m.name.clone());
            let r = crate::widgets::dropdown(ui, "lensBlurFocus", &shown, t.font(14.0), t.text_label);
            let r = r.on_hover_text(crate::i18n::tr("What stays sharp: the photo's subject, or the selection of one of its masks"));
            let mut pick: Option<Option<u32>> = None;
            egui::Popup::menu(&r).show(|ui| {
                let item = ui.selectable_label(chosen.is_none() && auto, crate::i18n::tr("Subject"));
                register(ui, "lensBlurFocus:subject", item.rect);
                if item.clicked() {
                    pick = Some(None);
                }
                for m in &d.masks {
                    let item = ui.selectable_label(chosen.is_some_and(|c| c.id == m.id), m.name.as_str());
                    register(ui, format!("lensBlurFocus:mask:{}", m.id), item.rect);
                    if item.clicked() {
                        pick = Some(Some(m.id));
                    }
                }
            });
            if let Some(focus) = pick {
                // "Subject": the estimate again, not a fixed shape
                let mut patch = json!({"focus_mask": focus});
                if focus.is_none() {
                    patch["shape"] = json!("auto");
                }
                let _ = app.run("develop.merge", json!({"settings": {"lens_blur": patch}, "label": "Lens Blur Focus"}));
            }
        });
        // without a focus mask: the focus area's shape (the estimate, or a fixed one), which can
        // be moved and resized on the photo
        if !has_mask {
            ui.add_space(4.0);
            focus_shape_row(app, ui, b);
            if b.shape != lightcraft_develop::FocusShape::Auto {
                let active = app.ui.tool == "lensBlurShape";
                let r = text_button(ui, "lensBlurEditShape", "Move & Resize", active).on_hover_text(crate::i18n::tr(
                    "Drag the shape on the photo to move it; drag its side or bottom handle to stretch it. A shape button puts it back in the middle.",
                ));
                if r.clicked() {
                    if active { app.ui.tool.clear() } else { app.ui.tool = "lensBlurShape".into() }
                }
            }
        }
        // the AI subject search runs only when asked: it takes a while on the CPU, and the
        // automatic estimate stays until then
        ui.add_space(4.0);
        let r = text_button(ui, "lensBlurFindSubject", "Find Subject", false).on_hover_text(crate::i18n::tr(
            "Select the subject with AI (SAM 3), starting where the camera focused when the file records it, and keep it in focus",
        ));
        if r.clicked() {
            match app.run_typed("lensBlur.findSubject", json!({})) {
                Ok(v) => {
                    let msg = if v["from"] == "camera" { "Selecting the subject at the camera's focus point…" } else { "Selecting the subject…" };
                    app.toast(ui.ctx(), crate::i18n::tr(msg));
                }
                Err(e) => app.ai_error(ui.ctx(), e, None),
            }
        }
        // a Find Subject still running: say so (meanwhile the automatic estimate stays in focus)
        let waiting = b.focus_mask.and_then(|id| d.masks.iter().find(|m| m.id == id)).is_some_and(lightcraft_pipeline::lens_blur::awaiting_ai);
        if waiting && app.session.segmenter.busy() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new(crate::i18n::tr("Finding the subject… The first time on a photo takes a while.")).size(11.0).color(t.text));
            });
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        } else if waiting {
            ui.label(
                RichText::new(crate::i18n::tr("The subject wasn't selected. Try Find Subject again, or click it in Masking (M)."))
                    .size(11.0)
                    .color(t.text),
            );
        }
        ui.label(
            RichText::new(crate::i18n::tr("Refine it in Masking (M): + → Object adds what was missed, − → Object leaves a part out."))
                .size(11.0)
                .color(t.text_dim),
        );
    }));
    lens_blur_brush(app, ui, b);
}

/// The Lens Blur focus area's shape when no mask sets it: one button per shape, each showing it
/// (drawn in the theme's colours, so it reads on the light themes too).
fn focus_shape_row(app: &mut LightcraftApp, ui: &mut egui::Ui, b: &lightcraft_develop::LensBlur) {
    use lightcraft_develop::FocusShape;
    let t = Tokens::get(ui.ctx());
    let enabled = ui.is_enabled();
    ui.horizontal(|ui| {
        ui.label(RichText::new(crate::i18n::tr("Shape")).font(t.font(13.0)).color(t.text_dim));
        ui.add_space(4.0);
        for shape in FocusShape::ALL {
            let selected = b.shape == shape;
            let (r, resp) = ui.allocate_exact_size(vec2(36.0, 28.0), Sense::click());
            let name = serde_json::to_value(shape).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
            register(ui, format!("lensBlurShape:{name}"), r);
            crate::widgets::announce(&resp, crate::i18n::tr(shape.label()));
            let resp = resp.on_hover_text(crate::i18n::tr(match shape {
                FocusShape::Auto => "Auto: the photo's subject, estimated",
                FocusShape::Round => "Round",
                FocusShape::Wide => "Horizontal oval",
                FocusShape::Tall => "Vertical oval",
            }));
            let p = ui.painter();
            let fill = if selected {
                t.pressed
            } else if resp.hovered() && enabled {
                t.hover
            } else {
                t.button
            };
            p.rect_filled(r, 4.0, fill);
            if selected {
                p.rect_stroke(r, 4.0, Stroke::new(1.0, if enabled { t.accent } else { t.text_dim }), egui::StrokeKind::Inside);
            }
            // (dimmed while the blur is off, in the theme's colours)
            let colour = match (enabled, selected) {
                (false, _) => t.icon.gamma_multiply(0.4),
                (true, true) => t.accent,
                (true, false) => t.icon,
            };
            paint_focus_shape(p, r.shrink(5.0), shape, colour);
            // a preset puts the shape back in the middle at its size (also the one in use, once moved)
            if resp.clicked() && (!selected || b.area.is_some()) {
                let _ = app.run("develop.merge", json!({"settings": {"lens_blur": {"shape": name, "area": null}}, "label": "Lens Blur Shape"}));
            }
        }
    });
}

/// A focus shape's picture in `r`: the oval in its proportions (the largest fills the height), or
/// the subject icon for Auto.
fn paint_focus_shape(p: &egui::Painter, r: Rect, shape: lightcraft_develop::FocusShape, colour: Color32) {
    let Some((rx, ry)) = shape.radii() else {
        paint(p, r, Icon::Subject, colour);
        return;
    };
    // the shapes' largest radius spans the box's height (all shapes on one scale)
    let k = r.height() / 2.0 / 0.32;
    let (ax, ay) = (rx as f32 * k, ry as f32 * k);
    let c = r.center();
    let pts: Vec<Pos2> = (0..48)
        .map(|i| {
            let a = i as f32 / 48.0 * std::f32::consts::TAU;
            pos2(c.x + ax * a.cos(), c.y + ay * a.sin())
        })
        .collect();
    p.add(egui::Shape::convex_polygon(pts.clone(), colour.gamma_multiply(0.3), Stroke::NONE));
    p.add(egui::Shape::closed_line(pts, Stroke::new(1.5, colour)));
}

/// Lens Blur Brush Refinement: the Focus and Blur brushes (painted on the photo, see
/// `detail::lens_blur_brush_overlay`), their Amount, the shared brush's Size, Feather, Flow and
/// Auto Mask, and Reset.
fn lens_blur_brush(app: &mut LightcraftApp, ui: &mut egui::Ui, b: &lightcraft_develop::LensBlur) {
    let on = b.enabled;
    sub_title(ui, crate::i18n::tr("Brush Refinement"));
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 0, bottom: 4 }).show(ui, |ui| {
        ui.add_enabled_ui(on, |ui| {
            ui.horizontal(|ui| {
                for (tool, label, tip) in
                    [("lensBlurFocus", "Focus", "Paint what should stay sharp"), ("lensBlurBlur", "Blur", "Paint what should be blurred")]
                {
                    let active = app.ui.tool == tool;
                    let r = text_button(ui, tool, label, active).on_hover_text(crate::i18n::tr(tip));
                    if r.clicked() {
                        if active { app.ui.tool.clear() } else { app.ui.tool = tool.to_string() }
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let painted = !b.focus_strokes.is_empty() || !b.blur_strokes.is_empty();
                    if text_button(ui, "lensBlurBrushReset", "Reset", false)
                        .on_hover_text(crate::i18n::tr("Remove the strokes of both brushes"))
                        .clicked()
                        && painted
                    {
                        let _ = app.run("lensBlur.brushReset", json!({}));
                    }
                });
            });
            let mut auto = app.ui.brush_auto_mask;
            if ui
                .checkbox(&mut auto, crate::i18n::tr("Auto Mask"))
                .on_hover_text(crate::i18n::tr("Paint only areas like the one under the brush"))
                .changed()
            {
                app.ui.brush_auto_mask = auto;
            }
        });
    });
    // the brush is the Masking brush's (its size also follows [ and ]), with its own Amount
    // (a reset puts the brush's Amount back to full strength, the others to their minimum)
    for (id, label, min, max, get) in [
        ("ui.lensBlurBrushAmount", "Amount", 1.0, 100.0, app.ui.lens_blur_brush_amount as f64),
        ("ui.brushSize", "Size", 1.0, 100.0, (app.ui.brush_size * 400.0) as f64),
        ("ui.brushFeather", "Feather", 0.0, 100.0, app.ui.brush_feather as f64),
        ("ui.brushFlow", "Flow", 1.0, 100.0, app.ui.brush_flow as f64),
    ] {
        let default = if id == "ui.lensBlurBrushAmount" { 100.0 } else { min };
        let s = ControlSpec { id, label, section: Section::LensBlur, min, max, default, step: 1.0, decimals: 0, track: Track::Plain };
        let out = slider(ui, &s, get, on, None);
        if let Some(v) = out.value {
            match id {
                "ui.lensBlurBrushAmount" => app.ui.lens_blur_brush_amount = v as f32,
                "ui.brushSize" => app.ui.brush_size = (v / 400.0) as f32,
                "ui.brushFeather" => app.ui.brush_feather = v as f32,
                _ => app.ui.brush_flow = v as f32,
            }
        }
    }
    // the brushes paint only while the blur is on
    if !on && app.ui.tool.starts_with("lensBlur") {
        app.ui.tool.clear();
    }
}
