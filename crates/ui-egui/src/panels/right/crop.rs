use super::{LightcraftApp, PhotoId, Tokens, divider, header, json, padded, register, slider, text_button};
use crate::icons::Icon;

/// The aspect presets of the Crop panel: (menu label, `crop.aspect` value).
const ASPECT_PRESETS: [(&str, &str); 10] = [
    ("Free", "free"),
    ("Original", "original"),
    ("1 × 1", "1x1"),
    ("4 × 5 / 8 × 10", "4x5"),
    ("8.5 × 11", "8.5x11"),
    ("5 × 7", "5x7"),
    ("2 × 3 / 4 × 6", "2x3"),
    ("4 × 3", "4x3"),
    ("16 × 9", "16x9"),
    ("16 × 10", "16x10"),
];

/// What the aspect button says for the stored lock (`aspect` = width × 100, height × 100):
/// a preset's name when the ratio matches one in either orientation, "Original" for the photo's own
/// shape, otherwise the ratio as `1.37 : 1`.
pub(super) fn aspect_label(aspect: Option<(u32, u32)>, original: Option<f64>) -> String {
    let Some((w, h)) = aspect else { return "Free".into() };
    if w == 0 || h == 0 {
        return "Free".into();
    }
    let r = w as f64 / h as f64;
    let is = |q: f64| q.is_finite() && q > 0.0 && ((r - q).abs() / q < 0.004 || (1.0 / r - q).abs() / q < 0.004);
    if original.is_some_and(is) {
        return "Original".into();
    }
    for (label, key) in ASPECT_PRESETS {
        if let Some((x, y)) = key.split_once('x')
            && let (Ok(x), Ok(y)) = (x.parse::<f64>(), y.parse::<f64>())
            && is(x / y)
        {
            return label.to_string();
        }
    }
    format!("{:.2} : 1", r.max(1.0 / r))
}

/// "Custom" row of the aspect menu: two number fields and an Apply button (`crop.aspect` `[w, h]`).
fn custom_aspect(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let key = egui::Id::unique("crop-custom-aspect");
    let (mut w, mut h): (String, String) = ui.data_mut(|d| d.get_temp(key)).unwrap_or_else(|| ("3".into(), "2".into()));
    let mut apply = false;
    ui.horizontal(|ui| {
        ui.label(crate::i18n::tr("Custom"));
        let a = ui.add(egui::TextEdit::singleline(&mut w).desired_width(34.0));
        crate::widgets::screen_reader_name(ui.ctx(), a.id, crate::i18n::tr("Width"));
        register(ui, "field:cropCustomW", a.rect);
        ui.label("×");
        let b = ui.add(egui::TextEdit::singleline(&mut h).desired_width(34.0));
        crate::widgets::screen_reader_name(ui.ctx(), b.id, crate::i18n::tr("Height"));
        register(ui, "field:cropCustomH", b.rect);
        let go = text_button(ui, "cropCustomApply", "Apply", false);
        apply = go.clicked() || ((a.lost_focus() || b.lost_focus()) && ui.input(|i| i.key_pressed(egui::Key::Enter)));
    });
    if apply {
        // Accept `3`, `2.5` and `3,5`; anything else (zero, negative, text, huge) is ignored.
        let num = |t: &str| t.trim().replace(',', ".").parse::<f64>().ok().filter(|v| v.is_finite() && *v > 0.0 && *v <= 1000.0);
        if let (Some(x), Some(y)) = (num(&w), num(&h))
            && (1.0 / lightcraft_geom::MAX_RATIO..=lightcraft_geom::MAX_RATIO).contains(&(x / y))
        {
            let _ = app.run("crop.aspect", json!({"aspect": [x, y]}));
            ui.close();
        }
    }
    ui.data_mut(|d| d.insert_temp(key, (w, h)));
}

