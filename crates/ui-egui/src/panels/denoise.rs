//! AI denoise in the interface: the line under the Detail panel's Denoise slider (what the picture is doing), the
//! Settings ▸ Denoise tab (the model and the cache) and the per-frame pump that keeps the engine's background work going.
//!
//! The Amount slider is an ordinary develop control (`enhance.denoise`); everything here is about the cleaned picture the
//! slider mixes in. It is cached data made in the background for the photos being looked at, and never a file in the
//! library. A model is fetched only when the user presses Download and accepts its terms.

use crate::i18n::{tr, tr_format};
use egui::RichText;
use lcc_catalog::PhotoId;
use lcc_engine::denoise::PhotoState;
use serde_json::{Value, json};

use super::settings::{check, choices, heading, hint, row};
use crate::LccApp;
use crate::theme::Tokens;
use crate::widgets::register;

/// How long a read of the model list is reused (it is a few small files, but not for every frame).
const REFRESH_SECS: f64 = 1.5;
/// Cache limits offered (GB).
const CACHE_LIMITS: [u32; 6] = [5, 10, 20, 50, 100, 250];

/// What the interface keeps between frames.
#[derive(Default)]
pub struct Ui {
    next_pump: f64,
    generation: u64,
    active: bool,
    /// (tiles done, tiles in all) of the photo being made.
    running: Option<(u64, u64)>,
    queued: u64,
    /// Bumped when a model is installed, removed or chosen, so Settings re-reads the list at once.
    pub epoch: u64,
    /// Models the user pressed Download for that have not been installed yet.
    dl_watch: Vec<String>,
    list: Option<(u64, f64, Value)>,
    /// Why the last file picked with Install from file… was refused, shown under the button.
    rejected: Option<String>,
}

