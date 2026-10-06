//! Faces: the Settings ▸ Faces tab (the on/off toggle and the list of face models) and the dialog that
//! shows a model file's licence terms before it is installed.
//!
//! Adding a model is: pick or drop a `.onnx` file → the engine inspects it (`faces.models.inspect`) →
//! this dialog shows what it is and its terms → the user ticks "I accept" → `faces.models.install`.
//! LightCraft fetches a model only when the user presses Download on a model it has a pinned address for: the
//! same dialog shows its terms first, and accepting them downloads it (`faces.models.download`: checked against its
//! size and SHA-256), installs it, starts using it and switches recognition on, with no further question. "Open
//! page" opens the model's own page in the browser for anything else.

use egui::RichText;
use serde_json::{Value, json};

use super::settings::{check, heading, hint};
use crate::LightcraftApp;
use crate::theme::Tokens;
use crate::widgets::register;

/// How hard the background scan may work (the engine's `faces.pump` pace), from what the user is doing: nothing new
/// while they drag, type or scroll; one photo at a time while they are around or the window is out of sight; half the
/// machine once they have been idle for a few seconds; most of it while they are looking at the scan's progress.
fn scan_pace(app: &LightcraftApp, ctx: &egui::Context, now: f64) -> (&'static str, bool) {
    let (focused, minimized) = ctx.input(|i| (i.focused, i.raw.viewports.get(&i.raw.viewport_id).and_then(|v| v.minimized).unwrap_or(false)));
    let in_front = focused && !minimized;
    let watching =
        matches!(&app.ui.dialog, Some(crate::state::Dialog::Settings { tab }) if tab == "faces") || app.ui.view == crate::state::ViewMode::People;
    (pace_for(in_front, now - app.caches.last_input, now - app.caches.last_move, watching), in_front)
}

