//! The right-hand panel next to the tool strip: Edit, Crop, Remove, Masking, Red Eye, Info,
//! Keywords, Versions, Activity.

use egui::{Align2, Rect, Sense, pos2, vec2};
use lightcraft_catalog::PhotoId;
use serde_json::json;

use crate::LightcraftApp;
use crate::icons::{Icon, paint};
use crate::state::RightPanel;
use crate::theme::Tokens;
use crate::widgets::{divider, register, slider, text_button};

pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let frame = egui::Frame::NONE.fill(t.chrome).stroke(egui::Stroke::new(1.0, t.divider));
    frame.show(ui, |ui| {
        // (taken on every pass the panel is drawn; one from earlier than the pass before is forgotten)
        let held = held_offset(ui.ctx());
        let Some(id) = app.session.active() else {
            // the Keyword List is the library's, not a photo's: it is there without a selection
            if app.ui.right == RightPanel::Keywords {
                // (without a selection there is no Add keyword box for Add Keywords to put the cursor in)
                app.ui.focus_keywords = None;
                egui::ScrollArea::vertical().id_salt("right-scroll").auto_shrink([false, false]).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    if fold_header(app, ui, "keywording", "Keywords") {
                        padded(ui, |ui| {
                            ui.label(egui::RichText::new(crate::i18n::tr("Select photos to give them keywords.")).color(t.text_dim));
                        });
                    }
                    super::keyword_list::show(app, ui);
                });
                return;
            }
            let r = ui.max_rect();
            // every tab (Edit, Info, Versions…) says it: nothing to show until photos come in
            if super::empty_library::shown(&app.session) {
                super::empty_message(ui, r, "No photos yet", "Import photos to get started");
            } else {
                super::empty_message(ui, r, "No photo selected", "Select a photo to edit");
            }
            return;
        };
        let mut area = egui::ScrollArea::vertical().id_salt("right-scroll").auto_shrink([false, false]);
        if let Some(offset) = held {
            area = area.vertical_scroll_offset(offset);
        }
        // (where the pass starts: by its end egui may have pulled a shrunken panel's offset back)
        let area_id = ui.data(|d| d.get_temp::<egui::Id>(egui::Id::unique(AREA)));
        let start = held.or_else(|| area_id.and_then(|a| egui::scroll_area::State::load(ui.ctx(), a)).map(|s| s.clamped_offset().y));
        ui.data_mut(|d| match start {
            Some(o) => {
                d.insert_temp(egui::Id::unique(OFFSET), o);
            }
            None => d.remove::<f32>(egui::Id::unique(OFFSET)),
        });
        let scrolled = area.show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            match app.ui.right {
                RightPanel::Edit => super::edit::show(app, ui, id),
                RightPanel::Profiles => super::profiles::show(app, ui, id),
                RightPanel::Crop => crop(app, ui, id),
                RightPanel::Remove => remove(app, ui, id),
                RightPanel::Masking => super::masking::show(app, ui, id),
                RightPanel::RedEye => red_eye(app, ui, id),
                RightPanel::Info => info(app, ui, id),
                RightPanel::Keywords => keywords(app, ui, id),
                RightPanel::Versions => versions(app, ui, id),
                RightPanel::Activity => activity(app, ui, id),
                RightPanel::None => {}
            }
        });
        ui.data_mut(|d| d.insert_temp(egui::Id::unique(AREA), scrolled.id));
    });
}

/// The panel's scroll area, its offset as the last pass started, and how far
/// [`hold_list_in_place`] asks it to move.
const AREA: &str = "right-scroll-id";
const OFFSET: &str = "right-scroll-offset";
const HOLD: &str = "right-scroll-hold";

/// Where [`hold_list_in_place`] holds the panel: its offset as the pass before started, moved by as much as
/// the Keywords box grew. Set on the scroll area as it starts, so the pass drawn again shows the list
/// in place (a scroll asked for during a pass only lands a frame later).
fn held_offset(ctx: &egui::Context) -> Option<f32> {
    let (grew, asked) = ctx.data_mut(|d| d.remove_temp::<(f32, u64)>(egui::Id::unique(HOLD)))?;
    if asked.checked_add(1) != Some(ctx.cumulative_pass_nr()) {
        return None;
    }
    let offset = ctx.data(|d| d.get_temp::<f32>(egui::Id::unique(OFFSET)))?;
    Some((offset + grew).max(0.0)).filter(|o| o.is_finite())
}

pub fn header(ui: &mut egui::Ui, title: &str) {
    let t = Tokens::get(ui.ctx());
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::hover());
    ui.painter().text(pos2(r.left() + 24.0, r.center().y + 2.0), Align2::LEFT_CENTER, crate::i18n::tr(title), t.semibold(15.0), t.text);
}

/// A panel title that folds its section shut and open again (`section:{id}`; kept in
/// [`crate::state::UiState::folded_sections`]). Says whether the section is open.
pub fn fold_header(app: &mut LightcraftApp, ui: &mut egui::Ui, id: &str, title: &str) -> bool {
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::click());
    let title = crate::i18n::tr(title);
    fold_title(app, ui, id, r, &resp, title)
}

/// [`fold_header`] for a title whose room is already taken (`r`, answering as `resp`).
pub fn fold_title(app: &mut LightcraftApp, ui: &egui::Ui, id: &str, r: Rect, resp: &egui::Response, title: &str) -> bool {
    let t = Tokens::get(ui.ctx());
    let open = !app.ui.section_folded(id);
    crate::widgets::announce_disclosure(resp, true, open, title);
    register(ui, format!("section:{id}"), r);
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(r, 0.0, t.chrome.gamma_multiply(1.06));
    }
    if resp.has_focus() {
        p.rect_stroke(r.shrink(2.0), 4.0, egui::Stroke::new(1.5, t.accent), egui::StrokeKind::Inside);
    }
    p.text(pos2(r.left() + 24.0, r.center().y + 2.0), Align2::LEFT_CENTER, title, t.semibold(15.0), t.text);
    let chevron = Rect::from_center_size(pos2(r.right() - 30.0, r.center().y + 2.0), vec2(14.0, 14.0));
    paint(p, chevron, if open { Icon::ChevronDown } else { Icon::ChevronRight }, t.icon);
    if resp.clicked() {
        app.ui.toggle_folded(id);
    }
    open
}

pub fn label_row(ui: &mut egui::Ui, label: &str, value: &str) {
    let t = Tokens::get(ui.ctx());
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
    ui.painter().text(pos2(r.left() + 24.0, r.center().y), Align2::LEFT_CENTER, crate::i18n::tr(label), t.font(12.5), t.text_dim);
    ui.painter().text(pos2(r.left() + 110.0, r.center().y), Align2::LEFT_CENTER, value, t.font(12.5), t.text_label);
}

fn padded(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 6, bottom: 6 }).show(ui, add);
}

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
fn aspect_label(aspect: Option<(u32, u32)>, original: Option<f64>) -> String {
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

fn crop(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId) {
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
                    if ui.button(crate::i18n::tr(label)).clicked() {
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
        super::edit::apply_slider_out(app, &spec, out, |app, v| app.run("crop.straighten", json!({"angle": v})));
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
                        .custom_formatter(|v, _| super::detail::crop_angle_label(v).trim_end_matches('°').to_string())
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
        super::edit::apply_slider_out(app, spec, out, |app, v| app.run("develop.set", json!({"control": spec.id, "value": v})));
    }
}

fn remove(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId) {
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
        super::edit::sub_title(ui, &crate::i18n::tr_format!("{mode} spot {} of {}", i + 1, d.spots.len(), mode = crate::i18n::tr(mode)));
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
            super::edit::apply_slider_out(app, &spec, out, |app, v| app.run("spot.update", json!({key: v / scale})));
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
                let item = ui.selectable_label(m == current, crate::i18n::tr(m.label()));
                register(ui, format!("toolOverlay:{}", m.name()), item.rect);
                if item.clicked() {
                    let _ = app.run("view.toolOverlay", json!({"mode": m.name()}));
                }
            }
        });
    });
}