/// Called every frame: keeps the engine's denoise work going and asks for a redraw when a photo's picture appears (the
/// loupe and the thumbnails then render from it) or while one is being made (the progress).
pub fn pump(app: &mut LccApp, ctx: &egui::Context) {
    watch_downloads(app, ctx);
    let now = ctx.input(|i| i.time);
    let (working, moving) = ctx.input(|i| {
        let keys = i.events.iter().any(|e| matches!(e, egui::Event::Key { .. } | egui::Event::Text(_) | egui::Event::MouseWheel { .. }));
        (i.pointer.any_down() || keys, i.pointer.is_moving())
    });
    if working {
        app.caches.last_input = now;
    }
    if moving || working {
        app.caches.last_move = now;
    }
    if now < app.caches.denoise.next_pump {
        return;
    }
    let watching = matches!(&app.ui.dialog, Some(crate::state::Dialog::Settings { tab }) if tab == "denoise");
    let pace = window_pace(app, ctx, now, watching);
    let Ok(v) = app.session.execute("denoise.pump", &json!({"pace": pace})) else { return };
    let c = &mut app.caches.denoise;
    let generation = v["generation"].as_u64().unwrap_or(0);
    if generation != c.generation {
        c.generation = generation;
        ctx.request_repaint();
    }
    c.active = v["active"] == true;
    c.queued = v["queued"].as_u64().unwrap_or(0);
    c.running = v["running"].is_object().then(|| (v["running"]["done"].as_u64().unwrap_or(0), v["running"]["total"].as_u64().unwrap_or(0)));
    let busy = c.running.is_some() || c.queued > 0;
    c.next_pump = now
        + if busy {
            0.1
        } else if c.active {
            0.5
        } else {
            1.0
        };
    // frames are drawn only when something asks for one: while a photo is being made, ask again for its progress
    if busy {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}

/// The engine's model list, re-read at most every [`REFRESH_SECS`], or at once after an action changed it.
fn models(app: &mut LccApp, ctx: &egui::Context) -> Value {
    let now = ctx.input(|i| i.time);
    let epoch = app.caches.denoise.epoch;
    if let Some((e, at, v)) = &app.caches.denoise.list
        && *e == epoch
        && now - at < REFRESH_SECS
    {
        return v.clone();
    }
    let v = app.run("denoise.models.list", json!({})).unwrap_or(Value::Null);
    app.caches.denoise.list = Some((epoch, now, v.clone()));
    v
}

/// Follows the downloads the user started: keeps redrawing while one runs (the progress bar), and says so when a
/// model has been installed and is in use (the engine does all of that by itself).
fn watch_downloads(app: &mut LccApp, ctx: &egui::Context) {
    if app.caches.denoise.dl_watch.is_empty() {
        return;
    }
    let Ok(v) = app.session.execute("denoise.models.downloads", &json!({})) else {
        app.caches.denoise.dl_watch.clear();
        return;
    };
    let rows: Vec<Value> = v["downloads"].as_array().cloned().unwrap_or_default();
    let (mut keep, mut installed) = (Vec::new(), Vec::new());
    for id in std::mem::take(&mut app.caches.denoise.dl_watch) {
        match rows.iter().find(|r| r["id"] == id.as_str()).and_then(|r| r["state"].as_str()) {
            Some("running" | "done" | "installing") => keep.push(id),
            Some("installed") => installed.push(id),
            _ => {}
        }
    }
    app.caches.denoise.dl_watch = keep;
    for id in installed {
        let _ = app.session.execute("denoise.models.downloadCancel", &json!({"id": id}));
        app.caches.denoise.epoch += 1;
        let text = tr("The denoise model is installed and in use: the AI Denoise Amount slider under Detail now works");
        app.ui.status = text.into();
        app.toast(ctx, text);
    }
    ctx.request_repaint_after(std::time::Duration::from_millis(120));
}

/// Per-photo AI Denoise switch.
pub fn detail_toggle(app: &mut LccApp, ui: &mut egui::Ui, id: PhotoId, on: bool, applicable: bool) {
    let mut enabled = on;
    egui::Frame::NONE.inner_margin(crate::widgets::row_margin(ui, egui::Margin { left: 12, right: 16, top: 6, bottom: 2 }, 3, 2)).show(ui, |ui| {
        let mut r = ui.add_enabled(applicable, egui::Checkbox::new(&mut enabled, crate::i18n::tr("AI Denoise")));
        if !applicable && let Some((why, detail)) = not_applicable(app, id) {
            let tip = match detail {
                Some(d) => format!("{} {d}", tr(why)),
                None => tr(why).to_string(),
            };
            r = r.on_disabled_hover_text(tip);
        }
        register(ui.ctx(), "denoise:enabled", r.rect);
        if r.changed() {
            let params = if app.session.active() == Some(id) { json!({"enabled": enabled}) } else { json!({"id": id.0, "enabled": enabled}) };
            let _ = app.run("denoise.toggle", params);
        }
    });
}

/// The bar under the Denoise slider while a photo's picture is made: at most 240 px wide, and
/// sweeping while the number of tiles is not known yet. The percentage is `progress::percent`, as the
/// bar's accessible value is: 100 % only once done. (The panel shows the job in the activity
/// stack instead; the bar stays for the progress-bar tests.)
#[cfg(test)]
pub(crate) fn detail_bar(ui: &mut egui::Ui, done: u64, total: u64) -> egui::Response {
    let fraction = crate::progress::fraction_of(done, total);
    let text = match fraction {
        Some(f) => tr_format!("Making the denoised picture… {}%", crate::progress::percent(f)),
        None => tr("Starting…").to_string(),
    };
    crate::progress::Progress::new(fraction).label(text).width(ui.available_width().min(240.0)).widget("denoise:progress").show(ui)
}

/// The bar in Settings, AI Denoise: 380 px wide however wide the tab is.
pub(crate) fn settings_bar(ui: &mut egui::Ui, done: u64, total: u64, text: String) -> egui::Response {
    crate::progress::Progress::new(crate::progress::fraction_of(done, total)).label(text).width(380.0).widget("denoise:settingsProgress").show(ui)
}

/// The line under the Denoise slider for the photo `id`, whose Amount is `amount`.
pub fn detail_status(app: &mut LccApp, ui: &mut egui::Ui, id: PhotoId, amount: f64) {
    let t = Tokens::get(ui.ctx());
    let state = app.session.denoise_photo_state(id);
    let pad = egui::Margin { left: 12, right: 16, top: 0, bottom: 6 };
    let note = |ui: &mut egui::Ui, text: &str, color: egui::Color32| {
        ui.add(egui::Label::new(RichText::new(crate::i18n::tr(text)).size(11.0).color(color)).wrap());
    };
    match state {
        PhotoState::Ready => {}
        PhotoState::NotApplicable => {
            // the switch is greyed out: say why, or it looks broken
            if let Some((why, detail)) = not_applicable(app, id) {
                egui::Frame::NONE.inner_margin(pad).show(ui, |ui| {
                    note(ui, why, t.text_dim);
                    if let Some(d) = detail {
                        ui.add(egui::Label::new(RichText::new(d).size(11.0).color(t.text_dim)).wrap());
                    }
                });
            }
        }
        // Waiting, in line and being made are passing states of background work: they add no line here (it would
        // push every control under it down and back up while the user drags a slider); the activity stack shows the
        // progress, with the photo's file name.
        PhotoState::Idle => {
            let held = app.session.denoise_held(id);
            if amount > 0.0 && app.session.denoise_auto() && !held {
                // the pump has not met this photo yet: let it come round soon instead of waiting out its pause
                let now = ui.ctx().input(|i| i.time);
                app.caches.denoise.next_pump = app.caches.denoise.next_pump.min(now + 0.05);
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(60));
            } else if amount > 0.0 {
                egui::Frame::NONE.inner_margin(pad).show(ui, |ui| {
                    let why = if held {
                        "Its denoised picture is not made yet: AI Denoise was stopped for it."
                    } else {
                        "Its denoised picture is not made yet, and pictures are not made on their own (Settings, AI Denoise)."
                    };
                    note(ui, why, t.text_dim);
                    ui.add_space(2.0);
                    let r = ui.small_button(crate::i18n::tr("Make it now"));
                    register(ui.ctx(), "denoise:make", r.rect);
                    if r.clicked() {
                        let _ = app.run("denoise.queue", json!({"ids": [id.0]}));
                    }
                });
            }
        }
        PhotoState::Queued { .. } | PhotoState::Running { .. } => {}
        PhotoState::Failed { why, unsupported } => {
            egui::Frame::NONE.inner_margin(pad).show(ui, |ui| {
                let color = if unsupported { t.text_dim } else { t.caution };
                note(ui, &sentence(&why), color);
                if !unsupported {
                    let r = ui.small_button(crate::i18n::tr("Try again"));
                    register(ui.ctx(), "denoise:retry", r.rect);
                    if r.clicked() {
                        let _ = app.run("denoise.queue", json!({"ids": [id.0], "retry": true}));
                    }
                }
            });
        }
    }
}

