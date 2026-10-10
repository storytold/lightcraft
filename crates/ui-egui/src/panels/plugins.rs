//! The Plug-in Manager window (File ▸ Plug-in Manager…, P4.3) and File ▸ Plug-in Extras.
//!
//! - Install: the module is inspected first and every permission it asks for is listed with a
//!   checkbox, unchecked unless an installed version already has it. Only what the user ticks is
//!   granted; an update shows what it newly asks for.
//! - Per plug-in: enable/disable, toggle (revoke) each requested permission, remove, read the log,
//!   run its commands (with their declarative dialogs).
//! - After an export, enabled export hooks run on the written files ([`after_export`]).
//!
//! Every action is an engine `plugin.*` command. State lives in a static here (no field on the
//! app), so the shell only needs the calls in `run`, `show`, `menu_items` and `after_export`.

use serde_json::{Value, json};

use crate::DacApp;
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

fn app_ctx(app: &DacApp) -> egui::Context {
    app.tasks.repaint.clone().unwrap_or_default()
}

/// UI commands: `plugins.manager {open?}` opens / closes the window; `plugins.install {path}`
/// inspects a module and opens the approval dialog; `plugins.runItem {plugin, command}` runs a
/// plug-in command from the menu (asking its dialog first, if it has one).
pub fn run(app: &mut DacApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
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
fn begin_install(app: &mut DacApp, s: &mut State, path: &str) -> Result<(), String> {
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
fn start_command(app: &mut DacApp, s: &mut State, plugin: &str, command: &str) -> Result<Value, String> {
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
pub fn menu_items(app: &DacApp) -> Vec<(String, Value, String)> {
    let mut v = Vec::new();
    // read without the session's &mut: the commands come straight from the manager
    let g = dac_engine::cmd::plugins::manager();
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

/// After an export: runs the plug-ins' export hooks on the written files, and reports failures.
pub fn after_export(app: &mut DacApp, ctx: &egui::Context, files: &[Value]) {
    let Some(results) = dac_engine::cmd::plugins::after_export(&mut app.session, files) else { return };
    let failed: Vec<String> = results
        .iter()
        .filter_map(|r| r.get("error").map(|e| format!("{}: {}", r["plugin"].as_str().unwrap_or(""), e.as_str().unwrap_or(""))))
        .collect();
    if let Some(first) = failed.first() {
        app.toast_error(ctx, format!("{} {first}", tr("Plug-in export hook failed:")));
    }
}

fn exec(app: &mut DacApp, id: &str, p: Value, msg: &mut String) -> Option<Value> {
    match app.session.execute(id, &p) {
        Ok(v) => Some(v),
        Err(e) => {
            *msg = e.to_string();
            None
        }
    }
}

pub fn show(app: &mut DacApp, ctx: &egui::Context) {
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

fn toast_result(app: &mut DacApp, ctx: &egui::Context, s: &mut State, v: &Value) {
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
    egui::Window::new(tr("Install Plug-in")).id(key().with("approve")).collapsible(false).resizable(false).show(ctx, |ui| {
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
        let g = dac_engine::cmd::plugins::manager();
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
        ui.weak(format!("{id} · {} bytes", p["size"]));
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
            let g = dac_engine::cmd::plugins::manager();
            for l in g.as_ref().and_then(|m| m.get(&id)).into_iter().flat_map(|i| i.log.iter()) {
                ui.monospace(l);
            }
        });
    });
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
