//! Search by content: the Settings ▸ Search tab (the indexing switch, how far indexing has got, its pace, Clear Index,
//! and the list of content models), the dialog that shows a model's licence before it is downloaded or used, and the
//! per-frame pump that keeps indexing going.
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

/// How a model compares with the others, from its manifest's hint: "Large · best search results, ~6× slower · multilingual".
/// Empty for a model without one (added from a catalog).
pub(crate) fn model_hint(m: &Value) -> String {
    let h = &m["hint"];
    let quality = match h["quality"].as_str() {
        Some("fastest") => tr("Small · fastest"),
        Some("fast") => tr("Base · fast"),
        Some("detailed") => tr("Base · finer detail"),
        Some("best") => tr("Large · best search results"),
        _ => return String::new(),
    };
    let mut line = quality.to_string();
    match h["relativeTime"].as_f64().filter(|r| r.is_finite() && *r > 0.0) {
        Some(r) if r >= 1.15 => line.push_str(&tr_format!(", ~{n}× slower", n = rounded(r))),
        Some(r) if r <= 0.87 => line.push_str(&tr_format!(", ~{n}× faster", n = rounded(1.0 / r))),
        _ => {}
    }
    line.push_str(" · ");
    line.push_str(if h["englishOnly"] == true { tr("English only") } else { tr("any language") });
    line
}

/// 2.4 → "2.4", 6.2 → "6", 13.6 → "14".
fn rounded(x: f64) -> String {
    if x < 3.0 { format!("{:.1}", (x * 10.0).round() / 10.0) } else { format!("{}", x.round() as u64) }
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
        forget_status(ui.ctx());
        app.caches.content_next_pump = 0.0;
    }
    match in_use {
        Some(m) if runtime => hint(ui, t, &tr_format!("Model in use: {name}", name = m["name"].as_str().unwrap_or(""))),
        _ if !runtime => hint(ui, t, "This build cannot run content models."),
        _ => hint(ui, t, "No model in use yet: download one below."),
    }
    if runtime && in_use.is_some() {
        index_section(app, ui, t, on);
        tags_section(app, ui, t);
    }
    heading(ui, t, "Models");
    hint(
        ui,
        t,
        "Each model keeps its own index of your photos: switching to another indexes the library once more with it, and switching back needs no new scan.",
    );
    for m in &all {
        model_row(app, ui, t, m, runtime);
    }
    for e in list["catalog"]["errors"].as_array().into_iter().flatten().filter_map(Value::as_str).take(3) {
        ui.add(egui::Label::new(RichText::new(e).font(t.font(11.5)).color(t.caution)).wrap());
    }
}

/// The index's status (`content.status`), re-read at most every [`REFRESH_SECS`] (the call also moves the scan along).
fn index_status(app: &mut LightcraftApp, ctx: &egui::Context) -> Value {
    let now = ctx.input(|i| i.time);
    let id = egui::Id::unique("content-index-status");
    if let Some((at, v)) = ctx.data(|d| d.get_temp::<(f64, Value)>(id))
        && now - at < REFRESH_SECS
        && now >= at
    {
        return v;
    }
    let v = app.run("content.status", json!({})).unwrap_or(Value::Null);
    ctx.data_mut(|d| d.insert_temp(id, (now, v.clone())));
    v
}

fn forget_status(ctx: &egui::Context) {
    ctx.data_mut(|d| d.remove::<(f64, Value)>(egui::Id::unique("content-index-status")));
}

/// "about 3 min", "about 2 h 10 min", "less than a minute".
fn time_left(secs: f64) -> String {
    let mins = (secs / 60.0).round() as u64;
    match mins {
        0 => tr("less than a minute left").to_string(),
        m if m < 60 => tr_format!("about {m} min left", m = m),
        m => tr_format!("about {h} h {m} min left", h = m / 60, m = m % 60),
    }
}