/// The Settings ▸ Denoise tab.
pub fn settings_tab(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens) {
    let list = models(app, ui.ctx());
    super::settings::heading_help(ui, t, "AI Denoise", "denoise");
    if list["dir"].is_null() {
        hint(
            ui,
            t,
            "AI Denoise runs automatically with the built-in denoiser. This build has nowhere to keep other denoise models. (The desktop app has.)",
        );
        return;
    }
    hint(
        ui,
        t,
        "Cleans noise out of raw photos before the picture is built: the AI Denoise Amount slider under Detail sets how much of it you see. \
         Your files are never changed and no extra files are made in your library: the cleaned picture is cached data that is made again when needed.",
    );
    let runtime = list["runtime"].as_bool() == Some(true);
    if list["model"].is_null() {
        hint(ui, t, "In use: the built-in denoiser. It needs nothing installed, so turning AI Denoise on just works.");
    }
    if !runtime {
        hint(ui, t, "This build cannot run other denoise models.");
    }
    hint(
        ui,
        t,
        "Works on raw files from cameras with a standard (Bayer) sensor, and on JPEG, PNG, TIFF and other finished pictures with a model made for them. Other raw files keep their normal noise reduction.",
    );
    hint(ui, t, "Denoise models are downloaded and chosen under Settings ▸ AI Models.");
    if crate::dialog_frame::link(ui, Some("denoise:openAiModels"), "Open AI Models").clicked() {
        let _ = app.run("app.settings", json!({"tab": "ai"}));
    }
    work_section(app, ui, t, &list);
}

