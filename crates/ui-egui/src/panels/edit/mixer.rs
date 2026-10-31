use super::curve::set_tat;
use super::sliders::{control, sub_title};
use super::{
    BAND_COLORS, Color32, DevelopSettings, Icon, LightcraftApp, Sense, Stroke, Tokens, apply_slider_out, controls, hex, json, paint, register,
    slider, text_button, vec2,
};
use egui::{Align2, pos2};

// ------------------------------------------------------------------------------ colour mixer

/// The Color Mixer's tabs (Mixer | Point Color), the Mixer's Adjust choice (HSL | Color) and the
/// HSL groups (Hue | Saturation | Luminance | All), as (label, key).
pub(crate) const MIXER_TABS: [(&str, &str); 2] = [("Mixer", "mixer"), ("Point Color", "point")];
pub(crate) const MIXER_ADJUST: [(&str, &str); 2] = [("HSL", "hsl"), ("Color", "color")];
pub(crate) const MIXER_GROUPS: [(&str, &str); 4] = [("Hue", "hue"), ("Saturation", "sat"), ("Luminance", "lum"), ("All", "all")];
/// The mixer bands' names, as the HSL sliders are labelled (parallel to `MIXER_BANDS`).
const BAND_NAMES: [&str; 8] = ["Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta"];

/// A Color Mixer targeted adjustment (`tat:hue|sat|lum`) whose button the mixer's layout no
/// longer shows is put down: a B&W photo shows Luminance's only, Point Color none, HSL only the chosen group's (all three in
/// All), Color mode all three. Dragging on the photo would otherwise change a hidden group.
pub(crate) fn drop_hidden_mixer_tool(app: &mut LightcraftApp) {
    // a B&W photo's mixer is its B&W mix, with only the Luminance targeted adjustment
    let bw = app.session.active().and_then(|id| app.session.develop_of(id)).is_some_and(|d| crate::is_bw(&d));
    // Point Color's picker lives on its own tab, which a B&W photo doesn't have
    if app.ui.tool == "pointColor" {
        if bw || app.ui.mixer_tab != "point" {
            app.ui.tool.clear();
        }
        return;
    }
    let Some(key) = app.ui.tool.strip_prefix("tat:") else { return };
    if !matches!(key, "hue" | "sat" | "lum") {
        return;
    }
    let shown = if bw {
        key == "lum"
    } else {
        app.ui.mixer_tab != "point" && (app.ui.mixer_adjust == "color" || app.ui.mixer_group == "all" || app.ui.mixer_group == key)
    };
    if !shown {
        app.ui.tool.clear();
    }
}

/// The Color Mixer, laid out as in Lightroom Classic: "Mixer | Point Color" tabs; the Mixer
/// adjusts by HSL (Hue | Saturation | Luminance | All: eight colour sliders per group, each group
/// with its targeted adjustment) or by Color (one colour at a time). The same develop controls
/// (`mixer.<band>.<hue|sat|lum>`) either way; a B&W photo shows its B&W mix.
pub(super) fn mixer(app: &mut LightcraftApp, ui: &mut egui::Ui, d: &DevelopSettings) {
    // (the B&W mix keeps its own Luminance targeted adjustment)
    if crate::is_bw(d) {
        mixer_by_color(app, ui, d);
        return;
    }
    let pick = |ui: &mut egui::Ui, id: &str, items: &[(&str, &str)], current: &str| {
        let at = items.iter().position(|(_, k)| *k == current).unwrap_or(0);
        egui::Frame::NONE
            .inner_margin(egui::Margin { left: 24, right: 22, top: 6, bottom: 2 })
            .show(ui, |ui| crate::widgets::segmented(ui, id, items, Some(at), items.len()))
            .inner
            .and_then(|i| items.get(i).map(|(_, k)| k.to_string()))
    };
    if let Some(k) = pick(ui, "mixerTab", &MIXER_TABS, &app.ui.mixer_tab) {
        app.ui.mixer_tab = k;
    }
    drop_hidden_mixer_tool(app);
    if app.ui.mixer_tab == "point" {
        point_color(app, ui, d);
        return;
    }
    let t = Tokens::get(ui.ctx());
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 4, bottom: 0 }).show(ui, |ui| {
        ui.label(egui::RichText::new(crate::i18n::tr("Adjust")).color(t.text_dim).size(11.5));
    });
    if let Some(k) = pick(ui, "mixerAdjust", &MIXER_ADJUST, &app.ui.mixer_adjust) {
        app.ui.mixer_adjust = k;
        drop_hidden_mixer_tool(app);
    }
    if app.ui.mixer_adjust == "color" {
        mixer_by_color(app, ui, d);
        return;
    }
    // HSL: Hue | Saturation | Luminance | All
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 6, bottom: 2 }).show(ui, |ui| {
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
        egui::Frame::NONE.inner_margin(egui::Margin { left: 20, right: 22, top: if all { 8 } else { 2 }, bottom: 0 }).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let tool = format!("tat:{key}");
                let active = app.ui.tool == tool;
                if crate::widgets::icon_button(
                    ui,
                    &format!("tatMixer-{key}"),
                    Icon::Target,
                    vec2(24.0, 24.0),
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
                    register(ui, format!("mixerHeading:{key}"), r.rect);
                }
            });
        });
        for (band, name) in lightcraft_develop::MIXER_BANDS.iter().zip(BAND_NAMES) {
            let id = format!("mixer.{band}.{key}");
            let Some(spec) = controls::find(&id) else { continue };
            // labelled by the colour alone: the group says what it adjusts
            let v = controls::get(d, &id).unwrap_or(spec.default);
            let out = slider(ui, spec, v, true, Some(name));
            apply_slider_out(app, spec, out, |app, v| app.run("develop.set", json!({"control": id, "value": v})));
        }
    }
}

/// A word tab (Hue, Saturation…): bright and underlined when it is the one shown.
fn group_tab(ui: &mut egui::Ui, id: &str, label: &str, active: bool) -> egui::Response {
    let t = Tokens::get(ui.ctx());
    let font = t.semibold(11.5);
    let (r, resp) = {
        let galley = ui.painter().layout_no_wrap(label.to_string(), font.clone(), t.text);
        ui.allocate_exact_size(vec2(galley.size().x, 20.0), Sense::click())
    };
    crate::widgets::announce_selected(&resp, label.to_string(), active);
    register(ui, format!("button:{id}"), r);
    let colour = if active || resp.hovered() { t.text } else { t.text_dim };
    ui.painter().text(pos2(r.left(), r.center().y), Align2::LEFT_CENTER, label, font, colour);
    if active {
        ui.painter().line_segment([pos2(r.left(), r.bottom() - 1.0), pos2(r.right(), r.bottom() - 1.0)], Stroke::new(1.5, t.text));
    }
    resp
}

/// The Mixer by colour (and a B&W photo's mix): a dot per colour, then that colour's sliders.
fn mixer_by_color(app: &mut LightcraftApp, ui: &mut egui::Ui, d: &DevelopSettings) {
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

/// Point Color: swatches of the samples (+ the eyedropper), the selected sample's shifts and range,
/// and "Visualize range".
pub(super) fn point_color(app: &mut LightcraftApp, ui: &mut egui::Ui, d: &DevelopSettings) {
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
                register(ui, format!("pointColor:{i}"), r);
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
