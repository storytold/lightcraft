//! The Edit panel: histogram, Auto/B&W, profile, and the Light / Color / Effects / Detail / Optics
//! sections, built from the engine's control specs.

use egui::epaint::{Mesh, Vertex};
use egui::{Align2, Color32, Pos2, Rect, RichText, Sense, Stroke, pos2, vec2};
use lightcraft_catalog::PhotoId;
use lightcraft_develop::{ControlSpec, DevelopSettings, Section, Track, WbMode, controls};
use lightcraft_geom::Point;
use lightcraft_raster::Histogram;
use serde_json::{Value, json};

use crate::LightcraftApp;
use crate::icons::{Icon, paint};
use crate::render::Slot;
use crate::state::RightPanel;
use crate::theme::Tokens;
use crate::widgets::{BAND_COLORS, SliderOut, divider, flyout_row, hex, register, section_header, slider, text_button};

/// Commit a slider interaction: begin → live updates → end, so a drag is one undo step.
pub fn apply_slider_out(
    app: &mut LightcraftApp,
    spec: &ControlSpec,
    out: SliderOut,
    mut set: impl FnMut(&mut LightcraftApp, f64) -> Result<Value, String>,
) {
    if out.drag_started && !out.reset {
        let _ = app.run("develop.beginInteraction", json!({"label": spec.label}));
        app.ui.dragging_control = Some(spec.id.to_string());
    }
    if let Some(v) = out.value {
        let _ = set(app, v);
    }
    if out.drag_stopped || out.reset {
        let _ = app.run("develop.endInteraction", json!({}));
        app.ui.dragging_control = None;
    }
}

pub(crate) fn control(app: &mut LightcraftApp, ui: &mut egui::Ui, d: &DevelopSettings, id: &str, enabled: bool) {
    let Some(spec) = controls::find(id) else { return };
    let v = controls::get(d, id).unwrap_or(spec.default);
    let out = slider(ui, spec, v, enabled, None);
    apply_slider_out(app, spec, out, |app, v| app.run("develop.set", json!({"control": id, "value": v})));
}

/// Relative temperature scale for rendered (non-raw) files: −100..100 ↔ Kelvin via mired shift.
const REL_TEMP: ControlSpec = ControlSpec {
    id: "wb.tempRel",
    label: "Temp",
    section: Section::Color,
    min: -100.0,
    max: 100.0,
    default: 0.0,
    step: 1.0,
    decimals: 0,
    track: Track::Temp,
};
const REL_TINT: ControlSpec = ControlSpec {
    id: "wb.tintRel",
    label: "Tint",
    section: Section::Color,
    min: -100.0,
    max: 100.0,
    default: 0.0,
    step: 1.0,
    decimals: 0,
    track: Track::Tint,
};

fn k_to_rel(k: f64) -> f64 {
    ((1e6 / 6500.0 - 1e6 / k) / 0.8).clamp(-100.0, 100.0)
}
fn rel_to_k(r: f64) -> f64 {
    1e6 / (1e6 / 6500.0 - r * 0.8)
}

pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId) {
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

    if app.ui.histogram {
        histogram(app, ui, id);
    }
    if app.ui.soft_proof {
        soft_proofing(app, ui, id);
    }
    // header: Edit + Auto / B&W / HDR
    let mut reset_clicked = false;
    let (hr, _) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::hover());
    ui.painter().text(pos2(hr.left() + 24.0, hr.bottom() - 10.0), Align2::LEFT_CENTER, crate::i18n::tr("Edit"), t.semibold(15.0), t.text);
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 8, bottom: 14 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            if text_button(ui, "auto", crate::i18n::tr("Auto"), false).clicked() {
                let _ = app.run("develop.auto", json!({}));
                app.toast(ui.ctx(), crate::i18n::tr("Auto settings applied"));
            }
            let bw = crate::is_bw(&d);
            if text_button(ui, "bw", crate::i18n::tr("B&W"), bw).clicked() {
                let _ = app.run("develop.treatment", json!({}));
            }
            // HDR editing, next to B&W as Lightroom has it
            if text_button(ui, "hdr", crate::i18n::tr("HDR"), d.hdr.enabled)
                .on_hover_text(crate::i18n::tr("Edit in HDR: highlights above SDR white, for HDR exports and displays"))
                .clicked()
            {
                let _ = app.run("develop.hdr", json!({"enabled": !d.hdr.enabled}));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if text_button(ui, "reset", crate::i18n::tr("Reset"), false)
                    .on_hover_text(reset_tip(&app.ui.settings.keymap, ui.ctx().os() == egui::os::OperatingSystem::Mac))
                    .clicked()
                {
                    reset_clicked = true;
                }
            });
        });
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
            egui::Popup::menu(&r).show(|ui| profile_menu(app, ui, &d));
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
            let mut on = d.profile.camera_local_tone;
            let r = ui.checkbox(&mut on, crate::i18n::tr("Camera local tone mapping"));
            crate::widgets::register(ui.ctx(), "check:profile.cameraLocalTone", r.rect);
            if r.on_hover_text(crate::i18n::tr("Apply the local tone mapping stored in the raw (Apple ProRAW), as the camera renders it")).changed() {
                let _ = app.run("develop.merge", json!({"settings": {"profile": {"camera_local_tone": on}}, "label": "Camera Local Tone Mapping"}));
            }
        });
    }
    divider(ui);

    section(app, ui, &d, "light", "Light", |app, ui, d| {
        hdr_row(app, ui, d);
        for c in ["light.exposure", "light.contrast", "light.highlights", "light.shadows", "light.whites", "light.blacks"] {
            control(app, ui, d, c, true);
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
                let r = crate::widgets::dropdown(ui, "wbMode", crate::i18n::tr(d.wb.mode.label()), t.font(14.0), t.text_label);
                egui::Popup::menu(&r).show(|ui| {
                    for m in WbMode::ALL {
                        if m == WbMode::Custom {
                            continue;
                        }
                        if ui.selectable_label(d.wb.mode == m, m.label()).clicked() {
                            let mode = serde_json::to_value(m).unwrap_or_default();
                            let _ = app.run("develop.wb", json!({"mode": mode}));
                        }
                    }
                });
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
        if raw {
            control(app, ui, d, "wb.temp", true);
            control(app, ui, d, "wb.tint", true);
        } else {
            let out = slider(ui, &REL_TEMP, k_to_rel(d.wb.temp), true, None);
            apply_slider_out(app, &REL_TEMP, out, |app, v| app.run("develop.set", json!({"control": "wb.temp", "value": rel_to_k(v)})));
            let out = slider(ui, &REL_TINT, d.wb.tint.clamp(-100.0, 100.0), true, None);
            apply_slider_out(app, &REL_TINT, out, |app, v| app.run("develop.set", json!({"control": "wb.tint", "value": v})));
        }
        control(app, ui, d, "color.vibrance", true);
        control(app, ui, d, "color.saturation", true);
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
        {
            use lightcraft_develop::VignetteStyle as V;
            let styles = [
                (V::HighlightPriority, "Highlight", "highlightPriority"),
                (V::ColorPriority, "Color", "colorPriority"),
                (V::PaintOverlay, "Paint", "paintOverlay"),
            ];
            let items: Vec<(&str, &str)> = styles.iter().map(|(_, l, k)| (*l, *k)).collect();
            let active = styles.iter().position(|(v, _, _)| *v == d.vignette.style);
            let mut chosen = None;
            egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 2, bottom: 4 }).show(ui, |ui| {
                chosen = crate::widgets::segmented(ui, "vignetteStyle", &items, active, 3);
            });
            if let Some(i) = chosen {
                let _ = app.run(
                    "develop.merge",
                    serde_json::json!({"settings": {"vignette": {"style": serde_json::to_value(styles[i].0).unwrap_or_default()}}}),
                );
            }
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
        ui.add_space(8.0);
    });
    section(app, ui, &d, "optics", "Optics", |app, ui, d| {
        let has_lens = app.session.catalog.photo(id).is_some_and(|p| p.embedded_lens.is_some());
        egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 4, bottom: 4 }).show(ui, |ui| {
            let mut ca = d.optics.remove_ca;
            if ui.checkbox(&mut ca, crate::i18n::tr("Remove Chromatic Aberration")).changed() {
                let _ = app.run("develop.merge", json!({"settings": {"optics": {"remove_ca": ca}}, "label": "Remove CA"}));
            }
            let mut lp = d.optics.lens_profile;
            if ui.checkbox(&mut lp, crate::i18n::tr("Enable Lens Corrections")).changed() {
                let _ = app.run("develop.merge", json!({"settings": {"optics": {"lens_profile": lp}}, "label": "Lens Corrections"}));
            }
            if lp && !has_lens {
                ui.label(
                    egui::RichText::new(crate::i18n::tr("No lens data embedded in this file.")).size(11.0).color(Tokens::get(ui.ctx()).text_dim),
                );
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

/// The profile dropdown: Favorites, Recent, one submenu per group, then favourite toggle and
/// Browse….
fn profile_menu(app: &mut LightcraftApp, ui: &mut egui::Ui, d: &DevelopSettings) {
    use lightcraft_engine::presets::{PROFILES, profile_groups};
    let t = Tokens::get(ui.ctx());
    ui.set_min_width(200.0);
    let cur = d.profile.id.as_str();
    // (clicked, hovered)
    let mut pick: (Option<String>, Option<String>) = (None, None);
    let item = |ui: &mut egui::Ui, key: &str, id: &str, name: &str, pick: &mut (Option<String>, Option<String>)| {
        let r = ui.selectable_label(id == cur, crate::i18n::profile_label(id, name));
        register(ui.ctx(), format!("profileMenu:{key}:{id}"), r.rect);
        if r.clicked() {
            pick.0 = Some(id.to_string());
        }
        if r.hovered() {
            pick.1 = Some(id.to_string());
        }
    };
    let heading = |ui: &mut egui::Ui, s: &str| {
        ui.label(egui::RichText::new(crate::i18n::tr(s)).font(t.semibold(11.5)).color(t.text_dim));
    };
    for (title, key, ids) in [("Favorites", "fav", app.session.profile_favorites.clone()), ("Recent", "recent", app.session.profile_recent.clone())] {
        let list: Vec<_> = ids.iter().filter_map(|id| app.session.profile_info(id).map(|(name, _)| (id.as_str(), name))).collect();
        if list.is_empty() {
            continue;
        }
        heading(ui, title);
        for (id, name) in list {
            item(ui, key, id, name, &mut pick);
        }
        ui.separator();
    }
    for g in profile_groups() {
        let r = crate::widgets::menu_button(ui, crate::i18n::tr(g), |ui| {
            ui.set_min_width(170.0);
            for p in PROFILES.iter().filter(|p| p.group == g) {
                item(ui, "group", p.id, p.name, &mut pick);
            }
        });
        register(ui.ctx(), format!("profileMenu:groupMenu:{g}"), r.response.rect);
    }
    // imported LUT profile groups
    let mut lut_groups: Vec<String> = app.session.lut_profiles.iter().map(|p| p.group.clone()).collect();
    lut_groups.sort_unstable();
    lut_groups.dedup();
    for g in &lut_groups {
        // a folder's name as it is, and its own widget id even when it matches a built-in group
        let r = crate::widgets::menu_button(ui, g.as_str(), |ui| {
            ui.set_min_width(170.0);
            for p in app.session.lut_profiles.iter().filter(|p| &p.group == g) {
                item(ui, "group", &p.id, &p.name, &mut pick);
            }
        });
        register(ui.ctx(), format!("profileMenu:lutGroupMenu:{g}"), r.response.rect);
    }
    ui.separator();
    if let Some((name, _)) = app.session.profile_info(cur) {
        let fav = app.session.profile_favorites.iter().any(|f| f == cur);
        let label = if fav {
            crate::i18n::tr_format!("Remove “{}” from Favorites", crate::i18n::profile_label(cur, name))
        } else {
            crate::i18n::tr_format!("Add “{}” to Favorites", crate::i18n::profile_label(cur, name))
        };
        let r = ui.button(label);
        register(ui.ctx(), "profileMenu:toggleFavorite", r.rect);
        if r.clicked() {
            let _ = app.run("profile.favorite", json!({"id": cur}));
        }
    }
    let r = ui.button(crate::i18n::tr("Browse…"));
    register(ui.ctx(), "profileMenu:browse", r.rect);
    if r.clicked() {
        let _ = app.run("panel.profiles", json!({}));
    }
    // resting on a profile previews it in the loupe
    if let Some(hovered_id) = &pick.1
        && hovered_id != cur
        && let Some((name, _)) = app.session.profile_info(hovered_id)
    {
        let mut s = d.clone();
        s.profile.id = hovered_id.clone();
        s.profile.amount = 100.0;
        app.hover_preview =
            Some(crate::HoverPreview { label: crate::i18n::tr_format!("Profile: {}", crate::i18n::profile_label(hovered_id, name)), settings: s });
    }
    if let Some(id) = pick.0 {
        let _ = app.run("develop.profile", json!({"id": id}));
    }
}

pub fn sub_title(ui: &mut egui::Ui, title: &str) {
    let t = Tokens::get(ui.ctx());
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
    ui.painter().text(pos2(r.left() + 24.0, r.center().y + 4.0), Align2::LEFT_CENTER, crate::i18n::tr(title), t.semibold(13.0), t.text_label);
}

fn section(
    app: &mut LightcraftApp,
    ui: &mut egui::Ui,
    d: &DevelopSettings,
    id: &str,
    title: &str,
    body: impl FnOnce(&mut LightcraftApp, &mut egui::Ui, &DevelopSettings),
) {
    let open = app.ui.section_open(id);
    let (resp, toggled) = section_header(ui, id, title, open, Some(d.section_enabled(id)));
    if let Some(on) = toggled {
        let _ = app.run("develop.sectionEnabled", json!({"section": id, "enabled": on}));
    } else if resp.clicked() {
        // ⌥-click opens just this section, as in Lightroom Classic
        let solo = ui.input(|i| i.modifiers.alt);
        if let Err(e) = app.run(&format!("section.{id}"), json!({"solo": solo})) {
            app.toast(ui.ctx(), e);
        }
    }
    resp.context_menu(|ui| {
        let mut solo = app.ui.single_panel;
        let item = ui.checkbox(&mut solo, crate::i18n::tr("Solo Mode"));
        register(ui.ctx(), "sectionMenu:solo", item.rect);
        if item.clicked() {
            if let Err(e) = app.run("view.soloSections", json!({"on": solo})) {
                app.toast(ui.ctx(), e);
            }
            ui.close();
        }
    });
    if open {
        body(app, ui, d);
    }
    divider(ui);
}

// ------------------------------------------------------------------------------ histogram

/// The current photo's existing preview histogram, shared by Histogram and Tone Curve.
fn preview_histogram(app: &LightcraftApp, id: PhotoId) -> Option<lightcraft_raster::Histogram> {
    app.renderer
        .textures
        .get(&Slot::Main)
        .filter(|t| t.photo == id)
        .and_then(|t| t.histogram.clone())
        .or_else(|| app.renderer.textures.get(&Slot::Thumb(id)).and_then(|t| t.histogram.clone()))
}

/// An HDR edit's histogram stays an HDR histogram: some renders carry only their 8-bit image's
/// (a drag's drafts, a cached thumbnail or stand-in), and showing those would swap the panel to
/// the SDR layout and back while a slider moves. Every render with HDR bins replaces the one kept for the photo, so
/// the histogram still follows the edit live; an SDR edit shows what it gets.
fn steady_hdr_histogram(ctx: &egui::Context, id: PhotoId, hdr_edit: bool, hist: Option<Histogram>) -> Option<Histogram> {
    let key = egui::Id::new(("hdrHistogram", id));
    match hist {
        Some(h) if h.hdr.is_some() => {
            ctx.data_mut(|d| d.insert_temp(key, h.clone()));
            Some(h)
        }
        h if hdr_edit => ctx.data(|d| d.get_temp::<Histogram>(key)).or(h),
        h => h,
    }
}

fn histogram(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId) {
    let t = Tokens::get(ui.ctx());
    let w = ui.available_width();
    let hdr_edit = app.session.develop_of(id).is_some_and(|d| d.hdr.enabled);
    // a drag of an HDR edit: its drafts have no HDR histogram, so a small render beside them
    // gives one (at its own pace: the drafts don't wait for it)
    let interacting = app.session.interaction.is_some();
    if interacting && hdr_edit {
        let edge = lightcraft_pipeline::HDR_HISTOGRAM_DRAG_EDGE;
        if let Some(job) = app.session.render_job(id, edge, edge, false, app.ui.right != RightPanel::Crop)
            && app.renderer.live_hdr_histogram.as_ref().is_none_or(|l| l.1 != job.key)
        {
            app.renderer.request(Slot::HdrHistogram, job, 95);
        }
    }
    let live = app.renderer.live_hdr_histogram.as_ref().filter(|l| interacting && l.0 == id).map(|l| l.2.clone());
    // else the loupe render of this photo, else its thumbnail (grid view before the loupe has
    // rendered it)
    let hist = live.or_else(|| preview_histogram(app, id));
    let hist = steady_hdr_histogram(ui.ctx(), id, hdr_edit, hist);
    // an HDR edit's histogram (`pipeline::hdr_histogram`) gets a colour bar and an SDR / HDR label
    // row under the plot
    let hdr = hist.as_ref().and_then(|h| h.hdr);
    let legend = if hdr.is_some() { 26.0 } else { 0.0 };
    let (r, resp) = ui.allocate_exact_size(vec2(w, 118.0 + legend), Sense::click());
    crate::widgets::announce(&resp, crate::i18n::tr("Histogram"));
    register(ui.ctx(), "histogram", r);
    let p = ui.painter();
    p.rect_filled(r, 0.0, t.button);
    let plot = Rect::from_min_max(r.min + vec2(0.0, 6.0), pos2(r.right(), r.bottom() - 26.0 - legend));
    if let Some(b) = hdr {
        // white for SDR, then one band per stop above SDR white: white too, or (Visualize HDR
        // range on) in its colours
        let x0 = plot.left() + plot.width() * b.from as f32 / 255.0;
        let bar = |a: f32, z: f32, c: Color32| p.rect_filled(Rect::from_min_max(pos2(a, plot.bottom() + 2.0), pos2(z, plot.bottom() + 7.0)), 0.0, c);
        let white = Color32::from_rgb(228, 228, 230);
        bar(plot.left(), x0 - 1.0, white);
        let stops = b.stops.round().max(1.0) as usize;
        let bands = lightcraft_pipeline::visualize::HDR_BANDS;
        let colours = app.ui.hdr_visualize;
        for k in 0..stops {
            let c = if colours { bands.get(k).or(bands.last()).copied().unwrap_or([255, 255, 255]) } else { [228, 228, 230] };
            let (a, z) = (x0 + (plot.right() - x0) * k as f32 / stops as f32, x0 + (plot.right() - x0) * (k + 1) as f32 / stops as f32);
            bar(a + 1.0, z - 1.0, Color32::from_rgb(c[0], c[1], c[2]));
        }
        let y = plot.bottom() + 18.0;
        p.text(pos2((plot.left() + x0) / 2.0, y), Align2::CENTER_CENTER, crate::i18n::tr("SDR"), t.font(12.5), t.text_label);
        p.text(pos2((x0 + plot.right()) / 2.0, y), Align2::CENTER_CENTER, crate::i18n::tr("HDR"), t.font(12.5), t.text_label);
        let info = Rect::from_center_size(pos2(plot.right() - 16.0, y), vec2(14.0, 14.0));
        paint(p, info, Icon::Info, t.text_label);
        ui.interact(info, egui::Id::new("hdrHistogramInfo"), Sense::hover()).on_hover_text(crate::i18n::tr(
            "Left: the standard (SDR) range up to SDR white. Right: the HDR range, one band per stop above SDR white (the orange line: the HDR headroom). With Visualize HDR range on, the bands take its colours.",
        ));
    }
    let clipping = hist.as_ref().map(|h| h.clipping());
    if let Some(h) = hist {
        let smooth = |v: &[u32]| -> Vec<f32> {
            (0..256)
                .map(|i| {
                    let (a, b) = (i.max(2) - 2, (i + 3).min(256));
                    v[a..b].iter().map(|x| *x as f32).sum::<f32>() / (b - a) as f32
                })
                .collect()
        };
        // HDR: SDR on the left half, a solid line at SDR white and dashed ones between the stops
        if let Some(b) = h.hdr {
            let x0 = plot.left() + plot.width() * b.from as f32 / 255.0;
            let stops = b.stops.round().max(1.0) as usize;
            for k in 1..stops {
                let x = x0 + (plot.right() - x0) * k as f32 / stops as f32;
                let mut y = plot.top();
                while y < plot.bottom() {
                    p.line_segment([pos2(x, y), pos2(x, (y + 5.0).min(plot.bottom()))], Stroke::new(1.0, Color32::from_gray(150)));
                    y += 9.0;
                }
            }
            p.line_segment([pos2(x0, plot.top()), pos2(x0, plot.bottom())], Stroke::new(1.5, Color32::from_gray(160)));
            // the photo's headroom limit on the fixed scale: marked, the range past it dimmed
            let limit = app.session.develop_of(id).map_or(b.stops, |d| d.hdr.max_ev as f32);
            if limit.is_finite() && limit >= 0.0 && limit < b.stops - 0.05 {
                let x = x0 + (plot.right() - x0) * (limit / b.stops).clamp(0.0, 1.0);
                p.rect_filled(Rect::from_min_max(pos2(x, plot.top()), plot.max), 0.0, Color32::from_black_alpha(70));
                p.line_segment([pos2(x, plot.top()), pos2(x, plot.bottom())], Stroke::new(1.0, Color32::from_rgb(230, 150, 60)));
            }
        }
        let chans = [(smooth(&h.r), hex("#df3939")), (smooth(&h.g), hex("#44b072")), (smooth(&h.b), hex("#3b6fe0"))];
        let peak = chans.iter().flat_map(|(v, _)| v.iter().skip(2).take(252)).fold(1.0f32, |a, b| a.max(*b));
        let to = |i: usize, v: f32| pos2(plot.left() + plot.width() * i as f32 / 255.0, plot.bottom() - plot.height() * (v / peak).sqrt().min(1.0));
        // luminance fill
        let lum = smooth(&h.luma);
        let mut mesh = Mesh::default();
        for i in 0..256 {
            let top = to(i, lum[i]);
            mesh.vertices.push(Vertex { pos: top, uv: Pos2::ZERO, color: Color32::from_gray(120).gamma_multiply(0.35) });
            mesh.vertices.push(Vertex { pos: pos2(top.x, plot.bottom()), uv: Pos2::ZERO, color: Color32::from_gray(120).gamma_multiply(0.35) });
            if i > 0 {
                let k = (i as u32) * 2;
                mesh.indices.extend([k - 2, k - 1, k, k - 1, k + 1, k]);
            }
        }
        p.add(mesh);
        // greyscale images: the three channels coincide — draw one neutral line
        let spread: f32 = (0..256).map(|i| (chans[0].0[i] - chans[1].0[i]).abs() + (chans[1].0[i] - chans[2].0[i]).abs()).sum();
        let total: f32 = chans[1].0.iter().sum::<f32>().max(1.0);
        if spread / total < 0.02 {
            let pts: Vec<Pos2> = (0..256).map(|i| to(i, chans[1].0[i])).collect();
            p.add(egui::Shape::line(pts, Stroke::new(1.5, Color32::from_gray(200))));
        } else {
            for (v, c) in &chans {
                let pts: Vec<Pos2> = (0..256).map(|i| to(i, v[i])).collect();
                p.add(egui::Shape::line(pts, Stroke::new(1.5, *c)));
            }
        }
    }
    let mut clicked = resp.clicked();
    // the panel's floating scroll bar widens over the right edge when hovered: both buttons keep
    // clear of it, the left one too so they stay symmetric (#1640)
    let scroll = ui.spacing().scroll;
    let inset = 20.0 + scroll.bar_width + scroll.bar_outer_margin;
    for (name, x, label, clipped) in [
        ("shadows", plot.left() + inset, "Shadow clipping", clipping.map(|(lo, _)| lo > 0.003)),
        ("highlights", plot.right() - inset, "Highlight clipping", clipping.map(|(_, hi)| hi > 0.003)),
    ] {
        let rect = Rect::from_center_size(pos2(x, plot.top() + 14.0), vec2(24.0, 22.0));
        let response = ui.interact(rect, ui.id().with(("histogram-clipping", name)), Sense::click());
        register(ui.ctx(), format!("histogram:{name}"), rect);
        response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, ui.is_enabled(), app.ui.show_clipping, crate::i18n::tr(label)));
        clicked |= response.clicked();
        let active = app.ui.show_clipping;
        // Keep the hit target comfortable, with a smaller box and clear space to the plot edges.
        let box_rect = rect.shrink2(vec2(2.0, 2.0));
        p.rect_filled(
            box_rect,
            3.0,
            if active {
                t.pressed
            } else if response.hovered() {
                t.hover
            } else {
                Color32::from_gray(60)
            },
        );
        if active {
            p.rect_stroke(box_rect, 3.0, Stroke::new(1.0, t.text), egui::StrokeKind::Inside);
        }
        paint(
            p,
            Rect::from_center_size(rect.center(), vec2(16.0, 16.0)),
            Icon::ChevronDown,
            if clipped == Some(true) || active { t.text } else { t.text_dim },
        );
        let status = match clipped {
            Some(true) => "Clipping detected in this preview.",
            Some(false) => "No clipping detected in this preview.",
            None => "Histogram not yet available.",
        };
        response.on_hover_text(format!(
            "{}\n{}\n{}",
            crate::i18n::tr(label),
            crate::i18n::tr(if active {
                "Click to hide clipping warnings for shadows and highlights."
            } else {
                "Click to show clipping warnings for shadows and highlights."
            }),
            crate::i18n::tr(status)
        ));
    }
    if clicked {
        let _ = app.run("view.clipping", json!({}));
    }
    // EXIF strip
    if let Some(ph) = app.session.catalog.photo(id) {
        let m = &ph.meta;
        let parts: Vec<String> = [
            m.iso.map(|i| format!("ISO {i}")),
            m.focal_mm.map(|f| format!("{f:.0}mm")),
            m.aperture.map(|a| format!("f/{a:.1}")),
            if m.shutter.is_empty() { None } else { Some(m.shutter.clone()) },
        ]
        .into_iter()
        .flatten()
        .collect();

        if !parts.is_empty() {
            let text = parts.join(" · ");
            let y = r.bottom() - 13.0;
            let x = r.left() + r.width() / 2.0;
            p.text(pos2(x, y), Align2::CENTER_CENTER, text, t.font(12.5), t.text_label);
        }
    }
}

// ------------------------------------------------------------------------------ soft proofing

/// The Soft Proofing strip under the histogram: proof profile, gamut warnings, Create Proof Copy.
fn soft_proofing(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId) {
    use lightcraft_engine::pipeline::OutputSpace;
    let t = Tokens::get(ui.ctx());
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 8, bottom: 6 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(crate::i18n::tr("Soft Proofing")).color(t.text).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut dest = app.ui.proof.dest_warning;
                let r = ui
                    .toggle_value(&mut dest, RichText::new("■").color(Color32::from_rgb(255, 40, 40)))
                    .on_hover_text(crate::i18n::tr("Show destination gamut warning"));
                register(ui.ctx(), "button:proofDestWarning", r.rect);
                let mut disp = app.ui.proof.display_warning;
                let r2 = ui
                    .toggle_value(&mut disp, RichText::new("■").color(Color32::from_rgb(40, 90, 255)))
                    .on_hover_text(crate::i18n::tr("Show display gamut warning"));
                register(ui.ctx(), "button:proofDisplayWarning", r2.rect);
                if dest != app.ui.proof.dest_warning || disp != app.ui.proof.display_warning {
                    let _ = app.run("view.softProof", json!({"destWarning": dest, "displayWarning": disp}));
                }
            });
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new(crate::i18n::tr("Profile:")).color(t.text_label));
            let cur = app.ui.proof.space;
            let device = app.ui.proof_profile.as_ref().map(|(_, n)| n.clone());
            let shown = device.clone().unwrap_or_else(|| cur.label().to_string());
            let mut pick = false;
            crate::widgets::combo_box("proof-space").width(170.0).selected_text(shown).show_ui(ui, |ui| {
                for s in OutputSpace::ALL {
                    if ui.selectable_label(device.is_none() && cur == s, s.label()).clicked() {
                        let _ = app.run("view.softProof", json!({"space": s}));
                    }
                }
                if let Some(n) = &device {
                    let _ = ui.selectable_label(true, n);
                }
                if app.services.pick_display_profile.is_some() && ui.selectable_label(false, crate::i18n::tr("Other…")).clicked() {
                    pick = true;
                }
            });
            if pick
                && let Some(path) = app.services.pick_display_profile.as_mut().and_then(|pick| pick().into_iter().next())
                && let Err(e) = app.run("view.softProof", json!({"profile": path}))
            {
                app.toast(ui.ctx(), e);
            }
        });
        if app.ui.proof_profile.is_some() {
            ui.horizontal(|ui| {
                ui.label(RichText::new(crate::i18n::tr("Intent:")).color(t.text_label));
                use lightcraft_engine::proof::ProofIntent;
                for (i, label) in [(ProofIntent::Perceptual, "Perceptual"), (ProofIntent::Relative, "Relative")] {
                    let r = ui.selectable_label(app.ui.proof_intent == i, crate::i18n::tr(label));
                    register(ui.ctx(), format!("button:proofIntent-{}", i.name()), r.rect);
                    if r.clicked() && app.ui.proof_intent != i {
                        let _ = app.run("view.softProof", json!({"intent": i.name()}));
                    }
                }
            });
            let mut paper = app.ui.proof_paper_ink;
            let r = ui.checkbox(&mut paper, crate::i18n::tr("Simulate Paper & Ink"));
            register(ui.ctx(), "button:proofPaperInk", r.rect);
            if r.changed() {
                let _ = app.run("view.softProof", json!({"paperInk": paper}));
            }
        }
        let r = ui.button(crate::i18n::tr("Create Proof Copy"));
        register(ui.ctx(), "button:createProofCopy", r.rect);
        if r.clicked() {
            let target = app.ui.proof_profile.as_ref().map_or_else(|| app.ui.proof.space.label().to_string(), |(_, n)| n.clone());
            let name = crate::i18n::tr_format!("Proof Copy ({})", target);
            if app.run("photo.virtualCopy", json!({"ids": [id.0], "name": name})).is_ok() {
                app.toast(ui.ctx(), crate::i18n::tr("Proof copy created"));
            }
        }
    });
    divider(ui);
}

