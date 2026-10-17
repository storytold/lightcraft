//! The Plug-in Manager window (File ▸ Plug-in Manager…, P4.3) and File ▸ Plug-in Extras.
//!
//! - Install: the module is inspected first and every permission it asks for is listed with a
//!   checkbox, unchecked unless an installed version already has it. Only what the user ticks is
//!   granted; an update shows what it newly asks for.
//! - Per plug-in: enable/disable, toggle (revoke) each requested permission, remove, read the log,
//!   run its commands (with their declarative dialogs).
//! - After an export, enabled export hooks and the chosen export destination run on the written
//!   files, on a worker with an activity row ([`after_export`], [`poll`]).
//!
//! Every action is an engine `plugin.*` command. State lives in a static here (no field on the
//! app), so the shell only needs the calls in `run`, `show`, `menu_items` and `after_export`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use lcc_engine::activity::{Cancel, TaskGuard};
use serde_json::{Value, json};

use crate::LccApp;
use crate::i18n::tr;

/// An install waiting for the user's approval: the file, its inspection and the ticked grant.
#[derive(Clone)]
struct Pending {
    path: String,
    info: Value,
    grant: Value,
}

#[derive(Clone, Default)]
struct State {
    open: bool,
    path: String,
    pending: Option<Pending>,
    /// The command whose dialog is open: (plugin, command, values).
    dialog: Option<(String, String, Value)>,
    message: String,
}

fn key() -> egui::Id {
    egui::Id::new("plugin-manager")
}

/// Window state: one Plug-in Manager per process (the manager itself is process-wide).
static STATE: std::sync::Mutex<Option<State>> = std::sync::Mutex::new(None);

fn state(ctx: &egui::Context) -> State {
    let _ = ctx;
    STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone().unwrap_or_default()
}

fn set_state(ctx: &egui::Context, s: State) {
    let _ = ctx;
    *STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(s);
}

fn app_ctx(app: &LccApp) -> egui::Context {
    app.tasks.repaint.clone().unwrap_or_default()
}

/// UI commands: `plugins.manager {open?}` opens / closes the window; `plugins.install {path}`
/// inspects a module and opens the approval dialog; `plugins.runItem {plugin, command}` runs a
/// plug-in command from the menu (asking its dialog first, if it has one).
pub fn run(app: &mut LccApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    let ctx = app_ctx(app);
    let mut s = state(&ctx);
    let r = match id {
        "plugins.manager" => {
            s.open = p.get("open").and_then(Value::as_bool).unwrap_or(!s.open);
            Ok(json!({"open": s.open}))
        }
        "plugins.install" => {
            let path = p.get("path").and_then(Value::as_str).unwrap_or(&s.path).trim().to_string();
            begin_install(app, &mut s, &path).map(|_| json!({"pending": path}))
        }
        "plugins.runItem" => {
            let (Some(plugin), Some(command)) = (p.get("plugin").and_then(Value::as_str), p.get("command").and_then(Value::as_str)) else {
                return Some(Err("plugins.runItem needs `plugin` and `command`".into()));
            };
            let r = start_command(app, &mut s, plugin, command);
            if let Some(m) = r.as_ref().ok().and_then(|v| v.get("message")).and_then(Value::as_str) {
                app.toast(&ctx, m.to_string());
            }
            r
        }
        _ => return None,
    };
    set_state(&ctx, s);
    ctx.request_repaint();
    Some(r)
}

/// Inspects `path` and opens the approval dialog with everything unticked but what an installed
/// version already holds.
fn begin_install(app: &mut LccApp, s: &mut State, path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err(tr("Choose a plug-in module (.wasm) first").to_string());
    }
    let info = app.session.execute("plugin.inspectFile", &json!({"path": path})).map_err(|e| e.to_string())?;
    let grant = info["installed"]["granted"].clone();
    let grant = if grant.is_object() { grant } else { json!({"catalog": false, "metadataWrite": false, "network": [], "fs": []}) };
    s.pending = Some(Pending { path: path.to_string(), info, grant });
    s.open = true;
    Ok(())
}