pub(super) fn crop(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId) {
    let d = app.session.develop_of(id).unwrap_or_default();
    header(ui, "Crop");
    padded(ui, |ui| {
        let original = app.session.catalog.photo(id).map(|p| {
            let (w, h) = (p.width.max(1) as f64, p.height.max(1) as f64);
            if d.orientation.swaps_axes() { h / w } else { w / h }
        });
        ui.label(egui::RichText::new(crate::i18n::tr("Aspect Ratio")).color(Tokens::get(ui.ctx()).text_dim).size(12.0));
        ui.horizontal_wrapped(|ui| {
            let cur = aspect_label(app.crop_tool_aspect(), original);
            let t = Tokens::get(ui.ctx());
            let r = crate::widgets::dropdown(ui, "cropAspect", crate::i18n::tr(&cur), t.font(12.5), t.text);

            egui::Popup::menu(&r).show(|ui| {
                for (label, a) in ASPECT_PRESETS {
                    if ui.button(crate::i18n::tr(label)).clicked() {
                        let _ = app.run("crop.aspect", json!({"aspect": a}));
                    }
                }
                ui.separator();
                custom_aspect(app, ui);
            });
            let locked = app.crop_tool_aspect().is_some();
            if crop_icon(ui, "cropLock", if locked { Icon::Lock } else { Icon::Unlock }, if locked { "Locked" } else { "Lock" }, locked).clicked() {
                let _ = app.run("crop.aspect", json!({"aspect": "toggle"}));
            }
        });
    });
    let ang = lightcraft_develop::controls::find("crop.angle").copied();
    if let Some(spec) = ang {
        let out = slider(ui, &spec, d.crop.geometry.angle, true, Some("Straighten"));
        crate::panels::edit::apply_slider_out(app, &spec, out, |app, v| app.run("crop.straighten", json!({"angle": v})));
        // an exact angle, typed (issue #534): a field of its own, besides the slider's value
        padded(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(crate::i18n::tr("Angle"));
                let mut angle = d.crop.geometry.angle;
                let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
                let r = ui.add(
                    egui::DragValue::new(&mut angle)
                        .range(spec.min..=spec.max)
                        .clamp_existing_to_range(false)
                        .speed(0.05)
                        .fixed_decimals(2)
                        // as the readout writes it: "+2.50", and every zero "0.00"
                        .custom_formatter(|v, _| crate::panels::detail::crop_angle_label(v).trim_end_matches('°').to_string())
                        .suffix("°")
                        // read as the Straighten value reads a typed one: "-3,25" too
                        .custom_parser(|text| crate::widgets::typed_value(&spec, text.trim().trim_end_matches('°')))
                        // a typed angle applies on Return (or leaving the field), not keystroke by
                        // keystroke: no half-typed angles, one undo step
                        .update_while_editing(false),
                );
                crate::widgets::register(ui, "cropAngleField", r.rect);
                // a drag on the field is one undo step, like the slider's
                if r.drag_started() {
                    let _ = app.run("develop.beginInteraction", json!({"label": "Straighten"}));
                }
                // egui reports a change while text is typed even when the value isn't updated yet:
                // apply only a new angle
                // Esc keeps the old angle, but egui applies the typed text on the frame after the
                // one where Esc took the focus away: remember the Esc for that frame
                let cancel = egui::Id::unique("cropAngleFieldEscaped");
                let escaped = ui.data_mut(|m| m.remove_temp::<bool>(cancel)).unwrap_or(false);
                if escape && r.lost_focus() {
                    ui.data_mut(|m| m.insert_temp(cancel, true));
                }
                if r.changed() && !escaped && angle.is_finite() && angle != d.crop.geometry.angle {
                    let _ = app.run("crop.straighten", json!({"angle": (angle.clamp(spec.min, spec.max) * 100.0).round() / 100.0}));
                }
                if r.drag_stopped() {
                    let _ = app.run("develop.endInteraction", json!({}));
                }
            });
        });
    }
    padded(ui, |ui| {
        ui.horizontal(|ui| {
            let on = app.ui.tool == "straighten";
            if text_button(ui, "straightenTool", crate::i18n::tr("Straighten Tool"), on)
                .on_hover_text(crate::i18n::tr("Drag along the horizon; double-click for Auto"))
                .clicked()
            {
                app.ui.tool = if on { String::new() } else { "straighten".into() };
            }
            if text_button(ui, "straightenAuto", crate::i18n::tr("Auto"), false).clicked() {
                app.auto_straighten(ui.ctx());
            }
        });
    });
    padded(ui, |ui| {
        ui.label(crate::i18n::tr("Overlay"));
        {
            use crate::state::CropOverlay as O;
            let opts = [
                (O::Thirds, "Thirds", "thirds"),
                (O::Grid, "Grid", "grid"),
                (O::Golden, "Golden", "golden"),
                (O::Diagonal, "Diagonal", "diagonal"),
                (O::Triangle, "Triangle", "triangle"),
                (O::Spiral, "Spiral", "spiral"),
                (O::None, "None", "none"),
            ];
            let current = opts.iter().find(|(o, _, _)| *o == app.ui.crop_overlay).map_or("None", |(_, label, _)| *label);
            let t = Tokens::get(ui.ctx());
            let response = crate::widgets::dropdown(ui, "cropOverlay", crate::i18n::tr(current), t.font(12.5), t.text);
            egui::Popup::menu(&response).show(|ui| {
                for (overlay, label, key) in opts {
                    let response = ui.selectable_label(app.ui.crop_overlay == overlay, crate::i18n::tr(label));
                    register(ui, format!("button:cropOverlay-{key}"), response.rect);
                    if response.clicked() {
                        app.ui.crop_overlay = overlay;
                        ui.close();
                    }
                }
            });
            if matches!(app.ui.crop_overlay, O::Triangle | O::Spiral)
                && text_button(ui, "cropOverlayOrient", crate::i18n::tr("Flip Overlay (⇧O)"), false).clicked()
            {
                let _ = app.run("view.cropOverlayOrientation", json!({}));
            }
        }
    });
    divider(ui);
    header(ui, "Rotate & Flip");
    padded(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            for (id, icon, label, command, active) in [
                ("cropRotateLeft", Icon::Rotate, "Rotate Left", "photo.rotateLeft", false),
                ("cropRotateRight", Icon::RotateRight, "Rotate Right", "photo.rotateRight", false),
                ("cropFlipH", Icon::Flip, "Flip H", "photo.flipHorizontal", d.crop.flip_h),
                ("cropFlipV", Icon::FlipVertical, "Flip V", "photo.flipVertical", d.crop.flip_v),
                ("cropSwapAspect", Icon::BeforeAfter, "Swap Aspect (X)", "crop.rotateAspect", false),
                ("cropReset", Icon::Rotate, "Reset", "crop.reset", false),
            ] {
                if crop_icon(ui, id, icon, label, active).clicked() {
                    let _ = app.run(command, json!({}));
                }
            }
        });
    });
    divider(ui);
    header(ui, "Geometry");
    padded(ui, |ui| {
        ui.label(crate::i18n::tr("Perspective Correction"));
        {
            use lightcraft_develop::Upright;
            let modes = [
                ("Off", Upright::Off, "off"),
                ("Auto", Upright::Auto, "auto"),
                ("Guided", Upright::Guided, "guided"),
                ("Level", Upright::Level, "level"),
                ("Vertical", Upright::Vertical, "vertical"),
                ("Full", Upright::Full, "full"),
            ];
            let t = Tokens::get(ui.ctx());
            let current = modes.iter().find(|(_, mode, _)| *mode == d.geometry.upright).map_or("Off", |(label, _, _)| *label);
            let response = crate::widgets::dropdown(ui, "upright", crate::i18n::tr(current), t.font(12.5), t.text);
            egui::Popup::menu(&response).show(|ui| {
                for (label, mode, key) in modes {
                    let response = ui.selectable_label(mode == d.geometry.upright, crate::i18n::tr(label));
                    register(ui, format!("button:upright-{key}"), response.rect);
                    if response.clicked() {
                        let _ = app.run("geometry.upright", json!({"mode": key}));
                        app.ui.tool = if mode == Upright::Guided { "guidedUpright".into() } else { String::new() };
                        ui.close();
                    }
                }
            });
        }
        let guided = d.geometry.upright == lightcraft_develop::Upright::Guided;
        ui.horizontal_wrapped(|ui| {
            if !guided
                && d.geometry.upright != lightcraft_develop::Upright::Off
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
                    "{} of 4 guides — drag along lines that should be vertical or horizontal.",
                    d.geometry.guides.len()
                ))
                .size(11.0)
                .color(Tokens::get(ui.ctx()).text_dim),
            );
        }
    });
    egui::Frame::NONE.fill(Tokens::get(ui.ctx()).inset).corner_radius(8.0).outer_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
        header(ui, "Transform");
        for spec in lightcraft_develop::controls::in_section(lightcraft_develop::Section::Geometry).filter(|c| c.id != "crop.angle") {
            let v = lightcraft_develop::controls::get(&d, spec.id).unwrap_or(spec.default);
            let out = slider(ui, spec, v, true, None);
            crate::panels::edit::apply_slider_out(app, spec, out, |app, v| app.run("develop.set", json!({"control": spec.id, "value": v})));
        }
    });
    padded(ui, |ui| {
        ui.add_space(4.0);
        let mut c = d.geometry.constrain_crop;
        if ui.checkbox(&mut c, crate::i18n::tr("Constrain Crop")).changed() {
            let _ = app.run("develop.merge", json!({"settings": {"geometry": {"constrain_crop": c}}, "label": "Constrain Crop"}));
        }
    });
}

