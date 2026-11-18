//! Plug-ins in the app (RFC-0046): File ▸ Plug-in Extras (the installed plug-ins' commands),
//! File ▸ Plug-in Manager… (what is installed, install from a file, a GitHub repository or a
//! catalog, remove), the install confirmation and a plug-in command's parameters, generated from
//! its manifest. Everything goes through the engine's `plugin.*` commands; downloads run on a
//! worker thread with a row in the activity stack (no progress window of their own), and the
//! confirmation opens when the bundle has arrived and been checked.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use lightcraft_engine::plugin::{self, Request};
use serde_json::{Map, Value, json};

use crate::LightcraftApp;
use crate::state::Dialog;
use crate::theme::Tokens;

/// The UI commands of this module: (id, label, shortcut, menu path) as in `UI_COMMANDS`.
pub const MANAGER: &str = "dialog.plugins";
pub const INSTALL_FILE: &str = "file.installPlugin";
pub const RUN: &str = "dialog.runPlugin";
pub const REVIEW: &str = "dialog.reviewPlugin";

/// File ▸ Plug-in Extras: one item per command of each installed plug-in (their labels are the
/// plug-ins' own, never translated), and the Plug-in Manager.
pub fn extras(app: &LightcraftApp) -> Vec<(Value, String)> {
    let mut items = Vec::new();
    for p in app.session.plugins.list() {
        let m = &p.manifest;
        for c in &m.commands {
            let label = if m.commands.len() > 1 { format!("{}: {}", m.name, c.label) } else { c.label.clone() };
            items.push((json!({"id": m.id, "command": c.id}), label));
        }
    }
    items
}

/// A fresh Plug-in Manager.
pub fn manager() -> Dialog {
    Dialog::Plugins { repo: String::new(), catalog: String::new(), packages: Vec::new() }
}

/// This module's UI commands, or `None` for another id.
pub fn command(app: &mut LightcraftApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    let ctx = app.tasks.repaint.clone().unwrap_or_default();
    Some(match id {
        MANAGER => {
            app.ui.dialog = Some(manager());
            Ok(Value::Null)
        }
        INSTALL_FILE => install_file(app, p),
        REVIEW => review(app, p),
        RUN => run_from_menu(app, &ctx, p),
        _ => return None,
    })
}

/// Install Plug-in from File…: the bundle `path` names, or the one picked → the confirmation.
fn install_file(app: &mut LightcraftApp, p: &Value) -> Result<Value, String> {
    let path = match p.get("path").and_then(Value::as_str) {
        Some(path) => path.to_string(),
        None => {
            let request = crate::pick::PickRequest::file(crate::i18n::tr("Install Plug-in"), crate::i18n::tr("Plug-ins"), &["craftplugin"]);
            match crate::pick::ask(app, INSTALL_FILE, p, "path", request, |_| None) {
                crate::pick::Picked::Now(paths) => match paths.into_iter().next() {
                    Some(path) => path,
                    None => return Ok(Value::Null),
                },
                crate::pick::Picked::Later => return Ok(Value::Null),
                crate::pick::Picked::Unavailable => return Err("no file dialog on this platform".into()),
            }
        }
    };
    open_review(app, json!({"path": path}))
}

/// Check the bundle `request` names (a local file or base64 data) and show the confirmation.
fn open_review(app: &mut LightcraftApp, request: Value) -> Result<Value, String> {
    let mut asked = request.clone();
    asked["review"] = json!(true);
    let info = app.run("plugin.install", asked)?;
    app.ui.dialog = Some(Dialog::PluginInstall { request, info: info.clone() });
    Ok(info)
}

