//! The right-hand panel next to the tool strip: Edit, Crop, Remove, Masking, Red Eye, Info,
//! Keywords, Versions, Activity.

use egui::{Align2, Rect, Sense, pos2, vec2};
use lcc_catalog::PhotoId;
use serde_json::json;

use crate::LccApp;
use crate::icons::{Icon, paint};
use crate::state::RightPanel;
use crate::text_field::{Ending, TextField};
use crate::theme::Tokens;
use crate::widgets::{divider, register, slider, text_button};
use lcc_engine::cmd::crop_aspects::{BUILTIN_ASPECTS, CustomAspect};

pub fn show(app: &mut LccApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let frame = egui::Frame::NONE.fill(t.chrome).stroke(egui::Stroke::new(1.0, t.divider));
    frame.show(ui, |ui| {
        // (taken on every pass the panel is drawn; one from earlier than the pass before is forgotten)
        let held = held_offset(ui.ctx());
        let Some(id) = app.session.active() else {
            // the Keyword List is the library's, not a photo's: it is there without a selection
            if app.ui.right == RightPanel::Keywords {
                egui::ScrollArea::vertical().id_salt("right-scroll").auto_shrink([false, false]).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    if fold_header(app, ui, "keywording", "Keywords") {
                        padded(ui, |ui| {
                            ui.label(egui::RichText::new(crate::i18n::tr("Select photos to give them keywords.")).color(t.text_dim));
                        });
                    }
                    super::keyword_list::show(app, ui);
                    crate::widgets::follow_focus(ui);
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
        // Edit: Lightroom Classic's Previous / Sync… bar under the panels, outside the scrolling
        let full = ui.available_rect_before_wrap();
        let footer = (app.ui.right == RightPanel::Edit).then(|| Rect::from_min_max(pos2(full.left(), full.bottom() - SYNC_BAR_H), full.max));
        let body = footer.map_or(full, |f| Rect::from_min_max(full.min, pos2(full.right(), f.top())));
        ui.scope_builder(egui::UiBuilder::new().max_rect(body), |ui| {
            let mut area = egui::ScrollArea::vertical().id_salt("right-scroll").auto_shrink([false, false]);
            if let Some(offset) = held {
                area = area.vertical_scroll_offset(offset);
            }
            // (where the pass starts: by its end egui may have pulled a shrunken panel's offset back)
            let area_id = ui.data(|d| d.get_temp::<egui::Id>(egui::Id::new(AREA)));
            let start = held.or_else(|| area_id.and_then(|a| egui::scroll_area::State::load(ui.ctx(), a)).map(|s| s.offset.y));
            ui.data_mut(|d| match start {
                Some(o) => {
                    d.insert_temp(egui::Id::new(OFFSET), o);
                }
                None => d.remove::<f32>(egui::Id::new(OFFSET)),
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
                    RightPanel::Info => super::metadata::show(app, ui, id),
                    RightPanel::Keywords => keywords(app, ui, id),
                    RightPanel::Versions => versions(app, ui, id),
                    RightPanel::Activity => activity(app, ui, id),
                    RightPanel::None => {}
                }
                crate::widgets::follow_focus(ui);
            });
            ui.data_mut(|d| d.insert_temp(egui::Id::new(AREA), scrolled.id));
        });
        // drawn after the panels: a pending numeric edit commits on blur before Reset takes precedence
        if let Some(f) = footer {
            ui.scope_builder(egui::UiBuilder::new().max_rect(f), |ui| sync_bar(app, ui, f));
        }
    });
}

/// Height of the Previous / Sync… bar under the Edit panel.
const SYNC_BAR_H: f32 = 44.0;

/// Lightroom Classic's bar under the Develop panels: the Auto Sync switch, then Previous (one photo:
/// paste the previous photo's settings), Sync… (several: Synchronize Settings) or Auto Sync (while
/// it is on: edits already go to every selected photo; a click turns it off).
fn sync_bar(app: &mut LccApp, ui: &mut egui::Ui, r: Rect) {
    let t = Tokens::get(ui.ctx());
    ui.painter().rect_filled(r, 0.0, t.chrome);
    ui.painter().hline(r.x_range(), r.top() + 0.5, egui::Stroke::new(1.0, t.divider));
    let auto = app.session.auto_sync;
    let several = app.session.selection.ids.len() > 1;
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.add_space(16.0);
        ui.spacing_mut().item_spacing.x = 8.0;
        // the switch
        let (sr, resp) = ui.allocate_exact_size(vec2(30.0, 16.0), Sense::click());
        register(ui.ctx(), "button:autoSyncSwitch", sr);
        resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, auto, crate::i18n::tr("Auto Sync")));
        let p = ui.painter();
        p.rect_filled(sr, 8.0, if auto { t.accent } else { t.divider });
        let knob = if auto { sr.right() - 8.0 } else { sr.left() + 8.0 };
        p.circle_filled(pos2(knob, sr.center().y), 6.0, t.text);
        let tip = crate::i18n::tr(if auto { "Auto Sync is on: edits apply to every selected photo" } else { "Turn on Auto Sync" });
        if resp.on_hover_text(tip).clicked() {
            let _ = app.run("develop.autoSync", json!({"on": !auto}));
        }
        let (id, label) = match (auto, several) {
            (true, _) => ("syncBar", "Auto Sync"),
            (false, true) => ("syncBar", "Sync…"),
            (false, false) => ("previousBar", "Previous"),
        };
        if text_button(ui, id, crate::i18n::tr(label), auto).clicked() {
            let _ = match (auto, several) {
                (true, _) => app.run("develop.autoSync", json!({"on": false})),
                (false, true) => app.run("dialog.syncSettings", json!({})),
                (false, false) => app.run("develop.pastePrevious", json!({})),
            };
        }
        // Lightroom Classic: Reset at the right end of the bar
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(8.0);
            if text_button(ui, "reset", crate::i18n::tr("Reset"), false)
                .on_hover_text(super::edit::reset_tip(&app.ui.settings.keymap, ui.ctx().os() == egui::os::OperatingSystem::Mac))
                .clicked()
            {
                let _ = app.run("develop.reset", json!({}));
            }
        });
    });
}

