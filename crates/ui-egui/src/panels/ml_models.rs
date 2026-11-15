//! Semantic search models in Settings ▸ AI Models (`ml.models.*`, `ml.status`, `ml.enable`).

use crate::i18n::{tr, tr_format};
use egui::RichText;
use serde_json::{Value, json};

use super::settings::check;
use crate::LccApp;
use crate::theme::Tokens;
use crate::widgets::register;

fn mb(bytes: Option<u64>) -> String {
    match bytes.filter(|b| *b > 0) {
        Some(b) => tr_format!("{n} MB", n = format!("{:.0}", b as f64 / 1_000_000.0)),
        None => String::new(),
    }
}

fn licence_line(m: &Value) -> String {
    let spdx = m["licence"]["spdx"].as_str().unwrap_or("");
    let summary = m["licence"]["summary"].as_str().unwrap_or("");
    match (spdx.is_empty(), summary.is_empty()) {
        (false, false) => format!("{} ({spdx}): {}", tr("Licence"), tr(summary)),
        (false, true) => format!("{}: {spdx}", tr("Licence")),
        (true, false) => tr(summary).into(),
        (true, true) => String::new(),
    }
}

/// How long a read of `ml.status`, `ml.models.list`, or `ml.models.downloads` is reused across frames.
const REFRESH_SECS: f64 = 1.0;

fn status_cache_id() -> egui::Id {
    egui::Id::new("ml-status-cache")
}

fn list_cache_id() -> egui::Id {
    egui::Id::new("ml-models-list-cache")
}

fn downloads_cache_id() -> egui::Id {
    egui::Id::new("ml-models-downloads-cache")
}

fn invalidate_ml_caches(ctx: &egui::Context) {
    ctx.data_mut(|d| {
        d.remove::<(f64, String, Value)>(status_cache_id());
        d.remove::<(f64, Value)>(list_cache_id());
        d.remove::<(f64, Vec<Value>)>(downloads_cache_id());
    });
}

/// The engine's `ml.status`, re-read at most every [`REFRESH_SECS`] or immediately after a device/enable change.
pub fn cached_status(app: &mut LccApp, ctx: &egui::Context) -> Value {
    let now = ctx.input(|i| i.time);
    let dev = app.ui.settings.ml_device.clone();
    if let Some((at, ref cached_dev, ref v)) = ctx.data(|d| d.get_temp::<(f64, String, Value)>(status_cache_id()))
        && *cached_dev == dev
        && now - at < REFRESH_SECS
    {
        return v.clone();
    }
    let v = app.session.execute("ml.status", &json!({})).unwrap_or(Value::Null);
    ctx.data_mut(|d| d.insert_temp(status_cache_id(), (now, dev, v.clone())));
    v
}

fn cached_list(app: &mut LccApp, ctx: &egui::Context) -> Value {
    let now = ctx.input(|i| i.time);
    if let Some((at, v)) = ctx.data(|d| d.get_temp::<(f64, Value)>(list_cache_id()))
        && now - at < REFRESH_SECS
    {
        return v;
    }
    let v = app.session.execute("ml.models.list", &json!({})).unwrap_or(Value::Null);
    ctx.data_mut(|d| d.insert_temp(list_cache_id(), (now, v.clone())));
    v
}

fn cached_downloads(app: &mut LccApp, ctx: &egui::Context) -> Vec<Value> {
    let now = ctx.input(|i| i.time);
    if let Some((at, v)) = ctx.data(|d| d.get_temp::<(f64, Vec<Value>)>(downloads_cache_id())) {
        let running = v.iter().any(|dl| dl["state"].as_str() == Some("running"));
        let ttl = if running { 0.2 } else { REFRESH_SECS };
        if now - at < ttl {
            return v;
        }
    }
    let v =
        app.session.execute("ml.models.downloads", &json!({})).map(|v| v["downloads"].as_array().cloned().unwrap_or_default()).unwrap_or_default();
    ctx.data_mut(|d| d.insert_temp(downloads_cache_id(), (now, v.clone())));
    v
}