/// `dialog.reviewPlugin {repo | catalog + id | path}`: get the bundle (a download runs on a worker
/// thread, listed in the activity stack) and show the install confirmation once it is checked.
fn review(app: &mut LightcraftApp, p: &Value) -> Result<Value, String> {
    let request = lightcraft_engine::plugin::install_request(p);
    let req = Request::parse("plugin.install", &request).map_err(|e| e.to_string())?;
    if !req.needs_network() {
        return open_review(app, request);
    }
    let catalogs = app.session.plugins.prefs.catalogs.clone();
    let network = app.session.plugins.network;
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    let work = move |_task: lightcraft_engine::activity::TaskHandle| {
        let mut f = plugin::plugin_fetch(network, flag);
        lightcraft_engine::guard::catch(plugin::DOWNLOAD_LABEL, || plugin::obtain_with(&req, f.as_mut(), &catalogs)).and_then(|r| r)
    };
    let shown = request.clone();
    let done = move |app: &mut LightcraftApp, ctx: &egui::Context, got: Result<plugin::Candidate, String>| {
        let opened = got.and_then(|c| {
            let sha = plugin::hold(&mut app.session, shown.clone(), c);
            let mut asked = shown.clone();
            asked["sha256"] = json!(sha);
            open_review(app, asked)
        });
        if let Err(e) = opened {
            app.toast_error(ctx, e);
        }
    };
    crate::tasks::spawn_tracked(app, plugin::DOWNLOAD_LABEL, plugin::DOWNLOAD_KIND, cancel, work, done)?;
    Ok(json!({"background": true}))
}

/// A Plug-in Extras item: a command with parameters asks for them first; one without runs.
fn run_from_menu(app: &mut LightcraftApp, ctx: &egui::Context, p: &Value) -> Result<Value, String> {
    let id = p.get("id").and_then(Value::as_str).ok_or("`id` required")?.to_string();
    let command = p.get("command").and_then(Value::as_str).map(str::to_string);
    let installed = app.session.plugins.get(&id).ok_or_else(|| format!("no plug-in `{id}` is installed"))?;
    let c = installed.manifest.command(command.as_deref()).ok_or_else(|| format!("{} has no such command", installed.manifest.name))?;
    let fields: Vec<_> = c.params.iter().filter(|p| p.is_field()).collect();
    if !fields.is_empty() && p.get("params").is_none() {
        let values: Map<String, Value> = fields.iter().map(|f| (f.name.clone(), f.default_value())).collect();
        let command = c.id.clone();
        app.ui.dialog = Some(Dialog::PluginRun { id, command, values });
        return Ok(Value::Null);
    }
    let command = c.id.clone();
    run(app, ctx, &id, &command, p.get("params").cloned().unwrap_or(Value::Null))
}