/// Runs a command right away, or opens its dialog. `Value::Null` when the dialog opened.
fn start_command(app: &mut LccApp, s: &mut State, plugin: &str, command: &str) -> Result<Value, String> {
    let list = app.session.execute("plugin.commands", &json!({})).map_err(|e| e.to_string())?;
    let decl = list.as_array().into_iter().flatten().find(|c| c["plugin"] == plugin && c["command"] == command).cloned();
    let decl = decl.ok_or_else(|| format!("{plugin}: {command}: {}", tr("no such plug-in command (is the plug-in enabled?)")))?;
    let fields = decl["dialog"].as_array().cloned().unwrap_or_default();
    if fields.is_empty() {
        return app.session.execute("plugin.run", &json!({"plugin": plugin, "command": command})).map_err(|e| e.to_string());
    }
    let values: serde_json::Map<String, Value> = fields.iter().filter_map(|f| Some((f["key"].as_str()?.to_string(), f["default"].clone()))).collect();
    s.dialog = Some((plugin.to_string(), command.to_string(), Value::Object(values)));
    Ok(Value::Null)
}

/// File ▸ Plug-in Extras: the commands of enabled plug-ins, then the manager.
pub fn menu_items(app: &LccApp) -> Vec<(String, Value, String)> {
    let mut v = Vec::new();
    // read without the session's &mut: the commands come straight from the manager
    let g = lcc_engine::cmd::plugins::manager();
    for p in g.as_ref().into_iter().flat_map(|m| m.list()).filter(|p| p.enabled) {
        let m = p.plugin.manifest();
        for c in &m.commands {
            let path = if c.menu.is_empty() { m.name.clone() } else { format!("{} ▸ {}", m.name, c.menu.replace('>', " ▸ ")) };
            let dots = if c.dialog.is_empty() { "" } else { "…" };
            v.push(("plugins.runItem".to_string(), json!({"plugin": m.id, "command": c.id}), format!("{path} ▸ {}{dots}", c.label)));
        }
    }
    let _ = app;
    v
}

/// After an export: runs the plug-ins' export hooks on the written files, then the plug-in
/// export destination the export named (`pluginDestination`), on a worker thread with a row in
/// the activity stack, so a long upload doesn't freeze the window. [`poll`] answers the plug-ins'
/// catalog requests on this thread and reports how they went (toast, `ui.inspect` → `export.last.plugins`).
pub fn after_export(app: &mut LccApp, ctx: &egui::Context, files: &[Value], destination: Option<&Value>) {
    let Some(hooks) = lcc_engine::cmd::plugins::export_hooks(app.session.plugin_library(), files, destination) else { return };
    let mut task = task_lock();
    if task.is_some() {
        drop(task);
        app.toast_error(ctx, tr("Plug-in export hooks are still running from the last export"));
        return;
    }
    let total = hooks.len();
    let cancel = Arc::new(AtomicBool::new(false));
    let progress = Arc::new(Mutex::new((0usize, String::new())));
    let (mut host, requests) = lcc_engine::cmd::plugins::relay_host();
    let (tx, results) = std::sync::mpsc::channel();
    let (c, p) = (cancel.clone(), progress.clone());
    let work = move || {
        let r = hooks.run(&mut host, &|| c.load(Ordering::Relaxed), &mut |done, id| {
            if let Ok(mut g) = p.lock() {
                *g = (done, id.to_string());
            }
        });
        let _ = tx.send(r);
    };
    if let Err(e) = std::thread::Builder::new().name("plugin-export".into()).spawn(work) {
        drop(task);
        app.toast_error(ctx, format!("{} {e}", tr("Plug-in export hook failed:")));
        return;
    }
    // a running plug-in call can't be interrupted: ✕ stops before the next plug-in
    let guard = app.session.activity.start("plugins", "Running plug-in export hooks", Cancel::Flag(cancel));
    guard.progress(0, total as u64);
    *task = Some(ExportHooksTask { total, progress, requests, results, guard });
    ctx.request_repaint();
}

/// Export hooks running on their worker ([`after_export`]).
struct ExportHooksTask {
    total: usize,
    /// Calls started so far and the plug-in in progress.
    progress: Arc<Mutex<(usize, String)>>,
    /// The plug-ins' catalog requests, answered here (the session lives on this thread).
    requests: std::sync::mpsc::Receiver<lcc_engine::cmd::plugins::HostRequest>,
    results: std::sync::mpsc::Receiver<Vec<Value>>,
    guard: TaskGuard,
}