/// How far indexing has got, the pace, and Clear Index.
fn index_section(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, on: bool) {
    let st = index_status(app, ui.ctx());
    let (indexed, total) = (st["indexed"].as_u64().unwrap_or(0), st["total"].as_u64().unwrap_or(0));
    if let Some(e) = st["error"].as_str() {
        ui.add(egui::Label::new(RichText::new(sentence(e)).font(t.font(11.5)).color(t.caution)).wrap());
    }
    if on && st["indexing"] == true {
        // the tab keeps moving while the scan runs
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(REFRESH_SECS));
        let frac = if total > 0 { (indexed as f32 / total as f32).clamp(0.0, 1.0) } else { 0.0 };
        let mut text = tr_format!("{n} of {total} photos indexed", n = indexed, total = total);
        if let Some(eta) = st["etaSeconds"].as_f64() {
            text = format!("{text} · {}", time_left(eta));
        }
        let bar = ui.add(egui::ProgressBar::new(frac).desired_width(320.0).text(RichText::new(text).font(t.font(11.5))));
        register(ui, "content:indexProgress", bar.rect);
    } else if on && st["stopped"] == true {
        ui.horizontal(|ui| {
            ui.label(RichText::new(tr_format!("Stopped at {n} of {total} photos.", n = indexed, total = total)).font(t.font(11.5)).color(t.text_dim));
            let r = ui.button(tr("Resume"));
            register(ui, "content:resume", r.rect);
            if r.clicked() {
                if let Err(e) = app.run("content.enable", json!({"on": true})) {
                    app.toast(ui.ctx(), e);
                }
                forget_status(ui.ctx());
            }
        });
    } else if total > 0 && st["indexed"].is_u64() {
        let line = if indexed >= total {
            tr_format!("All {total} photos are indexed.", total = total)
        } else {
            tr_format!("{n} of {total} photos indexed.", n = indexed, total = total)
        };
        ui.label(RichText::new(line).font(t.font(11.5)).color(t.text_dim));
    }
    let (failed, offline) = (st["failed"].as_u64().unwrap_or(0), st["offline"].as_u64().unwrap_or(0));
    if failed > 0 {
        ui.label(RichText::new(tr_format!("{n} photos could not be read.", n = failed)).font(t.font(11.5)).color(t.text_dim));
    }
    if offline > 0 {
        ui.label(
            RichText::new(tr_format!("{n} photos are offline: they are indexed when their drive is back.", n = offline))
                .font(t.font(11.5))
                .color(t.text_dim),
        );
    }
    if let Some(e) = st["index"]["saveError"].as_str() {
        ui.add(egui::Label::new(RichText::new(sentence(e)).font(t.font(11.5)).color(t.caution)).wrap());
    }
    ui.horizontal(|ui| {
        ui.label(RichText::new(tr("Pace")).color(t.text_label));
        let mut pace = match st["pace"].as_str() {
            Some("low") => 0,
            Some("fast") => 2,
            _ => 1,
        };
        if super::settings::choices(ui, "contentPace", &[(0, "Low"), (1, "Normal"), (2, "Fast")], &mut pace) {
            let name = ["low", "normal", "fast"].get(pace).copied().unwrap_or("normal");
            if let Err(e) = app.run("content.pace", json!({"pace": name})) {
                app.toast(ui.ctx(), e);
            }
            forget_status(ui.ctx());
        }
    });
    hint(ui, t, "Low barely slows the computer down; Fast uses most of it and finishes sooner.");
    ui.horizontal(|ui| {
        let r = ui.button(tr("Clear Index"));
        register(ui, "content:clearIndex", r.rect);
        if r.clicked() {
            match app.run("content.clearIndex", json!({})) {
                Ok(_) => app.toast(ui.ctx(), tr("The index was cleared")),
                Err(e) => app.toast(ui.ctx(), e),
            }
            forget_status(ui.ctx());
        }
        if let Some(bytes) = st["index"]["bytes"].as_u64() {
            ui.label(RichText::new(tr_format!("Index: {size}", size = size(Some(bytes)))).font(t.font(11.5)).color(t.text_dim));
        }
    });
}

/// The auto-tag vocabulary's state and options (`content.vocabulary` + `content.tagOptions`), re-read at most every
/// [`REFRESH_SECS`].
fn tags_status(app: &mut LightcraftApp, ctx: &egui::Context) -> (Value, Value) {
    let now = ctx.input(|i| i.time);
    let id = egui::Id::unique("content-tags-status");
    if let Some((at, v, o)) = ctx.data(|d| d.get_temp::<(f64, Value, Value)>(id))
        && now - at < REFRESH_SECS
        && now >= at
    {
        return (v, o);
    }
    let v = app.run("content.vocabulary", json!({})).map(|mut v| {
        // (the terms are not needed here)
        if let Some(o) = v.as_object_mut() {
            o.remove("terms");
        }
        v
    });
    let o = app.run("content.tagOptions", json!({})).unwrap_or(Value::Null);
    let v = v.unwrap_or(Value::Null);
    ctx.data_mut(|d| d.insert_temp(id, (now, v.clone(), o.clone())));
    (v, o)
}

fn forget_tags(ctx: &egui::Context) {
    ctx.data_mut(|d| d.remove::<(f64, Value, Value)>(egui::Id::unique("content-tags-status")));
}