/// The AI Models dialog's Semantic search group.
pub fn models_section(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens) {
    let ctx = ui.ctx().clone();
    let list = cached_list(app, &ctx);
    let models: Vec<Value> = list["models"].as_array().cloned().unwrap_or_default();
    if list["dir"].is_null() || models.is_empty() {
        return;
    }
    let status = cached_status(app, &ctx);
    let downloads = cached_downloads(app, &ctx);
    crate::dialog_frame::group_begin_help(ui, Some("Semantic search"), Some("semanticSearch"));
    let r = ui.label(tr(
        "Searches the catalog by description (for example “moody studio portraits”) using image-text embeddings computed in the background.",
    ));
    // `app.settings {section: "semanticSearch"}` (the indexing row's gear) brings this group into view
    register(ui.ctx(), "section:semanticSearch", r.rect);
    if app.ui.settings_section.as_deref() == Some("semanticSearch") {
        r.scroll_to_me(Some(egui::Align::TOP));
        app.ui.settings_section = None;
    }
    let mut enabled = status["enabled"].as_bool() == Some(true);
    if check(ui, "mlEnable", &mut enabled, "Semantic search: search photos by description") {
        let _ = app.run("ml.enable", json!({"enabled": enabled}));
        invalidate_ml_caches(&ctx);
    }
    super::settings::hint(ui, t, "Off: photos are not indexed and the AI search button is hidden.");
    ml_device_selector(app, ui, t, &status);
    if enabled && list["selected"].is_string() {
        let progress = app.session.execute("ml.indexStatus", &json!({})).unwrap_or(Value::Null);
        let indexer = super::index_controls::Indexer {
            key: "ml",
            pause: "ml.pause",
            resume: "ml.resume",
            clear: "ml.clearIndex",
            restart: "ml.restart",
            clear_question: "Delete the search index? Indexing stays paused until you resume it.",
            restart_question: "Delete the search index and index every photo again?",
        };
        if super::index_controls::show(app, ui, t, &indexer, &progress) {
            invalidate_ml_caches(&ctx);
            app.caches.ml_next_pump = 0.0;
        }
    }
    for m in &models {
        let dl = downloads.iter().find(|d| d["id"] == m["id"]);
        model_row(app, ui, t, m, dl);
    }
    crate::dialog_frame::group_end(ui);
}

/// The hardware device selector dropdown and active device readout for AI embeddings.
pub fn ml_device_selector(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens, status: &Value) {
    let auto = tr("Automatic").to_string();
    let cpu = tr("CPU").to_string();
    let empty_vec = Vec::new();
    let dev_list = status.get("availableDevices").and_then(Value::as_array).unwrap_or(&empty_vec);
    let gpu_cards: Vec<&Value> = dev_list.iter().filter(|d| d["kind"].as_str() == Some("gpu")).collect();

    let name_of = |setting: &str| {
        if setting.is_empty() || setting.eq_ignore_ascii_case("auto") {
            Some(auto.clone())
        } else if setting.eq_ignore_ascii_case("cpu") {
            Some(cpu.clone())
        } else {
            gpu_cards.iter().find(|d| d["id"].as_str() == Some(setting)).and_then(|d| d["name"].as_str().map(str::to_string))
        }
    };
    let active_choice = status.get("deviceChoice").and_then(Value::as_str).unwrap_or("");
    let raw_setting = if app.ui.settings.ml_device.is_empty() && !active_choice.is_empty() && !active_choice.eq_ignore_ascii_case("auto") {
        active_choice
    } else {
        app.ui.settings.ml_device.as_str()
    };
    let normalized_before = if raw_setting.eq_ignore_ascii_case("auto") { String::new() } else { raw_setting.to_string() };
    let current = name_of(&normalized_before).unwrap_or_else(|| auto.clone());
    let mut selected = normalized_before.clone();
    super::settings::row(ui, t, crate::i18n::tr("AI embedding device"), |ui| {
        let combo = crate::widgets::combo_box("settingsMlDevice").selected_text(current).width(260.0).show_ui(ui, |ui| {
            let r = ui.selectable_value(&mut selected, String::new(), auto.as_str());
            register(ui.ctx(), "button:settingsMlDevice-auto", r.rect);
            let r = ui.selectable_value(&mut selected, "cpu".to_string(), cpu.as_str());
            register(ui.ctx(), "button:settingsMlDevice-cpu", r.rect);
            for (i, d) in gpu_cards.iter().enumerate() {
                let id = d["id"].as_str().unwrap_or("");
                let name = d["name"].as_str().unwrap_or(id);
                let usable = d["usable"].as_bool().unwrap_or(true);
                let why_not = d["whyNot"].as_str();
                let r = ui.add_enabled_ui(usable, |ui| ui.selectable_value(&mut selected, id.to_string(), name)).inner;
                let r = match why_not {
                    Some(why) => r.on_disabled_hover_text(why),
                    None => r,
                };
                register(ui.ctx(), format!("button:settingsMlDevice-{i}"), r.rect);
            }
        });
        register(ui.ctx(), "combo:settingsMlDevice", combo.response.rect);
    });
    if selected != normalized_before {
        app.ui.settings.ml_device = selected.clone();
        let _ = app.run("ml.status", json!({"device": selected}));
        invalidate_ml_caches(ui.ctx());
    }

    let readout = if let Some(dev) = status.get("device").filter(|d| !d.is_null()) {
        let kind = dev["kind"].as_str().unwrap_or("cpu");
        let name = dev["name"].as_str().unwrap_or("CPU");
        if kind == "gpu" {
            tr_format!("Active: {name} (GPU accelerated)", name = name)
        } else if let Some(why) = dev["whyCpu"].as_str().filter(|w| !w.is_empty()) {
            tr_format!("Active: CPU · {why}", why = why)
        } else {
            tr("Active: CPU").to_string()
        }
    } else {
        #[cfg(not(target_os = "macos"))]
        let note = tr("GPU acceleration for AI embeddings currently requires Metal on macOS; running on CPU (multithreaded)");
        #[cfg(target_os = "macos")]
        let note = tr("Selected device initializes on first search");
        tr_format!("Active: CPU · {why}", why = note)
    };
    crate::dialog_frame::in_control_column(ui, |ui| {
        ui.add(egui::Label::new(egui::RichText::new(&readout).size(11.0).color(t.text_dim)).truncate()).on_hover_text(&readout);
    });
}

