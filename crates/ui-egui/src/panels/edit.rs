//! The Edit panel: histogram, Auto/B&W, profile, and the Light / Color / Effects / Detail / Optics
//! sections, built from the engine's control specs.

use egui::epaint::{Mesh, Vertex};
use egui::{Align2, Color32, Pos2, Rect, RichText, Sense, Stroke, pos2, vec2};
use lcc_catalog::PhotoId;
use lcc_develop::{ControlSpec, DevelopSettings, Section, ToneCurve, Track, WbMode, controls};
use lcc_geom::Point;
use lcc_pipeline::{PARAM_AMOUNTS, ParametricCurve};
use lcc_raster::{Clipped, Histogram, HistogramMode, HistogramScale};
use serde_json::{Value, json};

use crate::LccApp;
use crate::icons::{Icon, paint};
use crate::render::Slot;
use crate::state::RightPanel;
use crate::theme::Tokens;
use crate::widgets::{BAND_COLORS, SliderOut, divider, flyout_row, hex, register, section_header, slider, text_button};

/// Commit a slider interaction: begin → live updates → end, so a drag is one undo step.
pub fn apply_slider_out(app: &mut LccApp, spec: &ControlSpec, out: SliderOut, mut set: impl FnMut(&mut LccApp, f64) -> Result<Value, String>) {
    // a slider touched puts a picker away: the slider's own change is what is wanted now
    if out.drag_started || out.reset || out.value.is_some() {
        app.ui.put_away_picker();
    }
    if out.drag_started && !out.reset {
        // a MIDI turn still inside its idle window must not outlive the hand-over to the mouse
        app.end_midi_interaction();
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

pub(crate) fn control(app: &mut LccApp, ui: &mut egui::Ui, d: &DevelopSettings, id: &str, enabled: bool) {
    control_as(app, ui, d, id, enabled, None);
}

/// [`control`] under a shorter name, for a slider whose section already says what it belongs to
/// (issue #346: "Hue" under "Red Primary", not "Red Hue").
pub(crate) fn control_as(app: &mut LccApp, ui: &mut egui::Ui, d: &DevelopSettings, id: &str, enabled: bool, label: Option<&str>) {
    // the tool profile in use hides it, or holds it in its Custom group
    if !shown_here(app, ui, id) {
        return;
    }
    draw_control(app, ui, d, id, enabled, label);
}

/// Whether the Edit panel's tool profile lets a slider or option (`option:*`) show where it is drawn: always outside
/// the full Edit panel (a Modern card, the profile browser), and in it unless the profile hides it or moved it to its
/// Custom group.
pub(crate) fn shown_here(app: &LccApp, ui: &egui::Ui, item: &str) -> bool {
    !ui.ctx().data(|d| d.get_temp::<bool>(egui::Id::new(TOOL_FILTER))).unwrap_or(false) || crate::tool_profiles::item_shown(app, item)
}

/// [`shown_here`] for a whole group (an Edit section).
fn shown_here_group(app: &LccApp, ui: &egui::Ui, group: &str) -> bool {
    !ui.ctx().data(|d| d.get_temp::<bool>(egui::Id::new(TOOL_FILTER))).unwrap_or(false) || crate::tool_profiles::group_shown(app, group)
}

/// Set while the full Edit panel draws: the tool profile in use filters its sliders.
const TOOL_FILTER: &str = "edit-tool-profile-filter";

fn draw_control(app: &mut LccApp, ui: &mut egui::Ui, d: &DevelopSettings, id: &str, enabled: bool, label: Option<&str>) {
    let Some(spec) = controls::find(id) else { return };
    let v = controls::get(d, id).unwrap_or(spec.default);
    // Compact: the short name under its group's caption, so the section's name column stays narrow
    let label = label.or_else(|| if crate::theme::compact(ui.ctx()) { short_name(id) } else { None });
    // greyed out inside a disabled scope (a control video can't take)
    let out = slider(ui, spec, v, enabled && ui.is_enabled(), label);
    apply_slider_out(app, spec, out, |app, v| app.run("develop.set", json!({"control": id, "value": v})));
}

/// The short name a slider shows in the Compact layout under its group's caption (Grain ▸ Amount,
/// Sharpening ▸ Amount, as Lightroom labels them), so a section's name column stays narrow and its
/// tracks long (LAYOUT-DENSE, ported from OneSlips). The control keeps its full name everywhere
/// else (Classic, commands, the console, menus).
pub(crate) fn short_name(id: &str) -> Option<&'static str> {
    Some(match id {
        "vignette.amount" | "grain.amount" | "detail.sharpenAmount" | "optics.defringePurple" | "optics.defringeGreen" => "Amount",
        "detail.sharpenRadius" => "Radius",
        "detail.sharpenDetail" => "Detail",
        "detail.sharpenMasking" => "Masking",
        "vignette.midpoint" => "Midpoint",
        "vignette.feather" => "Feather",
        "vignette.roundness" => "Roundness",
        "vignette.highlights" => "Highlights",
        "grain.size" => "Size",
        "grain.sizeVariation" => "Variation",
        "grain.roughness" => "Roughness",
        "detail.nrLuminance" => "Luminance",
        "detail.nrColor" => "Color",
        "optics.profileDistortion" => "Distortion",
        "optics.profileVignetting" => "Vignetting",
        "optics.vignettingCalibrationAmount" => "Amount",
        "optics.vignettingMidpoint" => "Midpoint",
        "optics.defringePurpleHueLo" | "optics.defringeGreenHueLo" => "Hue Low",
        "optics.defringePurpleHueHi" | "optics.defringeGreenHueHi" => "Hue High",
        "optics.caRed" => "Red/Cyan",
        "optics.caBlue" => "Blue/Yellow",
        "calibration.shadowsTint" => "Tint",
        "calibration.redHue" | "calibration.greenHue" | "calibration.blueHue" => "Hue",
        "calibration.redSat" | "calibration.greenSat" | "calibration.blueSat" => "Saturation",
        "hdr.sdrBrightness" => "Brightness",
        "hdr.sdrContrast" => "Contrast",
        "hdr.sdrHighlights" => "Highlights",
        "hdr.sdrShadows" => "Shadows",
        "hdr.sdrWhites" => "Whites",
        "hdr.sdrClarity" => "Clarity",
        _ => return None,
    })
}

/// Relative temperature scale for rendered (non-raw) files: −100..100 ↔ Kelvin via mired shift.
pub(crate) static REL_TEMP: ControlSpec = ControlSpec {
    id: "wb.tempRel",
    label: "Temp",
    section: Section::Color,
    min: -100.0,
    max: 100.0,
    default: 0.0,
    step: 1.0,
    decimals: 0,
    track: Track::Temp,
    shade: lcc_develop::Shade::Plain,
};
pub(crate) static REL_TINT: ControlSpec = ControlSpec {
    id: "wb.tintRel",
    label: "Tint",
    section: Section::Color,
    min: -100.0,
    max: 100.0,
    default: 0.0,
    step: 1.0,
    decimals: 0,
    track: Track::Tint,
    shade: lcc_develop::Shade::Plain,
};

pub(crate) fn k_to_rel(k: f64) -> f64 {
    ((1e6 / 6500.0 - 1e6 / k) / 0.8).clamp(-100.0, 100.0)
}
pub(crate) fn rel_to_k(r: f64) -> f64 {
    1e6 / (1e6 / 6500.0 - r * 0.8)
}

pub fn show(app: &mut LccApp, ui: &mut egui::Ui, id: PhotoId) {
    pinned_histogram(app, ui, id);
    show_controls(app, ui, id);
}

/// The histogram at the head of the Edit panel (when shown). The right panel draws it above its
/// scroll area, so it stays in view while the controls under it scroll.
pub fn pinned_histogram(app: &mut LccApp, ui: &mut egui::Ui, id: PhotoId) {
    if app.ui.histogram {
        histogram(app, ui, id);
    }
}

/// The Edit panel under its histogram: soft proofing, the header and every section.
/// The Edit panel with one section only, open and without its header (`key`: a section's id, or
/// `profile` for the rows above Basic, `plugins` for the plug-in filters): the Modern layout's cards.
pub(crate) fn show_section(app: &mut LccApp, ui: &mut egui::Ui, id: PhotoId, key: &str) {
    ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new(ONLY_SECTION), key.to_string()));
    show_controls(app, ui, id);
    ui.ctx().data_mut(|d| d.remove::<String>(egui::Id::new(ONLY_SECTION)));
}

const ONLY_SECTION: &str = "edit-only-section";

/// The one section [`show_section`] asked for, if any.
fn only_section(ui: &egui::Ui) -> Option<String> {
    ui.ctx().data(|d| d.get_temp::<String>(egui::Id::new(ONLY_SECTION)))
}

pub fn show_controls(app: &mut LccApp, ui: &mut egui::Ui, id: PhotoId) {
    let t = Tokens::get(ui.ctx());
    let only = only_section(ui);
    let shows = |key: &str| only.as_deref().is_none_or(|o| o == key);
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
        let (temp, tint) = lcc_engine::pipeline::local::effective_wb(&info, &d);
        let wb = &mut std::sync::Arc::make_mut(&mut d).wb;
        wb.temp = temp;
        wb.tint = tint;
    }
    let preview_only = app.session.catalog.photo(id).and_then(|p| p.preview_only.clone());

    if app.ui.soft_proof && only.is_none() {
        soft_proofing(app, ui, id);
    }
    // the full panel: the tool profile in use picks its groups and sliders (a Modern card shows its section whole)
    let full = only.is_none();
    ui.ctx().data_mut(|m| m.insert_temp(egui::Id::new(TOOL_FILTER), full));
    let dense = t.dense && full;
    // the Modern layout's Profile & Treatment card: the rows above the sliders alone, with the way to the profiles
    if !full && shows("profile") {
        header_rows(app, ui, id, &d, preview_only.as_deref(), true);
    }
    // no header above the sections: the tool profile's row, then the rows above the sliders open Basic itself (as
    // Lightroom Classic's Basic panel has its Treatment row); with Basic switched off they stand on their own
    let basic = crate::tool_profiles::group_shown(app, "light");
    if full {
        tool_profile_row(app, ui);
        if !basic {
            header_rows(app, ui, id, &d, preview_only.as_deref(), false);
        }
    }
    let video = crate::video_ui::photo_is_video(app, id);
    // the profile's Custom group: its sliders, in its order, first
    let custom = app.ui.settings.tool_profiles.active().custom.clone();
    if full && !custom.is_empty() {
        section_with(app, ui, &d, crate::tool_profiles::CUSTOM, "Custom", false, None, |app, ui, d| {
            ui.push_id("toolProfileCustom", |ui| {
                for c in &custom {
                    custom_control(app, ui, d, c, raw);
                }
            });
            ui.add_space(8.0);
        });
    }
    // Lightroom Classic's Basic panel: white balance, then Tone (with Auto), then Presence
    // (Lightroom Classic's Basic has no on/off switch: it shows only while an older edit has Basic or Color switched off,
    // so that edit can be switched back on)
    let basic_switch = !d.section_enabled("light") || !d.section_enabled("color");
    section_with(app, ui, &d, "light", "Basic", basic_switch, None, |app, ui, d| {
        if full {
            header_rows(app, ui, id, d, preview_only.as_deref(), false);
        }
        wb_rows(app, ui, d, raw);
        sub_title(ui, "Tone");
        crate::video_ui::video_gate(ui, video, |ui| hdr_row(app, ui, d));
        for c in ["light.exposure", "light.contrast", "light.highlights", "light.shadows", "light.whites", "light.blacks"] {
            control(app, ui, d, c, true);
        }
        if shown_here(app, ui, "option:contrastMode") {
            contrast_mode_row(app, ui, d);
        }
        ui.add_space(6.0);
        sub_title(ui, crate::i18n::tr("Presence"));
        // video takes no local-contrast effects
        crate::video_ui::video_gate(ui, video, |ui| {
            for c in ["effects.texture", "effects.clarity", "effects.dehaze"] {
                control(app, ui, d, c, true);
            }
        });
        control(app, ui, d, "color.vibrance", true);
        control(app, ui, d, "color.saturation", true);
        ui.add_space(6.0);
        if d.hdr.enabled {
            let open = app.ui.flyout_open("sdrRendition");
            if flyout_row(ui, "sdrRendition", "SDR Rendition", Icon::Sliders, open).clicked() {
                app.ui.toggle_flyout("sdrRendition");
            }
            if open {
                // as in Lightroom Classic, the SDR sliders take effect where the screen shows the
                // SDR rendition: while it shows the HDR render they are greyed out (an agent can
                // still set them), with the way to see them
                let sdr_shown = !super::detail::hdr_shown(app);
                if !sdr_shown {
                    egui::Frame::NONE
                        .inner_margin(crate::widgets::row_margin(ui, egui::Margin { left: 12, right: 16, top: 2, bottom: 4 }, 2, 3))
                        .show(ui, |ui| {
                            let t = Tokens::get(ui.ctx());
                            let r = ui.label(egui::RichText::new(crate::i18n::tr(SDR_SLIDERS_NOTE)).font(t.font(12.0)).color(t.text_dim));
                            register(ui.ctx(), "note:sdrSlidersHidden", r.rect);
                        });
                }
                crate::widgets::slider_group(ui, "sdrRendition", |ui| {
                    for c in [
                        "hdr.sdrBrightness",
                        "hdr.sdrContrast",
                        "hdr.sdrHighlights",
                        "hdr.sdrShadows",
                        "hdr.sdrWhites",
                        "hdr.sdrClarity",
                        "hdr.sdrBlend",
                    ] {
                        control(app, ui, d, c, sdr_shown);
                    }
                });
                ui.add_space(6.0);
            }
        }
        ui.add_space(8.0);
    });
    // Witchcraft's palettes follow Basic (the grade continues where every photo starts: Basic,
    // then the colourist's Primaries wheels and HSL curves) before Lightroom's own panels — in
    // Eboda all three are away; the histogram's own mode menu (its scopes included) stays
    // Lightroom's way, and the two Witchcraft sections carry the mode's pill beside their titles
    if crate::tool_profiles::witchcraft_shown(app) {
        if shows("primaries") {
            section_with(app, ui, &d, "primaries", "Primaries", true, Some("Witchcraft"), |app, ui, d| {
                super::witchcraft::primaries(app, ui, d);
                ui.add_space(8.0);
            });
        }
        // Resolve's HSL curves over the colour mixer's bands (colour photos: the B&W mix has its
        // own panel)
        if shows("hsl") && !crate::is_bw(&d) {
            section_with(app, ui, &d, "hsl", "HSL Curves", true, Some("Witchcraft"), |app, ui, d| {
                super::witchcraft::hsl_curves(app, ui, d);
                ui.add_space(8.0);
            });
        }
    }
    // Lightroom Classic's Develop panels, in its order: Basic, Tone Curve, Color Mixer, Color Grading, Detail, Lens
    // Corrections, Transform, Effects, Calibration (the fork's: upstream groups them as the cloud app does)
    section(app, ui, &d, "curve", "Tone Curve", |app, ui, d| {
        curve_editor(app, ui, id, d, CurveTarget::Photo);
        ui.add_space(8.0);
    });
    // Mixer and Point Color, as the panel's two tabs (Point Color is for colour photos)
    section(app, ui, &d, "mixer", "Color Mixer", |app, ui, d| {
        let point = !crate::is_bw(d) && app.ui.flyout_open("pointColor");
        if !crate::is_bw(d) {
            let items = [("Mixer", "mixer"), ("Point Color", "pointColor")];
            let mut chosen = None;
            egui::Frame::NONE.inner_margin(crate::widgets::row_margin(ui, egui::Margin { left: 12, right: 16, top: 2, bottom: 6 }, 2, 3)).show(
                ui,
                |ui| {
                    chosen = crate::widgets::segmented(ui, "colorMixerTab", &items, Some(usize::from(point)), 2);
                },
            );
            if chosen.is_some_and(|i| (i == 1) != point) {
                app.ui.toggle_flyout("pointColor");
            }
        }
        if point {
            crate::video_ui::video_gate(ui, video, |ui| point_color(app, ui, d));
        } else {
            mixer(app, ui, d);
        }
        ui.add_space(8.0);
    });
    section(app, ui, &d, "grading", "Color Grading", |app, ui, d| {
        grading(app, ui, d, id);
        ui.add_space(8.0);
    });
    section(app, ui, &d, "detail", "Detail", |app, ui, d| {
        // Compact: the sharpening sliders under their caption, with short names
        if dense {
            sub_title(ui, crate::i18n::tr("Sharpening"));
        }
        for c in ["detail.sharpenAmount", "detail.sharpenRadius", "detail.sharpenDetail", "detail.sharpenMasking"] {
            control(app, ui, d, c, c == "detail.sharpenAmount" || d.detail.sharpen_amount > 0.0);
        }
        // AI Denoise is its own tool, apart from the manual noise reduction sliders below it
        if shown_here(app, ui, "option:aiDenoise") {
            sub_title(ui, crate::i18n::tr("AI Denoise"));
            // AI Denoise: how much of the photo's cleaned picture to mix in (raw files only), and what that picture is doing
            let applicable = !matches!(app.session.denoise_photo_state(id), lcc_engine::denoise::PhotoState::NotApplicable);
            let on = d.enhance.denoise_enabled();
            super::denoise::detail_toggle(app, ui, id, on, applicable);
            control(app, ui, d, "enhance.denoise", applicable && on);
            super::denoise::detail_status(app, ui, id, if on { d.enhance.denoise } else { 0.0 });
        }
        sub_title(ui, crate::i18n::tr("Noise Reduction"));
        // under "Noise Reduction", as in Lightroom Classic: Luminance, Detail, Contrast, Color, …
        for (c, label) in [
            ("detail.nrLuminance", Some("Luminance")),
            ("detail.nrDetail", None),
            ("detail.nrContrast", None),
            ("detail.nrColor", Some("Color")),
            ("detail.nrColorDetail", None),
            ("detail.nrColorSmoothness", None),
        ] {
            control_as(app, ui, d, c, true, label);
        }
        // single outlier photosites, replaced in the raw mosaic (#1345): raws only
        if app.session.catalog.photo(id).is_some_and(|p| p.develops_raw()) && shown_here(app, ui, "option:hotPixels") {
            egui::Frame::NONE.inner_margin(crate::widgets::row_margin(ui, egui::Margin { left: 12, right: 16, top: 4, bottom: 0 }, 3, 0)).show(
                ui,
                |ui| {
                    let mut on = d.detail.nr_outliers;
                    if super::settings::check(ui, "detail.nrOutliers", &mut on, "Remove Hot Pixels") {
                        let _ = app.run("develop.merge", json!({"settings": {"detail": {"nr_outliers": on}}, "label": "Remove Hot Pixels"}));
                    }
                    // hot, dead and stuck pixels this camera body has shown (its sensor map) and the clear ones in this photo
                    let mut fix = d.sensor.fixes_pixels();
                    if super::settings::check(ui, "sensor.pixelFix", &mut fix, "Fix Hot and Dead Pixels") {
                        let _ = app.run("develop.set", json!({"control": "sensor.pixelFix", "value": if fix { 1.0 } else { 0.0 }}));
                    }
                    if let Some(px) = app.session.source_info(id).pixels.filter(|p| p.on) {
                        let label = px.body.as_deref().map(lcc_engine::sensor_maps::body_label).unwrap_or_default();
                        let text = if label.is_empty() {
                            crate::i18n::tr_format!("{} fixed in this photo", px.found)
                        } else {
                            crate::i18n::tr_format!(
                                "{} fixed: {} known for camera {}, {} found in this photo",
                                px.from_map + px.found,
                                px.from_map,
                                label,
                                px.found
                            )
                        };
                        ui.label(egui::RichText::new(text).size(11.0).color(Tokens::get(ui.ctx()).text_dim));
                    }
                },
            );
        }
        ui.add_space(8.0);
    });
    // portrait retouching (beyond Lightroom), below Detail
    section(app, ui, &d, "retouch", "Retouch", |app, ui, d| super::retouch::panel(app, ui, d, id));
    // the lens database changed since this photo was looked up: look again (its correction and preview follow),
    // also while the Lens Corrections section is folded
    if d.optics.lens_profile && app.session.lens_profiles_stale(id) {
        let _ = app.session.refresh_lens_profiles(&[id]);
    }
    section(app, ui, &d, "optics", "Lens Corrections", |app, ui, d| {
        let has_lens = app.session.catalog.photo(id).is_some_and(|p| p.embedded_lens.is_some());
        egui::Frame::NONE.inner_margin(crate::widgets::row_margin(ui, egui::Margin { left: 12, right: 16, top: 4, bottom: 4 }, 3, 3)).show(
            ui,
            |ui| {
                let mut ca = d.optics.remove_ca;
                if ui.checkbox(&mut ca, crate::i18n::tr("Remove Chromatic Aberration")).changed() {
                    let _ = app.run("develop.merge", json!({"settings": {"optics": {"remove_ca": ca}}, "label": "Remove CA"}));
                }
                let mut lp = d.optics.lens_profile;
                if ui.checkbox(&mut lp, crate::i18n::tr("Enable Lens Corrections")).changed() {
                    let _ = app.run("develop.merge", json!({"settings": {"optics": {"lens_profile": lp}}, "label": "Lens Corrections"}));
                }
                if lp {
                    lens_note(app, ui, id, has_lens);
                    lens_picker(app, ui, id, d);
                }
            },
        );
        if d.optics.lens_profile {
            if dense {
                sub_title(ui, crate::i18n::tr("Profile"));
            }
            for c in ["optics.profileDistortion", "optics.profileVignetting"] {
                control(app, ui, d, c, has_lens);
            }
        }
        // Capture One's LCC "Uniform Light": shown whether or not lens corrections are on
        vignetting_calibration_section(app, ui, id, d);
        // What photos imported from now on start from, and one action that makes these settings it
        // (issue #148: the two checkboxes above reset on every photo, so a batch wants a default).
        // One quiet line under the lens controls (issue #346: it used to split them from the sliders).
        if shown_here(app, ui, "option:newPhotoDefault") {
            egui::Frame::NONE.inner_margin(crate::widgets::row_margin(ui, egui::Margin { left: 12, right: 16, top: 2, bottom: 4 }, 2, 3)).show(
                ui,
                |ui| {
                    let photo_camera = app.session.catalog.photo(id).map(|p| p.meta.camera.trim().to_string()).unwrap_or_default();
                    let current = {
                        let defaults = &app.session.import_defaults;
                        if defaults.per_camera && !photo_camera.is_empty() {
                            defaults.cameras.iter().find(|c| c.camera.eq_ignore_ascii_case(&photo_camera)).and_then(|c| c.preset.clone())
                        } else {
                            defaults.raw_preset.clone()
                        }
                    };
                    // the preset the default points at: its name for the row, and — when it is one of the
                    // user's own — the name the naming dialog starts from (a built-in's would make a twin)
                    let (starts_from, suggested) = match current.as_deref().and_then(|pid| app.session.presets.iter().find(|p| p.id == pid)) {
                        Some(p) => {
                            (crate::i18n::builtin_label(&p.name, p.builtin).to_string(), if p.builtin { String::new() } else { p.name.clone() })
                        }
                        None => (crate::i18n::tr("{app} Default").to_string(), String::new()),
                    };
                    // which photos it reaches, said on hover rather than as a line of its own
                    let reach = crate::i18n::tr("Photos imported from now on; photos already in the catalog keep theirs.");
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(crate::i18n::tr("New photos start from")).small().weak()).on_hover_text(reach);
                        ui.label(RichText::new(starts_from).small()).on_hover_text(reach);
                        // "Save as…": the name is the user's, so several named optics defaults can live side
                        // by side (one per camera, say) instead of one fixed entry
                        if text_button(ui, "opticsNewPhotoDefault", "Save as…", false).on_hover_text(reach).clicked() {
                            app.ui.dialog = Some(crate::state::Dialog::NewPhotoDefault { name: suggested });
                        }
                    });
                },
            );
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
            crate::widgets::slider_group(ui, "defringe", |ui| {
                sub_title(ui, crate::i18n::tr("Purple"));
                control_as(app, ui, d, "optics.defringePurple", true, Some("Amount"));
                let on = d.optics.defringe_purple_amount > 0.0;
                control_as(app, ui, d, "optics.defringePurpleHueLo", on, Some("Hue Low"));
                control_as(app, ui, d, "optics.defringePurpleHueHi", on, Some("Hue High"));
                sub_title(ui, crate::i18n::tr("Green"));
                control_as(app, ui, d, "optics.defringeGreen", true, Some("Amount"));
                let on = d.optics.defringe_green_amount > 0.0;
                control_as(app, ui, d, "optics.defringeGreenHueLo", on, Some("Hue Low"));
                control_as(app, ui, d, "optics.defringeGreenHueHi", on, Some("Hue High"));
                sub_title(ui, crate::i18n::tr("Lateral Chromatic Aberration"));
                control_as(app, ui, d, "optics.caRed", true, Some("Red/Cyan"));
                control_as(app, ui, d, "optics.caBlue", true, Some("Blue/Yellow"));
            });
        }
        ui.add_space(8.0);
    });
    // Lightroom Classic's Transform panel (the fork's: upstream keeps it in the Crop tool), after the lens corrections
    section(app, ui, &d, "geometry", "Transform", |app, ui, _| super::right::transform_body(app, ui, id));
    section(app, ui, &d, "effects", "Effects", |app, ui, d| {
        sub_title(ui, crate::i18n::tr("Vignette"));
        if shown_here(app, ui, "option:vignetteStyle") {
            use lcc_develop::VignetteStyle as V;
            let styles = [
                (V::HighlightPriority, "Highlight", "highlightPriority"),
                (V::ColorPriority, "Color", "colorPriority"),
                (V::PaintOverlay, "Paint", "paintOverlay"),
            ];
            let items: Vec<(&str, &str)> = styles.iter().map(|(_, l, k)| (*l, *k)).collect();
            let active = styles.iter().position(|(v, _, _)| *v == d.vignette.style);
            let mut chosen = None;
            egui::Frame::NONE.inner_margin(crate::widgets::row_margin(ui, egui::Margin { left: 12, right: 16, top: 2, bottom: 4 }, 2, 3)).show(
                ui,
                |ui| {
                    chosen = crate::widgets::segmented(ui, "vignetteStyle", &items, active, 3);
                },
            );
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
        // process V7's particle grain has its own controls (OneSlips); earlier processes render the
        // value-noise grain, which only reads Amount, Size and Roughness
        let particles = d.process.process() >= lcc_develop::Process::V7;
        let grain: &[&str] = if particles {
            &["grain.amount", "grain.size", "grain.sizeVariation", "grain.roughness", "grain.clumping", "grain.color", "grain.response", "grain.seed"]
        } else {
            &["grain.amount", "grain.size", "grain.roughness"]
        };
        for c in grain {
            control(app, ui, d, c, *c == "grain.amount" || d.grain.amount != 0.0);
        }
        ui.add_space(8.0);
    });
    // Lightroom Classic's Calibration panel (the cloud app hides it): last, like there.
    section(app, ui, &d, "calibration", "Calibration", |app, ui, d| {
        sub_title(ui, crate::i18n::tr("Shadows"));
        control_as(app, ui, d, "calibration.shadowsTint", true, Some("Tint"));
        for (title, k) in [("Red Primary", "red"), ("Green Primary", "green"), ("Blue Primary", "blue")] {
            sub_title(ui, title);
            control_as(app, ui, d, &format!("calibration.{k}Hue"), true, Some("Hue"));
            control_as(app, ui, d, &format!("calibration.{k}Sat"), true, Some("Saturation"));
        }
        ui.add_space(8.0);
    });
    if shows("plugins") {
        super::plugin_filters::show(app, ui, &d);
    }
    ui.add_space(40.0);
    ui.ctx().data_mut(|m| m.remove::<bool>(egui::Id::new(TOOL_FILTER)));
}

