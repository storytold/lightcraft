//! Studio capture in the app (File ▸ Tethered Capture): the settings window that starts a session
//! (`tether.start` without a folder opens it), and while a session is active, the capture bar at
//! the top of the photo area (session, shots, Stop) and the scan every two seconds
//! (`tether.scan`). A new shot becomes the selection and the loupe shows it.

use serde_json::{Value, json};

use crate::DacApp;
use crate::theme::Tokens;
use crate::widgets::register;

/// Seconds between scans of the watched folder.
const SCAN_EVERY: f64 = 2.0;

#[derive(Clone, Default)]
struct Form {
    open: bool,
    folder: String,
    session: String,
    copy: bool,
    naming: String,
    collection: String,
    keywords: String,
    preset: String,
    metadata_preset: String,
}

fn form_id() -> egui::Id {
    egui::Id::new("tether-form")
}
fn status_id() -> egui::Id {
    egui::Id::new("tether-status")
}

fn str_of(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or_default().to_string()
}

/// UI side of the `tether.*` commands: `tether.start` without a folder opens the settings window.
pub fn run(app: &mut DacApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    if id != "tether.start" || p.get("folder").and_then(Value::as_str).is_some_and(|f| !f.trim().is_empty()) {
        return None;
    }
    let ctx = app.tasks.repaint.clone().unwrap_or_default();
    let cur = app.session.execute("tether.status", &json!({})).unwrap_or(Value::Null);
    let form = Form {
        open: true,
        folder: str_of(&cur, "folder"),
        session: if cur.is_null() { crate::i18n::tr("Studio Session").to_string() } else { str_of(&cur, "name") },
        copy: cur["copy"].as_bool().unwrap_or(true),
        naming: if cur.is_null() { "{session}-{seq:4}".into() } else { str_of(&cur, "naming") },
        collection: str_of(&cur, "collection"),
        keywords: cur["keywords"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")).unwrap_or_default(),
        preset: str_of(&cur, "preset"),
        metadata_preset: str_of(&cur, "metadataPreset"),
    };
    ctx.data_mut(|d| d.insert_temp(form_id(), form));
    ctx.request_repaint();
    Some(Ok(json!({"dialog": "tether.start"})))
}

/// Every frame: the settings window when open, and the capture bar plus scans while a session is
/// active.
pub fn show(app: &mut DacApp, ctx: &egui::Context) {
    settings_window(app, ctx);
    let now = ctx.input(|i| i.time);
    let (at, status) = ctx.data(|d| d.get_temp::<(f64, Value)>(status_id())).unwrap_or((-1.0, Value::Null));
    let status = if at < 0.0 || now - at >= SCAN_EVERY {
        let st = app.session.execute("tether.status", &json!({})).unwrap_or(Value::Null);
        if st["active"].as_bool() == Some(true) {
            scan(app, ctx);
        }
        let st = app.session.execute("tether.status", &json!({})).unwrap_or(Value::Null);
        ctx.data_mut(|d| d.insert_temp(status_id(), (now, st.clone())));
        st
    } else {
        status
    };
    if status["active"].as_bool() != Some(true) {
        return;
    }
    ctx.request_repaint_after(std::time::Duration::from_secs_f64(SCAN_EVERY));
    bar(app, ctx, &status);
}

fn scan(app: &mut DacApp, ctx: &egui::Context) {
    let r = match app.session.execute("tether.scan", &json!({})) {
        Ok(r) => r,
        Err(e) => return log::warn!("studio capture: {e}"),
    };
    let n = r["imported"].as_array().map(Vec::len).unwrap_or(0);
    if let Some(e) = r["error"].as_str() {
        app.toast_error(ctx, format!("{}: {e}", crate::i18n::tr("Studio capture")));
    } else if n > 0 {
        // the newest shot is selected: show it in the loupe
        if app.ui.module == crate::module::ModuleId::Library {
            let _ = app.run("view.detail", json!({}));
        }
        app.toast(ctx, format!("{} {n}", crate::i18n::tr("New shots:")));
    }
}

fn bar(app: &mut DacApp, ctx: &egui::Context, st: &Value) {
    let t = Tokens::get(ctx);
    let top = t.top_bar_h + 6.0;
    egui::Area::new(egui::Id::new("tether-bar")).anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, top)).order(egui::Order::Foreground).show(
        ctx,
        |ui| {
            egui::Frame::NONE
                .fill(t.chrome)
                .stroke(egui::Stroke::new(1.0, t.divider))
                .corner_radius(6.0)
                .inner_margin(egui::Margin::symmetric(10, 5))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let (dot, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                        ui.painter().circle_filled(dot.center(), 4.0, t.reject);
                        ui.label(
                            egui::RichText::new(format!("{}  ·  {}", crate::i18n::tr("Studio Capture"), str_of(st, "name"))).strong().size(12.0),
                        );
                        let shots = st["shots"].as_u64().unwrap_or(0);
                        ui.label(egui::RichText::new(format!("{shots} {}", crate::i18n::tr("shots"))).color(t.text_dim).size(11.5));
                        ui.label(egui::RichText::new(str_of(st, "folder")).color(t.text_dim).size(11.0));
                        let r = ui.button(crate::i18n::tr("Settings…"));
                        register(ui.ctx(), "tetherSettings", r.rect);
                        if r.clicked() {
                            let _ = run(app, "tether.start", &json!({}));
                        }
                        let r = ui.button(crate::i18n::tr("Stop"));
                        register(ui.ctx(), "tetherStop", r.rect);
                        if r.clicked() {
                            let _ = app.run("tether.stop", json!({}));
                            ui.ctx().data_mut(|d| d.remove::<(f64, Value)>(status_id()));
                        }
                    });
                });
        },
    );
}

