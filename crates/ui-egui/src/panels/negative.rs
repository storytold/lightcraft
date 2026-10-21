//! The Convert Negative panel (Photo ▸ Convert Negative): a scanned film negative as a positive.
//!
//! Simple shows the film preset and six sliders. Advanced adds the film base and a gray point
//! picked from the scan with the selector (the `negBase` / `negGray` canvas tools, which run
//! `develop.negativePick`), the tone profile, white balance, colour look and the tone sliders. The
//! sliders are develop controls (`negative.*`); everything else goes through `develop.negative`.

use egui::{Color32, Sense, vec2};
use lightcraft_catalog::PhotoId;
use lightcraft_pipeline::negative::film::PRESETS;
use serde_json::{Value, json};

use super::edit::control;
use super::right::header;
use crate::LightcraftApp;
use crate::icons::Icon;
use crate::theme::Tokens;
use crate::widgets::{divider, register, text_button};

/// The Simple panel's sliders.
const SIMPLE: [&str; 6] = ["negative.exposure", "negative.brightness", "negative.contrast", "negative.temp", "negative.tint", "negative.saturation"];

/// The tone sliders Advanced adds.
const ADVANCED: [&str; 7] =
    ["negative.highlights", "negative.shadows", "negative.whites", "negative.blacks", "negative.fade", "negative.glow", "negative.range"];

/// Menu labels of the tone profiles, colour looks and white-balance presets, by the film model's
/// names.
const TONES: [(&str, &str); 13] = [
    ("lab_standard", "Lab Standard"),
    ("lab_soft", "Lab Soft"),
    ("lab_hard", "Lab Hard"),
    ("lab_highlight_soft", "Lab Highlight Soft"),
    ("lab_highlight_hard", "Lab Highlight Hard"),
    ("lab_shadow_soft", "Lab Shadow Soft"),
    ("lab_shadow_hard", "Lab Shadow Hard"),
    ("linear", "Linear"),
    ("linear_deep", "Linear Deep"),
    ("linear_flat", "Linear Flat"),
    ("cinematic", "Cinematic"),
    ("cinematic_flat", "Cinematic Flat"),
    ("cinematic_rich", "Cinematic Rich"),
];
const LOOKS: [(&str, &str); 5] = [("neutral", "Neutral"), ("lab_warm", "Lab Warm"), ("lab_cool", "Lab Cool"), ("print", "Print"), ("clean", "Clean")];
const WBS: [(&str, &str); 4] = [("as_is", "As Shot"), ("warm", "Warm"), ("cool", "Cool"), ("tungsten_film", "Tungsten Film")];

fn padded(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 6, bottom: 6 }).show(ui, add);
}

/// Runs `develop.negative` with `p`, telling the user when it fails.
fn set(app: &mut LightcraftApp, ui: &egui::Ui, p: Value) {
    if let Err(e) = app.run("develop.negative", p) {
        app.toast(ui.ctx(), e);
    }
}

/// A labelled dropdown of `options` (value, label) plus "From Preset" (the empty value) when
/// `from_preset` names what the preset uses; picking one sets `key`.
fn choice(app: &mut LightcraftApp, ui: &mut egui::Ui, key: &str, label: &str, current: &str, options: &[(&str, &str)], from_preset: Option<&str>) {
    let t = Tokens::get(ui.ctx());
    let shown = match (current, from_preset) {
        ("", Some(p)) => format!("{} ({})", crate::i18n::tr("From Preset"), crate::i18n::tr(p)),
        (v, _) => crate::i18n::tr(options.iter().find(|(k, _)| *k == v).map_or("From Preset", |(_, l)| *l)).to_string(),
    };
    padded(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(crate::i18n::tr(label)).font(t.font(13.0)).color(t.text_dim));
            let r = crate::widgets::dropdown(ui, &format!("negative-{key}"), &shown, t.font(14.0), t.text_label);
            egui::Popup::menu(&r).show(|ui| {
                if from_preset.is_some() && ui.selectable_label(current.is_empty(), crate::i18n::tr("From Preset")).clicked() {
                    set(app, ui, json!({ key: "" }));
                }
                for (value, name) in options {
                    if ui.selectable_label(current == *value, crate::i18n::tr(name)).clicked() {
                        set(app, ui, json!({ key: value }));
                    }
                }
            });
        });
    });
}

