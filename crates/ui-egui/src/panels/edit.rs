//! The Edit panel: histogram, Auto/B&W, profile, and the Light / Color / Effects / Detail / Optics
//! sections, built from the engine's control specs.

use craft::ui::choice::Checkbox;
use craft::ui::contract::{Bind, Props as _, Show as _};
use egui::epaint::{Mesh, Vertex};
use egui::{Align2, Color32, Pos2, Rect, RichText, Sense, Stroke, pos2, vec2};
use lightcraft_catalog::PhotoId;
use lightcraft_develop::{ControlSpec, DevelopSettings, Section, Track, WbMode, controls};
use lightcraft_geom::Point;
use lightcraft_pipeline::{PARAM_AMOUNTS, ParametricCurve};
use lightcraft_raster::Histogram;
use serde_json::{Value, json};

use crate::LightcraftApp;
use crate::icons::{Icon, paint};
use crate::render::Slot;
use crate::state::RightPanel;
use crate::theme::Tokens;
use crate::widgets::{BAND_COLORS, SliderOut, divider, flyout_row, hex, register, section_header, slider, text_button};

mod color_controls;
pub(crate) use color_controls::{frame_begin as color_frame_begin, prepare_keys as prepare_color_keys};
mod curve;
mod grading;
mod hdr_flow;
mod histogram;
mod lens_blur;
mod light_controls;
pub(crate) use light_controls::prepare_keys as prepare_light_keys;
mod mixer;
mod sliders;

use curve::curve_editor;
use grading::grading;
pub(crate) use grading::wheel_pointer_frame_end;
#[cfg(test)]
pub(crate) use grading::{WheelGrab, WheelLock, WheelMods, wheel_pick};
pub(crate) use histogram::histogram;
use histogram::soft_proofing;
use lens_blur::lens_blur;
use mixer::{mixer, point_color};
pub use sliders::{apply_slider_out, sub_title};
pub(crate) use sliders::{control, reload_preview_only};
use sliders::{hdr_row, header_buttons, profile_menu, quick_develop, reset_tip, section};

/// The source-info gate and existing merge command stay at the original photo's row.
/// A binding proposes only this pass's bool; no UI copy of the develop value is kept.
fn camera_local_tone_checkbox(ui: &mut egui::Ui, owner: PhotoId, current: bool) -> Option<bool> {
    use craft::behavior::Proposal;
    use craft::ui::contract::Sizing;

    let key = "check:profile.cameraLocalTone";
    let label = crate::i18n::tr("Camera local tone mapping");
    let tooltip = crate::i18n::tr("Apply the local tone mapping stored in the raw (Apple ProRAW), as the camera renders it");
    let checkbox = || {
        Checkbox::new(Bind::given(current))
            .id_salt(("camera-local-tone", owner))
            .label(label.to_owned())
            .tooltip(tooltip.to_owned())
            .sizing(Sizing::FillWidth)
    };
    if ui.is_sizing_pass() || craft::ui::runtime::measure::active(ui.ctx()) {
        // Measurement must not mount a live target or consume an old owner's release.
        let child = ui.new_child(egui::UiBuilder::new().max_rect(ui.available_rect_before_wrap()).sizing_pass().invisible());
        let size = checkbox().measure_in(&child, Proposal::Ideal);
        ui.allocate_exact_size(size, egui::Sense::hover());
        return None;
    }
    if crate::panels::settings::native_controls_active(ui.ctx()) {
        let mut on = current;
        let r = ui.checkbox(&mut on, label);
        crate::widgets::register(ui, key, r.rect);
        return r.on_hover_text(tooltip).changed().then_some(on).filter(|on| ui.is_enabled() && *on != current);
    }
    let shown = checkbox().show(ui);
    // The SDK's interaction rectangle includes the whole localized label, not just its mark.
    crate::widgets::register_response(key, shown.response.interact_rect, &shown.response);
    shown.events.committed().copied().filter(|on| ui.is_enabled() && *on != current)
}