fn red_eye(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId) {
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
    super::edit::sub_title(
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
        super::edit::apply_slider_out(app, spec, out, |app, v| app.run("develop.set", json!({"control": cid, "value": v})));
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

fn info(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId) {
    let Some(p) = app.session.catalog.photo(id).cloned() else { return };
    header(ui, "Info");
    let t = Tokens::get(ui.ctx());
    padded(ui, |ui| {
        camera_card(ui, &p);
        ui.add_space(6.0);
        if let Some(why) = &p.preview_only {
            if crate::widgets::preview_only_notice(ui, "info", why) {
                crate::panels::edit::reload_preview_only(app, ui.ctx(), id);
            }
            ui.add_space(6.0);
        }
        if let Some(name) = &p.copy_name {
            let of = p
                .copy_of
                .and_then(|m| app.session.catalog.photo(m))
                .map(|m| m.file_name.clone())
                .unwrap_or_else(|| crate::i18n::tr("a removed photo").into());
            ui.label(egui::RichText::new(crate::i18n::tr_format!("Virtual copy “{name}” of {of}", name = name, of = of)).color(t.text_label));
        }
        ui.add_space(8.0);
        if let Some(r) = ui.horizontal(|ui| crate::rating::Rating::new("info", p.rating).show(ui)).inner {
            let mut params = json!({"rating": r});
            crate::panels::compare::target_active(app, &mut params);
            let _ = app.run("photo.rate", params);
        }
        ui.add_space(6.0);
        // colour label: one swatch per label (hover shows its name); clicking the current one clears it
        ui.horizontal(|ui| {
            for l in lightcraft_catalog::ColorLabel::ALL {
                let (r, resp) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
                crate::widgets::announce(&resp, crate::i18n::tr(&format!("{l:?}")));
                let key = format!("{l:?}").to_lowercase();
                register(ui, format!("label:{key}"), r);
                let on = p.label == Some(l);
                ui.painter().circle_filled(r.center(), if on || resp.hovered() { 8.0 } else { 6.5 }, crate::panels::grid::label_color(l));
                if on {
                    ui.painter().circle_stroke(r.center(), 10.0, egui::Stroke::new(1.5, t.text));
                }
                let resp = resp.on_hover_text(crate::i18n::color_label(&app.session.catalog, l));
                if resp.clicked() {
                    // in Compare / Survey only the active photo, as the rating keys and the bottom pill do
                    let mut params = json!({"label": if on { "none".to_string() } else { key }});
                    crate::panels::compare::target_active(app, &mut params);
                    let _ = app.run("photo.label", params);
                }
            }
            if let Some(l) = p.label {
                ui.label(egui::RichText::new(app.session.catalog.label_name(l)).color(t.text_label));
            }
        });
    });
    divider(ui);
    // with several photos selected: Target Photo shows this photo's values, Selected Photos what
    // the selection shares (a field where they differ is empty and says Mixed)
    let selected = app.session.selected();
    let several = selected.len() > 1;
    let for_selected = several && app.ui.settings.info_selected_photos;
    let m = shown_meta(&app.session.catalog, &p, &selected, for_selected);
    let mixed = if for_selected { mixed_meta(&app.session.catalog, &selected, &m) } else { Vec::new() };
    // the photos an edit goes to: Target Photo edits the photo it shows
    let targets = if several && !for_selected { vec![id] } else { selected.clone() };
    padded(ui, |ui| {
        if several {
            metadata_for(app, ui);
        }
        for (label, key, value, lines) in [
            ("Title", "title", &m.title, 1),
            ("Caption", "caption", &m.caption, 2),
            ("Alt Text", "altText", &m.alt_text, 2),
            ("Extended Description", "extendedDescription", &m.extended_description, 3),
            ("Copyright", "copyright", &m.copyright, 1),
        ] {
            meta_field(app, ui, label, key, value, lines, mixed.contains(&key), &targets);
        }
        copyright_status(app, ui, m.copyright_status, mixed.contains(&"copyrightStatus"), &targets);
        for (label, key, value, lines) in [
            ("Rights Usage Terms", "usageTerms", &m.usage_terms, 2),
            ("Copyright Info URL", "copyrightUrl", &m.copyright_url, 1),
            ("Creator", "creator", &m.creator, 1),
        ] {
            meta_field(app, ui, label, key, value, lines, mixed.contains(&key), &targets);
        }
        // file: name (rename), path (reveal), capture time (edit)
        let small = |ui: &mut egui::Ui, text: &str| ui.label(egui::RichText::new(crate::i18n::tr(text)).size(11.5).color(t.text_dim));
        small(ui, "File Name");
        ui.allocate_ui_with_layout(vec2(ui.available_width(), 22.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if crate::widgets::icon_button(ui, "infoRename", Icon::Pencil, vec2(20.0, 20.0), false, true, "Rename…").clicked() {
                let _ = app.run("dialog.rename", json!({}));
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.add(egui::Label::new(egui::RichText::new(&p.file_name).color(t.text)).truncate());
            });
        });
        ui.add_space(4.0);
        small(ui, "File Path");
        ui.horizontal(|ui| {
            let path = match &p.source {
                lightcraft_catalog::Source::File { path } => path.clone(),
                lightcraft_catalog::Source::Demo { .. } => crate::i18n::tr("Generated demo photo").into(),
            };
            ui.add(egui::Label::new(egui::RichText::new(path).size(12.0).color(t.text_label)).truncate());
            if crate::menus::ui_enabled(app, "app.showInFinder") {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if crate::widgets::icon_button(ui, "infoReveal", Icon::Folder, vec2(20.0, 20.0), false, true, crate::menus::reveal_label())
                        .clicked()
                    {
                        let _ = app.run("app.showInFinder", json!({}));
                    }
                });
            }
        });
        ui.add_space(4.0);
        small(ui, "Captured");
        ui.allocate_ui_with_layout(vec2(ui.available_width(), 22.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if crate::widgets::icon_button(ui, "editCaptureTime", Icon::Pencil, vec2(20.0, 20.0), false, true, "Edit Capture Time…").clicked() {
                let _ = app.run("dialog.captureTime", json!({}));
            }
            let cap = p.captured.as_deref().map(crate::i18n::display_time).unwrap_or_else(|| "—".into());
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.add(egui::Label::new(egui::RichText::new(cap).size(12.5).color(t.text)).truncate());
            });
        });
        ui.add_space(6.0);
        for (label, key, value) in [
            ("Location", "location", &m.location),
            ("City", "city", &m.city),
            ("State / Province", "state", &m.state),
            ("Country", "country", &m.country),
        ] {
            meta_field(app, ui, label, key, value, 1, mixed.contains(&key), &targets);
        }
        // GPS: typed as "lat, lon" (or degrees / minutes / seconds); empty clears it
        let gps_mixed = mixed.contains(&"gps");
        let gps = if gps_mixed { String::new() } else { m.gps.map(|(la, lo)| format!("{la:.6}, {lo:.6}")).unwrap_or_default() };
        let gid = egui::Id::unique("info-gps");
        ui.label(egui::RichText::new("GPS").size(11.5).color(t.text_dim));
        let mut edit: FieldEdit = ui.data(|d| d.get_temp(gid)).unwrap_or_else(|| FieldEdit::new(&gps, gps_mixed, &targets));
        let hint = crate::i18n::tr(if gps_mixed { MIXED } else { "latitude, longitude" });
        let r = crate::text_field::TextField::singleline("field:gps", &mut edit.text).hint(hint).label("GPS").width(f32::INFINITY).show(ui);
        edit.typed |= edit.text != edit.start;
        // the fields' width (the row below and the map keep to it)
        let width = r.response.rect.width();
        if r.editing {
            ui.data_mut(|d| d.insert_temp(gid, edit.clone()));
        } else {
            ui.data_mut(|d| d.remove::<FieldEdit>(gid));
        }
        // Return or leaving saves; Esc gives the edit up
        let text = edit.text.trim();
        if r.committed()
            && (text != edit.start || edit.clears())
            && let Err(e) = app.run("photo.setMeta", json!({"ids": edit.ids, "gps": text}))
        {
            app.toast(ui.ctx(), e);
        }
        // (one photo's place: not shown while the selected photos' places differ)
        let one_place = m.gps.filter(|_| !gps_mixed);
        if let Some((la, lo)) = one_place {
            let pretty = format!("{:.5}° {}, {:.5}° {}", la.abs(), if la >= 0.0 { "N" } else { "S" }, lo.abs(), if lo >= 0.0 { "E" } else { "W" });
            ui.add(egui::Label::new(egui::RichText::new(pretty).size(11.0).color(t.text_dim)).truncate());
        }
        // the map of the shown photo's place (the active one's, also while the selected photos'
        // places differ); a click on it opens the Map view there, the button below opens
        // OpenStreetMap's site
        if let Some(pos) = lightcraft_map::LatLon::from_gps(m.gps) {
            ui.add_space(6.0);
            // in a private saved location the map stays hidden until asked for
            let pass = ui.ctx().cumulative_pass_nr();
            match super::map::locations::hides_mini_map(app, id, m.gps, pass) {
                Some(loc) => {
                    ui.label(
                        egui::RichText::new(crate::i18n::tr_format!("In the private location “{name}”: its map is hidden.", name = loc.name))
                            .size(11.0)
                            .color(t.text_dim),
                    );
                    if text_button(ui, "showPrivateMap", crate::i18n::tr("Show Map"), false).clicked() {
                        app.map.loc.reveal_private = Some(id);
                    }
                }
                None => super::map::mini_map(app, ui, id, pos, width),
            }
        }
        if let Some((la, lo)) = one_place {
            ui.add_space(6.0);
            if text_button(ui, "showOnMap", crate::i18n::tr("Open in Browser"), false)
                .on_hover_text(crate::i18n::tr("Open the place in OpenStreetMap"))
                .clicked()
            {
                let url = format!("https://www.openstreetmap.org/?mlat={la:.6}&mlon={lo:.6}#map=15/{la:.6}/{lo:.6}");
                if let Err(e) = crate::links::open(app, &url) {
                    app.toast(ui.ctx(), e);
                }
            }
            if text_button(ui, "fillLocation", crate::i18n::tr("Fill Location"), false)
                .on_hover_text(format!(
                    "{}\n{}",
                    crate::i18n::tr("Fill empty City, State and Country from GPS (offline GeoNames place names)"),
                    crate::i18n::tr("The place names list towns of 15,000 people or more: in a smaller one, City gets the nearest larger town.")
                ))
                .clicked()
                && let Err(e) = app.run("metadata.reverseGeocode", json!({}))
            {
                app.toast_error(ui.ctx(), e);
            }
        }
        if let Some(a) = p.analysis {
            let mut line = crate::i18n::tr_format!("Focus {:.0}", a.sharpness);
            if a.clipped > 0.02 {
                line.push_str(&crate::i18n::tr_format!(" · {:.0}% clipped", a.clipped * 100.0));
            }
            if a.group.is_some() {
                line.push_str(if a.best { " · best of its burst" } else { " · in a burst" });
            }
            ui.add_space(4.0);
            ui.label(egui::RichText::new(line).color(t.text_dim));
        }
        // offline originals / smart previews
        // (cached answers, checked off the UI thread: unknown counts as online / no smart preview)
        if let lightcraft_catalog::Source::File { path } = &p.source {
            let avail = &app.session.media.availability;
            let online = !avail.is_offline(path);
            let smart = app.session.media.smart_dir.as_ref().is_some_and(|d| {
                let shard = lightcraft_engine::smart::sharded_file_path(d, &p);
                let legacy = d.join(lightcraft_engine::smart::file_name(&p));
                avail.exists(&shard.to_string_lossy()) == Some(true) || avail.exists(&legacy.to_string_lossy()) == Some(true)
            });
            if !online || smart {
                let text = match (online, smart) {
                    (false, true) => "Original offline · editing the smart preview",
                    (false, false) => "Original offline · no smart preview",
                    _ => "Smart preview available",
                };
                ui.add_space(6.0);
                ui.label(egui::RichText::new(crate::i18n::tr(text)).color(if online { t.text_dim } else { t.accent }));
            }
        }
        ui.add_space(10.0);
        if text_button(ui, "allMetadata", crate::i18n::tr("All Metadata…"), false)
            .on_hover_text(crate::i18n::tr("Every EXIF, GPS and XMP field in the file"))
            .clicked()
        {
            let _ = app.run("dialog.allMetadata", json!({}));
        }
    });
}

