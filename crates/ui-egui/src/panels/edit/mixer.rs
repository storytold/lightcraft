use craft::ui::choice::{Checkbox, Swatch};
use craft::ui::contract::{Bind, Props as _, Show as _};

use super::curve::set_tat;
use super::sliders::{control, sub_title};
use super::{BAND_COLORS, Color32, DevelopSettings, Icon, LightcraftApp, Sense, Stroke, Tokens, hex, json, paint, register, text_button, vec2};

// ------------------------------------------------------------------------------ colour mixer

/// One UI-only band choice; the engine and the current frame's scalar snapshot remain unchanged.
fn mixer_band_picker(ui: &mut egui::Ui, selected: usize, bw: bool) -> Option<usize> {
    use craft::behavior::{NavShape, Proposal};
    use craft::ui::choice::RadioGroup;
    use craft::ui::contract::ShowContainer as _;

    let bands = lightcraft_develop::MIXER_BANDS;
    let label = crate::i18n::tr(if bw { "B&W Mixer" } else { "Color Mixer" });
    let real = !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx());
    let interactive = real && ui.is_enabled();
    egui::Frame::NONE
        .inner_margin(egui::Margin { left: 24, right: 22, top: 8, bottom: 4 })
        .show(ui, |ui| {
            let width = ui.available_width().min((ui.clip_rect().right() - ui.cursor().left()).max(1.0)).max(1.0);
            let measure = ui.new_child(egui::UiBuilder::new().max_rect(ui.max_rect()).sizing_pass().invisible());
            let sizes: Vec<_> = bands
                .iter()
                .enumerate()
                .map(|(index, band)| {
                    Swatch::new(crate::i18n::tr(band).to_owned(), point_color_srgb(hex(BAND_COLORS[index])))
                        .member(index as u8)
                        .measure_in(&measure, Proposal::Ideal)
                })
                .collect();
            let side = sizes.iter().map(|size| size.x.max(size.y)).fold(1.0_f32, f32::max);
            let gap = 5.0;
            let mut columns = bands.len();
            while columns > 1 && columns as f32 * side + (columns - 1) as f32 * gap > width {
                columns /= 2;
            }
            let mut activated = None;
            let mut body = |ui: &mut egui::Ui, _: &mut craft::ui::contract::BodyCx<'_>| {
                ui.spacing_mut().item_spacing.y = gap;
                for start in (0..bands.len()).step_by(columns) {
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = gap;
                        for index in start..(start + columns).min(bands.len()) {
                            let band = bands[index];
                            let shown = Swatch::new(crate::i18n::tr(band).to_owned(), point_color_srgb(hex(BAND_COLORS[index])))
                                .member(index as u8)
                                .id_salt(("mixer-band", band))
                                .show(ui);
                            if real && !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx()) {
                                crate::widgets::register_response(format!("mixerBand:{band}"), shown.response.interact_rect, &shown.response);
                                if interactive && shown.response.enabled() && shown.clicked {
                                    activated = Some(index);
                                }
                            }
                        }
                    });
                }
            };
            let chosen = selected as u8;
            let size = RadioGroup::new(label.to_owned(), Bind::given(chosen))
                .id_salt("mixer-band-picker")
                .nav(NavShape::Flat)
                .enabled(interactive)
                .measure_in(&measure, Proposal::Ideal, &mut body);
            let (rect, _) = ui.allocate_exact_size(vec2(width, size.y.max(side)), Sense::hover());
            if !real {
                return None;
            }
            let shown = RadioGroup::new(label.to_owned(), Bind::given(chosen))
                .id_salt("mixer-band-picker")
                .nav(NavShape::Flat)
                .enabled(interactive)
                .show_at(ui, rect, &mut body);
            if interactive { shown.output.events.committed().copied().map(usize::from).or(activated) } else { None }
        })
        .inner
}

pub(super) fn mixer(app: &mut LightcraftApp, ui: &mut egui::Ui, d: &DevelopSettings) {
    let t = Tokens::get(ui.ctx());
    let bands = lightcraft_develop::MIXER_BANDS;
    let sel = bands.iter().position(|b| *b == app.ui.mixer_mode).unwrap_or(0);
    if !crate::panels::settings::native_controls_active(ui.ctx()) {
        if let Some(index) = mixer_band_picker(ui, sel, crate::is_bw(d)) {
            if let Some(band) = bands.get(index) {
                app.ui.mixer_mode = (*band).to_owned();
            }
        }
    } else {
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
    }
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

/// Preserve the already quantized display colour: SDK Srgba is encoded, not linear.
fn point_color_srgb(color: Color32) -> craft::theme::Srgba {
    craft::theme::Srgba::rgb(color.r() as f32 / 255.0, color.g() as f32 / 255.0, color.b() as f32 / 255.0)
}

/// Samples select an index only; deliberately no picker/member or ColorRequest key.
fn point_color_swatch(ui: &mut egui::Ui, index: usize, color: Color32, selected: bool) -> bool {
    let measuring = ui.is_sizing_pass() || craft::ui::runtime::measure::active(ui.ctx());
    let id = format!("pointColor:{index}");
    let label = format!("{} {}", crate::i18n::tr("Point Color"), index + 1);
    let shown = Swatch::new(label, point_color_srgb(color)).id_salt(id.as_str()).selected(selected).show(ui);
    if measuring {
        return false;
    }
    crate::widgets::register_response(id, shown.response.rect, &shown.response);
    shown.clicked
}

/// Bind a local copy so speculative SDK/egui sizing never changes application UI state.
fn point_color_visualize(ui: &mut egui::Ui, value: &mut bool) {
    let measuring = ui.is_sizing_pass() || craft::ui::runtime::measure::active(ui.ctx());
    let mut local = *value;
    let shown = Checkbox::new(Bind::bound(&mut local))
        .id_salt("pointColorVisualize")
        .label(crate::i18n::tr("Visualize range").to_owned())
        .show(ui);
    if measuring {
        return;
    }
    crate::widgets::register_response("pointColorVisualize", shown.response.rect, &shown.response);
    if let Some(edit) = shown.value() {
        *value = *edit;
    }
}

/// Point Color: swatches of the samples (+ the eyedropper), the selected sample's shifts and range,
/// and "Visualize range".
pub(super) fn point_color(app: &mut LightcraftApp, ui: &mut egui::Ui, d: &DevelopSettings) {
    let t = Tokens::get(ui.ctx());
    let n = d.point_colors.len();
    // The last sample may have just been deleted. Lay out a valid sample even in
    // a measuring pass, but leave selection recovery to the next real frame.
    let sel = if n == 0 { app.ui.point_color } else { app.ui.point_color.min(n - 1) };
    if n > 0 && !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx()) {
        app.ui.point_color = sel;
    }
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
                if point_color_swatch(ui, i, oklch_color(p.lum, p.chroma, p.hue), i == sel) {
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
            point_color_visualize(ui, &mut app.ui.point_color_visualize);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if text_button(ui, "pointColorDelete", crate::i18n::tr("Delete"), false).clicked() {
                    let _ = app.run("pointColor.delete", json!({"index": sel}));
                }
            });
        });
    });
}

#[cfg(test)]
#[path = "point_color_controls_tests.rs"]
mod point_color_controls_tests;

#[cfg(test)]
#[path = "mixer_band_flow_tests.rs"]
mod mixer_band_flow_tests;