/// Models a person brings themselves: Install from file…, and the ones installed that way (Settings ▸ AI Models).
pub fn other_models_section(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens) {
    let list = models(app, ui.ctx());
    if list["dir"].is_null() {
        return;
    }
    let runtime = list["runtime"].as_bool() == Some(true);
    let all: Vec<Value> = list["models"].as_array().cloned().unwrap_or_default();
    let downloads = app
        .session
        .execute("denoise.models.downloads", &json!({}))
        .map(|v| v["downloads"].as_array().cloned().unwrap_or_default())
        .unwrap_or_default();
    super::settings::heading_help(ui, t, "Other models (optional)", "denoiseModels");
    if all.is_empty() {
        hint(ui, t, "Only if you were given a {app} denoise model (a folder with a .onnx file and denoise-model.json):");
    }
    let installing = downloads.iter().any(|d| d["id"] == "local-install" && d["state"].as_str() == Some("installing"));
    let r = crate::button::Button::new("Install from file…")
        .enabled(runtime && app.services.pick_denoise_model.is_some())
        .busy(installing)
        .widget("denoise:installFile")
        .show(ui);
    if r.clicked()
        && let Some(path) = app.services.pick_denoise_model.as_mut().and_then(|f| f().into_iter().next())
    {
        match app.session.execute("denoise.models.inspect", &json!({"path": path})) {
            Ok(info) => {
                app.caches.denoise.rejected = None;
                app.ui.dialog = Some(crate::state::Dialog::DenoiseModel { info, accepted: false });
            }
            Err(e) => app.caches.denoise.rejected = Some(plain(&e)),
        }
    }
    if let Some(why) = &app.caches.denoise.rejected {
        hint(ui, t, why);
    }
    if let Some(job) = downloads.iter().find(|d| d["id"] == "local-install") {
        match job["state"].as_str() {
            Some("installing") => hint(ui, t, "Checking and installing the model…"),
            Some("failed") => hint(ui, t, job["error"].as_str().unwrap_or("Model installation failed")),
            _ => {}
        }
    }
    // the models this app can download are listed with the other AI models; only the user's own are here
    for m in all.iter().filter(|m| m["known"] != true) {
        let dl = downloads.iter().find(|d| d["id"] == m["id"]);
        model_row(app, ui, t, m, dl, runtime);
    }
    if all.iter().all(|m| m["selected"] != true) && all.iter().any(|m| m["installed"] == true) {
        hint(ui, t, "The built-in denoiser is in use: press Use on a model to use that instead.");
    }
}

/// The AI Models dialog's AI Denoise and AI Super Resolution groups: each denoise model and upscaler the app knows how
/// to download, with its size and licence, under the same Download (terms first, then the activity stack), Use and Remove as Settings ▸ AI Denoise.
pub fn ai_models_section(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens) {
    let list = models(app, ui.ctx());
    let known: Vec<Value> = list["models"].as_array().map(|a| a.iter().filter(|m| m["known"] == true).cloned().collect()).unwrap_or_default();
    if list["dir"].is_null() || known.is_empty() {
        return;
    }
    let runtime = list["runtime"].as_bool() == Some(true);
    let downloads = app
        .session
        .execute("denoise.models.downloads", &json!({}))
        .map(|v| v["downloads"].as_array().cloned().unwrap_or_default())
        .unwrap_or_default();
    // denoise models and upscalers share the models folder, but each gets its own group under its own name
    let (upscalers, denoisers): (Vec<&Value>, Vec<&Value>) = known.iter().partition(|m| slot_kind(m) == "upscale");
    let groups = [
        (
            "AI Denoise",
            "Cleans noise out of raw photos and, with a model made for them, JPEG and other finished pictures (the AI Denoise Amount slider under Detail). Without a model the built-in denoiser is used.",
            denoisers,
        ),
        (
            "AI Super Resolution",
            "Enlarges or sharpens a photo with Photo ▸ Enhance…: AI Super Resolution and AI Sharpen. Without a model only the classical Super Resolution is offered.",
            upscalers,
        ),
    ];
    for (title, about, models) in groups {
        if models.is_empty() {
            continue;
        }
        crate::dialog_frame::group_begin(ui, Some(title));
        crate::dialog_frame::note(ui, RichText::new(tr(about)).color(t.text_dim));
        for m in models {
            let dl = downloads.iter().find(|d| d["id"] == m["id"]);
            model_row(app, ui, t, m, dl, runtime);
        }
        crate::dialog_frame::group_end(ui);
    }
}