// ------------------------------------------------------------------------------ tone curve

/// A 256-point, peak-normalized display profile of the photo histogram.
/// For HDR, only the SDR portion maps to a curve's input (0..1).
fn curve_histogram_profile(h: &lightcraft_raster::Histogram, channel: &str) -> [f32; 256] {
    let source = match channel {
        "red" => &h.r,
        "green" => &h.g,
        "blue" => &h.b,
        _ => &h.luma,
    };
    let bins = h.hdr.map_or(256, |hdr| hdr.from.clamp(2, 256));
    let samples: [f32; 256] = std::array::from_fn(|i| {
        let j = i * (bins - 1) / 255;
        let start = j.saturating_sub(2);
        let end = (j + 3).min(bins);
        let sum: f32 = (start..end).map(|k| source.get(k).copied().unwrap_or(0) as f32).sum();
        sum / (end - start) as f32
    });
    let peak = samples.iter().copied().fold(1.0_f32, f32::max);
    samples.map(|v| (v / peak).sqrt())
}

/// Show the currently rendered photo's tonal distribution behind the editable
/// curve, without requesting another render or blocking pointer interactions.
fn draw_curve_histogram(p: &egui::Painter, r: Rect, h: &lightcraft_raster::Histogram, channel: &str) {
    let profile = curve_histogram_profile(h, channel);
    let color = match channel {
        "red" => hex("#dd3333"),
        "green" => hex("#33bb55"),
        "blue" => hex("#3377ee"),
        _ => Color32::from_gray(130),
    }
    .gamma_multiply(0.27);
    let mut mesh = Mesh::default();
    for (i, height) in profile.into_iter().enumerate() {
        let x = r.left() + r.width() * i as f32 / 255.0;
        let top = pos2(x, r.bottom() - r.height() * height);
        mesh.vertices.push(Vertex { pos: top, uv: Pos2::ZERO, color });
        mesh.vertices.push(Vertex { pos: pos2(x, r.bottom()), uv: Pos2::ZERO, color });
        if i > 0 {
            let k = i as u32 * 2;
            mesh.indices.extend([k - 2, k - 1, k, k - 1, k + 1, k]);
        }
    }
    p.add(mesh);
}