/// The Classic Metadata panel (Library's right column).
pub fn metadata_panel(app: &mut LccApp, ui: &mut egui::Ui) {
    match app.session.active() {
        Some(id) => super::metadata::show(app, ui, id),
        None => {
            ui.label(crate::i18n::tr("No photo selected"));
        }
    }
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
    let (grew, asked) = ctx.data_mut(|d| d.remove_temp::<(f32, u64)>(egui::Id::new(HOLD)))?;
    if asked.checked_add(1) != Some(ctx.cumulative_pass_nr()) {
        return None;
    }
    let offset = ctx.data(|d| d.get_temp::<f32>(egui::Id::new(OFFSET)))?;
    Some((offset + grew).max(0.0)).filter(|o| o.is_finite())
}

pub fn header(ui: &mut egui::Ui, title: &str) {
    let t = Tokens::get(ui.ctx());
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::hover());
    ui.painter().text(pos2(r.left() + 24.0, r.center().y + 2.0), Align2::LEFT_CENTER, crate::i18n::tr(title), t.semibold(15.0), t.text);
}

/// A panel title that folds its section shut and open again (`section:{id}`; kept in
/// [`crate::state::UiState::folded_sections`]). Says whether the section is open.
pub fn fold_header(app: &mut LccApp, ui: &mut egui::Ui, id: &str, title: &str) -> bool {
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::click());
    let title = crate::i18n::tr(title);
    fold_title(app, ui, id, r, &resp, title)
}

/// [`fold_header`] for a title whose room is already taken (`r`, answering as `resp`).
pub fn fold_title(app: &mut LccApp, ui: &egui::Ui, id: &str, r: Rect, resp: &egui::Response, title: &str) -> bool {
    let t = Tokens::get(ui.ctx());
    let open = !app.ui.section_folded(id);
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::CollapsingHeader, true, open, title));
    register(ui.ctx(), format!("section:{id}"), r);
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

/// What the aspect button says for the stored lock (`aspect` = width × 100, height × 100):
/// "Original" for the photo's own shape, else the name of a ratio the user saved, else a built-in
/// ratio's name, when the ratio matches one in either orientation; otherwise the ratio as
/// `1.37 : 1`.
pub(crate) fn aspect_label(aspect: Option<(u32, u32)>, original: Option<f64>, custom: &[CustomAspect]) -> String {
    let Some((w, h)) = aspect else { return "Free".into() };
    if w == 0 || h == 0 {
        return "Free".into();
    }
    let r = w as f64 / h as f64;
    let is = |q: f64| q.is_finite() && q > 0.0 && ((r - q).abs() / q < 0.004 || (1.0 / r - q).abs() / q < 0.004);
    if original.is_some_and(is) {
        return "Original".into();
    }
    if let Some(a) = custom.iter().find(|a| is(a.aspect[0] / a.aspect[1])) {
        return a.name.clone();
    }
    for (label, key) in BUILTIN_ASPECTS {
        if let Some((x, y)) = key.split_once('x')
            && let (Ok(x), Ok(y)) = (x.parse::<f64>(), y.parse::<f64>())
            && is(x / y)
        {
            return label.to_string();
        }
    }
    format!("{:.2} : 1", r.max(1.0 / r))
}