/// What the cache holds, its limit, and what is being made.
fn work_section(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens, list: &Value) {
    let status = app.session.execute("denoise.status", &json!({})).unwrap_or(Value::Null);
    if status["enabled"] != true {
        return;
    }
    heading(ui, t, "Speed");
    let current = list["runOn"].as_str().unwrap_or("auto").to_string();
    let mut run_on = current.as_str();
    let options = [("auto", "Automatic"), ("gpu", "Graphics card"), ("cpu", "Processor")];
    if crate::dialog_frame::control_row(ui, |ui| choices(ui, "denoiseRunOn", &options, &mut run_on)) {
        let _ = app.run("denoise.settings", json!({"runOn": run_on}));
        app.caches.denoise.epoch += 1;
    }
    hint(
        ui,
        t,
        "Automatic uses the graphics card where it is faster than the processor: both are timed on this computer the first time a photo is made.",
    );
    hint(ui, t, &device_line(&status["device"], run_on));
    if run_on != "cpu" && status["device"]["retry"] == true {
        let r = crate::dialog_frame::control_row(ui, |ui| ui.small_button(crate::i18n::tr("Try the graphics card again")));
        register(ui.ctx(), "denoise:retryGpu", r.rect);
        if r.clicked() {
            // choosing where it runs, even the same again, forgets the failed set-up
            let _ = app.run("denoise.settings", json!({"runOn": run_on}));
            app.caches.denoise.epoch += 1;
        }
    }
    heading(ui, t, "Photos");
    let mut auto = list["auto"].as_bool().unwrap_or(true);
    if check(ui, "settings.denoiseAuto", &mut auto, "Make the denoised picture of the photos I am looking at") {
        let _ = app.run("denoise.settings", json!({"auto": auto}));
        app.caches.denoise.epoch += 1;
    }
    hint(ui, t, "Only photos whose AI Denoise amount is above 0 are made. Exports always make theirs if it is missing.");
    let (running, queued) = (app.caches.denoise.running, app.caches.denoise.queued);
    match running {
        Some((done, total)) => {
            let name = status["running"]["photo"]
                .as_u64()
                .and_then(|p| app.session.catalog.photo(PhotoId(p)).map(|p| p.file_name.clone()))
                .unwrap_or_default();
            let text = tr_format!(
                "Denoising {name}: {done} of {total} tiles{}",
                if queued > 0 { tr_format!(" · {queued} waiting", queued = queued) } else { String::new() },
                name = name,
                done = done,
                total = total
            );
            crate::dialog_frame::in_control_column(ui, |ui| settings_bar(ui, done, total, text));
        }
        None if queued > 0 => hint(ui, t, &count(queued, "photo waiting", "photos waiting")),
        None => {
            let ready = status["ready"].as_u64().unwrap_or(0);
            hint(ui, t, &count(ready, "photo has its denoised picture.", "photos have their denoised picture."))
        }
    }
    crate::dialog_frame::control_row(ui, |ui| {
        let r = ui.button(tr("Denoise all photos with an Amount"));
        register(ui.ctx(), "denoise:queueAll", r.rect);
        if r.clicked() {
            let _ = app.run("denoise.queue", json!({"scope": "withAmount"}));
        }
        if running.is_some() || queued > 0 {
            let r = ui.button(tr("Stop"));
            register(ui.ctx(), "denoise:stop", r.rect);
            if r.clicked() {
                let _ = app.run("denoise.cancel", json!({}));
            }
        }
    });
    heading(ui, t, "Cache");
    let (files, bytes) = (status["cache"]["files"].as_u64().unwrap_or(0), status["cache"]["bytes"].as_u64().unwrap_or(0));
    hint(
        ui,
        t,
        &tr_format!(
            "{} · {}. Kept in the catalog's “denoise” folder; the oldest ones that no photo uses go first when the limit is reached.",
            count(files, "cached picture", "cached pictures"),
            mb(Some(bytes))
        ),
    );
    row(ui, t, "Limit", |ui| {
        let current = list["cacheGb"].as_u64().unwrap_or(20) as u32;
        let mut choice = current;
        crate::widgets::combo_box("denoise-cache-limit").selected_text(tr_format!("{size} GB", size = current)).show_ui(ui, |ui| {
            for gb in CACHE_LIMITS {
                ui.selectable_value(&mut choice, gb, tr_format!("{size} GB", size = gb));
            }
        });
        if choice != current {
            let _ = app.run("denoise.settings", json!({"cacheGb": choice}));
            app.caches.denoise.epoch += 1;
        }
        let r = ui.button(tr("Clear cache"));
        register(ui.ctx(), "denoise:clear", r.rect);
        if r.clicked() {
            let _ = app.run("denoise.clear", json!({}));
        }
    });
}

/// Where the work runs, in a sentence (`denoise.status` → `device`), for the setting `run_on`. No timings: they are this
/// computer's, and only the comparison matters.
fn device_line(d: &Value, run_on: &str) -> String {
    match d["kind"].as_str() {
        _ if run_on == "cpu" => tr("The processor does the work.").into(),
        Some("gpu") => tr_format!("Running on {}.", d["adapter"].as_str().unwrap_or(tr("the graphics card"))),
        Some("cpu") => {
            tr_format!("Running on the processor: {}.", tr(d["reason"].as_str().unwrap_or("the graphics card is not used")).trim_end_matches('.'))
        }
        _ => tr("The graphics card is set up when the next photo is made.").into(),
    }
}