/// The camera card at the top of Info: camera, lens, size · file size and format, then the
/// capture settings.
fn camera_card(ui: &mut egui::Ui, p: &lightcraft_catalog::Photo) {
    let t = Tokens::get(ui.ctx());
    let m = &p.meta;
    egui::Frame::NONE.fill(t.canvas).corner_radius(6.0).inner_margin(egui::Margin::symmetric(12, 10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let dim = |s: &str| egui::RichText::new(s.to_string()).size(12.0).color(t.text_label);
        if !m.camera.is_empty() {
            ui.label(dim(&m.camera));
        }
        if !m.lens.is_empty() {
            ui.add(egui::Label::new(dim(&m.lens)).truncate());
        }
        ui.horizontal(|ui| {
            ui.label(dim(&format!("{} × {}  ·  {}", p.width, p.height, human_size(p.file_size))));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                egui::Frame::NONE.fill(t.chrome).corner_radius(3.0).inner_margin(egui::Margin::symmetric(5, 1)).show(ui, |ui| {
                    ui.label(egui::RichText::new(p.format.to_uppercase()).size(10.5).color(t.text_label));
                });
            });
        });
        let rows = [
            ("Focal length", m.focal_mm.map(|f| format!("{f:.1} mm").replace(".0 mm", " mm"))),
            ("Shutter speed", (!m.shutter.is_empty()).then(|| crate::i18n::tr_format!("{} sec", m.shutter))),
            ("Aperture", m.aperture.map(|f| format!("f / {f:.1}").replace(".0", ""))),
            ("ISO", m.iso.map(|i| i.to_string())),
        ];
        if rows.iter().any(|r| r.1.is_some()) {
            ui.add_space(6.0);
            egui::Grid::new("info-capture").num_columns(2).spacing([24.0, 3.0]).show(ui, |ui| {
                for (k, v) in rows {
                    ui.label(dim(crate::i18n::tr(k)));
                    ui.label(dim(&v.unwrap_or_else(|| "—".into())));
                    ui.end_row();
                }
            });
        }
        // where the camera focused, and how (View ▸ Focus Point draws it): only when known; the
        // whole card's width, so "AF-C · Expanded Flexible Spot · Face tracking" fits
        if let Some(text) = focus_text(m.focus.as_ref()) {
            ui.add_space(3.0);
            let r = ui.horizontal_wrapped(|ui| {
                ui.label(dim(crate::i18n::tr("Focus")));
                ui.add_space(12.0);
                ui.label(egui::RichText::new(text).size(12.0).color(t.text));
            });
            register(ui, "info:focus", r.response.rect);
        }
    });
}