/// "Custom" rows of the aspect menu: width × height with Apply (`crop.aspect` `[w, h]`), and a
/// name with Save, which keeps the ratio in the menu (`crop.saveAspect`) and applies it.
fn custom_aspect(app: &mut LccApp, ui: &mut egui::Ui) {
    let key = egui::Id::new("crop-custom-aspect");
    let (mut w, mut h, mut name): (String, String, String) =
        ui.data_mut(|d| d.get_temp(key)).unwrap_or_else(|| ("3".into(), "2".into(), String::new()));
    let (mut apply, mut save) = (false, false);
    ui.horizontal(|ui| {
        ui.label(crate::i18n::tr("Custom"));
        let a = TextField::singleline("field:cropCustomW", &mut w).label(crate::i18n::tr("Width")).width(34.0).select_on_focus(true).show(ui);
        ui.label("×");
        let b = TextField::singleline("field:cropCustomH", &mut h).label(crate::i18n::tr("Height")).width(34.0).select_on_focus(true).show(ui);
        let go = text_button(ui, "cropCustomApply", "Apply", false);
        apply = go.clicked() || a.ending == Some(Ending::Return) || b.ending == Some(Ending::Return);
    });
    ui.horizontal(|ui| {
        let n = TextField::singleline("field:cropCustomName", &mut name).hint(crate::i18n::tr("Name")).width(118.0).show(ui);
        let go = text_button(ui, "cropCustomSave", "Save", false);
        save = go.clicked() || n.ending == Some(Ending::Return);
    });
    // Accept `3`, `2.5` and `3,5`; anything else (zero, negative, text, too wide) is ignored.
    let num = |t: &str| t.trim().replace(',', ".").parse::<f64>().ok();
    let ratio = match (num(&w), num(&h)) {
        (Some(x), Some(y)) if lcc_engine::cmd::crop_aspects::valid_ratio(x, y) => Some((x, y)),
        _ => None,
    };
    if let Some((x, y)) = ratio {
        if save && !name.trim().is_empty() {
            if app.run("crop.saveAspect", json!({"name": name.trim(), "aspect": [x, y]})).is_ok() {
                let _ = app.run("crop.aspect", json!({"aspect": [x, y]}));
                name.clear();
                ui.close();
            }
        } else if apply {
            let _ = app.run("crop.aspect", json!({"aspect": [x, y]}));
            ui.close();
        }
    }
    ui.data_mut(|d| d.insert_temp(key, (w, h, name)));
}

/// The aspect menu is in its Edit List mode (egui temp data, a `bool`).
const ASPECT_EDITING: &str = "crop-aspect-edit";

/// The aspect menu: the built-in ratios the user didn't hide, the saved ones, the Custom rows,
/// and Edit List, which turns the list into one to reorder, delete and hide in (the menu stays
/// open for it).
fn aspect_menu(app: &mut LccApp, ui: &mut egui::Ui) {
    let editing_id = egui::Id::new(ASPECT_EDITING);
    let editing = ui.data(|d| d.get_temp::<bool>(editing_id)).unwrap_or(false);
    if editing {
        // room for a name beside its ↑ ↓ Delete buttons
        ui.set_min_width(260.0);
    }
    for (label, key) in BUILTIN_ASPECTS {
        let hidden = app.session.crop_aspects.is_hidden(key);
        if editing {
            if matches!(key, "free" | "original") {
                // always in the menu: shown ticked, and not to be unticked
                ui.add_enabled(false, egui::Checkbox::new(&mut true, crate::i18n::tr(label)));
                continue;
            }
            let mut shown = !hidden;
            let r = ui.checkbox(&mut shown, crate::i18n::tr(label));
            register(ui.ctx(), format!("check:cropAspectShow:{key}"), r.rect);
            if r.changed() {
                let _ = app.run("crop.hideAspect", json!({"aspect": key, "hidden": !shown}));
            }
        } else if !hidden {
            let r = ui.button(crate::i18n::tr(label));
            register(ui.ctx(), format!("cropAspectPreset:{key}"), r.rect);
            if r.clicked() {
                let _ = app.run("crop.aspect", json!({"aspect": key}));
                ui.close();
            }
        }
    }
    let custom = app.session.crop_aspects.custom.clone();
    if !custom.is_empty() {
        ui.separator();
    }
    let last = custom.len().saturating_sub(1);
    for (i, a) in custom.iter().enumerate() {
        if editing {
            ui.horizontal(|ui| {
                ui.label(&a.name);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if text_button(ui, &format!("cropAspectDelete-{i}"), "Delete", false).clicked() {
                        let _ = app.run("crop.deleteAspect", json!({"name": a.name}));
                    }
                    if i < last && text_button(ui, &format!("cropAspectDown-{i}"), "↓", false).clicked() {
                        let _ = app.run("crop.moveAspect", json!({"name": a.name, "to": i + 1}));
                    }
                    if i > 0 && text_button(ui, &format!("cropAspectUp-{i}"), "↑", false).clicked() {
                        let _ = app.run("crop.moveAspect", json!({"name": a.name, "to": i - 1}));
                    }
                });
            });
        } else {
            let r = ui.button(&a.name);
            register(ui.ctx(), format!("cropAspectSaved:{i}"), r.rect);
            if r.clicked() {
                let _ = app.run("crop.aspect", json!({"aspect": a.aspect}));
                ui.close();
            }
        }
    }
    ui.separator();
    if !editing {
        custom_aspect(app, ui);
        ui.separator();
    }
    if text_button(ui, "cropAspectEdit", if editing { "Done" } else { "Edit List" }, editing).clicked() {
        ui.data_mut(|d| d.insert_temp(editing_id, !editing));
    }
}