/// The pace for a window that is in front (or not), `worked` seconds after the user last dragged, typed or scrolled and
/// `moved` seconds after they last moved the pointer, while they are (or are not) looking at the scan's progress.
fn pace_for(in_front: bool, worked: f64, moved: f64, watching: bool) -> &'static str {
    if !in_front {
        "light"
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

/// How long a read of the model list is reused (it is a few small files, but not for every frame).
const REFRESH_SECS: f64 = 1.5;

fn list_id() -> egui::Id {
    egui::Id::new("faces-model-list")
}

/// The engine's model list, re-read at most every [`REFRESH_SECS`], or at once after an action changed it.
fn models(app: &mut LightcraftApp, ui: &egui::Ui) -> Value {
    let now = ui.input(|i| i.time);
    let epoch = app.caches.faces_epoch;
    if let Some((e, at, v)) = ui.data(|d| d.get_temp::<(u64, f64, Value)>(list_id()))
        && e == epoch
        && now - at < REFRESH_SECS
    {
        return v;
    }
    let v = app.run("faces.models.list", json!({})).unwrap_or(Value::Null);
    ui.data_mut(|d| d.insert_temp(list_id(), (epoch, now, v.clone())));
    v
}

/// A download-style size: decimal megabytes, as file managers and model pages show them.
fn mb(bytes: Option<u64>) -> String {
    match bytes {
        Some(b) if b >= 10_000_000 => format!("{} MB", (b as f64 / 1e6).round() as u64),
        Some(b) if b >= 1_000_000 => format!("{:.1} MB", b as f64 / 1e6),
        Some(b) => format!("{} KB", (b as f64 / 1e3).round().max(1.0) as u64),
        None => String::new(),
    }
}

/// "its input is not an image" → "Its input is not an image."
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

/// "Apache-2.0 · commercial use allowed"
fn licence_line(m: &Value) -> String {
    let name = m["licence"]["name"].as_str().filter(|s| !s.is_empty()).unwrap_or("Unknown licence");
    let terms = match m["licence"]["commercial"].as_str() {
        Some("yes") => "commercial use allowed",
        Some("no") => "non-commercial use only",
        _ => "terms unclear",
    };
    format!("{name} · {terms}")
}

fn open_page(app: &mut LightcraftApp, url: &str) {
    if let Some(f) = app.services.open_url.as_mut() {
        let _ = f(url);
    }
}

/// The Settings ▸ Faces tab.
pub fn settings_tab(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    let list = models(app, ui);
    heading(ui, t, "Face recognition");
    if list["dir"].is_null() {
        hint(ui, t, "This build has nowhere to keep face models, so the model list is not available. (The desktop app has.)");
        return;
    }
    let mut on = list["enabled"].as_bool().unwrap_or(false);
    if check(ui, "settings.facesEnabled", &mut on, "Recognise faces (experimental)") {
        let _ = app.run("faces.enable", json!({"enabled": on}));
        app.caches.faces_epoch += 1;
    }
    hint(ui, t, "Suggests who is in a photo from the faces you have named. Everything stays on your computer.");
    let runtime = list["runtime"].as_bool() == Some(true);
    if !runtime {
        hint(ui, t, "This build cannot run recognition models: they can be added and chosen, not used.");
    } else if on && list["embedder"].is_null() {
        hint(ui, t, "Choose a model below (Use) to start.");
    } else if on && app.caches.faces_active {
        scan_progress(app, ui, t);
    }
    let all: Vec<Value> = list["models"].as_array().cloned().unwrap_or_default();
    let downloads =
        app.session.execute("faces.models.downloads", &json!({})).map(|v| v["downloads"].as_array().cloned().unwrap_or_default()).unwrap_or_default();
    heading(ui, t, "Models");
    for m in all.iter().filter(|m| m["role"] == "embedder") {
        let dl = downloads.iter().find(|d| d["id"] == m["id"]);
        model_row(app, ui, t, m, dl, runtime);
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let can = app.services.pick_model_file.is_some();
        let r = ui.add_enabled(can, egui::Button::new("Add a model file…"));
        register(ui.ctx(), "faces:addModel", r.rect);
        if r.clicked() {
            let _ = app.run("dialog.faceModel", json!({}));
        }
        ui.label(RichText::new("or drop a .onnx file on the window").color(t.text_dim));
    });
    if let Some(d) = all.iter().find(|m| m["role"] == "detector") {
        ui.add_space(4.0);
        hint(ui, t, &format!("Faces are found by {} ({}, included).", d["name"].as_str().unwrap_or("the detector"), mb(d["sizeBytes"].as_u64())));
    }
}

/// How the scan is going: a bar while photos are left, then how many faces it learned.
fn scan_progress(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    let (left, peak, faces) = (app.caches.faces_pending, app.caches.faces_peak.max(1), app.caches.faces_indexed);
    if left == 0 {
        hint(ui, t, &format!("{faces} faces learned."));
        return;
    }
    let text = format!("Scanning photos for faces: {left} left · {faces} faces so far");
    let bar = ui.add(
        egui::ProgressBar::new((1.0 - left as f32 / peak as f32).clamp(0.0, 1.0)).desired_width(380.0).text(RichText::new(text).font(t.font(11.5))),
    );
    register(ui.ctx(), "faces:scanProgress", bar.rect);
}

