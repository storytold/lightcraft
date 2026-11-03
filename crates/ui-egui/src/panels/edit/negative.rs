//! The Negative section of the Edit panel (`docs/negative-conversion.md`): convert a film scan, pick the film type,
//! a Film Conversion preset, the tone and colour model, see where the film base came from, and the `negative.*`
//! sliders.

use lightcraft_develop::{BaseSource, DevelopSettings, FilmColorModel, FilmTone, FilmType};
use serde_json::{Value, json};

use super::{control, section, sub_title};
use crate::LightcraftApp;
use crate::theme::Tokens;
use crate::widgets::text_button;

/// Groups of the conversion presets (`crates/engine/src/presets.rs`): the conversion looks, then the film stocks.
const PRESET_GROUPS: [&str; 2] = ["Film Conversion", "Film Stocks"];

/// A row inside the panel's margins that wraps onto the next line when the panel is too narrow for it (the right
/// column can be narrow, and translations run long).
fn row(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 2, bottom: 4 }).show(ui, |ui| ui.horizontal_wrapped(add));
}

/// A labelled dropdown of `options` (`what`: widget id and label); the chosen one's JSON value is handed to `pick`.
fn choice<T: Copy + PartialEq + serde::Serialize>(
    app: &mut LightcraftApp,
    ui: &mut egui::Ui,
    (id, label): (&str, &str),
    current: T,
    options: &[T],
    name: fn(T) -> &'static str,
    pick: impl Fn(&mut LightcraftApp, Value) -> Result<Value, String>,
) {
    let t = Tokens::get(ui.ctx());
    row(ui, |ui| {
        ui.label(egui::RichText::new(crate::i18n::tr(label)).font(t.font(13.0)).color(t.text_dim));
        let r = crate::widgets::dropdown(ui, id, crate::i18n::tr(name(current)), t.font(14.0), t.text_label);
        egui::Popup::menu(&r).show(|ui| {
            for o in options {
                if ui.selectable_label(*o == current, crate::i18n::tr(name(*o))).clicked()
                    && let Err(e) = pick(app, serde_json::to_value(o).unwrap_or_default())
                {
                    app.toast(ui.ctx(), e);
                }
            }
        });
    });
}