const REMOVE_MODES: [(&str, &str, Icon); 3] = [("remove", "Remove", Icon::Eraser), ("heal", "Heal", Icon::Heal), ("clone", "Clone", Icon::Clone)];

fn remove_label(tool: &str) -> &'static str {
    match tool {
        "heal" => "Heal",
        "clone" => "Clone",
        "fill" => "Content-Aware",
        _ => "Remove Tool",
    }
}

fn remove_slider(app: &mut LightcraftApp, ui: &mut egui::Ui, key: &str, selected: bool, card: bool) {
    use lightcraft_develop::{ControlSpec, Section, Track};
    let (id, label, max, default, value, scale) = match key {
        "size" => ("ui.removeSize", "Size", 250.0, 20.0, app.ui.remove_size as f64 * 1000.0, 1000.0),
        "feather" => ("ui.removeFeather", "Feather", 100.0, 50.0, app.ui.remove_feather as f64, 1.0),
        _ => (if card { "ui.selectedSpotOpacity" } else { "ui.removeOpacity" }, "Opacity", 100.0, 100.0, app.ui.remove_opacity as f64, 1.0),
    };
    let spec = ControlSpec {
        id,
        label,
        section: Section::Detail,
        min: if key == "size" { 1.0 } else { 0.0 },
        max,
        default,
        step: 1.0,
        decimals: 0,
        track: Track::Plain,
    };
    let out = slider(ui, &spec, value, true, None);
    if let Some(value) = out.value {
        match key {
            "size" => app.ui.remove_size = (value / scale) as f32,
            "feather" => app.ui.remove_feather = value as f32,
            _ => app.ui.remove_opacity = value as f32,
        }
    }
    if selected {
        crate::panels::edit::apply_slider_out(app, &spec, out, |app, value| app.run("spot.update", json!({key: value / scale})));
    }
}