/// The camera's focus mode, AF area and tracking in the interface language ("AF-C · Wide · Animal Eye
/// Tracking"), or `None` when the photo has no focus point.
pub(crate) fn focus_text(f: Option<&lightcraft_meta::FocusPoint>) -> Option<String> {
    let f = f?;
    let words: Vec<&str> = f.words().into_iter().map(crate::i18n::tr).collect();
    Some(if words.is_empty() { crate::i18n::tr("Focus point").to_string() } else { words.join(" · ") })
}

fn human_size(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => format!("{} KB", b >> 10),
        b => format!("{b} B"),
    }
}

/// What a metadata field shows while the selected photos' values differ (Selected Photos).
const MIXED: &str = "Mixed";

/// An Info panel field's edit under way, kept in egui memory between frames. A field saves when
/// its text is not the text the edit started from, so a field left as it was saves nothing, even
/// when what the panel shows changed during the edit. It saves to the photos it edited when the
/// edit started (the selected photos, or Target Photo's photo alone), also when the selection
/// changed since.
#[derive(Clone)]
struct FieldEdit {
    text: String,
    start: String,
    /// The photos the field edited when the edit started (`photo.setMeta` `ids`).
    ids: Vec<u64>,
    /// The field said Mixed when the edit started.
    mixed: bool,
    /// Something was typed or deleted since.
    typed: bool,
}

impl FieldEdit {
    fn new(value: &str, mixed: bool, targets: &[PhotoId]) -> Self {
        let ids = targets.iter().map(|id| id.0).collect();
        FieldEdit { text: value.to_string(), start: value.to_string(), ids, mixed, typed: false }
    }

    /// A Mixed field that was typed in and left empty: it clears the field on every selected photo.
    fn clears(&self) -> bool {
        self.mixed && self.typed
    }
}

/// The Info panel's text fields, by their `photo.setMeta` key.
const META_TEXT_KEYS: [&str; 12] = [
    "title",
    "caption",
    "altText",
    "extendedDescription",
    "copyright",
    "usageTerms",
    "copyrightUrl",
    "creator",
    "location",
    "city",
    "state",
    "country",
];

/// The value of one of [`META_TEXT_KEYS`].
fn meta_text<'a>(m: &'a lightcraft_catalog::Meta, key: &str) -> Option<&'a str> {
    Some(match key {
        "title" => &m.title,
        "caption" => &m.caption,
        "altText" => &m.alt_text,
        "extendedDescription" => &m.extended_description,
        "copyright" => &m.copyright,
        "usageTerms" => &m.usage_terms,
        "copyrightUrl" => &m.copyright_url,
        "creator" => &m.creator,
        "location" => &m.location,
        "city" => &m.city,
        "state" => &m.state,
        "country" => &m.country,
        _ => return None,
    })
}

/// The metadata the Info panel's fields show: the active photo `p`'s, or with `for_selected`
/// (Selected Photos) the active photo's when it is one of the `selected` photos, else the first
/// selected photo's.
pub(crate) fn shown_meta(
    catalog: &lightcraft_catalog::Catalog,
    p: &lightcraft_catalog::Photo,
    selected: &[PhotoId],
    for_selected: bool,
) -> lightcraft_catalog::Meta {
    if !for_selected || selected.contains(&p.id) {
        return p.meta.clone();
    }
    selected.iter().find_map(|&id| catalog.photo(id)).map_or_else(|| p.meta.clone(), |q| q.meta.clone())
}

/// The Info panel fields (`photo.setMeta` keys, plus `copyrightStatus` and `gps`) where one of
/// the photos `ids` has a value other than `shown`'s, the metadata the panel draws.
pub(crate) fn mixed_meta(catalog: &lightcraft_catalog::Catalog, ids: &[PhotoId], shown: &lightcraft_catalog::Meta) -> Vec<&'static str> {
    let mut mixed = Vec::new();
    for m in ids.iter().filter_map(|&id| catalog.photo(id)).map(|p| &p.meta) {
        for key in META_TEXT_KEYS {
            if !mixed.contains(&key) && meta_text(m, key) != meta_text(shown, key) {
                mixed.push(key);
            }
        }
        if !mixed.contains(&"copyrightStatus") && m.copyright_status != shown.copyright_status {
            mixed.push("copyrightStatus");
        }
        if !mixed.contains(&"gps") && m.gps != shown.gps {
            mixed.push("gps");
        }
        if mixed.len() == META_TEXT_KEYS.len() + 2 {
            break;
        }
    }
    mixed
}

/// Target Photo / Selected Photos: whose metadata the fields below show while several photos
/// are selected (`view.infoSelectedPhotos`).
fn metadata_for(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    ui.label(egui::RichText::new(crate::i18n::tr("Show Metadata For")).size(11.5).color(t.text_dim));
    ui.horizontal_wrapped(|ui| {
        let on = app.ui.settings.info_selected_photos;
        for (id, label, selected) in [("infoFor-target", "Target Photo", false), ("infoFor-selected", "Selected Photos", true)] {
            if text_button(ui, id, label, on == selected).clicked() && on != selected {
                let _ = app.run("view.infoSelectedPhotos", json!({"on": selected}));
            }
        }
    });
    ui.add_space(6.0);
}

/// Copyright Status: Unknown / Copyrighted / Public Domain (`xmpRights:Marked`). `mixed`: the
/// selected photos' statuses differ, and any status picked is given to all of them. A status
/// picked goes to the photos `targets`.
fn copyright_status(app: &mut LightcraftApp, ui: &mut egui::Ui, current: lightcraft_catalog::CopyrightStatus, mixed: bool, targets: &[PhotoId]) {
    let t = Tokens::get(ui.ctx());
    ui.label(egui::RichText::new(crate::i18n::tr("Copyright Status")).size(11.5).color(t.text_dim));
    let shown = crate::i18n::tr(if mixed { MIXED } else { current.label() });
    let r = crate::widgets::combo_box("info-copyright-status").selected_text(shown).show_ui(ui, |ui| {
        for st in lightcraft_catalog::CopyrightStatus::ALL {
            let on = !mixed && st == current;
            if ui.selectable_label(on, crate::i18n::tr(st.label())).clicked() && !on {
                let ids: Vec<u64> = targets.iter().map(|id| id.0).collect();
                let _ = app.run("photo.setMeta", json!({"ids": ids, "copyrightStatus": st.id()}));
            }
        }
    });
    crate::widgets::name_combo(&r.response, crate::i18n::tr("Copyright Status"));
    register(ui, "field:copyrightStatus", r.response.rect);
    ui.add_space(6.0);
}