fn curve_editor(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId, d: &DevelopSettings) {
    let t = Tokens::get(ui.ctx());
    // channel selector
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 6, bottom: 4 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            for (ch, c) in [
                ("parametric", Color32::from_gray(200)),
                ("master", Color32::WHITE),
                ("red", hex("#dd3333")),
                ("green", hex("#33bb55")),
                ("blue", hex("#3377ee")),
            ] {
                let (r, resp) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::click());
                crate::widgets::announce(&resp, format!("{} {ch}", crate::i18n::tr("Curve channel")));
                register(ui.ctx(), format!("curveChannel:{ch}"), r);
                let sel = app.ui.curve_channel == ch;
                if ch == "parametric" {
                    paint(ui.painter(), r.shrink(4.0), Icon::Curve, if sel { t.text } else { t.icon });
                } else {
                    ui.painter().circle_stroke(r.center(), 7.0, Stroke::new(2.0, c));
                    if sel {
                        ui.painter().circle_filled(r.center(), 3.5, c);
                    }
                }
                if sel {
                    ui.painter().line_segment([r.left_bottom() + vec2(4.0, 1.0), r.right_bottom() + vec2(-4.0, 1.0)], Stroke::new(2.0, t.text));
                }
                let name = match ch {
                    "parametric" => "Parametric curve",
                    "master" => "Point curve",
                    "red" => "Red channel",
                    "green" => "Green channel",
                    _ => "Blue channel",
                };
                let resp = resp.on_hover_text(crate::i18n::tr_format!("{name} — double-click to reset it", name = name));
                if resp.double_clicked() {
                    app.ui.curve_channel = ch.into();
                    let _ = app.run("curve.reset", json!({"channel": ch}));
                } else if resp.clicked() {
                    app.ui.curve_channel = ch.into();
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                tat_button(app, ui, "tat:curve", "Targeted adjustment: drag up/down on the photo to adjust the curve there");
            });
        });
    });
    let ch = app.ui.curve_channel.clone();
    let w = ui.available_width();
    let side = (w - 48.0).max(80.0);
    let (outer, _) = ui.allocate_exact_size(vec2(w, side + 12.0), Sense::hover());
    let r = Rect::from_min_size(pos2(outer.left() + 24.0, outer.top() + 6.0), vec2(side, side));
    let resp = ui.interact(r, egui::Id::new(("curve", &ch)), Sense::click_and_drag());
    crate::widgets::announce(&resp, crate::i18n::tr("Tone Curve"));
    register(ui.ctx(), "curve", r);
    resp.context_menu(|ui| curve_reset_menu(app, ui, &ch));
    let p = ui.painter();
    p.rect_filled(r, 2.0, t.canvas);
    if let Some(hist) = preview_histogram(app, id) {
        draw_curve_histogram(p, r, &hist, &ch);
    }
    for i in 1..4 {
        let f = i as f32 / 4.0;
        p.line_segment(
            [pos2(r.left() + r.width() * f, r.top()), pos2(r.left() + r.width() * f, r.bottom())],
            Stroke::new(1.0, Color32::from_gray(52)),
        );
        p.line_segment(
            [pos2(r.left(), r.top() + r.height() * f), pos2(r.right(), r.top() + r.height() * f)],
            Stroke::new(1.0, Color32::from_gray(52)),
        );
    }
    p.line_segment([r.left_bottom(), r.right_top()], Stroke::new(1.0, Color32::from_gray(70)));
    let to_screen = |x: f64, y: f64| pos2(r.left() + x as f32 * r.width(), r.bottom() - y as f32 * r.height());
    let from_screen = |q: Pos2| (((q.x - r.left()) / r.width()).clamp(0.0, 1.0) as f64, ((r.bottom() - q.y) / r.height()).clamp(0.0, 1.0) as f64);
    if ch == "parametric" {
        // draw the effective parametric curve by sampling the pipeline's region curve approximation
        let c = &d.curve;
        let regions = [
            (0.0, c.split_shadows / 100.0, c.shadows),
            (c.split_shadows / 100.0, c.split_mid / 100.0, c.darks),
            (c.split_mid / 100.0, c.split_highlights / 100.0, c.lights),
            (c.split_highlights / 100.0, 1.0, c.highlights),
        ];
        let pts: Vec<Pos2> = (0..=64)
            .map(|i| {
                let x = i as f64 / 64.0;
                let mut dd = 0.0;
                for (a, b, amt) in regions {
                    let (ctr, half) = ((a + b) / 2.0, (b - a) * 0.75 + 0.05);
                    let tt = ((x - ctr) / half).clamp(-1.0, 1.0);
                    dd += amt / 100.0 * 0.22 * (0.5 + 0.5 * (tt * std::f64::consts::PI).cos());
                }
                to_screen(x, (x + dd * 4.0 * x * (1.0 - x)).clamp(0.0, 1.0))
            })
            .collect();
        p.add(egui::Shape::line(pts, Stroke::new(2.0, Color32::from_gray(220))));
        curve_footer(app, ui, d);
        for c in ["curve.highlights", "curve.lights", "curve.darks", "curve.shadows"] {
            control(app, ui, d, c, true);
        }
        control(app, ui, d, "curve.refineSaturation", true);
        return;
    }
    let pts_of = |d: &DevelopSettings| -> Vec<Point> {
        let v = match ch.as_str() {
            "red" => &d.curve.red,
            "green" => &d.curve.green,
            "blue" => &d.curve.blue,
            _ => &d.curve.master,
        };
        // in the order the curve is drawn (points from a sidecar, a preset or an agent can come
        // unsorted or not finite): the drag below keeps each point between its neighbours
        let mut v: Vec<Point> = v.iter().copied().filter(|q| q.x.is_finite() && q.y.is_finite()).collect();
        v.sort_by(|a, b| a.x.total_cmp(&b.x));
        if v.len() < 2 { vec![Point::new(0.0, 0.0), Point::new(1.0, 1.0)] } else { v }
    };
    let pts = pts_of(d);
    let curve = lightcraft_color::spline::MonotoneCurve::new(&pts.iter().map(|q| (q.x, q.y)).collect::<Vec<_>>());
    let color = match ch.as_str() {
        "red" => hex("#dd3333"),
        "green" => hex("#33bb55"),
        "blue" => hex("#3377ee"),
        _ => Color32::from_gray(225),
    };
    let line: Vec<Pos2> = (0..=96).map(|i| to_screen(i as f64 / 96.0, curve.eval(i as f64 / 96.0).clamp(0.0, 1.0))).collect();
    p.add(egui::Shape::line(line, Stroke::new(2.0, color)));
    // interaction: drag a point (both axes, between its neighbours; ⌥ = fine), drag empty space to add and
    // drag a new point, click empty space to add, double-click a point to delete it
    let drag_id = egui::Id::new(("curve-drag", &ch));
    // the point being dragged (stored as `Option<usize>` so a stale value can be cleared)
    let mut dragging: Option<usize> = ui.data(|dd| dd.get_temp::<Option<usize>>(drag_id)).flatten();
    let nearest = |q: Pos2| pts.iter().enumerate().map(|(i, p)| (i, to_screen(p.x, p.y).distance(q))).min_by(|a, b| a.1.total_cmp(&b.1));
    let hovered = resp.hover_pos().and_then(nearest).filter(|(_, dist)| *dist < 10.0).map(|(i, _)| i);
    if dragging.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    } else if hovered.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
    }
    for (i, q) in pts.iter().enumerate() {
        let c = to_screen(q.x, q.y);
        if dragging == Some(i) || (dragging.is_none() && hovered == Some(i)) {
            p.circle_filled(c, 5.5, color);
            p.circle_stroke(c, 5.5, Stroke::new(1.0, t.canvas));
        } else {
            p.circle_filled(c, 4.0, color);
        }
    }
    // input / output readout of the point being dragged (0–255 like the histogram)
    if let Some(q) = dragging.and_then(|i| pts.get(i)) {
        p.text(
            r.left_top() + vec2(6.0, 4.0),
            egui::Align2::LEFT_TOP,
            format!("{} / {}", (q.x * 255.0).round(), (q.y * 255.0).round()),
            t.font(11.0),
            t.text_dim,
        );
    }
    let mut new_pts = None;
    if resp.double_clicked()
        && let Some(q) = resp.interact_pointer_pos()
        && let Some((i, dist)) = nearest(q)
        && dist < 10.0
        && i != 0
        && i != pts.len() - 1
    {
        let mut v = pts.clone();
        v.remove(i);
        new_pts = Some(v);
    } else if resp.drag_started()
        && let Some(q) = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos())
    {
        // pick the point under the press, not where the pointer is once the drag threshold passed
        let _ = app.run("develop.beginInteraction", json!({"label": "Tone Curve"}));
        match nearest(q) {
            Some((i, dist)) if dist < 10.0 => dragging = Some(i),
            _ => {
                let (x, y) = from_screen(q);
                let mut v = pts.clone();
                let i = v.iter().position(|p| p.x > x).unwrap_or(v.len());
                v.insert(i, Point::new(x, y));
                dragging = Some(i);
                new_pts = Some(v);
            }
        }
    } else if resp.dragged()
        && let (Some(i), Some(q)) = (dragging, resp.interact_pointer_pos())
    {
        let mut v = pts.clone();
        // ⌥-drag = fine adjustment, as in Lightroom: the point moves a tenth of the pointer's
        // motion (relative to where it is, so pressing ⌥ mid-drag doesn't make it jump)
        let fine = ui.input(|i| i.modifiers.alt);
        let (mut x, y) = match v.get(i) {
            Some(cur) if fine => {
                let dd = ui.input(|i| i.pointer.delta());
                ((cur.x + (dd.x / r.width()) as f64 * 0.1).clamp(0.0, 1.0), (cur.y - (dd.y / r.height()) as f64 * 0.1).clamp(0.0, 1.0))
            }
            _ => from_screen(q),
        };
        if i < v.len() {
            let lo = if i == 0 { 0.0 } else { v[i - 1].x + 0.01 };
            let hi = if i + 1 >= v.len() { 1.0 } else { v[i + 1].x - 0.01 };
            if i == 0 {
                x = x.min(hi);
            } else if i + 1 == v.len() {
                x = x.max(lo);
            } else if lo <= hi {
                x = x.clamp(lo, hi);
            } else {
                // neighbours closer than the 0.02 spacing (two clicks side by side, or points
                // from a file) leave no room: the point stays midway (`clamp` would panic: min above max)
                x = (v[i - 1].x + v[i + 1].x) / 2.0;
            }
            v[i] = Point::new(x, y);
            new_pts = Some(v);
        }
    } else if resp.clicked()
        && let Some(q) = resp.interact_pointer_pos()
        && nearest(q).is_none_or(|(_, dist)| dist >= 10.0)
    {
        let (x, y) = from_screen(q);
        let mut v = pts.clone();
        let i = v.iter().position(|p| p.x > x).unwrap_or(v.len());
        v.insert(i, Point::new(x, y));
        new_pts = Some(v);
    }
    if let Some(v) = new_pts {
        let arr: Vec<[f64; 2]> = v.iter().map(|p| [p.x, p.y]).collect();
        let _ = app.run("develop.curve", json!({"channel": ch, "points": arr}));
    }
    if resp.drag_stopped() {
        dragging = None;
        let _ = app.run("develop.endInteraction", json!({}));
    }
    ui.data_mut(|dd| dd.insert_temp(drag_id, dragging));
    curve_footer(app, ui, d);
    control(app, ui, d, "curve.refineSaturation", true);
    let _ = id;
}