/// The tool profile row at the head of the Edit panel: the profile in use, a menu of the others, and the gear to
/// Settings ▸ Develop where they are made (`view.toolProfile`). A low row, below a slider's height.
fn tool_profile_row(app: &mut LccApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let m = crate::widgets::row_margin;
    let name = app.ui.settings.tool_profiles.active().label();
    egui::Frame::NONE.inner_margin(m(ui, egui::Margin { left: 12, right: 12, top: 4, bottom: 4 }, 2, 2)).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.set_height(crate::widgets::DENSE_ROW_H - 4.0);
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.label(egui::RichText::new(crate::i18n::tr("Tools:")).font(t.font(11.0)).color(t.text_dim));
            let gear = 20.0;
            let max = ui.available_width() - gear - ui.spacing().item_spacing.x;
            let r = crate::widgets::dropdown_within(ui, "toolProfile", &name, t.font(12.0), t.text_label, max)
                .on_hover_text(crate::i18n::tr("Tool profile: which groups and sliders the panel shows"));
            egui::Popup::menu(&r).show(|ui| {
                ui.set_min_width(160.0);
                let active = app.ui.settings.tool_profiles.active().id.clone();
                let all: Vec<(String, String)> = app.ui.settings.tool_profiles.all().map(|p| (p.id.clone(), p.label())).collect();
                let mut pick = None;
                for (pid, label) in &all {
                    let r = ui.selectable_label(*pid == active, label);
                    register(ui.ctx(), format!("toolProfileMenu:{pid}"), r.rect);
                    if r.clicked() {
                        pick = Some(pid.clone());
                    }
                }
                ui.separator();
                let r = ui.button(crate::i18n::tr("Edit Tool Profiles…"));
                register(ui.ctx(), "toolProfileMenu:settings", r.rect);
                if r.clicked() {
                    let _ = app.run("app.settings", json!({"tab": "develop"}));
                }
                if let Some(pid) = pick
                    && let Err(e) = app.run("view.toolProfile", json!({"id": pid}))
                {
                    app.toast(ui.ctx(), e);
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if crate::widgets::icon_button(
                    ui,
                    "toolProfileSettings",
                    Icon::Gear,
                    vec2(gear, gear),
                    false,
                    true,
                    crate::i18n::tr("Edit Tool Profiles…"),
                )
                .clicked()
                {
                    let _ = app.run("app.settings", json!({"tab": "develop"}));
                }
            });
        });
    });
    divider(ui);
}

/// One slider of the tool profile's Custom group: drawn whatever its own group hides, enabled, and with the white
/// balance on the scale the photo's sliders use (Kelvin for a raw, relative for a rendered file).
fn custom_control(app: &mut LccApp, ui: &mut egui::Ui, d: &DevelopSettings, id: &str, raw: bool) {
    match id {
        "wb.temp" if !raw => {
            let out = slider(ui, &REL_TEMP, k_to_rel(d.wb.temp), true, None);
            apply_slider_out(app, &REL_TEMP, out, |app, v| app.run("develop.set", json!({"control": "wb.temp", "value": rel_to_k(v)})));
        }
        "wb.tint" if !raw => {
            let out = slider(ui, &REL_TINT, d.wb.tint.clamp(-100.0, 100.0), true, None);
            apply_slider_out(app, &REL_TINT, out, |app, v| app.run("develop.set", json!({"control": "wb.tint", "value": v})));
        }
        _ => draw_control(app, ui, d, id, true, None),
    }
}

/// The rows at the top of Basic: Lightroom Classic's Auto · B&W | HDR buttons, the notices, Auto Sync, the profile's
/// Amount and the file's colour options. `browse`: with Browse Profiles at the buttons' left (the Modern layout's
/// Profile & Treatment card; elsewhere it is in the tool row above the panels).
fn header_rows(app: &mut LccApp, ui: &mut egui::Ui, id: PhotoId, d: &DevelopSettings, preview_only: Option<&str>, browse: bool) {
    // Lightroom Classic's Basic button row: Auto, B&W | HDR (issue #124), right-aligned on one row of the panel's row
    // height; no title (the tool row says which panel this is). Reset sits in the bar under the panels (issue #116).
    if browse || shown_here(app, ui, "option:treatment") {
        treatment_row(app, ui, id, d, browse);
    }
    header_notices(app, ui, id, d, preview_only);
}

/// [`header_rows`]' Auto · B&W | HDR row.
fn treatment_row(app: &mut LccApp, ui: &mut egui::Ui, id: PhotoId, d: &DevelopSettings, browse: bool) {
    let t = Tokens::get(ui.ctx());
    let dense = t.dense;
    let m = crate::widgets::row_margin;
    egui::Frame::NONE.inner_margin(m(ui, egui::Margin { left: 12, right: 16, top: 6, bottom: 6 }, 3, 3)).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.set_height(crate::widgets::DENSE_ROW_H - 2.0);
            if browse {
                let size = if dense { 20.0 } else { 24.0 };
                if crate::widgets::icon_button(
                    ui,
                    "profileBrowser",
                    Icon::ProfileGrid,
                    vec2(size, size),
                    false,
                    true,
                    crate::i18n::tr("Browse Profiles"),
                )
                .clicked()
                {
                    let _ = app.run("panel.profiles", json!({}));
                }
            }
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = if dense { 4.0 } else { 8.0 };
                if text_button(ui, "hdr", crate::i18n::tr("HDR"), d.hdr.enabled).on_hover_text(hdr_button_tip(app.hdr_display_sdr)).clicked() {
                    let _ = app.run("develop.hdr", json!({"enabled": !d.hdr.enabled}));
                }
                // the separator between the treatment and HDR, as in Classic
                let (sep, _) = ui.allocate_exact_size(vec2(1.0, if dense { 14.0 } else { 18.0 }), Sense::hover());
                ui.painter().vline(sep.center().x, sep.y_range(), egui::Stroke::new(1.0, t.button_border));
                let bw = crate::is_bw(d);
                if text_button(ui, "treatment-bw", crate::i18n::tr("B&W"), bw).on_hover_text(crate::i18n::tr("Black & White")).clicked() {
                    let _ = app.run("develop.treatment", json!({"bw": !bw}));
                }
                let applied = app.session.auto_is_current(id);
                let r = ui.add_enabled_ui(!applied, |ui| text_button(ui, "auto", crate::i18n::tr("Auto"), false)).inner;
                if r.clicked() {
                    let _ = app.run("develop.auto", json!({}));
                    app.toast(ui.ctx(), crate::i18n::tr("Auto settings applied"));
                }
            });
        });
    });
}

