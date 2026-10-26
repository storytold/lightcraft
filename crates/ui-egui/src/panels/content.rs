//! Search by content: the Settings ▸ Search tab (the indexing switch and the list of content models) and the dialog
//! that shows a model's licence before it is downloaded or used.
//!
//! LightCraft fetches a model only when the user presses Download and ticks "I have read these terms and accept them"
//! in the dialog (`content.models.download {acknowledged: true}`): from Hugging Face, checked against its recorded
//! sizes and SHA-256, shown in the activity stack while it runs. A model whose files are already in its folder (put
//! there by hand) offers "Check and Use" instead: the same dialog, then the files are checked and nothing is fetched.

use egui::RichText;
use serde_json::{Value, json};

use super::settings::{check, heading, hint};
use crate::LightcraftApp;
use crate::i18n::{tr, tr_format};
use crate::theme::Tokens;
use crate::widgets::register;

/// How long a read of the model list is reused (a few small files, but not every frame).
const REFRESH_SECS: f64 = 1.0;

fn list_id() -> egui::Id {
    egui::Id::unique("content-model-list")
}

/// The engine's model list, re-read at most every [`REFRESH_SECS`], and at once after [`forget`] or when a job starts
/// or ends. While a job runs the tab keeps repainting, so its progress moves.
fn models(app: &mut LightcraftApp, ctx: &egui::Context) -> Value {
    let now = ctx.input(|i| i.time);
    let running = app.session.content_jobs.running();
    if running.is_some() {
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(REFRESH_SECS));
    }
    let key = format!("{running:?}");
    if let Some((at, k, v)) = ctx.data(|d| d.get_temp::<(f64, String, Value)>(list_id()))
        && k == key
        && now - at < REFRESH_SECS
        && now >= at
    {
        return v;
    }
    let v = app.run("content.models.list", json!({})).unwrap_or(Value::Null);
    ctx.data_mut(|d| d.insert_temp(list_id(), (now, key, v.clone())));
    v
}

/// Read the list again on the next frame (an action changed it).
fn forget(ctx: &egui::Context) {
    ctx.data_mut(|d| d.remove::<(f64, String, Value)>(list_id()));
}

/// A download-style size: decimal megabytes or gigabytes, as model pages show them.
fn size(bytes: Option<u64>) -> String {
    match bytes {
        Some(b) if b >= 1_000_000_000 => format!("{:.1} GB", b as f64 / 1e9),
        Some(b) if b >= 1_000_000 => format!("{} MB", (b as f64 / 1e6).round() as u64),
        Some(b) => format!("{} KB", (b as f64 / 1e3).round().max(1.0) as u64),
        None => String::new(),
    }
}

/// "Apache-2.0 · commercial use allowed · Google"
fn licence_line(m: &Value) -> String {
    let name = m["licence"]["name"].as_str().filter(|s| !s.is_empty()).unwrap_or("Unknown licence");
    let terms = if m["licence"]["commercial"] == true { tr("commercial use allowed") } else { tr("non-commercial use only") };
    match m["publisher"].as_str() {
        Some(p) => format!("{name} · {terms} · {p}"),
        None => format!("{name} · {terms}"),
    }
}

/// "the download failed" → "The download failed."
fn sentence(s: &str) -> String {
    let s = s.trim();
    let mut c = s.chars();
    let mut out: String = c.next().map(|f| f.to_uppercase().collect()).unwrap_or_default();
    out.push_str(c.as_str());
    if !out.is_empty() && !out.ends_with(['.', '!', '?']) {
        out.push('.');
    }
    out
}

fn open_link(app: &mut LightcraftApp, ctx: &egui::Context, url: &str) {
    if let Err(e) = crate::links::open(app, url) {
        app.toast(ctx, e);
    }
}

/// The Settings ▸ Search tab.
pub fn settings_tab(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    let list = models(app, ui.ctx());
    heading(ui, t, "Search by content");
    hint(ui, t, "Everything stays on this computer: the model is downloaded once from Hugging Face, and no photo or search ever leaves it.");
    if list["dir"].is_null() {
        hint(ui, t, "This build has nowhere to keep content models. (The desktop app has.)");
        return;
    }
    let runtime = list["runtime"] == true;
    let all: Vec<Value> = list["models"].as_array().cloned().unwrap_or_default();
    let in_use = all.iter().find(|m| m["selected"] == true);
    let mut on = list["enabled"] == true;
    let r = ui
        .add_enabled(on || (runtime && in_use.is_some()), egui::Checkbox::new(&mut on, tr("Index photo contents")))
        .on_disabled_hover_text(tr("Choose a model below (Use) to start."));
    register(ui, "check:settings.contentEnabled", r.rect);
    if r.changed() {
        if let Err(e) = app.run("content.enable", json!({"on": on})) {
            app.toast(ui.ctx(), e);
        }
        forget(ui.ctx());
    }
    match in_use {
        Some(m) if runtime => hint(ui, t, &tr_format!("Model in use: {name}", name = m["name"].as_str().unwrap_or(""))),
        _ if !runtime => hint(ui, t, "This build cannot run content models."),
        _ => hint(ui, t, "No model in use yet: download one below."),
    }
    heading(ui, t, "Models");
    for m in &all {
        model_row(app, ui, t, m, runtime);
    }
    for e in list["catalog"]["errors"].as_array().into_iter().flatten().filter_map(Value::as_str).take(3) {
        ui.add(egui::Label::new(RichText::new(e).font(t.font(11.5)).color(t.caution)).wrap());
    }
}

