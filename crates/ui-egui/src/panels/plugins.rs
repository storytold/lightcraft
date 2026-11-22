//! The Plug-in Manager window (File ▸ Plug-in Manager…, P4.3) and File ▸ Plug-in Extras.
//!
//! - The window lists what is installed on the left, plug-ins and pixel filters in sections of
//!   their own, each with its icon ([`super::plugin_icon`]) and a mark: enabled, disabled or
//!   didn't load. The selected one's page is on the right: a plug-in's has tabs (Settings, when it
//!   declares any; Permissions; About; Log), a filter's shows what it is (it has no settings,
//!   permissions or log), and a module that didn't load says why. Selecting one changes nothing,
//!   so it is no command of its own: `plugins.page {id, tab?}` does it for agents.
//! - Install: *Install from File…* opens the system file picker; a `.wasm` dropped on the app while
//!   the window is open installs too ([`take_drop`]). The module is inspected first and every
//!   permission it may ask for is listed, for information: nothing is granted at install (an
//!   update keeps what the user allowed). One install waits at a time, and a module that is exactly
//!   what is installed selects its entry instead of asking again.
//! - Per plug-in: enable/disable, each requested permission with its state (allowed, denied on a
//!   date, not asked yet, or a folder that can't be granted) and **Allow** / **Revoke**, remove,
//!   read the log, edit its settings, run its commands (with their declarative dialogs, opening
//!   with the values they last ran with, or as the plug-in's `prepare` step says).
//! - A command the user starts that needs a permission they were never asked about ends with the
//!   usual error, then a prompt names the plug-in and the permission: **Allow** grants it and runs
//!   the command again, **Deny** remembers the answer ([`ask_ui`]).
//! - After an export, enabled export hooks and the chosen export destination run on the written
//!   files, on a worker with an activity row ([`after_export`], [`poll`]).
//! - Updates: a plug-in that can have update checks (its manifest names a feed, and its file is
//!   stamped and signed by its pinned key) has a *Check for updates* box on its page, off by
//!   default. Opening the window checks the feeds of those turned on whose last check is a day old
//!   (`plugin.checkUpdates {due: true}`), on a worker with an activity row; nothing checks while
//!   it's closed. An update found shows on the plug-in's page with its versions, notes, changelog
//!   and the permission groups its feed says it adds; **Update** downloads and installs it on a
//!   worker, and asks first when the file wants permission groups the feed didn't say
//!   ([`update_ask_ui`]).
//!
//! - Permissions are granted only here, by the user: the Allow buttons go through
//!   [`lcc_engine::cmd::plugins::allow_by_user`], which no command reaches, and ignore a click in a
//!   frame carrying the control channel's synthetic input (an agent's or a script's; it says so
//!   instead). The install button and going back to the previous version
//!   ([`lcc_engine::cmd::plugins::install_by_user`], [`lcc_engine::cmd::plugins::rollback_by_user`])
//!   keep the programs granted when the code changes; clicked by an agent they act as the
//!   `plugin.install` / `plugin.rollback` commands, which don't. A program's row shows where it is
//!   installed.
//!
//! Every other action is an engine `plugin.*` command. State lives in a static here (no field on the
//! app), so the shell only needs the calls in `run`, `show`, `menu_items`, `after_export` and
//! [`take_drop`].
#![allow(clippy::indexing_slicing, reason = "not yet converted to checked indexing: tracked in docs/gaps.md (indexing lint)")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use lcc_engine::activity::{Cancel, TaskGuard};
use serde_json::{Value, json};

use crate::LccApp;
use crate::i18n::{tr, tr_format};

/// An install waiting for the user's approval: the file and its inspection.
#[derive(Clone)]
struct Pending {
    path: String,
    info: Value,
}

/// A permission a command the user started needs, on screen as a prompt ([`ask_ui`]).
#[derive(Clone)]
struct Asking {
    ask: lcc_engine::cmd::plugins::PluginAsk,
    /// What it allows, as the Permissions tab words it.
    label: String,
}

/// A command's dialog, as `plugin.prepare` gave it.
#[derive(Clone)]
struct Dialog {
    plugin: String,
    command: String,
    label: String,
    /// The fields (with the options the plug-in gave) and their values.
    fields: Vec<Value>,
    values: Value,
    /// The plug-in's prepare step failed: why (the fields are the declared ones).
    error: Option<String>,
}

/// The tabs of a plug-in's page. A pixel filter's page and a module that didn't load have none.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Tab {
    Settings,
    Permissions,
    #[default]
    About,
    Log,
}

impl Tab {
    const ALL: [Tab; 4] = [Tab::Settings, Tab::Permissions, Tab::About, Tab::Log];

    fn key(self) -> &'static str {
        match self {
            Tab::Settings => "settings",
            Tab::Permissions => "permissions",
            Tab::About => "about",
            Tab::Log => "log",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Tab::Settings => "Settings",
            Tab::Permissions => "Permissions",
            Tab::About => "About",
            Tab::Log => "plug-in tab|Log",
        }
    }

    fn parse(s: &str) -> Option<Tab> {
        Tab::ALL.into_iter().find(|t| t.key() == s)
    }
}

/// The selected plug-in's Settings tab, as `plugin.settings` gave it.
#[derive(Clone)]
struct Page {
    id: String,
    fields: Vec<Value>,
    values: Value,
    /// The plug-in couldn't fill its own options, or saving failed.
    error: Option<String>,
    /// The module and settings it was read from ([`revision`]): when either changes (an update, a
    /// value set by the plug-in), the page is read again.
    revision: Value,
    /// The fields the user changed and hasn't saved: kept when the page is read again.
    edited: std::collections::BTreeSet<String>,
}

#[derive(Clone, Default)]
struct State {
    open: bool,
    pending: Option<Pending>,
    /// The command whose dialog is open.
    dialog: Option<Dialog>,
    /// The entry whose page shows (a plug-in's, a filter's, or a module's that didn't load).
    selected: Option<String>,
    tab: Tab,
    /// The selected plug-in's settings, when it declares any.
    page: Option<Page>,
    /// Remove was clicked on this entry's page: it asks first.
    removing: Option<Removing>,
    /// The permission prompt on screen.
    asking: Option<Asking>,
    message: String,
    /// The update checks due when the window opened have been started (cleared when it closes).
    checked_open: bool,
    /// An update that asks for permission groups its feed didn't declare: the question on screen.
    update_ask: Option<UpdateAsk>,
}

/// An update `plugin.update` didn't install because its file asks for permission groups its feed
/// didn't declare ([`update_ask_ui`]).
#[derive(Clone)]
struct UpdateAsk {
    id: String,
    name: String,
    /// What it asks for that the installed version doesn't, as the Permissions tab words it.
    requests: Vec<String>,
}

/// The question Remove asks before anything is deleted.
#[derive(Clone)]
struct Removing {
    /// The entry being removed ([`Entry::id`]).
    entry: String,
    /// "Keep its data and settings" is ticked (plug-ins only: a filter keeps none).
    keep_data: bool,
}

fn key() -> egui::Id {
    egui::Id::new("plugin-manager")
}

/// Window state: one Plug-in Manager per process (the manager itself is process-wide).
#[cfg(not(test))]
static STATE: std::sync::Mutex<Option<State>> = std::sync::Mutex::new(None);

// Tests run many apps in one process, each test on its own thread: there every test has a
// Plug-in Manager of its own. Shared, one test's open manager covered the other tests' windows
// (tests_i18n_sweep leaves it open; tests_import_tab's clicks in a 900 x 700 window landed on it).
#[cfg(test)]
thread_local! {
    static STATE: std::cell::RefCell<Option<State>> = const { std::cell::RefCell::new(None) };
}

fn state(ctx: &egui::Context) -> State {
    let _ = ctx;
    #[cfg(not(test))]
    let s = STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    #[cfg(test)]
    let s = STATE.with_borrow(Clone::clone);
    s.unwrap_or_default()
}

fn set_state(ctx: &egui::Context, s: State) {
    let _ = ctx;
    #[cfg(not(test))]
    {
        *STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(s);
    }
    #[cfg(test)]
    STATE.set(Some(s));
}

fn app_ctx(app: &LccApp) -> egui::Context {
    app.tasks.repaint.clone().unwrap_or_default()
}

/// UI commands: `plugins.manager {open?}` opens / closes the window; `plugins.install {path?}`
/// inspects a module and opens the approval dialog (without `path`, the file picker asks for
/// one; refused while another waits, and a module that is already installed selects its entry); `plugins.runItem {plugin, command}` runs a plug-in command from the menu (asking its
/// dialog first, if it has one); `plugins.page {id, tab?, open?}` opens the window on a plug-in's
/// page (`tab`: settings, permissions, about or log; `open: false` closes the window if it shows
/// that page); `plugins.showFolder` opens the plug-ins folder in the system file manager;
/// `plugins.openLink {id, link}` opens a link of a plug-in's About, or its update's changelog ([`open_link`]).
pub fn run(app: &mut LccApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    let ctx = app_ctx(app);
    let mut s = state(&ctx);
    let r = match id {
        "plugins.manager" => {
            s.open = p.get("open").and_then(Value::as_bool).unwrap_or(!s.open);
            Ok(json!({"open": s.open}))
        }
        "plugins.install" => match p.get("path") {
            Some(path) => {
                let path = path.as_str().unwrap_or("").trim().to_string();
                begin_install(app, &mut s, &path)
            }
            None => match one_install_at_a_time(&s).and_then(|()| pick_module(app, p)) {
                Ok(Some(path)) => begin_install(app, &mut s, &path),
                // cancelled, or the picker answers later (the command comes back with `path`)
                Ok(None) => Ok(Value::Null),
                Err(e) => Err(e),
            },
        },
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
        "plugins.page" => page_command(app, &mut s, p),
        "plugins.showFolder" => show_folder(app),
        "plugins.openLink" => open_link(app, p),
        _ => return None,
    };
    set_state(&ctx, s);
    ctx.request_repaint();
    Some(r)
}

/// `plugins.page {id, tab?, open?}`.
fn page_command(app: &mut LccApp, s: &mut State, p: &Value) -> Result<Value, String> {
    let id = p.get("id").and_then(Value::as_str).ok_or("plugins.page needs `id`")?;
    if p.get("open").and_then(Value::as_bool) == Some(false) {
        if s.selected.as_deref() == Some(id) {
            s.open = false;
        }
        return Ok(json!({"open": s.open}));
    }
    let tab = match p.get("tab").and_then(Value::as_str) {
        Some(t) => Some(Tab::parse(t).ok_or_else(|| format!("plugins.page: no tab {t:?} (settings, permissions, about or log)"))?),
        None => None,
    };
    select(app, s, id, tab)?;
    Ok(json!({"open": true, "id": id, "tab": s.tab.key()}))
}

/// Asks the file picker for a module for `plugins.install` (run with `params`, without `path`):
/// the path when a synchronous dialog answered, `None` when cancelled or answered later.
fn pick_module(app: &mut LccApp, params: &Value) -> Result<Option<String>, String> {
    use crate::pick::{PickRequest, Picked};
    let req = PickRequest::file(tr("Install Plug-in"), tr("Plug-in modules"), &["wasm"]);
    match crate::pick::ask(app, "plugins.install", params, "path", req, |s| s.pick_files.as_mut().map(|f| f())) {
        Picked::Now(paths) => Ok(paths.into_iter().next()),
        Picked::Later => Ok(None),
        Picked::Unavailable => Err("no file dialog on this platform: pass {path}".into()),
    }
}

/// `plugins.showFolder`: the plug-ins folder in the system file manager.
fn show_folder(app: &mut LccApp) -> Result<Value, String> {
    let list = app.session.execute("plugin.list", &json!({})).map_err(|e| e.to_string())?;
    let dir = list["dir"].as_str().ok_or_else(|| tr("Plug-ins are kept in memory in this run: there is no folder to show").to_string())?;
    let opener = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "/usr/bin/open"
    } else {
        "xdg-open"
    };
    app.session.desktop.launch(&[opener.to_string(), dir.to_string()])?;
    Ok(json!({"dir": dir}))
}