/// [`header_rows`]' notices and the profile's options under the treatment row.
fn header_notices(app: &mut LccApp, ui: &mut egui::Ui, id: PhotoId, d: &DevelopSettings, preview_only: Option<&str>) {
    let t = Tokens::get(ui.ctx());
    let dense = t.dense;
    let m = crate::widgets::row_margin;
    if let Some(why) = &preview_only {
        let reload = egui::Frame::NONE
            .inner_margin(m(ui, egui::Margin { left: 12, right: 16, top: 0, bottom: 12 }, 0, 4))
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
        egui::Frame::NONE.inner_margin(m(ui, egui::Margin { left: 12, right: 16, top: 0, bottom: 10 }, 0, 4)).show(ui, |ui| {
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
    if d.profile.id != "lc.color" {
        control(app, ui, d, "profile.amount", true);
        ui.add_space(if dense { 2.0 } else { 6.0 });
    }
    // whether to offer the two file options below: facts of the file that stay put while sources are decoded again
    // (an edit, crop mode), so the rows under them never move
    let facts = app.session.media.profile_facts(id);
    // a raw carrying its own local tone mapping (Apple ProRAW's gain table map): off renders it as
    // Lightroom Classic does, on as the camera does
    if facts.local_tone {
        egui::Frame::NONE.inner_margin(m(ui, egui::Margin { left: 12, right: 16, top: 0, bottom: 10 }, 0, 3)).show(ui, |ui| {
            let mut on = d.profile.camera_local_tone;
            let r = ui.checkbox(&mut on, crate::i18n::tr("Camera local tone mapping"));
            crate::widgets::register(ui.ctx(), "check:profile.cameraLocalTone", r.rect);
            if r.on_hover_text(crate::i18n::tr("Apply the local tone mapping stored in the raw (Apple ProRAW), as the camera renders it")).changed() {
                let _ = app.run("develop.merge", json!({"settings": {"profile": {"camera_local_tone": on}}, "label": "Camera Local Tone Mapping"}));
            }
        });
    }
    // a raw without colour matrices whose camera model has measured ones: off starts from the look fitted to the
    // camera's JPEG (Auto), on from the measured colour with the app's own tone curve (Colorimetric)
    if facts.colorimetric {
        egui::Frame::NONE.inner_margin(m(ui, egui::Margin { left: 12, right: 16, top: 0, bottom: 10 }, 0, 3)).show(ui, |ui| {
            let mut on = d.profile.camera_colour == lcc_develop::CameraColour::Colorimetric;
            let r = ui.checkbox(&mut on, crate::i18n::tr("Calibrated camera colour"));
            crate::widgets::register(ui.ctx(), "check:profile.cameraColour", r.rect);
            let tip = "Start from the colour measured for this camera model instead of the look fitted to the camera's JPEG";
            if r.on_hover_text(crate::i18n::tr(tip)).changed() {
                let mode = if on { "colorimetric" } else { "auto" };
                let _ = app.run("develop.merge", json!({"settings": {"profile": {"camera_colour": mode}}, "label": "Calibrated Camera Colour"}));
            }
        });
    }
}

/// The profile row's name and the line under it (OneSlips, LR-PROF-LAYERS): a photo that develops with its camera
/// profile's own look ("As profile") shows the camera profile's name and no line; one with a look of the app's on a
/// camera profile shows the look and "on <camera profile>".
pub(crate) fn profile_row_names(app: &LccApp, id: PhotoId, d: &DevelopSettings) -> (String, Option<String>) {
    let look = app.session.profile_info(&d.profile.id).map(|(name, _)| name).unwrap_or("Color");
    let look = crate::i18n::profile_label(&d.profile.id, look).to_string();
    let Some(photo) = app.session.catalog.photo(id) else { return (look, None) };
    let Some(camera) = lcc_engine::camera_profiles::photo_profile(photo) else { return (look, None) };
    let camera_name = if camera.name.trim().is_empty() { camera.model.clone() } else { camera.name.clone() };
    if d.keeps_camera_look() && camera.has_look() {
        return (camera_name, None);
    }
    (look, Some(crate::i18n::tr_format!("on {calibration}", calibration = camera_name).to_string()))
}

/// [`camera_profile_options`] as the menus offer them: without the ones the Profile Manager hides (`hidden`), except
/// the photo's own (`current`), which stays so it can be seen and changed.
pub(crate) fn shown_camera_profile_options(hidden: &[String], camera: &str, current: &str) -> Vec<(String, String)> {
    let mut options = camera_profile_options(camera);
    options.retain(|(o, _)| o == current || !hidden.iter().any(|h| h == o));
    options
}

/// The camera profiles a raw of `camera` can develop with (`profile.camera` choices): the camera's default, the
/// app's own colour, the imported `.dcp` files for its camera and its calibrated looks.
pub(crate) fn camera_profile_options(camera: &str) -> Vec<(String, String)> {
    let default_name = match lcc_raw::dcp::resolve_camera("", camera) {
        Some(p) => format!("{} ({})", crate::i18n::tr("Camera default"), p.name),
        None => crate::i18n::tr("Camera default (built-in)").to_string(),
    };
    let mut options: Vec<(String, String)> = vec![(String::new(), default_name), ("lc".into(), crate::i18n::tr("Built-in colour").to_string())];
    for (pid, p) in lcc_raw::dcp::library() {
        if lcc_raw::dcp::covers_model(&p.model, camera) {
            let name = if p.name.is_empty() { pid.trim_start_matches("dcp:").to_string() } else { p.name.clone() };
            options.push((pid, name));
        }
    }
    if let Some(cal) = lcc_raw::dcp::calibrated_for_camera(camera) {
        options.extend(cal.looks.iter().map(|(pid, p)| (pid.clone(), p.name.clone())));
    }
    options
}

/// Basic ▸ Tone's title row with Classic's Auto button at its right, greyed out while the tone sliders
/// still hold what Auto set (any change to them makes it available again).
/// A heading inside a section ("Tone", "AI Denoise"): between the section title and the slider
/// names in size (issue #346).
pub fn sub_title(ui: &mut egui::Ui, title: &str) {
    let t = Tokens::get(ui.ctx());
    if t.dense {
        // Compact: a quiet caption (dim 11 pt) on the rows' left margin, a hairline after it to the
        // right margin, so a group reads as one without a loud label (LAYOUT-DENSE, ported from OneSlips)
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), crate::widgets::DENSE_ROW_H - 2.0), Sense::hover());
        let left = r.left() + crate::widgets::PAD_L;
        let right = r.right() - crate::widgets::PAD_R;
        let mut job = egui::text::LayoutJob::simple_singleline(crate::i18n::tr(title).to_string(), t.semibold(11.0), t.text_dim);
        job.wrap =
            egui::text::TextWrapping { max_width: (right - left).max(1.0), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
        let g = ui.painter().layout_job(job);
        let y = r.center().y + 2.0;
        let text_right = left + g.size().x;
        ui.painter().galley(pos2(left, y - g.size().y / 2.0), g, t.text_dim);
        if right > text_right + 12.0 {
            ui.painter().hline((text_right + 6.0)..=right, y.round() + 0.5, Stroke::new(1.0, t.text_dim.gamma_multiply(0.3)));
        }
        return;
    }
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
    ui.painter().text(pos2(r.left() + 12.0, r.center().y + 2.0), Align2::LEFT_CENTER, crate::i18n::tr(title), t.semibold(11.75), t.text_label);
}

pub(crate) fn section(
    app: &mut LccApp,
    ui: &mut egui::Ui,
    d: &DevelopSettings,
    id: &str,
    title: &str,
    body: impl FnOnce(&mut LccApp, &mut egui::Ui, &DevelopSettings),
) {
    section_with(app, ui, d, id, title, true, None, body);
}

/// [`section`], with its on/off switch (`switch`) or without, and optionally a small pill
/// (`badge`, e.g. "Witchcraft") beside the title saying which mode the section belongs to.
pub(crate) fn section_with(
    app: &mut LccApp,
    ui: &mut egui::Ui,
    d: &DevelopSettings,
    id: &str,
    title: &str,
    switch: bool,
    badge: Option<&str>,
    body: impl FnOnce(&mut LccApp, &mut egui::Ui, &DevelopSettings),
) {
    // one section alone (the Modern layout's card): open, no header
    if let Some(only) = only_section(ui) {
        if only == id {
            let blocked = !crate::video_ui::section_allowed(id) && app.session.active().is_some_and(|p| crate::video_ui::photo_is_video(app, p));
            crate::video_ui::video_gate(ui, blocked, |ui| body(app, ui, d));
        }
        return;
    }
    // the tool profile in use switched the group off
    if !shown_here_group(app, ui, id) {
        return;
    }
    let open = app.ui.section_open(id);
    let (resp, toggled) = section_header(ui, id, title, open, switch.then(|| d.section_enabled(id)), badge);
    // video takes the global Light and Color controls only: the rest show greyed out
    let blocked = !crate::video_ui::section_allowed(id) && app.session.active().is_some_and(|p| crate::video_ui::photo_is_video(app, p));
    let resp = if blocked { resp.on_hover_text(crate::i18n::tr(crate::video_ui::NOT_FOR_VIDEO)) } else { resp };
    if let Some(on) = toggled.filter(|_| !blocked) {
        let _ = app.run("develop.sectionEnabled", json!({"section": id, "enabled": on}));
    } else if resp.clicked() {
        // ⌥-click opens just this section, as in Lightroom Classic
        let solo = ui.input(|i| i.modifiers.alt);
        if let Err(e) = app.run(&format!("section.{id}"), json!({"solo": solo})) {
            app.toast(ui.ctx(), e);
        }
    }
    crate::widgets::context_menu(&resp, |ui| {
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
        crate::video_ui::video_gate(ui, blocked, |ui| body(app, ui, d));
    }
    divider(ui);
}

// ------------------------------------------------------------------------------ histogram

/// One of the histogram's clipping indicators: a small triangle in the plot's top corner pointing
/// out of it (left for the shadows, right for the highlights). Filled with the colour of the
/// channels that clip — or while the per-pixel overlay is on — and hollow while none of them do.
fn paint_clip_triangle(p: &egui::Painter, rect: Rect, out: f32, color: Color32, lit: bool) {
    let c = rect.center();
    let (w, h) = (7.0, 8.0);
    let pts = if out < 0.0 {
        vec![pos2(c.x - w, c.y), pos2(c.x + w * 0.6, c.y - h), pos2(c.x + w * 0.6, c.y + h)]
    } else {
        vec![pos2(c.x + w, c.y), pos2(c.x - w * 0.6, c.y - h), pos2(c.x - w * 0.6, c.y + h)]
    };
    if lit {
        p.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
    } else {
        p.add(egui::Shape::closed_line(pts, Stroke::new(1.2, color)));
    }
}

/// The current photo's existing preview histogram, shared by Histogram and Tone Curve.
pub(crate) fn preview_histogram(app: &LccApp, id: PhotoId) -> Option<lcc_raster::Histogram> {
    app.renderer
        .textures
        .get(&Slot::Main)
        .filter(|t| t.photo == id)
        .and_then(|t| t.histogram.clone())
        .or_else(|| app.renderer.textures.get(&Slot::Thumb(id)).and_then(|t| t.histogram.clone()))
}

/// Long edge of the Histogram panel's own render of its photo ([`Slot::PanelHistogram`]): enough
/// for the waveform's 128 columns, small enough to be quick.
const PANEL_HISTOGRAM_EDGE: usize = 512;

/// `hist`, or — when it can't draw `mode` (none at all, or no waveform for the Waveform / Parade)
/// — the panel's own small render of the photo as it is now, asked for when missing. In the Library
/// grid the selected photo has only its thumbnail's histogram, which keeps no waveform, so the
/// scopes stayed empty there except for the photo last opened in the loupe.
pub(crate) fn panel_render_histogram(app: &mut LccApp, id: PhotoId, mode: lcc_raster::HistogramMode, hist: Option<Histogram>) -> Option<Histogram> {
    if hist.as_ref().is_some_and(|h| !mode.is_waveform() || h.has_waveform()) {
        return hist;
    }
    let edge = PANEL_HISTOGRAM_EDGE;
    let Some(job) = app.session.render_job(id, edge, edge, false, app.ui.right != RightPanel::Crop) else { return hist };
    // (the key the result comes back with)
    let job = app.renderer.job_for(Slot::PanelHistogram, job);
    match &app.renderer.panel_histogram {
        Some((photo, key, h)) if *photo == id && *key == job.key => Some(h.clone()),
        kept => {
            // until it lands, the last one of this photo (an edit moved on), else what there is
            let stale = kept.as_ref().filter(|(photo, _, _)| *photo == id).map(|(_, _, h)| h.clone());
            app.renderer.request(Slot::PanelHistogram, job, 40);
            stale.or(hist)
        }
    }
}

/// An HDR edit's histogram stays an HDR histogram: some renders carry only their 8-bit image's
/// (a drag's drafts, a cached thumbnail or stand-in), and showing those would swap the panel to
/// the SDR layout and back while a slider moves. Every render with HDR bins replaces the one kept for the photo, so
/// the histogram still follows the edit live; an SDR edit shows what it gets.
fn steady_hdr_histogram(ctx: &egui::Context, id: PhotoId, hdr_edit: bool, hist: Option<Histogram>) -> Option<Histogram> {
    let key = egui::Id::new(("hdrHistogram", id));
    match hist {
        Some(h) if hdr_edit && h.hdr.is_some() => {
            ctx.data_mut(|d| d.insert_temp(key, h.clone()));
            Some(h)
        }
        h if hdr_edit => ctx.data(|d| d.get_temp::<Histogram>(key)).or(h),
        // an SDR edit never shows an HDR histogram, even one a stale render still carries
        Some(h) if h.hdr.is_some() => Some(Histogram { hdr: None, ..h }),
        h => h,
    }
}

/// How long (s) after a drag its live histogram is kept when the loupe has no render on its way
/// (the release frame, before the loupe asks for its render).
const HISTOGRAM_HOLD_S: f64 = 0.25;

/// The histogram both of the panel's plots draw: a drag's live HDR histogram, else the loupe render
/// of this photo, else its thumbnail (grid view before the loupe has rendered it). On an HDR display
/// a drag of an HDR edit sends the loupe only float drafts, so its 8-bit render (and that histogram)
/// stays as it was until the release.
fn current_histogram(app: &LccApp, ctx: &egui::Context, id: PhotoId, hdr_edit: bool) -> Option<Histogram> {
    // (only while the edit is HDR: one left from a drag before HDR was turned off would bring the
    // HDR layout back for each later drag, #277)
    let interacting = app.session.interaction.is_some();
    let loupe_live = interacting && !super::detail::adjust_drafts(app) && !super::detail::hdr_shown(app);
    let live = app.renderer.live_hdr_histogram.as_ref().filter(|l| interacting && hdr_edit && !loupe_live && l.0 == id).map(|l| l.2.clone());
    // After such a drag its last live histogram stays until the loupe's render of the released edit
    // lands: until then the loupe render there is (on an HDR display) the one from before the drag,
    // and showing it would make the plots jump back for a moment on release.
    let hold = egui::Id::new(("histogramHold", id));
    let now = ctx.input(|i| i.time);
    let main_key = app.renderer.textures.get(&Slot::Main).filter(|t| t.photo == id).map(|t| t.key);
    let live = match live {
        Some(h) => {
            ctx.data_mut(|d| d.insert_temp(hold, (main_key, h.clone(), now)));
            Some(h)
        }
        None if !interacting && hdr_edit => {
            let held = ctx.data(|d| d.get_temp::<(Option<u64>, Histogram, f64)>(hold));
            // (released once the loupe's texture is replaced, or once nothing renders it: a release
            // that changed nothing renders nothing)
            let pending = app.renderer.is_pending(Slot::Main);
            match held.filter(|(key, _, at)| *key == main_key && (pending || now - *at < HISTOGRAM_HOLD_S)) {
                Some((_, h, _)) => Some(h),
                None => {
                    ctx.data_mut(|d| d.remove::<(Option<u64>, Histogram, f64)>(hold));
                    None
                }
            }
        }
        None => None,
    };
    live.or_else(|| preview_histogram(app, id))
}

/// What a histogram of photo `id` drawn in `mode` shows this frame (the panel's first plot, the
/// Modern layout's floating one), asking for the renders it needs.
pub(crate) fn shown_histogram(app: &mut LccApp, ctx: &egui::Context, id: PhotoId, mode: HistogramMode) -> Option<Histogram> {
    let hdr_edit = app.session.develop_of(id).is_some_and(|d| d.hdr.enabled);
    // a drag of an HDR edit: its drafts have no HDR histogram, so a small render beside them
    // gives one (at its own pace: the drafts don't wait for it)
    if app.session.interaction.is_some() && hdr_edit && (super::detail::adjust_drafts(app) || super::detail::hdr_shown(app)) {
        let edge = lcc_pipeline::HDR_HISTOGRAM_DRAG_EDGE;
        // (draft grade, like the drafts it stands beside: a full-quality pass would compete with them)
        if let Some(job) = app.session.render_job(id, edge, edge, false, app.ui.right != RightPanel::Crop).map(|j| j.draft())
            && app.renderer.live_hdr_histogram.as_ref().is_none_or(|l| l.1 != job.key)
        {
            app.renderer.request(Slot::HdrHistogram, job, 95);
        }
    }
    let hist = current_histogram(app, ctx, id, hdr_edit);
    let hist = panel_render_histogram(app, id, mode, hist);
    steady_hdr_histogram(ctx, id, hdr_edit, hist)
}

pub(crate) fn histogram(app: &mut LccApp, ui: &mut egui::Ui, id: PhotoId) {
    let t = Tokens::get(ui.ctx());
    let w = ui.available_width();
    let hdr_edit = app.session.develop_of(id).is_some_and(|d| d.hdr.enabled);
    let mode = super::histogram::mode_of(app);
    let hist = shown_histogram(app, ui.ctx(), id, mode);
    let scale = super::histogram::scale_of(app);
    // an HDR edit's 1D histogram (`pipeline::hdr_histogram`) gets a colour bar and an SDR / HDR
    // label row under the plot; a 2D scope gets a taller plot instead so its disc has room
    let hdr = hist.as_ref().and_then(|h| h.hdr).filter(|_| !mode.is_scope());
    let legend = if hdr.is_some() { 26.0 } else { 0.0 };
    // Compact: under the EXIF line a row of its own, the Original Photo box at its left and the
    // mode selector at its right
    let dense = t.dense;
    let options = if dense { crate::widgets::DENSE_ROW_H } else { 0.0 };
    let height = histogram_height(mode) + legend + options;
    let (r, resp) = ui.allocate_exact_size(vec2(w, height), Sense::click_and_drag());
    crate::widgets::announce(&resp, crate::i18n::tr("Histogram"));
    register(ui.ctx(), "histogram", r);
    let p = ui.painter();
    p.rect_filled(r, 0.0, t.button);
    let plot = Rect::from_min_max(r.min + vec2(0.0, 6.0), pos2(r.right(), r.bottom() - 26.0 - legend - options));
    register(ui.ctx(), "histogram:plot", plot);
    // the plot's near-black well, under the zone band and the distribution
    p.rect_filled(plot, 0.0, super::histogram::SCOPE_BG);
    // Lightroom Classic's tonal zones (Blacks / Shadows / Exposure / Highlights / Whites across the
    // plot): a drag over one moves its control (the band and the readout show which and how far), a
    // double-click puts it back to its default, and Shift drags a tenth as far
    let zone = super::histogram::interact(app, ui, &resp, plot, id);
    if let Some(a) = &zone {
        super::histogram::paint_zone(p, plot, a, &t);
    }
    if let Some(b) = hdr {
        // white for SDR, then one band per stop above SDR white: white too, or (Visualize HDR
        // range on) in its colours
        let x0 = plot.left() + plot.width() * b.from as f32 / 255.0;
        let bar = |a: f32, z: f32, c: Color32| p.rect_filled(Rect::from_min_max(pos2(a, plot.bottom() + 2.0), pos2(z, plot.bottom() + 7.0)), 0.0, c);
        let white = Color32::from_rgb(228, 228, 230);
        bar(plot.left(), x0 - 1.0, white);
        let stops = b.stops.round().max(1.0) as usize;
        let bands = lcc_pipeline::visualize::HDR_BANDS;
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
    let clipping = hist.as_ref().map(|h| h.clipping_channels());
    // HDR: SDR on the left half, a solid line at SDR white and dashed ones between the stops
    if let Some(b) = hdr {
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
    if let Some(h) = &hist {
        if dense {
            super::histogram::paint_plot_compact(p, plot, h, mode, scale);
        } else {
            super::histogram::paint_plot(p, plot, h, mode, scale);
        }
    }
    let mut clicked = resp.clicked();
    // the panel's floating scroll bar widens over the right edge when hovered: both buttons keep
    // clear of it, the left one too so they stay symmetric (#1640)
    let [shadow_rect, highlight_rect] = super::histogram::clipping_rects(ui, plot);
    // the clipping mask the pointer is resting on, previewed on the photo this frame
    let mut preview = false;
    for (name, rect, out, label, clipped) in [
        ("shadows", shadow_rect, -1.0, "Shadow clipping", clipping.map(|c| c.shadow_channels())),
        ("highlights", highlight_rect, 1.0, "Highlight clipping", clipping.map(|c| c.highlight_channels())),
    ] {
        let response = ui.interact(rect, ui.id().with(("histogram-clipping", name)), Sense::click());
        register(ui.ctx(), format!("histogram:{name}"), rect);
        response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, ui.is_enabled(), app.ui.show_clipping, crate::i18n::tr(label)));
        clicked |= response.clicked();
        preview |= response.hovered();
        let active = app.ui.show_clipping;
        // which channels reach this end of the histogram: the triangle takes their blend (white for
        // all three, yellow / cyan / magenta for a pair, a channel's own colour for one), and stays
        // hollow while none of them clip
        let parts = clipped.unwrap_or(Clipped::NONE);
        let lit = parts.any() || active;
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
        paint_clip_triangle(p, rect, out, if lit { super::histogram::blend_color(parts.parts()) } else { t.text_dim }, lit);
        let status = match clipped {
            Some(c) if c.any() => "Clipping detected in this preview.",
            Some(_) => "No clipping detected in this preview.",
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
    // the plot's own controls, in the strip under it: which distribution it draws and how tall its
    // bins are, the same values `view.histogram.mode` / `view.histogram.scale` select
    let menu_rect = if dense {
        Rect::from_min_size(pos2(r.right() - crate::widgets::PAD_R - 72.0, r.bottom() - options + 2.0), vec2(72.0, options - 4.0))
    } else {
        Rect::from_min_size(pos2(r.left() + 4.0, r.bottom() - 22.0), vec2(72.0, 18.0))
    };
    let menu = ui.interact(menu_rect, ui.id().with("histogram-display"), Sense::click());
    menu.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, ui.is_enabled(), crate::i18n::tr("Histogram display")));
    register(ui.ctx(), "histogram:display", menu_rect);
    let col = if menu.hovered() { t.text } else { t.text_label };
    let menu_clicked = menu.clicked();
    p.text(pos2(menu_rect.left() + 3.0, menu_rect.center().y), Align2::LEFT_CENTER, crate::i18n::tr(mode.short()), t.font(11.5), col);
    paint(p, Rect::from_center_size(pos2(menu_rect.right() - 8.0, menu_rect.center().y), vec2(12.0, 12.0)), Icon::ChevronDown, col);
    menu.clone().on_hover_text(format!(
        "{}\n{}\n{}",
        crate::i18n::tr("Histogram display"),
        crate::i18n::tr(mode.label()),
        crate::i18n::tr(scale.label())
    ));
    egui::Popup::menu(&menu).show(|ui| histogram_display_menu(app, ui, mode, scale));
    // a press on the plot's own controls is theirs: only the plot itself toggles the clipping
    // warnings, as it always has
    if clicked && !menu_clicked {
        let _ = app.run("view.clipping", json!({}));
    }
    // (only once the pointer rests on the indicator: gliding across the histogram passes over the
    // triangles, and each preview toggled the overlay, so the photo rendered again, every tile at
    // 100 %, twice per pass)
    let (now, pass) = (ui.input(|i| i.time), ui.ctx().cumulative_pass_nr());
    app.clipping_hover = if preview {
        let since = app.clipping_hover.filter(|(_, seen)| seen.saturating_add(1) >= pass).map_or(now, |(since, _)| since);
        Some((since, pass))
    } else {
        None
    };
    app.clipping_preview = app.clipping_hover.is_some_and(|(since, _)| clipping_preview_due(since, now));
    if let Some((since, _)) = app.clipping_hover.filter(|_| !app.clipping_preview) {
        let left = (since + CLIPPING_PREVIEW_REST_S - now).clamp(0.0, CLIPPING_PREVIEW_REST_S);
        ui.ctx().request_repaint_after(std::time::Duration::try_from_secs_f64(left).unwrap_or_default());
    }
    // the histogram's context menu: which distribution it draws and how tall its bins are, then the
    // format of the colour readout (Lightroom's "Show Lab Color Values")
    crate::widgets::context_menu(&resp, |ui| {
        histogram_display_menu(app, ui, mode, scale);
        ui.separator();
        ui.menu_button(crate::i18n::tr("Second Histogram"), |ui| second_histogram_menu(app, ui));
        ui.separator();
        for f in crate::color_readout::ColorReadout::ALL {
            let label = match f {
                crate::color_readout::ColorReadout::Rgb => crate::i18n::tr("Show RGB Color Values"),
                crate::color_readout::ColorReadout::Hsl => crate::i18n::tr("Show HSL Color Values"),
                crate::color_readout::ColorReadout::Lab => crate::i18n::tr("Show Lab Color Values"),
            };
            if ui.radio(app.ui.settings.color_readout == f, label).clicked() {
                let _ = app.run(f.command(), json!({}));
                ui.close();
            }
        }
    });
    // the colour under the pointer in the loupe, and the tonal zone the pointer is on, replace the
    // EXIF strip while either is there (the histogram's own readout comes first: it is what the
    // pointer is on). All three are centred in the room the mode selector leaves, so the two never
    // run into each other in a narrow panel
    let footer = if dense {
        // the EXIF line across the whole width, over the options row
        Rect::from_min_max(pos2(r.left(), r.bottom() - options - 26.0), pos2(r.right(), r.bottom() - options))
    } else {
        Rect::from_min_max(pos2(menu_rect.right() + 4.0, r.bottom() - 26.0), pos2(r.right(), r.bottom()))
    };
    let footer_x = footer.center().x;
    let footer_y = footer.bottom() - 13.0;

    let pass = ui.ctx().cumulative_pass_nr();
    let hover = app.ui.hover_color.filter(|(at, _)| pass.saturating_sub(*at) <= 1).map(|(_, c)| c);
    if let Some(text) = zone.as_ref().and_then(|a| a.readout()) {
        register(ui.ctx(), "histogramReadout", r);
        p.text(pos2(footer_x, footer_y), Align2::CENTER_CENTER, text, t.font(12.5), t.text_label);
    } else if let Some(rgb) = hover {
        let text = app.ui.settings.color_readout.format(rgb);
        register(ui.ctx(), "histogramReadout", r);
        p.text(pos2(footer_x, footer_y), Align2::CENTER_CENTER, text, t.font(12.5), t.text_label);
    } else if let Some(ph) = app.session.catalog.photo(id) {
        // EXIF strip
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
            // a smaller size where the strip is narrow (the Library's Histogram panel), so it stays inside the panel
            let fits = |size: f32| p.layout_no_wrap(text.clone(), t.font(size), t.text_label).size().x <= footer.width() - 6.0;
            let size = [12.5, 11.5, 10.5].into_iter().find(|s| fits(*s)).unwrap_or(10.5);
            p.text(pos2(footer_x, footer_y), Align2::CENTER_CENTER, text, t.font(size), t.text_label);
        }
    }
    if dense {
        original_photo_box(
            app,
            ui,
            Rect::from_min_max(pos2(r.left() + crate::widgets::PAD_L, r.bottom() - options), pos2(menu_rect.left() - 4.0, r.bottom())),
        );
    }
    if let Some(second) = HistogramMode::parse(&app.ui.histogram_second) {
        second_histogram(app, ui, id, second, scale, hdr_edit);
    }
}

/// The Compact histogram's Original Photo box (`view.showOriginal`): while ticked the loupe shows the
/// photo without its edit, as holding `\` does. Own design.
fn original_photo_box(app: &mut LccApp, ui: &mut egui::Ui, row: Rect) {
    if !(row.width() > 1.0 && row.height() > 1.0) {
        return;
    }
    ui.scope_builder(egui::UiBuilder::new().max_rect(row).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
        let t = Tokens::get(ui.ctx());
        let mut on = app.ui.before_after == crate::state::BeforeAfter::Original;
        let r = ui.checkbox(&mut on, RichText::new(crate::i18n::tr("Original Photo")).font(t.font(11.5)).color(t.text_label));
        register(ui.ctx(), "check:originalPhoto", r.rect);
        if r.on_hover_text(crate::i18n::tr("Show the photo without its edit while ticked")).changed() {
            let _ = app.run("view.showOriginal", json!({"on": on}));
        }
    });
}