/// Run a plug-in command and say how it went (its messages, or why it failed).
pub fn run(app: &mut LightcraftApp, ctx: &egui::Context, id: &str, command: &str, params: Value) -> Result<Value, String> {
    let r = app.run("plugin.run", json!({"id": id, "command": command, "params": params}))?;
    let notes: Vec<&str> = r["notes"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    let mut said = notes.join(" · ");
    if r["panel"].is_string() {
        if !said.is_empty() {
            said.push_str(" · ");
        }
        said.push_str(crate::i18n::tr("LightCraft doesn't show plug-in panels yet"));
    }
    if !said.is_empty() {
        app.toast(ctx, said);
    }
    Ok(r)
}

/// The confirm of the install confirmation: install the bundle that was reviewed, then back to the
/// Plug-in Manager.
pub fn confirm_install(app: &mut LightcraftApp, request: &Value, info: &Value) -> Result<Value, String> {
    let mut go = lightcraft_engine::plugin::install_request(request);
    go["sha256"] = info["sha256"].clone();
    let r = app.run("plugin.install", go)?;
    app.ui.dialog = Some(manager());
    Ok(r)
}

/// The confirm of a plug-in command's parameters: run it with them.
pub fn confirm_run(app: &mut LightcraftApp, id: &str, command: &str, values: &Map<String, Value>) -> Result<Value, String> {
    let ctx = app.tasks.repaint.clone().unwrap_or_default();
    run(app, &ctx, id, command, Value::Object(values.clone()))
}

/// The title of a plug-in command's parameters dialog: the command's own label.
pub fn run_title(app: &LightcraftApp, id: &str, command: &str) -> String {
    app.session
        .plugins
        .get(id)
        .and_then(|p| p.manifest.command(Some(command)).map(|c| c.label.trim_end_matches(['…', '.']).to_string()))
        .unwrap_or_else(|| crate::i18n::tr("Run Plug-in").to_string())
}

// ---------- drawing ----------

/// One line of dim text.
fn dim(ui: &mut egui::Ui, t: &Tokens, text: impl Into<String>) {
    ui.label(egui::RichText::new(text.into()).color(t.text_dim));
}

/// Where a plug-in came from, in a few words.
fn source_text(source: &Value) -> String {
    match source["type"].as_str() {
        Some("github") => format!("github.com/{} {}", source["repo"].as_str().unwrap_or_default(), source["tag"].as_str().unwrap_or_default()),
        Some("catalog") => format!("{} · {}", source["package"].as_str().unwrap_or_default(), source["catalog"].as_str().unwrap_or_default()),
        Some("file") => source["path"].as_str().unwrap_or_default().to_string(),
        _ => source["location"].as_str().unwrap_or_default().to_string(),
    }
}

/// What a manifest's permissions allow, in words.
fn permissions_text(permissions: &Value) -> String {
    let access = if permissions["access"] == "write" {
        crate::i18n::tr("Can change photos and the library").to_string()
    } else {
        crate::i18n::tr("Can only read the library").to_string()
    };
    match permissions["commands"].as_array().filter(|c| !c.is_empty()) {
        Some(commands) => {
            let list: Vec<&str> = commands.iter().filter_map(Value::as_str).collect();
            format!("{access} · {}", crate::i18n::tr_format!("only these commands: {list}", list = list.join(", ")))
        }
        None => access,
    }
}

/// The Plug-in Manager: installed plug-ins (and those that can't run here, and why), and the
/// three ways to install one.
pub fn manager_body(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, repo: &mut String, catalog: &mut String, packages: &mut Vec<Value>) {
    installed_list(app, ui, t);
    ui.separator();
    ui.label(egui::RichText::new(crate::i18n::tr("Install a plug-in")).strong());
    if crate::button::Button::new("Install from File…").widget("button:pluginFromFile").show(ui).clicked() {
        report(app, ui.ctx(), INSTALL_FILE, json!({}));
    }
    ui.horizontal(|ui| {
        let r = crate::text_field::TextField::singleline("field:pluginRepo", repo)
            .hint(crate::i18n::tr("owner/repository or owner/repository@tag"))
            .width(300.0)
            .show(ui);
        let go = crate::button::Button::new("Review…").widget("button:pluginFromRepo").enabled(!repo.trim().is_empty()).show(ui).clicked();
        if (go || r.ending == Some(crate::text_field::Ending::Return)) && !repo.trim().is_empty() {
            report(app, ui.ctx(), REVIEW, json!({"repo": repo.trim()}));
        }
    });
    dim(
        ui,
        t,
        crate::i18n::tr("From a GitHub repository's latest release (or the tag after @). Its bundle is checked against the release's SHA256SUMS."),
    );
    ui.separator();
    catalogs(app, ui, t, catalog, packages);
}

/// The installed plug-ins, each with Remove.
fn installed_list(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    let listed: Vec<Value> = app.session.plugins.list().iter().map(plugin::InstalledPlugin::describe).collect();
    if listed.is_empty() {
        dim(ui, t, crate::i18n::tr("No plug-ins are installed."));
    }
    let mut remove = None;
    for (i, p) in listed.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!("{} {}", p["name"].as_str().unwrap_or_default(), p["version"].as_str().unwrap_or_default())).strong(),
            );
            if crate::button::Button::new("Remove").small().widget(format!("button:pluginRemove:{i}")).show(ui).clicked() {
                remove = p["id"].as_str().map(str::to_string);
            }
        });
        dim(ui, t, permissions_text(&p["permissions"]));
        dim(ui, t, source_text(&p["source"]));
    }
    if !app.session.plugins.has_store() {
        dim(ui, t, crate::i18n::tr("Plug-ins installed now are kept for this session only."));
    }
    for problem in app.session.plugins.problems().to_vec() {
        ui.label(egui::RichText::new(problem).color(t.text_dim).italics());
    }
    if let Some(id) = remove {
        report(app, ui.ctx(), "plugin.remove", json!({"id": id}));
    }
}