fn crop(app: &mut LccApp, ui: &mut egui::Ui, id: PhotoId) {
    let d = app.session.develop_of(id).unwrap_or_default();
    header(ui, "Crop");
    padded(ui, |ui| {
        let original = app.session.catalog.photo(id).map(|p| {
            let (w, h) = (p.width.max(1) as f64, p.height.max(1) as f64);
            if d.orientation.swaps_axes() { h / w } else { w / h }
        });
        ui.horizontal(|ui| {
            ui.label(crate::i18n::tr("Aspect Ratio"));
            let cur = aspect_label(app.crop_tool_aspect(), original, &app.session.crop_aspects.custom);
            let t = Tokens::get(ui.ctx());
            // a saved ratio's name is the user's own words: not looked up for a translation
            let saved = app.session.crop_aspects.custom.iter().any(|a| a.name == cur);
            let text = if saved { cur.as_str() } else { crate::i18n::tr(&cur) };
            let r = crate::widgets::dropdown(ui, "cropAspect", text, t.font(12.5), t.text);

            if r.clicked() {
                // the menu opens as a list to pick from, not in the middle of an edit
                ui.data_mut(|d| d.insert_temp(egui::Id::new(ASPECT_EDITING), false));
            }
            // picking a ratio closes it; editing the list or typing in its fields does not
            egui::Popup::menu(&r).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| aspect_menu(app, ui));
            let locked = app.crop_tool_aspect().is_some();
            let (icon, label) = if locked { (Icon::Link, "Aspect Locked") } else { (Icon::Unlink, "Lock Aspect") };
            if cmd_icon(app, ui, "cropLock", icon, label, None, locked).clicked() {
                let _ = app.run("crop.aspect", json!({"aspect": "toggle"}));
            }
        });
        ui.add_space(6.0);
        // one row of icon buttons, as in the suite's transform panels (issue #972); each command's
        // name and shortcut show on hover
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let row = [
                ("cropRotateLeft", Icon::RotateLeft, "Rotate Left", "photo.rotateLeft", false),
                ("cropRotateRight", Icon::RotateRight, "Rotate Right", "photo.rotateRight", false),
                ("cropFlipH", Icon::FlipH, "Flip Horizontal", "photo.flipHorizontal", d.crop.flip_h),
                ("cropFlipV", Icon::FlipV, "Flip Vertical", "photo.flipVertical", d.crop.flip_v),
                ("cropSwapAspect", Icon::SwapAspect, "Rotate Crop Aspect", "crop.rotateAspect", false),
                ("cropReset", Icon::Reset, "Reset Crop", "crop.reset", false),
            ];
            for (id, icon, label, command, on) in row {
                if cmd_icon(app, ui, id, icon, label, Some(command), on).clicked() {
                    let _ = app.run(command, json!({}));
                }
            }
        });
    });
    let ang = lcc_develop::controls::find("crop.angle").copied();
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
                crate::widgets::register(ui.ctx(), "cropAngleField", r.rect);
                // a drag on the field is one undo step, like the slider's
                if r.drag_started() {
                    let _ = app.run("develop.beginInteraction", json!({"label": "Straighten"}));
                }
                // egui reports a change while text is typed even when the value isn't updated yet:
                // apply only a new angle
                // Esc keeps the old angle, but egui applies the typed text on the frame after the
                // one where Esc took the focus away: remember the Esc for that frame
                let cancel = egui::Id::new("cropAngleFieldEscaped");
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
            if cmd_icon(app, ui, "straightenTool", Icon::Straighten, "Straighten Tool", None, on)
                .on_hover_text(crate::i18n::tr("Drag along the horizon; double-click for Auto"))
                .clicked()
            {
                app.ui.tool = if on { String::new() } else { "straighten".into() };
            }
            if cmd_icon(app, ui, "straightenAuto", Icon::Auto, "Auto Straighten", Some("crop.autoStraighten"), false).clicked() {
                let _ = app.run("crop.autoStraighten", json!({}));
            }
        });
    });
    padded(ui, |ui| {
        ui.label(crate::i18n::tr("Overlay"));
        {
            use crate::state::CropOverlay as O;
            let opts = [
                (O::Thirds, Icon::OverlayThirds, "Thirds", "thirds"),
                (O::Grid, Icon::OverlayGrid, "Grid", "grid"),
                (O::Golden, Icon::OverlayGolden, "Golden", "golden"),
                (O::Diagonal, Icon::OverlayDiagonal, "Diagonal", "diagonal"),
                (O::Triangle, Icon::OverlayTriangle, "Triangle", "triangle"),
                (O::Spiral, Icon::OverlaySpiral, "Spiral", "spiral"),
                (O::None, Icon::OverlayNone, "None", "none"),
            ];
            let items: Vec<(Icon, &str, &str)> = opts.iter().map(|(_, i, l, k)| (*i, *l, *k)).collect();
            let active = opts.iter().position(|(o, ..)| *o == app.ui.crop_overlay);
            if let Some(i) = crate::widgets::icon_segmented(ui, "cropOverlay", &items, active)
                && let Some(o) = opts.get(i)
            {
                app.ui.crop_overlay = o.0;
            }
            if matches!(app.ui.crop_overlay, O::Triangle | O::Spiral)
                && cmd_icon(app, ui, "cropOverlayOrient", Icon::FlipH, "Flip Overlay (⇧O)", None, false).clicked()
            {
                let _ = app.run("view.cropOverlayOrientation", json!({}));
            }
        }
    });
    divider(ui);
    header(ui, "Geometry");
    // a clip takes the crop, straighten and flips only: Upright and the transform sliders are photo-only
    let video = crate::video_ui::photo_is_video(app, id);
    crate::video_ui::video_gate(ui, video, |ui| {
        padded(ui, |ui| {
            ui.label(crate::i18n::tr("Upright"));
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
                if let Some(i) = crate::widgets::segmented(ui, "upright", &items, active, 3) {
                    let (_, mode, key) = modes[i];
                    let _ = app.run("geometry.upright", json!({"mode": key}));
                    app.ui.tool = if mode == Upright::Guided { "guidedUpright".into() } else { String::new() };
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
        for spec in lcc_develop::controls::in_section(lcc_develop::Section::Geometry).filter(|c| c.id != "crop.angle") {
            let v = lcc_develop::controls::get(&d, spec.id).unwrap_or(spec.default);
            let out = slider(ui, spec, v, true, None);
            super::edit::apply_slider_out(app, spec, out, |app, v| app.run("develop.set", json!({"control": spec.id, "value": v})));
        }
    });
}

fn remove(app: &mut LccApp, ui: &mut egui::Ui, id: PhotoId) {
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
            lcc_develop::SpotMode::Heal => "Heal",
            lcc_develop::SpotMode::Clone => "Clone",
            lcc_develop::SpotMode::Remove => "Remove",
            lcc_develop::SpotMode::Fill => "Content-Aware",
        };
        super::edit::sub_title(ui, &crate::i18n::tr_format!("{mode} spot {} of {}", i + 1, d.spots.len(), mode = crate::i18n::tr(mode)));
    }
    let plain = |id: &'static str, label: &'static str, min: f64, max: f64, default: f64| lcc_develop::ControlSpec {
        id,
        label,
        section: lcc_develop::Section::Detail,
        min,
        max,
        default,
        step: 1.0,
        decimals: 0,
        track: lcc_develop::Track::Plain,
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
    let spec = lcc_develop::ControlSpec {
        id: "ui.spotsThreshold",
        label: "Threshold",
        section: lcc_develop::Section::Detail,
        min: 0.0,
        max: 100.0,
        default: 50.0,
        step: 1.0,
        decimals: 0,
        track: lcc_develop::Track::Plain,
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
pub(crate) fn tool_overlay_options(app: &mut LccApp, ui: &mut egui::Ui) {
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
                register(ui.ctx(), format!("toolOverlay:{}", m.name()), item.rect);
                if item.clicked() {
                    let _ = app.run("view.toolOverlay", json!({"mode": m.name()}));
                }
            }
        });
    });
}