/// One model: what it is, its licence, how it stands, and what can be done with it.
fn model_row(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, m: &Value, runtime: bool) {
    let id = m["id"].as_str().unwrap_or("").to_string();
    let state = m["state"].as_str().unwrap_or("available");
    let job = &m["job"];
    let running = job["state"] == "running";
    let supported = runtime && m["supported"] == true;
    egui::Frame::NONE.inner_margin(egui::Margin::symmetric(0, 3)).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(m["name"].as_str().unwrap_or(&id)).color(t.text));
                    ui.label(RichText::new(size(m["sizeBytes"].as_u64())).color(t.text_dim));
                    let tag = match state {
                        _ if m["selected"] == true => Some((tr("in use"), t.accent)),
                        "installed" => Some((tr("installed"), t.text_dim)),
                        "found" => Some((tr("found on this computer"), t.text_dim)),
                        "damaged" => Some((tr("damaged"), t.caution)),
                        _ if m["default"] == true => Some((tr("recommended"), t.text_dim)),
                        _ => None,
                    };
                    if let Some((tag, color)) = tag {
                        ui.label(RichText::new(tag).color(color));
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new(licence_line(m)).font(t.font(11.5)).color(t.text_dim));
                    if let Some(url) = m["licence"]["url"].as_str() {
                        let r = ui.link(RichText::new(tr("Licence")).font(t.font(11.5)));
                        register(ui, format!("content:licence:{id}"), r.rect);
                        if r.clicked() {
                            open_link(app, ui.ctx(), url);
                        }
                    }
                    if let Some(url) = m["source"].as_str() {
                        let r = ui.link(RichText::new(tr("Model page")).font(t.font(11.5)));
                        register(ui, format!("content:page:{id}"), r.rect);
                        if r.clicked() {
                            open_link(app, ui.ctx(), url);
                        }
                    }
                });
                let test = &m["selfTest"];
                if test["ok"] == true {
                    let line = match test["msPerImage"].as_f64() {
                        Some(ms) => tr_format!("Works · {ms} ms per photo", ms = ms.round() as u64),
                        None => tr("Works").to_string(),
                    };
                    ui.label(RichText::new(line).font(t.font(11.5)).color(t.text_dim));
                } else if test["ok"] == false {
                    let why = sentence(test["error"].as_str().unwrap_or("Failed its last test"));
                    ui.add(egui::Label::new(RichText::new(why).font(t.font(11.5)).color(t.caution)).wrap());
                }
                if let Some(p) = m["problem"].as_str() {
                    ui.add(egui::Label::new(RichText::new(sentence(p)).font(t.font(11.5)).color(t.caution)).wrap());
                }
                if !supported && state != "installed" {
                    ui.label(RichText::new(tr("Not supported in this build")).font(t.font(11.5)).color(t.text_dim));
                }
                job_line(ui, t, &id, m);
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let button = |ui: &mut egui::Ui, what: &str, label: &str| {
                    let r = ui.button(tr(label));
                    register(ui, format!("content:{what}:{id}"), r.rect);
                    r.clicked()
                };
                if running && job["kind"] == "download" {
                    if button(ui, "cancel", "Cancel") {
                        let _ = app.run("content.models.downloadCancel", json!({"id": id}));
                        forget(ui.ctx());
                    }
                    return;
                }
                if running {
                    return;
                }
                match state {
                    "installed" => {
                        if button(ui, "remove", "Remove") {
                            run_and_refresh(app, ui.ctx(), "content.models.remove", json!({"id": id}));
                        }
                        if supported && button(ui, "test", "Test") {
                            run_and_refresh(app, ui.ctx(), "content.models.test", json!({"id": id}));
                        }
                        if m["selected"] != true && button(ui, "use", "Use") {
                            run_and_refresh(app, ui.ctx(), "content.models.select", json!({"id": id}));
                        }
                    }
                    "found" => {
                        if button(ui, "remove", "Remove") {
                            run_and_refresh(app, ui.ctx(), "content.models.remove", json!({"id": id}));
                        }
                        if supported && button(ui, "download", "Check and Use") {
                            open_dialog(app, m);
                        }
                    }
                    _ => {
                        if state == "damaged" && button(ui, "remove", "Remove") {
                            run_and_refresh(app, ui.ctx(), "content.models.remove", json!({"id": id}));
                        }
                        if supported && m["downloadHost"].is_string() {
                            let label = if job["state"] == "failed" { "Try Again" } else { "Download" };
                            if button(ui, "download", label) {
                                open_dialog(app, m);
                            }
                        }
                    }
                }
            });
        });
    });
}