/// One-shot SDK proposals: develop.merge remains the only owner of the photo value.
fn optics_checkbox(ui: &mut egui::Ui, key: &str, label: &str, value: bool) -> Option<bool> {
    let shown = Checkbox::new(Bind::given(value)).id_salt(key).label(crate::i18n::tr(label).to_owned()).show(ui);
    if ui.is_sizing_pass() || craft::ui::runtime::measure::active(ui.ctx()) {
        return None;
    }
    crate::widgets::register_response(key, shown.response.rect, &shown.response);
    shown.events.committed().copied().filter(|on| ui.is_enabled() && *on != value)
}

fn optics_checkboxes(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId, d: &DevelopSettings) -> bool {
    if let Some(ca) = optics_checkbox(ui, "check:optics.removeCA", "Remove Chromatic Aberration", d.optics.remove_ca) {
        let _ = app.run("develop.merge", json!({"settings": {"optics": {"remove_ca": ca}}, "label": "Remove CA"}));
    }
    let mut lp = d.optics.lens_profile;
    if let Some(on) = optics_checkbox(ui, "check:optics.lensProfile", "Enable Lens Corrections", lp) {
        lp = on;
        let ok = app.run("develop.merge", json!({"settings": {"optics": {"lens_profile": lp}}, "label": "Lens Corrections"})).is_ok();
        // Only a successfully enabled target may enter the existing deferred lookup path.
        if ok && lp {
            let without = crate::lens_db::to_look_up(&app.session, id);
            app.ui.lens_lookup_ids.extend(without);
        }
    }
    lp
}

/// One real radio arena owns the complete localized style labels; the engine still owns every edit.
fn vignette_style(
    app: &LightcraftApp,
    ui: &mut egui::Ui,
    owner: PhotoId,
    current: lightcraft_develop::VignetteStyle,
) -> Option<lightcraft_develop::VignetteStyle> {
    use craft::behavior::{NavShape, Proposal};
    use craft::ui::choice::{Radio, RadioGroup};
    use craft::ui::contract::ShowContainer as _;
    use lightcraft_develop::VignetteStyle as V;

    let styles = [
        (V::HighlightPriority, "Highlight", "highlightPriority"),
        (V::ColorPriority, "Color", "colorPriority"),
        (V::PaintOverlay, "Paint", "paintOverlay"),
    ];
    let active = styles.iter().position(|(style, _, _)| *style == current);
    let real = !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx());
    let interactive = real && ui.is_enabled() && app.session.active() == Some(owner);
    let picked = egui::Frame::NONE
        .inner_margin(egui::Margin { left: 24, right: 22, top: 2, bottom: 4 })
        .show(ui, |ui| {
            if !interactive {
                ui.disable();
            }
            if crate::panels::settings::native_controls_active(ui.ctx()) {
                return if real {
                    let items: Vec<(&str, &str)> = styles.iter().map(|(_, label, key)| (*label, *key)).collect();
                    crate::widgets::segmented(ui, "vignetteStyle", &items, active, 3)
                } else {
                    ui.allocate_exact_size(egui::vec2(ui.available_width(), 24.0), egui::Sense::hover());
                    None
                };
            }
            let width = ui.available_width().min((ui.clip_rect().right() - ui.cursor().left()).max(1.0)).max(1.0);
            let cell_width = ((width - 8.0) / 3.0).max(1.0);
            let mut activated = None;
            let mut body = |ui: &mut egui::Ui, _: &mut craft::ui::contract::BodyCx<'_>| {
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    for (index, (_, label, key)) in styles.iter().enumerate() {
                        ui.allocate_ui_with_layout(egui::vec2(cell_width, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                            ui.set_min_width(cell_width);
                            ui.set_max_width(cell_width);
                            let response = Radio::new(index as u8)
                                .id_salt(("vignette-style-choice", owner, key))
                                .label(crate::i18n::tr(label).to_owned())
                                .show(ui);
                            if !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx()) {
                                // rect is the mark; the actual SDK interaction rectangle includes its whole label.
                                crate::widgets::register_response(format!("button:vignetteStyle-{key}"), response.interact_rect, &response);
                                // The original action also runs when its current style is genuinely reactivated.
                                // Read that real response, never infer activation from a changed binding or focus.
                                if interactive && response.enabled() && response.clicked() {
                                    activated = Some(index);
                                }
                            }
                        });
                    }
                });
            };
            let selected = active.unwrap_or_default() as u8;
            let child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(egui::Rect::from_min_size(ui.cursor().min, egui::vec2(width, f32::INFINITY)))
                    .sizing_pass()
                    .invisible(),
            );
            let size = RadioGroup::new(crate::i18n::tr("Vignette").to_owned(), Bind::given(selected))
                .id_salt(("vignette-style", owner))
                .nav(NavShape::Flat)
                .enabled(interactive)
                .measure_in(&child, Proposal::Ideal, &mut body);
            let (rect, _) = ui.allocate_exact_size(egui::vec2(width, size.y.max(24.0)), egui::Sense::hover());
            if !real {
                // External SDK measurement can use a non-sizing egui Ui. Allocate, but never mount members.
                return None;
            }
            let shown = RadioGroup::new(crate::i18n::tr("Vignette").to_owned(), Bind::given(selected))
                .id_salt(("vignette-style", owner))
                .nav(NavShape::Flat)
                .enabled(interactive)
                .show_at(ui, rect, &mut body);
            shown.output.events.committed().copied().map(usize::from).or(activated)
        })
        .inner;
    if interactive && app.session.active() == Some(owner) { picked.and_then(|index| styles.get(index).map(|(style, _, _)| *style)) } else { None }
}

pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId) {
    light_controls::apply_pending(app, ui.ctx());
    color_controls::apply_pending(app, ui.ctx());
    let t = Tokens::get(ui.ctx());
    let d = app.session.develop_of(id).unwrap_or_default();
    // a raw shown from its embedded JPEG (preview only) gets the rendered-file white balance scale
    let raw = app.session.catalog.photo(id).is_some_and(|p| p.develops_raw() && !p.relative_wb());
    let mut d = d;
    if d.wb.mode == WbMode::AsShot && app.session.catalog.photo(id).is_some_and(|p| p.relative_wb()) {
        let wb = &mut std::sync::Arc::make_mut(&mut d).wb;
        wb.temp = 6500.0;
        wb.tint = 0.0;
    } else if raw {
        // the Kelvin sliders show the white balance in effect: As Shot reads the file's as-shot white (the stored
        // numbers can predate a decoder update, or the camera's matrices giving the file its Kelvin scale), a
        // preset its Kelvin value, and a value written on the old relative scale the Kelvin it renders as
        let info = app.session.source_info(id);
        let (temp, tint) = lightcraft_engine::pipeline::local::effective_wb(&info, &d);
        let wb = &mut std::sync::Arc::make_mut(&mut d).wb;
        wb.temp = temp;
        wb.tint = tint;
    }
    let preview_only = app.session.catalog.photo(id).and_then(|p| p.preview_only.clone());

    if app.ui.soft_proof {
        soft_proofing(app, ui, id);
    }
    // header: Edit + Auto / B&W / HDR
    let mut reset_clicked = false;
    let (hr, _) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::hover());
    ui.painter().text(pos2(hr.left() + 24.0, hr.bottom() - 10.0), Align2::LEFT_CENTER, crate::i18n::tr("Edit"), t.semibold(15.0), t.text);
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 8, bottom: 14 }).show(ui, |ui| {
        let tip = reset_tip(&app.ui.settings.keymap, ui.ctx().os() == egui::os::OperatingSystem::Mac);
        let clicks = header_buttons(ui, crate::is_bw(&d), d.hdr.enabled, &tip);
        if clicks.auto {
            let _ = app.run("develop.auto", json!({}));
            app.toast(ui.ctx(), crate::i18n::tr("Auto settings applied"));
        }
        if clicks.bw {
            let _ = app.run("develop.treatment", json!({}));
        }
        if clicks.hdr {
            let _ = app.run("develop.hdr", json!({"enabled": !d.hdr.enabled}));
        }
        reset_clicked = clicks.reset;
    });
    if let Some(why) = &preview_only {
        let reload = egui::Frame::NONE
            .inner_margin(egui::Margin { left: 24, right: 22, top: 0, bottom: 12 })
            .show(ui, |ui| crate::widgets::preview_only_notice(ui, "edit", why))
            .inner;
        if reload {
            reload_preview_only(app, ui.ctx(), id);
        }
    }
    let n = app.session.selection.ids.len();
    if n > 1 && matches!(app.ui.view, crate::state::ViewMode::PhotoGrid | crate::state::ViewMode::SquareGrid) {
        quick_develop(app, ui, n);
    }
    if app.session.auto_sync && n > 1 {
        egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 0, bottom: 10 }).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(crate::i18n::tr_format!("Auto Sync: edits apply to {n} photos", n = n)).color(t.accent).size(12.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if text_button(ui, "autoSyncOff", crate::i18n::tr("Turn Off"), false).clicked() {
                        let _ = app.run("develop.autoSync", json!({"on": false}));
                    }
                });
            });
        });
    }
    divider(ui);
    // profile row
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 14, bottom: 14 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(crate::i18n::tr("Profile")).font(t.font(13.0)).color(t.text_dim));
            let name = app.session.profile_info(&d.profile.id).map(|(name, _)| name).unwrap_or("Color");
            // a long name is cut short before Browse Profiles: a wider row would widen the whole panel's
            // content, and the sliders below would run past its edge
            let max = ui.available_width() - 28.0 - ui.spacing().item_spacing.x;
            let r = crate::widgets::dropdown_within(ui, "profile", crate::i18n::profile_label(&d.profile.id, name), t.font(15.0), t.text_label, max);
            let below = r.rect.bottom();
            egui::Popup::menu(&r).show(|ui| profile_menu(app, ui, &d, below));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if crate::widgets::icon_button(
                    ui,
                    "profileBrowser",
                    Icon::ProfileGrid,
                    vec2(28.0, 28.0),
                    false,
                    true,
                    crate::i18n::tr("Browse Profiles"),
                )
                .clicked()
                {
                    let _ = app.run("panel.profiles", json!({}));
                }
            });
        });
    });
    if d.profile.id != "lc.color" {
        control(app, ui, &d, "profile.amount", true);
        ui.add_space(6.0);
    }
    // a raw carrying its own local tone mapping (Apple ProRAW's gain table map): off renders it as
    // Lightroom Classic does, on as the camera does
    if app.session.source_info(id).local_tone.is_some() {
        egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 0, bottom: 10 }).show(ui, |ui| {
            if let Some(on) = camera_local_tone_checkbox(ui, id, d.profile.camera_local_tone) {
                let _ = app.run("develop.merge", json!({"settings": {"profile": {"camera_local_tone": on}}, "label": "Camera Local Tone Mapping"}));
            }
        });
    }
    divider(ui);

    section(app, ui, &d, "light", "Light", |app, ui, d| {
        hdr_row(app, ui, d, id);
        for c in ["light.exposure", "light.contrast", "light.highlights", "light.shadows", "light.whites", "light.blacks"] {
            light_controls::control(app, ui, d, c, true);
        }
        ui.add_space(6.0);
        if d.hdr.enabled {
            let open = app.ui.flyout_open("sdrRendition");
            if flyout_row(ui, "sdrRendition", "SDR Rendition", Icon::Sliders, open).clicked() {
                app.ui.toggle_flyout("sdrRendition");
            }
            if open {
                for c in ["hdr.sdrBrightness", "hdr.sdrContrast", "hdr.sdrHighlights", "hdr.sdrShadows", "hdr.sdrWhites", "hdr.sdrClarity"] {
                    control(app, ui, d, c, true);
                }
                ui.add_space(6.0);
            }
        }
        let open = app.ui.flyout_open("curve");
        if flyout_row(ui, "curve", crate::i18n::tr("Curve"), Icon::Curve, open).clicked() {
            app.ui.toggle_flyout("curve");
        }
        if open {
            curve_editor(app, ui, id, d);
        }
        ui.add_space(8.0);
    });
    section(app, ui, &d, "color", "Color", |app, ui, d| {
        // White balance row
        egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 2, bottom: 2 }).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(crate::i18n::tr("White Balance")).font(t.font(13.0)).color(t.text_dim));
                color_controls::preset(app, ui, d.wb.mode);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let active = app.ui.tool == "wbPicker";
                    if crate::widgets::icon_button(
                        ui,
                        "wbPicker",
                        Icon::Picker,
                        vec2(28.0, 28.0),
                        active,
                        true,
                        crate::i18n::tr("White Balance Selector (W)"),
                    )
                    .clicked()
                    {
                        if active {
                            app.ui.tool.clear();
                        } else if let Err(e) = app.run("tool.wbPicker", json!({})) {
                            // the same as W: from Compare, Survey or the grid it opens the photo in Detail
                            app.toast(ui.ctx(), e);
                        }
                    }
                });
            });
        });
        color_controls::values(app, ui, d, raw);
        color_controls::adjustment(app, ui, d, "color.vibrance");
        color_controls::adjustment(app, ui, d, "color.saturation");
        ui.add_space(6.0);
        let open = app.ui.flyout_open("mixer");
        if flyout_row(ui, "mixer", if crate::is_bw(d) { "B&W Mixer" } else { "Color Mixer" }, Icon::Radial, open).clicked() {
            app.ui.toggle_flyout("mixer");
        }
        if open {
            mixer(app, ui, d);
        }
        if !crate::is_bw(d) {
            let open = app.ui.flyout_open("pointColor");
            if flyout_row(ui, "pointColor", crate::i18n::tr("Point Color"), Icon::Picker, open).clicked() {
                app.ui.toggle_flyout("pointColor");
            }
            if open {
                point_color(app, ui, d);
            }
        }
        let open = app.ui.flyout_open("grading");
        if flyout_row(ui, "grading", crate::i18n::tr("Color Grading"), Icon::Presets, open).clicked() {
            app.ui.toggle_flyout("grading");
        }
        if open {
            grading(app, ui, d, id);
        }
        ui.add_space(8.0);
    });
    section(app, ui, &d, "effects", "Effects", |app, ui, d| {
        for c in ["effects.texture", "effects.clarity", "effects.dehaze"] {
            control(app, ui, d, c, true);
        }
        sub_title(ui, crate::i18n::tr("Vignette"));
        if let Some(style) = vignette_style(app, ui, id, d.vignette.style) {
            let _ =
                app.run("develop.merge", serde_json::json!({"settings": {"vignette": {"style": serde_json::to_value(style).unwrap_or_default()}}}));
        }
        for c in ["vignette.amount", "vignette.midpoint", "vignette.feather", "vignette.roundness", "vignette.highlights"] {
            let enabled = c == "vignette.amount" || d.vignette.amount != 0.0;
            control(app, ui, d, c, enabled);
        }
        sub_title(ui, crate::i18n::tr("Grain"));
        for c in ["grain.amount", "grain.size", "grain.roughness"] {
            control(app, ui, d, c, c == "grain.amount" || d.grain.amount != 0.0);
        }
        ui.add_space(8.0);
    });
    section(app, ui, &d, "detail", "Detail", |app, ui, d| {
        for c in ["detail.sharpenAmount", "detail.sharpenRadius", "detail.sharpenDetail", "detail.sharpenMasking"] {
            control(app, ui, d, c, c == "detail.sharpenAmount" || d.detail.sharpen_amount > 0.0);
        }
        // AI Denoise is its own tool, apart from the manual noise reduction sliders below it
        sub_title(ui, crate::i18n::tr("AI Denoise"));
        // AI Denoise: how much of the photo's cleaned picture to mix in (raw files only), and what that picture is doing
        let applicable = !matches!(app.session.denoise_photo_state(id), lightcraft_engine::denoise::PhotoState::NotApplicable);
        let on = d.enhance.denoise_enabled();
        super::denoise::detail_toggle(app, ui, id, on, applicable);
        control(app, ui, d, "enhance.denoise", applicable && on);
        super::denoise::detail_status(app, ui, id, if on { d.enhance.denoise } else { 0.0 });
        sub_title(ui, crate::i18n::tr("Noise Reduction"));
        for c in ["detail.nrLuminance", "detail.nrDetail", "detail.nrContrast", "detail.nrColor", "detail.nrColorDetail", "detail.nrColorSmoothness"]
        {
            control(app, ui, d, c, true);
        }
        // single outlier photosites, replaced in the raw mosaic (#1345): raws only
        if app.session.catalog.photo(id).is_some_and(|p| p.develops_raw()) {
            egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 4, bottom: 0 }).show(ui, |ui| {
                let mut on = d.detail.nr_outliers;
                if crate::dialogs::settings::check(ui, "detail.nrOutliers", &mut on, "Remove Hot Pixels") {
                    let _ = app.run("develop.merge", json!({"settings": {"detail": {"nr_outliers": on}}, "label": "Remove Hot Pixels"}));
                }
            });
        }
        ui.add_space(8.0);
    });
    section(app, ui, &d, "optics", "Optics", |app, ui, d| {
        let has_lens = app.session.catalog.photo(id).is_some_and(|p| p.embedded_lens.is_some());
        egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 4, bottom: 4 }).show(ui, |ui| {
            let lp = optics_checkboxes(app, ui, id, d);
            if lp && let Some(p) = app.session.catalog.photo(id).cloned() {
                let text = crate::lens_db::readout(app, &p);
                if !text.is_empty() {
                    let r = ui.label(egui::RichText::new(text).size(11.0).color(Tokens::get(ui.ctx()).text_dim));
                    crate::widgets::register(ui, "optics:lensProfile", r.rect);
                }
            }
        });
        if d.optics.lens_profile {
            for c in ["optics.profileDistortion", "optics.profileVignetting"] {
                control(app, ui, d, c, has_lens);
            }
        }
        sub_title(ui, crate::i18n::tr("Manual"));
        for c in ["optics.distortion", "optics.vignetting"] {
            control(app, ui, d, c, true);
        }
        control(app, ui, d, "optics.vignettingMidpoint", d.optics.vignetting != 0.0);
        ui.add_space(6.0);
        let open = app.ui.flyout_open("defringe");
        if flyout_row(ui, "defringe", crate::i18n::tr("Defringe"), Icon::Picker, open).clicked() {
            app.ui.toggle_flyout("defringe");
        }
        if open {
            control(app, ui, d, "optics.defringePurple", true);
            let on = d.optics.defringe_purple_amount > 0.0;
            control(app, ui, d, "optics.defringePurpleHueLo", on);
            control(app, ui, d, "optics.defringePurpleHueHi", on);
            control(app, ui, d, "optics.defringeGreen", true);
            let on = d.optics.defringe_green_amount > 0.0;
            control(app, ui, d, "optics.defringeGreenHueLo", on);
            control(app, ui, d, "optics.defringeGreenHueHi", on);
            sub_title(ui, crate::i18n::tr("Lateral Chromatic Aberration"));
            control(app, ui, d, "optics.caRed", true);
            control(app, ui, d, "optics.caBlue", true);
        }
        ui.add_space(8.0);
    });
    section(app, ui, &d, "lensBlur", "Lens Blur", |app, ui, d| {
        lens_blur(app, ui, d);
        ui.add_space(8.0);
    });
    // Lightroom Classic's Calibration panel (the cloud app hides it): last, like there.
    section(app, ui, &d, "calibration", "Calibration", |app, ui, d| {
        sub_title(ui, crate::i18n::tr("Shadows"));
        control(app, ui, d, "calibration.shadowsTint", true);
        for (title, k) in [("Red Primary", "red"), ("Green Primary", "green"), ("Blue Primary", "blue")] {
            sub_title(ui, title);
            control(app, ui, d, &format!("calibration.{k}Hue"), true);
            control(app, ui, d, &format!("calibration.{k}Sat"), true);
        }
        ui.add_space(8.0);
    });
    ui.add_space(40.0);
    // A pending numeric edit commits on blur before the explicit Reset takes precedence.
    if reset_clicked {
        let _ = app.run("develop.reset", json!({}));
    }
}

#[cfg(test)]
#[path = "optics_checkbox_port_tests.rs"]
mod optics_checkbox_port_tests;

#[cfg(test)]
mod vignette_style_flow_tests;

#[cfg(test)]
#[path = "edit/camera_local_tone_flow_tests.rs"]
mod camera_local_tone_flow_tests;