/// Auto-tags: how far they are, the keyword parent accepted tags go under, the cut-off (advanced) and the vocabulary.
fn tags_section(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    let (voc, opts) = tags_status(app, ui.ctx());
    heading(ui, t, "Auto-tags");
    hint(
        ui,
        t,
        "Photos get tags from a vocabulary of things they can show (beach, forest, bird, car…): filter by them in the Library Filter (Contents), or accept them as keywords in the Keywording panel.",
    );
    let count = voc["count"].as_u64().unwrap_or(0);
    let line = match voc["state"].as_str() {
        Some("preparing") => {
            ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(REFRESH_SECS));
            tr_format!("Preparing {n} tags…", n = count)
        }
        Some("ready") => tr_format!("{n} tags in the vocabulary.", n = count),
        Some("stopped") => tr("Preparing the tags was stopped.").to_string(),
        Some("noIndex") => tr("Tags appear once photos are indexed.").to_string(),
        Some("error") => sentence(voc["error"].as_str().unwrap_or("")),
        _ => String::new(),
    };
    if !line.is_empty() {
        let r = ui.label(RichText::new(line).font(t.font(11.5)).color(if voc["state"] == "error" { t.caution } else { t.text_dim }));
        register(ui, "label:contentTagsState", r.rect);
    }
    // the keyword parent of accepted tags
    ui.horizontal(|ui| {
        ui.label(RichText::new(tr("Accepted tags go under the keyword")).color(t.text_label));
        let id = egui::Id::unique("content-tag-parent");
        let current = opts["parent"].as_str().unwrap_or("Contents").to_string();
        let mut text: String = ui.data(|d| d.get_temp(id)).unwrap_or_else(|| current.clone());
        let r = crate::text_field::TextField::singleline("field:contentTagParent", &mut text).hint(tr("none")).width(140.0).show(ui);
        if r.committed() && text.trim() != current {
            if let Err(e) = app.run("content.tagOptions", json!({"parent": text.trim()})) {
                app.toast(ui.ctx(), e);
            }
            forget_tags(ui.ctx());
            ui.data_mut(|d| d.remove::<String>(id));
        } else if r.response.has_focus() {
            ui.data_mut(|d| d.insert_temp(id, text));
        } else {
            ui.data_mut(|d| d.remove::<String>(id));
        }
    });
    hint(ui, t, "Empty: the tag's name alone. Accepted tags are ordinary keywords: written to XMP and exported.");
    // the cut-off (advanced)
    egui::CollapsingHeader::new(tr("Advanced")).id_salt("content-tags-advanced").show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(tr("Tag cut-off")).color(t.text_label));
            let model = opts["modelThreshold"].as_f64().unwrap_or(0.01);
            let mut v = opts["threshold"].as_f64().unwrap_or(model);
            let r = ui.add(egui::Slider::new(&mut v, 0.001..=0.3).clamping(egui::SliderClamping::Edits).logarithmic(true).max_decimals(3));
            register(ui, "slider:contentTagThreshold", r.rect);
            if r.drag_stopped() || (r.changed() && !r.dragged()) {
                if let Err(e) = app.run("content.tagOptions", json!({"threshold": v})) {
                    app.toast(ui.ctx(), e);
                }
                forget_tags(ui.ctx());
            }
            if opts["custom"] == true {
                let b = ui.button(tr("Use the model's"));
                register(ui, "button:contentTagThresholdReset", b.rect);
                if b.clicked() {
                    let _ = app.run("content.tagOptions", json!({"threshold": Value::Null}));
                    forget_tags(ui.ctx());
                }
            }
        });
        hint(ui, t, "Lower shows more tags (and more wrong ones); higher shows fewer, surer ones.");
    });
    // the vocabulary
    let open = egui::CollapsingHeader::new(tr("Vocabulary")).id_salt("content-tags-vocabulary").show(ui, |ui| vocabulary_editor(app, ui, t, &voc));
    register(ui, "content:vocabulary", open.header_response.rect);
}