/// Height of the histogram for `mode`, without an HDR legend (the waveforms are as wide as the
/// panel: they need less height than the disc).
fn histogram_height(mode: HistogramMode) -> f32 {
    if mode.is_waveform() {
        195.0
    } else if mode.is_scope() {
        240.0
    } else {
        154.0
    }
}

/// The optional second histogram under the first (`view.histogram.second`): another
/// distribution of the same render, with its own mode selector. The tonal zones and the clipping
/// warnings stay on the first.
fn second_histogram(app: &mut LccApp, ui: &mut egui::Ui, id: PhotoId, mode: HistogramMode, scale: HistogramScale, hdr_edit: bool) {
    let t = Tokens::get(ui.ctx());
    let hist = current_histogram(app, ui.ctx(), id, hdr_edit);
    let hist = panel_render_histogram(app, id, mode, hist);
    let hist = steady_hdr_histogram(ui.ctx(), id, hdr_edit, hist).map(|h| if mode.is_scope() { h } else { Histogram { hdr: None, ..h } });
    ui.add_space(2.0);
    let w = ui.available_width();
    let (r, resp) = ui.allocate_exact_size(vec2(w, histogram_height(mode)), Sense::click());
    crate::widgets::announce(&resp, crate::i18n::tr("Second Histogram"));
    register(ui.ctx(), "histogram2", r);
    let p = ui.painter();
    p.rect_filled(r, 0.0, t.button);
    let plot = Rect::from_min_max(r.min + vec2(0.0, 6.0), pos2(r.right(), r.bottom() - 26.0));
    p.rect_filled(plot, 0.0, super::histogram::SCOPE_BG);
    if let Some(h) = &hist {
        super::histogram::paint_plot(p, plot, h, mode, scale);
    }
    let menu_rect = Rect::from_min_size(pos2(r.left() + 4.0, r.bottom() - 22.0), vec2(72.0, 18.0));
    let menu = ui.interact(menu_rect, ui.id().with("histogram2-display"), Sense::click());
    register(ui.ctx(), "histogram2:display", menu_rect);
    let col = if menu.hovered() { t.text } else { t.text_label };
    p.text(pos2(menu_rect.left() + 3.0, menu_rect.center().y), Align2::LEFT_CENTER, crate::i18n::tr(mode.short()), t.font(11.5), col);
    paint(p, Rect::from_center_size(pos2(menu_rect.right() - 8.0, menu_rect.center().y), vec2(12.0, 12.0)), Icon::ChevronDown, col);
    egui::Popup::menu(&menu).show(|ui| second_histogram_menu(app, ui));
    crate::widgets::context_menu(&resp, |ui| second_histogram_menu(app, ui));
}

/// The second histogram's mode, or none: in both histograms' menus.
fn second_histogram_menu(app: &mut LccApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let cur = HistogramMode::parse(&app.ui.histogram_second);
    ui.label(RichText::new(crate::i18n::tr("Second Histogram")).color(t.text_label));
    if ui.selectable_label(cur.is_none(), crate::i18n::tr("None")).clicked() {
        let _ = app.run("view.histogram.second", json!({"mode": "off"}));
        ui.close();
    }
    for m in HistogramMode::ALL {
        if ui.selectable_label(cur == Some(m), crate::i18n::tr(m.label())).clicked() {
            let _ = app.run("view.histogram.second", json!({"mode": m.name()}));
            ui.close();
        }
    }
}

/// The histogram's own display menu, shared by the selector under the plot and the plot's
/// right-click menu: the distribution it draws, then the scale it draws it at.
fn histogram_display_menu(app: &mut LccApp, ui: &mut egui::Ui, mode: HistogramMode, scale: HistogramScale) {
    let t = Tokens::get(ui.ctx());
    ui.label(RichText::new(crate::i18n::tr("Histogram Mode")).color(t.text_label));
    for m in HistogramMode::ALL {
        if ui.selectable_label(mode == m, crate::i18n::tr(m.label())).clicked() {
            let _ = app.run("view.histogram.mode", json!({"mode": m.name()}));
            ui.close();
        }
    }
    ui.separator();
    ui.label(RichText::new(crate::i18n::tr("Scale")).color(t.text_label));
    for s in HistogramScale::ALL {
        if ui.selectable_label(scale == s, crate::i18n::tr(s.label())).clicked() {
            let _ = app.run("view.histogram.scale", json!({"scale": s.name()}));
            ui.close();
        }
    }
}

// ------------------------------------------------------------------------------ soft proofing