/// A labeled metadata text field: the typed text lives in egui memory while focused and is
/// saved (photo.setMeta `key`) when the field loses focus. `mixed`: the selected photos' values
/// differ, so the field starts empty and says so, and is saved only when it was typed in (left
/// empty, it clears the field on every selected photo). The edit is saved to the photos `targets`.
#[allow(clippy::too_many_arguments)]
fn meta_field(app: &mut LightcraftApp, ui: &mut egui::Ui, label: &str, key: &str, value: &str, lines: usize, mixed: bool, targets: &[PhotoId]) {
    let t = Tokens::get(ui.ctx());
    ui.label(egui::RichText::new(crate::i18n::tr(label)).size(11.5).color(t.text_dim));
    let id = egui::Id::unique(("info-field", key));
    let value = if mixed { "" } else { value };
    let mut edit: FieldEdit = ui.data(|d| d.get_temp(id)).unwrap_or_else(|| FieldEdit::new(value, mixed, targets));
    let widget = format!("field:{key}");
    let field = if lines > 1 {
        crate::text_field::TextField::multiline(&widget, &mut edit.text).rows(lines)
    } else {
        crate::text_field::TextField::singleline(&widget, &mut edit.text)
    };
    let field = if mixed { field.hint(crate::i18n::tr(MIXED)) } else { field };
    let r = field.label(crate::i18n::tr(label)).width(f32::INFINITY).show(ui);
    edit.typed |= edit.text != edit.start;
    // the typed text outlives frames while it is edited (the photo's value is drawn otherwise)
    if r.editing {
        ui.data_mut(|d| d.insert_temp(id, edit.clone()));
    } else {
        ui.data_mut(|d| d.remove::<FieldEdit>(id));
    }
    // Return (one line) or leaving saves; Esc gives the edit up
    if r.committed() && (edit.text != edit.start || edit.clears()) {
        let _ = app.run("photo.setMeta", json!({"ids": edit.ids, key: edit.text}));
    }
    ui.add_space(6.0);
}

fn keywords(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId) {
    // (the box is the selection's: the active photo only says that there is one)
    if app.session.catalog.photo(id).is_none() {
        return;
    }
    let top = ui.cursor().top();
    if fold_header(app, ui, "keywording", "Keywords") {
        keywording(app, ui);
    }
    hold_list_in_place(ui, top);
    super::keyword_list::show(app, ui);
}