/// What kind of photo a model is made for, as its row says it.
fn made_for(m: &Value) -> &'static str {
    tr(match m["domain"].as_str() {
        Some("rgbToRgb") => "For JPEG and other finished pictures",
        Some("rgbToRgb4x") => "For Super Resolution and AI Sharpen",
        _ => "For raw photos",
    })
}

/// The settings slot a model fills (`kind` of `denoise.models.select`).
fn slot_kind(m: &Value) -> &'static str {
    match m["domain"].as_str() {
        Some("rgbToRgb") => "rgb",
        Some("rgbToRgb4x") => "upscale",
        _ => "raw",
    }
}

fn model_row(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens, m: &Value, dl: Option<&Value>, can_run: bool) {
    let id = m["id"].as_str().unwrap_or("").to_string();
    let (installed, selected) = (m["installed"].as_bool() == Some(true), m["selected"].as_bool() == Some(true));
    let dl_state = if installed { None } else { dl.and_then(|d| d["state"].as_str()) };
    let host = if can_run { m["downloadHost"].as_str().unwrap_or("").to_string() } else { String::new() };
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
                ui.label(RichText::new(made_for(m)).font(t.font(11.5)).color(t.text_dim));
                ui.label(RichText::new(licence_line(m)).font(t.font(11.5)).color(if m["licence"]["commercial"] == "yes" {
                    t.text_dim
                } else {
                    t.caution
                }));
                match m["accepted"]["selfTest"].as_object() {
                    Some(test) if test.get("ok").and_then(Value::as_bool) == Some(true) => {
                        ui.label(RichText::new(tr("Tested and works on this computer")).font(t.font(11.5)).color(t.text_dim));
                    }
                    Some(_) => {
                        ui.label(RichText::new(tr("Failed its last test")).font(t.font(11.5)).color(t.caution));
                    }
                    None => {}
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
                            .widget(format!("denoise:progress:{id}"))
                            .show(ui);
                    }
                    (Some("done" | "installing"), _) => {
                        ui.horizontal(|ui| {
                            crate::progress::spinner(ui, 12.0);
                            ui.label(RichText::new(tr("Downloaded and checked. Installing…")).font(t.font(11.5)).color(t.text_dim));
                        });
                    }
                    (Some("failed"), Some(d)) => {
                        let why = sentence(d["error"].as_str().unwrap_or("The download failed"));
                        ui.add(egui::Label::new(RichText::new(why).font(t.font(11.5)).color(t.caution)).wrap());
                    }
                    _ => {}
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if installed {
                    let r = ui.button(tr("Remove"));
                    register(ui.ctx(), format!("denoise:remove:{id}"), r.rect);
                    if r.clicked() {
                        let _ = app.run("denoise.models.remove", json!({"id": id}));
                        app.caches.denoise.epoch += 1;
                    }
                    if selected {
                        let r = ui.button(tr("Stop using"));
                        register(ui.ctx(), format!("denoise:off:{id}"), r.rect);
                        if r.clicked() {
                            let kind = slot_kind(m);
                            let _ = app.run("denoise.models.select", json!({"id": null, "kind": kind}));
                            app.caches.denoise.epoch += 1;
                        }
                    } else {
                        let r = ui.button(tr("Use"));
                        register(ui.ctx(), format!("denoise:use:{id}"), r.rect);
                        if r.clicked() {
                            let _ = app.run("denoise.models.select", json!({"id": id}));
                            app.caches.denoise.epoch += 1;
                        }
                    }
                } else {
                    match dl_state {
                        Some("running") => {
                            let r = ui.button(tr("Cancel"));
                            register(ui.ctx(), format!("denoise:cancelDownload:{id}"), r.rect);
                            if r.clicked() {
                                let _ = app.run("denoise.models.downloadCancel", json!({"id": id}));
                                app.caches.denoise.dl_watch.retain(|w| w != &id);
                            }
                        }
                        // the engine installs it within a moment; nothing to press
                        Some("done" | "installing") => {}
                        _ => {
                            if let Some(url) = m["source"].as_str() {
                                let r = ui
                                    .button(tr("Open page"))
                                    .on_hover_text(tr("Opens the model's own page in your browser, to read about it or get the file yourself."));
                                register(ui.ctx(), format!("denoise:get:{id}"), r.rect);
                                if r.clicked() {
                                    open_page(app, url);
                                }
                            }
                            if !host.is_empty() {
                                let label = if dl_state == Some("failed") { "Try again" } else { "Download" };
                                let tip = tr_format!(
                                    "Shows the model's terms, then downloads {} from {host}, installs it and starts using it. Nothing else is sent.",
                                    mb(m["sizeBytes"].as_u64()),
                                    host = host
                                );
                                let r = ui.button(tr(label)).on_hover_text(tip);
                                register(ui.ctx(), format!("denoise:download:{id}"), r.rect);
                                if r.clicked() {
                                    open_download_dialog(app, m);
                                }
                            }
                        }
                    }
                }
            });
        });
    });
}