/// The Soft Proofing strip under the histogram: proof profile, gamut warnings, Create Proof Copy.
fn soft_proofing(app: &mut LccApp, ui: &mut egui::Ui, id: PhotoId) {
    use lcc_engine::pipeline::OutputSpace;
    let t = Tokens::get(ui.ctx());
    egui::Frame::NONE.inner_margin(crate::widgets::row_margin(ui, egui::Margin { left: 12, right: 16, top: 8, bottom: 6 }, 3, 3)).show(ui, |ui| {
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
                use lcc_engine::proof::ProofIntent;
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
fn curve_histogram_profile(h: &lcc_raster::Histogram, channel: &str) -> [f32; 256] {
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
fn draw_curve_histogram(p: &egui::Painter, r: Rect, h: &lcc_raster::Histogram, channel: &str) {
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

/// Segments the graph's curve line is drawn with.
pub(crate) const CURVE_SAMPLES: i32 = 96;

/// What the parametric graph draws and drags: the region curve with the master point curve on top
/// of it, stacked as `lcc_pipeline::finish::curve_luts` stacks them. A Point Curve preset such as
/// Medium Contrast shows in the line, as in Lightroom; a drag still moves only the region sliders.
pub(crate) struct StackedCurve {
    pub(crate) param: ParametricCurve,
    /// The master point curve, `None` while it is the identity.
    master: Option<lcc_color::spline::MonotoneCurve>,
}

impl StackedCurve {
    pub(crate) fn new(c: &ToneCurve) -> StackedCurve {
        let master = (!ToneCurve::point_curve_is_identity(&c.master))
            .then(|| lcc_color::spline::MonotoneCurve::new(&c.master.iter().map(|q| (q.x, q.y)).collect::<Vec<_>>()));
        StackedCurve { param: ParametricCurve::new(c), master }
    }

    /// The master point curve at a level.
    pub(crate) fn master_at(&self, y: f64) -> f64 {
        match &self.master {
            Some(m) => m.eval(y.clamp(0.0, 1.0)).clamp(0.0, 1.0),
            None => y,
        }
    }

    /// The tone the renderer gives graph position `x`.
    pub(crate) fn at(&self, x: f64) -> f64 {
        self.master_at(self.param.at(x))
    }

    /// The region-curve level the master curve takes to `level`: what a drag on the drawn line
    /// asks of the region sliders. Found by halving, as the master curve rises (falls, if it is
    /// drawn upside down); where it is flat, any level of the flat part is a fine answer.
    pub(crate) fn param_level(&self, level: f64) -> f64 {
        if self.master.is_none() {
            return level;
        }
        let rising = self.master_at(1.0) >= self.master_at(0.0);
        let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
        for _ in 0..40 {
            let mid = (lo + hi) / 2.0;
            if (self.master_at(mid) < level) == rising {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        (lo + hi) / 2.0
    }
}

/// Close a graph drag's `develop` interaction, as one undo step.
///
/// It is closed when the drag ends *or* the pointer comes up without egui reporting `drag_stopped`
/// for this widget (released outside the window, over a popup, or on a frame the widget does not
/// run). A dangling interaction would make every later edit a silent preview — Undo would then
/// appear to do nothing at all. Returns whether the drag is over, so the caller clears its state.
fn commit_interaction(app: &mut LccApp, ui: &egui::Ui, dragging: bool, stopped: bool) -> bool {
    let released = ui.input(|i| i.pointer.any_released()) || !ui.input(|i| i.pointer.primary_down());
    if !dragging || !(stopped || released) {
        return false;
    }
    let _ = app.run("develop.endInteraction", json!({}));
    true
}

/// How close (screen points) to the curve a press counts as grabbing it.
const PARAM_GRAB: f32 = 14.0;

/// The parametric curve's own drag: the graph shows the curve and nothing else, and dragging the
/// line bends the region the pointer is over, with that region's slider following on the same frame.
///
/// The whole gesture runs inside one `develop` interaction, so it is one undo step (Ctrl+Z takes the
/// gesture back, not the last frame of it), and every write goes through `develop.set`, so the
/// slider, the curve and what is drawn are the same state rather than three things kept in step.
fn param_drag(app: &mut LccApp, ui: &mut egui::Ui, r: Rect, resp: &egui::Response, d: &DevelopSettings) {
    if r.width() <= 1.0 || r.height() <= 1.0 {
        return;
    }
    let t = Tokens::get(ui.ctx());
    let stacked = StackedCurve::new(&d.curve);
    let model = &stacked.param;
    let to_screen = |x: f64, y: f64| pos2(r.left() + x as f32 * r.width(), r.bottom() - y as f32 * r.height());
    let from_screen_x = |q: Pos2| ((q.x - r.left()) / r.width()).clamp(0.0, 1.0) as f64;
    // the graph x of a point, and how far it is from the drawn curve (in screen points)
    let near_curve = |q: Pos2| {
        let x = from_screen_x(q);
        to_screen(x, stacked.at(x)).distance(q)
    };
    let drag_id = egui::Id::new("curve-param-drag");
    let cursor_id = egui::Id::new("curve-param-cursor");
    // (region, x, initial_amount, start_y): the region being dragged, the graph x the drag is anchored
    // at, the region's starting amount, and the starting screen Y position (stored as an `Option` so
    // a stale value can be cleared, as the point-curve editor's drag state is)
    let mut dragged: Option<(usize, f64, f64, f32)> = ui.data(|dd| dd.get_temp::<Option<(usize, f64, f64, f32)>>(drag_id)).flatten();
    let hovered = dragged.is_none() && resp.hover_pos().is_some_and(|q| near_curve(q) <= PARAM_GRAB);
    let mut set: Option<(&'static str, f64)> = None;
    // the press, not where the pointer is once the drag threshold has passed: a press slightly off
    // the line is still a grab, and the pointer has usually left that spot by the time it starts
    let press = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos());
    if resp.drag_started()
        && let Some(q) = press
        && near_curve(q) <= PARAM_GRAB
    {
        let x = from_screen_x(q);
        let _ = app.run("develop.beginInteraction", json!({"label": "Parametric Curve"}));
        let region = model.region_at(x);
        let initial_amount = model.amounts.get(region).copied().unwrap_or(0.0);
        let start_pos = to_screen(x, stacked.at(x));
        dragged = Some((region, x, initial_amount, start_pos.y));
        ui.data_mut(|dd| dd.insert_temp(cursor_id, start_pos));
    }
    if let Some((region, x, initial_amount, start_y)) = dragged
        && resp.dragged()
    {
        // Follow the pointer's motion from where the curve was grabbed, so the press itself moves
        // nothing and the line tracks the hand. ⌥ = a tenth of the motion, as the point curve does.
        let alt = ui.input(|i| i.modifiers.alt);
        let dd = ui.input(|i| i.pointer.delta());
        let step = if alt { vec2(dd.x * 0.1, dd.y * 0.1) } else { vec2(dd.x, dd.y) };
        let cursor = ui.data(|d| d.get_temp::<Pos2>(cursor_id)).unwrap_or(pos2(0.0, start_y)) + step;
        ui.data_mut(|d| d.insert_temp(cursor_id, cursor));
        // Only the vertical motion since the grab counts (a horizontal drag changes nothing, however
        // much the neighbouring regions lift the curve there). The pointer is on the drawn (stacked)
        // line, so the motion is measured in region-curve levels: what the master curve takes to it.
        let level_of = |y: f32| ((r.bottom() - y) / r.height()).clamp(0.0, 1.0) as f64;
        let delta_level = stacked.param_level(level_of(cursor.y)) - stacked.param_level(level_of(start_y));
        let per_unit = model.level_per_unit(region, x);
        let new_amount =
            if per_unit.abs() > f64::EPSILON { (initial_amount + delta_level / per_unit).clamp(-100.0, 100.0).round() } else { initial_amount };
        if let Some(&id) = PARAM_AMOUNTS.get(region) {
            set = Some((id, new_amount));
        }
    }
    if let Some((id, v)) = set {
        let _ = app.run("develop.set", json!({"control": id, "value": v}));
    }
    let dropped = resp.drag_stopped();
    if commit_interaction(app, ui, dragged.is_some(), dropped) {
        dragged = None;
        ui.data_mut(|d| {
            d.remove::<Pos2>(cursor_id);
            d.remove::<Option<(usize, f64, f64, f32)>>(drag_id);
        });
    }
    ui.data_mut(|dd| dd.insert_temp(drag_id, dragged));
    if dragged.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    } else if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
    }
    // the region under the pointer (or the one in hand), named as its slider below reads it
    let shown = dragged.map(|(region, ..)| region).or_else(|| hovered.then(|| resp.hover_pos()).flatten().map(|q| model.region_at(from_screen_x(q))));
    if let Some(region) = shown
        && let Some(&id) = PARAM_AMOUNTS.get(region)
        && let Some(v) = set.and_then(|(s_id, v)| (s_id == id).then_some(v)).or_else(|| controls::get(d, id))
    {
        let name = controls::find(id).map_or("", |s| s.label);
        ui.painter().text(r.left_top() + vec2(6.0, 4.0), Align2::LEFT_TOP, format!("{name} {v:.0}"), t.font(11.0), t.text_dim);
    }
}

/// Whose point curves a curve editor edits: the photo's Tone Curve, or one mask's local Curve (the
/// same editor without the parametric curve, Refine Saturation and the targeted adjustment; the curve
/// commands take the mask's id as `mask`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum CurveTarget {
    Photo,
    Mask(u32),
}

impl CurveTarget {
    /// The name of one of the editor's widgets: `curve…` for the Tone Curve, `maskCurve…` for a mask's.
    fn widget(self, suffix: &str) -> String {
        match self {
            CurveTarget::Photo => format!("curve{suffix}"),
            CurveTarget::Mask(_) => format!("maskCurve{suffix}"),
        }
    }
    /// Command parameters `p` for this target (`mask` added for a mask's curve).
    fn params(self, mut p: Value) -> Value {
        if let (CurveTarget::Mask(m), Some(o)) = (self, p.as_object_mut()) {
            o.insert("mask".into(), json!(m));
        }
        p
    }
    /// The point curves this target edits in `d` (a mask that is gone has none).
    fn points(self, d: &DevelopSettings) -> lcc_develop::PointCurves {
        match self {
            CurveTarget::Photo => d.curve.points(),
            CurveTarget::Mask(m) => d.masks.iter().find(|x| x.id == m).map(|x| x.curve.clone()).unwrap_or_default(),
        }
    }
    /// The channel shown: the Tone Curve's and the masks' are remembered apart.
    fn channel(self, app: &LccApp) -> String {
        match self {
            CurveTarget::Photo => app.ui.curve_channel.clone(),
            CurveTarget::Mask(_) => app.ui.mask_curve_channel.clone(),
        }
    }
    fn set_channel(self, app: &mut LccApp, ch: &str) {
        match self {
            CurveTarget::Photo => app.ui.curve_channel = ch.into(),
            CurveTarget::Mask(_) => app.ui.mask_curve_channel = ch.into(),
        }
    }
    /// The undo label of a drag.
    fn label(self) -> &'static str {
        match self {
            CurveTarget::Photo => "Tone Curve",
            CurveTarget::Mask(_) => "Mask Curve",
        }
    }
}

/// The Tone Curve editor (`CurveTarget::Photo`) or a mask's local Curve editor.
/// The closest two points of a point curve may be in x (a fraction of the graph's width).
pub(crate) const POINT_GAP: f64 = 0.01;

/// `pts` with a point added at (`x`, `y`), and where it went; `None` when a point is already
/// within [`POINT_GAP`] of `x` (the curve would fall off a cliff between the two).
pub(crate) fn with_point(pts: &[Point], x: f64, y: f64) -> Option<(usize, Vec<Point>)> {
    if pts.iter().any(|p| (p.x - x).abs() < POINT_GAP) {
        return None;
    }
    let i = pts.iter().position(|p| p.x > x).unwrap_or(pts.len());
    let mut v = pts.to_vec();
    v.insert(i, Point::new(x, y));
    Some((i, v))
}

pub(crate) fn curve_editor(app: &mut LccApp, ui: &mut egui::Ui, id: PhotoId, d: &DevelopSettings, target: CurveTarget) {
    let t = Tokens::get(ui.ctx());
    let photo = target == CurveTarget::Photo;
    if !photo && !lcc_develop::PointCurves::CHANNELS.contains(&target.channel(app).as_str()) {
        target.set_channel(app, "master");
    }
    // channel selector
    egui::Frame::NONE.inner_margin(crate::widgets::row_margin(ui, egui::Margin { left: 12, right: 16, top: 6, bottom: 4 }, 3, 3)).show(ui, |ui| {
        ui.horizontal(|ui| {
            for (ch, c) in [
                ("parametric", Color32::from_gray(200)),
                ("master", Color32::WHITE),
                ("red", hex("#dd3333")),
                ("green", hex("#33bb55")),
                ("blue", hex("#3377ee")),
            ] {
                if ch == "parametric" && !photo {
                    continue;
                }
                let (r, resp) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::click());
                crate::widgets::announce(&resp, format!("{} {ch}", crate::i18n::tr("Curve channel")));
                register(ui.ctx(), target.widget(&format!("Channel:{ch}")), r);
                let sel = target.channel(app) == ch;
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
                let resp = resp.on_hover_text(crate::i18n::tr_format!("{name}: double-click to reset it", name = name));
                if resp.double_clicked() {
                    target.set_channel(app, ch);
                    let _ = app.run("curve.reset", target.params(json!({"channel": ch})));
                } else if resp.clicked() {
                    target.set_channel(app, ch);
                }
            }
            if photo {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    tat_button(app, ui, "tat:curve", "Targeted adjustment: drag up/down on the photo to adjust the curve there");
                });
            }
        });
    });
    let ch = target.channel(app);
    let w = ui.available_width();
    let side = (w - 48.0).max(80.0);
    let (outer, _) = ui.allocate_exact_size(vec2(w, side + 12.0), Sense::hover());
    let r = Rect::from_min_size(pos2(outer.left() + 24.0, outer.top() + 6.0), vec2(side, side));
    let resp = ui.interact(r, egui::Id::new(("curve", target.widget(""), &ch)), Sense::click_and_drag());
    crate::widgets::announce(&resp, crate::i18n::tr(target.label()));
    register(ui.ctx(), target.widget(""), r);
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
    if ch == "parametric" && photo {
        // The Effective curve: the model the renderer builds from the four region sliders, with the
        // master point curve on top, sampled here and rebuilt every frame. The graph holds no state
        // of its own, so a slider moved by hand, a preset, a Reset or an Undo redraws it from the
        // settings in the same frame it changes.
        let c = &d.curve;
        let model = StackedCurve::new(c);
        let pts: Vec<Pos2> =
            (0..=CURVE_SAMPLES).map(|i| to_screen(i as f64 / CURVE_SAMPLES as f64, model.at(i as f64 / CURVE_SAMPLES as f64))).collect();
        p.add(egui::Shape::line(pts, Stroke::new(2.0, Color32::from_gray(220))));
        param_drag(app, ui, r, &resp, d);
        curve_footer(app, ui, d, target);
        for c in PARAM_AMOUNTS {
            control(app, ui, d, c, true);
        }
        control_as(app, ui, d, "curve.refineSaturation", true, Some("Refine"));
        return;
    }
    let curves = target.points(d);
    let pts_of = |c: &lcc_develop::PointCurves| -> Vec<Point> {
        let v = c.channel(&ch).map_or(&c.master, |v| v);
        // in the order the curve is drawn (points from a sidecar, a preset or an agent can come
        // unsorted or not finite): the drag below keeps each point between its neighbours
        let mut v: Vec<Point> = v.iter().copied().filter(|q| q.x.is_finite() && q.y.is_finite()).collect();
        v.sort_by(|a, b| a.x.total_cmp(&b.x));
        if v.len() < 2 { vec![Point::new(0.0, 0.0), Point::new(1.0, 1.0)] } else { v }
    };
    let pts = pts_of(&curves);
    let curve = lcc_color::spline::MonotoneCurve::new(&pts.iter().map(|q| (q.x, q.y)).collect::<Vec<_>>());
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
    let drag_id = egui::Id::new(("curve-drag", target.widget(""), &ch));
    // the point being dragged (stored as `Option<usize>` so a stale value can be cleared)
    let mut dragging: Option<usize> = ui.data(|dd| dd.get_temp::<Option<usize>>(drag_id)).flatten();
    // the point the Input / Output fields edit: the last one clicked, added or dragged
    let sel_id = egui::Id::new(("curve-selected", target.widget(""), &ch));
    let mut selected: Option<usize> = ui.data(|dd| dd.get_temp::<Option<usize>>(sel_id)).flatten().filter(|i| *i < pts.len());
    let nearest = |q: Pos2| pts.iter().enumerate().map(|(i, p)| (i, to_screen(p.x, p.y).distance(q))).min_by(|a, b| a.1.total_cmp(&b.1));
    let hovered = resp.hover_pos().and_then(nearest).filter(|(_, dist)| *dist < 10.0).map(|(i, _)| i);
    // right-click on a point: the menu opens with Delete Control Point (not on the two end points);
    // the point under the pointer is read when the menu opens and kept while it is open
    let menu_point_id = egui::Id::new(("curve-menu-point", target.widget(""), &ch));
    if resp.secondary_clicked() {
        let hit = resp
            .interact_pointer_pos()
            .and_then(nearest)
            .filter(|(i, dist)| *dist < 10.0 && *i != 0 && i.checked_add(1).is_some_and(|n| n < pts.len()))
            .map(|(i, _)| i);
        ui.data_mut(|dd| dd.insert_temp(menu_point_id, hit));
    }
    let menu_point: Option<usize> = ui.data(|dd| dd.get_temp::<Option<usize>>(menu_point_id)).flatten().filter(|i| *i < pts.len());
    crate::widgets::context_menu(&resp, |ui| {
        if let Some(i) = menu_point {
            let r = ui.button(crate::i18n::tr("Delete Control Point"));
            register(ui.ctx(), target.widget("Menu:deletePoint"), r.rect);
            if r.clicked() {
                let arr: Vec<[f64; 2]> = pts.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, p)| [p.x, p.y]).collect();
                let _ = app.run("develop.curve", target.params(json!({"channel": ch, "points": arr})));
                ui.close();
            }
            ui.separator();
        }
        curve_reset_menu(app, ui, &ch, target);
    });
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
        let (x, y) = from_screen(q);
        let grabbed = match nearest(q) {
            Some((i, dist)) if dist < 10.0 => Some((i, None)),
            _ => with_point(&pts, x, y).map(|(i, v)| (i, Some(v))),
        };
        // a press that grabs nothing and may add nothing starts no interaction (it would never be closed)
        if let Some((i, added)) = grabbed {
            let _ = app.run("develop.beginInteraction", json!({"label": target.label()}));
            dragging = Some(i);
            selected = Some(i);
            new_pts = added;
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
            let lo = if i == 0 { 0.0 } else { v[i - 1].x + POINT_GAP };
            let hi = if i + 1 >= v.len() { 1.0 } else { v[i + 1].x - POINT_GAP };
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
        // the new point goes on the curve at the clicked x, as in Lightroom Classic: adding a
        // point doesn't change the curve until it is dragged
        let (x, _) = from_screen(q);
        let y = curve.eval(x).clamp(0.0, 1.0);
        if let Some((i, v)) = with_point(&pts, x, y) {
            selected = Some(i);
            new_pts = Some(v);
        }
    } else if resp.clicked()
        && let Some(q) = resp.interact_pointer_pos()
        && let Some((i, _)) = nearest(q)
    {
        selected = Some(i);
    }
    if resp.double_clicked() && new_pts.is_some() {
        selected = None;
    }
    if new_pts.is_none()
        && let Some(i) = selected
        && let Some(q) = curve_point_fields(ui, target, i, &pts)
    {
        let mut v = pts.clone();
        if let Some(slot) = v.get_mut(i) {
            *slot = q;
            new_pts = Some(v);
        }
    }
    if let Some(v) = new_pts {
        let arr: Vec<[f64; 2]> = v.iter().map(|p| [p.x, p.y]).collect();
        let _ = app.run("develop.curve", target.params(json!({"channel": ch, "points": arr})));
    }
    // the drag is over when egui says so, or when the pointer comes up without it saying so (a
    // release outside the window would otherwise leave the interaction open, and every later edit a
    // silent preview that Undo cannot take back)
    if commit_interaction(app, ui, dragging.is_some(), resp.drag_stopped()) {
        dragging = None;
    }
    ui.data_mut(|dd| {
        dd.insert_temp(drag_id, dragging);
        dd.insert_temp(sel_id, selected);
    });
    curve_footer(app, ui, d, target);
    if photo {
        control_as(app, ui, d, "curve.refineSaturation", true, Some("Refine"));
    }
    let _ = id;
}

/// Input and Output fields (0–255, like the histogram) for point `i` of a point curve, as in
/// Lightroom Classic: typing a value places the point exactly. Returns the moved point when an edit
/// is committed; the input stays between the neighbouring points.
fn curve_point_fields(ui: &mut egui::Ui, target: CurveTarget, i: usize, pts: &[Point]) -> Option<Point> {
    use crate::text_field::TextField;
    let t = Tokens::get(ui.ctx());
    let cur = *pts.get(i)?;
    let mut out = None;
    egui::Frame::NONE.inner_margin(crate::widgets::row_margin(ui, egui::Margin { left: 12, right: 16, top: 0, bottom: 2 }, 0, 2)).show(ui, |ui| {
        ui.horizontal(|ui| {
            for (axis, name) in [(0, "Input"), (1, "Output")] {
                let widget = target.widget(&format!("Point{name}"));
                let value = if axis == 0 { cur.x } else { cur.y };
                let shown = format!("{}", (value * 255.0).round());
                // the text being typed lives with the field until the edit ends; otherwise it shows the point
                let buf_id = egui::Id::new(("curve-field", &widget));
                let focused = ui.memory(|m| m.has_focus(crate::text_field::id(&widget)));
                let mut text = if focused { ui.data(|d| d.get_temp::<String>(buf_id)).unwrap_or_else(|| shown.clone()) } else { shown.clone() };
                ui.label(RichText::new(crate::i18n::tr(name)).font(t.font(12.0)).color(t.text_label));
                let r = TextField::singleline(&widget, &mut text)
                    .width(36.0)
                    .font(t.font(12.0))
                    .select_on_focus(true)
                    .label(crate::i18n::tr(name))
                    .show(ui);
                if r.committed() {
                    if let Ok(n) = text.trim().parse::<f64>()
                        && n.is_finite()
                    {
                        let v = (n / 255.0).clamp(0.0, 1.0);
                        let mut q = cur;
                        if axis == 0 {
                            let lo = if i == 0 { 0.0 } else { pts.get(i - 1).map_or(0.0, |p| p.x + 0.01) };
                            let hi = pts.get(i + 1).map_or(1.0, |p| p.x - 0.01);
                            q.x = if lo <= hi { v.clamp(lo, hi) } else { cur.x };
                        } else {
                            q.y = v;
                        }
                        out = Some(q);
                    }
                    ui.data_mut(|d| d.remove::<String>(buf_id));
                } else if r.cancelled() {
                    ui.data_mut(|d| d.remove::<String>(buf_id));
                } else if focused || r.response.has_focus() {
                    ui.data_mut(|d| d.insert_temp(buf_id, text));
                }
                ui.add_space(8.0);
            }
        });
    });
    out
}

/// The row under the curve graph: point-curve presets and reset every curve.
fn curve_footer(app: &mut LccApp, ui: &mut egui::Ui, d: &DevelopSettings, target: CurveTarget) {
    use lcc_engine::cmd::curves::{all_presets, matching_points_preset};
    let t = Tokens::get(ui.ctx());
    egui::Frame::NONE.inner_margin(crate::widgets::row_margin(ui, egui::Margin { left: 12, right: 16, top: 2, bottom: 4 }, 2, 3)).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(crate::i18n::tr("Point Curve")).font(t.font(12.0)).color(t.text_label));
            let current = matching_points_preset(&app.session, &target.points(d));
            let builtin = current.as_ref().is_none_or(|name| all_presets(&app.session).iter().any(|p| p.builtin && &p.name == name));
            let label = crate::i18n::builtin_label(current.as_deref().unwrap_or("Custom"), builtin);
            let r = crate::widgets::dropdown(ui, &target.widget("Preset"), label, t.font(12.0), t.text);
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
                    register(ui.ctx(), target.widget(&format!("Preset:{}", p.name)), r.rect);
                    if r.clicked() {
                        let _ = app.run("curve.applyPreset", target.params(json!({"name": p.name})));
                        ui.close();
                    }
                    if !p.builtin {
                        crate::widgets::context_menu(&r, |ui| {
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
                register(ui.ctx(), target.widget("PresetMenu:save"), r.rect);
                if r.clicked() {
                    crate::panels::dialogs::prompt(
                        app,
                        "Save Point Curve Preset",
                        "Preset name",
                        "",
                        "curve.savePreset",
                        target.params(json!({})),
                        "name",
                    );
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
                let tip = match target {
                    CurveTarget::Photo => {
                        "Reset all curves: point curves (all channels) and parametric (double-click a channel to reset only that one)"
                    }
                    CurveTarget::Mask(_) => "Reset the mask's curve, all channels (double-click a channel to reset only that one)",
                };
                let r = text_button(ui, &target.widget("Reset"), crate::i18n::tr("Reset"), false).on_hover_text(crate::i18n::tr(tip));
                if r.clicked() {
                    let _ = app.run("curve.reset", target.params(json!({"channel": "all"})));
                }
            });
        });
    });
}

