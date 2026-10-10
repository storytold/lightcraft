//! The Plug-in Manager window (File ▸ Plug-in Manager…, P4.3): install from a `.wasm` path,
//! enable/disable, grant or revoke each requested permission, remove, read the log, and run
//! plug-in commands with their declarative dialogs. Every action is an engine `plugin.*` command.
//!
//! State lives in egui's temp data (no field on the app), so the shell only needs the calls in
//! `run` and `show`.

use serde_json::{Value, json};

use crate::DacApp;

#[derive(Clone, Default)]
struct State {
    open: bool,
    path: String,
    /// The command whose dialog is open: (plugin, command, values).
    dialog: Option<(String, String, Value)>,
    message: String,
}

fn key() -> egui::Id {
    egui::Id::new("plugin-manager")
}

fn state(ctx: &egui::Context) -> State {
    ctx.data(|d| d.get_temp::<State>(key())).unwrap_or_default()
}

fn set_state(ctx: &egui::Context, s: State) {
    ctx.data_mut(|d| d.insert_temp(key(), s));
}

/// `plugins.manager` opens / closes the window; `{open: bool}` sets it.
pub fn run(app: &mut DacApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    if id != "plugins.manager" {
        return None;
    }
    let ctx = app.tasks.repaint.clone().unwrap_or_default();
    let mut s = state(&ctx);
    s.open = p.get("open").and_then(Value::as_bool).unwrap_or(!s.open);
    let open = s.open;
    set_state(&ctx, s);
    ctx.request_repaint();
    Some(Ok(json!({"open": open})))
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
    if !s.open {
        return;
    }
    let list = app.session.execute("plugin.list", &json!({})).unwrap_or(Value::Null);
    let mut open = true;
    let mut actions: Vec<(&'static str, Value)> = Vec::new();
    egui::Window::new("Plug-in Manager").id(key()).open(&mut open).default_width(560.0).vscroll(true).show(ctx, |ui| {
        ui.horizontal(|ui| {
            ui.label("Module (.wasm):");
            ui.add(egui::TextEdit::singleline(&mut s.path).desired_width(320.0));
            if ui.button("Install").clicked() && !s.path.trim().is_empty() {
                actions.push(("plugin.install", json!({"path": s.path.trim()})));
            }
        });
        if let Some(dir) = list["dir"].as_str() {
            ui.weak(format!("Installed in {dir}"));
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
            ui.weak("No plug-ins installed.");
        }
        for p in &plugins {
            plugin_ui(ui, p, &mut s, &mut actions);
        }
    });
    if let Some((plugin, command, mut values)) = s.dialog.take() {
        let decl = plugins_command(&list, &plugin, &command);
        let mut keep = true;
        egui::Window::new(decl["label"].as_str().unwrap_or("Run")).id(key().with("dialog")).collapsible(false).show(ctx, |ui| {
            for f in decl["dialog"].as_array().into_iter().flatten() {
                field_ui(ui, f, &mut values);
            }
            ui.horizontal(|ui| {
                if ui.button("Run").clicked() {
                    actions.push(("plugin.run", json!({"plugin": plugin, "command": command, "args": values})));
                    keep = false;
                }
                if ui.button("Cancel").clicked() {
                    keep = false;
                }
            });
        });
        if keep {
            s.dialog = Some((plugin, command, values));
        }
    }
    for (id, p) in actions {
        s.message.clear();
        if let Some(v) = exec(app, id, p, &mut s.message)
            && id == "plugin.run"
        {
            s.message = v.get("message").and_then(Value::as_str).map_or_else(|| v.to_string(), str::to_string);
            app.toast(ctx, s.message.clone());
        }
    }
    s.open = open;
    set_state(ctx, s);
}

fn plugins_command(list: &Value, plugin: &str, command: &str) -> Value {
    list["plugins"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| p["id"] == plugin)
        .flat_map(|p| p["commands"].as_array().cloned().unwrap_or_default())
        .find(|c| c["id"] == command)
        .unwrap_or(Value::Null)
}

fn plugin_ui(ui: &mut egui::Ui, p: &Value, s: &mut State, actions: &mut Vec<(&'static str, Value)>) {
    let id = p["id"].as_str().unwrap_or_default().to_string();
    let title = format!("{} {}", p["name"].as_str().unwrap_or(&id), p["version"].as_str().unwrap_or(""));
    egui::CollapsingHeader::new(title).id_salt(("plugin", &id)).default_open(true).show(ui, |ui| {
        if let Some(d) = p["description"].as_str().filter(|d| !d.is_empty()) {
            ui.label(d);
        }
        ui.weak(format!("{id} · {} bytes", p["size"]));
        ui.horizontal(|ui| {
            let mut on = p["enabled"].as_bool().unwrap_or(false);
            if ui.checkbox(&mut on, "Enabled").changed() {
                actions.push(("plugin.enable", json!({"id": id, "enabled": on})));
            }
            if ui.button("Remove").clicked() {
                actions.push(("plugin.uninstall", json!({"id": id})));
            }
        });
        // permissions: one checkbox per requested capability
        let req = &p["requested"];
        let mut grant = p["granted"].clone();
        let mut changed = false;
        ui.label("Permissions:");
        for (k, label) in [("catalog", "Read the catalog"), ("metadataWrite", "Write photo metadata")] {
            if req[k].as_bool() == Some(true) {
                let mut on = grant[k].as_bool().unwrap_or(false);
                changed |= ui.checkbox(&mut on, label).changed();
                grant[k] = json!(on);
            }
        }
        let granted_hosts: Vec<Value> = grant["network"].as_array().cloned().unwrap_or_default();
        let mut hosts = Vec::new();
        for h in req["network"].as_array().into_iter().flatten() {
            let mut on = granted_hosts.contains(h);
            changed |= ui.checkbox(&mut on, format!("Network: {}", h.as_str().unwrap_or(""))).changed();
            if on {
                hosts.push(h.clone());
            }
        }
        grant["network"] = Value::Array(hosts);
        let granted_fs: Vec<Value> = grant["fs"].as_array().cloned().unwrap_or_default();
        let mut roots = Vec::new();
        for r in req["fs"].as_array().into_iter().flatten() {
            let mut on = granted_fs.iter().any(|g| g["path"] == r["path"]);
            let mode = if r["write"].as_bool() == Some(true) { "read/write" } else { "read" };
            changed |= ui.checkbox(&mut on, format!("Files ({mode}): {}", r["path"].as_str().unwrap_or(""))).changed();
            if on {
                roots.push(r.clone());
            }
        }
        grant["fs"] = Value::Array(roots);
        if changed {
            actions.push(("plugin.grant", json!({"id": id, "grant": grant})));
        }
        if p["enabled"].as_bool() == Some(true) {
            for c in p["commands"].as_array().into_iter().flatten() {
                let cid = c["id"].as_str().unwrap_or_default();
                if ui.button(c["label"].as_str().unwrap_or(cid)).clicked() {
                    let fields = c["dialog"].as_array().cloned().unwrap_or_default();
                    if fields.is_empty() {
                        actions.push(("plugin.run", json!({"plugin": id, "command": cid})));
                    } else {
                        let values: serde_json::Map<String, Value> =
                            fields.iter().filter_map(|f| Some((f["key"].as_str()?.to_string(), f["default"].clone()))).collect();
                        s.dialog = Some((id.clone(), cid.to_string(), Value::Object(values)));
                    }
                }
            }
        }
        egui::CollapsingHeader::new("Log").id_salt(("plugin-log", &id)).show(ui, |ui| {
            // the log is read on demand (it isn't in plugin.list)
            if let Some(lines) = ctx_log(&id) {
                for l in lines {
                    ui.monospace(l);
                }
            }
        });
    });
}

fn ctx_log(id: &str) -> Option<Vec<String>> {
    let g = dac_engine::cmd::plugins::manager();
    g.as_ref()?.get(id).map(|i| i.log.iter().cloned().collect())
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
                ui.add(egui::Slider::new(&mut x, min..=max));
                *v = json!(x);
            }
            Some("bool") => {
                let mut b = v.as_bool().unwrap_or(false);
                ui.checkbox(&mut b, "");
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
                ui.text_edit_singleline(&mut t);
                *v = json!(t);
            }
        }
    });
}