/// The Keywording box: the field that adds keywords, the selection's keywords, the keyword set,
/// the painter and the suggestions.
fn keywording(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    padded(ui, |ui| {
        let kid = egui::Id::unique("kw-input");
        let mut text = ui.data_mut(|d| d.get_temp::<String>(kid).unwrap_or_default());
        // the keywords every selected photo has: those only some have are still offered and suggested
        let selection = app.session.selected();
        let have: Vec<String> =
            app.caches.keyword_chips(&app.session.catalog, &selection).iter().filter(|c| c.on_all()).map(|c| c.path.clone()).collect();
        // while the box has the cursor it offers the library's keywords for what is being typed,
        // right under itself; it stays for a click on one of them, which takes the cursor away
        let field = crate::text_field::id("field:keyword");
        let oid = egui::Id::unique("kw-offers");
        let pass = ui.ctx().cumulative_pass_nr();
        // (remembered from the last frame: the text the highlight was for and the keyword it was
        // on, where the offers were drawn, and whether the box was in sight)
        let (typed_then, at_then, drawn, drawn_pass, hidden): (String, Option<String>, Rect, u64, bool) =
            ui.data(|d| d.get_temp(oid)).unwrap_or((String::new(), None, Rect::NOTHING, 0, false));
        let last_frame = drawn_pass.checked_add(1) == Some(pass);
        let clicking = last_frame
            && ui.input(|i| (i.pointer.any_down() || i.pointer.any_released()) && i.pointer.interact_pos().is_some_and(|p| drawn.contains(p)));
        let typed = super::keywording::typing(&text).to_string();
        // (none while the box is scrolled out of the panel: they would float over what is there)
        let offers = ((ui.memory(|m| m.has_focus(field)) || clicking) && !(last_frame && hidden))
            .then(|| app.caches.completions(&app.session.catalog, &have, &typed))
            .filter(|o| !o.keywords.is_empty() || o.create.is_some());
        // ↓ / ↑ move through the offers (none is highlighted at first, and ↑ from the first goes
        // back to none: Return then gives what was typed, as without offers); Return gives the
        // highlighted one. The highlight is on a keyword, not a line: it stays with it when the
        // library changes under it, and goes when the keyword does. With Shift or Alt the keys
        // are the text's own.
        let mut at = at_then.filter(|k| typed_then == typed && offers.as_ref().is_some_and(|o| o.keywords.contains(k)));
        let mut picked = None;
        if let Some(o) = offers.as_ref().filter(|o| !o.keywords.is_empty()) {
            let pressed = |ui: &mut egui::Ui, key: egui::Key| {
                ui.input_mut(|i| {
                    let plain =
                        |e: &egui::Event| matches!(e, egui::Event::Key { key: k, pressed: true, modifiers, .. } if *k == key && modifiers.is_none());
                    let found = i.events.iter().any(plain);
                    i.events.retain(|e| !plain(e));
                    found
                })
            };
            let line = at.as_ref().and_then(|k| o.keywords.iter().position(|x| x == k));
            if pressed(ui, egui::Key::ArrowDown) {
                let next = line.map_or(0, |a| a.saturating_add(1)).min(o.keywords.len().saturating_sub(1));
                at = o.keywords.get(next).cloned();
            } else if pressed(ui, egui::Key::ArrowUp) {
                at = line.and_then(|a| a.checked_sub(1)).and_then(|a| o.keywords.get(a).cloned());
            }
            if let Some(k) = &at
                && pressed(ui, egui::Key::Enter)
            {
                picked = Some(super::keywording::Picked::Keyword(k.clone()));
            }
        }
        // (after an offer was taken the box starts a new edit, empty: Esc has nothing to bring back)
        // (asked for on the frame before this one only: a box that wasn't drawn then, its panel
        // switched away, doesn't claim the cursor whenever it is next drawn)
        let bid = egui::Id::unique("kw-begin");
        let begin = ui.data_mut(|d| d.remove_temp::<u64>(bid)).is_some_and(|asked| asked.checked_add(1) == Some(pass));
        // Add Keywords (⌘K) asks for the cursor too, and for the box to be scrolled into sight
        let asked = app.ui.focus_keywords.take().is_some_and(|at| pass <= at.saturating_add(crate::state::FOCUS_KEYWORDS_PASSES));
        let r = crate::text_field::TextField::singleline("field:keyword", &mut text)
            .hint(crate::i18n::tr("Add keyword"))
            .width(f32::INFINITY)
            .begin_edit(begin || asked)
            .show(ui);
        if asked {
            r.response.scroll_to_me(None);
        }
        let in_sight = ui.clip_rect().contains(r.response.rect.center());
        let mut drawn = Rect::NOTHING;
        // (what isn't drawn can't be taken: Return on the frame the box scrolls out gives nothing)
        if !in_sight {
            picked = None;
        }
        if offers.is_none() {
            // (not over the offers)
            r.response.clone().on_hover_text(crate::i18n::tr("Separate several keywords with commas. Return gives them to the selected photos."));
        }
        if let Some(o) = offers.as_ref().filter(|_| in_sight) {
            let (clicked, rect) = super::keywording::offers_below(ui, r.response.rect, o, at.as_deref());
            drawn = rect;
            picked = picked.or(clicked);
        }
        ui.data_mut(|d| d.insert_temp(oid, (typed.clone(), at, drawn, pass, !in_sight)));
        // Return gives the keywords to the selected photos; Esc gives back what was there before
        // this edit (the shared field does)
        let returned = r.ending == Some(crate::text_field::Ending::Return) && !text.trim().is_empty();
        // a new name goes inside the default parent (Put New Keywords Inside This Keyword)
        let as_typed = |app: &LightcraftApp, text: &str| -> Vec<String> {
            text.split(',').map(|s| app.session.catalog.typed_keyword(s)).filter(|s| !s.is_empty()).collect()
        };
        let adding: Option<Vec<String>> = match picked {
            // an offer: it, and the keywords typed before the last comma
            Some(super::keywording::Picked::Keyword(k)) => {
                let mut kws = super::keywording::typed_before(&app.session.catalog, &text);
                kws.push(k);
                Some(kws)
            }
            Some(super::keywording::Picked::Typed) => Some(as_typed(app, &text)),
            None if returned => Some(as_typed(app, &text)),
            None => None,
        };
        if let Some(kws) = adding {
            // (refused, the text stays to be put right, and the refusal is said)
            match app.run("photo.setMeta", json!({"addKeywords": kws})) {
                Ok(_) => {
                    text.clear();
                    // an offer taken leaves the cursor in the box, for the next keyword
                    if !returned {
                        ui.data_mut(|d| d.insert_temp(bid, pass));
                    }
                }
                Err(e) => app.toast_error(ui.ctx(), e),
            }
        }
        ui.data_mut(|d| d.insert_temp(kid, text));
        ui.add_space(8.0);
        super::keywording::view_switch(app, ui);
        ui.add_space(4.0);
        super::keywording::scope(app, ui, &selection);
        if app.ui.keywording_view == crate::state::KeywordingView::Keywords {
            super::keywording::chip_row(app, ui, &selection);
        } else {
            super::keywording::names_row(app, ui);
        }
        ui.add_space(10.0);
        keyword_set(app, ui);
        ui.add_space(8.0);
        // the painter: click photos in the grid to give them (or take away) a keyword
        ui.horizontal(|ui| {
            let pid = egui::Id::unique("kw-painter");
            let painting = app.ui.keyword_painter.clone();
            let mut k: String = ui.data(|d| d.get_temp(pid)).unwrap_or_else(|| painting.clone().unwrap_or_default());
            // (fixed while painting: Stop first)
            let field = ui
                .add_enabled_ui(painting.is_none(), |ui| {
                    crate::text_field::TextField::singleline("field:keywordPainter", &mut k)
                        .hint(crate::i18n::tr("Keyword to paint"))
                        .width(130.0)
                        .show(ui)
                })
                .inner;
            ui.data_mut(|d| d.insert_temp(pid, k.clone()));
            let label = if painting.is_some() { "Stop" } else { "Paint" };
            let returned = field.ending == Some(crate::text_field::Ending::Return) && painting.is_none() && !k.trim().is_empty();
            if text_button(ui, "keywordPaint", label, painting.is_some())
                .on_hover_text(crate::i18n::tr("Click photos in the grid to toggle the keyword; Esc stops"))
                .clicked()
                || returned
            {
                let _ = app.run("tool.keywordPainter", json!({"keyword": if painting.is_some() { serde_json::Value::Null } else { json!(k) }}));
            }
        });
        ui.add_space(10.0);
        // suggestions: keywords used together with the selection's keywords, else the most used
        // ones (what matches the text being typed is offered under the box itself)
        let suggestions = (*app.caches.suggestions(&app.session.catalog, &have, "", 12)).clone();
        if !suggestions.is_empty() {
            ui.label(egui::RichText::new(crate::i18n::tr("Suggestions")).color(t.text_dim));
            // grouped: the keyword's own name, its whole path in the tooltip
            let grouped = app.ui.settings.keyword_chips == crate::state::KeywordChips::Grouped;
            let names = if grouped { super::keywording::suggestion_names(&suggestions) } else { Vec::new() };
            ui.horizontal_wrapped(|ui| {
                if grouped {
                    ui.spacing_mut().item_spacing = vec2(6.0, 4.0);
                }
                for (i, k) in suggestions.into_iter().enumerate() {
                    let whole = k.replace('|', " › ");
                    let name = names.get(i).unwrap_or(&whole);
                    let r = ui.add(egui::Button::new(egui::RichText::new(format!("+ {name}")).color(t.text_label)).corner_radius(10.0));
                    let r = if grouped { r.on_hover_text(&whole) } else { r };
                    register(ui, format!("kwSuggest:{k}"), r.rect);
                    if r.clicked() {
                        let _ = app.run("photo.setMeta", json!({"addKeywords": [k]}));
                    }
                }
            });
        }
    });
}

/// Keeps the Keyword List where it is on screen when the Keywords box above it (starting at
/// `top`) grows or shrinks: a keyword ticked or unticked in the list, another selection. Without it
/// a long tree slides away from under the pointer by a row of chips each time. It holds only while
/// the box's top is scrolled out of sight (at the top of the panel the box is watched growing) and
/// while the change does not come from the box itself (the pointer on it, or typing in its field).
fn hold_list_in_place(ui: &mut egui::Ui, top: f32) {
    let height = ui.cursor().top() - top;
    let key = egui::Id::unique("kw-box-height");
    let pass = ui.ctx().cumulative_pass_nr();
    // (the height drawn on the pass before: one from an earlier visit to the panel says nothing)
    let before = ui.data(|d| d.get_temp::<(f32, u64)>(key)).filter(|&(_, drawn)| drawn.checked_add(1) == Some(pass)).map(|(h, _)| h);
    ui.data_mut(|d| d.insert_temp(key, (height, pass)));
    let Some(before) = before else { return };
    let grew = height - before;
    if !grew.is_finite() || grew.abs() < 0.5 || top >= ui.clip_rect().top() - 0.5 {
        return;
    }
    // (only the part of it in sight: the pointer on the top bar above a half-hidden box isn't on it)
    let old_box = Rect::from_x_y_ranges(ui.max_rect().x_range(), top..=top + before).intersect(ui.clip_rect());
    let on_box = ui.input(|i| i.pointer.latest_pos()).is_some_and(|p| old_box.contains(p));
    let typing = ui.memory(|m| m.has_focus(crate::text_field::id("field:keyword")));
    if on_box || typing {
        return;
    }
    ui.data_mut(|d| d.insert_temp(egui::Id::unique(HOLD), (grew, pass)));
    // (drawn again at once at the new offset, so the list never shows a frame out of place)
    ui.ctx().request_discard("the Keyword List holds its place");
}