static TASK: Mutex<Option<ExportHooksTask>> = Mutex::new(None);

fn task_lock() -> std::sync::MutexGuard<'static, Option<ExportHooksTask>> {
    TASK.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Whether export hooks are running (`ui.inspect`, tests).
pub fn export_hooks_running() -> bool {
    task_lock().is_some()
}

/// Per frame: answer the running export hooks' catalog requests, keep their activity row up to
/// date and, when they finish, report how they went.
pub fn poll(app: &mut LccApp, ctx: &egui::Context) {
    let mut g = task_lock();
    let Some(task) = g.as_mut() else { return };
    while let Ok(req) = task.requests.try_recv() {
        req.answer(&mut app.session);
    }
    let results = match task.results.try_recv() {
        Ok(r) => r,
        Err(std::sync::mpsc::TryRecvError::Empty) => {
            let (done, current) = task.progress.lock().map(|g| g.clone()).unwrap_or_default();
            task.guard.progress(done as u64, task.total as u64);
            task.guard.detail(&current);
            // the worker may be waiting on an answer: look again soon
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
            return;
        }
        Err(std::sync::mpsc::TryRecvError::Disconnected) => vec![json!({"plugin": "", "error": "the plug-in worker stopped unexpectedly"})],
    };
    *g = None;
    drop(g);
    report(app, ctx, &results);
}

fn report(app: &mut LccApp, ctx: &egui::Context, results: &[Value]) {
    if let Some(last) = app.last_export_result.as_mut().filter(|l| l.is_object()) {
        last["plugins"] = json!(results);
    }
    let failed: Vec<String> = results
        .iter()
        .filter_map(|r| r.get("error").map(|e| format!("{}: {}", r["plugin"].as_str().unwrap_or(""), e.as_str().unwrap_or(""))))
        .collect();
    if let Some(first) = failed.first() {
        app.toast_error(ctx, format!("{} {first}", tr("Plug-in export hook failed:")));
    } else if let Some(m) = results.iter().filter(|r| r.get("destination").is_some()).find_map(|r| r["ok"]["message"].as_str()) {
        app.toast(ctx, m.to_string());
    }
}

/// The plug-in export destination chosen in the Export dialog: (plugin, destination, values).
static DESTINATION: std::sync::Mutex<Option<(String, String, Value)>> = std::sync::Mutex::new(None);

fn destination_lock() -> std::sync::MutexGuard<'static, Option<(String, String, Value)>> {
    DESTINATION.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// What to pass as `app.export {pluginDestination}`: the destination chosen in the Export
/// dialog, if any (and if its plug-in still offers it).
pub fn chosen_destination() -> Option<Value> {
    let chosen = destination_lock().clone()?;
    let offered =
        lcc_engine::cmd::plugins::manager().as_ref().is_some_and(|m| m.export_destinations().iter().any(|(p, d)| *p == chosen.0 && d.id == chosen.1));
    offered.then(|| json!({"plugin": chosen.0, "destination": chosen.1, "args": chosen.2}))
}

/// Export dialog row "Then send to": None or one of the enabled plug-ins' export destinations,
/// with the destination's settings below it. Shows nothing when no plug-in offers one.
pub fn export_destination_ui(ui: &mut egui::Ui) {
    let offered: Vec<(String, _)> = lcc_engine::cmd::plugins::manager().as_ref().map(|m| m.export_destinations()).unwrap_or_default();
    if offered.is_empty() {
        return;
    }
    let mut chosen = destination_lock().clone();
    let current = chosen.as_ref().and_then(|(p, d, _)| offered.iter().find(|(op, od)| op == p && od.id == *d));
    let text = current.map_or_else(|| tr("None").to_string(), |(_, d)| d.name.clone());
    ui.horizontal(|ui| {
        ui.label(tr("Then send to"));
        let r = egui::ComboBox::from_id_salt("export-plugin-destination").selected_text(text).show_ui(ui, |ui| {
            if ui.selectable_label(current.is_none(), tr("None")).clicked() {
                chosen = None;
            }
            for (plugin, d) in &offered {
                let on = current.is_some_and(|(p, cd)| p == plugin && cd.id == d.id);
                if ui.selectable_label(on, &d.name).clicked() && !on {
                    let values: serde_json::Map<String, Value> = d.dialog.iter().map(|f| (f.key.clone(), f.default_value())).collect();
                    chosen = Some((plugin.clone(), d.id.clone(), Value::Object(values)));
                }
            }
        });
        crate::widgets::register(ui.ctx(), "export:pluginDestination", r.response.rect);
        crate::access::label(&r.response, "Then send to");
    });
    if let Some((plugin, dest, values)) = chosen.as_mut()
        && let Some((_, d)) = offered.iter().find(|(p, d)| p == plugin && d.id == *dest)
    {
        for f in &d.dialog {
            if let Ok(f) = serde_json::to_value(f) {
                field_ui(ui, &f, values);
            }
        }
    }
    *destination_lock() = chosen;
}