/// The vocabulary as text: one tag per line, `path|of|tag = synonym, synonym`.
fn vocabulary_editor(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, voc: &Value) {
    hint(ui, t, "One tag per line: animal|bird|owl, or animal|dog = dog, puppy (synonyms: one tag). Levels above a tag count its photos too.");
    let id = egui::Id::unique("content-vocab-text");
    let mut text: String = match ui.data(|d| d.get_temp(id)) {
        Some(t) => t,
        None => {
            let t =
                app.run("content.vocabulary", json!({"text": true})).ok().and_then(|v| v["text"].as_str().map(str::to_string)).unwrap_or_default();
            ui.data_mut(|d| d.insert_temp(id, t.clone()));
            t
        }
    };
    egui::ScrollArea::vertical().id_salt("content-vocab-scroll").max_height(220.0).show(ui, |ui| {
        crate::text_field::TextField::multiline("field:contentVocabulary", &mut text).rows(10).width(f32::INFINITY).font(t.font(11.5)).show(ui);
    });
    ui.data_mut(|d| d.insert_temp(id, text.clone()));
    ui.horizontal(|ui| {
        let save = ui.button(tr("Save Vocabulary"));
        register(ui, "button:contentVocabularySave", save.rect);
        if save.clicked() {
            match app.run("content.setVocabulary", json!({"text": text})) {
                Ok(r) => {
                    let notes = r["notes"].as_array().map_or(0, Vec::len);
                    let msg = if notes > 0 {
                        tr_format!("Vocabulary saved; {n} lines were skipped", n = notes)
                    } else {
                        tr("Vocabulary saved").to_string()
                    };
                    app.toast(ui.ctx(), msg);
                    ui.data_mut(|d| d.remove::<String>(id));
                }
                Err(e) => app.toast(ui.ctx(), e),
            }
            forget_tags(ui.ctx());
        }
        let reset = ui.add_enabled(voc["custom"] == true, egui::Button::new(tr("Restore Defaults")));
        register(ui, "button:contentVocabularyReset", reset.rect);
        if reset.clicked() {
            match app.run("content.setVocabulary", json!({"reset": true})) {
                Ok(_) => app.toast(ui.ctx(), tr("The built-in vocabulary is back")),
                Err(e) => app.toast(ui.ctx(), e),
            }
            ui.data_mut(|d| d.remove::<String>(id));
            forget_tags(ui.ctx());
        }
    });
    if voc["custom"] == true {
        hint(ui, t, "Your own vocabulary is in use (kept for every library).");
    }
}

/// Called every frame: keeps content indexing going while it is on. The scan's work runs on its own thread; this only
/// hands it the next photos and takes in what it finished, a few times a second while it runs (nothing new is started
/// while the user drags, types or scrolls), and about once a second otherwise.
pub fn pump(app: &mut LightcraftApp, ctx: &egui::Context, now: f64) {
    if now < app.caches.content_next_pump {
        return;
    }
    let pause = now - app.caches.last_input < 0.4;
    let Ok(v) = app.session.execute("content.pump", &json!({"pause": pause})) else { return };
    let indexing = v["indexing"] == true || v["inFlight"].as_u64().unwrap_or(0) > 0;
    if indexing != app.caches.content_indexing {
        // Settings ▸ Search shows the change at once
        forget_status(ctx);
    }
    app.caches.content_indexing = indexing;
    let wait = if indexing { 0.25 } else { 1.0 };
    app.caches.content_next_pump = now + wait;
    if indexing {
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(wait));
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
                let quality = model_hint(m);
                if !quality.is_empty() {
                    let r = ui.label(RichText::new(quality).font(t.font(11.5)).color(t.text_dim));
                    register(ui, format!("label:content:hint:{id}"), r.rect);
                }
                if let Some(n) = m["libraryIndex"]["photos"].as_u64().filter(|n| *n > 0) {
                    ui.label(RichText::new(tr_format!("Its index of this library: {n} photos", n = n)).font(t.font(11.5)).color(t.text_dim));
                }
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
                            // each model has its own index: say what switching does to it
                            let name = m["name"].as_str().unwrap_or(&id);
                            let msg = match app.run("content.models.select", json!({"id": id})) {
                                Err(e) => e,
                                Ok(_) if m["libraryIndex"]["photos"].as_u64().is_some_and(|n| n > 0) => {
                                    tr_format!("Using {name}: its own index of this library is back (the other model's is kept)", name = name)
                                }
                                Ok(_) => tr_format!("Using {name}: it indexes the library once (the other model's index is kept)", name = name),
                            };
                            app.toast(ui.ctx(), msg);
                            forget(ui.ctx());
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
    // (only the SigLIP 2 models are Google's, under Apache-2.0; the licence line above says what any other is under)
    if m["publisher"] == "Google" {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_model_says_what_it_is_for_and_what_it_costs() {
        let hint =
            |quality: &str, rel: f64, english: bool| model_hint(&json!({"hint": {"quality": quality, "relativeTime": rel, "englishOnly": english}}));
        assert_eq!(hint("fast", 1.0, false), "Base · fast · any language");
        assert_eq!(hint("fastest", 0.5, true), "Small · fastest, ~2.0× faster · English only");
        assert_eq!(hint("best", 7.5, false), "Large · best search results, ~8× slower · any language");
        assert_eq!(hint("detailed", 2.4, false), "Base · finer detail, ~2.4× slower · any language");
        // a model without a hint (from a catalog), or a nonsense one, says nothing more than its name
        assert_eq!(model_hint(&json!({"hint": null})), "");
        assert_eq!(model_hint(&json!({"hint": {"quality": "magic"}})), "");
        assert_eq!(hint("fast", f64::NAN, false), "Base · fast · any language");
        assert_eq!(hint("fast", -3.0, true), "Base · fast · English only");
    }
}