pub(super) fn remove(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId) {
    use egui::{Frame, Margin, RichText, Stroke, vec2};
    let t = Tokens::get(ui.ctx());
    let d = app.session.develop_of(id).unwrap_or_default();
    let selected = app.session.active_spot.and_then(|i| d.spots.get(i).map(|spot| (i, spot.clone())));
    if let Some((_, spot)) = &selected {
        (app.ui.remove_size, app.ui.remove_feather, app.ui.remove_opacity) = (spot.size as f32, spot.feather as f32, spot.opacity as f32);
    }
    Frame::NONE.inner_margin(Margin { left: 22, right: 20, top: 20, bottom: 16 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(crate::i18n::tr("Remove Tool")).font(t.semibold(15.0)).color(t.text));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let reset = crop_icon(ui, "removeReset", Icon::Rotate, "Delete all spots", false);
                egui::Popup::menu(&reset).show(|ui| {
                    let confirm = ui.add_enabled(!d.spots.is_empty(), egui::Button::new(crate::i18n::tr("Delete all spots")));
                    register(ui, "button:removeResetConfirm", confirm.rect);
                    if confirm.clicked() {
                        let _ = app.run("spot.clear", json!({}));
                        ui.close();
                    }
                });
            });
        });
        ui.add_space(12.0);
        divider(ui);
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            for (tool, label, icon) in REMOVE_MODES {
                let active = app.ui.tool == tool || (tool == "remove" && app.ui.tool == "fill");
                let response =
                    crate::widgets::icon_button(ui, &format!("removeMode-{tool}"), icon, vec2(38.0, 38.0), active, true, crate::i18n::tr(label));
                register(ui, format!("button:removeMode-{tool}"), response.rect);
                if response.clicked() {
                    app.ui.tool = tool.into();
                }
            }
        });
        ui.add_space(14.0);
        ui.label(RichText::new(crate::i18n::tr(remove_label(&app.ui.tool))).font(t.semibold(13.0)).color(t.text));
        ui.add_space(5.0);
        let help = match app.ui.tool.as_str() {
            "heal" => "Blend a distraction with the surrounding texture.",
            "clone" => "Copy texture from another area of the photo.",
            _ => "Paint over a distraction on the photo to remove it.",
        };
        ui.add(egui::Label::new(RichText::new(crate::i18n::tr(help)).font(t.font(12.0)).color(t.text_dim)).wrap());
        ui.add_space(14.0);
        remove_slider(app, ui, "size", selected.is_some(), false);
        ui.add_space(8.0);
        remove_slider(app, ui, "opacity", selected.is_some(), false);
        ui.add_space(16.0);
        if let Some((index, spot)) = &selected {
            Frame::new().fill(t.canvas).stroke(Stroke::new(1.0, t.field_border.gamma_multiply(0.45))).corner_radius(9).inner_margin(14).show(
                ui,
                |ui| {
                    ui.set_min_width((ui.available_width() - 1.0).max(1.0));
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(crate::i18n::tr("Selected")).font(t.semibold(12.0)).color(t.text));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if crop_icon(ui, "spotDelete", Icon::Trash, "Delete (⌫)", false).clicked() {
                                let _ = app.run("spot.delete", json!({}));
                            }
                            if matches!(spot.mode, lightcraft_develop::SpotMode::Heal | lightcraft_develop::SpotMode::Clone)
                                && crop_icon(ui, "spotRefresh", Icon::Rotate, "Refresh Source (/)", false).clicked()
                            {
                                let _ = app.run("spot.refreshSource", json!({}));
                            }
                        });
                    });
                    divider(ui);
                    ui.add_space(8.0);
                    let mode = match spot.mode {
                        lightcraft_develop::SpotMode::Remove => "remove",
                        lightcraft_develop::SpotMode::Fill => "fill",
                        lightcraft_develop::SpotMode::Heal => "heal",
                        lightcraft_develop::SpotMode::Clone => "clone",
                    };
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(crate::i18n::tr("Type")).color(t.text_dim));
                        let response = crate::widgets::dropdown(ui, "selectedSpotMode", crate::i18n::tr(remove_label(mode)), t.font(12.0), t.text);
                        egui::Popup::menu(&response).show(|ui| {
                            for tool in ["remove", "heal", "clone", "fill"] {
                                let response = ui.selectable_label(tool == mode, crate::i18n::tr(remove_label(tool)));
                                register(ui, format!("spotMode:{tool}"), response.rect);
                                if response.clicked() {
                                    let _ = app.run("spot.update", json!({"mode": tool}));
                                    ui.close();
                                }
                            }
                        });
                    });
                    ui.add_space(8.0);
                    remove_slider(app, ui, "opacity", true, true);
                    ui.add_space(8.0);
                    ui.label(RichText::new(format!("{} / {}", index + 1, d.spots.len())).font(t.font(10.5)).color(t.text_dim));
                },
            );
            ui.add_space(18.0);
        }
        divider(ui);
        ui.add_space(12.0);
        let mut hover = app.ui.tool_overlay == crate::state::ToolOverlay::Auto;
        let response = ui.checkbox(&mut hover, crate::i18n::tr("Show overlay on hover"));
        register(ui, "check:removeOverlayHover", response.rect);
        if response.changed() {
            let mode = if hover { "auto" } else { "never" };
            let _ = app.run("view.toolOverlay", json!({"mode": mode}));
        }
        ui.add_space(12.0);
        let advanced = egui::CollapsingHeader::new(crate::i18n::tr("More options")).id_salt("remove-advanced").show(ui, |ui| {
            ui.add_space(8.0);
            remove_slider(app, ui, "feather", selected.is_some(), false);
            ui.add_space(8.0);
            if text_button(ui, "findDust", crate::i18n::tr("Find Dust Spots"), false).clicked() {
                match app.run("spot.findDust", json!({})) {
                    Ok(result) => {
                        let count = result["added"].as_u64().unwrap_or(0);
                        let message = if count == 0 {
                            crate::i18n::tr("No dust spots found").to_string()
                        } else {
                            format!("{} : {count}", crate::i18n::tr("Corrections added"))
                        };
                        app.toast(ui.ctx(), message);
                    }
                    Err(error) => app.toast(ui.ctx(), error),
                }
            }
            ui.add_space(8.0);
            if text_button(ui, "findBlemishes", crate::i18n::tr("Find Blemishes"), false)
                .on_hover_text(crate::i18n::tr("Requires the face detector in Settings > Faces"))
                .clicked()
            {
                match app.run_typed("spot.findBlemishes", json!({})) {
                    Ok(result) => {
                        let count = result["added"].as_u64().unwrap_or(0);
                        app.toast(
                            ui.ctx(),
                            if count == 0 {
                                crate::i18n::tr("No blemishes found").to_string()
                            } else {
                                format!("{} : {count}", crate::i18n::tr("Corrections added"))
                            },
                        );
                    }
                    Err(error) if error.kind == lightcraft_engine::ErrorKind::ModelMissing(lightcraft_engine::AiModel::FaceDetector) => {
                        let _ = app.run("app.settings", json!({"tab":"faces"}));
                    }
                    Err(error) => app.toast_error(ui.ctx(), error.message),
                }
            }
            ui.add_space(8.0);
            ui.checkbox(&mut app.ui.visualize_spots, crate::i18n::tr("Visualize Spots (A)"));
            let spec = lightcraft_develop::ControlSpec {
                id: "ui.spotsThreshold",
                label: "Threshold",
                section: lightcraft_develop::Section::Detail,
                min: 0.0,
                max: 100.0,
                default: 50.0,
                step: 1.0,
                decimals: 0,
                track: lightcraft_develop::Track::Plain,
            };
            let out = slider(ui, &spec, app.ui.spots_threshold as f64, app.ui.visualize_spots, None);
            if let Some(value) = out.value {
                app.ui.spots_threshold = value as f32;
            }
            tool_overlay_options(app, ui);
            if !d.spots.is_empty() && text_button(ui, "removeClear", crate::i18n::tr("Delete all spots"), false).clicked() {
                let _ = app.run("spot.clear", json!({}));
            }
        });
        register(ui, "section:removeAdvanced", advanced.header_response.rect);
    });
}