/// The Metadata panel's section for plug-in fields (`plugin.metadataFields`) of the photos shown
/// (`photos`, `ids`; `photo` the active one): each enabled plug-in that declares fields, with its
/// fields edited in place on every photo shown (`plugin.setField`; text saves when the field loses
/// focus). A field whose value differs between the photos reads `< mixed >` (from the catalog's copy
/// of the fields, so a big selection costs no plug-in reads). Nothing when no plug-in has fields.
pub fn metadata_section(app: &mut LccApp, ui: &mut egui::Ui, photo: u64, photos: &[std::sync::Arc<lcc_catalog::Photo>], ids: &[u64]) {
    let Ok(v) = app.session.execute("plugin.metadataFields", &json!({"id": photo})) else { return };
    let plugins = v["plugins"].as_array().cloned().unwrap_or_default();
    if plugins.is_empty() {
        return;
    }
    ui.add_space(6.0);
    ui.strong(tr("Plug-in fields"));
    for p in &plugins {
        let plugin = p["plugin"].as_str().unwrap_or_default().to_string();
        ui.add_space(2.0);
        ui.weak(p["pluginName"].as_str().unwrap_or(&plugin));
        if let Some(e) = p["error"].as_str() {
            ui.colored_label(ui.visuals().error_fg_color, e);
            continue;
        }
        for f in p["fields"].as_array().into_iter().flatten() {
            let key = f["key"].as_str().unwrap_or_default();
            let copy = format!("{plugin}/{key}");
            let first = photos.first().map(|q| q.plugin_fields.get(&copy));
            let mixed = photos.iter().any(|q| Some(q.plugin_fields.get(&copy)) != first);
            let mut f = f.clone();
            if mixed {
                f["value"] = Value::Null;
            }
            if let Some(new) = metadata_field_ui(ui, &plugin, photo, &f, mixed) {
                let ids: Vec<u64> = if ids.is_empty() { vec![photo] } else { ids.to_vec() };
                if let Err(e) = app.session.execute("plugin.setField", &json!({"plugin": plugin, "field": key, "value": new, "ids": ids})) {
                    app.toast_error(ui.ctx(), e.to_string());
                }
            }
        }
    }
}