/// The catalogs added, adding one, and their packages once listed.
fn catalogs(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, catalog: &mut String, packages: &mut Vec<Value>) {
    ui.label(egui::RichText::new(crate::i18n::tr("Catalogs")).strong());
    for url in app.session.plugins.prefs.catalogs.clone() {
        dim(ui, t, url);
    }
    ui.horizontal(|ui| {
        let r =
            crate::text_field::TextField::singleline("field:pluginCatalog", catalog).hint(crate::i18n::tr("index.json URL")).width(300.0).show(ui);
        let add = crate::button::Button::new("Add").widget("button:pluginCatalogAdd").enabled(!catalog.trim().is_empty()).show(ui).clicked();
        if (add || r.ending == Some(crate::text_field::Ending::Return)) && !catalog.trim().is_empty() {
            report(app, ui.ctx(), "plugin.catalog.add", json!({"url": catalog.trim()}));
            catalog.clear();
        }
    });
    let has = !app.session.plugins.prefs.catalogs.is_empty();
    if crate::button::Button::new("Show Packages").widget("button:pluginCatalogList").enabled(has).show(ui).clicked() {
        list_packages(app, ui.ctx());
    }
    if let Some(found) = app.plugin_packages.take() {
        *packages = found;
    }
    for (i, pkg) in packages.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(format!("{} {}", pkg["name"].as_str().unwrap_or_default(), pkg["version"].as_str().unwrap_or_default()));
            if crate::button::Button::new("Review…").small().widget(format!("button:pluginPackage:{i}")).show(ui).clicked() {
                report(app, ui.ctx(), REVIEW, json!({"catalog": pkg["catalog"], "id": pkg["id"]}));
            }
        });
        if let Some(d) = pkg["description"].as_str().filter(|d| !d.is_empty()) {
            dim(ui, t, d);
        }
    }
}

/// Read every catalog added on a worker thread (a row in the activity stack); the manager shows
/// their packages when it is done.
fn list_packages(app: &mut LightcraftApp, ctx: &egui::Context) {
    let urls = app.session.plugins.prefs.catalogs.clone();
    let network = app.session.plugins.network;
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    let work = move |_task: lightcraft_engine::activity::TaskHandle| {
        let mut f = plugin::plugin_fetch(network, flag);
        let (mut packages, mut warnings) = (Vec::new(), Vec::new());
        for url in &urls {
            match lightcraft_engine::guard::catch(plugin::DOWNLOAD_LABEL, || plugin::catalog_packages_with(f.as_mut(), url)).and_then(|r| r) {
                Ok((found, _)) => packages.extend(found),
                Err(e) => warnings.push(format!("{url}: {e}")),
            }
        }
        (packages, warnings)
    };
    let done = |app: &mut LightcraftApp, ctx: &egui::Context, (packages, warnings): (Vec<Value>, Vec<String>)| {
        app.plugin_packages = Some(packages);
        if !warnings.is_empty() {
            app.toast_error(ctx, warnings.join(" · "));
        }
    };
    if let Err(e) = crate::tasks::spawn_tracked(app, plugin::DOWNLOAD_LABEL, plugin::DOWNLOAD_KIND, cancel, work, done) {
        app.toast_error(ctx, e);
    }
}

/// Run `id` and toast its error.
fn report(app: &mut LightcraftApp, ctx: &egui::Context, id: &str, params: Value) {
    if let Err(e) = app.run(id, params) {
        app.toast_error(ctx, e);
    }
}

/// The install confirmation: what the plug-in may do, its kind, where it comes from and its
/// SHA-256, and plainly when it is in no catalog in use.
pub fn review_body(ui: &mut egui::Ui, t: &Tokens, info: &Value) {
    let s = |k: &str| info[k].as_str().unwrap_or_default().to_string();
    ui.label(egui::RichText::new(format!("{} {}", s("name"), s("version"))).strong());
    if !s("author").is_empty() {
        dim(ui, t, crate::i18n::tr_format!("by {author}", author = s("author")));
    }
    if !s("description").is_empty() {
        ui.label(s("description"));
    }
    egui::Grid::new("plugin-review").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label(crate::i18n::tr("Permissions"));
        ui.label(permissions_text(&info["permissions"]));
        ui.end_row();
        ui.label(crate::i18n::tr("Kind"));
        ui.label(s("kind"));
        ui.end_row();
        ui.label(crate::i18n::tr("Source"));
        ui.label(source_text(&info["source"]));
        ui.end_row();
        ui.label(crate::i18n::tr("SHA-256"));
        ui.label(egui::RichText::new(s("sha256")).monospace().small());
        ui.end_row();
        if !s("license").is_empty() {
            ui.label(crate::i18n::tr("Licence"));
            ui.label(s("license"));
            ui.end_row();
        }
    });
    if info["inCatalog"] == json!(false) {
        ui.label(
            egui::RichText::new(crate::i18n::tr("This plug-in is not in any catalog you use: install it only if you trust where it comes from."))
                .strong(),
        );
    }
    if let Some(old) = info["replaces"].as_str() {
        dim(ui, t, crate::i18n::tr_format!("Replaces version {old}", old = old));
    }
    if info["permissionsChanged"] == json!(true) {
        ui.label(egui::RichText::new(crate::i18n::tr("This version asks for other permissions than the one installed.")).strong());
    }
    if info["stored"] == json!(false) {
        dim(ui, t, crate::i18n::tr("Plug-ins installed now are kept for this session only."));
    }
}