/// Tool Overlay: when the spot and mask pins show on the photo (Auto / Always / Selected / Never;
/// Shift+H steps through them). Remove and Masking panels.
pub(crate) fn tool_overlay_options(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    use crate::state::ToolOverlay;
    let t = Tokens::get(ui.ctx());
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(crate::i18n::tr("Tool Overlay")).color(t.text_dim));
        let current = app.ui.tool_overlay;
        let choices = |ui: &mut egui::Ui| {
            for m in ToolOverlay::ALL {
                let item = ui.selectable_label(m == current, crate::i18n::tr(m.label()));
                register(ui, format!("toolOverlay:{}", m.name()), item.rect);
                if item.clicked() {
                    let _ = app.run("view.toolOverlay", json!({"mode": m.name()}));
                    ui.close();
                }
            }
        };
        if egui::containers::menu::is_in_menu(ui) {
            let response = crate::widgets::menu_button(ui, crate::i18n::tr(current.label()), choices);
            register(ui, "dropdown:toolOverlay", response.response.rect);
        } else {
            let response = crate::widgets::dropdown(ui, "toolOverlay", crate::i18n::tr(current.label()), t.font(12.5), t.text_label)
                .on_hover_text(crate::i18n::tr("When pins show on the photo (Shift+H cycles)"));
            egui::Popup::menu(&response).show(choices);
        }
    });
}