fn run_and_refresh(app: &mut LightcraftApp, ctx: &egui::Context, id: &str, params: Value) {
    if let Err(e) = app.run(id, params) {
        app.toast(ctx, e);
    }
    forget(ctx);
}

/// A running download's progress, or how the last job ended.
fn job_line(ui: &mut egui::Ui, t: &Tokens, id: &str, m: &Value) {
    let job = &m["job"];
    match (job["kind"].as_str(), job["state"].as_str()) {
        (Some("download"), Some("running")) => {
            let (got, total) = (job["bytes"].as_u64().unwrap_or(0), job["total"].as_u64().unwrap_or(0));
            let frac = if total > 0 { (got as f32 / total as f32).clamp(0.0, 1.0) } else { 0.0 };
            let text = if m["state"] == "found" || (total > 0 && got >= total) {
                tr("Checking the files…").to_string()
            } else {
                tr_format!(
                    "{got} of {total} from {host}",
                    got = size(Some(got)),
                    total = size(Some(total)),
                    host = m["downloadHost"].as_str().unwrap_or("")
                )
            };
            let bar = ui.add(egui::ProgressBar::new(frac).desired_width(280.0).text(RichText::new(text).font(t.font(11.5))));
            register(ui, format!("content:progress:{id}"), bar.rect);
        }
        (Some("test"), Some("running")) => {
            ui.label(RichText::new(tr("Testing…")).font(t.font(11.5)).color(t.text_dim));
        }
        (_, Some("failed")) if m["state"] != "installed" || job["kind"] == "test" => {
            let why = sentence(job["error"].as_str().unwrap_or("It failed"));
            ui.add(egui::Label::new(RichText::new(why).font(t.font(11.5)).color(t.caution)).wrap());
        }
        _ => {}
    }
}

/// Download (or Check and Use): the model's terms first. Nothing is fetched before they are accepted.
pub fn open_dialog(app: &mut LightcraftApp, m: &Value) {
    app.ui.dialog = Some(crate::state::Dialog::ContentModel { info: m.clone(), accepted: false });
}

/// The dialog's title.
pub fn dialog_title(info: &Value) -> &'static str {
    if info["state"] == "found" { "Use Search Model" } else { "Download Search Model" }
}

/// The dialog's OK button.
pub fn ok_label(info: &Value) -> &'static str {
    if info["state"] == "found" { "Check and Use" } else { "Download" }
}

/// The body of the dialog: what the model is, where it comes from, its terms, and the accept box.
pub fn model_dialog(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, m: &Value, accepted: &mut bool) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(m["name"].as_str().unwrap_or("")).font(t.semibold(14.0)).color(t.text));
        ui.label(RichText::new(size(m["sizeBytes"].as_u64())).color(t.text_dim));
    });
    let what = if m["state"] == "found" {
        tr("Its files are already on this computer: LightCraft checks them (nothing is downloaded), then starts using it.").to_string()
    } else {
        tr_format!(
            "Downloads {size} from {host}, once. Each file is checked against its recorded checksum; then LightCraft starts using it.",
            size = size(m["sizeBytes"].as_u64()),
            host = m["downloadHost"].as_str().unwrap_or("")
        )
    };
    ui.add(egui::Label::new(RichText::new(what).color(t.text_label)).wrap());
    ui.label(RichText::new(licence_line(m)).font(t.semibold(12.5)).color(if m["licence"]["commercial"] == true { t.text } else { t.caution }));
    if m["publisher"].is_string() {
        ui.add(
            egui::Label::new(
                RichText::new(tr("Made by Google and published under the Apache License 2.0, which allows any use, commercial included."))
                    .color(t.text_label),
            )
            .wrap(),
        );
    }
    ui.horizontal(|ui| {
        if let Some(url) = m["licence"]["url"].as_str() {
            let r = ui.link(tr("Read the licence"));
            register(ui, "contentModel:licence", r.rect);
            if r.clicked() {
                open_link(app, ui.ctx(), url);
            }
        }
        if let Some(url) = m["source"].as_str() {
            let r = ui.link(tr("Open the model's page"));
            register(ui, "contentModel:page", r.rect);
            if r.clicked() {
                open_link(app, ui.ctx(), url);
            }
        }
    });
    ui.add_space(4.0);
    check(ui, "contentModel.accept", accepted, "I have read these terms and accept them");
    ui.label(
        RichText::new(tr("The model stays on this computer. LightCraft never uploads your photos or what you search for."))
            .font(t.font(11.5))
            .color(t.text_dim),
    );
}

/// The dialog's OK: start the download (or the check) once the terms are accepted, and go back to Settings ▸ Search
/// where its progress shows.
pub fn confirm(app: &mut LightcraftApp, m: &Value, accepted: bool) -> Result<Value, String> {
    if !accepted {
        return Err(tr("Tick the box to accept the model's terms first").to_string());
    }
    let id = m["id"].as_str().unwrap_or("");
    let r = app.run("content.models.download", json!({"id": id, "acknowledged": true}));
    if r.is_ok() {
        app.ui.dialog = Some(crate::state::Dialog::Settings { tab: "search".into() });
    }
    r
}