/// Pressing Download: the model's terms first, in the licence dialog. Accepting them starts the download;
/// nothing is fetched before.
fn open_download_dialog(app: &mut LccApp, m: &Value) {
    let info = json!({
        "kind": "known",
        "domain": "denoise",
        "fileName": m["id"],
        "sizeBytes": m["sizeBytes"],
        "model": m,
        "alreadyInstalled": false,
        "download": m["id"],
        "host": m["downloadHost"],
        "assumptions": [],
        "reason": null,
    });
    app.ui.dialog = Some(crate::state::Dialog::DenoiseModel { info, accepted: false });
}

/// The dialog's OK for a denoise model: start the download and watch it.
pub fn install(app: &mut LccApp, info: &Value, accepted: bool) -> Result<Value, String> {
    if !accepted {
        return Err(tr("Tick the box to accept the model's terms first").into());
    }
    app.caches.denoise.epoch += 1;
    let (id, r) = if let Some(id) = info["download"].as_str() {
        (id.to_string(), app.run("denoise.models.download", json!({"id": id, "acknowledged": true})))
    } else {
        let path = info["path"].as_str().ok_or_else(|| tr("There is no model file to install").to_string())?;
        ("local-install".into(), app.run("denoise.models.install", json!({"path": path, "acknowledged": true, "background": true})))
    };
    if r.is_ok() {
        app.caches.denoise.dl_watch.push(id);
        app.ui.dialog = Some(crate::state::Dialog::Settings { tab: "ai".into() });
    }
    r
}

/// What an engine error says, without the command it came from (the install job's errors read the same way).
fn plain(e: &lcc_engine::EngineError) -> String {
    match e {
        lcc_engine::EngineError::BadParams { msg, .. } => msg.clone(),
        other => other.to_string(),
    }
}

/// "1 photo" / "3 photos": the count with the right noun.
fn count(n: u64, one: &str, many: &str) -> String {
    tr_format!("{n} {noun}", n = n, noun = tr(if n == 1 { one } else { many }))
}

pub(super) fn window_pace(app: &LccApp, ctx: &egui::Context, now: f64, watching: bool) -> &'static str {
    let (focused, minimized) = ctx.input(|i| (i.focused, i.raw.viewports.get(&i.raw.viewport_id).and_then(|v| v.minimized).unwrap_or(false)));
    pace_for(focused, minimized, now - app.caches.last_input, now - app.caches.last_move, watching)
}

fn pace_for(focused: bool, minimized: bool, worked: f64, moved: f64, watching: bool) -> &'static str {
    if minimized {
        "light"
    } else if !focused {
        // still on screen, but the user is working in another app: no input here to get in the way of
        "normal"
    } else if worked < 0.4 {
        "pause"
    } else if moved < 3.0 {
        "light"
    } else if watching {
        "full"
    } else {
        "normal"
    }
}

pub(super) fn mb(bytes: Option<u64>) -> String {
    match bytes {
        Some(b) if b >= 10_000_000 => tr_format!("{} MB", (b as f64 / 1e6).round() as u64),
        Some(b) if b >= 1_000_000 => tr_format!("{:.1} MB", b as f64 / 1e6),
        Some(b) => tr_format!("{} KB", (b as f64 / 1e3).round().max(1.0) as u64),
        None => String::new(),
    }
}

/// Why AI Denoise can't be used on photo `id`: a raw shown from its embedded preview, with the raw
/// decoder's reason, or a file that isn't a raw. `None` when there is nothing to explain (a photo
/// not in the library, in Recently Deleted, or one denoise does apply to).
pub(crate) fn not_applicable(app: &LccApp, id: PhotoId) -> Option<(&'static str, Option<String>)> {
    let p = app.session.catalog.photo(id)?;
    if p.deleted {
        return None;
    }
    match (&p.kind, &p.preview_only) {
        (lcc_catalog::MediaKind::Raw, Some(why)) => {
            Some(("AI Denoise needs the raw sensor data, and this file opens as its embedded preview.", Some(sentence(why))))
        }
        (lcc_catalog::MediaKind::Raw, None) => None,
        _ => Some(("AI Denoise works on raw files only.", None)),
    }
}