/// Right-click menu of the curve graph.
fn curve_reset_menu(app: &mut LccApp, ui: &mut egui::Ui, ch: &str, target: CurveTarget) {
    let label = if ch == "parametric" {
        crate::i18n::tr("Reset Parametric Curve").to_string()
    } else {
        crate::i18n::tr_format!("Reset {} Channel", crate::i18n::tr(channel_label(ch)))
    };
    let r = ui.button(label);
    register(ui.ctx(), target.widget("Menu:resetChannel"), r.rect);
    if r.clicked() {
        let _ = app.run("curve.reset", target.params(json!({"channel": ch})));
        ui.close();
    }
    let r = ui.button(crate::i18n::tr("Reset All Curves"));
    register(ui.ctx(), target.widget("Menu:resetAll"), r.rect);
    if r.clicked() {
        let _ = app.run("curve.reset", target.params(json!({"channel": "all"})));
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
fn tat_button(app: &mut LccApp, ui: &mut egui::Ui, tool: &str, tip: &str) {
    let active = app.ui.tool == tool;
    let id = tool.replace(':', "-");
    if crate::widgets::icon_button(ui, &id, Icon::Target, vec2(26.0, 26.0), active, true, tip).clicked() {
        set_tat(app, ui.ctx(), tool, active);
    }
}

/// Turn targeted-adjustment tool `tool` off (`active`) or on. Turning it on says how to use and
/// leave it: while it is on, dragging on the photo adjusts instead of zooming or panning, which is
/// easy to miss when the button looks like a tab.
fn set_tat(app: &mut LccApp, ctx: &egui::Context, tool: &str, active: bool) {
    if active {
        app.ui.tool.clear();
    } else {
        app.ui.tool = tool.to_string();
        app.toast(ctx, crate::i18n::tr("Targeted adjustment on: drag up or down on the photo · Esc to stop"));
    }
}

/// Targeted adjustment `target` (`curve|hue|sat|lum`) on, from Tools > Target Adjustment: the Edit panel
/// opens on the section whose control the drag changes, and a Color Mixer group is chosen the way its
/// button chooses it. Dragging on the photo then adjusts; Esc or None puts it away.
pub(crate) fn start_tat(app: &mut LccApp, target: &str) -> Result<(), String> {
    let Some(id) = app.session.active() else { return Err("tool.tat: no photo is open".into()) };
    let bw = app.session.develop_of(id).is_some_and(|d| crate::is_bw(&d));
    let section = match target {
        "curve" => "curve",
        "hue" | "sat" | "lum" => {
            if bw && target != "lum" {
                return Err(format!("tool.tat: a B&W photo has the B&W Mix (`lum`) only, not `{target}`"));
            }
            "mixer"
        }
        other => return Err(format!("tool.tat: unknown target `{other}` (curve|hue|sat|lum)")),
    };
    app.ui.right = RightPanel::Edit;
    app.ui.view = crate::state::ViewMode::Detail;
    if !app.ui.section_open(section) {
        app.ui.toggle_section(section);
    }
    if section == "mixer" {
        // the Mixer tab (not Point Color), on the group the drag changes unless All shows them all
        if app.ui.flyout_open("pointColor") {
            app.ui.toggle_flyout("pointColor");
        }
        if app.ui.mixer_adjust != "color" && app.ui.mixer_group != "all" {
            app.ui.mixer_group = target.to_string();
        }
    }
    app.ui.tool = format!("tat:{target}");
    Ok(())
}

// ------------------------------------------------------------------------------ colour mixer

/// The Mixer's Adjust choice (HSL | Color) and the HSL groups (Hue | Saturation | Luminance | All),
/// as (label, key).
pub(crate) const MIXER_ADJUST: [(&str, &str); 2] = [("HSL", "hsl"), ("Color", "color")];
pub(crate) const MIXER_GROUPS: [(&str, &str); 4] = [("Hue", "hue"), ("Saturation", "sat"), ("Luminance", "lum"), ("All", "all")];
/// The mixer bands' names, as the HSL sliders are labelled (parallel to `MIXER_BANDS`).
pub(crate) const BAND_NAMES: [&str; 8] = ["Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta"];

/// A Color Mixer targeted adjustment (`tat:hue|sat|lum`) whose button the mixer's layout no longer
/// shows is put down (a B&W photo shows Luminance's only, Point Color none, HSL the chosen group's
/// or all three in All, Color mode all three): dragging on the photo would change a hidden group.
pub(crate) fn drop_hidden_mixer_tool(app: &mut LccApp) {
    let Some(key) = app.ui.tool.strip_prefix("tat:") else { return };
    if !matches!(key, "hue" | "sat" | "lum") {
        return;
    }
    let bw = app.session.active().and_then(|id| app.session.develop_of(id)).is_some_and(|d| crate::is_bw(&d));
    let shown = if bw {
        key == "lum"
    } else {
        !app.ui.flyout_open("pointColor") && (app.ui.mixer_adjust == "color" || app.ui.mixer_group == "all" || app.ui.mixer_group == key)
    };
    if !shown {
        app.ui.tool.clear();
    }
}

/// The Color Mixer's Mixer tab, as in Lightroom Classic: it adjusts by HSL (Hue | Saturation |
/// Luminance | All: eight colour sliders per group, each group with its targeted adjustment) or by
/// Color (one colour at a time). The same `mixer.<band>.<hue|sat|lum>` controls either way; a B&W
/// photo shows its B&W mix.
fn mixer(app: &mut LccApp, ui: &mut egui::Ui, d: &DevelopSettings) {
    if crate::is_bw(d) {
        mixer_by_color(app, ui, d);
        return;
    }
    let t = Tokens::get(ui.ctx());
    egui::Frame::NONE.inner_margin(crate::widgets::row_margin(ui, egui::Margin { left: 12, right: 16, top: 4, bottom: 2 }, 3, 2)).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(crate::i18n::tr("Adjust")).color(t.text_dim).size(11.5));
            let at = MIXER_ADJUST.iter().position(|(_, k)| *k == app.ui.mixer_adjust).unwrap_or(0);
            if let Some((_, k)) = crate::widgets::segmented(ui, "mixerAdjust", &MIXER_ADJUST, Some(at), 2).and_then(|i| MIXER_ADJUST.get(i)) {
                app.ui.mixer_adjust = k.to_string();
            }
        });
    });
    drop_hidden_mixer_tool(app);
    if app.ui.mixer_adjust == "color" {
        mixer_by_color(app, ui, d);
        return;
    }
    egui::Frame::NONE.inner_margin(crate::widgets::row_margin(ui, egui::Margin { left: 12, right: 16, top: 6, bottom: 2 }, 3, 2)).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 14.0;
            for (label, key) in MIXER_GROUPS {
                if group_tab(ui, &format!("mixerGroup-{key}"), crate::i18n::tr(label), app.ui.mixer_group == key).clicked() {
                    app.ui.mixer_group = key.to_string();
                    drop_hidden_mixer_tool(app);
                }
            }
        });
    });
    let groups: &[(&str, &str)] = match app.ui.mixer_group.as_str() {
        "sat" => &MIXER_GROUPS[1..2],
        "lum" => &MIXER_GROUPS[2..3],
        "all" => &MIXER_GROUPS[..3],
        _ => &MIXER_GROUPS[..1],
    };
    let all = groups.len() > 1;
    for (label, key) in groups {
        // the group's targeted adjustment (and, in All, its heading)
        egui::Frame::NONE.inner_margin(egui::Margin { left: 12, right: 16, top: if all { 8 } else { 2 }, bottom: 0 }).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let tool = format!("tat:{key}");
                let active = app.ui.tool == tool;
                if crate::widgets::icon_button(
                    ui,
                    &format!("tatMixer-{key}"),
                    Icon::Target,
                    vec2(22.0, 22.0),
                    active,
                    true,
                    "Targeted adjustment: drag up/down on the photo to adjust the colours there",
                )
                .clicked()
                {
                    set_tat(app, ui.ctx(), &tool, active);
                }
                if all {
                    let r = ui.label(egui::RichText::new(crate::i18n::tr(label)).color(t.text).strong().size(12.0));
                    register(ui.ctx(), format!("mixerHeading:{key}"), r.rect);
                }
            });
        });
        for (band, name) in lcc_develop::MIXER_BANDS.iter().zip(BAND_NAMES) {
            control_as(app, ui, d, &format!("mixer.{band}.{key}"), true, Some(name));
        }
    }
}

/// A word tab (Hue, Saturation…): bright and underlined when it is the one shown.
pub(crate) fn group_tab(ui: &mut egui::Ui, id: &str, label: &str, active: bool) -> egui::Response {
    let t = Tokens::get(ui.ctx());
    let font = t.semibold(11.5);
    let galley = ui.painter().layout_no_wrap(label.to_string(), font.clone(), t.text);
    let (r, resp) = ui.allocate_exact_size(vec2(galley.size().x, 20.0), Sense::click());
    crate::widgets::announce_selected(&resp, label.to_string(), active);
    register(ui.ctx(), format!("button:{id}"), r);
    let colour = if active || resp.hovered() { t.text } else { t.text_dim };
    ui.painter().text(pos2(r.left(), r.center().y), Align2::LEFT_CENTER, label, font, colour);
    if active {
        ui.painter().line_segment([pos2(r.left(), r.bottom() - 1.0), pos2(r.right(), r.bottom() - 1.0)], Stroke::new(1.5, t.text));
    }
    resp
}