/// While the Plug-in Manager is open, a drop of exactly one `.wasm` file installs it (the approval
/// dialog opens) instead of importing it: true when the drop was taken.
pub fn take_drop(app: &mut LccApp, ctx: &egui::Context, paths: &[String]) -> bool {
    let mut s = state(ctx);
    if !takes_drop(&s, paths) {
        return false;
    }
    let path = paths.first().cloned().unwrap_or_default();
    s.message.clear();
    match begin_install(app, &mut s, &path) {
        Ok(v) => s.message = v["message"].as_str().unwrap_or_default().to_string(),
        Err(e) => s.message = e,
    }
    set_state(ctx, s);
    ctx.request_repaint();
    true
}

/// Whether files dragged over the app would install as a plug-in when dropped (the drop
/// overlay says so).
pub fn would_take_drop(ctx: &egui::Context, paths: &[String]) -> bool {
    takes_drop(&state(ctx), paths)
}

fn takes_drop(s: &State, paths: &[String]) -> bool {
    let [path] = paths else { return false };
    s.open && std::path::Path::new(path).extension().is_some_and(|x| x.eq_ignore_ascii_case("wasm"))
}

/// An install waits for the user's approval: another one would replace it, so it's refused.
fn one_install_at_a_time(s: &State) -> Result<(), String> {
    let Some(p) = &s.pending else { return Ok(()) };
    let m = &p.info["manifest"];
    let name = m["name"].as_str().filter(|n| !n.is_empty()).or(m["id"].as_str()).unwrap_or_default();
    Err(tr_format!("Finish or cancel installing {name} first", name = name))
}

/// `plugin.run` for a command the user started here: a permission it needs that the user was
/// never asked about leaves a prompt ([`lcc_engine::Session::plugin_asks`], [`ask_ui`]).
fn run_as_user(app: &mut LccApp, params: &Value) -> Result<Value, String> {
    app.session.plugin_ask_user = true;
    let r = app.session.execute("plugin.run", params).map_err(|e| e.to_string());
    app.session.plugin_ask_user = false;
    r
}

/// The note for a click on something only the user may do, made by the control channel.
fn agent_click() -> String {
    tr("Only you can allow a permission: a click from an agent or a script changes nothing.").to_string()
}

/// Inspects `path` and opens the install dialog, which lists what the plug-in may ask for:
/// `{pending: path}`. A module that is exactly what is installed (the same version and code) opens
/// no dialog: its page shows instead, `{installed: id, message}`.
fn begin_install(app: &mut LccApp, s: &mut State, path: &str) -> Result<Value, String> {
    one_install_at_a_time(s)?;
    if path.is_empty() {
        return Err(tr("Choose a plug-in module (.wasm) first").to_string());
    }
    // the permissions it lists are lcc_manifest's, never only a stamped section's claim
    let info = app.session.execute("plugin.inspectFile", &json!({"path": path, "verify": true})).map_err(|e| e.to_string())?;
    if info["installed"]["same"] == true {
        let m = &info["manifest"];
        let id = m["id"].as_str().unwrap_or_default();
        let name = m["name"].as_str().filter(|n| !n.is_empty()).unwrap_or(id);
        let message = tr_format!("{name} {version} is already installed", name = name, version = m["version"].as_str().unwrap_or_default());
        // the entry on screen keeps its page, and the values typed in it
        if s.selected.as_deref() == Some(id) {
            s.open = true;
        } else {
            select(app, s, id, None)?;
        }
        s.message.clone_from(&message);
        return Ok(json!({"installed": id, "message": message}));
    }
    s.pending = Some(Pending { path: path.to_string(), info });
    s.open = true;
    Ok(json!({"pending": path}))
}

/// Runs a command right away, or opens its dialog. `Value::Null` when the dialog opened. A command
/// with a `prepare` step is asked first (`plugin.prepare`): it may run at once, explain why it
/// can't (`{message}`, shown, nothing runs) or give the dialog its own options; the dialog opens
/// with the values it last ran with.
fn start_command(app: &mut LccApp, s: &mut State, plugin: &str, command: &str) -> Result<Value, String> {
    let list = app.session.execute("plugin.commands", &json!({})).map_err(|e| e.to_string())?;
    let decl = list.as_array().into_iter().flatten().find(|c| c["plugin"] == plugin && c["command"] == command).cloned();
    let decl = decl.ok_or_else(|| format!("{plugin}: {command}: {}", tr("no such plug-in command (is the plug-in enabled?)")))?;
    // the prepare step runs for the user too: what it needs that they were never asked about
    // leaves a prompt
    app.session.plugin_ask_user = true;
    let prepared = app.session.execute("plugin.prepare", &json!({"plugin": plugin, "command": command}));
    app.session.plugin_ask_user = false;
    let prepared = prepared.map_err(|e| e.to_string())?;
    if let Some(m) = prepared.get("message").and_then(Value::as_str) {
        return Ok(json!({"message": m}));
    }
    let fields = prepared["dialog"].as_array().cloned().unwrap_or_default();
    if let (true, Some(e)) = (fields.is_empty(), prepared["error"].as_str()) {
        // the prepare step failed and there is no dialog to show it in: nothing runs
        return Err(e.to_string());
    }
    if prepared.get("run").is_some() || fields.is_empty() {
        // `plugin.run` asks the prepare step again and runs with what it gives
        return run_as_user(app, &json!({"plugin": plugin, "command": command}));
    }
    let values: serde_json::Map<String, Value> = fields.iter().filter_map(|f| Some((f["key"].as_str()?.to_string(), f["value"].clone()))).collect();
    s.dialog = Some(Dialog {
        plugin: plugin.to_string(),
        command: command.to_string(),
        label: decl["label"].as_str().unwrap_or(command).to_string(),
        fields,
        values: Value::Object(values),
        error: prepared["error"].as_str().map(str::to_string),
    });
    Ok(Value::Null)
}

/// The selected plug-in's settings as they are (`plugin.settings`), `None` when it declares none.
fn settings_page(app: &mut LccApp, id: &str) -> Result<Option<Page>, String> {
    let v = app.session.execute("plugin.settings", &json!({"id": id})).map_err(|e| e.to_string())?;
    let fields = v["fields"].as_array().cloned().unwrap_or_default();
    if fields.is_empty() {
        return Ok(None);
    }
    // read after the settings: a value the plug-in sets while preparing them is on the page already
    let list = app.session.execute("plugin.list", &json!({})).map_err(|e| e.to_string())?;
    let revision = entries(&list).iter().find(|e| e.id == id).map_or(Value::Null, revision);
    Ok(Some(Page {
        id: id.to_string(),
        fields,
        values: v["values"].clone(),
        error: v["error"].as_str().map(str::to_string),
        revision,
        edited: Default::default(),
    }))
}

/// Whether a value typed into field `was` still fits the field as it is `now`: the same type and,
/// for a choice, still one of its options (an update may have changed either).
fn still_fits(was: &Value, now: &Value, value: &Value) -> bool {
    let options = now["options"].as_array();
    was["type"] == now["type"] && (now["type"] != "choice" || options.is_none_or(|o| o.is_empty() || o.contains(value)))
}

/// What a plug-in's settings page was read from: its module and its settings' revision.
fn revision(e: &Entry) -> Value {
    json!([e.info["moduleHash"], e.info["settingsRevision"]])
}

/// Entry `e`'s Settings page: `None` for a filter, a module that didn't load and a plug-in that
/// declares no settings. One whose settings can't be read shows them (their defaults) and why.
fn build_page(app: &mut LccApp, e: &Entry) -> Option<Page> {
    let declared = e.info["settings"].as_array().cloned().unwrap_or_default();
    if e.filter || e.mark == Mark::Failed || declared.is_empty() {
        return None;
    }
    settings_page(app, &e.id).unwrap_or_else(|err| {
        let values: serde_json::Map<String, Value> =
            declared.iter().filter_map(|f| Some((f["key"].as_str()?.to_string(), f["default"].clone()))).collect();
        Some(Page {
            id: e.id.clone(),
            fields: declared.clone(),
            values: Value::Object(values),
            error: Some(err),
            revision: revision(e),
            edited: Default::default(),
        })
    })
}

/// Reads the selected plug-in's page again when its module or settings changed since it was read
/// (an update or reinstall, a value the plug-in set): the current fields and values, with what the
/// user typed and hasn't saved kept in the fields that still exist.
fn refresh_page(app: &mut LccApp, s: &mut State, e: &Entry) {
    let declared = e.info["settings"].as_array().is_some_and(|a| !a.is_empty());
    let stale = match &s.page {
        Some(pg) => pg.id != e.id || pg.revision != revision(e),
        None => declared && !e.filter && e.mark != Mark::Failed,
    };
    if !stale {
        return;
    }
    let old = s.page.take();
    let mut page = build_page(app, e);
    if let (Some(new), Some(old)) = (page.as_mut(), old.filter(|o| o.id == e.id)) {
        for k in old.edited {
            let field = |fields: &[Value]| fields.iter().find(|f| f["key"] == k.as_str()).cloned();
            let (Some(was), Some(now)) = (field(&old.fields), field(&new.fields)) else { continue };
            let value = old.values.get(&k).cloned().unwrap_or(Value::Null);
            if still_fits(&was, &now, &value) {
                new.values[&k] = value;
                new.edited.insert(k);
            }
        }
    }
    s.page = page;
}