fn settings_window(app: &mut DacApp, ctx: &egui::Context) {
    let Some(mut form) = ctx.data(|d| d.get_temp::<Form>(form_id())).filter(|f| f.open) else { return };
    let mut open = true;
    let mut start = false;
    egui::Window::new(crate::i18n::tr("Tethered Capture Settings"))
        .id(egui::Id::new("tether-settings"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            egui::Grid::new("tether-grid").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                ui.label(crate::i18n::tr("Watched folder"));
                let r = ui.add(
                    egui::TextEdit::singleline(&mut form.folder)
                        .desired_width(260.0)
                        .hint_text(crate::i18n::tr("Where the camera software saves shots")),
                );
                register(ui.ctx(), "tetherFolder", r.rect);
                ui.end_row();
                ui.label(crate::i18n::tr("Session name"));
                let r = ui.add(egui::TextEdit::singleline(&mut form.session).desired_width(260.0));
                register(ui.ctx(), "tetherSession", r.rect);
                ui.end_row();
                ui.label(crate::i18n::tr("Copy into library"));
                ui.checkbox(&mut form.copy, crate::i18n::tr("Copy shots into Originals/<session>"));
                ui.end_row();
                ui.label(crate::i18n::tr("File naming"));
                ui.add_enabled(form.copy, egui::TextEdit::singleline(&mut form.naming).desired_width(260.0).hint_text("{session}-{seq:4}"));
                ui.end_row();
                ui.label(crate::i18n::tr("Collection"));
                ui.add(egui::TextEdit::singleline(&mut form.collection).desired_width(260.0).hint_text(crate::i18n::tr("Default: the session name")));
                ui.end_row();
                ui.label(crate::i18n::tr("Keywords"));
                ui.add(egui::TextEdit::singleline(&mut form.keywords).desired_width(260.0).hint_text("studio, client"));
                ui.end_row();
                ui.label(crate::i18n::tr("Develop preset"));
                let presets: Vec<(String, String)> = app.session.presets.iter().map(|p| (p.id.clone(), p.name.clone())).collect();
                let shown =
                    presets.iter().find(|(id, _)| *id == form.preset).map(|(_, n)| n.clone()).unwrap_or_else(|| crate::i18n::tr("None").to_string());
                egui::ComboBox::from_id_salt("tether-preset").selected_text(shown).width(260.0).show_ui(ui, |ui| {
                    ui.selectable_value(&mut form.preset, String::new(), crate::i18n::tr("None"));
                    for (id, name) in &presets {
                        ui.selectable_value(&mut form.preset, id.clone(), name);
                    }
                });
                ui.end_row();
                ui.label(crate::i18n::tr("Metadata preset"));
                let metas: Vec<String> = app.session.metadata_presets.iter().map(|m| m.name.clone()).collect();
                let shown = if form.metadata_preset.is_empty() { crate::i18n::tr("None").to_string() } else { form.metadata_preset.clone() };
                egui::ComboBox::from_id_salt("tether-meta").selected_text(shown).width(260.0).show_ui(ui, |ui| {
                    ui.selectable_value(&mut form.metadata_preset, String::new(), crate::i18n::tr("None"));
                    for name in &metas {
                        ui.selectable_value(&mut form.metadata_preset, name.clone(), name);
                    }
                });
                ui.end_row();
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let r = ui.button(crate::i18n::tr("Start"));
                register(ui.ctx(), "tetherStart", r.rect);
                start = r.clicked();
            });
        });
    if start {
        let opt = |s: &str| if s.trim().is_empty() { Value::Null } else { json!(s.trim()) };
        let p = json!({
            "folder": form.folder.trim(), "session": form.session.trim(), "copy": form.copy, "naming": opt(&form.naming),
            "collection": opt(&form.collection), "keywords": form.keywords, "preset": opt(&form.preset), "metadataPreset": opt(&form.metadata_preset),
        });
        match app.run("tether.start", p) {
            Ok(_) => {
                form.open = false;
                ctx.data_mut(|d| d.remove::<(f64, Value)>(status_id()));
            }
            Err(e) => app.toast_error(ctx, e),
        }
    }
    if !open {
        form.open = false;
    }
    ctx.data_mut(|d| d.insert_temp(form_id(), form));
}
