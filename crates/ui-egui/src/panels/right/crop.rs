use crate::menu_item::MenuItem;
use super::{LightcraftApp, PhotoId, Tokens, divider, header, json, padded, register, slider, text_button};

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
        ui.horizontal(|ui| {
            ui.label(crate::i18n::tr("Aspect Ratio"));
            let cur = aspect_label(app.crop_tool_aspect(), original);
            let t = Tokens::get(ui.ctx());
            let r = crate::widgets::dropdown(ui, "cropAspect", crate::i18n::tr(&cur), t.font(12.5), t.text);

            egui::Popup::menu(&r).show(|ui| {
                for (label, a) in ASPECT_PRESETS {
                    if MenuItem::new(crate::i18n::tr(label)).show(ui).clicked() {
                        let _ = app.run("crop.aspect", json!({"aspect": a}));
                    }
                }
                ui.separator();
                custom_aspect(app, ui);
            });
            let locked = app.crop_tool_aspect().is_some();
            if text_button(ui, "cropLock", if locked { "Locked" } else { "Lock" }, locked).clicked() {
                let _ = app.run("crop.aspect", json!({"aspect": "toggle"}));
            }
        });
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            if text_button(ui, "cropRotateLeft", crate::i18n::tr("Rotate Left"), false).clicked() {
                let _ = app.run("photo.rotateLeft", json!({}));
            }
            if text_button(ui, "cropRotateRight", crate::i18n::tr("Rotate Right"), false).clicked() {
                let _ = app.run("photo.rotateRight", json!({}));
            }
        });
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            if text_button(ui, "cropFlipH", crate::i18n::tr("Flip H"), d.crop.flip_h).clicked() {
                let _ = app.run("photo.flipHorizontal", json!({}));
            }
            if text_button(ui, "cropFlipV", crate::i18n::tr("Flip V"), d.crop.flip_v).clicked() {
                let _ = app.run("photo.flipVertical", json!({}));
            }
            if text_button(ui, "cropSwapAspect", crate::i18n::tr("Swap Aspect (X)"), false).clicked() {
                let _ = app.run("crop.rotateAspect", json!({}));
            }
            if text_button(ui, "cropReset", crate::i18n::tr("Reset"), false).clicked() {
                let _ = app.run("crop.reset", json!({}));
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
            let items: Vec<(&str, &str)> = opts.iter().map(|(_, l, k)| (*l, *k)).collect();
            let active = opts.iter().position(|(o, _, _)| *o == app.ui.crop_overlay);
            if let Some(i) = crate::widgets::segmented(ui, "cropOverlay", &items, active, 4) {
                app.ui.crop_overlay = opts[i].0;
            }
            if matches!(app.ui.crop_overlay, O::Triangle | O::Spiral)
                && text_button(ui, "cropOverlayOrient", crate::i18n::tr("Flip Overlay (⇧O)"), false).clicked()
            {
                let _ = app.run("view.cropOverlayOrientation", json!({}));
            }
        }
    });
    divider(ui);
    header(ui, "Geometry");
    padded(ui, |ui| {
        ui.label(crate::i18n::tr("Upright"));
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
            let items: Vec<(&str, &str)> = modes.iter().map(|(l, _, k)| (*l, *k)).collect();
            let active = modes.iter().position(|(_, m, _)| *m == d.geometry.upright);
            if let Some(i) = crate::widgets::segmented(ui, "upright", &items, active, 3) {
                let (_, mode, key) = modes[i];
                let _ = app.run("geometry.upright", json!({"mode": key}));
                app.ui.tool = if mode == Upright::Guided { "guidedUpright".into() } else { String::new() };
            }
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
        ui.add_space(4.0);
        let mut c = d.geometry.constrain_crop;
        if ui.checkbox(&mut c, crate::i18n::tr("Constrain Crop")).changed() {
            let _ = app.run("develop.merge", json!({"settings": {"geometry": {"constrain_crop": c}}, "label": "Constrain Crop"}));
        }
    });
    for spec in lightcraft_develop::controls::in_section(lightcraft_develop::Section::Geometry).filter(|c| c.id != "crop.angle") {
        let v = lightcraft_develop::controls::get(&d, spec.id).unwrap_or(spec.default);
        let out = slider(ui, spec, v, true, None);
        crate::panels::edit::apply_slider_out(app, spec, out, |app, v| app.run("develop.set", json!({"control": spec.id, "value": v})));
    }
}