/// The row under the curve graph: point-curve presets and reset every curve.
fn curve_footer(app: &mut LightcraftApp, ui: &mut egui::Ui, d: &DevelopSettings) {
    use lightcraft_engine::cmd::curves::{all_presets, matching_preset};
    let t = Tokens::get(ui.ctx());
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 2, bottom: 4 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(crate::i18n::tr("Point Curve")).font(t.font(12.0)).color(t.text_label));
            let current = matching_preset(&app.session, &d.curve);
            let builtin = current.as_ref().is_none_or(|name| all_presets(&app.session).iter().any(|p| p.builtin && &p.name == name));
            let label = crate::i18n::builtin_label(current.as_deref().unwrap_or("Custom"), builtin);
            let r = crate::widgets::dropdown(ui, "curvePreset", label, t.font(12.0), t.text);
            egui::Popup::menu(&r).show(|ui| {
                ui.set_min_width(180.0);
                let presets = all_presets(&app.session);
                let mut user_seen = false;
                for p in &presets {
                    if !p.builtin && !user_seen {
                        user_seen = true;
                        ui.separator();
                    }
                    let r = ui.selectable_label(current.as_deref() == Some(p.name.as_str()), crate::i18n::builtin_label(&p.name, p.builtin));
                    register(ui.ctx(), format!("curvePreset:{}", p.name), r.rect);
                    if r.clicked() {
                        let _ = app.run("curve.applyPreset", json!({"name": p.name}));
                        ui.close();
                    }
                    if !p.builtin {
                        r.context_menu(|ui| {
                            if ui.button(crate::i18n::tr("Delete Preset")).clicked() {
                                let _ = app.run("curve.deletePreset", json!({"name": p.name}));
                                ui.close();
                            }
                        });
                    }
                }
                ui.separator();
                let shaped = current.as_deref() != Some("Linear");
                let r = ui.add_enabled(shaped, egui::Button::new(crate::i18n::tr("Save Point Curve…")));
                register(ui.ctx(), "curvePresetMenu:save", r.rect);
                if r.clicked() {
                    crate::panels::dialogs::prompt(app, "Save Point Curve Preset", "Preset name", "", "curve.savePreset", json!({}), "name");
                    ui.close();
                }
                if ui.add_enabled(app.services.pick_curve_preset_files.is_some(), egui::Button::new(crate::i18n::tr("Import Presets…"))).clicked() {
                    let _ = app.run("file.importCurvePresets", json!({}));
                    ui.close();
                }
                let can_export = app.services.save_curve_preset_file.is_some() && !app.session.curve_presets.is_empty();
                if ui.add_enabled(can_export, egui::Button::new(crate::i18n::tr("Export Presets…"))).clicked() {
                    let _ = app.run("file.exportCurvePresets", json!({}));
                    ui.close();
                }
                if presets.iter().any(|p| !p.builtin) {
                    ui.label(RichText::new(crate::i18n::tr("Right-click a preset to delete it")).font(t.font(11.0)).color(t.text_dim));
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let r = text_button(ui, "curveReset", crate::i18n::tr("Reset"), false).on_hover_text(crate::i18n::tr(
                    "Reset all curves: point curves (all channels) and parametric (double-click a channel to reset only that one)",
                ));
                if r.clicked() {
                    let _ = app.run("curve.reset", json!({"channel": "all"}));
                }
            });
        });
    });
}

/// Right-click menu of the curve graph.
fn curve_reset_menu(app: &mut LightcraftApp, ui: &mut egui::Ui, ch: &str) {
    let label = if ch == "parametric" {
        crate::i18n::tr("Reset Parametric Curve").to_string()
    } else {
        crate::i18n::tr_format!("Reset {} Channel", crate::i18n::tr(channel_label(ch)))
    };
    let r = ui.button(label);
    register(ui.ctx(), "curveMenu:resetChannel", r.rect);
    if r.clicked() {
        let _ = app.run("curve.reset", json!({"channel": ch}));
        ui.close();
    }
    let r = ui.button(crate::i18n::tr("Reset All Curves"));
    register(ui.ctx(), "curveMenu:resetAll", r.rect);
    if r.clicked() {
        let _ = app.run("curve.reset", json!({"channel": "all"}));
        ui.close();
    }
}

fn channel_label(ch: &str) -> &'static str {
    match ch {
        "red" => "Red",
        "green" => "Green",
        "blue" => "Blue",
        _ => "RGB",
    }
}

/// Toggle for a targeted-adjustment tool (`tool` = `tat:<target>`).
fn tat_button(app: &mut LightcraftApp, ui: &mut egui::Ui, tool: &str, tip: &str) {
    let active = app.ui.tool == tool;
    let id = tool.replace(':', "-");
    if crate::widgets::icon_button(ui, &id, Icon::Target, vec2(26.0, 26.0), active, true, tip).clicked() {
        set_tat(app, ui.ctx(), tool, active);
    }
}

/// Turn targeted-adjustment tool `tool` off (`active`) or on. Turning it on says how to use and
/// leave it: while it is on, dragging on the photo adjusts instead of zooming or panning, which is
/// easy to miss when the button looks like a tab.
fn set_tat(app: &mut LightcraftApp, ctx: &egui::Context, tool: &str, active: bool) {
    if active {
        app.ui.tool.clear();
    } else {
        app.ui.tool = tool.to_string();
        app.toast(ctx, crate::i18n::tr("Targeted adjustment on: drag up or down on the photo · Esc to stop"));
    }
}

// ------------------------------------------------------------------------------ colour mixer