fn model_row(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, m: &Value, dl: Option<&Value>, can_run: bool) {
    let id = m["id"].as_str().unwrap_or("").to_string();
    let (installed, selected) = (m["installed"].as_bool() == Some(true), m["selected"].as_bool() == Some(true));
    let dl_state = if installed { None } else { dl.and_then(|d| d["state"].as_str()) };
    // a build that cannot run recognition models has no use for a download
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
                        ui.label(RichText::new("in use").color(t.accent));
                    } else if installed {
                        ui.label(RichText::new("installed").color(t.text_dim));
                    }
                });
                ui.label(RichText::new(licence_line(m)).font(t.font(11.5)).color(if m["licence"]["commercial"] == "yes" {
                    t.text_dim
                } else {
                    t.caution
                }));
                if let Some(test) = m["accepted"]["selfTest"].as_object() {
                    let (line, ok) = match (test.get("ok").and_then(Value::as_bool), test.get("embedMs").and_then(Value::as_f64)) {
                        (Some(true), Some(ms)) => (format!("Works · {ms:.0} ms per face"), true),
                        _ => ("Failed its last test".to_string(), false),
                    };
                    ui.label(RichText::new(line).font(t.font(11.5)).color(if ok { t.text_dim } else { t.caution }));
                }
                match (dl_state, dl) {
                    (Some("running"), Some(d)) => {
                        let (got, total) = (d["bytes"].as_u64().unwrap_or(0), d["total"].as_u64().unwrap_or(0));
                        let frac = if total > 0 { (got as f32 / total as f32).clamp(0.0, 1.0) } else { 0.0 };
                        let text = if total > 0 && got >= total {
                            "Checking…".to_string()
                        } else {
                            format!("{} of {} from {host}", mb(Some(got)), mb(Some(total)))
                        };
                        let bar = ui.add(egui::ProgressBar::new(frac).desired_width(260.0).text(RichText::new(text).font(t.font(11.5))));
                        register(ui.ctx(), format!("faces:progress:{id}"), bar.rect);
                    }
                    (Some("done"), _) => {
                        ui.label(RichText::new("Downloaded and checked. Installing…").font(t.font(11.5)).color(t.text_dim));
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
                    if m["bundled"] != true {
                        let r = ui.button("Remove");
                        register(ui.ctx(), format!("faces:remove:{id}"), r.rect);
                        if r.clicked() {
                            let _ = app.run("faces.models.remove", json!({"id": id}));
                            app.caches.faces_epoch += 1;
                        }
                    }
                    if !selected {
                        let r = ui.button("Use");
                        register(ui.ctx(), format!("faces:use:{id}"), r.rect);
                        if r.clicked() {
                            let _ = app.run("faces.models.select", json!({"id": id}));
                            app.caches.faces_epoch += 1;
                        }
                    }
                } else {
                    match dl_state {
                        Some("running") => {
                            let r = ui.button("Cancel");
                            register(ui.ctx(), format!("faces:cancelDownload:{id}"), r.rect);
                            if r.clicked() {
                                let _ = app.run("faces.models.downloadCancel", json!({"id": id}));
                                app.caches.faces_dl_watch.retain(|w| w != &id);
                            }
                        }
                        // the engine installs it within a moment; nothing to press
                        Some("done") => {}
                        _ => {
                            if let Some(url) = m["source"].as_str() {
                                let r = ui
                                    .button("Open page")
                                    .on_hover_text("Opens the model's own page in your browser, to read about it or get the file yourself.");
                                register(ui.ctx(), format!("faces:get:{id}"), r.rect);
                                if r.clicked() {
                                    open_page(app, url);
                                }
                            }
                            if !host.is_empty() {
                                let label = if dl_state == Some("failed") { "Try again" } else { "Download" };
                                let tip = format!(
                                    "Shows the model's terms, then downloads {} from {host}, installs it and starts using it. Nothing else is sent.",
                                    mb(m["sizeBytes"].as_u64())
                                );
                                let r = ui.button(label).on_hover_text(tip);
                                register(ui.ctx(), format!("faces:download:{id}"), r.rect);
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

/// The body of the "Add Face Model" dialog: what the file is, its terms, and the accept box.
/// `info` is the engine's `faces.models.inspect` answer.
pub fn model_dialog(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, info: &Value, accepted: &mut bool) {
    let kind = info["kind"].as_str().unwrap_or("unsupported");
    let file = info["fileName"].as_str().unwrap_or("");
    if kind == "unsupported" {
        ui.label(RichText::new("This file cannot be used yet").font(t.semibold(13.5)).color(t.caution));
        ui.add(
            egui::Label::new(
                RichText::new(sentence(info["reason"].as_str().unwrap_or("It is not a face recognition model LightCraft understands")))
                    .color(t.text_label),
            )
            .wrap(),
        );
        ui.label(RichText::new(format!("{file} · {}", mb(info["sizeBytes"].as_u64()))).color(t.text_dim));
        return;
    }
    let m = &info["model"];
    ui.horizontal(|ui| {
        ui.label(RichText::new(m["name"].as_str().unwrap_or(file)).font(t.semibold(14.0)).color(t.text));
        ui.label(RichText::new(mb(info["sizeBytes"].as_u64())).color(t.text_dim));
    });
    if info["alreadyInstalled"] == true {
        ui.label(RichText::new("Already installed. Installing again is harmless.").color(t.text_dim));
    }
    let download = info["download"].is_string();
    if download {
        let host = info["host"].as_str().unwrap_or("its own repository");
        ui.add(
            egui::Label::new(
                RichText::new(format!(
                    "Downloads from {host}. Once it has arrived and checked out, LightCraft installs it, starts using it and turns face recognition on."
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
        ui.add(egui::Label::new(RichText::new(format!("Trained on: {provenance}")).color(t.text_dim)).wrap());
    }
    if kind == "draft" {
        ui.add_space(2.0);
        ui.label(RichText::new("LightCraft does not know this model, so it assumed:").color(t.text_label));
        for a in info["assumptions"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            ui.add(egui::Label::new(RichText::new(format!("•  {a}")).font(t.font(12.0)).color(t.text_dim)).wrap());
        }
    }
    if let Some(url) = m["source"].as_str().or(m["licence"]["url"].as_str()) {
        let r = ui.link("Open the model's page");
        register(ui.ctx(), "faceModel:page", r.rect);
        if r.clicked() {
            open_page(app, url);
        }
    }
    ui.add_space(4.0);
    check(ui, "faceModel.accept", accepted, "I have read these terms and accept them for my own use");
    let closing = if download {
        "Nothing is fetched until you accept. The model is kept on this computer only; LightCraft never uploads or shares it."
    } else {
        "The model is kept on this computer only. LightCraft never uploads or shares it."
    };
    ui.label(RichText::new(closing).font(t.font(11.5)).color(t.text_dim));
}

/// Pressing Download: the model's terms first, in the same dialog as a model file. Accepting them starts the download;
/// nothing is fetched before.
fn open_download_dialog(app: &mut LightcraftApp, m: &Value) {
    let info = json!({
        "kind": "known",
        "fileName": m["id"],
        "sizeBytes": m["sizeBytes"],
        "model": m,
        "alreadyInstalled": false,
        "download": m["id"],
        "host": m["downloadHost"],
        "assumptions": [],
        "reason": null,
    });
    app.ui.dialog = Some(crate::state::Dialog::FaceModel { path: String::new(), info, accepted: false });
}

/// Inspect `path` and open the dialog for it (also what dropping a `.onnx` file on the window does).
pub fn open_dialog(app: &mut LightcraftApp, path: &str) -> Result<Value, String> {
    let info = app.run("faces.models.inspect", json!({"path": path}))?;
    app.ui.dialog = Some(crate::state::Dialog::FaceModel { path: path.to_string(), info, accepted: false });
    Ok(Value::Null)
}

/// The dialog's OK (the engine refuses without the acceptance): for a download, start it and watch it; for a file,
/// install it. Either way the model ends up installed, in use, with recognition on.
pub fn install(app: &mut LightcraftApp, path: &str, info: &Value, accepted: bool) -> Result<Value, String> {
    if !accepted {
        return Err("Tick the box to accept the model's terms first".into());
    }
    app.caches.faces_epoch += 1;
    let back_to_settings = |app: &mut LightcraftApp| app.ui.dialog = Some(crate::state::Dialog::Settings { tab: "faces".into() });
    if let Some(id) = info["download"].as_str() {
        let r = app.run("faces.models.download", json!({"id": id, "acknowledged": true}));
        if r.is_ok() {
            app.caches.faces_dl_watch.push(id.to_string());
            // the progress is in Settings: stay there
            back_to_settings(app);
        }
        return r;
    }
    let r = app.run("faces.models.install", json!({"path": path, "acknowledged": true}));
    if let Ok(v) = &r {
        app.ui.status = format!("{} is installed and in use: face recognition is on", v["installed"]["name"].as_str().unwrap_or("The model"));
        back_to_settings(app);
    }
    r
}

// ---------------------------------------------------------------------------------------------------
// Naming faces in the loupe

/// A suggestion for one unnamed face: the best guess (when it passes the bar) and the closest few people.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Hint {
    pub suggestion: Option<(String, f32)>,
    pub candidates: Vec<(String, f32)>,
}

/// Suggestions for the unnamed faces of one photo, by region index.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Hints {
    pub by_index: std::collections::HashMap<usize, Hint>,
}

/// Suggestions for `photo`'s unnamed faces from what is already indexed (nothing is embedded for this: the background
/// indexer does that). `None` while recognition is off. Asked again only when the catalog or the index changed.
pub fn hints_for(app: &mut LightcraftApp, photo: u64) -> Option<std::sync::Arc<Hints>> {
    if !app.caches.faces_active {
        return None;
    }
    let (rev, indexed) = (app.session.catalog.revision, app.caches.faces_indexed);
    if let Some((p, r, i, h)) = &app.caches.face_hints
        && (*p, *r, *i) == (photo, rev, indexed)
    {
        return Some(h.clone());
    }
    let answer = app.session.execute("faces.suggest", &json!({"ids": [photo], "budgetMs": 0})).ok()?;
    let pair = |v: &Value| Some((v["name"].as_str()?.to_string(), v["score"].as_f64()? as f32));
    let mut hints = Hints::default();
    for f in answer["photos"][0]["faces"].as_array().into_iter().flatten() {
        let Some(index) = f["index"].as_u64().and_then(|i| usize::try_from(i).ok()) else { continue };
        hints.by_index.insert(
            index,
            Hint { suggestion: pair(&f["suggestion"]), candidates: f["candidates"].as_array().into_iter().flatten().filter_map(pair).collect() },
        );
    }
    let hints = std::sync::Arc::new(hints);
    app.caches.face_hints = Some((photo, rev, indexed, hints.clone()));
    Some(hints)
}

/// What the name box did this frame.
pub enum Editor {
    Open,
    Submit(String),
    Cancel,
}

/// The inline name box under a face: type a name (completed from the people already named), pick one of the
/// suggestions, Enter to confirm, Escape to cancel.
pub fn name_editor(
    ctx: &egui::Context,
    at: egui::Pos2,
    edit: &mut crate::state::NameEdit,
    candidates: &[(String, f32)],
    people: &[String],
) -> Editor {
    let t = Tokens::get(ctx);
    let mut outcome = Editor::Open;
    let opening = edit.fresh;
    let shown = egui::Area::new(egui::Id::new("face-name-editor")).order(egui::Order::Foreground).fixed_pos(at).constrain(true).show(ctx, |ui| {
        egui::Frame::popup(ui.style()).show(ui, |ui| {
            ui.set_min_width(230.0);
            let r = ui.add(egui::TextEdit::singleline(&mut edit.text).hint_text("Name").desired_width(220.0));
            register(ui.ctx(), "field:faceName", r.rect);
            if edit.fresh {
                r.request_focus();
                edit.fresh = false;
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                outcome = Editor::Submit(edit.text.trim().to_string());
            }
            let typed = edit.text.trim().to_lowercase();
            let mut offered: Vec<(String, Option<f32>)> = candidates.iter().take(3).map(|(n, s)| (n.clone(), Some(*s))).collect();
            if !typed.is_empty() {
                for p in people.iter().filter(|p| p.to_lowercase().starts_with(&typed)) {
                    if offered.len() >= 7 {
                        break;
                    }
                    if !offered.iter().any(|(n, _)| n.eq_ignore_ascii_case(p)) {
                        offered.push((p.clone(), None));
                    }
                }
            }
            for (name, score) in offered {
                let label = match score {
                    Some(s) => format!("{name}   {:.0}%", (s * 100.0).max(0.0)),
                    None => name.clone(),
                };
                let b = ui.add(egui::Button::new(RichText::new(label).color(t.text_label)).frame(false).min_size(egui::vec2(220.0, 0.0)));
                register(ui.ctx(), format!("faceName:pick:{name}"), b.rect);
                if b.clicked() {
                    outcome = Editor::Submit(name);
                }
            }
            ui.label(RichText::new("Enter to confirm, Esc to cancel").font(t.font(11.0)).color(t.text_dim));
        });
    });
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        outcome = Editor::Cancel;
    }
    // a click anywhere else closes it (not on the frame it opened, which is the click that opened it)
    if !opening && ctx.input(|i| i.pointer.any_pressed()) && ctx.input(|i| i.pointer.interact_pos()).is_some_and(|p| !shown.response.rect.contains(p))
    {
        outcome = Editor::Cancel;
    }
    outcome
}

/// Called every frame: keeps the background face indexer going while recognition is on, and notes whether it is
/// running, how many faces it has done and how many photos are left (the loupe's suggestions depend on it).
pub fn pump(app: &mut LightcraftApp, ctx: &egui::Context) {
    watch_downloads(app, ctx);
    let now = ctx.input(|i| i.time);
    let (working, moving) = ctx.input(|i| {
        let key_or_scroll = i.events.iter().any(|e| matches!(e, egui::Event::Key { .. } | egui::Event::Text(_) | egui::Event::MouseWheel { .. }));
        (i.pointer.any_down() || key_or_scroll, i.pointer.is_moving())
    });
    if working {
        app.caches.last_input = now;
    }
    if moving || working {
        app.caches.last_move = now;
    }
    if now < app.caches.faces_next_pump {
        return;
    }
    let (pace, in_front) = scan_pace(app, ctx, now);
    app.caches.faces_pace = pace;
    app.caches.faces_in_front = in_front;
    let Ok(v) = app.session.execute("faces.pump", &json!({"pace": pace})) else { return };
    app.caches.faces_active = v["active"] == true;
    app.caches.faces_indexed = v["indexedFaces"].as_u64().unwrap_or(0);
    app.caches.faces_pending = v["pendingPhotos"].as_u64().unwrap_or(0);
    // the progress bar's whole: the most photos that were left at once since the scan last finished
    app.caches.faces_peak = if app.caches.faces_pending == 0 { 0 } else { app.caches.faces_peak.max(app.caches.faces_pending) };
    // Frames are drawn only when something asks for one, so nothing here wakes the window needlessly: while there is work it
    // asks to be called again in 50 ms (the progress in Settings, the next photos for the workers); with recognition on and
    // nothing to do it looks again every few seconds (a few wake-ups a minute); with recognition off it asks for nothing
    // (and is called on every frame, which is cheap: whatever makes a frame, such as switching recognition on, is seen at once).
    let busy = app.caches.faces_active && (app.caches.faces_pending > 0 || v["inFlight"].as_u64().unwrap_or(0) > 0);
    let wait = match (busy, app.caches.faces_active) {
        (true, _) => Some(0.05),
        (false, true) => Some(5.0),
        (false, false) => None,
    };
    app.caches.faces_next_pump = now
        + if busy {
            0.05
        } else if app.caches.faces_active {
            1.0
        } else {
            0.0
        };
    if let Some(secs) = wait {
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(secs));
    }
}

/// Sort the watched downloads: those that have been installed (to announce), and those still to watch (running, or
/// arrived and being installed). A failed or cancelled one is dropped: the model's row in Settings says what happened.
fn due(rows: &[Value], watched: Vec<String>) -> (Vec<String>, Vec<String>) {
    let (mut installed, mut keep) = (Vec::new(), Vec::new());
    for id in watched {
        match rows.iter().find(|r| r["id"] == id.as_str()).and_then(|r| r["state"].as_str()) {
            Some("running" | "done") => keep.push(id),
            Some("installed") => installed.push(id),
            _ => {}
        }
    }
    (installed, keep)
}

/// Follows the downloads the user started: keeps redrawing while one runs (the progress bar), and says so when a
/// model has been installed, is in use and recognition is on (the engine does all of that by itself).
fn watch_downloads(app: &mut LightcraftApp, ctx: &egui::Context) {
    if app.caches.faces_dl_watch.is_empty() {
        return;
    }
    let Ok(v) = app.session.execute("faces.models.downloads", &json!({})) else {
        app.caches.faces_dl_watch.clear();
        return;
    };
    let rows: Vec<Value> = v["downloads"].as_array().cloned().unwrap_or_default();
    let (installed, keep) = due(&rows, std::mem::take(&mut app.caches.faces_dl_watch));
    app.caches.faces_dl_watch = keep;
    for id in installed {
        let name = app
            .session
            .execute("faces.models.list", &json!({}))
            .ok()
            .and_then(|l| {
                l["models"].as_array().and_then(|a| a.iter().find(|m| m["id"] == id.as_str()).and_then(|m| m["name"].as_str().map(str::to_string)))
            })
            .unwrap_or_else(|| id.clone());
        let _ = app.session.execute("faces.models.downloadCancel", &json!({"id": id}));
        app.caches.faces_epoch += 1;
        let text = format!("{name} is installed and in use: face recognition is on");
        app.ui.status = text.clone();
        app.toast(ctx, text);
    }
    ctx.request_repaint_after(std::time::Duration::from_millis(120));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scan_works_hard_only_when_nobody_is_in_the_way() {
        // (in front, seconds since a drag/key/scroll, seconds since the pointer moved, watching the progress)
        assert_eq!(pace_for(true, 0.1, 0.1, true), "pause", "dragging a slider: nothing new, even if watching");
        assert_eq!(pace_for(true, 2.0, 0.2, false), "light", "the pointer is moving: one photo at a time");
        assert_eq!(pace_for(true, 5.0, 2.9, true), "light");
        assert_eq!(pace_for(true, 5.0, 4.0, false), "normal", "idle for a few seconds: half the machine");
        assert_eq!(pace_for(true, 60.0, 60.0, true), "full", "idle and looking at the progress: most of it");
        // out of sight (minimized, another app in front): gentle, whatever the user did last
        assert_eq!([pace_for(false, 0.0, 0.0, true), pace_for(false, 99.0, 99.0, true)], ["light", "light"]);
    }

    fn row(id: &str, state: &str) -> Value {
        json!({"id": id, "state": state})
    }

    #[test]
    fn an_installed_download_is_announced_once_and_the_rest_are_followed() {
        let rows = vec![row("a", "running"), row("b", "done"), row("c", "installed"), row("d", "failed"), row("e", "cancelled")];
        let watched = ["a", "b", "c", "d", "e", "gone"].map(String::from).to_vec();
        let (installed, keep) = due(&rows, watched);
        assert_eq!(installed, ["c"]);
        // running and just-arrived ones are watched; failed, cancelled and vanished ones are dropped
        assert_eq!(keep, ["a", "b"]);
        assert_eq!(due(&[], vec!["x".into()]), (vec![], vec![]));
    }
}
