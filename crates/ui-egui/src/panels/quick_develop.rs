//! The Library module's Quick Develop panel (Classic): changes for every selected photo at once.
//! The arrows add a step to each photo's own value (`develop.quickAdjust`); the menus and Auto
//! Tone set one choice on each (`develop.quickSet`); each click is one undo step.

use serde_json::json;

use crate::DacApp;
use crate::theme::Tokens;
use crate::widgets::register;

/// Crop ratios offered (value for `crop.aspect`, label).
const RATIOS: &[(&str, &str)] = &[
    ("original", "Original"),
    ("free", "Free"),
    ("1x1", "1 × 1"),
    ("4x5", "4 × 5 / 8 × 10"),
    ("8.5x11", "8.5 × 11"),
    ("5x7", "5 × 7"),
    ("2x3", "2 × 3 / 4 × 6"),
    ("4x3", "4 × 3"),
    ("16x9", "16 × 9"),
    ("16x10", "16 × 10"),
];

/// White balance choices (`develop.wb` modes).
const WB: &[(&str, &str)] = &[
    ("asShot", "As Shot"),
    ("auto", "Auto"),
    ("daylight", "Daylight"),
    ("cloudy", "Cloudy"),
    ("shade", "Shade"),
    ("tungsten", "Tungsten"),
    ("fluorescent", "Fluorescent"),
    ("flash", "Flash"),
];

/// The step rows: (label, control, small step, big step).
pub(crate) const WB_ROWS: &[(&str, &str, f64, f64)] = &[("Temperature", "wb.temp", 100.0, 500.0), ("Tint", "wb.tint", 2.0, 10.0)];
pub(crate) const TONE_ROWS: &[(&str, &str, f64, f64)] = &[
    ("Exposure", "light.exposure", 1.0 / 3.0, 1.0),
    ("Contrast", "light.contrast", 5.0, 20.0),
    ("Highlights", "light.highlights", 5.0, 20.0),
    ("Shadows", "light.shadows", 5.0, 20.0),
    ("Whites", "light.whites", 5.0, 20.0),
    ("Blacks", "light.blacks", 5.0, 20.0),
];
pub(crate) const PRESENCE_ROWS: &[(&str, &str, f64, f64)] = &[("Clarity", "effects.clarity", 5.0, 20.0), ("Vibrance", "color.vibrance", 5.0, 20.0)];

pub fn show(app: &mut DacApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let n = app.session.targets(&json!({})).len();
    if n == 0 {
        ui.label(egui::RichText::new(crate::i18n::tr("No photo selected")).color(t.text_dim));
        return;
    }
    ui.label(egui::RichText::new(crate::i18n::tr_format!("{n} selected", n = n)).size(11.5).color(t.text_dim));
    // Saved Preset
    let presets: Vec<(String, String)> = app.session.presets.iter().map(|p| (p.id.clone(), p.name.clone())).collect();
    if let Some(p) = choice_row(ui, "qd-preset", "Saved Preset", "Choose…", |ui| {
        let mut pick = None;
        egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
            for (id, name) in &presets {
                if ui.button(name).clicked() {
                    pick = Some(id.clone());
                }
            }
        });
        pick.map(|id| json!({"preset": id}))
    }) {
        quick_set(app, ui, p);
    }
    // Crop Ratio
    if let Some(p) = choice_row(ui, "qd-crop", "Crop Ratio", "Choose…", |ui| {
        let mut pick = None;
        for (v, label) in RATIOS {
            if ui.button(crate::i18n::tr(label)).clicked() {
                pick = Some(json!({"aspect": v}));
            }
        }
        pick
    }) {
        quick_set(app, ui, p);
    }
    // Treatment
    if let Some(p) = choice_row(ui, "qd-treatment", "Treatment", "Choose…", |ui| {
        let mut pick = None;
        if ui.button(crate::i18n::tr("Color")).clicked() {
            pick = Some(json!({"treatment": "color"}));
        }
        if ui.button(crate::i18n::tr("Black & White")).clicked() {
            pick = Some(json!({"treatment": "bw"}));
        }
        pick
    }) {
        quick_set(app, ui, p);
    }
    ui.add_space(6.0);
    sub_header(ui, "White Balance");
    if let Some(p) = choice_row(ui, "qd-wb", "White Balance", "Choose…", |ui| {
        let mut pick = None;
        for (v, label) in WB {
            if ui.button(crate::i18n::tr(label)).clicked() {
                pick = Some(json!({"wb": v}));
            }
        }
        pick
    }) {
        quick_set(app, ui, p);
    }
    steps(app, ui, WB_ROWS);
    ui.add_space(6.0);
    sub_header(ui, "Tone Control");
    let r = ui.add(egui::Button::new(crate::i18n::tr("Auto Tone")).min_size(egui::vec2(ui.available_width() - 1.0, 22.0)));
    register(ui.ctx(), "button:qd-autoTone", r.rect);
    if r.clicked() {
        quick_set(app, ui, json!({"autoTone": true}));
    }
    steps(app, ui, TONE_ROWS);
    steps(app, ui, PRESENCE_ROWS);
    ui.add_space(6.0);
    let r = ui.add(egui::Button::new(crate::i18n::tr("Reset All")).min_size(egui::vec2(ui.available_width() - 1.0, 22.0)));
    register(ui.ctx(), "button:qd-reset", r.rect);
    if r.on_hover_text(crate::i18n::tr("Reset the selected photos' edits")).clicked()
        && let Err(e) = app.run("develop.reset", json!({}))
    {
        app.toast(ui.ctx(), e);
    }
}

