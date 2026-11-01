//! A mask's own Color Mixer (LR-MASK-MIXER): the flyout under the selected mask's Curve in the Masking
//! panel. Laid out as the Edit panel's Color Mixer (a dot per colour, the targeted adjustment of Hue,
//! Saturation or Luminance, then the chosen colour's three sliders), it edits the mask through
//! `mask.mixer` and, dragging on the photo, `mask.mixerTargeted`.

use egui::{Color32, Sense, Stroke, vec2};
use lightcraft_develop::{ControlSpec, MIXER_BANDS, Mask, Mixer};
use serde_json::json;

use super::edit::apply_slider_out;
use crate::LightcraftApp;
use crate::icons::{Icon, paint};
use crate::theme::Tokens;
use crate::widgets::{BAND_COLORS, hex, register, slider, text_button};

/// The on-photo targeted adjustment of the selected mask's Color Mixer: `maskMixer:<hue|sat|lum>`.
pub const MIXER_TOOL: &str = "maskMixer:";

/// The mask mixer's sliders (`maskMixer.<band>.<hue|sat|lum>`; the `mask.mixer` key after the prefix), in
/// `MIXER_BANDS` order, each band's hue, saturation and luminance.
const MIXER_IDS: [&str; 24] = [
    "maskMixer.red.hue",
    "maskMixer.red.sat",
    "maskMixer.red.lum",
    "maskMixer.orange.hue",
    "maskMixer.orange.sat",
    "maskMixer.orange.lum",
    "maskMixer.yellow.hue",
    "maskMixer.yellow.sat",
    "maskMixer.yellow.lum",
    "maskMixer.green.hue",
    "maskMixer.green.sat",
    "maskMixer.green.lum",
    "maskMixer.aqua.hue",
    "maskMixer.aqua.sat",
    "maskMixer.aqua.lum",
    "maskMixer.blue.hue",
    "maskMixer.blue.sat",
    "maskMixer.blue.lum",
    "maskMixer.purple.hue",
    "maskMixer.purple.sat",
    "maskMixer.purple.lum",
    "maskMixer.magenta.hue",
    "maskMixer.magenta.sat",
    "maskMixer.magenta.lum",
];

/// The slider of band `b` (index into `MIXER_BANDS`) and attribute `k` (0 hue, 1 sat, 2 lum): the global
/// mixer's (label, range, coloured track) under the mask's id.
fn mixer_spec(b: usize, k: usize) -> Option<ControlSpec> {
    let id = *MIXER_IDS.get(b.checked_mul(3)?.checked_add(k)?)?;
    let key = id.strip_prefix("maskMixer.")?;
    let global = lightcraft_develop::controls::find(&format!("mixer.{key}"))?;
    Some(ControlSpec { id, ..*global })
}

fn mixer_value(m: &Mixer, b: usize, k: usize) -> f64 {
    m.bands().get(b).map_or(0.0, |h| match k {
        0 => h.hue,
        1 => h.sat,
        _ => h.lum,
    })
}

/// The selected mask's Color Mixer (the body of its "Color Mixer" flyout).
pub(crate) fn mixer(app: &mut LightcraftApp, ui: &mut egui::Ui, m: &Mask) {
    let t = Tokens::get(ui.ctx());
    let sel = MIXER_BANDS.iter().position(|b| *b == app.ui.mask_mixer_band).unwrap_or(0);
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 8, bottom: 4 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 5.0;
            for (i, b) in MIXER_BANDS.iter().enumerate() {
                let (r, resp) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
                crate::widgets::announce(&resp, format!("{} {}", crate::i18n::tr("Mask Color Mixer"), crate::i18n::tr(b)));
                register(ui, format!("maskMixerBand:{b}"), r);
                ui.painter().circle_filled(r.center(), 9.0, hex(BAND_COLORS.get(i).copied().unwrap_or("#808080")));
                if i == sel {
                    ui.painter().circle_stroke(r.center(), 10.5, Stroke::new(1.5, t.text));
                    ui.painter().circle_filled(r.center(), 2.0, Color32::WHITE);
                }
                if resp.clicked() {
                    app.ui.mask_mixer_band = b.to_string();
                }
            }
        });
    });
    // targeted adjustment: pick the attribute, then drag on the photo; Reset while anything is set
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 2, bottom: 2 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let (r, _) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
            paint(ui.painter(), r, Icon::Target, t.icon);
            for (k, label) in [("hue", "Hue"), ("sat", "Saturation"), ("lum", "Luminance")] {
                let tool = format!("{MIXER_TOOL}{k}");
                let active = app.ui.tool == tool;
                if text_button(ui, &format!("maskMixerTargeted-{k}"), crate::i18n::tr(label), active)
                    .on_hover_text(crate::i18n::tr("Targeted adjustment: drag up/down on the photo to adjust the mask's colours there"))
                    .clicked()
                {
                    if active {
                        app.ui.tool.clear();
                    } else {
                        app.ui.tool = tool;
                        app.toast(ui.ctx(), crate::i18n::tr("Targeted adjustment on: drag up or down on the photo · Esc to stop"));
                    }
                }
            }
            if !m.mixer.is_neutral() {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if text_button(ui, "maskMixerReset", crate::i18n::tr("Reset"), false).clicked() {
                        let _ = app.run("mask.mixer", json!({"id": m.id, "reset": true}));
                    }
                });
            }
        });
    });
    for k in 0..3 {
        let Some(s) = mixer_spec(sel, k) else { continue };
        let out = slider(ui, &s, mixer_value(&m.mixer, sel, k), true, None);
        let key = s.id.strip_prefix("maskMixer.").unwrap_or(s.id);
        apply_slider_out(app, &s, out, |app, v| app.run("mask.mixer", json!({"id": m.id, "values": {key: v}})));
    }
}
