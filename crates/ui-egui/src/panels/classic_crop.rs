//! The crop tool laid out as Lightroom Classic's Crop & Straighten drawer (fork-owned): Aspect and
//! its lock, Angle with the straighten tool and Auto, Constrain to Image, and Reset / Close. The
//! rotate and flip buttons (also in the Photo menu, issue #705). The rest of what upstream's Crop
//! panel holds lives where Lightroom Classic keeps it: the guide overlays in View ▸ Tool Overlay (cycled with ⇧O), and Upright with
//! the transform sliders in the Edit panel's Transform section ([`transform_body`]). A child module
//! of `right`, so it uses that file's helpers; upstream's `crop` stays as it is for merges.

use lcc_catalog::PhotoId;
use serde_json::json;

use super::{aspect_label, aspect_menu, cmd_icon, header, padded};
use crate::LccApp;
use crate::icons::Icon;
use crate::state::RightPanel;
use crate::theme::Tokens;
use crate::widgets::{slider, text_button};

pub(super) fn show(app: &mut LccApp, ui: &mut egui::Ui, id: PhotoId) {
    let d = app.session.develop_of(id).unwrap_or_default();
    header(ui, "Crop & Straighten");
    padded(ui, |ui| {
        let original = app.session.catalog.photo(id).map(|p| {
            let (w, h) = (p.width.max(1) as f64, p.height.max(1) as f64);
            if d.orientation.swaps_axes() { h / w } else { w / h }
        });
        ui.horizontal(|ui| {
            ui.label(crate::i18n::tr("Aspect"));
            let cur = aspect_label(app.crop_tool_aspect(), original, &app.session.crop_aspects.custom);
            let t = Tokens::get(ui.ctx());
            // a saved ratio's name is the user's own words: not looked up for a translation
            let saved = app.session.crop_aspects.custom.iter().any(|a| a.name == cur);
            let text = if saved { cur.as_str() } else { crate::i18n::tr(&cur) };
            let r = crate::widgets::dropdown(ui, "cropAspect", text, t.font(12.5), t.text);
            if r.clicked() {
                ui.data_mut(|d| d.insert_temp(egui::Id::new(super::ASPECT_EDITING), false));
            }
            egui::Popup::menu(&r).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| aspect_menu(app, ui));
            let locked = app.crop_tool_aspect().is_some();
            let (icon, label) = if locked { (Icon::Link, "Aspect Locked") } else { (Icon::Unlink, "Lock Aspect") };
            if cmd_icon(app, ui, "cropLock", icon, label, None, locked).clicked() {
                let _ = app.run("crop.aspect", json!({"aspect": "toggle"}));
            }
        });
        ui.add_space(6.0);
        // rotate and flip, here as well as in Photo ▸ Rotate & Flip (issue #705)
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let row = [
                ("cropRotateLeft", Icon::RotateLeft, "Rotate Left", "photo.rotateLeft", false),
                ("cropRotateRight", Icon::RotateRight, "Rotate Right", "photo.rotateRight", false),
                ("cropFlipH", Icon::FlipH, "Flip Horizontal", "photo.flipHorizontal", d.crop.flip_h),
                ("cropFlipV", Icon::FlipV, "Flip Vertical", "photo.flipVertical", d.crop.flip_v),
                ("cropSwapAspect", Icon::SwapAspect, "Rotate Crop Aspect", "crop.rotateAspect", false),
            ];
            for (wid, icon, label, command, on) in row {
                if cmd_icon(app, ui, wid, icon, label, Some(command), on).clicked() {
                    let _ = app.run(command, json!({}));
                }
            }
        });
    });
    // Angle: the slider (its value can be typed), then the straighten tool and Auto under it
    if let Some(spec) = lcc_develop::controls::find("crop.angle").copied() {
        let out = slider(ui, &spec, d.crop.geometry.angle, true, Some("Angle"));
        super::super::edit::apply_slider_out(app, &spec, out, |app, v| app.run("crop.straighten", json!({"angle": v})));
    }
    padded(ui, |ui| {
        ui.horizontal(|ui| {
            let on = app.ui.tool == "straighten";
            if cmd_icon(app, ui, "straightenTool", Icon::Straighten, "Straighten Tool", None, on)
                .on_hover_text(crate::i18n::tr("Drag along the horizon; double-click for Auto"))
                .clicked()
            {
                app.ui.tool = if on { String::new() } else { "straighten".into() };
            }
            if text_button(ui, "straightenAuto", crate::i18n::tr("Auto"), false).clicked() {
                app.auto_straighten(ui.ctx());
            }
        });
        ui.add_space(4.0);
        let mut c = d.geometry.constrain_crop;
        if ui.checkbox(&mut c, crate::i18n::tr("Constrain to Image")).changed() {
            let _ = app.run("develop.merge", json!({"settings": {"geometry": {"constrain_crop": c}}, "label": "Constrain to Image"}));
        }
    });
    padded(ui, |ui| {
        // one row at the right edge, as the drawer's buttons sit in Lightroom Classic
        ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 28.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if text_button(ui, "cropClose", crate::i18n::tr("Close"), false).clicked() {
                app.ui.right = RightPanel::Edit;
                app.ui.tool = String::new();
            }
            if text_button(ui, "cropReset", crate::i18n::tr("Reset"), false).clicked() {
                let _ = app.run("crop.reset", json!({}));
            }
        });
    });
}