/// One plug-in metadata field; `Some(new value)` when the user changed it. `mixed`: the photos shown
/// differ (`< mixed >`; leaving a mixed text field empty changes nothing).
fn metadata_field_ui(ui: &mut egui::Ui, plugin: &str, photo: u64, f: &Value, mixed: bool) -> Option<Value> {
    let key = f["key"].as_str()?;
    let label = f["label"].as_str().unwrap_or(key);
    let value = &f["value"];
    let read_only = f["readOnly"].as_bool() == Some(true);
    let mut out = None;
    ui.horizontal(|ui| {
        ui.label(label);
        if read_only {
            let text = match value {
                _ if mixed => super::metadata::MIXED.to_string(),
                Value::Null => "—".to_string(),
                Value::Bool(b) => if *b { "✓" } else { "—" }.to_string(),
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            ui.label(text);
            return;
        }
        match f["type"].as_str() {
            Some("number") => {
                let (min, max) = (f["min"].as_f64().unwrap_or(0.0), f["max"].as_f64().unwrap_or(1.0));
                let mut x = value.as_f64().unwrap_or(min);
                if mixed {
                    ui.weak(super::metadata::MIXED);
                }
                let r = ui.add(egui::DragValue::new(&mut x).range(min..=max));
                crate::access::label(&r, label);
                if r.drag_stopped() || (r.changed() && !r.dragged()) {
                    out = Some(json!(x));
                }
            }
            Some("bool") => {
                let mut b = value.as_bool().unwrap_or(false);
                let r = if mixed {
                    // indeterminate: a click sets every photo shown
                    ui.add(egui::Checkbox::new(&mut b, "").indeterminate(true))
                } else {
                    ui.checkbox(&mut b, "")
                };
                crate::access::label(&r, label);
                if r.changed() {
                    out = Some(json!(b));
                }
            }
            Some("choice") => {
                let cur = value.as_str().unwrap_or("").to_string();
                let shown = if mixed {
                    super::metadata::MIXED
                } else if cur.is_empty() {
                    "—"
                } else {
                    &cur
                };
                let r = egui::ComboBox::from_id_salt(("plugin-meta", plugin, key)).selected_text(shown).show_ui(ui, |ui| {
                    if ui.selectable_label(cur.is_empty() && !mixed, "—").clicked() && (mixed || !cur.is_empty()) {
                        out = Some(Value::Null);
                    }
                    for o in f["options"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                        if ui.selectable_label(cur == o && !mixed, o).clicked() && (mixed || cur != o) {
                            out = Some(json!(o));
                        }
                    }
                });
                crate::access::label(&r.response, label);
            }
            _ => {
                let shown = value.as_str().unwrap_or("").to_string();
                let temp = egui::Id::new(("plugin-meta-text", plugin, key, photo));
                let mut text: String = ui.data(|d| d.get_temp(temp)).unwrap_or_else(|| shown.clone());
                let widget = format!("pluginField:{plugin}:{key}");
                let field = crate::text_field::TextField::singleline(&widget, &mut text);
                let field = if mixed { field.hint(super::metadata::MIXED) } else { field };
                let r = field.width(ui.available_width()).show(ui);
                crate::access::label(&r.response, label);
                if r.editing {
                    ui.data_mut(|d| d.insert_temp(temp, text.clone()));
                } else {
                    ui.data_mut(|d| d.remove::<String>(temp));
                }
                if r.committed() && text != shown && !(mixed && text.is_empty()) {
                    out = Some(if text.trim().is_empty() { Value::Null } else { json!(text) });
                }
            }
        }
    });
    out
}

fn exec(app: &mut LccApp, id: &str, p: Value, msg: &mut String) -> Option<Value> {
    match app.session.execute(id, &p) {
        Ok(v) => Some(v),
        Err(e) => {
            *msg = e.to_string();
            None
        }
    }
}

pub fn show(app: &mut LccApp, ctx: &egui::Context) {
    let mut s = state(ctx);
    if !s.open && s.dialog.is_none() {
        return;
    }
    let list = app.session.execute("plugin.list", &json!({})).unwrap_or(Value::Null);
    let mut actions: Vec<(&'static str, Value)> = Vec::new();
    let mut inspect: Option<String> = None;
    if s.open {
        let mut open = true;
        egui::Window::new(tr("Plug-in Manager")).id(key()).open(&mut open).default_width(560.0).vscroll(true).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(tr("Module (.wasm):"));
                let r = ui.add(egui::TextEdit::singleline(&mut s.path).desired_width(320.0));
                crate::access::label(&r, "Module (.wasm):");
                if ui.button(tr("Install…")).clicked() {
                    inspect = Some(s.path.trim().to_string());
                }
            });
            if let Some(dir) = list["dir"].as_str() {
                ui.weak(format!("{} {dir}", tr("Installed in")));
            }
            for f in list["failed"].as_array().into_iter().flatten() {
                ui.colored_label(ui.visuals().error_fg_color, format!("{}: {}", f["file"].as_str().unwrap_or(""), f["error"].as_str().unwrap_or("")));
            }
            if !s.message.is_empty() {
                ui.colored_label(ui.visuals().warn_fg_color, &s.message);
            }
            ui.separator();
            let plugins = list["plugins"].as_array().cloned().unwrap_or_default();
            if plugins.is_empty() {
                ui.weak(tr("No plug-ins installed."));
            }
            for p in &plugins {
                plugin_ui(ui, p, &mut actions);
            }
        });
        s.open = open;
    }
    if let Some(path) = inspect {
        s.message.clear();
        if let Err(e) = begin_install(app, &mut s, &path) {
            s.message = e;
        }
    }
    approval_ui(ctx, &mut s, &mut actions);
    dialog_ui(ctx, &mut s, &mut actions);
    for (id, p) in actions {
        s.message.clear();
        match id {
            "plugins.runItem" => {
                let (plugin, command) = (p["plugin"].as_str().unwrap_or("").to_string(), p["command"].as_str().unwrap_or("").to_string());
                match start_command(app, &mut s, &plugin, &command) {
                    Ok(v) if !v.is_null() => toast_result(app, ctx, &mut s, &v),
                    Ok(_) => {}
                    Err(e) => s.message = e,
                }
            }
            _ => {
                if let Some(v) = exec(app, id, p, &mut s.message)
                    && id == "plugin.run"
                {
                    toast_result(app, ctx, &mut s, &v);
                }
            }
        }
    }
    set_state(ctx, s);
}