/// The keyword set: pick a set, then nine buttons (⌥1–⌥9) that toggle its keywords on the
/// selected photos; "Save as Set…" keeps the current nine under a name.
fn keyword_set(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    // on: every selected photo has it (⌥1–⌥9 then take it off them); partly on: only some do
    let selection = app.session.selected();
    let chips = app.caches.keyword_chips(&app.session.catalog, &selection);
    let t = Tokens::get(ui.ctx());
    let sets = lightcraft_engine::cmd::keywords::keyword_sets_json(&app.session);
    let current = sets["current"].as_str().unwrap_or_default().to_string();
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(crate::i18n::tr("Keyword Set")).color(t.text_dim));
        let combo = crate::widgets::combo_box("kw-set")
            .selected_text(crate::i18n::builtin_label(&current, current == lightcraft_engine::cmd::keywords::RECENT))
            .show_ui(ui, |ui| {
                for set in sets["sets"].as_array().into_iter().flatten() {
                    let name = set["name"].as_str().unwrap_or_default();
                    if ui
                        .selectable_label(name == current, crate::i18n::builtin_label(name, name == lightcraft_engine::cmd::keywords::RECENT))
                        .clicked()
                    {
                        let _ = app.run("keyword.useSet", json!({"name": name}));
                    }
                }
                ui.separator();
                // Edit Set…: the current nine, slot by slot (named, Recent Keywords becomes a set)
                let edit = ui.button(crate::i18n::tr("Edit Set…"));
                register(ui, "keywordSetMenu:edit", edit.rect);
                if edit.clicked() {
                    let mut slots: Vec<String> =
                        sets["keywords"].as_array().into_iter().flatten().filter_map(|k| k.as_str().map(str::to_string)).collect();
                    slots.resize(9, String::new());
                    let named = current != lightcraft_engine::cmd::keywords::RECENT;
                    app.ui.dialog = Some(crate::state::Dialog::KeywordSet {
                        replaces: named.then(|| current.clone()),
                        name: if named { current.clone() } else { String::new() },
                        slots,
                        as_new: false,
                    });
                }
                if ui.button(crate::i18n::tr("Save Current Keywords as Set…")).clicked() {
                    app.ui.dialog = Some(crate::state::Dialog::TextPrompt {
                        title: "Save Keyword Set".into(),
                        hint: "Set name".into(),
                        value: String::new(),
                        command: "keyword.saveSet".into(),
                        params: json!({}),
                        key: "name".into(),
                    });
                }
                if current != lightcraft_engine::cmd::keywords::RECENT
                    && ui.button(crate::i18n::tr_format!("Delete “{current}”", current = current)).clicked()
                {
                    let _ = app.run("keyword.deleteSet", json!({"name": current}));
                }
            });
        crate::widgets::name_combo(&combo.response, crate::i18n::tr("Keyword Set"));
        register(ui, "keywordSetCombo", combo.response.rect);
    });
    let kws: Vec<String> = sets["keywords"].as_array().into_iter().flatten().filter_map(|k| k.as_str().map(str::to_string)).collect();
    if kws.is_empty() {
        ui.label(egui::RichText::new(crate::i18n::tr("Keywords you add appear here; ⌥1–⌥9 apply them.")).color(t.text_dim));
        return;
    }
    let bw = ((ui.available_width() - 8.0) / 3.0).floor().max(40.0);
    egui::Grid::new("kw-set-grid").num_columns(3).spacing([4.0, 4.0]).show(ui, |ui| {
        for (i, k) in kws.iter().enumerate() {
            // an empty slot: an idle button, so the others keep their ⌥ keys
            if k.is_empty() {
                let r = ui.add_enabled_ui(false, |ui| ui.add_sized([bw, 22.0], egui::Button::new(""))).inner;
                register(ui, format!("kwSetEmpty:{}", i + 1), r.rect);
                if i % 3 == 2 {
                    ui.end_row();
                }
                continue;
            }
            let chip = chips.iter().find(|c| lightcraft_catalog::keywords::same(&c.path, &lightcraft_catalog::keywords::clean(k)));
            let on = chip.is_some_and(|c| c.on_all());
            let some = chip.is_some() && !on;
            let short = format!("{}{}", k.rsplit('|').next().unwrap_or(k), if some { " *" } else { "" });
            let r = ui
                .add_sized(
                    [bw, 22.0],
                    egui::Button::new(egui::RichText::new(short).size(11.5).color(if on { t.text } else { t.text_label })).selected(on).truncate(),
                )
                .on_hover_text(format!("{} — ⌥{}", k.replace('|', " › "), i + 1));
            register(ui, format!("kwSet:{}", i + 1), r.rect);
            if on {
                register(ui, format!("kwSetOn:{}", i + 1), r.rect);
            } else if some {
                register(ui, format!("kwSetSome:{}", i + 1), r.rect);
            }
            if r.clicked() {
                let _ = app.run("keyword.toggleFromSet", json!({"index": i + 1}));
            }
            if i % 3 == 2 {
                ui.end_row();
            }
        }
    });
}

fn versions(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId) {
    let Some(p) = app.session.catalog.photo(id).cloned() else { return };
    header(ui, "Versions");
    let t = Tokens::get(ui.ctx());
    padded(ui, |ui| {
        if text_button(ui, "versionCreate", crate::i18n::tr("Create Version"), false).clicked() {
            let _ = app.run("version.create", json!({}));
        }
        ui.add_space(8.0);
        // Named (made by you) and Auto (made by LightCraft) versions
        let tab_id = egui::Id::unique("versions-tab");
        let mut auto: bool = ui.data(|d| d.get_temp(tab_id)).unwrap_or(false);
        let named_n = p.versions.iter().filter(|v| !v.auto).count();
        let items = [("Named", "named"), ("Auto", "auto")];
        let labels =
            [crate::i18n::tr_format!("Named ({named_n})", named_n = named_n), crate::i18n::tr_format!("Auto ({})", p.versions.len() - named_n)];
        let items: Vec<(&str, &str)> = items.iter().zip(&labels).map(|((_, k), l)| (l.as_str(), *k)).collect();
        if let Some(i) = crate::widgets::segmented(ui, "versionsTab", &items, Some(auto as usize), 2) {
            auto = i == 1;
            ui.data_mut(|d| d.insert_temp(tab_id, auto));
        }
        ui.add_space(6.0);
        let shown: Vec<(usize, &lightcraft_catalog::Version)> = p.versions.iter().enumerate().filter(|(_, v)| v.auto == auto).collect();
        if shown.is_empty() {
            let msg = if auto { "No automatic versions." } else { "No versions yet. Create one to keep this look." };
            ui.label(egui::RichText::new(crate::i18n::tr(msg)).color(t.text_dim));
        }
        // newest first
        for (i, v) in shown.into_iter().rev() {
            let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 58.0), Sense::click());
            crate::widgets::announce(&resp, format!("{} {}", crate::i18n::tr("Version"), i + 1));
            register(ui, format!("version:{i}"), r);
            let current = *v.settings == *p.develop;
            if resp.hovered() {
                ui.painter().rect_filled(r, 4.0, t.hover.gamma_multiply(0.7));
            }
            // thumbnail of the version's look
            let tr = Rect::from_min_size(r.min + vec2(4.0, 5.0), vec2(72.0, 48.0));
            ui.painter().rect_filled(tr, 2.0, t.canvas);
            if let Some(job) = app.session.variant_job(id, &v.settings, 160)
                && let Some(tex) = app.renderer.variant(job)
            {
                ui.painter().image(tex.tex.id(), tr, crate::panels::presets::cover_uv(tr, tex.size), egui::Color32::WHITE);
            }
            let x = tr.right() + 10.0;
            ui.painter().with_clip_rect(r.with_max_x(r.right() - 24.0)).text(
                pos2(x, r.top() + 18.0),
                Align2::LEFT_CENTER,
                &v.name,
                t.semibold(13.0),
                t.text,
            );
            let painter = ui.painter().with_clip_rect(r);
            painter.text(pos2(x, r.top() + 38.0), Align2::LEFT_CENTER, short_time(&v.created), t.font(11.0), t.text_dim);
            if current {
                paint(ui.painter(), Rect::from_center_size(pos2(r.right() - 14.0, r.center().y), vec2(14.0, 14.0)), Icon::Check, t.accent);
            }
            // resting on a version shows it in the loupe (nothing is changed)
            if resp.hovered() && !current {
                app.hover_preview =
                    Some(crate::HoverPreview { label: crate::i18n::tr_format!("Version: {}", v.name), settings: (*v.settings).clone() });
            }
            if resp.double_clicked() {
                crate::panels::dialogs::prompt(app, "Rename Version", "Version name", &v.name, "version.rename", json!({"index": i}), "name");
            } else if resp.clicked() && !current {
                let _ = app.run("version.restore", json!({"index": i}));
            }
            let resp = resp.on_hover_text(crate::i18n::tr(if current {
                "The photo has these settings"
            } else {
                "Click to restore · double-click to rename"
            }));
            crate::widgets::context_menu(&resp, |ui| {
                if ui.button(crate::i18n::tr("Restore")).clicked() {
                    let _ = app.run("version.restore", json!({"index": i}));
                }
                if ui.button(crate::i18n::tr("Update with Current Settings")).clicked() {
                    let _ = app.run("version.update", json!({"index": i}));
                }
                if ui.button(crate::i18n::tr("Rename…")).clicked() {
                    crate::panels::dialogs::prompt(app, "Rename Version", "Version name", &v.name, "version.rename", json!({"index": i}), "name");
                }
                if ui.button(crate::i18n::tr("Set as Before")).clicked() {
                    let _ = app.run("beforeAfter.setBefore", json!({"source": "version", "name": v.name}));
                }
                ui.separator();
                if ui.button(crate::i18n::tr("Delete")).clicked() {
                    let _ = app.run("version.delete", json!({"index": i}));
                }
            });
        }
    });
}