/// The Negative section of the Edit panel: the film type buttons on a scan not converted yet; on a converted one the
/// film type, preset groups, tone and colour model, where the film base came from, the eyedropper, Re-analyze /
/// Unlock, Sync Roll and Reset, and the sliders.
pub(super) fn negative(app: &mut LightcraftApp, ui: &mut egui::Ui, d: &DevelopSettings) {
    section(app, ui, d, "negative", "Negative", |app, ui, d| {
        let t = Tokens::get(ui.ctx());
        let n = &d.negative;
        if !n.converts() {
            row(ui, |ui| {
                ui.label(egui::RichText::new(crate::i18n::tr("Convert a film scan:")).font(t.font(13.0)).color(t.text_dim));
            });
            row(ui, |ui| {
                for (film, label) in [(FilmType::ColorNegative, "Color Negative"), (FilmType::BwNegative, "B&W Negative"), (FilmType::Slide, "Slide")]
                {
                    let film = serde_json::to_value(film).unwrap_or_default();
                    let id = format!("negativeConvert-{}", film.as_str().unwrap_or_default());
                    // (asks first when converting would reset edits made on the negative)
                    if text_button(ui, &id, label, false).clicked()
                        && let Err(e) = app.run("dialog.convertNegative", json!({"film": film}))
                    {
                        app.toast(ui.ctx(), e);
                    }
                }
            });
            ui.add_space(8.0);
            return;
        }
        choice(app, ui, ("negativeFilm", "Film"), n.film, &FilmType::ALL, FilmType::label, |app, v| {
            if v == json!("off") { app.run("negative.reset", json!({})) } else { app.run("negative.set", json!({"film": v})) }
        });
        // conversion looks and film stocks: options only, so each frame keeps its own measured base and range
        let groups: Vec<(&str, Vec<(String, String)>)> = PRESET_GROUPS
            .iter()
            .map(|g| (*g, app.session.presets.iter().filter(|p| p.group == *g).map(|p| (p.id.clone(), p.name.clone())).collect::<Vec<_>>()))
            .filter(|(_, v)| !v.is_empty())
            .collect();
        if !groups.is_empty() {
            row(ui, |ui| {
                ui.label(egui::RichText::new(crate::i18n::tr("Look")).font(t.font(13.0)).color(t.text_dim));
                let r = crate::widgets::dropdown(ui, "negativePreset", crate::i18n::tr("Choose…"), t.font(14.0), t.text_label);
                egui::Popup::menu(&r).show(|ui| {
                    for (k, (group, presets)) in groups.iter().enumerate() {
                        if k > 0 {
                            ui.separator();
                        }
                        ui.label(egui::RichText::new(crate::i18n::tr(group)).size(11.5).color(t.text_dim));
                        for (id, name) in presets {
                            if ui.selectable_label(false, name.as_str()).clicked()
                                && let Err(e) = app.run("preset.apply", json!({"id": id}))
                            {
                                app.toast(ui.ctx(), e);
                            }
                        }
                    }
                });
            });
        }
        choice(app, ui, ("negativeTone", "Tone"), n.tone, &FilmTone::ALL, FilmTone::label, |app, v| app.run("negative.set", json!({"tone": v})));
        if n.film != FilmType::BwNegative {
            choice(app, ui, ("negativeColorModel", "Color"), n.color_model, &FilmColorModel::ALL, FilmColorModel::label, |app, v| {
                app.run("negative.set", json!({"colorModel": v}))
            });
        }
        // where the film base came from: it sets what reads as neutral
        let (status, warn) = match n.analysis.map(|a| a.base_source) {
            Some(BaseSource::Detected) => ("Film base: found on the film edge", false),
            Some(BaseSource::Sampled) => ("Film base: sampled", false),
            Some(BaseSource::Estimated) if n.film == FilmType::Slide => ("Range measured", false),
            Some(BaseSource::Estimated) => ("Film base: estimated (no film edge found). Pick it with the eyedropper", true),
            None => ("Film base: measured when shown (Re-analyze to keep it)", true),
        };
        row(ui, |ui| {
            ui.label(egui::RichText::new(crate::i18n::tr(status)).size(12.0).color(if warn { t.text_dim } else { t.accent }));
        });
        if n.locked {
            row(ui, |ui| {
                ui.label(egui::RichText::new(crate::i18n::tr("Roll-synced: base and range shared with the roll")).size(12.0).color(t.accent));
            });
        }
        // the analysis measured another part of the photo than the crop shows now
        let region = d.negative_region();
        // (measured for another film type: always out of date)
        let stale = n.analysis.map(|a| {
            !a.fits(n.film)
                || match a.region {
                    Some(r) => [r.x0 - region.x0, r.y0 - region.y0, r.x1 - region.x1, r.y1 - region.y1].iter().any(|v| v.abs() > 1e-3),
                    // measured by an older LightCraft (whole scan, medians only)
                    None => true,
                }
        });
        // (a roll-synced frame keeps the roll's numbers whatever its crop)
        if stale == Some(true) && (!n.locked || n.analysis.is_some_and(|a| !a.fits(n.film))) {
            row(ui, |ui| {
                ui.label(
                    egui::RichText::new(crate::i18n::tr("The analysis is out of date (crop changed, or an older version): Re-analyze"))
                        .size(12.0)
                        .color(t.text_dim),
                );
            });
        }
        row(ui, |ui| {
            let picking = app.ui.tool == "filmBase";
            // (slide film has no base to pick: its conversion ignores one)
            if n.film != FilmType::Slide
                && crate::widgets::icon_button(
                    ui,
                    "negativeBasePicker",
                    crate::icons::Icon::Picker,
                    egui::vec2(28.0, 28.0),
                    picking,
                    true,
                    crate::i18n::tr("Film Base Selector: click unexposed film (the rebate)"),
                )
                .clicked()
            {
                if picking {
                    app.ui.tool.clear();
                } else if let Err(e) = app.run("tool.filmBase", json!({})) {
                    app.toast(ui.ctx(), e);
                }
            }
            if n.locked {
                if text_button(ui, "negativeUnlock", "Unlock", false).clicked()
                    && let Err(e) = app.run("negative.unlock", json!({}))
                {
                    app.toast(ui.ctx(), e);
                }
            } else if text_button(ui, "negativeAnalyze", "Re-analyze", false).clicked()
                && let Err(e) = app.run("negative.analyze", json!({}))
            {
                app.toast(ui.ctx(), e);
            }
            // two frames or more selected: one analysis for all of them
            if app.session.selected().len() >= 2
                && text_button(ui, "negativeSyncRoll", "Sync Roll", false).clicked()
                && let Err(e) = app.run("dialog.syncRoll", json!({}))
            {
                app.toast(ui.ctx(), e);
            }
            if text_button(ui, "negativeReset", "Reset", false).clicked()
                && let Err(e) = app.run("negative.reset", json!({}))
            {
                app.toast(ui.ctx(), e);
            }
        });
        sub_title(ui, "Tone");
        for c in ["negative.brightness", "negative.contrast", "negative.whitePoint", "negative.blackPoint"] {
            control(app, ui, d, c, true);
        }
        if n.film != FilmType::BwNegative {
            sub_title(ui, "Color");
            for c in ["negative.temp", "negative.tint", "negative.saturation", "negative.balance"] {
                control(app, ui, d, c, true);
            }
            sub_title(ui, "Shadow & Highlight Toning");
            for c in ["negative.shadowTemp", "negative.shadowTint", "negative.highlightTemp", "negative.highlightTint"] {
                control(app, ui, d, c, true);
            }
        }
        sub_title(ui, "Analysis");
        control(app, ui, d, "negative.border", true);
        ui.add_space(8.0);
    });
}