/// One row of the window's list.
struct Entry {
    id: String,
    name: String,
    version: String,
    filter: bool,
    mark: Mark,
    /// Its `plugin.list` row (`plugins` or `failed`).
    info: Value,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mark {
    Enabled,
    Disabled,
    Failed,
}

/// The list's rows from `plugin.list`: plug-ins, then pixel filters, each with the modules of its
/// kind that didn't load (a failure that isn't a module, such as an unreadable folder, has no id
/// and no row).
fn entries(list: &Value) -> Vec<Entry> {
    let mut out: Vec<Entry> = list["plugins"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| {
            let id = p["id"].as_str()?.to_string();
            let filter = p["kind"] == "filter";
            Some(Entry {
                name: p["name"].as_str().filter(|n| !n.is_empty()).unwrap_or(&id).to_string(),
                version: p["version"].as_str().unwrap_or("").to_string(),
                filter,
                mark: if filter || p["enabled"].as_bool() == Some(true) { Mark::Enabled } else { Mark::Disabled },
                info: p.clone(),
                id,
            })
        })
        .collect();
    let mut seen: std::collections::HashSet<String> = out.iter().map(|e| e.id.clone()).collect();
    out.extend(list["failed"].as_array().into_iter().flatten().filter_map(|f| {
        let name = f["id"].as_str().filter(|i| !i.is_empty())?.to_string();
        // another module has its id (the first by file name loaded): its row goes by its file
        let id = if seen.insert(name.clone()) { name.clone() } else { f["file"].as_str().unwrap_or(&name).to_string() };
        Some(Entry { name, version: String::new(), filter: f["kind"] == "filter", mark: Mark::Failed, info: f.clone(), id })
    }));
    // plug-ins first; failures after the ones that loaded, each section by name
    out.sort_by_key(|e| (e.filter, e.mark == Mark::Failed, e.name.to_lowercase()));
    out
}

/// Shows entry `id`'s page, on `tab` (or the plug-in's settings when it has any, else About) and
/// opens the window.
fn select(app: &mut LccApp, s: &mut State, id: &str, tab: Option<Tab>) -> Result<(), String> {
    let list = app.session.execute("plugin.list", &json!({})).map_err(|e| e.to_string())?;
    let entry = entries(&list).into_iter().find(|e| e.id == id).ok_or_else(|| format!("{id}: {}", tr("no such plug-in is installed")))?;
    let page = build_page(app, &entry);
    let tab = match tab {
        Some(t) if entry.filter || entry.mark == Mark::Failed => {
            return Err(format!("{id}: {} ({})", tr("this page has no tabs"), t.key()));
        }
        Some(Tab::Settings) if page.is_none() => return Err(format!("{id}: {}", tr("this plug-in has no settings"))),
        Some(t) => t,
        None if page.is_some() => Tab::Settings,
        None => Tab::About,
    };
    s.selected = Some(id.to_string());
    s.tab = tab;
    s.page = page;
    s.open = true;
    Ok(())
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
    let (mut host, requests) = lcc_engine::cmd::plugins::relay_host(&app.session);
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

#[cfg(not(test))]
static TASK: Mutex<Option<ExportHooksTask>> = Mutex::new(None);

// Under test, each test's own (as the manager's [`STATE`]): a lock per test thread, leaked so its
// guard lives as long as the static's.
#[cfg(test)]
thread_local! {
    static TASK: &'static Mutex<Option<ExportHooksTask>> = Box::leak(Box::new(Mutex::new(None)));
}

fn task_lock() -> std::sync::MutexGuard<'static, Option<ExportHooksTask>> {
    #[cfg(not(test))]
    let task = &TASK;
    #[cfg(test)]
    let task = TASK.with(|t| *t);
    task.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Whether export hooks are running (`ui.inspect`, tests).
pub fn export_hooks_running() -> bool {
    task_lock().is_some()
}

/// Per frame: answer the running export hooks' catalog requests, keep their activity row up to
/// date and, when they finish, report how they went.
pub fn poll(app: &mut LccApp, ctx: &egui::Context) {
    poll_updates(app, ctx);
    // a plug-in's program.end hook, or a job, asks for the catalog from its own thread
    lcc_engine::cmd::plugin_programs::answer_requests(&mut app.session);
    if lcc_engine::cmd::plugin_programs::hooks_running() || lcc_engine::cmd::plugin_jobs::running() {
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
    }
    for ended in lcc_engine::cmd::plugin_jobs::take_finished(&mut app.session) {
        job_ended(app, ctx, &ended);
    }
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

/// A plug-in job ended (`plugin_jobs::take_finished`): its last reply's `message` as a toast, or
/// its error; a cancelled job says nothing (its row was stopped on purpose).
fn job_ended(app: &mut LccApp, ctx: &egui::Context, ended: &Value) {
    if let Some(e) = ended["error"].as_str() {
        let label = ended["label"].as_str().unwrap_or("");
        app.toast_error(ctx, format!("{} {label}: {e}", tr("Plug-in command failed:")));
    } else if let Some(m) = ended["ok"]["message"].as_str() {
        app.toast(ctx, m.to_string());
    }
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
#[cfg(not(test))]
static DESTINATION: std::sync::Mutex<Option<(String, String, Value)>> = std::sync::Mutex::new(None);

// Under test, each test's own, as [`TASK`].
#[cfg(test)]
thread_local! {
    static DESTINATION: &'static Mutex<Option<(String, String, Value)>> = Box::leak(Box::new(Mutex::new(None)));
}

fn destination_lock() -> std::sync::MutexGuard<'static, Option<(String, String, Value)>> {
    #[cfg(not(test))]
    let destination = &DESTINATION;
    #[cfg(test)]
    let destination = DESTINATION.with(|d| *d);
    destination.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
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
                    // the values it last ran with
                    let values = lcc_engine::cmd::plugins::manager().as_ref().map(|m| m.destination_values(plugin, &d.id)).unwrap_or_default();
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
        let scope = format!("{plugin}:{dest}");
        for f in &d.dialog {
            if let Ok(f) = serde_json::to_value(f) {
                field_ui(ui, &scope, &f, values);
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
                Value::Null => crate::i18n::tr("None").to_string(),
                Value::Bool(b) => crate::i18n::tr(if *b { "On" } else { "Off" }).to_string(),
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
                    crate::i18n::tr("None")
                } else {
                    &cur
                };
                let r = egui::ComboBox::from_id_salt(("plugin-meta", plugin, key)).selected_text(shown).show_ui(ui, |ui| {
                    if ui.selectable_label(cur.is_empty() && !mixed, crate::i18n::tr("None")).clicked() && (mixed || !cur.is_empty()) {
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

/// The list's width and the height of the list and the page.
const LIST_W: f32 = 250.0;
const BODY_H: f32 = 400.0;
/// The narrowest a page gets (the window's edge stops there).
const MIN_PAGE_W: f32 = 360.0;

pub fn show(app: &mut LccApp, ctx: &egui::Context) {
    let mut s = state(ctx);
    ask_ui(app, ctx, &mut s);
    if !s.open {
        s.checked_open = false;
    } else if !s.checked_open {
        // opening the window checks the feeds the user turned on, a day after the last check
        s.checked_open = true;
        if let Err(e) = start_check(app, ctx, None, true) {
            log::info!("plug-in update check not started: {e}");
        }
    }
    if !s.open && s.dialog.is_none() && s.pending.is_none() {
        set_state(ctx, s);
        return;
    }
    let list = app.session.execute("plugin.list", &json!({})).unwrap_or(Value::Null);
    let installs = program_installs(app, &list, s.pending.as_ref());
    if installs.values().any(|t| t == tr("checking…")) {
        // a lookup runs in the background: show its answer when it comes
        ctx.request_repaint_after(std::time::Duration::from_millis(200));
    }
    let entries = entries(&list);
    own_icons(app, ctx, &entries);
    own_headers(app, ctx, &entries);
    let mut actions: Vec<(&'static str, Value)> = Vec::new();
    if s.open {
        // a page on screen: the first entry's when none (or one just removed) was selected
        if !entries.iter().any(|e| s.selected.as_deref() == Some(e.id.as_str())) {
            s.selected = None;
            s.page = None;
            if let Some(first) = entries.first().map(|e| e.id.clone())
                && let Err(e) = select(app, &mut s, &first, None)
            {
                s.message = e;
            }
        }
        let selected = entries.iter().find(|e| s.selected.as_deref() == Some(e.id.as_str()));
        if let Some(e) = selected {
            refresh_page(app, &mut s, e);
        }
        let open = !crate::dialog_frame::esc_closes(app.ui.dialog.is_some(), ctx);
        let mut closed = false;
        egui::Window::new(tr("Plug-in Manager"))
            .id(key())
            .title_bar(false)
            .frame(crate::dialog_frame::card_frame(ctx))
            .collapsible(false)
            .default_width(LIST_W + 560.0)
            // nothing on a page sets the window's width: its text wraps instead
            .min_width(LIST_W + MIN_PAGE_W)
            .show(ctx, |ui| {
                use crate::button::{Button, Kind};
                if crate::dialog_frame::card_header(ui, tr("Plug-in Manager"), Some("plugin-manager:close")) {
                    closed = true;
                }
                button_row(ui, |ui| {
                    if Button::new("Install from File…").kind(Kind::Primary).widget("button:pluginInstallFile").show(ui).clicked() {
                        actions.push(("plugins.pick", Value::Null));
                    }
                });
                // modules past the most the folder loads: not looked at, so they have no rows
                if let Some(n) = list["skipped"].as_u64().filter(|n| *n > 0) {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        tr_format!("{count} modules in the plug-ins folder weren't loaded: it holds more than it can load.", count = n),
                    );
                }
                // a failure that isn't a module (the folder couldn't be opened) has no row
                for f in list["failed"].as_array().into_iter().flatten().filter(|f| f["id"].as_str().is_none_or(str::is_empty)) {
                    ui.colored_label(
                        ui.visuals().error_fg_color,
                        format!("{}: {}", f["file"].as_str().unwrap_or(""), f["error"].as_str().unwrap_or("")),
                    );
                }
                if !s.message.is_empty() {
                    ui.colored_label(ui.visuals().warn_fg_color, &s.message);
                }
                ui.separator();
                ui.horizontal_top(|ui| {
                    ui.allocate_ui_with_layout(egui::vec2(LIST_W, BODY_H), egui::Layout::top_down(egui::Align::Min), |ui| {
                        ui.set_min_size(egui::vec2(LIST_W, BODY_H));
                        egui::ScrollArea::vertical()
                            .id_salt("plugin-list")
                            .auto_shrink([false, false])
                            .max_height(BODY_H - 28.0)
                            .show(ui, |ui| list_ui(ui, &entries, s.selected.as_deref(), &mut actions));
                        ui.weak(tr("Drop a .wasm file here to install it"));
                    });
                    // a line as tall as the list (a separator here would take the screen's height)
                    let (line, _) = ui.allocate_exact_size(egui::vec2(9.0, BODY_H), egui::Sense::hover());
                    ui.painter().vline(line.center().x, line.y_range(), ui.visuals().widgets.noninteractive.bg_stroke);
                    // the page takes the width the window has and never asks for more (a minimum
                    // width here would hold the window at its widest)
                    let w = ui.available_width();
                    ui.allocate_ui_with_layout(egui::vec2(w, BODY_H), egui::Layout::top_down(egui::Align::Min), |ui| {
                        ui.set_min_height(BODY_H);
                        ui.set_max_width(w);
                        if let Some(e) = selected {
                            egui::ScrollArea::vertical().id_salt("plugin-page").auto_shrink([false, false]).max_height(BODY_H).show(ui, |ui| {
                                page_ui(ui, e, &mut s.tab, s.page.as_mut(), &mut s.removing, &installs, &mut actions);
                            });
                        }
                    });
                });
                ui.separator();
                ui.weak(tr("Supports {app} plug-ins and PhotoCraft pixel filters (.wasm)."));
                if let Some(dir) = list["dir"].as_str() {
                    // the button first, on the right: a long folder is shortened (whole on hover), never wider than the window
                    ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 24.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if Button::new("Show Folder").small().widget("button:pluginShowFolder").show(ui).clicked() {
                            actions.push(("plugins.showFolder", Value::Null));
                        }
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            let text = egui::RichText::new(format!("{} {dir}", tr("Installed in"))).weak();
                            ui.add(egui::Label::new(text).truncate()).on_hover_text(dir);
                        });
                    });
                }
            });
        s.open = open && !closed;
    }
    approval_ui(ctx, &mut s, &installs, &mut actions);
    dialog_ui(ctx, &mut s, &mut actions);
    update_ask_ui(ctx, &mut s, &mut actions);
    for (id, p) in actions {
        s.message.clear();
        match id {
            "plugins.pick" => match one_install_at_a_time(&s).and_then(|()| pick_module(app, &json!({}))) {
                Ok(Some(path)) => match begin_install(app, &mut s, &path) {
                    Ok(v) => s.message = v["message"].as_str().unwrap_or_default().to_string(),
                    Err(e) => s.message = e,
                },
                Ok(None) => {}
                Err(e) => s.message = e,
            },
            "plugins.select" => {
                // the entry on screen keeps its page, and the values typed in it
                let id = p["id"].as_str().unwrap_or("");
                if s.selected.as_deref() != Some(id)
                    && let Err(e) = select(app, &mut s, id, None)
                {
                    s.message = e;
                }
            }
            "plugins.showFolder" => {
                if let Err(e) = show_folder(app) {
                    s.message = e;
                }
            }
            "plugins.openLink" => {
                if let Err(e) = open_link(app, &p) {
                    s.message = e;
                }
            }
            "plugins.revertSettings" => match settings_page(app, p["id"].as_str().unwrap_or("")) {
                Ok(page) => s.page = page,
                Err(e) => s.message = e,
            },
            "plugin.setSettings" => match app.session.execute(id, &p) {
                // filled again as the plug-in keeps them
                Ok(_) => match settings_page(app, p["id"].as_str().unwrap_or("")) {
                    Ok(page) => s.page = page,
                    Err(e) => s.message = e,
                },
                Err(e) => {
                    // the tab keeps the user's values and says why they weren't saved
                    if let Some(pg) = s.page.as_mut() {
                        pg.error = Some(e.to_string());
                    }
                }
            },
            "plugins.runItem" => {
                let (plugin, command) = (p["plugin"].as_str().unwrap_or("").to_string(), p["command"].as_str().unwrap_or("").to_string());
                match start_command(app, &mut s, &plugin, &command) {
                    Ok(v) if !v.is_null() => toast_result(app, ctx, &mut s, &v),
                    Ok(_) => {}
                    Err(e) => s.message = e,
                }
            }
            "plugins.askRemove" => {
                s.removing = p["entry"].as_str().map(|entry| Removing { entry: entry.to_string(), keep_data: false });
            }
            // only the user's own clicks allow a permission or keep programs; one in a frame the
            // control channel's input reached (an agent, a script) does what a command does
            "plugins.userAllow" if app.synthetic_frame => s.message = agent_click(),
            "plugins.userAllow" => {
                if let Err(e) = lcc_engine::cmd::plugins::allow_by_user(&mut app.session, &p) {
                    s.message = e.to_string();
                }
            }
            "plugins.userInstall" if app.synthetic_frame => {
                exec(app, "plugin.install", p, &mut s.message);
            }
            "plugins.userInstall" => {
                if let Err(e) = lcc_engine::cmd::plugins::install_by_user(&mut app.session, &p) {
                    s.message = e.to_string();
                }
            }
            "plugins.userRollback" if app.synthetic_frame => {
                exec(app, "plugin.rollback", p, &mut s.message);
            }
            "plugins.userRollback" => {
                if let Err(e) = lcc_engine::cmd::plugins::rollback_by_user(&mut app.session, &p) {
                    s.message = e.to_string();
                }
            }
            "plugins.checkUpdates" => {
                if let Err(e) = start_check(app, ctx, p["id"].as_str().map(str::to_string), false) {
                    s.message = e;
                }
            }
            // as the user's own install, unless the control channel clicked (then as plugin.update)
            "plugins.userUpdate" => {
                let (id, accept) = (p["id"].as_str().unwrap_or("").to_string(), p["acceptUndeclared"].as_bool() == Some(true));
                if let Err(e) = start_update(app, ctx, id, accept, !app.synthetic_frame) {
                    s.message = e;
                }
            }
            "plugin.run" => match run_as_user(app, &p) {
                Ok(v) => toast_result(app, ctx, &mut s, &v),
                Err(e) => s.message = e,
            },
            _ => {
                exec(app, id, p, &mut s.message);
            }
        }
    }
    set_state(ctx, s);
}

/// The prompt for a permission a command the user started needs and they were never asked about
/// ([`lcc_engine::Session::plugin_asks`]), one at a time: **Allow** grants it and runs the command
/// again (through the same path, so it may ask for the next one), **Deny** remembers the answer.
/// Allow ignores a click the control channel made.
fn ask_ui(app: &mut LccApp, ctx: &egui::Context, s: &mut State) {
    if s.asking.is_none() && !app.session.plugin_asks.is_empty() {
        let ask = app.session.plugin_asks.remove(0);
        let list = app.session.execute("plugin.list", &json!({})).unwrap_or(Value::Null);
        let requested = list["plugins"].as_array().into_iter().flatten().find(|p| p["id"] == ask.plugin.as_str()).map(|p| p["requested"].clone());
        let want = serde_json::to_value(&ask.request).unwrap_or(Value::Null);
        let row = requested.as_ref().map(permission_rows).unwrap_or_default().into_iter().find(|r| {
            let r = r.request();
            r["group"] == want["group"] && (r["value"] == want["value"] || want.get("value").is_none() && r["value"].is_null())
        });
        let label = row.map_or_else(|| ask.request.describe(), |r| r.label(&Installs::new(), &ask.plugin));
        s.asking = Some(Asking { ask, label });
    }
    let Some(asking) = s.asking.clone() else { return };
    let window_id = key().with("ask");
    let mut answer = None;
    let shown = egui::Window::new(tr("Allow Permission"))
        .id(window_id)
        .title_bar(false)
        .frame(crate::dialog_frame::card_frame(ctx))
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            crate::dialog_frame::card_header(ui, tr("Allow Permission"), None);
            let mut layout = crate::dialog_frame::ModalLayout::begin(ui, window_id);
            layout.body(ui, |ui| {
                ui.label(tr_format!("{plugin} asks to: {permission}", plugin = asking.ask.plugin_name, permission = asking.label));
                ui.weak(tr("Allow runs the command again. You can change this later on the plug-in's Permissions tab."));
            });
            layout.footer(ui, |ui| {
                ui.separator();
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    use crate::button::{Button, Kind};
                    if Button::new("Allow").kind(Kind::Primary).widget("button:pluginAskAllow").show(ui).clicked() {
                        answer = Some(true);
                    }
                    if Button::new("Deny").widget("button:pluginAskDeny").show(ui).clicked() {
                        answer = Some(false);
                    }
                });
            });
        });
    crate::dialog_frame::ModalLayout::finish(ctx, window_id, shown.as_ref().map(|w| w.response.rect));
    let mut call = serde_json::to_value(&asking.ask.request).unwrap_or(Value::Null);
    call["id"] = json!(asking.ask.plugin);
    match answer {
        None => {}
        Some(true) if app.synthetic_frame => app.toast(ctx, agent_click()),
        Some(true) => {
            s.asking = None;
            match lcc_engine::cmd::plugins::allow_by_user(&mut app.session, &call) {
                // a prepare step's ask starts the command again as from its menu item
                Ok(_) if asking.ask.prepare => {
                    match start_command(app, s, &asking.ask.plugin, asking.ask.rerun["command"].as_str().unwrap_or_default()) {
                        Ok(v) if !v.is_null() => toast_result(app, ctx, s, &v),
                        Ok(_) => {}
                        Err(e) => app.toast(ctx, e),
                    }
                }
                Ok(_) => match run_as_user(app, &asking.ask.rerun) {
                    Ok(v) => toast_result(app, ctx, s, &v),
                    Err(e) => app.toast(ctx, e),
                },
                Err(e) => app.toast(ctx, e.to_string()),
            }
        }
        Some(false) => {
            s.asking = None;
            if let Err(e) = lcc_engine::cmd::plugins::deny_by_user(&mut app.session, &call) {
                app.toast(ctx, e.to_string());
            }
        }
    }
}