pub(super) fn sentence(s: &str) -> String {
    let s = s.trim();
    let mut out = crate::i18n::upper_first(s);
    if !out.is_empty() && !out.ends_with(['.', '!', '?']) {
        out.push('.');
    }
    out
}

pub(super) fn licence_line(m: &Value) -> String {
    let name = m["licence"]["name"].as_str().filter(|s| !s.is_empty()).unwrap_or("Unknown licence");
    let terms = match m["licence"]["commercial"].as_str() {
        Some("yes") if name.starts_with("GPL") => "commercial use subject to GPL conditions",
        Some("yes") => "commercial use allowed",
        Some("no") => "non-commercial use only",
        _ => "terms unclear",
    };
    tr_format!("{name} · {terms}", name = tr(name), terms = tr(terms))
}

pub(super) fn open_page(app: &mut LccApp, url: &str) {
    if let Some(f) = app.services.open_url.as_mut() {
        let _ = f(url);
    }
}

pub fn model_dialog(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens, info: &Value, accepted: &mut bool) {
    let kind = info["kind"].as_str().unwrap_or("unsupported");
    let file = info["fileName"].as_str().unwrap_or("");
    if kind == "unsupported" {
        ui.label(RichText::new(tr("This file cannot be used yet")).font(t.semibold(13.5)).color(t.caution));
        ui.add(
            egui::Label::new(
                RichText::new(sentence(tr(info["reason"].as_str().unwrap_or("It is not a denoise model {app} understands")))).color(t.text_label),
            )
            .wrap(),
        );
        ui.label(RichText::new(tr_format!("{file} · {size}", file = file, size = mb(info["sizeBytes"].as_u64()))).color(t.text_dim));
        return;
    }
    let m = &info["model"];
    ui.horizontal(|ui| {
        ui.label(RichText::new(m["name"].as_str().unwrap_or(file)).font(t.semibold(14.0)).color(t.text));
        ui.label(RichText::new(mb(info["sizeBytes"].as_u64())).color(t.text_dim));
    });
    if info["alreadyInstalled"] == true {
        ui.label(RichText::new(tr("Already installed. Remove it before installing a replacement.")).color(t.text_dim));
    }
    let download = info["download"].is_string();
    if download {
        let host = info["host"].as_str().unwrap_or("its own repository");
        let after = if info["domain"] == "denoise" { "starts using it for AI Denoise" } else { "makes AI Denoise available" };
        ui.add(
            egui::Label::new(
                RichText::new(tr_format!(
                    "Downloads from {host}. Once it has arrived and checked out, {app} installs it, {after}.",
                    host = host,
                    after = tr(after)
                ))
                .color(t.text_label),
            )
            .wrap(),
        );
    }
    let commercial = m["licence"]["commercial"].as_str().unwrap_or("unknown");
    ui.label(RichText::new(licence_line(m)).font(t.semibold(12.5)).color(if commercial == "yes" { t.text } else { t.caution }));
    let notice = m["licence"]["notice"].as_str().unwrap_or("");
    if !notice.is_empty() {
        ui.add(egui::Label::new(RichText::new(notice).color(t.text_label)).wrap());
    }
    let provenance = m["provenance"].as_str().unwrap_or("");
    if !provenance.is_empty() {
        ui.add(egui::Label::new(RichText::new(tr_format!("Trained on: {provenance}", provenance = provenance)).color(t.text_dim)).wrap());
    }
    if kind == "draft" {
        ui.add_space(2.0);
        ui.label(RichText::new(tr("{app} does not know this model, so it assumed:")).color(t.text_label));
        for a in info["assumptions"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            ui.add(egui::Label::new(RichText::new(tr_format!("•  {assumption}", assumption = tr(a))).font(t.font(12.0)).color(t.text_dim)).wrap());
        }
    }
    if let Some(url) = m["source"].as_str().or(m["licence"]["url"].as_str()) {
        let r = ui.link(tr("Open the model's page"));
        register(ui.ctx(), "denoiseModel:page", r.rect);
        if r.clicked() {
            open_page(app, url);
        }
    }
    ui.add_space(4.0);
    check(ui, "denoiseModel.accept", accepted, "I have read these terms and accept them for my own use");
    let closing = if download {
        "Nothing is fetched until you accept. The model is kept on this computer only; {app} never uploads or shares it."
    } else {
        "The model is kept on this computer only. {app} never uploads or shares it."
    };
    ui.label(RichText::new(tr(closing)).font(t.font(11.5)).color(t.text_dim));
}
