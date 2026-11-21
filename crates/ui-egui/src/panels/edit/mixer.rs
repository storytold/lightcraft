use super::curve::set_tat;
use super::sliders::{apply_slider_out, control, sub_title};
use super::{
    BAND_COLORS, Color32, DevelopSettings, Icon, LightcraftApp, Sense, Stroke, Tokens, controls, hex, json, paint, register, slider, text_button,
    vec2,
};

// ------------------------------------------------------------------------------ colour mixer

pub(super) fn mixer(app: &mut LightcraftApp, ui: &mut egui::Ui, d: &DevelopSettings) {
    let t = Tokens::get(ui.ctx());
    let bands = lightcraft_develop::MIXER_BANDS;
    let sel = bands.iter().position(|b| *b == app.ui.mixer_mode).unwrap_or(0);
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 8, bottom: 4 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 5.0;
            for (i, b) in bands.iter().enumerate() {
                let (r, resp) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
                crate::widgets::announce(&resp, crate::i18n::tr(b));
                register(ui, format!("mixerBand:{b}"), r);
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

/// What a Point Color block edits: the photo's own samples or one mask's.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum PointTarget {
    Develop,
    Mask(u32),
}

impl PointTarget {
    /// The eyedropper tool this block arms (`UiState::tool`); the photo click is routed by it.
    fn tool(self) -> &'static str {
        match self {
            Self::Develop => "pointColor",
            Self::Mask(_) => "maskPointColor",
        }
    }

    /// Prefix of the widget ids (`<prefix>Picker`, `<prefix>:<i>`, `<prefix>Delete`).
    fn prefix(self) -> &'static str {
        match self {
            Self::Develop => "pointColor",
            Self::Mask(_) => "maskPointColor",
        }
    }

    fn selected(self, app: &LightcraftApp) -> usize {
        match self {
            Self::Develop => app.ui.point_color,
            Self::Mask(_) => app.ui.mask_point_color,
        }
    }

    fn select(self, app: &mut LightcraftApp, i: usize) {
        match self {
            Self::Develop => app.ui.point_color = i,
            Self::Mask(_) => app.ui.mask_point_color = i,
        }
    }

    /// Set field `k` (`hueShift`, `range`, ...) of sample `sel`.
    fn set(self, app: &mut LightcraftApp, sel: usize, k: &str, v: f64) -> Result<serde_json::Value, String> {
        match self {
            Self::Develop => app.run("develop.set", json!({"control": format!("pointColor.{sel}.{k}"), "value": v})),
            Self::Mask(id) => app.run("mask.pointColor.set", json!({"id": id, "index": sel, k: v})),
        }
    }

    fn delete(self, app: &mut LightcraftApp, sel: usize) {
        let _ = match self {
            Self::Develop => app.run("pointColor.delete", json!({"index": sel})),
            Self::Mask(id) => app.run("mask.pointColor.delete", json!({"id": id, "index": sel})),
        };
    }
}

fn sample_field(p: &lightcraft_develop::PointColor, k: &str) -> f64 {
    match k {
        "hueShift" => p.hue_shift,
        "satShift" => p.sat_shift,
        "lumShift" => p.lum_shift,
        "variance" => p.variance,
        "range" => p.range,
        "hueRange" => p.hue_range,
        "satRange" => p.sat_range,
        _ => p.lum_range,
    }
}

/// Point Color of the photo: swatches of the samples (+ the eyedropper), the selected sample's shifts and
/// range, and "Visualize range".
pub(super) fn point_color(app: &mut LightcraftApp, ui: &mut egui::Ui, d: &DevelopSettings) {
    point_color_block(app, ui, PointTarget::Develop, &d.point_colors);
}

/// Point Color of mask `id`, through the `mask.pointColor.*` commands. No "Visualize range": the overlay
/// (`Overlay::PointColorRange`) reads the photo's samples only.
pub(crate) fn mask_point_color(app: &mut LightcraftApp, ui: &mut egui::Ui, id: u32, samples: &[lightcraft_develop::PointColor]) {
    point_color_block(app, ui, PointTarget::Mask(id), samples);
}

fn point_color_block(app: &mut LightcraftApp, ui: &mut egui::Ui, target: PointTarget, samples: &[lightcraft_develop::PointColor]) {
    let t = Tokens::get(ui.ctx());
    let n = samples.len();
    if target.selected(app) >= n && n > 0 {
        target.select(app, n - 1);
    }
    let sel = target.selected(app);
    let prefix = target.prefix();
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 8, bottom: 4 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 5.0;
            let active = app.ui.tool == target.tool();
            let full = n >= lightcraft_develop::MAX_POINT_COLORS;
            if crate::widgets::icon_button(
                ui,
                &format!("{prefix}Picker"),
                Icon::Picker,
                vec2(26.0, 26.0),
                active,
                !full,
                crate::i18n::tr("Sample a colour on the photo"),
            )
            .clicked()
            {
                app.ui.tool = if active { String::new() } else { target.tool().into() };
            }
            for (i, p) in samples.iter().enumerate() {
                let (r, resp) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
                crate::widgets::announce(&resp, format!("{} {}", crate::i18n::tr("Point Color"), i + 1));
                register(ui, format!("{prefix}:{i}"), r);
                ui.painter().rect_filled(r.shrink(2.0), 3.0, oklch_color(p.lum, p.chroma, p.hue));
                if i == sel {
                    ui.painter().rect_stroke(r, 4.0, Stroke::new(1.5, t.text), egui::StrokeKind::Inside);
                }
                if resp.clicked() {
                    target.select(app, i);
                }
            }
        });
    });
    let Some(sample) = samples.get(sel) else {
        egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 2, bottom: 8 }).show(ui, |ui| {
            ui.label(egui::RichText::new(crate::i18n::tr("Pick a colour on the photo with the eyedropper.")).size(12.0).color(t.text_dim));
        });
        return;
    };
    let field = |app: &mut LightcraftApp, ui: &mut egui::Ui, k: &str| {
        let Some(spec) = controls::find(&format!("pointColor.{sel}.{k}")) else { return };
        let out = slider(ui, spec, sample_field(sample, k), true, None);
        apply_slider_out(app, spec, out, |app, v| target.set(app, sel, k, v));
    };
    for k in ["hueShift", "satShift", "lumShift", "variance"] {
        field(app, ui, k);
    }
    sub_title(ui, crate::i18n::tr("Range"));
    for k in ["range", "hueRange", "satRange", "lumRange"] {
        field(app, ui, k);
    }
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 4, bottom: 8 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            if target == PointTarget::Develop {
                let mut v = app.ui.point_color_visualize;
                if ui.checkbox(&mut v, crate::i18n::tr("Visualize range")).changed() {
                    app.ui.point_color_visualize = v;
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if text_button(ui, &format!("{prefix}Delete"), crate::i18n::tr("Delete"), false).clicked() {
                    target.delete(app, sel);
                }
            });
        });
    });
}