fn toast_result(app: &mut LccApp, ctx: &egui::Context, s: &mut State, v: &Value) {
    s.message = v.get("message").and_then(Value::as_str).map_or_else(|| v.to_string(), str::to_string);
    app.toast(ctx, s.message.clone());
}

/// The install approval: each requested permission with a checkbox; new ones marked.
fn approval_ui(ctx: &egui::Context, s: &mut State, actions: &mut Vec<(&'static str, Value)>) {
    let Some(mut pending) = s.pending.take() else { return };
    let m = pending.info["manifest"].clone();
    let installed = pending.info["installed"].clone();
    let had = if installed.is_object() { installed["granted"].clone() } else { Value::Null };
    let mut keep = true;
    let window_id = key().with("approve");
    let shown = egui::Window::new(tr("Install Plug-in")).id(window_id).collapsible(false).resizable(false).show(ctx, |ui| {
        let mut layout = crate::dialog_frame::ModalLayout::begin(ui, window_id);
        layout.body(ui, |ui| {
            ui.strong(format!("{} {}", m["name"].as_str().unwrap_or(""), m["version"].as_str().unwrap_or("")));
            ui.weak(format!("{} · {}", m["id"].as_str().unwrap_or(""), m["author"].as_str().unwrap_or("")));
            if installed.is_object() {
                ui.label(format!("{} {}", tr("Updates the installed version"), installed["version"].as_str().unwrap_or("")));
            }
            ui.separator();
            let req = &m["permissions"];
            let rows = permission_rows(req);
            if rows.is_empty() {
                ui.label(tr("It asks for no permissions: it can only keep its own data."));
            } else {
                ui.label(tr("It asks for these permissions. Tick the ones you allow:"));
            }
            for row in rows {
                let mut on = row.granted(&pending.grant);
                let new = !row.granted(&had);
                let text = if new && installed.is_object() { format!("{}  ({})", row.label(), tr("new")) } else { row.label() };
                if ui.checkbox(&mut on, text).changed() {
                    row.set(&mut pending.grant, on);
                }
            }
        });
        layout.footer(ui, |ui| {
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button(tr("Install")).clicked() {
                    actions.push(("plugin.install", json!({"path": pending.path, "grant": pending.grant})));
                    keep = false;
                }
                if ui.button(tr("Cancel")).clicked() {
                    keep = false;
                }
            });
        });
    });
    crate::dialog_frame::ModalLayout::finish(ctx, window_id, shown.as_ref().map(|w| w.response.rect));
    if keep {
        s.pending = Some(pending);
    }
}