fn model_row(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens, m: &Value, dl: Option<&Value>) {
    let id = m["id"].as_str().unwrap_or("").to_string();
    let (installed, selected) = (m["installed"].as_bool() == Some(true), m["selected"].as_bool() == Some(true));
    let dl_state = if installed { None } else { dl.and_then(|d| d["state"].as_str()) };
    let host = m["downloadHost"].as_str().unwrap_or("").to_string();
    egui::Frame::NONE.inner_margin(egui::Margin::symmetric(0, 3)).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(m["name"].as_str().unwrap_or("")).color(t.text));
                    let size = mb(m["sizeBytes"].as_u64());
                    if !size.is_empty() {
                        ui.label(RichText::new(size).color(t.text_dim));
                    }
                    if selected {
                        ui.label(RichText::new(tr("in use")).color(t.accent));
                    } else if installed {
                        ui.label(RichText::new(tr("installed")).color(t.text_dim));
                    }
                });
                let lic = licence_line(m);
                if !lic.is_empty() {
                    ui.label(RichText::new(lic).font(t.font(11.5)).color(if m["licence"]["commercial"] == "yes" { t.text_dim } else { t.caution }));
                }
                match (dl_state, dl) {
                    (Some("running"), Some(d)) => {
                        let (got, total) = (d["bytes"].as_u64().unwrap_or(0), d["total"].as_u64().unwrap_or(0));
                        let text = if total > 0 && got >= total {
                            tr("Checking…").to_string()
                        } else {
                            tr_format!("{} of {} from {host}", mb(Some(got)), mb(Some(total)), host = host)
                        };
                        crate::progress::Progress::new(crate::progress::fraction_of(got, total))
                            .label(text)
                            .width(crate::progress::ROW_WIDTH.min(ui.available_width()))
                            .widget(format!("ml:progress:{id}"))
                            .show(ui);
                        ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
                    }
                    (Some("done" | "installing"), _) => {
                        ui.horizontal(|ui| {
                            crate::progress::spinner(ui, 12.0);
                            ui.label(RichText::new(tr("Downloaded and checked. Installing…")).font(t.font(11.5)).color(t.text_dim));
                        });
                        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
                    }
                    (Some("failed"), Some(d)) => {
                        let why = d["error"].as_str().unwrap_or("The download failed");
                        ui.add(egui::Label::new(RichText::new(why).font(t.font(11.5)).color(t.caution)).wrap());
                    }
                    _ => {}
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if installed {
                    let r = ui.button(tr("Remove"));
                    register(ui.ctx(), format!("ml:remove:{id}"), r.rect);
                    if r.clicked() {
                        let _ = app.run("ml.models.remove", json!({"id": id}));
                        invalidate_ml_caches(ui.ctx());
                    }
                    if !selected {
                        let r = ui.button(tr("Use"));
                        register(ui.ctx(), format!("ml:use:{id}"), r.rect);
                        if r.clicked() {
                            let _ = app.run("ml.models.select", json!({"id": id}));
                            invalidate_ml_caches(ui.ctx());
                        }
                    }
                } else {
                    match dl_state {
                        Some("running") => {
                            let r = ui.button(tr("Cancel"));
                            register(ui.ctx(), format!("ml:cancelDownload:{id}"), r.rect);
                            if r.clicked() {
                                let _ = app.run("ml.models.downloadCancel", json!({"id": id}));
                                invalidate_ml_caches(ui.ctx());
                            }
                        }
                        Some("done" | "installing") => {}
                        _ if !host.is_empty() => {
                            let label = if dl_state == Some("failed") { "Try again" } else { "Download" };
                            let r = ui.button(tr(label));
                            register(ui.ctx(), format!("ml:download:{id}"), r.rect);
                            if r.clicked() {
                                match app.run("ml.models.download", json!({"id": id, "acceptLicence": true, "acknowledged": true})) {
                                    Ok(_) => invalidate_ml_caches(ui.ctx()),
                                    Err(e) => app.toast_error(ui.ctx(), e),
                                }
                            }
                        }
                        _ => {}
                    }
                }
            });
        });
    });
}