fn mixer(app: &mut LightcraftApp, ui: &mut egui::Ui, d: &DevelopSettings) {
    let t = Tokens::get(ui.ctx());
    let bands = lightcraft_develop::MIXER_BANDS;
    let sel = bands.iter().position(|b| *b == app.ui.mixer_mode).unwrap_or(0);
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 8, bottom: 4 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 5.0;
            for (i, b) in bands.iter().enumerate() {
                let (r, resp) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
                crate::widgets::announce(&resp, crate::i18n::tr(b));
                register(ui.ctx(), format!("mixerBand:{b}"), r);
                ui.painter().circle_filled(r.center(), 9.0, hex(BAND_COLORS[i]));
                if i == sel {
                    ui.painter().circle_stroke(r.center(), 10.5, Stroke::new(1.5, t.text));
                    ui.painter().circle_filled(r.center(), 2.0, Color32::WHITE);
                }
                if resp.clicked() {
                    app.ui.mixer_mode = b.to_string();
                }
            }
        });
    });
    // targeted adjustment: pick the attribute, then drag on the photo
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 2, bottom: 2 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let (r, _) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
            paint(ui.painter(), r, Icon::Target, t.icon);
            let items: &[(&str, &str)] =
                if crate::is_bw(d) { &[("lum", "B&W Mix")] } else { &[("hue", "Hue"), ("sat", "Saturation"), ("lum", "Luminance")] };
            for (k, label) in items {
                let tool = format!("tat:{k}");
                let active = app.ui.tool == tool;
                if text_button(ui, &format!("tatMixer-{k}"), crate::i18n::tr(label), active)
                    .on_hover_text(crate::i18n::tr("Targeted adjustment: drag up/down on the photo to adjust the colours there"))
                    .clicked()
                {
                    set_tat(app, ui.ctx(), &tool, active);
                }
            }
        });
    });
    let b = bands[sel];
    if crate::is_bw(d) {
        ui.horizontal(|ui| {
            ui.add_space(24.0);
            if crate::widgets::text_button(ui, "bwAuto", crate::i18n::tr("Auto"), false)
                .on_hover_text(crate::i18n::tr("Set the mix from the photo's colours"))
                .clicked()
            {
                let _ = app.run("develop.autoBwMix", json!({}));
            }
        });
        control(app, ui, d, &format!("bw.{b}"), true);
    } else {
        for k in ["hue", "sat", "lum"] {
            control(app, ui, d, &format!("mixer.{b}.{k}"), true);
        }
    }
}

// ------------------------------------------------------------------------------ point color

/// Display colour of an OkLCh sample (lightness, chroma, hue in degrees).
fn oklch_color(l: f64, c: f64, h_deg: f64) -> Color32 {
    use lightcraft_color::perceptual::{lch_to_lab, oklab_to_2020};
    let lin = oklab_to_2020(lch_to_lab([l as f32, c as f32, (h_deg as f32).to_radians()]));
    let s = lightcraft_color::REC2020.to_space(&lightcraft_color::SRGB).apply_f32(lin);
    let e = s.map(|v| (lightcraft_color::transfer::linear_to_srgb(v.clamp(0.0, 1.0)) * 255.0).round() as u8);
    Color32::from_rgb(e[0], e[1], e[2])
}

/// Point Color: swatches of the samples (+ the eyedropper), the selected sample's shifts and range,
/// and "Visualize range".
fn point_color(app: &mut LightcraftApp, ui: &mut egui::Ui, d: &DevelopSettings) {
    let t = Tokens::get(ui.ctx());
    let n = d.point_colors.len();
    if app.ui.point_color >= n && n > 0 {
        app.ui.point_color = n - 1;
    }
    let sel = app.ui.point_color;
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 8, bottom: 4 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 5.0;
            let active = app.ui.tool == "pointColor";
            let full = n >= lightcraft_develop::MAX_POINT_COLORS;
            if crate::widgets::icon_button(
                ui,
                "pointColorPicker",
                Icon::Picker,
                vec2(26.0, 26.0),
                active,
                !full,
                crate::i18n::tr("Sample a colour on the photo"),
            )
            .clicked()
            {
                app.ui.tool = if active { String::new() } else { "pointColor".into() };
            }
            for (i, p) in d.point_colors.iter().enumerate() {
                let (r, resp) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
                crate::widgets::announce(&resp, format!("{} {}", crate::i18n::tr("Point Color"), i + 1));
                register(ui.ctx(), format!("pointColor:{i}"), r);
                ui.painter().rect_filled(r.shrink(2.0), 3.0, oklch_color(p.lum, p.chroma, p.hue));
                if i == sel {
                    ui.painter().rect_stroke(r, 4.0, Stroke::new(1.5, t.text), egui::StrokeKind::Inside);
                }
                if resp.clicked() {
                    app.ui.point_color = i;
                }
            }
        });
    });
    if n == 0 {
        egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 2, bottom: 8 }).show(ui, |ui| {
            ui.label(egui::RichText::new(crate::i18n::tr("Pick a colour on the photo with the eyedropper.")).size(12.0).color(t.text_dim));
        });
        return;
    }
    for k in ["hueShift", "satShift", "lumShift", "variance"] {
        control(app, ui, d, &format!("pointColor.{sel}.{k}"), true);
    }
    sub_title(ui, crate::i18n::tr("Range"));
    for k in ["range", "hueRange", "satRange", "lumRange"] {
        control(app, ui, d, &format!("pointColor.{sel}.{k}"), true);
    }
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 4, bottom: 8 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            let mut v = app.ui.point_color_visualize;
            if ui.checkbox(&mut v, crate::i18n::tr("Visualize range")).changed() {
                app.ui.point_color_visualize = v;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if text_button(ui, "pointColorDelete", crate::i18n::tr("Delete"), false).clicked() {
                    let _ = app.run("pointColor.delete", json!({"index": sel}));
                }
            });
        });
    });
}

// ------------------------------------------------------------------------------ HDR

/// The Light panel's HDR row: the HDR switch and Visualize HDR, then (HDR on) the headroom limit.
/// The Light panel's HDR settings while HDR editing is on (the switch itself is the HDR button
/// next to B&W): Visualize HDR range and the headroom limit.
fn hdr_row(app: &mut LightcraftApp, ui: &mut egui::Ui, d: &DevelopSettings) {
    if !d.hdr.enabled {
        return;
    }
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 4, bottom: 4 }).show(ui, |ui| {
        let mut v = app.ui.hdr_visualize;
        let r = ui.checkbox(&mut v, "Visualize HDR");
        register(ui.ctx(), "check:hdrVisualize", r.rect);
        if r.on_hover_text("Grey below SDR white; yellow, orange, red, magenta up to 1, 2, 3 and over 3 stops above it").changed() {
            app.ui.hdr_visualize = v;
        }
    });
    control(app, ui, d, "hdr.maxEv", true);
}

// ------------------------------------------------------------------------------ colour grading

/// The colour grading tabs: key, tooltip, icon.
const GRADING_TABS: [(&str, &str, Icon); 4] = [
    ("3way", "3-Way", Icon::GradeAll),
    ("shadows", "Shadows", Icon::GradeShadows),
    ("midtones", "Midtones", Icon::GradeMidtones),
    ("highlights", "Highlights", Icon::GradeHighlights),
];

/// The wheels, in the order of `UiState::grading_hidden`.
const GRADING_WHEELS: [(&str, &str); 3] = [("shadows", "Shadows"), ("midtones", "Midtones"), ("highlights", "Highlights")];

fn grading(app: &mut LightcraftApp, ui: &mut egui::Ui, d: &DevelopSettings, photo: PhotoId) {
    let mode = GRADING_TABS.iter().find(|(k, ..)| *k == app.ui.grading_mode).map_or("3way", |(k, ..)| *k);
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 6, bottom: 2 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for (key, tip, icon) in GRADING_TABS {
                if crate::widgets::icon_button(ui, &format!("grading-{key}"), icon, vec2(28.0, 28.0), key == mode, true, crate::i18n::tr(tip))
                    .clicked()
                {
                    app.ui.grading_mode = key.to_string();
                }
            }
        });
    });
    let w = ui.available_width();
    if mode == "3way" {
        // three wheels of one size: midtones on top, shadows and highlights below
        let rad = 40.0;
        let (area, _) = ui.allocate_exact_size(vec2(w, 276.0), Sense::hover());
        let at = [
            pos2(area.left() + w * 0.28, area.top() + 196.0),
            pos2(area.center().x, area.top() + 70.0),
            pos2(area.left() + w * 0.72, area.top() + 196.0),
        ];
        for (i, c) in at.into_iter().enumerate() {
            grading_wheel(app, ui, d, photo, i, c, rad, true);
        }
        for c in ["grading.shadows.lum", "grading.midtones.lum", "grading.highlights.lum"] {
            control(app, ui, d, c, true);
        }
    } else {
        let i = GRADING_WHEELS.iter().position(|(k, _)| *k == mode).unwrap_or(1);
        let rad = (w * 0.3).clamp(40.0, 80.0);
        let (area, _) = ui.allocate_exact_size(vec2(w, rad * 2.0 + 40.0), Sense::hover());
        grading_wheel(app, ui, d, photo, i, pos2(area.center().x, area.top() + rad + 10.0), rad, false);
        for part in ["hue", "sat", "lum"] {
            control(app, ui, d, &format!("grading.{mode}.{part}"), true);
        }
    }
    for c in ["grading.blending", "grading.balance"] {
        control(app, ui, d, c, true);
    }
}