/// One requested permission.
enum Row {
    Flag(&'static str),
    Host(Value),
    Root(Value),
}

fn permission_rows(req: &Value) -> Vec<Row> {
    let mut v = Vec::new();
    for k in ["catalog", "metadataWrite"] {
        if req[k].as_bool() == Some(true) {
            v.push(Row::Flag(k));
        }
    }
    v.extend(req["network"].as_array().into_iter().flatten().cloned().map(Row::Host));
    v.extend(req["fs"].as_array().into_iter().flatten().cloned().map(Row::Root));
    v
}

impl Row {
    fn label(&self) -> String {
        match self {
            Row::Flag("catalog") => tr("Read the catalog").to_string(),
            Row::Flag(_) => tr("Write photo metadata").to_string(),
            Row::Host(h) => format!("{} {}", tr("Connect to"), h.as_str().unwrap_or("")),
            Row::Root(r) => {
                let what = if r["write"].as_bool() == Some(true) { tr("Read and write files in") } else { tr("Read files in") };
                format!("{what} {}", r["path"].as_str().unwrap_or(""))
            }
        }
    }
    fn granted(&self, g: &Value) -> bool {
        match self {
            Row::Flag(k) => g[*k].as_bool() == Some(true),
            Row::Host(h) => g["network"].as_array().is_some_and(|a| a.contains(h)),
            Row::Root(r) => g["fs"].as_array().is_some_and(|a| a.iter().any(|x| x["path"] == r["path"])),
        }
    }
    fn set(&self, g: &mut Value, on: bool) {
        let list = |g: &mut Value, k: &str, item: &Value, same: &dyn Fn(&Value) -> bool| {
            let mut a: Vec<Value> = g[k].as_array().cloned().unwrap_or_default();
            a.retain(|x| !same(x));
            if on {
                a.push(item.clone());
            }
            g[k] = Value::Array(a);
        };
        match self {
            Row::Flag(k) => g[*k] = json!(on),
            Row::Host(h) => list(g, "network", h, &|x| x == h),
            Row::Root(r) => list(g, "fs", r, &|x| x["path"] == r["path"]),
        }
    }
}

fn dialog_ui(ctx: &egui::Context, s: &mut State, actions: &mut Vec<(&'static str, Value)>) {
    let Some((plugin, command, mut values)) = s.dialog.take() else { return };
    let decl = {
        let g = lcc_engine::cmd::plugins::manager();
        g.as_ref().and_then(|m| m.get(&plugin)).and_then(|p| p.plugin.manifest().command(&command).cloned())
    };
    let Some(decl) = decl else { return };
    let mut keep = true;
    egui::Window::new(decl.label.clone()).id(key().with("dialog")).collapsible(false).show(ctx, |ui| {
        for f in &decl.dialog {
            if let Ok(f) = serde_json::to_value(f) {
                field_ui(ui, &f, &mut values);
            }
        }
        ui.horizontal(|ui| {
            if ui.button(tr("Run")).clicked() {
                actions.push(("plugin.run", json!({"plugin": plugin, "command": command, "args": values})));
                keep = false;
            }
            if ui.button(tr("Cancel")).clicked() {
                keep = false;
            }
        });
    });
    if keep {
        s.dialog = Some((plugin, command, values));
    }
}

fn plugin_ui(ui: &mut egui::Ui, p: &Value, actions: &mut Vec<(&'static str, Value)>) {
    let id = p["id"].as_str().unwrap_or_default().to_string();
    let title = format!("{} {}", p["name"].as_str().unwrap_or(&id), p["version"].as_str().unwrap_or(""));
    egui::CollapsingHeader::new(title).id_salt(("plugin", &id)).default_open(true).show(ui, |ui| {
        if let Some(d) = p["description"].as_str().filter(|d| !d.is_empty()) {
            ui.label(d);
        }
        let author = p["author"].as_str().filter(|a| !a.is_empty()).map(|a| format!(" · {a}")).unwrap_or_default();
        ui.weak(format!("{id}{author} · {} KB", p["size"].as_u64().unwrap_or(0).div_ceil(1024)));
        let adds = contributions(p);
        if !adds.is_empty() {
            ui.label(format!("{} {}", tr("Adds:"), adds.join(" · ")));
        }
        ui.horizontal(|ui| {
            let mut on = p["enabled"].as_bool().unwrap_or(false);
            if ui.checkbox(&mut on, tr("Enabled")).changed() {
                actions.push(("plugin.enable", json!({"id": id, "enabled": on})));
            }
            if ui.button(tr("Remove")).clicked() {
                actions.push(("plugin.uninstall", json!({"id": id})));
            }
        });
        let rows = permission_rows(&p["requested"]);
        if !rows.is_empty() {
            ui.label(tr("Permissions:"));
        }
        let mut grant = p["granted"].clone();
        let mut changed = false;
        for row in rows {
            let mut on = row.granted(&grant);
            if ui.checkbox(&mut on, row.label()).changed() {
                row.set(&mut grant, on);
                changed = true;
            }
        }
        if changed {
            actions.push(("plugin.grant", json!({"id": id, "grant": grant})));
        }
        if p["enabled"].as_bool() == Some(true) {
            for c in p["commands"].as_array().into_iter().flatten() {
                let cid = c["id"].as_str().unwrap_or_default();
                if ui.button(c["label"].as_str().unwrap_or(cid)).clicked() {
                    actions.push(("plugins.runItem", json!({"plugin": id, "command": cid})));
                }
            }
        }
        egui::CollapsingHeader::new(tr("Log")).id_salt(("plugin-log", &id)).show(ui, |ui| {
            let g = lcc_engine::cmd::plugins::manager();
            for l in g.as_ref().and_then(|m| m.get(&id)).into_iter().flat_map(|i| i.log.iter()) {
                ui.monospace(l);
            }
        });
    });
}

/// What a plug-in adds to the app (from `plugin.list`), for its row in the manager.
fn contributions(p: &Value) -> Vec<String> {
    let mut v = Vec::new();
    let n = |k: &str| p[k].as_array().map_or(0, Vec::len);
    if n("commands") > 0 {
        v.push(format!("{} ({})", tr("menu commands"), n("commands")));
    }
    if p["hooks"]["export"].as_bool() == Some(true) {
        v.push(tr("export hook").to_string());
    }
    if p["hooks"]["metadata"].as_bool() == Some(true) {
        v.push(tr("metadata suggestions").to_string());
    }
    if let Some(name) = p["publish"]["name"].as_str() {
        v.push(format!("{} “{name}”", tr("publish service")));
    }
    for d in p["exportDestinations"].as_array().into_iter().flatten() {
        v.push(format!("{} “{}”", tr("export destination"), d["name"].as_str().unwrap_or("")));
    }
    if n("metadataFields") > 0 {
        v.push(format!("{} ({})", tr("metadata fields"), n("metadataFields")));
    }
    v
}

fn field_ui(ui: &mut egui::Ui, f: &Value, values: &mut Value) {
    let Some(k) = f["key"].as_str() else { return };
    let label = f["label"].as_str().unwrap_or(k);
    let v = &mut values[k];
    ui.horizontal(|ui| {
        ui.label(label);
        match f["type"].as_str() {
            Some("number") => {
                let (min, max) = (f["min"].as_f64().unwrap_or(0.0), f["max"].as_f64().unwrap_or(1.0));
                let mut x = v.as_f64().unwrap_or(min);
                let r = ui.add(egui::Slider::new(&mut x, min..=max));
                crate::access::label(&r, label);
                *v = json!(x);
            }
            Some("bool") => {
                let mut b = v.as_bool().unwrap_or(false);
                let r = ui.checkbox(&mut b, "");
                crate::access::label(&r, label);
                *v = json!(b);
            }
            Some("choice") => {
                let mut cur = v.as_str().unwrap_or("").to_string();
                egui::ComboBox::from_id_salt(("plugin-field", k)).selected_text(cur.clone()).show_ui(ui, |ui| {
                    for o in f["options"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                        ui.selectable_value(&mut cur, o.to_string(), o);
                    }
                });
                *v = json!(cur);
            }
            _ => {
                let mut t = v.as_str().unwrap_or("").to_string();
                let r = ui.text_edit_singleline(&mut t);
                crate::access::label(&r, label);
                *v = json!(t);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_rows_tick_only_what_the_user_allows() {
        let req = json!({"catalog": true, "metadataWrite": false, "network": ["a.com", "b.com"], "fs": [{"path": "/x", "write": true}]});
        let rows = permission_rows(&req);
        assert_eq!(rows.len(), 4, "catalog, two hosts, one root");
        let mut g = json!({"catalog": false, "metadataWrite": false, "network": [], "fs": []});
        assert!(rows.iter().all(|r| !r.granted(&g)), "nothing is pre-approved on a first install");
        rows[2].set(&mut g, true);
        rows[3].set(&mut g, true);
        assert_eq!(g["network"], json!(["b.com"]));
        assert_eq!(g["fs"], json!([{"path": "/x", "write": true}]));
        rows[3].set(&mut g, false);
        assert_eq!(g["fs"], json!([]));
        assert!(!rows[0].granted(&g));
    }
}