/// "Sep 30, 2026, 12:00 PM" from an ISO time.
fn short_time(iso: &str) -> String {
    // Other languages use their own date patterns (`i18n::display_time`).
    if crate::i18n::language() != crate::i18n::Locale::En {
        return crate::i18n::display_time(iso);
    }
    let long = lightcraft_catalog::dates::display_time(iso);
    // "September 30, 2026 at 12:00:00 PM" → month abbreviated, seconds dropped
    let (date, time) = long.split_once(" at ").map_or((long.as_str(), None), |(d, t)| (d, Some(t)));
    let date = match date.split_once(' ') {
        Some((m, rest)) => format!("{} {rest}", m.chars().take(3).collect::<String>()),
        None => date.to_string(),
    };
    match time.and_then(|t| t.rsplit_once(' ')) {
        Some((hms, ampm)) => format!("{date}, {} {ampm}", hms.rsplit_once(':').map_or(hms, |x| x.0)),
        None => date,
    }
}

/// How a History row is drawn: the step the settings are at is highlighted, the steps after it
/// (still there to return to until a new edit) are greyed out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HistoryRow {
    Done,
    Current,
    Undone,
}

fn history_row(i: usize, cursor: Option<usize>) -> HistoryRow {
    match cursor {
        Some(c) if i == c => HistoryRow::Current,
        Some(c) if i > c => HistoryRow::Undone,
        _ => HistoryRow::Done,
    }
}

fn activity(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId) {
    let Some(p) = app.session.catalog.photo(id).cloned() else { return };
    header(ui, "History");
    let t = Tokens::get(ui.ctx());
    padded(ui, |ui| {
        if p.history.is_empty() {
            ui.label(egui::RichText::new(crate::i18n::tr("No edits yet.")).color(t.text_dim));
        }
        let cursor = p.history_index();
        for (i, h) in p.history.iter().enumerate().rev() {
            let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::click());
            crate::widgets::announce(&resp, crate::i18n::tr(&h.label));
            register(ui, format!("history:{i}"), r);
            let row = history_row(i, cursor);
            if row == HistoryRow::Current {
                ui.painter().rect_filled(r, 3.0, t.pressed);
            } else if resp.hovered() {
                ui.painter().rect_filled(r, 3.0, t.hover);
            }
            let (icon, text) = if row == HistoryRow::Undone { (t.text_disabled, t.text_disabled) } else { (t.icon, t.text_label) };
            paint(ui.painter(), Rect::from_min_size(r.min + vec2(0.0, 4.0), vec2(16.0, 16.0)), Icon::Clock, icon);
            ui.painter().text(
                pos2(r.left() + 24.0, r.center().y),
                Align2::LEFT_CENTER,
                crate::i18n::history_label(&h.label, &app.session.presets),
                t.font(12.5),
                text,
            );
            if resp.clicked() {
                let _ = app.run("history.restore", json!({"index": i}));
            }
            crate::widgets::context_menu(&resp, |ui| {
                if ui.button(crate::i18n::tr("Copy History Step Settings to Before")).clicked() {
                    let _ = app.run("beforeAfter.setBefore", json!({"source": "history", "index": i}));
                }
                if ui.button(crate::i18n::tr("Create Version from Step")).clicked() {
                    let _ = app.run("history.restore", json!({"index": i}));
                    let _ = app.run("version.create", json!({"name": h.label}));
                }
                ui.separator();
                if ui.button(crate::i18n::tr("Clear History")).clicked() {
                    let _ = app.run("history.clear", json!({}));
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    // Feature: the History panel greys out the steps after the cursor
    #[test]
    fn history_rows_after_the_cursor_are_greyed() {
        use super::{HistoryRow, history_row};
        let rows: Vec<_> = (0..4).map(|i| history_row(i, Some(1))).collect();
        assert_eq!(rows, [HistoryRow::Done, HistoryRow::Current, HistoryRow::Undone, HistoryRow::Undone]);
        assert_eq!(history_row(3, Some(3)), HistoryRow::Current, "at the latest step nothing is greyed");
        assert_eq!(history_row(0, None), HistoryRow::Done);
    }

    // Feature: the aspect button tells what is locked
    #[test]
    fn aspect_label_names_presets_in_either_orientation() {
        assert_eq!(super::aspect_label(None, Some(1.5)), "Free");
        assert_eq!(super::aspect_label(Some((1600, 900)), None), "16 × 9");
        assert_eq!(super::aspect_label(Some((900, 1600)), None), "16 × 9", "portrait 16:9");
        assert_eq!(super::aspect_label(Some((400, 500)), None), "4 × 5 / 8 × 10");
    }

    #[test]
    fn aspect_label_prefers_original_over_a_coinciding_preset_and_never_shows_pixel_sizes() {
        assert_eq!(super::aspect_label(Some((300, 200)), Some(1.5)), "Original");
        assert_eq!(super::aspect_label(Some((600_000, 400_000)), Some(1.5)), "Original");
        assert_eq!(super::aspect_label(Some((137, 100)), Some(1.5)), "1.37 : 1");
        assert_eq!(super::aspect_label(Some((0, 5)), None), "Free");
    }

    #[test]
    fn short_times() {
        assert_eq!(super::short_time("2026-09-30T12:00:05"), "Sep 30, 2026, 12:00 PM");
        assert_eq!(super::short_time("2026-01-02"), "Jan 2, 2026");
    }
}