/// One colour grading wheel (`i` into [`GRADING_WHEELS`]) centred on `c`: drag to pick hue and
/// saturation, double-click to reset, and an eye at its bottom-left corner that hides its effect
/// on the photo for viewing. `readout`: show hue and saturation centred above the wheel while
/// hovered (3-Way); otherwise the wheel is the large one and gets saturation guide rings.
#[allow(clippy::too_many_arguments)]
fn grading_wheel(app: &mut LightcraftApp, ui: &mut egui::Ui, d: &DevelopSettings, photo: PhotoId, i: usize, c: Pos2, rad: f32, readout: bool) {
    let t = Tokens::get(ui.ctx());
    let Some(&(key, label)) = GRADING_WHEELS.get(i) else { return };
    let wh = match key {
        "shadows" => d.grading.shadows,
        "midtones" => d.grading.midtones,
        _ => d.grading.highlights,
    };
    let hidden = app.ui.grading_hidden_for(photo).get(i).copied().unwrap_or(false);
    let r = Rect::from_center_size(c, vec2(rad * 2.0, rad * 2.0));
    // the corners (eye, readout) count as the wheel for hovering
    let zone = r.expand(10.0);
    let hovering = ui.rect_contains_pointer(zone);
    let resp = ui.interact(r, egui::Id::new(("wheel", key)), Sense::click_and_drag());
    crate::widgets::announce(&resp, format!("{} {key}", crate::i18n::tr("Color wheel")));
    let resp = resp.on_hover_text(crate::i18n::tr(
        "Drag to set hue and saturation. Hold Shift to keep the hue, Cmd/Ctrl to keep the saturation, Alt/Option to fine-adjust. Double-click to reset.",
    ));
    register(ui.ctx(), format!("wheel:{key}"), r);
    paint_wheel(ui.painter(), c, rad);
    if hidden {
        ui.painter().circle_filled(c, rad, Color32::from_black_alpha(150));
    }
    // the large single wheel: dashed rings at 25 / 50 / 75 % saturation to judge strength by eye
    if !readout {
        for f in [0.25f32, 0.5, 0.75] {
            let ring_rad = rad * f;
            let n = (ring_rad * std::f32::consts::TAU / 2.0).max(8.0) as usize;
            let ring: Vec<Pos2> = (0..=n).map(|k| c + egui::Vec2::angled(k as f32 / n as f32 * std::f32::consts::TAU) * ring_rad).collect();
            ui.painter().extend(egui::Shape::dashed_line(&ring, Stroke::new(0.8, Color32::from_white_alpha(45)), 4.0, 3.5));
        }
    }
    let a = (wh.hue as f32).to_radians();
    let dot = c + vec2(a.cos(), -a.sin()) * rad * (wh.sat as f32 / 100.0);
    // Modifiers (as in Lightroom): Shift keeps the hue, Cmd/Ctrl keeps the saturation, Alt moves
    // the puck a tenth as far as the pointer. Pressing or releasing one mid-drag re-grabs the
    // puck as it is at that moment.
    let grab_id = egui::Id::new(("wheelGrab", key));
    let mods = ui.input(|i| WheelMods::from_modifiers(i.modifiers));
    let grab = match resp.interact_pointer_pos() {
        Some(q) if resp.dragged() || resp.clicked() => {
            let held = ui.data(|m| m.get_temp::<WheelGrab>(grab_id)).filter(|g| g.mods == mods && !resp.drag_started());
            Some(held.unwrap_or_else(|| {
                // a fine drag counts the pointer's travel from the press, not from the frame
                // egui first called it a drag
                let origin = if resp.drag_started() { ui.input(|i| i.pointer.press_origin()).unwrap_or(q) } else { q };
                let g = WheelGrab::new(mods, wh.hue as f32, wh.sat as f32, origin - c, rad);
                ui.data_mut(|m| m.insert_temp(grab_id, g));
                g
            }))
        }
        _ => {
            ui.data_mut(|m| m.remove::<WheelGrab>(grab_id));
            None
        }
    };
    // while a constraint is held: the guide for the one move left (the whole spoke for the hue,
    // the circle for the saturation)
    let guide = Stroke::new(1.5, Color32::from_white_alpha(170));
    match grab.and_then(|g| g.lock()) {
        Some(WheelLock::Hue(h)) => {
            let a = h.to_radians();
            ui.painter().line_segment([c, c + vec2(a.cos(), -a.sin()) * rad], guide);
        }
        Some(WheelLock::Sat(s)) => {
            ui.painter().circle_stroke(c, rad * s / 100.0, guide);
        }
        None => {}
    }
    // the neutral centre (issue #559), under the point
    ui.painter().circle_filled(c, 2.0, Color32::WHITE);
    ui.painter().circle_filled(dot, 5.0, if hidden { Color32::from_gray(150) } else { Color32::WHITE });
    ui.painter().circle_stroke(dot, 5.0, Stroke::new(1.0, Color32::BLACK));
    ui.painter().text(pos2(c.x, c.y + rad + 12.0), Align2::CENTER_CENTER, crate::i18n::tr(label), t.font(12.0), t.text_label);
    if resp.drag_started() {
        let _ = app.run("develop.beginInteraction", json!({"label": crate::i18n::tr_format!("{label} Grading", label = label)}));
    }
    if (resp.dragged() || resp.clicked())
        && let Some(q) = resp.interact_pointer_pos()
    {
        let (hue, sat) = grab.map_or_else(|| wheel_pick(q - c, rad, None), |g| g.pick(q - c, rad));
        let _ = app.run("develop.set", json!({"values": {format!("grading.{key}.hue"): hue.round(), format!("grading.{key}.sat"): sat.round()}}));
    }
    if resp.drag_stopped() || resp.clicked() {
        let _ = app.run("develop.endInteraction", json!({}));
    }
    if resp.double_clicked() {
        let _ = app.run("develop.set", json!({"values": {format!("grading.{key}.hue"): 0, format!("grading.{key}.sat"): 0}}));
    }
    // hue and saturation readout: centred above the wheel, shown like the eye (no fade)
    if readout && (hovering || resp.dragged()) {
        // set a little below the gap's midpoint: the eye reads it as centred there
        ui.painter().text(
            pos2(c.x, r.top() - 13.0),
            Align2::CENTER_CENTER,
            format!("H {}°  S {}", wh.hue.round() as i64, wh.sat.round() as i64),
            t.font(11.0),
            t.text_label,
        );
    }
    // the eye: on top of the wheel, shown while hovering or while the wheel is hidden
    if hovering || hidden {
        let eye = Rect::from_center_size(r.left_bottom() + vec2(2.0, -2.0), vec2(16.0, 16.0));
        let er = ui.interact(eye, egui::Id::new(("wheelEye", key)), Sense::click());
        crate::widgets::announce(&er, crate::i18n::tr("Show / hide"));
        register(ui.ctx(), format!("wheelEye:{key}"), eye);
        paint(ui.painter(), eye, if hidden { Icon::EyeOff } else { Icon::Eye }, if hidden { t.text_disabled } else { t.icon });
        let er = er.on_hover_text(crate::i18n::tr(if hidden { "Show this wheel's effect" } else { "Hide this wheel's effect" }));
        if er.clicked() {
            app.ui.set_grading_hidden(photo, i, !hidden);
        }
    }
}

/// A wheel's modifier state. Shift keeps the hue, Cmd/Ctrl the saturation (Shift wins when both
/// are down: keeping both would freeze the puck), Alt fine-adjusts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct WheelMods {
    pub keep_hue: bool,
    pub keep_sat: bool,
    pub fine: bool,
}

impl WheelMods {
    pub(crate) fn from_modifiers(m: egui::Modifiers) -> Self {
        Self { keep_hue: m.shift, keep_sat: m.command && !m.shift, fine: m.alt }
    }
}

/// What a constraint holds still while the pointer moves.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum WheelLock {
    /// Keep this hue (degrees): only the saturation changes, along the spoke at that hue.
    Hue(f32),
    /// Keep this saturation (0..100): only the hue changes, round the circle at that radius.
    Sat(f32),
}

/// Pointer travel to puck travel with Alt held, as for a slider's fine adjust.
const WHEEL_FINE: f32 = 0.1;

/// The puck as it was when the pointer grabbed it (or a modifier last changed): the hue and
/// saturation a constraint keeps, and the origin a fine adjust measures the pointer's travel from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct WheelGrab {
    pub mods: WheelMods,
    /// The puck's hue (degrees) and saturation (0..100) at the grab.
    hue: f32,
    sat: f32,
    /// The pointer at the grab, as an offset from the wheel's centre.
    origin: egui::Vec2,
}

impl WheelGrab {
    /// Grab a puck at `hue` / `sat` with the pointer `origin` from the centre of a wheel of
    /// radius `rad`. The kept hue is the puck's own, never the angle the pointer happens to be
    /// at. A puck at the centre (saturation under 1) has no hue yet, so there it is the angle of
    /// the pointer at the grab; the same rule covers a non-finite hue.
    pub(crate) fn new(mods: WheelMods, hue: f32, sat: f32, origin: egui::Vec2, rad: f32) -> Self {
        let sat = if sat.is_finite() { sat.clamp(0.0, 100.0) } else { 0.0 };
        let hue = if hue.is_finite() && sat >= 1.0 { hue.rem_euclid(360.0) } else { wheel_pick(origin, rad, None).0 };
        Self { mods, hue, sat, origin }
    }

    /// The constraint this grab holds, if any.
    pub(crate) fn lock(&self) -> Option<WheelLock> {
        if self.mods.keep_hue {
            Some(WheelLock::Hue(self.hue))
        } else if self.mods.keep_sat {
            Some(WheelLock::Sat(self.sat))
        } else {
            None
        }
    }

    /// The new hue and saturation for the pointer at offset `v` from the centre.
    pub(crate) fn pick(&self, v: egui::Vec2, rad: f32) -> (f32, f32) {
        let v = if self.mods.fine {
            // the puck where it was, moved a tenth of the pointer's travel since the grab
            let a = self.hue.to_radians();
            let puck = vec2(a.cos(), -a.sin()) * rad.max(1.0) * (self.sat / 100.0);
            puck + (v - self.origin) * WHEEL_FINE
        } else {
            v
        };
        wheel_pick(v, rad, self.lock())
    }
}

/// Hue (degrees) and saturation (0..100) for a pointer at offset `v` from a wheel's centre.
/// `lock`: hold the hue (the saturation is `v` projected onto that hue's spoke) or hold the
/// saturation (the hue is the angle of `v`). A non-finite `v` gives a finite answer (the locked
/// value, else 0).
pub(crate) fn wheel_pick(v: egui::Vec2, rad: f32, lock: Option<WheelLock>) -> (f32, f32) {
    let rad = rad.max(1.0);
    let v = if v.x.is_finite() && v.y.is_finite() { v } else { egui::Vec2::ZERO };
    let angle = || ((-v.y).atan2(v.x).to_degrees() + 360.0) % 360.0;
    match lock {
        Some(WheelLock::Hue(h)) => {
            let h = if h.is_finite() { h } else { 0.0 };
            let a = h.to_radians();
            let along = v.x * a.cos() - v.y * a.sin();
            (h, (along / rad * 100.0).clamp(0.0, 100.0))
        }
        Some(WheelLock::Sat(s)) => (angle(), if s.is_finite() { s.clamp(0.0, 100.0) } else { 0.0 }),
        None => (angle(), (v.length() / rad * 100.0).min(100.0)),
    }
}