fn toast_result(app: &mut LccApp, ctx: &egui::Context, s: &mut State, v: &Value) {
    // a job started: its row shows it, and its message comes when it ends
    if v["running"] == true && v["job"].is_u64() {
        return;
    }
    s.message = v.get("message").and_then(Value::as_str).map_or_else(|| v.to_string(), str::to_string);
    app.toast(ctx, s.message.clone());
}

/// The install dialog: each permission the plug-in may ask for, for information; new ones marked.
fn approval_ui(ctx: &egui::Context, s: &mut State, installs: &Installs, actions: &mut Vec<(&'static str, Value)>) {
    let Some(pending) = s.pending.take() else { return };
    let m = pending.info["manifest"].clone();
    let installed = pending.info["installed"].clone();
    let had = if installed.is_object() { installed["granted"].clone() } else { Value::Null };
    let id = m["id"].as_str().unwrap_or("").to_string();
    // what stops the install (plugin.install refuses it too): features this version lacks, a bad version
    let missing: Vec<String> = pending.info["missing"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect();
    let version_error = pending.info["versionError"].as_str().map(str::to_string);
    // a module of the other kind (plug-in or pixel filter) has its id
    let conflict = pending.info["conflict"].as_str().map(str::to_string);
    // the author signature: who signed it, and whether that's the installed version's pinned signer
    let signature = pending.info["signature"].as_str().unwrap_or("none").to_string();
    let signer = pending.info["signer"].as_str().unwrap_or_default().to_string();
    // what the yes is bound to: exactly the key shown, not only its fingerprint
    let signer_key = pending.info["signerKey"].as_str().unwrap_or_default().to_string();
    let pinned = pending.info["pinned"].clone();
    let invalid = (signature == "invalid").then(|| pending.info["signatureError"].as_str().unwrap_or_default().to_string());
    let blocked = !missing.is_empty() || version_error.is_some() || conflict.is_some() || invalid.is_some();
    let downgrade = pending.info["downgrade"].as_bool() == Some(true);
    // another signer than the pinned one, or none: installing is the user's yes to it
    let signer_changed = pending.info["matchesPin"] == false;
    let mut keep = true;
    let window_id = key().with("approve");
    let shown = egui::Window::new(tr("Install Plug-in"))
        .id(window_id)
        .title_bar(false)
        .frame(crate::dialog_frame::card_frame(ctx))
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            crate::dialog_frame::card_header(ui, tr("Install Plug-in"), None);
            let mut layout = crate::dialog_frame::ModalLayout::begin(ui, window_id);
            layout.body(ui, |ui| {
                ui.strong(format!("{} {}", m["name"].as_str().unwrap_or(""), m["version"].as_str().unwrap_or("")));
                ui.weak(format!("{} · {}", m["id"].as_str().unwrap_or(""), m["author"].as_str().unwrap_or("")));
                let (new, old) = (m["version"].as_str().unwrap_or(""), installed["version"].as_str().unwrap_or(""));
                if downgrade {
                    ui.label(tr_format!("Version {new} is older than the installed version {installed}", new = new, installed = old));
                } else if installed.is_object() {
                    ui.label(format!("{} {old}", tr("Updates the installed version")));
                }
                match signature.as_str() {
                    "valid" if pinned["signer"].as_str() == Some(signer.as_str()) => {
                        ui.label(tr_format!("Signed by {signer}, the same author as the installed version", signer = signer));
                    }
                    "valid" => {
                        ui.label(tr_format!("Signed by {signer}", signer = signer));
                    }
                    "none" => {
                        ui.label(tr("Not signed: nothing shows who made it"));
                    }
                    // invalid: what stops the install, below
                    _ => {}
                }
                if signer_changed && !blocked {
                    let text = match (pinned["readable"] == false, pinned["signer"].as_str(), signature.as_str()) {
                        (true, ..) => tr("The installed version's signer can't be read. Install it only if you trust where it came from.").to_string(),
                        (false, Some(old), "valid") => tr_format!(
                            "The installed version was signed by {old}, but this file is signed by {new}. Install it only if you trust where it came from.",
                            old = old,
                            new = signer
                        ),
                        (false, Some(old), _) => tr_format!(
                            "The installed version was signed by {old}, but this file isn't signed. Install it only if you trust where it came from.",
                            old = old
                        ),
                        _ => String::new(),
                    };
                    ui.colored_label(ui.visuals().warn_fg_color, text);
                }
                // fields this version doesn't know: a typo, or a field from a newer app
                let unknown: Vec<&str> = pending.info["unknownFields"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
                if !unknown.is_empty() {
                    ui.label(tr_format!(
                        "Its manifest has fields this version doesn't know, which it ignores: {fields}",
                        fields = unknown.join(", ")
                    ));
                }
                // values it can't use (a link that isn't https://): left out, the install goes ahead
                let dropped: Vec<&str> = pending.info["dropped"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
                if !dropped.is_empty() {
                    ui.label(tr_format!(
                        "Its manifest has values this version can't use, which it leaves out: {values}",
                        values = dropped.join("; ")
                    ));
                }
                ui.separator();
                if blocked {
                    if !missing.is_empty() {
                        ui.label(tr("It needs features this version of the app doesn't have:"));
                        for f in &missing {
                            ui.label(format!("• {f}"));
                        }
                        ui.label(tr("Install a newer version of the app to use it."));
                    }
                    for e in version_error.iter().chain(&conflict).chain(&invalid) {
                        ui.label(format!("{} {e}", tr("It can't be installed:")));
                    }
                    return;
                }
                let rows = permission_rows(&m["permissions"]);
                if rows.is_empty() {
                    ui.label(tr("It asks for no permissions: it can only keep its own data."));
                } else {
                    ui.label(tr("It may ask for these permissions when it needs them; nothing is allowed until you say so:"));
                }
                for row in rows {
                    let label = row.label(installs, &id);
                    // an update keeps what the user allowed
                    let text = if row.granted(&had) { format!("• {label}  ({})", tr("allowed")) } else { format!("• {label}") };
                    ui.label(text);
                }
            });
            layout.footer(ui, |ui| {
                ui.separator();
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    use crate::button::{Button, Kind};
                    let label = if signer_changed {
                        "Install Anyway"
                    } else if downgrade {
                        "Install Older Version"
                    } else {
                        "Install"
                    };
                    if !blocked && Button::new(label).kind(Kind::Primary).widget("button:pluginInstall").show(ui).clicked() {
                        let mut install = json!({"path": pending.path, "allowDowngrade": downgrade});
                        // the click is the user's yes to the signer the dialog named
                        if signer_changed && signature == "valid" {
                            install["acceptSigner"] = json!(signer_key);
                        } else if signer_changed {
                            install["acceptUnsigned"] = json!(true);
                        }
                        actions.push(("plugins.userInstall", install));
                        keep = false;
                    }
                    if Button::new("Cancel").widget("button:pluginInstallCancel").show(ui).clicked() {
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

/// Where each declared program is installed, by `(plug-in id, program id)`: a line for its row.
type Installs = std::collections::BTreeMap<(String, String), String>;

/// Looks up the programs of the listed plug-ins and of the one waiting to install. Never waits:
/// the desktop looks in the background and keeps answers for a few seconds; until the first comes
/// the row says "checking…".
fn program_installs(app: &LccApp, list: &Value, pending: Option<&Pending>) -> Installs {
    let mut out = Installs::new();
    let manifests = list["plugins"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|p| (p["id"].clone(), p["requested"]["programs"].clone()))
        .chain(pending.map(|p| (p.info["manifest"]["id"].clone(), p.info["manifest"]["permissions"]["programs"].clone())));
    for (id, programs) in manifests {
        for decl in programs.as_array().into_iter().flatten() {
            let text = match lcc_engine::cmd::plugins::find_program(&app.session, decl) {
                None => tr("checking…").to_string(),
                Some(found) => installed_text(&found),
            };
            out.insert((id.as_str().unwrap_or("").to_string(), decl["id"].as_str().unwrap_or("").to_string()), text);
        }
    }
    out
}

/// "installed: Tool 2.0, Tool 3.1", or "not installed".
fn installed_text(found: &[Value]) -> String {
    if found.is_empty() {
        return tr("not installed").to_string();
    }
    let each: Vec<String> = found
        .iter()
        .map(|f| {
            let path = f["path"].as_str().unwrap_or("");
            let name = std::path::Path::new(path).file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            match f["version"].as_str() {
                Some(v) => format!("{name} {v}"),
                None => name,
            }
        })
        .collect();
    format!("{} {}", tr("installed:"), each.join(", "))
}

/// What a program entry names on this system: its bundle ids on macOS, its executable elsewhere.
fn program_identity(d: &Value) -> String {
    if cfg!(target_os = "macos") {
        d["macBundleIds"].as_array().into_iter().flatten().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")
    } else {
        let exe = if cfg!(windows) { "windowsExe" } else { "linuxExe" };
        d[exe].as_str().unwrap_or("").to_string()
    }
}

/// Does entry `d` name only what the granted entry `g` names (`lcc_plugins::ProgramDecl::within`):
/// its bundle ids among `g`'s, each executable `g`'s or none?
fn program_within(d: &Value, g: &Value) -> bool {
    let ids = |v: &Value| v["macBundleIds"].as_array().cloned().unwrap_or_default();
    let exe = |k: &str| d[k].is_null() || d[k] == g[k];
    d["id"] == g["id"] && ids(d).iter().all(|b| ids(g).contains(b)) && exe("windowsExe") && exe("linuxExe")
}

/// One requested permission.
enum Row {
    Flag(&'static str),
    Host(Value),
    Root(Value),
    /// A program it may start: `{id, name, …}`.
    Program(Value),
}

fn permission_rows(req: &Value) -> Vec<Row> {
    let mut v = Vec::new();
    for k in ["catalog", "metadataWrite", "libraryAdd", "pixels", "crop", "virtualCopy"] {
        if req[k].as_bool() == Some(true) {
            v.push(Row::Flag(k));
        }
    }
    v.extend(req["network"].as_array().into_iter().flatten().cloned().map(Row::Host));
    v.extend(req["fs"].as_array().into_iter().flatten().cloned().map(Row::Root));
    v.extend(req["programs"].as_array().into_iter().flatten().cloned().map(Row::Program));
    v
}

impl Row {
    /// The row's text; a program's says where it's installed (`installs`, for `plugin`).
    fn label(&self, installs: &Installs, plugin: &str) -> String {
        match self {
            Row::Program(d) => {
                let id = d["id"].as_str().unwrap_or("");
                let name = d["name"].as_str().unwrap_or(id);
                let at = installs.get(&(plugin.to_string(), id.to_string())).cloned().unwrap_or_default();
                match program_identity(d) {
                    ident if ident.is_empty() => format!("{} {name} ({at})", tr("Start the program")),
                    ident => format!("{} {name} [{ident}] ({at})", tr("Start the program")),
                }
            }
            Row::Flag("catalog") => tr("Read the catalog").to_string(),
            Row::Flag("libraryAdd") => tr("Add files made from photos to the catalog").to_string(),
            Row::Flag("pixels") => tr("Look at downsized copies of photos").to_string(),
            Row::Flag("crop") => tr("Change the crop of photos").to_string(),
            Row::Flag("virtualCopy") => tr("Make virtual copies of photos").to_string(),
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
            // an update that widened a granted entry is no longer allowed (and shows as new)
            Row::Program(d) => g["programs"].as_array().is_some_and(|a| a.iter().any(|x| program_within(d, x))),
        }
    }
    /// The request it is, as `plugin.list` → `requests` and the Allow / Revoke calls name it.
    fn request(&self) -> Value {
        match self {
            Row::Flag(k) => json!({"group": k}),
            Row::Host(h) => json!({"group": "network", "value": h}),
            Row::Root(r) => json!({"group": "fs", "value": r["path"]}),
            Row::Program(d) => json!({"group": "programs", "value": d["id"]}),
        }
    }
}

fn dialog_ui(ctx: &egui::Context, s: &mut State, actions: &mut Vec<(&'static str, Value)>) {
    let Some(mut d) = s.dialog.take() else { return };
    let mut keep = true;
    egui::Window::new(d.label.clone()).id(key().with("dialog")).title_bar(false).frame(crate::dialog_frame::card_frame(ctx)).collapsible(false).show(
        ctx,
        |ui| {
            // the plug-in's own header picture above the command's title
            if let Some(tex) = header_of(ctx, &d.plugin) {
                ui.add(egui::Image::new(&tex).max_height(HEADER_HEIGHT).max_width(ui.available_width()));
                ui.add_space(4.0);
            }
            crate::dialog_frame::card_header(ui, &d.label, None);
            if let Some(e) = &d.error {
                ui.colored_label(ui.visuals().warn_fg_color, e);
            }
            let scope = format!("{}:{}", d.plugin, d.command);
            for f in &d.fields {
                field_ui(ui, &scope, f, &mut d.values);
            }
            ui.add_space(6.0);
            button_row(ui, |ui| {
                use crate::button::{Button, Kind};
                if Button::new("Run").kind(Kind::Primary).widget("button:pluginRun").show(ui).clicked() {
                    actions.push(("plugin.run", json!({"plugin": d.plugin, "command": d.command, "args": d.values})));
                    keep = false;
                }
                if Button::new("Cancel").widget("button:pluginRunCancel").show(ui).clicked() {
                    keep = false;
                }
            });
        },
    );
    if keep {
        s.dialog = Some(d);
    }
}

/// A dialog's buttons, right-aligned on one row (a bare right-to-left layout takes the window's
/// whole height and leaves the dialog tall and mostly empty).
fn button_row(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 30.0), egui::Layout::right_to_left(egui::Align::Center), add);
}

/// Loads the own icon of every plug-in and filter whose manifest carries one (`plugin.picture`),
/// once per module: the rows and pages then draw it in place of the identicon. A module without
/// one (an update or a rollback to a version without a picture) clears the one kept for its id.
fn own_icons(app: &mut LccApp, ctx: &egui::Context, entries: &[Entry]) {
    for e in entries {
        let hash = e.info["moduleHash"].as_str().unwrap_or("");
        if super::plugin_icon::custom_is_current(ctx, &e.id, hash) {
            continue;
        }
        if e.info["icon"].as_bool() != Some(true) {
            super::plugin_icon::keep_custom(ctx, &e.id, hash, None);
            continue;
        }
        let tex = app
            .session
            .execute("plugin.picture", &json!({"id": e.id, "picture": "icon"}))
            .ok()
            .and_then(|r| r["svg"].as_str().map(str::to_string))
            .and_then(|svg| crate::plate::rasterise_untrusted_svg(&svg, 128, 128).ok())
            .map(|img| ctx.load_texture("plugin-own-icon", img, egui::TextureOptions::LINEAR));
        super::plugin_icon::keep_custom(ctx, &e.id, hash, tex);
    }
}

/// The box a header picture is rasterised into (pixels: about twice what it's shown at, for sharp
/// HiDPI) and its height on screen (points).
const HEADER_PIXELS: (u32, u32) = (640, 96);
const HEADER_HEIGHT: f32 = 40.0;

/// The key a plug-in's header picture is kept under for an interface theme.
fn header_key(id: &str, dark: bool) -> egui::Id {
    egui::Id::new(("plugin-own-header", id, dark))
}

/// Loads the header picture of every plug-in whose manifest carries one (`plugin.picture`), once
/// per module and interface theme: its page and its command dialogs draw it in place of its name.
/// A module without one clears the one kept for its id.
fn own_headers(app: &mut LccApp, ctx: &egui::Context, entries: &[Entry]) {
    let dark = ctx.theme() == egui::Theme::Dark;
    for e in entries.iter().filter(|e| !e.filter) {
        let hash = e.info["moduleHash"].as_str().unwrap_or("").to_string();
        let key = header_key(&e.id, dark);
        if ctx.data(|d| d.get_temp::<(String, Option<egui::TextureHandle>)>(key)).is_some_and(|(h, _)| h == hash) {
            continue;
        }
        if e.info["header"].as_bool() != Some(true) {
            ctx.data_mut(|d| d.insert_temp::<(String, Option<egui::TextureHandle>)>(key, (hash, None)));
            continue;
        }
        let theme = if dark { "dark" } else { "light" };
        let tex = app
            .session
            .execute("plugin.picture", &json!({"id": e.id, "picture": "header", "theme": theme}))
            .ok()
            .and_then(|r| r["svg"].as_str().map(str::to_string))
            .and_then(|svg| crate::plate::rasterise_untrusted_svg(&svg, HEADER_PIXELS.0, HEADER_PIXELS.1).ok())
            .map(|img| ctx.load_texture("plugin-own-header", img, egui::TextureOptions::LINEAR));
        ctx.data_mut(|d| d.insert_temp(key, (hash, tex)));
    }
}

/// Plug-in `id`'s header picture for the current interface theme, once [`own_headers`] loaded it.
fn header_of(ctx: &egui::Context, id: &str) -> Option<egui::TextureHandle> {
    let dark = ctx.theme() == egui::Theme::Dark;
    ctx.data(|d| d.get_temp::<(String, Option<egui::TextureHandle>)>(header_key(id, dark))).and_then(|(_, t)| t)
}

/// The list: plug-ins, then pixel filters, each under its heading.
fn list_ui(ui: &mut egui::Ui, entries: &[Entry], selected: Option<&str>, actions: &mut Vec<(&'static str, Value)>) {
    if entries.is_empty() {
        ui.weak(tr("No plug-ins installed."));
        return;
    }
    for (filter, heading) in [(false, tr("Plug-ins")), (true, tr("Pixel Filters"))] {
        let rows: Vec<&Entry> = entries.iter().filter(|e| e.filter == filter).collect();
        if rows.is_empty() {
            continue;
        }
        ui.add_space(4.0);
        ui.label(egui::RichText::new(heading.to_uppercase()).small().strong());
        for e in rows {
            if entry_row(ui, e, selected == Some(e.id.as_str())) {
                actions.push(("plugins.select", json!({"id": e.id})));
            }
        }
    }
}

fn mark_name(m: Mark) -> &'static str {
    match m {
        Mark::Enabled => tr("enabled"),
        Mark::Disabled => tr("disabled"),
        Mark::Failed => tr("didn't load"),
    }
}

/// An entry's mark, drawn by the app beside its icon (never in it): a filled dot when enabled, a
/// ring when disabled, a warning triangle when it didn't load.
fn paint_mark(painter: &egui::Painter, c: egui::Pos2, m: Mark, t: &crate::theme::Tokens) {
    match m {
        Mark::Enabled => {
            painter.circle_filled(c, 4.0, t.accent);
        }
        Mark::Disabled => {
            painter.circle_stroke(c, 3.5, egui::Stroke::new(1.3, t.text_dim));
        }
        Mark::Failed => {
            let pts = vec![c + egui::vec2(0.0, -5.0), c + egui::vec2(5.0, 4.0), c + egui::vec2(-5.0, 4.0)];
            painter.add(egui::Shape::convex_polygon(pts, t.caution, egui::Stroke::NONE));
        }
    }
}

/// One row of the list (`pluginEntry:<id>`): icon, mark, name and version. True when clicked.
fn entry_row(ui: &mut egui::Ui, e: &Entry, selected: bool) -> bool {
    let t = crate::theme::Tokens::get(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 30.0), egui::Sense::click());
    crate::widgets::register(ui.ctx(), format!("pluginEntry:{}", e.id), rect);
    crate::widgets::announce_selected(&resp, format!("{} {} ({})", e.name, e.version, mark_name(e.mark)), selected);
    let fill = if selected {
        t.pressed
    } else if resp.hovered() {
        t.hover
    } else {
        egui::Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 4.0, fill);
    let icon = egui::Rect::from_min_size(rect.min + egui::vec2(4.0, 4.0), egui::vec2(22.0, 22.0));
    super::plugin_icon::paint_for(ui, icon, &e.id);
    let mark = egui::pos2(icon.right() + 10.0, rect.center().y);
    paint_mark(ui.painter(), mark, e.mark, &t);
    let font = egui::FontId::proportional(13.0);
    let version =
        ui.painter().text(egui::pos2(rect.right() - 6.0, rect.center().y), egui::Align2::RIGHT_CENTER, &e.version, font.clone(), t.text_dim);
    let name_clip = egui::Rect::from_min_max(egui::pos2(mark.x + 9.0, rect.top()), egui::pos2(version.left() - 6.0, rect.bottom()));
    ui.painter().with_clip_rect(name_clip).text(egui::pos2(name_clip.left(), rect.center().y), egui::Align2::LEFT_CENTER, &e.name, font, t.text);
    resp.clicked()
}

/// The selected entry's page: its header, then what its kind shows.
fn page_ui(
    ui: &mut egui::Ui,
    e: &Entry,
    tab: &mut Tab,
    page: Option<&mut Page>,
    removing: &mut Option<Removing>,
    installs: &Installs,
    actions: &mut Vec<(&'static str, Value)>,
) {
    let t = crate::theme::Tokens::get(ui.ctx());
    ui.horizontal(|ui| {
        super::plugin_icon::show(ui, &e.id, 52.0);
        ui.vertical(|ui| {
            ui.horizontal_wrapped(|ui| {
                match header_of(ui.ctx(), &e.id).filter(|_| !e.filter) {
                    // the plug-in's own header picture stands for its name
                    Some(tex) => {
                        ui.add(egui::Image::new(&tex).max_height(HEADER_HEIGHT).max_width(ui.available_width() - 60.0)).on_hover_text(&e.name);
                    }
                    None => {
                        ui.label(egui::RichText::new(&e.name).font(t.semibold(16.0)).color(t.text));
                    }
                }
                if !e.version.is_empty() {
                    ui.weak(&e.version);
                }
            });
            match e.info["author"].as_str().filter(|a| !a.is_empty()) {
                Some(a) => ui.weak(tr_format!("by {author} · {id}", author = a, id = e.id)),
                None => ui.weak(&e.id),
            };
        });
    });
    ui.add_space(6.0);
    // another entry's question goes when this page shows
    if removing.as_ref().is_some_and(|r| r.entry != e.id) {
        *removing = None;
    }
    if removing.is_some() {
        confirm_remove(ui, e, removing, actions);
        ui.add_space(6.0);
    }
    match (e.mark, e.filter) {
        (Mark::Failed, _) => failed_page(ui, e, actions),
        (_, true) => filter_page(ui, e, actions),
        _ => plugin_page(ui, e, tab, page, installs, actions),
    }
}

/// A module's size in KB, as the page shows it.
fn kilobytes(info: &Value) -> u64 {
    info["size"].as_u64().unwrap_or(0).div_ceil(1024)
}

/// A plug-in's page: Enabled and Remove, then its tabs.
fn plugin_page(ui: &mut egui::Ui, e: &Entry, tab: &mut Tab, page: Option<&mut Page>, installs: &Installs, actions: &mut Vec<(&'static str, Value)>) {
    use crate::button::Button;
    let id = &e.id;
    ui.horizontal(|ui| {
        let mut on = e.info["enabled"].as_bool().unwrap_or(false);
        let r = ui.checkbox(&mut on, tr("Enabled"));
        crate::widgets::register(ui.ctx(), format!("pluginEnabled:{id}"), r.rect);
        if r.changed() {
            actions.push(("plugin.enable", json!({"id": id, "enabled": on})));
        }
        if Button::new("Remove").widget(format!("button:pluginRemove:{id}")).show(ui).clicked() {
            actions.push(("plugins.askRemove", json!({"entry": id})));
        }
        // the version its last update replaced
        if let Some(v) = e.info["previousVersion"].as_str() {
            let label = tr_format!("Go Back to Version {version}", version = v);
            if Button::new(&label).widget(format!("button:pluginRollback:{id}")).show(ui).clicked() {
                actions.push(("plugins.userRollback", json!({"id": id})));
            }
        }
    });
    updates_ui(ui, e, actions);
    ui.add_space(6.0);
    let tabs: Vec<Tab> = Tab::ALL.into_iter().filter(|t| *t != Tab::Settings || page.is_some()).collect();
    if !tabs.contains(tab) {
        *tab = Tab::About;
    }
    let items: Vec<(&str, &str)> = tabs.iter().map(|t| (t.label(), t.key())).collect();
    if let Some(t) = crate::widgets::segmented(ui, "pluginPage", &items, tabs.iter().position(|t| t == tab), items.len()).and_then(|i| tabs.get(i)) {
        *tab = *t;
    }
    ui.add_space(8.0);
    match *tab {
        Tab::Settings => settings_tab(ui, page, actions),
        Tab::Permissions => permissions_tab(ui, e, installs, actions),
        Tab::About => about_tab(ui, e, actions),
        Tab::Log => log_tab(ui, id),
    }
}

/// The plug-in's settings, saved together with Save (`plugin.setSettings`, checked by the host).
fn settings_tab(ui: &mut egui::Ui, page: Option<&mut Page>, actions: &mut Vec<(&'static str, Value)>) {
    use crate::button::{Button, Kind};
    let Some(pg) = page else { return };
    if let Some(e) = &pg.error {
        ui.colored_label(ui.visuals().warn_fg_color, e);
    }
    let scope = format!("settings:{}", pg.id);
    for f in &pg.fields {
        if field_ui(ui, &scope, f, &mut pg.values)
            && let Some(k) = f["key"].as_str()
        {
            pg.edited.insert(k.to_string());
        }
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if Button::new("Save").kind(Kind::Primary).widget("button:pluginSettingsSave").show(ui).clicked() {
            // only what the user changed: a value set meanwhile by the plug-in stays
            let changed: serde_json::Map<String, Value> =
                pg.edited.iter().map(|k| (k.clone(), pg.values.get(k).cloned().unwrap_or(Value::Null))).collect();
            actions.push(("plugin.setSettings", json!({"id": pg.id, "values": changed})));
        }
        if Button::new("Revert").widget("button:pluginSettingsRevert").show(ui).clicked() {
            actions.push(("plugins.revertSettings", json!({"id": pg.id})));
        }
    });
}

/// Each permission the plug-in asks for, with where it stands (allowed, denied on a date, not
/// asked yet, or a folder that can't be granted and why), and **Allow** or **Revoke**.
fn permissions_tab(ui: &mut egui::Ui, e: &Entry, installs: &Installs, actions: &mut Vec<(&'static str, Value)>) {
    use crate::button::Button;
    let rows = permission_rows(&e.info["requested"]);
    if rows.is_empty() {
        ui.label(tr("It asks for no permissions: it can only keep its own data."));
        return;
    }
    let states = e.info["requests"].as_array().cloned().unwrap_or_default();
    for (i, row) in rows.iter().enumerate() {
        let req = row.request();
        let state =
            states.iter().find(|r| r["group"] == req["group"] && (r["value"] == req["value"] || req["value"].is_null() && r.get("value").is_none()));
        let state = state.cloned().unwrap_or(Value::Null);
        ui.horizontal_wrapped(|ui| {
            ui.label(row.label(installs, &e.id));
            let what = state["state"].as_str().unwrap_or("never");
            match what {
                "allowed" => {
                    ui.weak(tr("allowed"));
                }
                "denied" => {
                    ui.weak(tr_format!("denied on {date}", date = state["deniedOn"].as_str().unwrap_or("")));
                }
                "refused" => {
                    ui.weak(format!("{} {}", tr("can't be granted:"), state["reason"].as_str().unwrap_or("")));
                }
                _ => {
                    ui.weak(tr("not asked yet"));
                }
            }
            let mut call = req.clone();
            call["id"] = json!(e.id);
            if what == "allowed" {
                if Button::new("Revoke").small().widget(format!("pluginRevoke:{}:{i}", e.id)).show(ui).clicked() {
                    actions.push(("plugin.revoke", call));
                }
            } else if what != "refused" && Button::new("Allow").small().widget(format!("pluginAllow:{}:{i}", e.id)).show(ui).clicked() {
                actions.push(("plugins.userAllow", call));
            }
        });
    }
}

/// What the plug-in is and adds, its commands to run, and its module's size.
fn about_tab(ui: &mut egui::Ui, e: &Entry, actions: &mut Vec<(&'static str, Value)>) {
    let p = &e.info;
    if let Some(d) = p["description"].as_str().filter(|d| !d.is_empty()) {
        ui.label(d);
        ui.add_space(4.0);
    }
    let adds = contributions(p);
    if !adds.is_empty() {
        ui.label(format!("{} {}", tr("Adds:"), adds.join(" · ")));
    }
    if p["enabled"].as_bool() == Some(true) {
        ui.horizontal_wrapped(|ui| {
            for c in p["commands"].as_array().into_iter().flatten() {
                let cid = c["id"].as_str().unwrap_or_default();
                if ui.button(c["label"].as_str().unwrap_or(cid)).clicked() {
                    actions.push(("plugins.runItem", json!({"plugin": e.id, "command": cid})));
                }
            }
        });
    }
    ui.add_space(4.0);
    about_rows(ui, e, actions);
    ui.weak(tr_format!("Module: {size} KB", size = kilobytes(p)));
}

/// The links an entry's About may open (`plugins.openLink` → `link`) besides `issues`, in the
/// order of their rows.
const ABOUT_LINKS: &[&str] = &["homepage", "repository", "changelog"];

/// The name of link `key`'s row.
fn about_link_name(key: &str) -> &'static str {
    match key {
        "homepage" => tr("Homepage"),
        "repository" => tr("Source Code"),
        _ => tr("Changelog"),
    }
}

/// Where a plug-in or filter comes from (`plugin.list` → `about`): Homepage, Source Code and Changelog
/// as links named by their host, so the user sees where one goes before clicking (the whole link
/// on hover); Report a Problem beside a GitHub repository; Licence as text. Only the fields the
/// manifest has get a row, and nothing is fetched to show them.
fn about_rows(ui: &mut egui::Ui, e: &Entry, actions: &mut Vec<(&'static str, Value)>) {
    let about = &e.info["about"];
    let mut link = |ui: &mut egui::Ui, key: &'static str, text: &str, url: &str| {
        let r = ui.link(text).on_hover_text(url);
        crate::widgets::register(ui.ctx(), format!("link:plugin-{key}-{}", e.id), r.rect);
        if r.clicked() {
            actions.push(("plugins.openLink", json!({"id": e.id, "link": key})));
        }
    };
    for &key in ABOUT_LINKS {
        let Some(url) = about[key].as_str() else { continue };
        let Some(host) = lcc_engine::cmd::plugins::link_host(url) else { continue };
        about_row(ui, about_link_name(key), |ui| {
            link(ui, key, &host, url);
            if let Some(issues) = about["issues"].as_str().filter(|_| key == "repository") {
                ui.weak("·");
                link(ui, "issues", tr("Report a Problem"), issues);
            }
        });
    }
    if let Some(l) = about["license"].as_str() {
        fact_row(ui, tr("Licence"), l);
    }
}

/// A fact row ([`fact_row`]) whose value is drawn by `value` (links).
fn about_row(ui: &mut egui::Ui, name: &str, value: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(egui::vec2(FACT_W, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
            ui.set_width(FACT_W);
            ui.weak(name);
        });
        ui.horizontal_wrapped(value);
    });
}

/// `plugins.openLink {id, link}`: opens link `link` (`homepage`, `repository`, `issues` or
/// `changelog`) of installed plug-in or filter `id`'s About in the browser, as its page's link
/// does, or (`updateChangelog`) the changelog of the update its last check found. The browser
/// service opens only `https://` links.
fn open_link(app: &mut LccApp, p: &Value) -> Result<Value, String> {
    let (Some(id), Some(link)) = (p.get("id").and_then(Value::as_str), p.get("link").and_then(Value::as_str)) else {
        return Err("plugins.openLink needs `id` and `link`".into());
    };
    if link != "issues" && link != "updateChangelog" && !ABOUT_LINKS.contains(&link) {
        return Err(format!("plugins.openLink: no link {link:?} (homepage, repository, issues, changelog or updateChangelog)"));
    }
    let list = app.session.execute("plugin.list", &json!({})).map_err(|e| e.to_string())?;
    let entry = entries(&list).into_iter().find(|e| e.id == id && e.mark != Mark::Failed);
    let entry = entry.ok_or_else(|| format!("{id}: {}", tr("no such plug-in is installed")))?;
    let url = match link {
        // the changelog the update found names (`plugin.list` → updates.offer)
        "updateChangelog" => entry.info["updates"]["offer"]["changelog"].as_str(),
        _ => entry.info["about"][link].as_str(),
    };
    let url = url.ok_or_else(|| format!("{id}: {}", tr_format!("its manifest has no {link} link", link = link)))?;
    crate::links::open(app, url)
}

/// A plug-in's update check on its page: the *Check for updates* box and **Check Now** for a
/// plug-in that can have one (none for any other: its first stamped, signed version is a hand
/// install), why the last check failed, and the update it found ([`update_row`]).
fn updates_ui(ui: &mut egui::Ui, e: &Entry, actions: &mut Vec<(&'static str, Value)>) {
    use crate::button::Button;
    let u = &e.info["updates"];
    if u["available"].as_bool() != Some(true) {
        return;
    }
    let id = &e.id;
    ui.horizontal(|ui| {
        let mut on = u["check"].as_bool().unwrap_or(false);
        let r = ui.checkbox(&mut on, tr("Check for updates"));
        crate::widgets::register(ui.ctx(), format!("pluginUpdateCheck:{id}"), r.rect);
        if r.changed() {
            actions.push(("plugin.setUpdateCheck", json!({"id": id, "on": on})));
        }
        if Button::new("Check Now").small().widget(format!("button:pluginCheckUpdates:{id}")).show(ui).clicked() {
            actions.push(("plugins.checkUpdates", json!({"id": id})));
        }
    });
    if let Some(err) = u["error"].as_str() {
        ui.colored_label(ui.visuals().warn_fg_color, tr_format!("Couldn't check for updates: {error}", error = err));
    }
    if u["offer"].is_object() {
        update_row(ui, e, &u["offer"], actions);
    }
}

/// The update a plug-in's last check found, all from its feed (nothing is downloaded to show
/// it): the version, the author's note as plain text, the changelog link, the permission groups
/// the feed says it adds, and **Update**, or the host features this version lacks.
fn update_row(ui: &mut egui::Ui, e: &Entry, offer: &Value, actions: &mut Vec<(&'static str, Value)>) {
    use crate::button::{Button, Kind};
    let id = &e.id;
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_wrapped(|ui| {
            ui.strong(tr_format!("Version {version} is available", version = offer["version"].as_str().unwrap_or("")));
            if let Some(url) = offer["changelog"].as_str()
                && let Some(host) = lcc_engine::cmd::plugins::link_host(url)
            {
                ui.weak("·");
                let r = ui.link(format!("{} ({host})", tr("Changelog"))).on_hover_text(url);
                crate::widgets::register(ui.ctx(), format!("link:plugin-updateChangelog-{id}"), r.rect);
                if r.clicked() {
                    actions.push(("plugins.openLink", json!({"id": id, "link": "updateChangelog"})));
                }
            }
        });
        // plain text, cut by the host: markup and links show as the text they are
        if let Some(notes) = offer["notes"].as_str() {
            ui.label(notes);
        }
        let adds: Vec<&str> = offer["adds"].as_array().into_iter().flatten().filter_map(Value::as_str).map(group_label).collect();
        if !adds.is_empty() {
            ui.weak(format!("{} {}", tr("Its update feed says it also asks to:"), adds.join(" · ")));
        }
        let missing: Vec<&str> = offer["missing"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        if missing.is_empty() {
            if Button::new("Update").kind(Kind::Primary).widget(format!("button:pluginUpdate:{id}")).show(ui).clicked() {
                actions.push(("plugins.userUpdate", json!({"id": id})));
            }
        } else {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!("{} {}", tr("It needs features this version of the app doesn't have:"), missing.join(", ")),
            );
        }
    });
}

/// A permission group as the update row names it.
fn group_label(group: &str) -> &'static str {
    match group {
        "catalog" => tr("Read the catalog"),
        "metadataWrite" => tr("Write photo metadata"),
        "libraryAdd" => tr("Add files made from photos to the catalog"),
        "pixels" => tr("Look at downsized copies of photos"),
        "crop" => tr("Change the crop of photos"),
        "virtualCopy" => tr("Make virtual copies of photos"),
        "network" => tr("Connect to the network"),
        "fs" => tr("Read or write files"),
        _ => tr("Start programs"),
    }
}

/// One request an update asks for (`plugin.update` → requests: `{group, value?}`), as the
/// Permissions tab words it.
fn request_label(r: &Value) -> String {
    let group = r["group"].as_str().unwrap_or("");
    match (group, r["value"].as_str()) {
        ("network", Some(h)) => format!("{} {h}", tr("Connect to")),
        ("fs", Some(path)) => format!("{} {path}", tr("Read or write files in")),
        ("programs", Some(p)) => format!("{} {p}", tr("Start the program")),
        _ => group_label(group).to_string(),
    }
}

/// The question an update asks when its file wants permission groups its feed didn't declare:
/// what it asks for, then **Update Anyway** (downloaded and checked again, then installed; none
/// of it is granted) or **Cancel**.
fn update_ask_ui(ctx: &egui::Context, s: &mut State, actions: &mut Vec<(&'static str, Value)>) {
    let Some(ask) = s.update_ask.take() else { return };
    let mut keep = true;
    let window_id = key().with("update-ask");
    egui::Window::new(tr("Update"))
        .id(window_id)
        .title_bar(false)
        .frame(crate::dialog_frame::card_frame(ctx))
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            crate::dialog_frame::card_header(ui, &ask.name, None);
            ui.label(tr("It asks for more than its update feed said:"));
            for r in &ask.requests {
                ui.label(format!("• {r}"));
            }
            ui.weak(tr("Nothing new is allowed: it asks when it needs one."));
            ui.add_space(6.0);
            button_row(ui, |ui| {
                use crate::button::{Button, Kind};
                if Button::new("Update Anyway").kind(Kind::Primary).widget("button:pluginUpdateAnyway").show(ui).clicked() {
                    actions.push(("plugins.userUpdate", json!({"id": ask.id, "acceptUndeclared": true})));
                    keep = false;
                }
                if Button::new("Cancel").widget("button:pluginUpdateCancel").show(ui).clicked() {
                    keep = false;
                }
            });
        });
    if keep {
        s.update_ask = Some(ask);
    }
}

/// An update check or download running on its worker ([`start_check`], [`start_update`]).
struct UpdateTask {
    done: std::sync::mpsc::Receiver<UpdateDone>,
    /// Its row in the activity stack, while it runs.
    _guard: TaskGuard,
}

enum UpdateDone {
    Checked(Result<Value, String>),
    /// Plug-in `id`'s update downloaded (or why not), to install here with the user's answer.
    Downloaded {
        id: String,
        accept: bool,
        by_user: bool,
        bytes: Result<Vec<u8>, String>,
    },
}

#[cfg(not(test))]
static UPDATE: Mutex<Option<UpdateTask>> = Mutex::new(None);

// Under test, each test's own, as [`TASK`].
#[cfg(test)]
thread_local! {
    static UPDATE: &'static Mutex<Option<UpdateTask>> = Box::leak(Box::new(Mutex::new(None)));
}

fn update_lock() -> std::sync::MutexGuard<'static, Option<UpdateTask>> {
    #[cfg(not(test))]
    let task = &UPDATE;
    #[cfg(test)]
    let task = UPDATE.with(|t| *t);
    task.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Whether an update check or download is running (`ui.inspect`, tests).
pub fn updates_running() -> bool {
    update_lock().is_some()
}

/// Runs `work` on a worker with an activity row `label`, one at a time.
fn start_update_work(app: &mut LccApp, ctx: &egui::Context, label: &str, work: impl FnOnce() -> UpdateDone + Send + 'static) -> Result<(), String> {
    let mut g = update_lock();
    if g.is_some() {
        return Err(tr("A plug-in update is already being checked or downloaded").to_string());
    }
    let (tx, done) = std::sync::mpsc::channel();
    let repaint = ctx.clone();
    std::thread::Builder::new()
        .name("plugin-updates".into())
        .spawn(move || {
            let _ = tx.send(work());
            repaint.request_repaint();
        })
        .map_err(|e| e.to_string())?;
    let guard = app.session.activity.start("plugins", label, Cancel::No);
    *g = Some(UpdateTask { done, _guard: guard });
    Ok(())
}

/// Checks update feeds on a worker: plug-in `id` now, or (no `id`) those whose check is on, only
/// the ones due when `due` (the window opening). Nothing to check starts nothing.
fn start_check(app: &mut LccApp, ctx: &egui::Context, id: Option<String>, due: bool) -> Result<(), String> {
    if id.is_none() {
        let now = chrono::Utc::now().timestamp();
        let g = lcc_engine::cmd::plugins::manager();
        let any = g.as_ref().and_then(|m| m.update_checks(None, due.then_some(now)).ok()).is_some_and(|c| !c.is_empty());
        if !any {
            return Ok(());
        }
    }
    start_update_work(app, ctx, "Checking plug-ins for updates", move || {
        UpdateDone::Checked(lcc_engine::cmd::plugins::check_updates(id.as_deref(), due).map_err(|e| e.to_string()))
    })
}

/// Downloads plug-in `id`'s update on a worker; [`poll_updates`] installs it.
fn start_update(app: &mut LccApp, ctx: &egui::Context, id: String, accept: bool, by_user: bool) -> Result<(), String> {
    start_update_work(app, ctx, "Downloading a plug-in update", move || {
        let bytes = lcc_engine::cmd::plugins::download_update(&id).map_err(|e| e.to_string());
        UpdateDone::Downloaded { id, accept, by_user, bytes }
    })
}

/// Per frame: when an update check ends, says why it failed; when a download ends, installs it
/// (`plugin.update`'s path), or asks the user about permission groups its feed didn't declare.
fn poll_updates(app: &mut LccApp, ctx: &egui::Context) {
    let mut g = update_lock();
    let Some(task) = g.as_ref() else { return };
    let done = match task.done.try_recv() {
        Ok(d) => d,
        Err(std::sync::mpsc::TryRecvError::Empty) => return,
        Err(std::sync::mpsc::TryRecvError::Disconnected) => UpdateDone::Checked(Err(tr("the plug-in worker stopped unexpectedly").to_string())),
    };
    *g = None;
    drop(g);
    let mut s = state(ctx);
    match done {
        UpdateDone::Checked(Ok(_)) => {}
        UpdateDone::Checked(Err(e)) => s.message = e,
        UpdateDone::Downloaded { bytes: Err(e), .. } => s.message = e,
        UpdateDone::Downloaded { id, accept, by_user, bytes: Ok(bytes) } => {
            match lcc_engine::cmd::plugins::install_update(&mut app.session, &id, &bytes, accept, by_user) {
                Ok(v) if v["updated"] == json!(true) => {
                    let p = &v["plugin"];
                    let (name, version) = (p["name"].as_str().unwrap_or(&id), p["version"].as_str().unwrap_or(""));
                    app.toast(ctx, tr_format!("Updated {name} to version {version}", name = name, version = version));
                }
                Ok(v) => {
                    let name = lcc_engine::cmd::plugins::manager()
                        .as_ref()
                        .and_then(|m| m.get(&id).map(|i| i.plugin.manifest().name.clone()))
                        .unwrap_or_else(|| id.clone());
                    let requests = v["requests"].as_array().into_iter().flatten().map(request_label).collect();
                    s.update_ask = Some(UpdateAsk { id, name, requests });
                }
                Err(e) => s.message = e.to_string(),
            }
        }
    }
    set_state(ctx, s);
    ctx.request_repaint();
}

/// What the plug-in has logged.
fn log_tab(ui: &mut egui::Ui, id: &str) {
    let g = lcc_engine::cmd::plugins::manager();
    let lines: Vec<String> = g.as_ref().and_then(|m| m.get(id)).map(|i| i.log.iter().cloned().collect()).unwrap_or_default();
    drop(g);
    if lines.is_empty() {
        ui.weak(tr("Nothing logged yet."));
    }
    for l in &lines {
        ui.monospace(l);
    }
}

/// A pixel filter's page: what it is and Remove. It has no settings, permissions, log, or Enabled
/// box (a filter can't be turned off).
fn filter_page(ui: &mut egui::Ui, e: &Entry, actions: &mut Vec<(&'static str, Value)>) {
    use crate::button::Button;
    let p = &e.info;
    if Button::new("Remove").widget(format!("button:pluginRemove:{}", e.id)).show(ui).clicked() {
        actions.push(("plugins.askRemove", json!({"entry": e.id})));
    }
    ui.add_space(6.0);
    if let Some(d) = p["description"].as_str().filter(|d| !d.is_empty()) {
        ui.label(d);
        ui.add_space(4.0);
    }
    let params: Vec<String> = p["paramOrder"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|k| p["params"][k]["label"].as_str().unwrap_or(k).to_string())
        .collect();
    let hash = p["moduleHash"].as_str().unwrap_or("");
    let stage =
        if p["stage"] == "source" { tr("Source: on the raw, before the develop adjustments") } else { tr("Display: after the develop adjustments") };
    fact_row(ui, tr("Stage"), stage);
    let measures = match (p["analyze"].as_bool() == Some(true), p["analyzeArea"] == "crop") {
        (false, _) => tr("No"),
        (true, true) => tr("Yes: the cropped photo"),
        (true, false) => tr("Yes: the whole photo"),
    };
    fact_row(ui, tr("Measures"), measures);
    fact_row(ui, tr("Base tone"), if p["baseTone"] == "neutral" { tr("Neutral (its own rendering)") } else { tr("The camera's") });
    fact_row(ui, tr("Parameters"), &if params.is_empty() { tr("None").to_string() } else { params.join(", ") });
    let module = format!("{} KB · sha256 {}…", kilobytes(p), hash.get(..12).unwrap_or(hash));
    fact_row(ui, tr("Module"), &module).on_hover_text(format!("sha256 {hash}"));
    about_rows(ui, e, actions);
}

/// The width of a filter fact's name.
const FACT_W: f32 = 90.0;

/// One of a filter's facts: its name, and beside it the value, wrapped to the page's width (a
/// grid's cells don't wrap: a long parameter list would widen the window).
fn fact_row(ui: &mut egui::Ui, name: &str, value: &str) -> egui::Response {
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(egui::vec2(FACT_W, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
            ui.set_width(FACT_W);
            ui.weak(name);
        });
        ui.add(egui::Label::new(value).wrap())
    })
    .inner
}

/// A module that didn't load: why, and Remove.
fn failed_page(ui: &mut egui::Ui, e: &Entry, actions: &mut Vec<(&'static str, Value)>) {
    use crate::button::Button;
    let t = crate::theme::Tokens::get(ui.ctx());
    ui.colored_label(t.caution, tr("It didn't load."));
    ui.label(e.info["error"].as_str().unwrap_or(""));
    let missing: Vec<&str> = e.info["missing"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    if !missing.is_empty() {
        ui.label(tr("It needs features this version of the app doesn't have:"));
        for f in missing {
            ui.label(format!("• {f}"));
        }
    }
    if let Some(file) = e.info["file"].as_str() {
        ui.weak(file);
    }
    ui.add_space(6.0);
    // a module that didn't load can still be removed
    if Button::new("Remove").widget(format!("button:pluginRemoveFailed:{}", e.id)).show(ui).clicked() {
        actions.push(("plugins.askRemove", json!({"entry": e.id})));
    }
}

/// Remove's question: Remove (keeping a plug-in's data and settings when ticked) or Cancel, which
/// changes nothing.
fn confirm_remove(ui: &mut egui::Ui, e: &Entry, removing: &mut Option<Removing>, actions: &mut Vec<(&'static str, Value)>) {
    use crate::button::{Button, Kind};
    let Some(r) = removing.as_mut() else { return };
    let t = crate::theme::Tokens::get(ui.ctx());
    let mut done = false;
    egui::Frame::NONE.fill(t.inset).stroke(egui::Stroke::new(1.0, t.caution.gamma_multiply(0.7))).corner_radius(4.0).inner_margin(8).show(ui, |ui| {
        ui.label(tr_format!("Remove {name}?", name = e.name));
        if !e.filter {
            let c = ui.checkbox(&mut r.keep_data, tr("Keep its data and settings, for when it's installed again"));
            crate::widgets::register(ui.ctx(), format!("pluginKeepData:{}", e.id), c.rect);
        }
        ui.horizontal(|ui| {
            if Button::new("Remove").kind(Kind::Primary).widget(format!("button:pluginRemoveConfirm:{}", e.id)).show(ui).clicked() {
                // a module that didn't load goes by its file, so only it goes when another module has its id
                let mut p = match (e.mark, e.info["file"].as_str()) {
                    (Mark::Failed, Some(file)) => json!({"file": file}),
                    _ => json!({"id": e.id}),
                };
                if !e.filter {
                    p["keepData"] = json!(r.keep_data);
                }
                actions.push(("plugin.uninstall", p));
                done = true;
            }
            if Button::new("Cancel").widget(format!("button:pluginRemoveCancel:{}", e.id)).show(ui).clicked() {
                done = true;
            }
        });
    });
    if done {
        *removing = None;
    }
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

/// One dialog or settings field; `scope` (the plug-in and its dialog) keeps widget ids apart. True
/// when the user changed it.
fn field_ui(ui: &mut egui::Ui, scope: &str, f: &Value, values: &mut Value) -> bool {
    let Some(k) = f["key"].as_str() else { return false };
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
                r.changed()
            }
            Some("bool") => {
                let mut b = v.as_bool().unwrap_or(false);
                let r = ui.checkbox(&mut b, "");
                crate::access::label(&r, label);
                *v = json!(b);
                r.changed()
            }
            Some("choice") => {
                let was = v.as_str().unwrap_or("").to_string();
                let mut cur = was.clone();
                let r = egui::ComboBox::from_id_salt(("plugin-field", scope, k)).selected_text(cur.clone()).show_ui(ui, |ui| {
                    for o in f["options"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                        ui.selectable_value(&mut cur, o.to_string(), o);
                    }
                });
                crate::widgets::register(ui.ctx(), format!("pluginChoice:{scope}:{k}"), r.response.rect);
                crate::access::label(&r.response, label);
                *v = json!(cur);
                cur != was
            }
            _ => {
                let was = v.as_str().unwrap_or("").to_string();
                let mut t = was.clone();
                let widget = format!("pluginText:{scope}:{k}");
                let r = crate::text_field::TextField::singleline(&widget, &mut t).width(ui.available_width().min(240.0)).show(ui);
                crate::access::label(&r.response, label);
                *v = json!(t);
                t != was
            }
        }
    })
    .inner
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Under test every test has a Plug-in Manager of its own: one opened on another test's thread
    /// doesn't cover this test's window.
    #[test]
    fn a_manager_opened_on_another_test_thread_stays_there() {
        let ctx = egui::Context::default();
        let other = std::thread::spawn(|| {
            let ctx = egui::Context::default();
            set_state(&ctx, State { open: true, ..State::default() });
            state(&ctx).open
        });
        assert!(other.join().unwrap(), "open on its own thread");
        assert!(!state(&ctx).open, "and closed on this one");
    }

    /// So are the running export hooks: hooks started on another test's thread don't run here.
    #[test]
    fn export_hooks_started_on_another_test_thread_stay_there() {
        let other = std::thread::spawn(|| {
            let session = lcc_engine::Session::new();
            let (_asks, requests) = std::sync::mpsc::channel();
            let (_ends, results) = std::sync::mpsc::channel();
            let guard = session.activity.start("plugins", "Running plug-in export hooks", Cancel::No);
            *task_lock() = Some(ExportHooksTask { total: 1, progress: Arc::default(), requests, results, guard });
            let running = export_hooks_running();
            *task_lock() = None;
            running
        });
        assert!(other.join().unwrap(), "running on its own thread");
        assert!(!export_hooks_running(), "and not on this one");
    }

    /// And the plug-in export destination chosen in the Export dialog.
    #[test]
    fn a_destination_chosen_on_another_test_thread_stays_there() {
        let other = std::thread::spawn(|| {
            *destination_lock() = Some(("test.plugin".into(), "upload".into(), json!({})));
            destination_lock().is_some()
        });
        assert!(other.join().unwrap(), "chosen on its own thread");
        assert!(destination_lock().is_none(), "and none on this one");
    }

    #[test]
    fn rows_name_the_request_each_one_is() {
        let req = json!({"catalog": true, "metadataWrite": false, "network": ["a.com", "b.com"], "fs": [{"path": "/x", "write": true}]});
        let rows = permission_rows(&req);
        assert_eq!(rows.len(), 4, "catalog, two hosts, one root");
        let requests: Vec<Value> = rows.iter().map(Row::request).collect();
        assert_eq!(
            requests,
            vec![
                json!({"group": "catalog"}),
                json!({"group": "network", "value": "a.com"}),
                json!({"group": "network", "value": "b.com"}),
                json!({"group": "fs", "value": "/x"})
            ]
        );
        // the order plugin.list gives its requests in
        let manifest: lcc_plugins::Permissions = serde_json::from_value(req).unwrap();
        let listed: Vec<Value> = manifest.requests().iter().map(|r| serde_json::to_value(r).unwrap()).collect();
        assert_eq!(listed, requests);
    }

    #[test]
    fn program_rows_say_where_the_program_is() {
        let conv = json!({"id": "conv", "name": "Converter", "macBundleIds": ["org.example.conv"]});
        let rows = permission_rows(&json!({"catalog": true, "programs": [conv.clone(), {"id": "x", "name": "X", "linuxExe": "x"}]}));
        assert_eq!(rows.len(), 3, "catalog and two programs");
        let mut installs = Installs::new();
        installs.insert(("p".into(), "conv".into()), "installed: Converter 2.0".into());
        let label = rows[1].label(&installs, "p");
        assert!(label.contains("Converter") && label.contains("installed: Converter 2.0"), "{label}");
        assert_eq!(rows[1].request(), json!({"group": "programs", "value": "conv"}));
        let g = json!({"catalog": true, "programs": [conv]});
        assert!(rows[1].granted(&g) && !rows[2].granted(&g));
        if cfg!(target_os = "macos") {
            assert!(label.contains("org.example.conv"), "the row names what it starts: {label}");
        }
    }

    #[test]
    fn a_widened_program_is_no_longer_allowed_and_a_narrowed_one_is() {
        let had = json!({"programs": [{"id": "conv", "name": "Converter", "macBundleIds": ["a.one", "a.two"], "linuxExe": "conv"}]});
        let rows = |entry: Value| permission_rows(&json!({"programs": [entry]}));
        let narrowed = rows(json!({"id": "conv", "name": "Converter 2", "macBundleIds": ["a.two"]}));
        assert!(narrowed[0].granted(&had), "renamed and narrowed: still granted");
        let more_ids = rows(json!({"id": "conv", "name": "Converter", "macBundleIds": ["a.one", "a.three"], "linuxExe": "conv"}));
        assert!(!more_ids[0].granted(&had), "another bundle id: no longer allowed");
        let other_exe = rows(json!({"id": "conv", "name": "Converter", "macBundleIds": ["a.one"], "linuxExe": "sh"}));
        assert!(!other_exe[0].granted(&had), "another executable: no longer allowed");
        let added_exe = rows(json!({"id": "conv", "name": "Converter", "macBundleIds": ["a.one"], "linuxExe": "conv", "windowsExe": "conv.exe"}));
        assert!(!added_exe[0].granted(&had), "an added executable: no longer allowed");
    }
}