pub(super) fn red_eye(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId) {
    let d = app.session.develop_of(id).unwrap_or_default();
    header(ui, "Red Eye");
    padded(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            for (label, pet) in [("Red Eye", false), ("Pet Eye", true)] {
                if text_button(ui, &format!("eyeMode-{}", if pet { "pet" } else { "red" }), label, app.ui.eye_pet == pet).clicked() {
                    app.ui.eye_pet = pet;
                }
            }
        });
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(crate::i18n::tr("Drag over an eye on the photo; the pupil inside is found automatically."))
                .color(Tokens::get(ui.ctx()).text_dim),
        );
    });
    let n = d.red_eye.len();
    if n == 0 {
        return;
    }
    if app.ui.eye >= n {
        app.ui.eye = n - 1;
    }
    let i = app.ui.eye;
    let eye = d.red_eye[i];
    divider(ui);
    crate::panels::edit::sub_title(
        ui,
        &crate::i18n::tr_format!(
            "{label} {index} of {n}",
            label = crate::i18n::tr(if eye.pet { "Pet Eye" } else { "Red Eye" }),
            index = i + 1,
            n = n
        ),
    );
    for k in ["pupilSize", "darken"] {
        let cid = format!("redEye.{i}.{k}");
        let Some(spec) = lightcraft_develop::controls::find(&cid) else { continue };
        let v = lightcraft_develop::controls::get(&d, &cid).unwrap_or(spec.default);
        let out = slider(ui, spec, v, true, None);
        crate::panels::edit::apply_slider_out(app, spec, out, |app, v| app.run("develop.set", json!({"control": cid, "value": v})));
    }
    padded(ui, |ui| {
        if eye.pet {
            let mut on = eye.catchlight.is_some();
            if ui.checkbox(&mut on, crate::i18n::tr("Add Catchlight")).changed() {
                let _ = app.run("redeye.catchlight", json!({"index": i, "on": on}));
            }
            ui.add_space(4.0);
        }
        ui.horizontal(|ui| {
            if text_button(ui, "eyeDelete", crate::i18n::tr("Delete"), false).clicked() {
                let _ = app.run("redeye.delete", json!({"index": i}));
            }
            if n > 1 && text_button(ui, "eyeDeleteAll", crate::i18n::tr("Delete all"), false).clicked() {
                for k in (0..n).rev() {
                    let _ = app.run("redeye.delete", json!({"index": k}));
                }
            }
        });
    });
}

/// Compact tool action with the same command and automation target as its text counterpart.
fn crop_icon(ui: &mut egui::Ui, id: &str, icon: Icon, label: &str, active: bool) -> egui::Response {
    let response = crate::widgets::icon_button(ui, id, icon, egui::vec2(34.0, 32.0), active, true, crate::i18n::tr(label));
    register(ui, format!("button:{id}"), response.rect);
    response
}