fn paint_wheel(p: &egui::Painter, c: Pos2, rad: f32) {
    p.add(wheel_mesh(c, rad));
    p.circle_stroke(c, rad, Stroke::new(1.0, Color32::from_gray(40)));
}

/// The picture of a grading wheel: a hue disc whose rim is fully saturated display sRGB at full
/// brightness, fading to a neutral grey at the centre. It is built from concentric rings so the
/// saturation ramps evenly with the radius instead of one fan of triangles blending each hue
/// straight into grey (which made the whole disc look muddy, issue #77). Only the drawing:
/// what a position means for the edit is [`wheel_pick`].
pub(crate) fn wheel_mesh(c: Pos2, rad: f32) -> Mesh {
    const SEGMENTS: u32 = 72;
    const RINGS: u32 = 12;
    /// Brightness (HSV value) at the centre; the rim is 1.0.
    const CENTRE_V: f32 = 0.6;
    let colour = |hue_deg: f32, t: f32| {
        let rgb = lightcraft_color::perceptual::hsv_to_rgb(hue_deg, t, CENTRE_V + (1.0 - CENTRE_V) * t);
        let u = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
        Color32::from_rgb(u(rgb[0]), u(rgb[1]), u(rgb[2]))
    };
    let mut mesh = Mesh::default();
    mesh.vertices.push(Vertex { pos: c, uv: Pos2::ZERO, color: colour(0.0, 0.0) });
    // ring k (1..=RINGS) holds SEGMENTS + 1 vertices starting at index 1 + (k - 1) * (SEGMENTS + 1)
    let row = SEGMENTS + 1;
    for k in 1..=RINGS {
        let t = k as f32 / RINGS as f32;
        for i in 0..=SEGMENTS {
            let a = i as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
            mesh.vertices.push(Vertex { pos: c + vec2(a.cos(), -a.sin()) * rad * t, uv: Pos2::ZERO, color: colour(a.to_degrees(), t) });
        }
    }
    for i in 0..SEGMENTS {
        mesh.indices.extend([0, 1 + i, 2 + i]);
    }
    for k in 1..RINGS {
        let inner = 1 + (k - 1) * row;
        let outer = inner + row;
        for i in 0..SEGMENTS {
            mesh.indices.extend([inner + i, outer + i, outer + i + 1, inner + i, outer + i + 1, inner + i + 1]);
        }
    }
    mesh
}

/// Quick Develop (grid with several photos selected): relative steps applied to every selected
/// photo from its own value.
fn quick_develop(app: &mut LightcraftApp, ui: &mut egui::Ui, n: usize) {
    let t = Tokens::get(ui.ctx());
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 0, bottom: 10 }).show(ui, |ui| {
        ui.label(egui::RichText::new(crate::i18n::tr_format!("Quick Develop · {n} photos", n = n)).color(t.text_label).size(12.5));
        ui.add_space(4.0);
        let rows: [(&str, &str, f64, f64); 9] = [
            ("Exposure", "light.exposure", 1.0 / 3.0, 1.0),
            ("Contrast", "light.contrast", 5.0, 20.0),
            ("Highlights", "light.highlights", 5.0, 20.0),
            ("Shadows", "light.shadows", 5.0, 20.0),
            ("Whites", "light.whites", 5.0, 20.0),
            ("Blacks", "light.blacks", 5.0, 20.0),
            ("Clarity", "effects.clarity", 5.0, 20.0),
            ("Vibrance", "color.vibrance", 5.0, 20.0),
            ("Temp", "wb.temp", 100.0, 500.0),
        ];
        egui::Grid::new("quick-develop").num_columns(5).spacing([4.0, 3.0]).show(ui, |ui| {
            for (label, ctl, small, big) in rows {
                ui.label(egui::RichText::new(crate::i18n::tr(label)).size(11.5).color(t.text_dim));
                // Keep the existing automation ids while drawing all step arrows as vectors.
                for (txt, icon, d) in [
                    ("◀◀", Icon::ChevronDoubleLeft, -big),
                    ("◀", Icon::ChevronLeft, -small),
                    ("▶", Icon::ChevronRight, small),
                    ("▶▶", Icon::ChevronDoubleRight, big),
                ] {
                    let r = crate::widgets::arrow_button(ui, "", icon, false);
                    r.widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::WidgetType::Button,
                            ui.is_enabled(),
                            crate::i18n::tr_format!("{label} {d:+} on every selected photo", d = d, label = crate::i18n::tr(label)),
                        )
                    });
                    crate::widgets::register(ui.ctx(), format!("button:quick-{ctl}-{txt}"), r.rect);
                    if r.on_hover_text(crate::i18n::tr_format!("{label} {d:+} on every selected photo", d = d, label = crate::i18n::tr(label)))
                        .clicked()
                    {
                        let _ = app.run("develop.quickAdjust", json!({"control": ctl, "delta": d}));
                    }
                }
                ui.end_row();
            }
        });
    });
}

/// The preview-only notice's Reload from Disk: re-read the photo's file (a raw a newer version can
/// decode, whose drive was offline when the library opened), and say what came of it.
pub(crate) fn reload_preview_only(app: &mut LightcraftApp, ctx: &egui::Context, id: PhotoId) {
    let r = app.run("photo.reload", json!({"ids": [id.0]}));
    let decoded = app.session.catalog.photo(id).is_some_and(|p| p.preview_only.is_none());
    match r {
        Err(e) => app.toast_error(ctx, e),
        Ok(_) if decoded => app.toast(ctx, crate::i18n::tr("This raw is now developed from its raw data")),
        Ok(_) => app.toast(ctx, crate::i18n::tr("This raw variant still can't be decoded")),
    }
}

/// The Reset button's tooltip with the reset shortcut as this platform writes it (issue #1751).
fn reset_tip(keymap: &crate::shortcuts::Keymap, mac: bool) -> String {
    let base = crate::i18n::tr("Reset all edits");
    match crate::shortcuts::shortcut_of(keymap, "develop.reset") {
        Some(sc) => format!("{base} ({})", crate::menubar::shortcut_text(sc, mac)),
        None => base.to_string(),
    }
}

#[cfg(test)]
mod curve_histogram_tests {
    use super::*;

    #[test]
    fn curve_backdrop_tracks_luma_and_selected_channel() {
        let img = lightcraft_raster::Rgba8::from_fn(64, 32, |x, _| if x < 32 { [32, 100, 32, 255] } else { [224, 100, 224, 255] });
        let h = lightcraft_raster::Histogram::of_srgb8(&img);
        let red = curve_histogram_profile(&h, "red");
        let green = curve_histogram_profile(&h, "green");
        let luma = curve_histogram_profile(&h, "master");
        assert!(red[32] > red[150] && red[224] > red[150]);
        assert!(green[100] > green[32] && green[100] > green[224]);
        assert!(luma.iter().any(|v| *v > 0.0));
    }

    #[test]
    fn hdr_backdrop_uses_only_the_sdr_bins_and_never_overflows() {
        let h = lightcraft_raster::Histogram::of_hdr(&[[0.1, 0.1, 0.1], [1.0, 1.0, 1.0], [8.0, 8.0, 8.0]], 4.0);
        let values = curve_histogram_profile(&h, "parametric");
        assert!(values.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)));
        assert!(values.iter().any(|v| *v > 0.0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_hdr_edit_keeps_its_hdr_histogram_through_8_bit_renders() {
        let ctx = egui::Context::default();
        let id = PhotoId(7);
        let sdr = Histogram::of_hdr(&[], 5.0);
        let sdr = Histogram { hdr: None, ..sdr };
        let hdr = |v: f32| Histogram::of_hdr(&[[v, v, v]], 5.0);
        // nothing kept yet: what arrives
        assert_eq!(steady_hdr_histogram(&ctx, id, true, Some(sdr.clone())), Some(sdr.clone()));
        assert_eq!(steady_hdr_histogram(&ctx, id, true, Some(hdr(2.0))), Some(hdr(2.0)));
        // a cached thumbnail's 8-bit histogram (or none) doesn't swap the panel to SDR
        assert_eq!(steady_hdr_histogram(&ctx, id, true, Some(sdr.clone())), Some(hdr(2.0)));
        assert_eq!(steady_hdr_histogram(&ctx, id, true, None), Some(hdr(2.0)));
        // the next HDR render replaces it: the histogram follows the slider
        assert_eq!(steady_hdr_histogram(&ctx, id, true, Some(hdr(4.0))), Some(hdr(4.0)));
        assert_eq!(steady_hdr_histogram(&ctx, id, true, Some(sdr.clone())), Some(hdr(4.0)));
        // HDR turned off: the SDR histogram at once; other photos keep their own
        assert_eq!(steady_hdr_histogram(&ctx, id, false, Some(sdr.clone())), Some(sdr.clone()));
        assert_eq!(steady_hdr_histogram(&ctx, PhotoId(8), true, Some(sdr.clone())), Some(sdr));
    }

    /// Issue #1751: Windows and Linux read Ctrl where macOS reads Cmd, and a rebound shortcut shows.
    #[test]
    fn reset_tooltip_names_the_shortcut_of_this_platform() {
        let keymap = crate::shortcuts::Keymap::new();
        assert_eq!(reset_tip(&keymap, false), "Reset all edits (Ctrl+Shift+R)");
        assert_eq!(reset_tip(&keymap, true), "Reset all edits (⌘⇧R)");
        let rebound: crate::shortcuts::Keymap = [("develop.reset".to_string(), "Cmd+Alt+R".to_string())].into();
        assert_eq!(reset_tip(&rebound, false), "Reset all edits (Ctrl+Alt+R)");
        let unbound: crate::shortcuts::Keymap = [("develop.reset".to_string(), String::new())].into();
        assert_eq!(reset_tip(&unbound, false), "Reset all edits");
    }
}
