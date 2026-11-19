//! AI Defringe (docs/ai-defringe.md): its switch, Amount and progress at the top of Optics ▸ Defringe, and the
//! model the user installs in Settings ▸ AI Defringe.

use egui::RichText;
use lightcraft_catalog::PhotoId;
use lightcraft_develop::DevelopSettings;
use serde_json::{Value, json};

use super::settings::{heading, hint};
use crate::LightcraftApp;
use crate::i18n::{tr, tr_format};
use crate::theme::Tokens;
use crate::widgets::register;

/// What the pump last saw, and a model file picked in Settings waiting to be accepted.
#[derive(Default)]
pub struct Ui {
    next_pump: f64,
    generation: u64,
    /// The status the panel shows, re-read with each pump.
    status: Value,
    /// A picked model's manifest and file, shown for the user to accept before it is installed.
    picked: Option<(Value, String)>,
    /// Why the last pick or install was refused.
    rejected: Option<String>,
}

/// Called every frame: takes in a finished correction (and asks for a redraw, so the loupe and the thumbnails render
/// with it) and keeps the open photo's coming; while one is being made, asks again for its progress.
pub fn pump_background(app: &mut LightcraftApp, ctx: &egui::Context, now: f64) {
    if now < app.caches.aidefringe.next_pump {
        return;
    }
    let Ok(v) = app.session.execute("aiDefringe.pump", &json!({})) else { return };
    let generation = v["generation"].as_u64().unwrap_or(0);
    let status = app.session.execute("aiDefringe.status", &json!({})).unwrap_or(Value::Null);
    let running = status["photo"]["state"] == "running";
    let c = &mut app.caches.aidefringe;
    if generation != c.generation {
        c.generation = generation;
        ctx.request_repaint();
    }
    c.status = status;
    c.next_pump = now + if running { 0.1 } else { 0.5 };
    if running {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}

fn note(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    ui.add(egui::Label::new(RichText::new(text).size(11.0).color(color)).wrap());
}

/// Optics ▸ Defringe ▸ AI Defringe: the switch, its Amount and how the photo's correction is coming along.
pub fn section(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId, d: &DevelopSettings) {
    let t = Tokens::get(ui.ctx());
    crate::panels::edit::sub_title(ui, tr("AI Defringe"));
    let status = app.caches.aidefringe.status.clone();
    let has_model = status["model"].is_object();
    let pad = |top, bottom| egui::Margin { left: 24, right: 22, top, bottom };
    if !has_model {
        egui::Frame::NONE.inner_margin(pad(2, 6)).show(ui, |ui| {
            let why = if status["runtime"] == false {
                "This build of LightCraft can't run AI models."
            } else {
                "Removes colour fringes of every hue with a trained model. Install one first, in Settings under AI Defringe."
            };
            note(ui, tr(why), t.text_dim);
            if status["runtime"] != false {
                ui.add_space(2.0);
                let r = ui.small_button(tr("Open Settings…"));
                register(ui, "aidefringe:settings", r.rect);
                if r.clicked() {
                    app.ui.dialog = Some(crate::state::Dialog::Settings { tab: "aidefringe".into() });
                }
            }
        });
        return;
    }
    let mut on = d.optics.ai_defringe_enabled();
    egui::Frame::NONE.inner_margin(pad(6, 2)).show(ui, |ui| {
        let r = ui.add(egui::Checkbox::new(&mut on, tr("AI Defringe"))).on_hover_text(tr(
            "Removes colour fringes of every hue (purple, blue, green) with the installed defringe model, leaving real colours alone. \
             The correction is made once in the background and kept; switching it off and on, or changing Amount, is instant.",
        ));
        register(ui, "aidefringe:enabled", r.rect);
        if r.changed() {
            let _ = app.run("aiDefringe.toggle", json!({"id": id.0, "enabled": on}));
        }
    });
    crate::panels::edit::control(app, ui, d, "optics.aiDefringe", on);
    if !on || status["photo"].is_null() {
        return;
    }
    let photo = &status["photo"];
    match photo["state"].as_str() {
        Some("running") | Some("waiting") => {
            let (done, total) = (photo["done"].as_u64().unwrap_or(0), photo["total"].as_u64().unwrap_or(0));
            egui::Frame::NONE.inner_margin(pad(0, 6)).show(ui, |ui| {
                let fraction = crate::progress::fraction_of(done, total);
                let text = match fraction {
                    Some(f) => tr_format!("Removing fringes… {pct}%", pct = crate::progress::percent(f)),
                    None => tr("Starting AI Defringe…").to_string(),
                };
                crate::progress::Progress::new(fraction).label(text).width(ui.available_width().min(240.0)).widget("aidefringe:progress").show(ui);
            });
        }
        Some("failed") => {
            egui::Frame::NONE.inner_margin(pad(0, 6)).show(ui, |ui| {
                note(ui, &tr_format!("AI Defringe failed: {why}", why = photo["error"].as_str().unwrap_or("")), t.caution);
                ui.add_space(2.0);
                let r = ui.small_button(tr("Try again"));
                register(ui, "aidefringe:retry", r.rect);
                if r.clicked() {
                    let _ = app.session.execute("aiDefringe.retry", &json!({"id": id.0}));
                    app.caches.aidefringe.next_pump = 0.0;
                }
            });
        }
        _ => {}
    }
}

/// Settings ▸ AI Defringe: the installed model, and installing or removing one.
pub fn settings_tab(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    let status = app.session.execute("aiDefringe.status", &json!({})).unwrap_or(Value::Null);
    heading(ui, t, "AI Defringe");
    hint(
        ui,
        t,
        "Removes colour fringes (purple, blue and green edges from the lens) with a trained model: the AI Defringe switch is in the Optics panel, under Defringe. \
         Your files are never changed: each photo's correction is cached data that is made again when needed.",
    );
    if status["dir"].is_null() {
        hint(ui, t, "This build has nowhere to keep a defringe model, so it cannot use one. (The desktop app has.)");
        return;
    }
    let runtime = status["runtime"] == true;
    if !runtime {
        hint(ui, t, "This build cannot run AI models.");
    }
    let m = &status["model"];
    if m.is_object() {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(m["name"].as_str().unwrap_or("")).font(t.semibold(13.5)).color(t.text));
            ui.label(RichText::new(m["version"].as_str().unwrap_or("")).color(t.text_dim));
        });
        ui.label(RichText::new(tr_format!("Licence: {licence}", licence = m["licence"].as_str().unwrap_or(""))).color(t.text_label));
        if let Some(n) = m["notice"].as_str() {
            ui.add(egui::Label::new(RichText::new(n).color(t.text_dim)).wrap());
        }
        let r = crate::button::Button::new("Remove").widget("aidefringe:remove").show(ui);
        if r.clicked() {
            let _ = app.session.execute("aiDefringe.models.remove", &json!({}));
            app.caches.aidefringe.next_pump = 0.0;
        }
    } else {
        hint(
            ui,
            t,
            "No defringe model is installed. LightCraft doesn't come with one: install any ONNX defringe model. A defringe-model.json beside it gives its name and licence; without one, LightCraft names it after its file.",
        );
    }
    ui.add_space(6.0);
    let r = crate::button::Button::new("Install from file…")
        .enabled(runtime && app.services.pick_denoise_model.is_some())
        .widget("aidefringe:installFile")
        .show(ui);
    // (the desktop app's ONNX picker, shared with AI Denoise)
    if r.clicked()
        && let Some(path) = app.services.pick_denoise_model.as_mut().and_then(|f| f().into_iter().next())
    {
        match app.session.execute("aiDefringe.models.inspect", &json!({"path": path})) {
            Ok(info) => {
                app.caches.aidefringe.rejected = None;
                app.caches.aidefringe.picked = Some((info, path));
            }
            Err(e) => app.caches.aidefringe.rejected = Some(plain(&e)),
        }
    }
    if let Some((info, path)) = app.caches.aidefringe.picked.clone() {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.label(RichText::new(info["name"].as_str().unwrap_or("")).font(t.semibold(13.0)).color(t.text));
            ui.label(RichText::new(tr_format!("Licence: {licence}", licence = info["licence"].as_str().unwrap_or(""))).color(t.text_label));
            if let Some(n) = info["notice"].as_str() {
                ui.add(egui::Label::new(RichText::new(n).color(t.text_dim)).wrap());
            }
            hint(ui, t, "Installing it means you accept its licence and its notice above.");
            ui.horizontal(|ui| {
                let install = crate::button::Button::new("Accept and Install").widget("aidefringe:accept").show(ui);
                let cancel = crate::button::Button::new("Cancel").widget("aidefringe:cancel").show(ui);
                if install.clicked() {
                    match app.session.execute("aiDefringe.models.install", &json!({"path": path, "acknowledged": true})) {
                        Ok(_) => app.caches.aidefringe.rejected = None,
                        Err(e) => app.caches.aidefringe.rejected = Some(plain(&e)),
                    }
                    app.caches.aidefringe.picked = None;
                    app.caches.aidefringe.next_pump = 0.0;
                }
                if cancel.clicked() {
                    app.caches.aidefringe.picked = None;
                }
            });
        });
    }
    if let Some(why) = app.caches.aidefringe.rejected.clone() {
        hint(ui, t, &why);
    }
}

/// What an engine error says, without the command it came from.
fn plain(e: &lightcraft_engine::EngineError) -> String {
    match e {
        lightcraft_engine::EngineError::BadParams { msg, .. } => msg.clone(),
        other => other.to_string(),
    }
}