/// The Mixer by colour (and a B&W photo's mix): a dot per colour, then that colour's sliders.
fn mixer_by_color(app: &mut LccApp, ui: &mut egui::Ui, d: &DevelopSettings) {
    let t = Tokens::get(ui.ctx());
    let bands = lcc_develop::MIXER_BANDS;
    let sel = bands.iter().position(|b| *b == app.ui.mixer_mode).unwrap_or(0);
    egui::Frame::NONE.inner_margin(crate::widgets::row_margin(ui, egui::Margin { left: 12, right: 16, top: 8, bottom: 4 }, 3, 3)).show(ui, |ui| {
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
    egui::Frame::NONE.inner_margin(crate::widgets::row_margin(ui, egui::Margin { left: 12, right: 16, top: 2, bottom: 2 }, 2, 2)).show(ui, |ui| {
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
        for (k, label) in [("hue", "Hue"), ("sat", "Saturation"), ("lum", "Luminance")] {
            control_as(app, ui, d, &format!("mixer.{b}.{k}"), true, Some(label));
        }
    }
}

// ------------------------------------------------------------------------------ point color

/// Display colour of an OkLCh sample (lightness, chroma, hue in degrees).
pub(crate) fn oklch_color(l: f64, c: f64, h_deg: f64) -> Color32 {
    use lcc_color::perceptual::{lch_to_lab, oklab_to_2020};
    let lin = oklab_to_2020(lch_to_lab([l as f32, c as f32, (h_deg as f32).to_radians()]));
    let s = lcc_color::REC2020.to_space(&lcc_color::SRGB).apply_f32(lin);
    let e = s.map(|v| (lcc_color::transfer::linear_to_srgb(v.clamp(0.0, 1.0)) * 255.0).round() as u8);
    Color32::from_rgb(e[0], e[1], e[2])
}

/// Point Color: swatches of the samples (+ the eyedropper), the selected sample's shifts and range,
/// and "Visualize range".
fn point_color(app: &mut LccApp, ui: &mut egui::Ui, d: &DevelopSettings) {
    let t = Tokens::get(ui.ctx());
    let n = d.point_colors.len();
    if app.ui.point_color >= n && n > 0 {
        app.ui.point_color = n - 1;
    }
    let sel = app.ui.point_color;
    egui::Frame::NONE.inner_margin(crate::widgets::row_margin(ui, egui::Margin { left: 12, right: 16, top: 8, bottom: 4 }, 3, 3)).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 5.0;
            let active = app.ui.tool == "pointColor";
            let full = n >= lcc_develop::MAX_POINT_COLORS;
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
                let swatch = oklch_color(p.lum, p.chroma, p.hue);
                ui.painter().rect_filled(r.shrink(2.0), 3.0, swatch);
                // the sampled colour as RGB, HSL and Lab
                let resp = crate::color_readout::on_picker(resp, [swatch.r(), swatch.g(), swatch.b()]);
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
        egui::Frame::NONE.inner_margin(crate::widgets::row_margin(ui, egui::Margin { left: 12, right: 16, top: 2, bottom: 8 }, 2, 3)).show(
            ui,
            |ui| {
                ui.label(egui::RichText::new(crate::i18n::tr("Pick a colour on the photo with the eyedropper.")).size(12.0).color(t.text_dim));
            },
        );
        return;
    }
    for k in ["hueShift", "satShift", "lumShift", "variance"] {
        control(app, ui, d, &format!("pointColor.{sel}.{k}"), true);
    }
    sub_title(ui, crate::i18n::tr("Range"));
    for k in ["range", "hueRange", "satRange", "lumRange"] {
        control(app, ui, d, &format!("pointColor.{sel}.{k}"), true);
    }
    egui::Frame::NONE.inner_margin(crate::widgets::row_margin(ui, egui::Margin { left: 12, right: 16, top: 4, bottom: 8 }, 3, 3)).show(ui, |ui| {
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

/// The white balance rows: the preset menu and the selector, then Temp and Tint (Kelvin on raws,
/// relative elsewhere). At the top of Light, where Lightroom Classic's Basic panel has them.
fn wb_rows(app: &mut LccApp, ui: &mut egui::Ui, d: &DevelopSettings, raw: bool) {
    let t = Tokens::get(ui.ctx());
    // White balance row
    if shown_here(app, ui, "option:wbSelector") {
        egui::Frame::NONE.inner_margin(crate::widgets::row_margin(ui, egui::Margin { left: 12, right: 16, top: 2, bottom: 2 }, 2, 2)).show(
            ui,
            |ui| {
                ui.horizontal(|ui| {
                    // Lightroom Classic: the selector on the left, then "WB:" and the preset menu
                    let active = app.ui.tool == "wbPicker";
                    // (Compact: the profile row's sizes, so the two rows line up)
                    let dense = t.dense;
                    let icon = if dense { 20.0 } else { 28.0 };
                    if crate::widgets::icon_button(ui, "wbPicker", Icon::Picker, vec2(icon, icon), active, true, "White Balance Selector (W)")
                        .clicked()
                    {
                        if active {
                            app.ui.tool.clear();
                        } else if let Err(e) = app.run("tool.wbPicker", json!({})) {
                            // the same as W: from Compare, Survey or the grid it opens the photo in Detail
                            app.toast(ui.ctx(), e);
                        }
                    }
                    if active {
                        // how many pixels a click averages (#644), as Lightroom's selector toolbar offers
                        let n = app.ui.wb_sample;
                        let r = crate::widgets::dropdown(ui, "wbSample", &format!("{n}×{n}"), t.font(13.0), t.text_label)
                            .on_hover_text(crate::i18n::tr("Sample size: the square of pixels the selector averages"));
                        egui::Popup::menu(&r).show(|ui| {
                            for s in crate::panels::detail::WB_SAMPLES {
                                if ui.selectable_label(n == s, format!("{s}×{s}")).clicked() {
                                    app.ui.wb_sample = s;
                                }
                            }
                        });
                    }
                    ui.label(egui::RichText::new(crate::i18n::tr("WB:")).font(t.font(if dense { 11.0 } else { 13.0 })).color(t.text_dim));
                    let r = crate::widgets::dropdown(
                        ui,
                        "wbMode",
                        crate::i18n::tr(d.wb.mode.label()),
                        t.font(if dense { 12.5 } else { 14.0 }),
                        t.text_label,
                    );
                    egui::Popup::menu(&r).show(|ui| {
                        for m in WbMode::ALL {
                            if m == WbMode::Custom {
                                continue;
                            }
                            if ui.selectable_label(d.wb.mode == m, crate::i18n::tr(m.label())).clicked() {
                                let mode = serde_json::to_value(m).unwrap_or_default();
                                let _ = app.run("develop.wb", json!({"mode": mode}));
                            }
                        }
                    });
                });
            },
        );
    }
    // (the Kelvin scale for a raw, the relative one for a rendered file)
    for c in ["wb.temp", "wb.tint"] {
        if shown_here(app, ui, c) {
            custom_control(app, ui, d, c, raw);
        }
    }
}

/// The HDR button's tooltip; `sdr_window`: the window can't show HDR, so say the screen shows the
/// SDR rendition rather than let an HDR display look like the switch does nothing (issue #47).
fn hdr_button_tip(sdr_window: bool) -> String {
    let tip = crate::i18n::tr("Edit in HDR: highlights above SDR white, for HDR exports and displays");
    if sdr_window { format!("{tip}\n\n{}", crate::i18n::tr(HDR_SDR_WINDOW_NOTE)) } else { tip.to_string() }
}

const HDR_SDR_WINDOW_NOTE: &str = "This display isn't showing HDR (it is SDR, or HDR is off in the system settings), so the window shows the SDR rendition. HDR exports keep the full range.";

const SDR_SLIDERS_NOTE: &str = "These set how the photo looks on SDR displays and in SDR exports. Turn on Preview for SDR Display to see them.";

/// The Light panel's HDR settings while HDR editing is on (the switch itself is the HDR button
/// next to B&W): Visualize HDR range, Preview for SDR Display and the headroom limit.
fn hdr_row(app: &mut LccApp, ui: &mut egui::Ui, d: &DevelopSettings) {
    if !d.hdr.enabled {
        return;
    }
    egui::Frame::NONE.inner_margin(crate::widgets::row_margin(ui, egui::Margin { left: 12, right: 16, top: 4, bottom: 4 }, 3, 3)).show(ui, |ui| {
        let mut v = app.ui.hdr_visualize;
        let r = ui.checkbox(&mut v, crate::i18n::tr("Visualize HDR"));
        register(ui.ctx(), "check:hdrVisualize", r.rect);
        if r.on_hover_text(crate::i18n::tr("Grey below SDR white; yellow, orange, red, magenta up to 1, 2, 3 and over 3 stops above it")).changed() {
            app.ui.hdr_visualize = v;
        }
        // Lightroom Classic's Preview for SDR Display: the photo as SDR displays and SDR exports
        // show it (its SDR rendition) instead of in HDR. The same switch as View ▸ HDR Preview,
        // the other way round; on a window that can't show HDR it is always on.
        let hdr_window = app.hdr_headroom.is_some();
        let mut sdr = !hdr_window || !app.ui.hdr_preview;
        let r = ui.add_enabled(hdr_window, egui::Checkbox::new(&mut sdr, crate::i18n::tr("Preview for SDR Display")));
        register(ui.ctx(), "check:hdrSdrPreview", r.rect);
        let r = r
            .on_hover_text(crate::i18n::tr("Show the photo as SDR displays and SDR exports show it, to adjust its SDR Rendition"))
            .on_disabled_hover_text(crate::i18n::tr(HDR_SDR_WINDOW_NOTE));
        if r.changed()
            && let Err(e) = app.run("view.hdrPreview", json!({"on": !sdr}))
        {
            app.ui.status = e;
        }
    });
    control(app, ui, d, "hdr.maxEv", true);
}

const CONTRAST_MODES: [(lcc_develop::ContrastMode, &str, &str); 2] =
    [(lcc_develop::ContrastMode::Luminance, "Luminance", "luminance"), (lcc_develop::ContrastMode::Rgb, "RGB", "rgb")];

/// The Basic ▸ Tone switch between luminance-only and per-channel RGB Contrast and Blacks (#839).
fn contrast_mode_row(app: &mut LccApp, ui: &mut egui::Ui, d: &DevelopSettings) {
    let t = Tokens::get(ui.ctx());
    let items: [(&str, &str); 2] = CONTRAST_MODES.map(|(_, l, k)| (l, k));
    let active = CONTRAST_MODES.iter().position(|(m, _, _)| *m == d.light.contrast_mode);
    let mut chosen = None;
    egui::Frame::NONE.inner_margin(egui::Margin { left: 12, right: 16, top: 4, bottom: 2 }).show(ui, |ui| {
        let full_w = ui.available_width();
        let (row_rect, _) = ui.allocate_exact_size(egui::vec2(full_w, 24.0), egui::Sense::hover());
        let label_w = 90.0f32.min(full_w * 0.4);
        let label_rect = egui::Rect::from_min_size(row_rect.min, egui::vec2(label_w, row_rect.height()));
        let seg_rect = egui::Rect::from_min_max(
            egui::pos2(row_rect.min.x + label_w + 8.0, row_rect.min.y),
            row_rect.max,
        );
        let resp = ui.interact(label_rect, ui.id().with("contrast_mode_label"), egui::Sense::hover());
        resp.on_hover_text(crate::i18n::tr(
            "Luminance adjusts tonal brightness while keeping linear channel ratios; RGB applies Contrast and Blacks across the Red, Green and Blue channels like an RGB tone curve",
        ));
        ui.painter().text(
            egui::pos2(label_rect.left(), label_rect.center().y),
            egui::Align2::LEFT_CENTER,
            crate::i18n::tr("Contrast Mode"),
            t.font(11.5),
            t.text_dim,
        );
        let mut child_ui = ui.new_child(egui::UiBuilder::new().max_rect(seg_rect));
        chosen = crate::widgets::segmented(&mut child_ui, "contrastMode", &items, active, 2);
    });
    if let Some((mode, _, _)) = chosen.and_then(|i| CONTRAST_MODES.get(i)).copied()
        && mode != d.light.contrast_mode
    {
        let _ = app.run("develop.merge", json!({"settings": {"light": {"contrast_mode": mode}}, "label": "Contrast Mode"}));
    }
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

fn grading(app: &mut LccApp, ui: &mut egui::Ui, d: &DevelopSettings, photo: PhotoId) {
    let mode = GRADING_TABS.iter().find(|(k, ..)| *k == app.ui.grading_mode).map_or("3way", |(k, ..)| *k);
    egui::Frame::NONE.inner_margin(crate::widgets::row_margin(ui, egui::Margin { left: 12, right: 16, top: 6, bottom: 2 }, 3, 2)).show(ui, |ui| {
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
        // under the wheels they name, the luminance sliders need only the wheel's name (issue #346)
        for (c, label) in [("grading.shadows.lum", "Shadows"), ("grading.midtones.lum", "Midtones"), ("grading.highlights.lum", "Highlights")] {
            control_as(app, ui, d, c, true, Some(label));
        }
    } else {
        let i = GRADING_WHEELS.iter().position(|(k, _)| *k == mode).unwrap_or(1);
        let rad = (w * 0.3).clamp(40.0, 80.0);
        let (area, _) = ui.allocate_exact_size(vec2(w, rad * 2.0 + 40.0), Sense::hover());
        grading_wheel(app, ui, d, photo, i, pos2(area.center().x, area.top() + rad + 10.0), rad, false);
        for (part, label) in [("hue", "Hue"), ("sat", "Saturation"), ("lum", "Luminance")] {
            control_as(app, ui, d, &format!("grading.{mode}.{part}"), true, Some(label));
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
fn grading_wheel(app: &mut LccApp, ui: &mut egui::Ui, d: &DevelopSettings, photo: PhotoId, i: usize, c: Pos2, rad: f32, readout: bool) {
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
    let pointer = wheel_pointer(ui, &resp);
    let grab = match pointer {
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
        && let Some(q) = pointer
    {
        let (hue, sat) = grab.map_or_else(|| wheel_pick(q - c, rad, None), |g| g.pick(q - c, rad));
        let _ = app.run("develop.set", json!({"values": {format!("grading.{key}.hue"): hue.round(), format!("grading.{key}.sat"): sat.round()}}));
    }
    if resp.drag_stopped() || resp.clicked() {
        let _ = app.run("develop.endInteraction", json!({}));
    }
    // released, or Esc (egui ends a drag on it)
    if resp.drag_stopped()
        && let Some(held) = ui.ctx().data_mut(|m| m.remove_temp::<WheelCapture>(egui::Id::new(WHEEL_CAPTURE)))
        && !held.dropped
    {
        give_pointer_back(ui.ctx(), held.origin);
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

/// A wheel drag holds the pointer, as in Lightroom (issue #1899): it is hidden for the drag, the
/// puck follows the mouse's own travel, and on release the pointer is back where the button went
/// down, so a long (Alt) drag doesn't leave it far from the wheels. On macOS the pointer is locked
/// in place for the drag; elsewhere it moves unseen and is put back. The browser can't move it
/// (eframe's web host ignores the command), so there it only shows again where the drag ended.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct WheelCapture {
    /// Where the button went down: the pointer goes back there.
    pub(crate) origin: Pos2,
    /// Where the pointer would be had it been free: where the drag began plus the travel since.
    pos: Pos2,
    /// Let go early (the window lost focus): the rest of this drag leaves the wheel alone.
    pub(crate) dropped: bool,
    /// The pass that last drew the dragged wheel.
    pass: u64,
}

/// One wheel is dragged at a time, so one capture for them all.
pub(crate) const WHEEL_CAPTURE: &str = "wheelCapture";

/// The mouse's travel this frame: the pointer's, or, while the pointer stands still (locked, or
/// against a screen edge), the raw motion the host reports.
fn wheel_travel(delta: egui::Vec2, motion: Option<egui::Vec2>) -> egui::Vec2 {
    if delta != egui::Vec2::ZERO {
        return delta;
    }
    motion.filter(|m| m.x.is_finite() && m.y.is_finite()).unwrap_or(egui::Vec2::ZERO)
}

/// Where the wheel `resp` reads the pointer this frame: where it clicked, or for a drag the held
/// pointer ([`WheelCapture`]), `None` once a lost focus let go of it (Esc ends the drag itself:
/// egui stops a drag on Esc).
pub(crate) fn wheel_pointer(ui: &egui::Ui, resp: &egui::Response) -> Option<Pos2> {
    let q = resp.interact_pointer_pos()?;
    if !resp.dragged() {
        return Some(q);
    }
    let ctx = ui.ctx();
    let id = egui::Id::new(WHEEL_CAPTURE);
    let held = ctx.data(|m| m.get_temp::<WheelCapture>(id)).filter(|_| !resp.drag_started());
    let mut cap = held.unwrap_or_else(|| {
        if cfg!(target_os = "macos") {
            ctx.send_viewport_cmd(egui::ViewportCommand::CursorGrab(egui::CursorGrab::Locked));
        }
        WheelCapture { origin: ui.input(|i| i.pointer.press_origin()).unwrap_or(q), pos: q, ..Default::default() }
    });
    cap.pass = ctx.cumulative_pass_nr();
    if !cap.dropped {
        let (travel, unfocused) = ui.input(|i| {
            // the event, not `focused`: a click that activates the window may land before its focus
            let unfocused = i.events.iter().any(|e| matches!(e, egui::Event::WindowFocused(false)));
            (wheel_travel(i.pointer.delta(), i.pointer.motion()), unfocused)
        });
        if !resp.drag_started() {
            cap.pos += travel;
        }
        if unfocused {
            give_pointer_back(ctx, cap.origin);
            cap.dropped = true;
        } else {
            ctx.set_cursor_icon(egui::CursorIcon::None);
        }
    }
    ctx.data_mut(|m| m.insert_temp(id, cap));
    (!cap.dropped).then_some(cap.pos)
}

/// Unlock and show the pointer a wheel drag held, back at `origin`.
pub(crate) fn give_pointer_back(ctx: &egui::Context, origin: Pos2) {
    if cfg!(target_os = "macos") {
        ctx.send_viewport_cmd(egui::ViewportCommand::CursorGrab(egui::CursorGrab::None));
    }
    if cfg!(not(target_arch = "wasm32")) {
        ctx.send_viewport_cmd(egui::ViewportCommand::CursorPosition(origin));
    }
}

/// Once a frame, after the panels: a held pointer stays hidden over whatever set its own cursor,
/// and is given back if no wheel drew this pass (the flyout closed or the view changed under the
/// drag), so it is never left locked or hidden.
pub(crate) fn wheel_pointer_frame_end(ctx: &egui::Context) {
    let id = egui::Id::new(WHEEL_CAPTURE);
    let Some(cap) = ctx.data(|m| m.get_temp::<WheelCapture>(id)) else { return };
    if cap.pass != ctx.cumulative_pass_nr() {
        ctx.data_mut(|m| m.remove::<WheelCapture>(id));
        if !cap.dropped {
            give_pointer_back(ctx, cap.origin);
        }
    } else if !cap.dropped {
        ctx.set_cursor_icon(egui::CursorIcon::None);
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

pub(crate) fn paint_wheel(p: &egui::Painter, c: Pos2, rad: f32) {
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
        let rgb = lcc_color::perceptual::hsv_to_rgb(hue_deg, t, CENTRE_V + (1.0 - CENTRE_V) * t);
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
fn quick_develop(app: &mut LccApp, ui: &mut egui::Ui, n: usize) {
    let t = Tokens::get(ui.ctx());
    egui::Frame::NONE.inner_margin(crate::widgets::row_margin(ui, egui::Margin { left: 12, right: 16, top: 0, bottom: 10 }, 0, 3)).show(ui, |ui| {
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

/// What Enable Lens Corrections applies to photo `id` (issue #307): the database profile it uses (matched by name or
/// chosen), or the correction its file carries; else why nothing applies: no lens database (a link that opens
/// Settings ▸ Lens profiles), no lens named in its metadata, or none of the database's profiles fits its lens.
/// `has_lens`: the photo carries a correction (its file's, or one found in the database).
fn lens_note(app: &mut LccApp, ui: &mut egui::Ui, id: lcc_catalog::PhotoId, has_lens: bool) {
    use crate::i18n::tr;
    use lcc_engine::lens_profiles::Match;
    let t = Tokens::get(ui.ctx());
    let Some(photo) = app.session.catalog.photo(id) else { return };
    let applied = match app.session.lens_match(photo) {
        None if has_lens => Some(tr("Correction from the file").to_string()),
        Some(Match::Found(name)) => Some(tr("Profile: {name}").replace("{name}", &name)),
        Some(Match::Chosen(name)) => Some(tr("Profile: {name} (chosen)").replace("{name}", &name)),
        _ => None,
    };
    if let Some(text) = applied {
        let r = ui.label(egui::RichText::new(text).size(11.0).color(t.text_dim));
        crate::widgets::register(ui.ctx(), "label:lensStatus", r.rect);
        return;
    }
    if has_lens {
        return;
    }
    let Some(state) = app.session.catalog.photo(id).and_then(|p| app.session.lens_match(p)) else { return };
    let text = match state {
        Match::NoDatabase => {
            let link = egui::RichText::new(tr("No lens database. Set one up or download it…")).size(11.0).color(t.accent).underline();
            let r = ui.add(egui::Label::new(link).sense(egui::Sense::click()));
            crate::widgets::register(ui.ctx(), "link:lensDbSetup", r.rect);
            if r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                let _ = app.run("app.settings", serde_json::json!({"section": "lensProfiles"}));
            }
            return;
        }
        Match::NotRaw => tr("Lens profiles apply to raw photos only.").to_string(),
        Match::NoLensName => tr("No lens named in this photo's metadata.").to_string(),
        Match::NotFound(lens) => tr("No profile for {lens} in the lens database.").replace("{lens}", &lens),
        // found: the photo picks it up when it is looked up again
        Match::NoFocal(lens) => tr("No focal length for {lens}: the photo has none and none was given.").replace("{lens}", &lens),
        Match::Found(_) | Match::Chosen(_) => return,
    };
    let r = ui.label(egui::RichText::new(text).size(11.0).color(t.text_dim));
    crate::widgets::register(ui.ctx(), "label:lensNote", r.rect);
}

/// What the flat-field panel's answers are kept for: the photo's settings, and how many raws' placements the
/// background reader has found so far (`lcc_engine::vignetting_calibration_placement`): a match waiting for the photo's header
/// is asked again once it is read.
fn vignetting_calibration_key(app: &LccApp, d: &DevelopSettings) -> u64 {
    // (and the catalog's maps and which are withdrawn: an undo of Create or of a withdraw changes no settings; maps are
    // few)
    let maps = app.session.catalog.vignetting_maps().fold(0u64, |acc, (id, r)| {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (id.as_str(), r.withdrawn).hash(&mut h);
        acc ^ h.finish()
    });
    d.hash64() ^ app.session.vignetting_calibration_placements.generation().wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ maps.rotate_left(17)
}

/// `vignettingCalibration.photo` for the photo as its settings are now, read once per change of the settings (or when a raw's
/// placement read in the background became known: [`vignetting_calibration_key`]).
fn vignetting_calibration_info(app: &mut LccApp, id: PhotoId, d: &DevelopSettings) -> Value {
    let hash = vignetting_calibration_key(app, d);
    if let Some((pid, h, v)) = &app.ui.vignetting_calibration_info
        && (*pid, *h) == (id, hash)
    {
        return v.clone();
    }
    let v = app.run("vignettingCalibration.photo", json!({"id": id.0})).unwrap_or(Value::Null);
    app.ui.vignetting_calibration_info = Some((id, hash, v.clone()));
    v
}

/// One line for a map's reference shot: lens, focal length and aperture (numbers stay as they are).
fn vignetting_calibration_source_line(m: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(l) = m["lens"].as_str().filter(|l| !l.trim().is_empty()) {
        parts.push(l.trim().to_string());
    }
    if let Some(f) = m["focal"].as_f64() {
        parts.push(format!("{} mm", f.round()));
    }
    if let Some(a) = m["aperture"].as_f64() {
        parts.push(format!("f/{a:.1}"));
    }
    if let Some(b) = m["model"].as_str().filter(|b| !b.trim().is_empty()) {
        parts.push(b.trim().to_string());
    }
    parts.join(", ")
}

/// Why a map doesn't apply to the photo (`vignettingCalibration.photo`'s match), as one sentence.
fn vignetting_calibration_refusal(r: &Value) -> Option<String> {
    use crate::i18n::tr;
    Some(match r["refused"].as_str()? {
        "imageArea" => tr("This calibration doesn't fit this photo's image area").to_string(),
        "anotherCamera" => tr("Made with another camera").to_string(),
        "anotherLens" => tr("Made with another lens").to_string(),
        "focalLength" => tr("Made at another focal length").to_string(),
        "aperture" => tr("Made at another aperture").to_string(),
        "apertureUnknown" => tr("Only one of the photo and the reference records its aperture").to_string(),
        _ => return None,
    })
}

/// What differs between a map and the photo, as one short, quiet line (`vignettingCalibration.photo`): "+2 mm ·
/// unevenly lit reference".
fn vignetting_calibration_note(r: &Value) -> Option<String> {
    use crate::i18n::tr;
    let parts: Vec<String> = r["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|w| {
            Some(match w["kind"].as_str()? {
                "focalLength" => format!("{:+} mm", w["diff"].as_f64().unwrap_or(0.0)),
                "lensUnknown" => tr("lens not recorded, can't be checked").to_string(),
                "apertureUnknown" => tr("aperture not recorded, can't be checked").to_string(),
                "uneven" => tr("unevenly lit reference").to_string(),
                _ => return None,
            })
        })
        .collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// What the last Create measured on its reference, in camera terms, and a note when it should be shot again.
fn vignetting_calibration_exposure_lines(report: &Value) -> Vec<String> {
    use crate::i18n::tr;
    let e = &report["exposure"];
    let pct = |v: &Value| format!("{:.0}", v.as_f64().unwrap_or(0.0) * 100.0);
    let mut out = vec![
        tr("Reference: centre at {centre} % of white, darkest corner at {corner} %")
            .replace("{centre}", &pct(&e["centre"][1]))
            .replace("{corner}", &pct(&e["darkest"])),
    ];
    match e["verdict"].as_str() {
        Some("under") => out.push(tr("Underexposed: aim the histogram peak at 2/3 to 3/4 of its width").to_string()),
        Some("nearClip") => out.push(tr("Close to clipping: expose a little less").to_string()),
        _ => {}
    }
    if report["keptForOtherAreas"].as_array().is_some_and(|a| !a.is_empty()) {
        out.push(tr("References for other image areas with this lens and aperture stay in use for those shots").to_string());
    }
    if report["vr"].as_bool() == Some(true) {
        out.push(tr("VR was on: turn it off for references for the most precise result").to_string());
    }
    out
}

/// Create: measure photo `id`'s raw on a worker thread (the decode takes a second or more: the window keeps drawing,
/// the activity stack shows it), then keep and apply the map here, say "Vignetting calibration created" and keep what
/// was measured for the panel's exposure lines. (A command run by an agent or the CLI measures in place:
/// `vignettingCalibration.create`.)
fn start_vignetting_calibration_create(app: &mut LccApp, ctx: &egui::Context, id: PhotoId) {
    use crate::i18n::tr;
    let (path, file_name, captured) = match lcc_engine::cmd::vignetting_calibration::reference_file(&app.session, id) {
        Ok(f) => f,
        Err(e) => return app.toast_error(ctx, e.to_string()),
    };
    use lcc_engine::cmd::vignetting_calibration::{MeasureError, Measured};
    app.ui.vignetting_calibration_creating = Some(id);
    app.ui.vignetting_calibration_pending = None;
    let work = move || lcc_engine::cmd::vignetting_calibration::measure(&path, &file_name, captured);
    let done = move |app: &mut LccApp, ctx: &egui::Context, measured: Result<Measured, MeasureError>| {
        app.ui.vignetting_calibration_creating = None;
        app.ui.vignetting_calibration_info = None;
        match measured {
            // clearly not a flat field: refused, with the reason
            Err(MeasureError::Implausible(p)) => {
                app.toast_error(ctx, format!("{}. {}", tr("This photo doesn't look like a flat-field reference"), plausibility_reasons(&p.reasons)));
            }
            Err(MeasureError::Other(e)) => app.toast_error(ctx, e),
            Ok(m) => {
                if let Err(e) = continue_vignetting_calibration(app, id, m, false) {
                    app.toast_error(ctx, e);
                }
            }
        }
    };
    if let Err(e) = crate::tasks::spawn(app, tr("Measuring the vignetting reference"), Some("vignettingCalibration"), work, done) {
        app.ui.vignetting_calibration_creating = None;
        app.toast_error(ctx, e);
    }
}

/// What Create does with a measured reference, step by step: a Borderline one asks first (Create Anyway; `anyway`:
/// asked already), then one for a combination with a reference in use asks whether to replace it, then it is kept and
/// applied. While a question is open the measured map waits in [`crate::state::UiState::vignetting_calibration_pending`]
/// (answering doesn't measure again). `Ok(Null)` while asking.
pub(crate) fn continue_vignetting_calibration(
    app: &mut LccApp,
    id: PhotoId,
    m: lcc_engine::cmd::vignetting_calibration::Measured,
    anyway: bool,
) -> Result<Value, String> {
    use lcc_engine::cmd::vignetting_calibration as vc;
    if m.plausibility.verdict == lcc_raw::vignetting_plausibility::Verdict::Borderline && !anyway {
        let reasons = plausibility_reasons(&m.plausibility.reasons);
        app.ui.vignetting_calibration_pending = Some((id, Box::new(m)));
        app.ui.dialog = Some(crate::state::Dialog::VignettingCreateAnyway { photo: id.0, reasons });
        return Ok(Value::Null);
    }
    if let Some(old) = vc::conflicts(&app.session, &m).first() {
        let mut question = replace_question(&old.map);
        // (a new full-frame reference replacing a crop one: nothing is lost, say so)
        let lib = &lcc_engine::vignetting_calibration_library::coverage;
        if lib(&old.map) != lib(&m.map) {
            question = format!("{question} {}", crate::i18n::tr("The new reference covers its image area too."));
        }
        app.ui.vignetting_calibration_pending = Some((id, Box::new(m)));
        app.ui.dialog = Some(crate::state::Dialog::VignettingReplace { photo: id.0, question });
        return Ok(Value::Null);
    }
    commit_vignetting_calibration(app, id, m, false)
}

/// "There's already a reference for NIKKOR Z 50mm f/1.8 S at f/1.8 on NIKON Z 8 (from SHOT_0001.NEF, 2026-01-28).
/// Replace it?"
fn replace_question(old: &lcc_develop::EmbeddedVignettingMap) -> String {
    use crate::i18n::tr;
    let s = &old.source;
    let mut lens = s.lens.clone().filter(|l| !l.trim().is_empty()).unwrap_or_else(|| tr("this lens").to_string());
    if let (true, Some(f)) = (s.lens.as_deref().is_some_and(lcc_engine::vignetting_calibration_library::is_zoom), s.focal_length) {
        lens = format!("{lens} ({} mm)", f.round());
    }
    let aperture = s.aperture.map_or_else(|| "?".to_string(), |a| format!("f/{}", lcc_engine::cmd::vignetting_calibration::aperture_text(a)));
    tr("There's already a reference for {lens} at {aperture} on {camera} (from {file}, {date}). Replace it?")
        .replace("{lens}", &lens)
        .replace("{aperture}", &aperture)
        .replace("{camera}", s.model.as_deref().unwrap_or(""))
        .replace("{file}", s.file_name.as_deref().unwrap_or(""))
        .replace("{date}", s.captured.as_deref().map(|c| c.get(..10).unwrap_or(c)).unwrap_or(""))
}

/// Keep and apply a measured reference (`replace`: the one in use for its combination is withdrawn in the same step)
/// and say so.
fn commit_vignetting_calibration(
    app: &mut LccApp,
    id: PhotoId,
    m: lcc_engine::cmd::vignetting_calibration::Measured,
    replace: bool,
) -> Result<Value, String> {
    // (a Borderline one gets here only after Create Anyway)
    let report = lcc_engine::cmd::vignetting_calibration::commit_measured(&mut app.session, id, m, true, replace).map_err(|e| e.to_string())?;
    app.ui.vignetting_calibration_info = None;
    app.ui.vignetting_calibration_report = Some((id, report.clone()));
    if let Some(ctx) = app.tasks.repaint.clone() {
        app.toast(&ctx, crate::i18n::tr("Vignetting calibration created"));
    }
    Ok(report)
}

/// The measured reference waiting for `photo`'s question.
fn take_pending(app: &mut LccApp, photo: u64) -> Result<(PhotoId, lcc_engine::cmd::vignetting_calibration::Measured), String> {
    app.ui
        .vignetting_calibration_pending
        .take()
        .filter(|(id, _)| id.0 == photo)
        .map(|(id, m)| (id, *m))
        .ok_or_else(|| "the measured reference is gone; Create again".into())
}

/// Create Anyway in [`crate::state::Dialog::VignettingCreateAnyway`]: go on with the reference measured meanwhile.
pub(crate) fn create_vignetting_calibration_anyway(app: &mut LccApp, photo: u64) -> Result<Value, String> {
    let (id, m) = take_pending(app, photo)?;
    continue_vignetting_calibration(app, id, m, true)
}

/// Replace in [`crate::state::Dialog::VignettingReplace`]: keep the new reference, withdraw the old one (one undo
/// step; photos that use it keep it).
pub(crate) fn replace_vignetting_calibration(app: &mut LccApp, photo: u64) -> Result<Value, String> {
    let (id, m) = take_pending(app, photo)?;
    commit_vignetting_calibration(app, id, m, true)
}

/// Why a raw doesn't look like a flat-field reference, as sentences.
fn plausibility_reasons(reasons: &[lcc_raw::vignetting_plausibility::Reason]) -> String {
    use crate::i18n::tr;
    use lcc_raw::vignetting_plausibility::Reason;
    let text: Vec<&str> = reasons
        .iter()
        .map(|r| match r {
            Reason::Detail => tr("It has detail in it: a flat-field reference shows only an even surface"),
            Reason::Colour => tr("Its colours aren't even"),
            Reason::Shape => tr("It doesn't get darker evenly from the middle toward the edges"),
            Reason::NoFalloff => tr("It doesn't get darker toward the edges"),
            Reason::Dark => tr("It is too dark"),
            Reason::Clipped => tr("Parts of it are clipped"),
        })
        .collect();
    format!("{}.", text.join(". "))
}

/// Develop ▸ Lens Corrections ▸ Vignetting Calibration (design notes §10, *UI*). Two buttons: Create (measure this raw
/// as a reference) and Apply (each photo of the selection, or this one, gets the reference that fits it, picked by
/// itself: `vignettingCalibration.apply` without a map). With a calibration: which reference it is, with Show
/// Reference, what differs, the Amount and Remove.
fn vignetting_calibration_section(app: &mut LccApp, ui: &mut egui::Ui, id: PhotoId, d: &DevelopSettings) {
    use crate::i18n::tr;
    let t = Tokens::get(ui.ctx());
    sub_title(ui, tr("Vignetting Calibration"));
    let info = vignetting_calibration_info(app, id, d);
    let note = |ui: &mut egui::Ui, text: &str, key: &str| {
        let r = ui.label(egui::RichText::new(text).size(11.0).color(t.text_dim));
        register(ui.ctx(), key, r.rect);
    };
    let mut run: Option<(&str, Value)> = None;
    let map = &info["map"];
    let has_map = !map.is_null();
    let missing = info["missing"].as_bool() == Some(true);
    egui::Frame::NONE.inner_margin(crate::widgets::row_margin(ui, egui::Margin { left: 12, right: 16, top: 2, bottom: 4 }, 2, 3)).show(ui, |ui| {
        let raw = info["raw"].as_bool().unwrap_or(false);
        let measuring = app.ui.vignetting_calibration_creating.is_some();
        let selected = app.session.selection.ids.len();
        let auto = &info["auto"];
        let can_apply = selected > 1 || auto["id"].is_string();
        ui.horizontal_wrapped(|ui| {
            let r = ui.add_enabled_ui(raw && !measuring, |ui| text_button(ui, "vignettingCalibrationCreate", "Create", false)).inner;
            register(ui.ctx(), "button:vignettingCalibrationCreate", r.rect);
            let r = if raw {
                r.on_hover_text(tr("Measure this photo as a reference: an even screen or a diffuser, shot with the lens and aperture of the photos"))
            } else {
                r.on_disabled_hover_text(tr("Vignetting calibrations are made from raw photos"))
            };
            if r.clicked() {
                start_vignetting_calibration_create(app, ui.ctx(), id);
            }
            let r = ui.add_enabled_ui(can_apply, |ui| text_button(ui, "vignettingCalibrationApply", "Apply", false)).inner;
            register(ui.ctx(), "button:vignettingCalibrationApply", r.rect);
            let r = if !can_apply {
                // (there is a reference for the lens, only not at this aperture: say which aperture is missing)
                let text = match (auto["none"].as_str(), auto["aperture"].as_f64()) {
                    (Some("noReferenceAtAperture"), Some(a)) => tr("No reference at {aperture} for this lens yet")
                        .replace("{aperture}", &format!("f/{}", lcc_engine::cmd::vignetting_calibration::aperture_text(a))),
                    _ => tr("No reference for this camera and lens yet").to_string(),
                };
                r.on_disabled_hover_text(text)
            } else if selected > 1 {
                r.on_hover_text(tr("Each of the {n} selected photos gets the reference that fits it").replace("{n}", &selected.to_string()))
            } else {
                r.on_hover_text(tr("Apply the reference that fits this photo"))
            };
            if r.clicked() {
                // (each photo's own reference; the selection when there is one, else this photo)
                let ids: Vec<u64> = if selected > 1 { app.session.selection.ids.iter().map(|p| p.0).collect() } else { vec![id.0] };
                run = Some(("vignettingCalibration.apply", json!({"ids": ids})));
            }
            if has_map {
                let r = text_button(ui, "vignettingCalibrationRemove", "Remove", false);
                register(ui.ctx(), "button:vignettingCalibrationRemove", r.rect);
                if r.clicked() {
                    run = Some(("vignettingCalibration.remove", json!({"ids": [id.0]})));
                }
            }
        });
        if app.ui.vignetting_calibration_creating == Some(id) {
            note(ui, tr("Measuring the vignetting reference…"), "label:vignettingCalibrationMeasuring");
        }
        // this photo as a reference: each map measured from it and in use, with Stop Using as Reference (one undo
        // step). A withdrawn one shows nothing: the photo is a normal photo again, and Create makes it a reference again
        // (the same map, put back in use).
        let active = info["referenceOf"].as_array().into_iter().flatten().filter(|m| m["withdrawn"].as_bool() != Some(true));
        for (k, m) in active.enumerate() {
            let line = tr("This photo is a vignetting reference ({reference})").replace("{reference}", &vignetting_calibration_source_line(m));
            note(ui, &line, &format!("label:vignettingCalibrationIsReference{k}"));
            let r = text_button(ui, &format!("vignettingCalibrationWithdraw{k}"), "Stop Using as Reference", false)
                .on_hover_text(tr("Apply no longer picks this reference; photos that use it keep it"));
            register(ui.ctx(), format!("button:vignettingCalibrationWithdraw{k}"), r.rect);
            if r.clicked() {
                run = Some(("vignettingCalibration.withdraw", json!({"map": m["id"]})));
            }
        }
        if !has_map {
            if auto["id"].is_string() {
                note(
                    ui,
                    &tr("Apply uses {reference}").replace("{reference}", &vignetting_calibration_source_line(auto)),
                    "label:vignettingCalibrationAuto",
                );
            }
            return;
        }
        if missing {
            note(ui, tr("This photo's vignetting map is missing"), "label:vignettingCalibrationMissing");
            return;
        }
        let r = ui.label(egui::RichText::new(vignetting_calibration_source_line(map)).size(11.5));
        register(ui.ctx(), "label:vignettingCalibrationSource", r.rect);
        let file = map["fileName"].as_str().unwrap_or("");
        let date = map["captured"].as_str().map(|c| c.get(..10).unwrap_or(c)).unwrap_or("");
        if info["reference"]["inCatalog"].as_bool() == Some(true) {
            ui.horizontal_wrapped(|ui| {
                note(ui, &tr("From {file}, {date}").replace("{file}", file).replace("{date}", date), "label:vignettingCalibrationReference");
                let link = egui::RichText::new(tr("Show Reference")).size(11.0).color(t.accent).underline();
                let r = ui.add(egui::Label::new(link).sense(egui::Sense::click()));
                register(ui.ctx(), "link:vignettingCalibrationShowReference", r.rect);
                if r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    run = Some(("vignettingCalibration.showReference", json!({"id": id.0})));
                }
            });
        } else {
            note(ui, &tr("From {file}, {date}").replace("{file}", file).replace("{date}", date), "label:vignettingCalibrationReference");
            note(
                ui,
                tr("The reference photo is no longer in this catalog; the correction still applies"),
                "label:vignettingCalibrationReferenceGone",
            );
        }
        if let Some(n) = vignetting_calibration_refusal(&info["match"]) {
            note(ui, &n, "label:vignettingCalibrationRefused");
        }
        if let Some(n) = vignetting_calibration_note(&info["match"]) {
            note(ui, &n, "label:vignettingCalibrationNote");
        }
        if let Some((_, report)) = app.ui.vignetting_calibration_report.as_ref().filter(|(p, r)| *p == id && r["map"]["id"] == map["id"]) {
            for (k, line) in vignetting_calibration_exposure_lines(report).iter().enumerate() {
                note(ui, line, &format!("label:vignettingCalibrationExposure{k}"));
            }
        }
        if info["profileVignettingReplaced"].as_bool() == Some(true) {
            note(ui, tr("Profile vignetting is replaced by the vignetting calibration"), "label:vignettingCalibrationReplacesProfile");
        }
    });
    if has_map {
        control_as(app, ui, d, "optics.vignettingCalibrationAmount", !missing, Some("Amount"));
    }
    if let Some((c, p)) = run {
        app.ui.vignetting_calibration_info = None;
        if let Err(e) = app.run(c, p) {
            app.toast_error(ui.ctx(), e);
        }
    }
}

/// Develop ▸ Lens Corrections' lens picker (issue #307): Make → Model dropdowns filled from the lens database
/// (`lens.choose`; "Auto" goes back to the file's own correction or the match by name), and a focal-length field
/// for a photo that records none (a manual lens). Raws only, with a lens database set up.
fn lens_picker(app: &mut LccApp, ui: &mut egui::Ui, id: lcc_catalog::PhotoId, d: &lcc_develop::DevelopSettings) {
    use crate::i18n::tr;
    let Some(photo) = app.session.catalog.photo(id) else { return };
    if !photo.develops_raw() {
        return;
    }
    let photo_focal = photo.meta.focal_mm.is_some_and(|f| f.is_finite() && f > 0.0);
    let Some(db) = lcc_engine::lens_profiles::database() else { return };
    let picked = d.optics.lens.clone();
    let t = Tokens::get(ui.ctx());
    let make = picked.as_ref().and_then(|l| l.make.clone()).or_else(|| app.ui.lens_pick_make.clone());
    let auto = tr("Auto").to_string();
    let mut action: Option<serde_json::Value> = None;
    egui::Grid::new("lens-picker").num_columns(2).spacing([6.0, 4.0]).show(ui, |ui| {
        ui.label(egui::RichText::new(tr("Make")).size(11.5).color(t.text_dim));
        let r = crate::widgets::combo_box("lens-make").width(150.0).selected_text(make.clone().unwrap_or_else(|| auto.clone())).show_ui(ui, |ui| {
            if ui.selectable_label(picked.is_none() && make.is_none(), &auto).clicked() {
                app.ui.lens_pick_make = None;
                action = Some(serde_json::Value::Null);
            }
            let mut makers: Vec<&str> = db.lenses.iter().map(|l| l.maker.trim()).filter(|m| !m.is_empty()).collect();
            makers.sort_unstable_by_key(|m| m.to_lowercase());
            makers.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
            for m in makers {
                if ui.selectable_label(make.as_deref().is_some_and(|c| c.eq_ignore_ascii_case(m)), m).clicked() {
                    app.ui.lens_pick_make = Some(m.to_string());
                }
            }
        });
        crate::widgets::register(ui.ctx(), "combo:lensMake", r.response.rect);
        ui.end_row();
        ui.label(egui::RichText::new(tr("Model")).size(11.5).color(t.text_dim));
        let model_text = picked.as_ref().map(|l| l.model.clone()).unwrap_or_else(|| auto.clone());
        let r = ui
            .add_enabled_ui(make.is_some(), |ui| {
                crate::widgets::combo_box("lens-model").width(150.0).selected_text(model_text).show_ui(ui, |ui| {
                    let Some(mk) = make.as_deref() else { return };
                    let mut models: Vec<&str> = db
                        .lenses
                        .iter()
                        .filter(|l| l.maker.trim().eq_ignore_ascii_case(mk))
                        .filter_map(|l| l.models.first().map(|m| m.trim()))
                        .collect();
                    models.sort_unstable_by_key(|m| m.to_lowercase());
                    models.dedup();
                    for m in models {
                        let on = picked.as_ref().is_some_and(|l| l.model.eq_ignore_ascii_case(m));
                        if ui.selectable_label(on, m).clicked() {
                            let focal = picked.as_ref().and_then(|l| l.focal);
                            action = Some(json!({"maker": mk, "model": m, "focal": focal}));
                        }
                    }
                })
            })
            .inner;
        crate::widgets::register(ui.ctx(), "combo:lensModel", r.response.rect);
        ui.end_row();
        // a manual lens records no focal length: the picked profile needs one
        if let Some(l) = picked.as_ref().filter(|_| !photo_focal) {
            ui.label(egui::RichText::new(tr("Focal length")).size(11.5).color(t.text_dim));
            let mut f = l.focal.unwrap_or(50.0);
            let r = ui.add(egui::DragValue::new(&mut f).range(1.0..=2000.0).speed(1.0).suffix(" mm"));
            crate::widgets::register(ui.ctx(), "drag:lensFocal", r.rect);
            if (r.changed() && !r.dragged()) || r.drag_stopped() || (l.focal.is_none() && r.changed()) {
                action = Some(json!({"maker": l.make, "model": l.model, "focal": f}));
            }
            ui.end_row();
        }
    });
    if let Some(lens) = action
        && let Err(e) = app.run("lens.choose", json!({ "lens": lens }))
    {
        app.toast_error(ui.ctx(), e);
    }
}

/// The preview-only notice's Reload from Disk: re-read the photo's file (a raw a newer version can
/// decode, whose drive was offline when the library opened) in the background; a toast says what
/// came of it once it's done (`crate::reload`).
pub(crate) fn reload_preview_only(app: &mut LccApp, ctx: &egui::Context, id: PhotoId) {
    match app.run("photo.reload", json!({"ids": [id.0]})) {
        Err(e) => app.toast_error(ctx, e),
        Ok(r) if r["background"] == true => {}
        // nothing to read in the background (a photo without a file): answered in place
        Ok(_) if app.session.catalog.photo(id).is_some_and(|p| p.preview_only.is_none()) => {
            app.toast(ctx, crate::i18n::tr("This raw is now developed from its raw data"))
        }
        Ok(_) => app.toast(ctx, crate::i18n::tr("This raw variant still can't be decoded")),
    }
}

/// The Reset button's tooltip with the reset shortcut as this platform writes it (issue #1751).
pub(crate) fn reset_tip(keymap: &crate::shortcuts::Keymap, mac: bool) -> String {
    let base = crate::i18n::tr("Reset all edits");
    match crate::shortcuts::shortcut_of(keymap, "develop.reset") {
        Some(sc) => format!("{base} ({})", crate::menubar::shortcut_text(sc, mac)),
        None => base.to_string(),
    }
}

/// How long the pointer rests on a histogram clipping indicator before its overlay is previewed
/// on the photo (see [`crate::LccApp::clipping_hover`]).
pub(crate) const CLIPPING_PREVIEW_REST_S: f64 = 0.3;

/// Whether a clipping indicator hovered since `since` has been rested on long enough at `now` (a
/// clock that went back counts as no rest).
pub(crate) fn clipping_preview_due(since: f64, now: f64) -> bool {
    let rested = now - since;
    rested.is_finite() && rested >= CLIPPING_PREVIEW_REST_S
}

#[cfg(test)]
mod curve_histogram_tests {
    use super::*;

    #[test]
    fn curve_backdrop_tracks_luma_and_selected_channel() {
        let img = lcc_raster::Rgba8::from_fn(64, 32, |x, _| if x < 32 { [32, 100, 32, 255] } else { [224, 100, 224, 255] });
        let h = lcc_raster::Histogram::of_srgb8(&img);
        let red = curve_histogram_profile(&h, "red");
        let green = curve_histogram_profile(&h, "green");
        let luma = curve_histogram_profile(&h, "master");
        assert!(red[32] > red[150] && red[224] > red[150]);
        assert!(green[100] > green[32] && green[100] > green[224]);
        assert!(luma.iter().any(|v| *v > 0.0));
    }

    #[test]
    fn hdr_backdrop_uses_only_the_sdr_bins_and_never_overflows() {
        let h = lcc_raster::Histogram::of_hdr(&[[0.1, 0.1, 0.1], [1.0, 1.0, 1.0], [8.0, 8.0, 8.0]], 4.0);
        let values = curve_histogram_profile(&h, "parametric");
        assert!(values.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)));
        assert!(values.iter().any(|v| *v > 0.0));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_point_is_not_added_on_top_of_another() {
        use super::{POINT_GAP, with_point};
        use lcc_geom::Point;
        let pts = [Point::new(0.0, 0.0), Point::new(0.5, 0.5), Point::new(1.0, 1.0)];
        assert!(with_point(&pts, 0.5 + POINT_GAP / 2.0, 0.2).is_none());
        assert!(with_point(&pts, 0.0, 0.2).is_none());
        let (i, v) = with_point(&pts, 0.6, 0.2).expect("clear of the others");
        assert_eq!((i, v.len()), (2, 4));
        assert!(v.windows(2).all(|w| w[1].x - w[0].x >= POINT_GAP));
    }

    use super::*;

    /// The Profile Manager's hidden camera profiles are left out of the Camera Profiles submenu too, except the
    /// photo's own.
    #[test]
    fn the_camera_profiles_submenu_leaves_out_hidden_profiles() {
        let ids = |v: Vec<(String, String)>| v.into_iter().map(|(o, _)| o).collect::<Vec<_>>();
        let all = ids(camera_profile_options("Sony ILCE-9"));
        assert!(all.iter().any(|o| o == "lc"), "built-in colour is offered: {all:?}");
        let hidden = vec!["lc".to_string()];
        let shown = ids(shown_camera_profile_options(&hidden, "Sony ILCE-9", ""));
        assert!(!shown.iter().any(|o| o == "lc"), "a hidden profile is left out: {shown:?}");
        assert!(shown.iter().any(|o| o.is_empty()), "the camera default stays: {shown:?}");
        let own = ids(shown_camera_profile_options(&hidden, "Sony ILCE-9", "lc"));
        assert!(own.iter().any(|o| o == "lc"), "the photo's own stays: {own:?}");
    }

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

    /// Issue #277: after HDR is turned off, a stale HDR histogram (a drag's live render from before)
    /// must not bring back the taller HDR layout while a slider moves.
    #[test]
    fn an_sdr_edit_never_shows_an_hdr_histogram() {
        let ctx = egui::Context::default();
        let id = PhotoId(7);
        let stale = Histogram::of_hdr(&[[3.0, 3.0, 3.0]], 5.0);
        assert!(steady_hdr_histogram(&ctx, id, false, Some(stale)).is_some_and(|h| h.hdr.is_none()));
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