/// The film base / gray point row: a swatch of the picked colour (none: what is used instead),
/// the selector and a clear button.
fn picker_row(app: &mut LightcraftApp, ui: &mut egui::Ui, tool: &str, label: &str, picked: Option<[f64; 3]>, fallback: &str) {
    let t = Tokens::get(ui.ctx());
    padded(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(crate::i18n::tr(label)).font(t.font(13.0)).color(t.text_dim));
            let (r, resp) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::hover());
            match picked {
                Some(c) => {
                    let enc = |v: f64| (lightcraft_color::transfer::linear_to_srgb(v.clamp(0.0, 1.0) as f32) * 255.0).round() as u8;
                    let colour = Color32::from_rgb(enc(c[0]), enc(c[1]), enc(c[2]));
                    ui.painter().rect_filled(r, 4.0, colour);
                    ui.painter().rect_stroke(r, 4.0, egui::Stroke::new(1.0, t.text_dim), egui::StrokeKind::Inside);
                    let rgb = c.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
                    resp.on_hover_text(format!("RGB {} {} {}", rgb[0], rgb[1], rgb[2]));
                }
                None => {
                    ui.painter().rect_stroke(r, 4.0, egui::Stroke::new(1.0, t.text_dim), egui::StrokeKind::Inside);
                    ui.label(egui::RichText::new(crate::i18n::tr(fallback)).font(t.font(12.0)).color(t.text_dim));
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let active = app.ui.tool == tool;
                let tip = if tool == "negBase" { "Pick the film base from the unexposed border" } else { "Pick something that should be gray" };
                if crate::widgets::icon_button(ui, tool, Icon::Picker, vec2(28.0, 28.0), active, true, tip).clicked() {
                    if active {
                        app.ui.tool.clear();
                    } else if let Err(e) = app.run(&format!("tool.{tool}"), json!({})) {
                        app.toast(ui.ctx(), e);
                    }
                }
                let clear = if tool == "negBase" { "clearBase" } else { "clearGray" };
                if picked.is_some() && text_button(ui, &format!("negative-{clear}"), "Clear", false).clicked() {
                    set(app, ui, json!({ clear: true }));
                }
            });
        });
    });
}

pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId) {
    let d = app.session.develop_of(id).unwrap_or_default();
    let n = d.negative.clone();
    let t = Tokens::get(ui.ctx());
    header(ui, "Convert Negative");
    padded(ui, |ui| {
        let mut on = n.enabled;
        let r = ui.checkbox(&mut on, crate::i18n::tr("Film negative"));
        register(ui, "check:negativeOn", r.rect);
        if r.changed() {
            set(app, ui, json!({"on": on}));
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            for (label, advanced) in [("Simple", false), ("Advanced", true)] {
                let key = if advanced { "negativeMode-advanced" } else { "negativeMode-simple" };
                if text_button(ui, key, label, n.advanced == advanced).clicked() && n.advanced != advanced {
                    set(app, ui, json!({"advanced": advanced}));
                }
            }
        });
    });
    if !n.enabled {
        padded(ui, |ui| {
            ui.label(egui::RichText::new(crate::i18n::tr("Switch on to render this scan of a film negative as a positive.")).color(t.text_dim));
        });
        return;
    }
    let preset = lightcraft_pipeline::negative::preset_of(&n);
    padded(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(crate::i18n::tr("Film")).font(t.font(13.0)).color(t.text_dim));
            let r = crate::widgets::dropdown(ui, "negative-preset", preset.name, t.font(14.0), t.text_label);
            egui::Popup::menu(&r).show(|ui| {
                for p in &PRESETS {
                    if ui.selectable_label(p.name == preset.name, p.name).clicked() {
                        set(app, ui, json!({"preset": p.name}));
                    }
                }
            });
        });
    });
    if n.advanced {
        picker_row(app, ui, "negBase", "Film Base", n.base, if n.measured.is_some() { "Measured" } else { "From Preset" });
        picker_row(app, ui, "negGray", "Gray Point", n.gray, "None");
    }
    for c in SIMPLE {
        control(app, ui, &d, c, true);
    }
    if n.advanced {
        divider(ui);
        choice(app, ui, "tone", "Tone", &n.tone, &TONES, Some(label_of(&TONES, preset.tone.name())));
        choice(app, ui, "look", "Color", &n.look, &LOOKS, Some(label_of(&LOOKS, preset.look.name())));
        let wb = if n.wb.is_empty() { "as_is" } else { n.wb.as_str() };
        choice(app, ui, "wb", "White Balance", wb, &WBS, None);
        for c in ADVANCED {
            control(app, ui, &d, c, true);
        }
    }
    divider(ui);
    padded(ui, |ui| {
        ui.horizontal(|ui| {
            if text_button(ui, "negative-measure", "Measure Again", false).clicked() {
                set(app, ui, json!({"measure": true}));
            }
            if text_button(ui, "negative-done", "Done", false).clicked() {
                let _ = app.run("panel.negative", json!({}));
            }
        });
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(crate::i18n::tr(
                "The film base and levels are measured inside the crop, 8 % in from each edge. Crop to the frame and measure again if the holder or the border shows.",
            ))
            .font(t.font(12.0))
            .color(t.text_dim),
        );
    });
}

fn label_of(options: &[(&'static str, &'static str)], name: &str) -> &'static str {
    options.iter().find(|(k, _)| *k == name).map_or("", |(_, l)| *l)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_pipeline::negative::film::{Look, Tone, WbPreset};

    /// The film model's names are what `develop.negative` takes; keep the menus in step with them.
    #[test]
    fn menus_name_every_choice_of_the_film_model() {
        assert_eq!(TONES.len(), Tone::ALL.len());
        assert!(TONES.iter().all(|(k, _)| Tone::parse(k).is_some()));
        assert_eq!(LOOKS.len(), Look::ALL.len());
        assert!(LOOKS.iter().all(|(k, _)| Look::parse(k).is_some()));
        assert_eq!(WBS.len(), WbPreset::ALL.len());
        assert!(WBS.iter().all(|(k, _)| WbPreset::parse(k).is_some()));
    }
}