fn quick_set(app: &mut DacApp, ui: &egui::Ui, p: serde_json::Value) {
    match app.run("develop.quickSet", p) {
        Ok(r) => {
            if let Some(e) = r["errors"].as_array().and_then(|a| a.first()).and_then(|e| e.as_str()) {
                app.toast(ui.ctx(), e.to_string());
            }
        }
        Err(e) => app.toast(ui.ctx(), e),
    }
}

fn sub_header(ui: &mut egui::Ui, title: &str) {
    let t = Tokens::get(ui.ctx());
    ui.label(egui::RichText::new(crate::i18n::tr(title)).size(11.5).strong().color(t.text_label));
}

/// A label and a menu button; the menu returns the `develop.quickSet` params of the choice.
fn choice_row(
    ui: &mut egui::Ui,
    id: &str,
    label: &str,
    button: &str,
    menu: impl FnOnce(&mut egui::Ui) -> Option<serde_json::Value>,
) -> Option<serde_json::Value> {
    let t = Tokens::get(ui.ctx());
    let mut out = None;
    ui.horizontal(|ui| {
        ui.add_sized([84.0, 20.0], egui::Label::new(egui::RichText::new(crate::i18n::tr(label)).size(11.5).color(t.text_dim)).truncate());
        let r = ui.add(
            egui::Button::new(egui::RichText::new(crate::i18n::tr(button)).size(11.5))
                .min_size(egui::vec2((ui.available_width() - 1.0).max(40.0), 20.0)),
        );
        register(ui.ctx(), format!("button:{id}"), r.rect);
        egui::Popup::menu(&r).show(|ui| {
            if let Some(v) = menu(ui) {
                out = Some(v);
                ui.close();
            }
        });
    });
    out
}

/// Rows of ◀◀ ◀ ▶ ▶▶ step buttons (`button:qd-<control>-<i>`, i = 0..3 from the left).
fn steps(app: &mut DacApp, ui: &mut egui::Ui, rows: &[(&str, &str, f64, f64)]) {
    let t = Tokens::get(ui.ctx());
    for (label, ctl, small, big) in rows {
        ui.horizontal(|ui| {
            ui.add_sized([84.0, 20.0], egui::Label::new(egui::RichText::new(crate::i18n::tr(label)).size(11.5).color(t.text_dim)).truncate());
            let gap = ui.spacing().item_spacing.x;
            let bw = ((ui.available_width() - 3.0 * gap - 1.0) / 4.0).clamp(14.0, 40.0);
            for (i, (txt, d)) in [("◀◀", -big), ("◀", -small), ("▶", *small), ("▶▶", *big)].into_iter().enumerate() {
                let r = ui.add(egui::Button::new(egui::RichText::new(txt).size(10.0)).min_size(egui::vec2(bw, 20.0)));
                register(ui.ctx(), format!("button:qd-{ctl}-{i}"), r.rect);
                if r.on_hover_text(crate::i18n::tr_format!("{label} {d:+} on every selected photo", d = round(d), label = crate::i18n::tr(label)))
                    .clicked()
                    && let Err(e) = app.run("develop.quickAdjust", json!({"control": ctl, "delta": d}))
                {
                    app.toast(ui.ctx(), e);
                }
            }
        });
    }
}

fn round(d: f64) -> f64 {
    (d * 100.0).round() / 100.0
}