/// The Edit panel's Transform section, as Lightroom Classic's Transform panel: Upright, Constrain
/// Crop and the transform sliders. A clip takes the crop, straighten and flips only.
pub(crate) fn transform_body(app: &mut LccApp, ui: &mut egui::Ui, id: PhotoId) {
    let d = app.session.develop_of(id).unwrap_or_default();
    let video = crate::video_ui::photo_is_video(app, id);
    crate::video_ui::video_gate(ui, video, |ui| {
        padded(ui, |ui| {
            crate::widgets::row_caption(ui, "upright", "Upright");
            {
                use lcc_develop::Upright;
                let modes = [
                    ("Off", Upright::Off, "off"),
                    ("Auto", Upright::Auto, "auto"),
                    ("Guided", Upright::Guided, "guided"),
                    ("Level", Upright::Level, "level"),
                    ("Vertical", Upright::Vertical, "vertical"),
                    ("Full", Upright::Full, "full"),
                ];
                let items: Vec<(&str, &str)> = modes.iter().map(|(l, _, k)| (*l, *k)).collect();
                let active = modes.iter().position(|(_, m, _)| *m == d.geometry.upright);
                if let Some(i) = crate::widgets::segmented(ui, "upright", &items, active, 3)
                    && let Some((_, mode, key)) = modes.get(i)
                {
                    let _ = app.run("geometry.upright", json!({"mode": key, "again": Some(i) == active}));
                    app.ui.tool = if *mode == Upright::Guided { "guidedUpright".into() } else { String::new() };
                }
            }
            let guided = d.geometry.upright == lcc_develop::Upright::Guided;
            ui.horizontal_wrapped(|ui| {
                if !guided
                    && d.geometry.upright != lcc_develop::Upright::Off
                    && text_button(ui, "uprightUpdate", crate::i18n::tr("Update"), false).clicked()
                {
                    let mode = serde_json::to_value(d.geometry.upright).unwrap_or_default();
                    let _ = app.run("geometry.upright", json!({"mode": mode}));
                }
                if guided {
                    let drawing = app.ui.tool == "guidedUpright";
                    if text_button(ui, "uprightDraw", crate::i18n::tr("Draw Guides"), drawing).clicked() {
                        app.ui.tool = if drawing { String::new() } else { "guidedUpright".into() };
                    }
                    if !d.geometry.guides.is_empty() && text_button(ui, "uprightClear", crate::i18n::tr("Clear Guides"), false).clicked() {
                        let _ = app.run("geometry.guides", json!({"guides": []}));
                    }
                }
            });
            if guided {
                ui.label(
                    egui::RichText::new(crate::i18n::tr_format!(
                        "{} of 4 guides. Drag along lines that should be vertical or horizontal.",
                        d.geometry.guides.len()
                    ))
                    .size(11.0)
                    .color(Tokens::get(ui.ctx()).text_dim),
                );
            }
            ui.add_space(4.0);
            let mut c = d.geometry.constrain_crop;
            if ui.checkbox(&mut c, crate::i18n::tr("Constrain Crop")).changed() {
                let _ = app.run("develop.merge", json!({"settings": {"geometry": {"constrain_crop": c}}, "label": "Constrain Crop"}));
            }
        });
        for spec in lcc_develop::controls::in_section(lcc_develop::Section::Geometry).filter(|c| c.id != "crop.angle") {
            // (in the Edit panel's Transform: the tool profile in use may hide it)
            if !super::super::edit::shown_here(app, ui, spec.id) {
                continue;
            }
            let v = lcc_develop::controls::get(&d, spec.id).unwrap_or(spec.default);
            let out = slider(ui, spec, v, true, None);
            super::super::edit::apply_slider_out(app, spec, out, |app, v| app.run("develop.set", json!({"control": spec.id, "value": v})));
        }
    });
}