fn red_eye(app: &mut LccApp, ui: &mut egui::Ui, id: PhotoId) {
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
        let Some(spec) = lcc_develop::controls::find(&cid) else { continue };
        let v = lcc_develop::controls::get(&d, &cid).unwrap_or(spec.default);
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

fn keywords(app: &mut LccApp, ui: &mut egui::Ui, id: PhotoId) {
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

/// The keyword entry, chips, keyword set, painter and suggestions for the selected photos (the
/// Classic shell's Keywording panel; the Keywords panel adds the Keyword List below them).
pub(crate) fn keywording(app: &mut LccApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    padded(ui, |ui| {
        let kid = egui::Id::new("kw-input");
        let mut text = ui.data_mut(|d| d.get_temp::<String>(kid).unwrap_or_default());
        // the keywords every selected photo has: those only some have are still offered and suggested
        let selection = app.session.selected();
        let have: Vec<String> =
            app.caches.keyword_chips(&app.session.catalog, &selection).iter().filter(|c| c.on_all()).map(|c| c.path.clone()).collect();
        // while the box has the cursor it offers the library's keywords for what is being typed,
        // right under itself; it stays for a click on one of them, which takes the cursor away
        let field = crate::text_field::id("field:keyword");
        let oid = egui::Id::new("kw-offers");
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
        let bid = egui::Id::new("kw-begin");
        let begin = ui.data_mut(|d| d.remove_temp::<u64>(bid)).is_some_and(|asked| asked.checked_add(1) == Some(pass));
        let r = crate::text_field::TextField::singleline("field:keyword", &mut text)
            .hint(crate::i18n::tr("Add keyword"))
            .width(f32::INFINITY)
            .begin_edit(begin)
            .show(ui);
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
        let as_typed = |app: &LccApp, text: &str| -> Vec<String> {
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
            let pid = egui::Id::new("kw-painter");
            let painting = app.ui.lib.painter.as_ref().filter(|p| p.kind == crate::libtools::PaintKind::Keywords).map(|p| p.value.clone());
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
                    register(ui.ctx(), format!("kwSuggest:{k}"), r.rect);
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
    let key = egui::Id::new("kw-box-height");
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
    ui.data_mut(|d| d.insert_temp(egui::Id::new(HOLD), (grew, pass)));
    // (drawn again at once at the new offset, so the list never shows a frame out of place)
    ui.ctx().request_discard("the Keyword List holds its place");
}

/// The keyword set: pick a set, then nine buttons (⌥1–⌥9) that toggle its keywords on the
/// selected photos; "Save as Set…" keeps the current nine under a name.
fn keyword_set(app: &mut LccApp, ui: &mut egui::Ui) {
    // on: every selected photo has it (⌥1–⌥9 then take it off them); partly on: only some do
    let selection = app.session.selected();
    let chips = app.caches.keyword_chips(&app.session.catalog, &selection);
    let t = Tokens::get(ui.ctx());
    let sets = lcc_engine::cmd::keywords::keyword_sets_json(&app.session);
    let current = sets["current"].as_str().unwrap_or_default().to_string();
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(crate::i18n::tr("Keyword Set")).color(t.text_dim));
        let combo = crate::widgets::combo_box("kw-set")
            .selected_text(crate::i18n::builtin_label(&current, current == lcc_engine::cmd::keywords::RECENT))
            .show_ui(ui, |ui| {
                for set in sets["sets"].as_array().into_iter().flatten() {
                    let name = set["name"].as_str().unwrap_or_default();
                    if ui.selectable_label(name == current, crate::i18n::builtin_label(name, name == lcc_engine::cmd::keywords::RECENT)).clicked() {
                        let _ = app.run("keyword.useSet", json!({"name": name}));
                    }
                }
                ui.separator();
                // Edit Set…: the current nine, slot by slot (named, Recent Keywords becomes a set)
                let edit = ui.button(crate::i18n::tr("Edit Set…"));
                register(ui.ctx(), "keywordSetMenu:edit", edit.rect);
                if edit.clicked() {
                    let mut slots: Vec<String> =
                        sets["keywords"].as_array().into_iter().flatten().filter_map(|k| k.as_str().map(str::to_string)).collect();
                    slots.resize(9, String::new());
                    let named = current != lcc_engine::cmd::keywords::RECENT;
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
                if current != lcc_engine::cmd::keywords::RECENT
                    && ui.button(crate::i18n::tr_format!("Delete “{current}”", current = current)).clicked()
                {
                    let _ = app.run("keyword.deleteSet", json!({"name": current}));
                }
            });
        register(ui.ctx(), "keywordSetCombo", combo.response.rect);
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
                register(ui.ctx(), format!("kwSetEmpty:{}", i + 1), r.rect);
                if i % 3 == 2 {
                    ui.end_row();
                }
                continue;
            }
            let chip = chips.iter().find(|c| lcc_catalog::keywords::same(&c.path, &lcc_catalog::keywords::clean(k)));
            let on = chip.is_some_and(|c| c.on_all());
            let some = chip.is_some() && !on;
            let short = format!("{}{}", k.rsplit('|').next().unwrap_or(k), if some { " *" } else { "" });
            let r = ui
                .add_sized(
                    [bw, 22.0],
                    egui::Button::new(egui::RichText::new(short).size(11.5).color(if on { t.text } else { t.text_label })).selected(on).truncate(),
                )
                .on_hover_text(format!("{} — ⌥{}", k.replace('|', " › "), i + 1));
            register(ui.ctx(), format!("kwSet:{}", i + 1), r.rect);
            if on {
                register(ui.ctx(), format!("kwSetOn:{}", i + 1), r.rect);
            } else if some {
                register(ui.ctx(), format!("kwSetSome:{}", i + 1), r.rect);
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

fn versions(app: &mut LccApp, ui: &mut egui::Ui, id: PhotoId) {
    header(ui, "Versions");
    versions_body(app, ui, id);
}

/// The Versions rows without the right column's own header: the fork's Develop column draws them
/// under its Classic "Versions" header (see `panels::classic_left`).
pub(crate) fn versions_body(app: &mut LccApp, ui: &mut egui::Ui, id: PhotoId) {
    let Some(p) = app.session.catalog.photo(id).cloned() else { return };
    let t = Tokens::get(ui.ctx());
    padded(ui, |ui| {
        if text_button(ui, "versionCreate", crate::i18n::tr("Create Version"), false).clicked() {
            let _ = app.run("version.create", json!({}));
        }
        ui.add_space(8.0);
        // Named (made by you) and Auto (made by the app) versions
        let tab_id = egui::Id::new("versions-tab");
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
        let shown: Vec<(usize, &lcc_catalog::Version)> = p.versions.iter().enumerate().filter(|(_, v)| v.auto == auto).collect();
        if shown.is_empty() {
            let msg = if auto { "No automatic versions." } else { "No versions yet. Create one to keep this look." };
            ui.label(egui::RichText::new(crate::i18n::tr(msg)).color(t.text_dim));
        }
        // newest first
        for (i, v) in shown.into_iter().rev() {
            let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 58.0), Sense::click());
            crate::widgets::announce(&resp, format!("{} {}", crate::i18n::tr("Version"), i + 1));
            register(ui.ctx(), format!("version:{i}"), r);
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
            resp.on_hover_text(crate::i18n::tr(if current { "The photo has these settings" } else { "Click to restore · double-click to rename" }))
                .context_menu(|ui| {
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
    let long = lcc_catalog::dates::display_time(iso);
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

fn activity(app: &mut LccApp, ui: &mut egui::Ui, id: PhotoId) {
    header(ui, "History");
    history_body(app, ui, id);
}

/// The heading of a History group: its Develop section, or for a step that spans several sections
/// (a preset, paste, sync…) the step itself.
fn group_title(section: Option<&str>, step_label: &str, presets: &[lcc_develop::Preset]) -> String {
    let name = match section {
        Some("basic") => "Basic",
        Some("curve") => "Tone Curve",
        Some("color") => "Color",
        Some("grading") => "Color Grading",
        Some("detail") => "Detail",
        Some("effects") => "Effects",
        Some("lens") => "Lens Corrections",
        Some("geometry") => "Geometry",
        Some("crop") => "Crop",
        Some("masking") => "Masking",
        Some("healing") => "Remove",
        Some("lensBlur") => "Lens Blur",
        Some("enhance") => "Enhance",
        Some("calibration") => "Calibration",
        Some("pluginFilters") => "Plug-in Filters",
        Some(other) => other,
        None => return crate::i18n::history_label(step_label, presets),
    };
    crate::i18n::tr(name).to_string()
}

/// The History rows without the right column's own header: the fork's Develop column draws them
/// under its Classic "History" header (see `panels::classic_left`). Two levels, newest first:
/// groups of consecutive steps on one section, each expanding (its chevron) to its steps. Going
/// back to a step and editing there keeps the later groups and replays them on top.
pub(crate) fn history_body(app: &mut LccApp, ui: &mut egui::Ui, id: PhotoId) {
    let Some(p) = app.session.catalog.photo(id).cloned() else { return };
    let t = Tokens::get(ui.ctx());
    padded(ui, |ui| {
        if p.history.is_empty() {
            ui.label(egui::RichText::new(crate::i18n::tr("No edits yet.")).color(t.text_dim));
        }
        let cursor = p.history_index();
        if p.history_cursor.is_some_and(|c| c.saturating_add(1) < p.history.len())
            && text_button(ui, "historyLatest", "Return to Latest", false).clicked()
        {
            let _ = app.run("history.latest", json!({}));
        }
        for g in lcc_engine::history_groups::groups(&p.history).into_iter().rev() {
            let several = g.end > g.start;
            let open_id = egui::Id::new(("historyGroupOpen", id.0, g.start));
            let open = several && ui.ctx().data(|d| d.get_temp::<bool>(open_id)).unwrap_or(false);
            let Some(newest) = p.history.get(g.end) else { continue };
            let first = p.history.get(g.start).map_or("", |h| h.label.as_str());
            let title = group_title(g.section.as_deref(), first, &app.session.presets);
            let title = if several { format!("{title} ({})", g.end - g.start + 1) } else { title };
            let holds_cursor = cursor.is_some_and(|c| (g.start..=g.end).contains(&c));
            // a closed group reads as its newest step: current when the cursor is inside it
            let row = if holds_cursor && !open { HistoryRow::Current } else { history_row(g.end, cursor) };
            let lead = if !several {
                Icon::Clock
            } else if open {
                Icon::ChevronDown
            } else {
                Icon::ChevronRight
            };
            let (r, resp) = history_row_ui(ui, &t, &title, lead, row, 0.0, true);
            register(ui.ctx(), format!("historyGroup:{}", g.start), r);
            if !several {
                register(ui.ctx(), format!("history:{}", g.end), r);
            }
            if resp.clicked() {
                // the chevron expands; the rest of the row goes to the group's newest step
                if several && resp.interact_pointer_pos().is_some_and(|pos| pos.x < r.left() + 20.0) {
                    ui.ctx().data_mut(|d| d.insert_temp(open_id, !open));
                } else {
                    let _ = app.run("history.restore", json!({"index": g.end}));
                }
            }
            step_menu(app, &resp, &newest.label, g.end);
            if !open {
                continue;
            }
            for i in (g.start..=g.end).rev() {
                let Some(h) = p.history.get(i) else { continue };
                let label = crate::i18n::history_label(&h.label, &app.session.presets);
                let (r, resp) = history_row_ui(ui, &t, &label, Icon::Clock, history_row(i, cursor), 20.0, false);
                register(ui.ctx(), format!("history:{i}"), r);
                if resp.clicked() {
                    let _ = app.run("history.restore", json!({"index": i}));
                }
                step_menu(app, &resp, &h.label, i);
            }
        }
    });
}

/// One History row (a group heading in semibold, or an indented step), drawn by its `row` state.
fn history_row_ui(ui: &mut egui::Ui, t: &Tokens, text: &str, icon: Icon, row: HistoryRow, indent: f32, heading: bool) -> (Rect, egui::Response) {
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::click());
    crate::widgets::announce(&resp, text);
    if row == HistoryRow::Current {
        ui.painter().rect_filled(r, 3.0, t.pressed);
    } else if resp.hovered() {
        ui.painter().rect_filled(r, 3.0, t.hover);
    }
    let (ic, tc) = if row == HistoryRow::Undone { (t.text_disabled, t.text_disabled) } else { (t.icon, t.text_label) };
    paint(ui.painter(), Rect::from_min_size(r.min + vec2(indent, 4.0), vec2(16.0, 16.0)), icon, ic);
    let font = if heading { t.semibold(12.5) } else { t.font(12.5) };
    ui.painter().text(pos2(r.left() + indent + 24.0, r.center().y), Align2::LEFT_CENTER, text, font, tc);
    (r, resp)
}

/// The context menu of History step `i` (labelled `label`).
fn step_menu(app: &mut LccApp, resp: &egui::Response, label: &str, i: usize) {
    resp.context_menu(|ui| {
        if ui.button(crate::i18n::tr("Copy History Step Settings to Before")).clicked() {
            let _ = app.run("beforeAfter.setBefore", json!({"source": "history", "index": i}));
        }
        if ui.button(crate::i18n::tr("Create Version from Step")).clicked() {
            let _ = app.run("history.restore", json!({"index": i}));
            let _ = app.run("version.create", json!({"name": label}));
        }
        ui.separator();
        if ui.button(crate::i18n::tr("Clear History")).clicked() {
            let _ = app.run("history.clear", json!({}));
        }
    });
}

/// A panel icon button for `label` (translated: the accessible name), its hover text the label plus
/// `command`'s current shortcut. Addressable as `button:{id}` (as the text button it replaced was)
/// and `icon:{id}`.
fn cmd_icon(app: &LccApp, ui: &mut egui::Ui, id: &str, icon: Icon, label: &str, command: Option<&str>, active: bool) -> egui::Response {
    let sc = command.and_then(|c| crate::shortcuts::shortcut_of(&app.ui.settings.keymap, c));
    let tip = crate::widgets::command_tooltip(ui, label, sc);
    let r = crate::widgets::icon_button_named(ui, id, icon, vec2(28.0, 24.0), active, true, crate::i18n::tr(label), &tip);
    register(ui.ctx(), format!("button:{id}"), r.rect);
    r
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
        assert_eq!(super::aspect_label(None, Some(1.5), &[]), "Free");
        assert_eq!(super::aspect_label(Some((1600, 900)), None, &[]), "16 × 9");
        assert_eq!(super::aspect_label(Some((900, 1600)), None, &[]), "16 × 9", "portrait 16:9");
        assert_eq!(super::aspect_label(Some((400, 500)), None, &[]), "4 × 5 / 8 × 10");
    }

    #[test]
    fn aspect_label_prefers_original_over_a_coinciding_preset_and_never_shows_pixel_sizes() {
        assert_eq!(super::aspect_label(Some((300, 200)), Some(1.5), &[]), "Original");
        assert_eq!(super::aspect_label(Some((600_000, 400_000)), Some(1.5), &[]), "Original");
        assert_eq!(super::aspect_label(Some((137, 100)), Some(1.5), &[]), "1.37 : 1");
        assert_eq!(super::aspect_label(Some((0, 5)), None, &[]), "Free");
    }

    #[test]
    fn short_times() {
        assert_eq!(super::short_time("2026-09-30T12:00:05"), "Sep 30, 2026, 12:00 PM");
        assert_eq!(super::short_time("2026-01-02"), "Jan 2, 2026");
    }
}