pub(super) fn remove(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId) {
    let d = app.session.develop_of(id).unwrap_or_default();
    header(ui, "Remove");
    padded(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            for (label, tool) in [("Remove", "remove"), ("Content-Aware", "fill"), ("Heal", "heal"), ("Clone", "clone")] {
                if text_button(ui, &format!("removeMode-{tool}"), label, app.ui.tool == tool).clicked() {
                    app.ui.tool = tool.into();
                }
            }
        });
        ui.add_space(8.0);
        ui.label(crate::i18n::tr_format!("{} spot(s) on this photo", d.spots.len()));
        ui.add_space(4.0);
        if text_button(ui, "findDust", crate::i18n::tr("Find Dust Spots"), false)
            .on_hover_text(crate::i18n::tr("Heal the small, soft dark spots sensor dust leaves on smooth areas"))
            .clicked()
        {
            match app.run("spot.findDust", json!({})) {
                Ok(r) => {
                    let n = r["added"].as_u64().unwrap_or(0);
                    app.toast(
                        ui.ctx(),
                        if n == 0 {
                            crate::i18n::tr("No dust spots found").to_string()
                        } else {
                            crate::i18n::tr_format!("Healed {n} dust spot{}", if n == 1 { "" } else { "s" }, n = n)
                        },
                    );
                }
                Err(e) => app.toast(ui.ctx(), e),
            }
        }
        ui.add_space(4.0);
        ui.label(egui::RichText::new(crate::i18n::tr("Paint over a distraction on the photo to remove it.")).color(Tokens::get(ui.ctx()).text_dim));
    });
    // brush settings; with a spot selected they edit that spot too
    let sel = app.session.active_spot.and_then(|i| d.spots.get(i).map(|sp| (i, sp.clone())));
    if let Some((i, sp)) = &sel {
        (app.ui.remove_size, app.ui.remove_feather, app.ui.remove_opacity) = (sp.size as f32, sp.feather as f32, sp.opacity as f32);
        divider(ui);
        let mode = match sp.mode {
            lightcraft_develop::SpotMode::Heal => "Heal",
            lightcraft_develop::SpotMode::Clone => "Clone",
            lightcraft_develop::SpotMode::Remove => "Remove",
            lightcraft_develop::SpotMode::Fill => "Content-Aware",
        };
        crate::panels::edit::sub_title(ui, &crate::i18n::tr_format!("{mode} spot {} of {}", i + 1, d.spots.len(), mode = crate::i18n::tr(mode)));
    }
    let plain = |id: &'static str, label: &'static str, min: f64, max: f64, default: f64| lightcraft_develop::ControlSpec {
        id,
        label,
        section: lightcraft_develop::Section::Detail,
        min,
        max,
        default,
        step: 1.0,
        decimals: 0,
        track: lightcraft_develop::Track::Plain,
    };
    let sliders = [
        (plain("ui.removeSize", "Size", 1.0, 250.0, 20.0), "size", (app.ui.remove_size * 1000.0) as f64),
        (plain("ui.removeFeather", "Feather", 0.0, 100.0, 50.0), "feather", app.ui.remove_feather as f64),
        (plain("ui.removeOpacity", "Opacity", 0.0, 100.0, 100.0), "opacity", app.ui.remove_opacity as f64),
    ];
    for (spec, key, v) in sliders {
        let out = slider(ui, &spec, v, true, None);
        let scale = if key == "size" { 1000.0 } else { 1.0 };
        if let Some(v) = out.value {
            match key {
                "size" => app.ui.remove_size = (v / scale) as f32,
                "feather" => app.ui.remove_feather = v as f32,
                _ => app.ui.remove_opacity = v as f32,
            }
        }
        if sel.is_some() {
            crate::panels::edit::apply_slider_out(app, &spec, out, |app, v| app.run("spot.update", json!({key: v / scale})));
        }
    }
    if sel.is_some() {
        padded(ui, |ui| {
            ui.horizontal(|ui| {
                if text_button(ui, "spotRefresh", crate::i18n::tr("Refresh Source (/)"), false).clicked() {
                    let _ = app.run("spot.refreshSource", json!({}));
                }
                if text_button(ui, "spotDelete", crate::i18n::tr("Delete (⌫)"), false).clicked() {
                    let _ = app.run("spot.delete", json!({}));
                }
            });
        });
    }
    // Visualize Spots (A): a black/white high-pass view that makes dust and specks stand out
    padded(ui, |ui| {
        let mut v = app.ui.visualize_spots;
        if ui.checkbox(&mut v, crate::i18n::tr("Visualize Spots (A)")).changed() {
            app.ui.visualize_spots = v;
        }
    });
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
    if let Some(v) = out.value {
        app.ui.spots_threshold = v as f32;
    }
    padded(ui, |ui| {
        tool_overlay_options(app, ui);
        ui.add_space(4.0);
        if !d.spots.is_empty() && text_button(ui, "removeClear", crate::i18n::tr("Delete all spots"), false).clicked() {
            for i in (0..d.spots.len()).rev() {
                let _ = app.run("spot.delete", json!({"index": i}));
            }
        }
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
        let r = crate::widgets::dropdown(ui, "toolOverlay", crate::i18n::tr(current.label()), t.font(12.5), t.text_label)
            .on_hover_text(crate::i18n::tr("When pins show on the photo (Shift+H cycles)"));
        egui::Popup::menu(&r).show(|ui| {
            for m in ToolOverlay::ALL {
                let item = MenuItem::new(crate::i18n::tr(m.label())).selected(m == current).widget(format!("toolOverlay:{}", m.name())).show(ui);
                if item.clicked() {
                    let _ = app.run("view.toolOverlay", json!({"mode": m.name()}));
                }
            }
        });
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