/// A plug-in command's parameters, as its manifest describes them.
pub fn params_body(app: &LightcraftApp, ui: &mut egui::Ui, id: &str, command: &str, values: &mut Map<String, Value>) {
    let Some(c) = app.session.plugins.get(id).and_then(|p| p.manifest.command(Some(command))).cloned() else {
        ui.label(crate::i18n::tr("This plug-in isn't installed any more."));
        return;
    };
    egui::Grid::new("plugin-params").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
        for (i, param) in c.params.iter().filter(|p| p.is_field()).enumerate() {
            ui.label(param.title());
            let value = values.entry(param.name.clone()).or_insert_with(|| param.default_value());
            param_field(ui, i, param, value);
            ui.end_row();
        }
    });
}

/// One parameter's field.
fn param_field(ui: &mut egui::Ui, i: usize, param: &plugin::craft_plugin::Param, value: &mut Value) {
    use plugin::craft_plugin::ParamKind;
    match param.kind {
        ParamKind::Boolean => {
            let mut on = value.as_bool().unwrap_or(false);
            if crate::widgets::named(ui.checkbox(&mut on, ""), param.title()).changed() {
                *value = json!(on);
            }
        }
        ParamKind::Number | ParamKind::Slider => {
            let mut n = value.as_f64().unwrap_or(0.0);
            let lo = param.min.unwrap_or(f64::MIN);
            let hi = param.max.unwrap_or(f64::MAX);
            let r = if param.kind == ParamKind::Slider && param.min.is_some() && param.max.is_some() && lo < hi {
                ui.add(egui::Slider::new(&mut n, lo..=hi))
            } else {
                ui.add(egui::DragValue::new(&mut n).range(lo..=hi))
            };
            if r.changed() && n.is_finite() {
                *value = json!(n);
            }
        }
        ParamKind::Select | ParamKind::List => {
            let current = value.as_str().unwrap_or_default().to_string();
            let r = crate::widgets::combo_box(("plugin-param", i)).selected_text(current.clone()).show_ui(ui, |ui| {
                for option in &param.options {
                    if ui.selectable_label(*option == current, option).clicked() {
                        *value = json!(option);
                    }
                }
            });
            crate::widgets::name_combo(&r.response, param.title());
        }
        // text, and a colour typed as #RRGGBB
        _ => {
            let mut text = value.as_str().unwrap_or_default().to_string();
            let widget = format!("field:pluginParam:{i}");
            if crate::text_field::TextField::singleline(&widget, &mut text).width(220.0).show(ui).response.changed() {
                *value = json!(text);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::LightcraftApp;
    use crate::menus::menubar::{MenuNode, menu_bar};
    use crate::state::Dialog;

    /// The smallest command plug-in module: exports `memory` and `craft_run`, which returns 0.
    const WASM: &[u8] = &[
        0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // magic, version
        0x01, 0x06, 0x01, 0x60, 0x01, 0x7f, 0x01, 0x7f, // type: (i32) -> i32
        0x03, 0x02, 0x01, 0x00, // function 0 has it
        0x05, 0x03, 0x01, 0x00, 0x01, // one page of memory
        0x07, 0x16, 0x02, 0x06, b'm', b'e', b'm', b'o', b'r', b'y', 0x02, 0x00, 0x09, b'c', b'r', b'a', b'f', b't', b'_', b'r', b'u', b'n', 0x00,
        0x00, // exports
        0x0a, 0x06, 0x01, 0x04, 0x00, 0x41, 0x00, 0x0b, // body: i32.const 0
    ];

    fn bundle_file(tag: &str, params: serde_json::Value) -> std::path::PathBuf {
        let manifest = json!({
            "id": "io.example.ui", "name": "UI Test", "version": "1.0.0", "main": "p.wasm", "author": "Example",
            "apps": {"lightcraft": {"api": 6}}, "permissions": {"access": "write"},
            "commands": [{"id": "go", "label": "Do It…", "params": params}]
        });
        let bytes =
            lightcraft_engine::plugin::craft_plugin::bundle::Bundle { manifest: manifest.to_string(), wasm: WASM.to_vec(), assets: vec![] }.encode();
        let dir = std::env::temp_dir().join(format!("lc-ui-plugins-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("ui.craftplugin");
        std::fs::write(&file, bytes).unwrap();
        file
    }

    fn app() -> LightcraftApp {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Default::default());
        app.session.plugins.network = false;
        app
    }

    fn install(app: &mut LightcraftApp, file: &std::path::Path) {
        app.run(super::INSTALL_FILE, json!({"path": file.to_string_lossy()})).unwrap();
        let Some(Dialog::PluginInstall { request, info }) = app.ui.dialog.clone() else { panic!("no confirmation: {:?}", app.ui.dialog) };
        assert_eq!(info["installed"], false, "nothing is installed before the confirmation");
        assert_eq!(info["kind"], "command");
        assert!(info["sha256"].as_str().is_some_and(|s| s.len() == 64), "{info}");
        assert!(app.session.plugins.list().is_empty());
        super::confirm_install(app, &request, &info).unwrap();
        assert!(matches!(app.ui.dialog, Some(Dialog::Plugins { .. })), "back to the manager");
    }

    fn extras_items(app: &LightcraftApp) -> Vec<MenuNode> {
        let bar = menu_bar(app);
        let file = &bar.iter().find(|(title, _)| title == "File").unwrap().1;
        file.iter()
            .find_map(|n| match n {
                MenuNode::Submenu { label, children } if label == "Plug-in Extras" => Some(children.clone()),
                _ => None,
            })
            .unwrap_or_default()
    }

    #[test]
    fn the_manager_opens_from_the_file_menu() {
        let mut app = app();
        let bar = menu_bar(&app);
        let file = &bar.iter().find(|(title, _)| title == "File").unwrap().1;
        assert!(file.iter().any(|n| matches!(n, MenuNode::Item { id, .. } if id == super::MANAGER)));
        assert!(extras_items(&app).is_empty(), "no Plug-in Extras without plug-ins");
        app.run(super::MANAGER, json!({})).unwrap();
        assert!(matches!(app.ui.dialog, Some(Dialog::Plugins { .. })));
    }

    #[test]
    fn a_file_installs_after_its_confirmation_and_its_command_runs_from_plugin_extras() {
        let mut app = app();
        let file = bundle_file("run", json!([]));
        install(&mut app, &file);
        let items = extras_items(&app);
        let [MenuNode::Item { id, params, label, .. }] = items.as_slice() else { panic!("{items:?}") };
        assert_eq!((id.as_str(), label.as_str()), (super::RUN, "Do It…"), "the plug-in's own label");
        let r = app.run(id, params.clone()).unwrap();
        assert_eq!(r["plugin"], "io.example.ui", "{r}");
        let _ = std::fs::remove_dir_all(file.parent().unwrap());
    }

    #[test]
    fn a_command_with_parameters_asks_for_them_first() {
        let mut app = app();
        let file = bundle_file("params", json!([{"name": "stars", "label": "Stars", "type": "number", "default": 3, "min": 0, "max": 5}]));
        install(&mut app, &file);
        app.run(super::RUN, json!({"id": "io.example.ui", "command": "go"})).unwrap();
        let Some(Dialog::PluginRun { id, command, values }) = app.ui.dialog.clone() else { panic!("{:?}", app.ui.dialog) };
        assert_eq!(values["stars"].as_f64(), Some(3.0), "the default");
        assert_eq!(super::run_title(&app, &id, &command), "Do It");
        let r = super::confirm_run(&mut app, &id, &command, &values).unwrap();
        assert_eq!(r["command"], "go", "{r}");
        let _ = std::fs::remove_dir_all(file.parent().unwrap());
    }

    #[test]
    fn a_review_needs_a_source_and_a_download_answers_in_the_background() {
        let mut app = app();
        assert!(app.run(super::REVIEW, json!({})).is_err());
        let r = app.run(super::REVIEW, json!({"repo": "example/tools"})).unwrap();
        assert_eq!(r["background"], true, "a download never holds up the UI");
        assert!(!matches!(app.ui.dialog, Some(Dialog::PluginInstall { .. })));
    }
}
