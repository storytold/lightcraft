//! Edit In and Actions, the desktop half (P4.4 / P4.5).
//!
//! - After `photo.editIn` / `photo.openAsLayers` made a file, open it in the preset's application
//!   and bring the editor's saves back ([`after_command`], [`watch`], [`pump`]):
//!   - a preset with PhotoCraft's control channel opens the file there (`app.open`) and subscribes
//!     to its events: `document.saved` reloads the photo at once, `document.closed` stops tracking;
//!   - an editor that takes `--wait` (PhotoCraft, feature-detected, or a preset whose arguments
//!     hold it) runs on a worker thread with an activity row, and the photo reloads when it exits
//!     (`--single-instance` too when supported, so repeated Edit In opens tabs in one window);
//!   - anything else opens through the host's `open_with`;
//!   - always: the photo reloads when the app regains focus, and a cheap modification-time poll
//!     of the files out for editing (once a second) catches saves while the app keeps the focus.
//! - Edit In on a PSD or TIFF asks, as Lightroom does, whether to edit the original (the default),
//!   a copy, or a copy with the adjustments ([`ask_mode`]).
//! - Photo ▸ Edit In ▸ Edit In Presets… (`dialog.editInPresets`): a preset editor over the
//!   `editIn.*` commands.
//! - Window ▸ Actions (`panel.actions`): record / stop / play / delete actions and set their
//!   shortcuts, over the `actions.*` commands.
//! - Saved action shortcuts play their action ([`show`] checks them every frame while no text
//!   field has the keyboard).
//!
//! The windows' state is per UI thread (the app has one), so no shared app state is touched.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use lcc_engine::cmd::edit_in::editor::{self, EditorEvent, Watched};
use serde_json::{Value, json};

use crate::LccApp;
use crate::i18n::tr;

/// The ids [`intercept`] handles.
pub const PRESETS_DIALOG: &str = "dialog.editInPresets";
pub const ACTIONS_PANEL: &str = "panel.actions";

const FORMATS: &[&str] = &["tiff", "psd"];
const SPACES: &[&str] = &["adobeRgb", "proPhoto", "displayP3", "srgb"];
const MODES: &[(&str, &str)] = &[("copyWithAdjustments", "Edit a Copy with Adjustments"), ("copy", "Edit a Copy"), ("original", "Edit Original")];

/// The preset being edited.
#[derive(Clone, Debug, PartialEq)]
pub struct Draft {
    pub name: String,
    pub app: String,
    /// Arguments, separated by spaces.
    pub args: String,
    pub format: String,
    pub color_space: String,
    pub bit_depth: u8,
    pub mode: String,
    pub naming: String,
    pub stack: bool,
}

impl Default for Draft {
    fn default() -> Self {
        Draft {
            name: String::new(),
            app: String::new(),
            args: String::new(),
            format: "tiff".into(),
            color_space: "adobeRgb".into(),
            bit_depth: 16,
            mode: "copyWithAdjustments".into(),
            naming: "{name}-Edit".into(),
            stack: true,
        }
    }
}

impl Draft {
    fn from_preset(p: &Value) -> Self {
        let s = |k: &str, d: &str| p[k].as_str().unwrap_or(d).to_string();
        Draft {
            name: s("name", ""),
            app: s("app", ""),
            args: p["args"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" ")).unwrap_or_default(),
            format: s("format", "tiff"),
            color_space: s("colorSpace", "adobeRgb"),
            bit_depth: p["bitDepth"].as_u64().and_then(|b| u8::try_from(b).ok()).unwrap_or(16),
            mode: s("mode", "copyWithAdjustments"),
            naming: s("naming", "{name}-Edit"),
            stack: p["stack"].as_bool().unwrap_or(true),
        }
    }

    pub fn to_params(&self) -> Value {
        json!({
            "name": self.name.trim(),
            "app": self.app.trim(),
            "args": self.args.split_whitespace().collect::<Vec<_>>(),
            "format": self.format,
            "colorSpace": self.color_space,
            "bitDepth": self.bit_depth,
            "mode": self.mode,
            "naming": self.naming,
            "stack": self.stack,
        })
    }
}

/// The two windows' state.
#[derive(Clone, Debug, Default)]
pub struct WorkflowUi {
    pub presets_open: bool,
    pub actions_open: bool,
    pub draft: Draft,
    /// Name typed for a new recording.
    pub record_name: String,
    /// The action selected in the panel, and its shortcut being edited.
    pub selected: Option<String>,
    pub shortcut: String,
    /// The last command's outcome, shown at the bottom of the window.
    pub message: String,
    /// Edit In on a PSD/TIFF waiting for the user's choice: the command's parameters and the mode
    /// picked so far.
    pub ask: Option<(Value, String)>,
}

thread_local! {
    static STATE: RefCell<WorkflowUi> = RefCell::new(WorkflowUi::default());
}

/// A copy of the windows' state (tests, `ui.inspect`-style checks).
#[cfg(test)]
pub fn state() -> WorkflowUi {
    STATE.with_borrow(Clone::clone)
}

fn update(f: impl FnOnce(&mut WorkflowUi)) {
    STATE.with_borrow_mut(f);
}

/// Open the windows by command id (menu items, control channel). `None` = not ours.
pub(crate) fn intercept(_app: &mut LccApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    let show = p.get("show").and_then(Value::as_bool);
    match id {
        PRESETS_DIALOG => {
            update(|s| s.presets_open = show.unwrap_or(true));
            Some(Ok(Value::Null))
        }
        ACTIONS_PANEL => {
            update(|s| s.actions_open = show.unwrap_or(!s.actions_open));
            Some(Ok(Value::Null))
        }
        "photo.editIn" => ask_mode(_app, p),
        _ => None,
    }
}

/// Edit In from a menu or shortcut (no `mode`, nothing scripted) on a PSD or TIFF: ask first, as
/// Lightroom does (Edit Original preselected). `None` = run the command as it is.
pub(crate) fn ask_mode(app: &LccApp, p: &Value) -> Option<Result<Value, String>> {
    let plain = match p {
        Value::Null => true,
        Value::Object(o) => o.keys().all(|k| matches!(k.as_str(), "id" | "preset")),
        _ => false,
    };
    if !plain || p.get("mode").is_some() {
        return None;
    }
    let id = p.get("id").and_then(Value::as_u64).map(lcc_catalog::PhotoId).or_else(|| app.session.active())?;
    let ph = app.session.catalog.photo(id)?;
    if lcc_engine::cmd::edit_in::suggested_mode(&ph.file_name) != lcc_engine::cmd::edit_in::Mode::Original {
        return None;
    }
    let mut params = if p.is_object() { p.clone() } else { json!({}) };
    if let Some(o) = params.as_object_mut() {
        o.insert("id".into(), json!(id.0));
    }
    update(|s| s.ask = Some((params, "original".into())));
    Some(Ok(json!({"asked": true})))
}

/// Called with every engine command's successful result.
pub(crate) fn after_command(app: &mut LccApp, id: &str, r: &Value) {
    if !matches!(id, "photo.editIn" | "photo.openAsLayers") || !r["opened"].is_null() {
        return;
    }
    let w = &r["openWith"];
    let Some(path) = w["path"].as_str() else { return };
    let ids: Vec<u64> = w["reload"].as_array().into_iter().flatten().filter_map(Value::as_u64).collect();
    watch(app, Path::new(path), &ids);
    let tool = w["app"].as_str().unwrap_or_default().to_string();
    let args: Vec<String> = w["args"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect();
    let label = w["preset"].as_str().filter(|n| !n.is_empty()).unwrap_or(tool.as_str()).to_string();
    #[cfg(not(target_arch = "wasm32"))]
    {
        if let Ok(Some(c)) = serde_json::from_value::<Option<lcc_engine::cmd::edit_in::Control>>(w["control"].clone()) {
            start_control(c, PathBuf::from(path));
            return;
        }
        // (a host that can't open files, e.g. tests and headless runs, starts no programs either)
        if app.services.open_with.is_some() && !tool.is_empty() && (editor::is_photocraft(&tool) || args.iter().any(|a| a == "--wait")) {
            start_editor(app, tool, args, label, PathBuf::from(path));
            return;
        }
    }
    let _ = (&args, &label);
    let editor = if tool.is_empty() { app.ui.settings.external_editor.clone() } else { tool };
    if let Some(f) = app.services.open_with.as_mut()
        && let Err(e) = f(path, &editor)
    {
        app.ui.status = format!("{}: {e}", tr("Couldn't open the editor"));
    }
}

/// What the editor threads report.
#[derive(Debug)]
enum Msg {
    Event(EditorEvent),
    /// The editor process exited.
    Exited(PathBuf),
    /// A control channel without events (an older PhotoCraft): polling stays the way back.
    NoEvents(u16, String),
    Error(String),
}

/// Files out for editing and the editor threads' mailbox.
struct RoundTrip {
    watched: Watched,
    tx: std::sync::mpsc::Sender<Msg>,
    rx: std::sync::mpsc::Receiver<Msg>,
    /// Control ports with a live event subscription.
    subscribed: std::collections::HashSet<u16>,
    last_poll: f64,
}

impl Default for RoundTrip {
    fn default() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        RoundTrip { watched: Watched::default(), tx, rx, subscribed: Default::default(), last_poll: 0.0 }
    }
}

thread_local! {
    static ROUND_TRIP: RefCell<RoundTrip> = RefCell::new(RoundTrip::default());
}

/// Track `path` (out for editing) for the photos `ids`: they reload when it changes.
pub(crate) fn watch(app: &mut LccApp, path: &Path, ids: &[u64]) {
    for id in ids {
        if !app.ui.external_edits.contains(id) {
            app.ui.external_edits.push(*id);
        }
    }
    ROUND_TRIP.with_borrow_mut(|rt| rt.watched.add(path, ids));
}

/// How many files are out for editing (tests).
#[cfg(test)]
fn watched_files() -> usize {
    ROUND_TRIP.with_borrow(|rt| rt.watched.len())
}

/// Open `path` in PhotoCraft over its control channel and follow its events (one subscription per
/// port), on a worker thread.
#[cfg(not(target_arch = "wasm32"))]
fn start_control(c: lcc_engine::cmd::edit_in::Control, path: PathBuf) {
    let (tx, subscribe) = ROUND_TRIP.with_borrow_mut(|rt| (rt.tx.clone(), rt.subscribed.insert(c.port)));
    let spawned = std::thread::Builder::new().name("edit-in-control".into()).spawn(move || {
        if let Err(e) = lcc_engine::cmd::edit_in::control_open(&c, &path) {
            let _ = tx.send(Msg::Error(format!("PhotoCraft: {e}")));
        }
        if !subscribe {
            return;
        }
        let stop = std::sync::atomic::AtomicBool::new(false);
        let r = editor::subscribe(&c, &stop, |ev| tx.send(Msg::Event(ev)).is_ok());
        let why = r.err().unwrap_or_else(|| "the connection closed".into());
        let _ = tx.send(Msg::NoEvents(c.port, why));
    });
    if let Err(e) = spawned {
        log::warn!("edit in: can't start a thread: {e}");
    }
}

/// Start the editor on a worker thread: with `--wait` when it takes it (the activity stack shows
/// the edit until it exits, then the photo reloads), else just started.
#[cfg(not(target_arch = "wasm32"))]
fn start_editor(app: &mut LccApp, tool: String, args: Vec<String>, label: String, path: PathBuf) {
    let tx = ROUND_TRIP.with_borrow(|rt| rt.tx.clone());
    let activity = app.session.activity.clone();
    let desktop = app.session.desktop.clone();
    let spawned = std::thread::Builder::new().name("edit-in-editor".into()).spawn(move || {
        let caps = editor::capabilities(&tool, &args);
        if !caps.wait {
            let line = lcc_engine::cmd::edit_in::command_line(&tool, &args, &path);
            if let Err(e) = desktop.launch(&line) {
                let _ = tx.send(Msg::Error(e));
            }
            return;
        }
        let line = editor::wait_command_line(&tool, &args, &path, caps);
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let task = activity.start("editIn", &format!("Editing {name} in {label}…"), lcc_engine::activity::Cancel::No);
        let r = desktop.run_and_wait(&line);
        drop(task);
        match r {
            Ok(_) => {
                let _ = tx.send(Msg::Exited(path));
            }
            Err(e) => {
                let _ = tx.send(Msg::Error(e));
            }
        }
    });
    if let Err(e) = spawned {
        app.ui.status = format!("{}: {e}", tr("Couldn't open the editor"));
    }
}

fn reload(app: &mut LccApp, ctx: &egui::Context, ids: Vec<u64>) {
    if ids.is_empty() {
        return;
    }
    match app.session.execute("photo.reload", &json!({"ids": ids})) {
        Ok(r) if r["reloaded"].as_array().is_some_and(|a| !a.is_empty()) => {
            app.toast(ctx, tr("Updated the edits saved in the external editor"));
            // fork: Edit In versions of Immich-linked photos go to Immich (RT.C1)
            #[cfg(not(target_arch = "wasm32"))]
            crate::panels::connections::after_edit_saved(app, ctx, &ids);
        }
        Ok(_) => {}
        Err(e) => app.ui.status = e.to_string(),
    }
}

/// Every frame: the editor threads' news, and once a second the modification times of the files
/// out for editing.
pub(crate) fn pump(app: &mut LccApp, ctx: &egui::Context) {
    let now = ctx.input(|i| i.time);
    let (msgs, changed, busy) = ROUND_TRIP.with_borrow_mut(|rt| {
        let msgs: Vec<Msg> = rt.rx.try_iter().take(256).collect();
        let changed = if !rt.watched.is_empty() && (now - rt.last_poll >= 1.0 || now < rt.last_poll) {
            rt.last_poll = now;
            rt.watched.changed()
        } else {
            Vec::new()
        };
        (msgs, changed, !rt.watched.is_empty())
    });
    for m in msgs {
        match m {
            Msg::Event(EditorEvent::Saved(p)) | Msg::Exited(p) => {
                let ids = ROUND_TRIP.with_borrow_mut(|rt| {
                    let ids = rt.watched.ids(&p);
                    rt.watched.mark_seen(&p);
                    ids
                });
                reload(app, ctx, ids);
            }
            Msg::Event(EditorEvent::Closed(p)) => ROUND_TRIP.with_borrow_mut(|rt| rt.watched.remove(&p)),
            Msg::NoEvents(port, why) => {
                log::info!("edit in: no editor events on port {port} ({why}); polling for saves instead");
                ROUND_TRIP.with_borrow_mut(|rt| rt.subscribed.remove(&port));
            }
            Msg::Error(e) => app.ui.status = format!("{}: {e}", tr("Couldn't open the editor")),
        }
    }
    reload(app, ctx, changed);
    if busy {
        ctx.request_repaint_after(std::time::Duration::from_secs(1));
    }
}

/// Every frame: the windows that are open, and the saved action shortcuts.
pub(crate) fn show(app: &mut LccApp, ctx: &egui::Context) {
    play_shortcuts(app, ctx);
    pump(app, ctx);
    let mut st = STATE.with_borrow_mut(std::mem::take);
    if st.ask.is_some() {
        ask_window(app, ctx, &mut st);
    }
    if st.presets_open {
        presets_window(app, ctx, &mut st);
    }
    if st.actions_open {
        actions_window(app, ctx, &mut st);
    }
    STATE.with_borrow_mut(|s| *s = st);
}

fn outcome(st: &mut WorkflowUi, r: Result<Value, String>, ok: &str) -> Option<Value> {
    match r {
        Ok(v) => {
            st.message = tr(ok).to_string();
            Some(v)
        }
        Err(e) => {
            st.message = e;
            None
        }
    }
}

fn combo(ui: &mut egui::Ui, id: &str, label: &str, value: &mut String, choices: &[(&str, &str)]) {
    ui.label(tr(label));
    let shown = choices.iter().find(|(v, _)| v == value).map(|(_, l)| tr(l)).unwrap_or(value.as_str()).to_string();
    egui::ComboBox::from_id_salt(id).selected_text(shown).show_ui(ui, |ui| {
        for (v, l) in choices {
            ui.selectable_value(value, v.to_string(), tr(l));
        }
    });
    ui.end_row();
}

fn presets_window(app: &mut LccApp, ctx: &egui::Context, st: &mut WorkflowUi) {
    let list = app.session.execute("editIn.presets", &json!({})).ok();
    let mut open = true;
    egui::Window::new(tr("Edit In Presets")).id(egui::Id::new("edit-in-presets")).open(&mut open).default_width(420.0).collapsible(false).show(
        ctx,
        |ui| {
            ui.horizontal_wrapped(|ui| {
                for (kind, builtin) in [("builtin", true), ("user", false)] {
                    for p in list.as_ref().and_then(|l| l[kind].as_array()).into_iter().flatten() {
                        let name = p["name"].as_str().unwrap_or_default();
                        let label = if builtin { format!("{name} ({})", tr("built-in")) } else { name.to_string() };
                        if ui.selectable_label(st.draft.name == name, label).clicked() {
                            st.draft = Draft::from_preset(p);
                            if builtin {
                                // a built-in is a starting point: saving makes a copy
                                st.draft.name = format!("{name} ({})", tr("copy"));
                            }
                        }
                    }
                }
            });
            ui.separator();
            egui::Grid::new("edit-in-grid").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                ui.label(tr("Name"));
                let r = ui.text_edit_singleline(&mut st.draft.name);
                crate::access::label(&r, "Name");
                ui.end_row();
                ui.label(tr("Application"));
                let r = ui.add(egui::TextEdit::singleline(&mut st.draft.app).hint_text(tr("System default")));
                crate::access::label(&r, "System default");
                ui.end_row();
                ui.label(tr("Arguments"));
                let r = ui.add(egui::TextEdit::singleline(&mut st.draft.args).hint_text("{file}"));
                crate::access::label(&r, "Arguments");
                ui.end_row();
                combo(ui, "ei-mode", "Mode", &mut st.draft.mode, MODES);
                let formats: Vec<(&str, &str)> = FORMATS.iter().map(|f| (*f, if *f == "psd" { "PSD (layered)" } else { "TIFF" })).collect();
                combo(ui, "ei-format", "File Format", &mut st.draft.format, &formats);
                let spaces: Vec<(&str, &str)> = SPACES
                    .iter()
                    .map(|c| {
                        (
                            *c,
                            match *c {
                                "adobeRgb" => "Adobe RGB (1998)",
                                "proPhoto" => "ProPhoto RGB",
                                "displayP3" => "Display P3",
                                _ => "sRGB",
                            },
                        )
                    })
                    .collect();
                combo(ui, "ei-space", "Color Space", &mut st.draft.color_space, &spaces);
                ui.label(tr("Bit Depth"));
                ui.horizontal(|ui| {
                    ui.radio_value(&mut st.draft.bit_depth, 8, tr("8 bits/component"));
                    ui.radio_value(&mut st.draft.bit_depth, 16, tr("16 bits/component"));
                });
                ui.end_row();
                ui.label(tr("File Name"));
                let r = ui.text_edit_singleline(&mut st.draft.naming);
                crate::access::label(&r, "File Name");
                ui.end_row();
                ui.label("");
                ui.checkbox(&mut st.draft.stack, tr("Stack With Original"));
                ui.end_row();
            });
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button(tr("Save Preset")).clicked() {
                    let r = app.session.execute("editIn.savePreset", &st.draft.to_params()).map_err(|e| e.to_string());
                    outcome(st, r, "Preset saved");
                }
                if ui.button(tr("Delete Preset")).clicked() {
                    let r = app.session.execute("editIn.deletePreset", &json!({"name": st.draft.name.trim()})).map_err(|e| e.to_string());
                    outcome(st, r, "Preset deleted");
                }
                if ui.button(tr("Edit In")).clicked() {
                    let mut p = st.draft.to_params();
                    if let Some(o) = p.as_object_mut() {
                        o.remove("name");
                    }
                    let r = app.run("photo.editIn", p);
                    outcome(st, r, "Opened for editing; it is stacked with the original");
                }
            });
            if !st.message.is_empty() {
                ui.label(egui::RichText::new(&st.message).small());
            }
        },
    );
    st.presets_open = open;
}

/// Lightroom's "Edit Photo" question for a PSD or TIFF.
fn ask_window(app: &mut LccApp, ctx: &egui::Context, st: &mut WorkflowUi) {
    let Some((params, mut mode)) = st.ask.take() else { return };
    let mut open = true;
    let mut go = false;
    let mut cancel = false;
    egui::Window::new(tr("Edit Photo")).id(egui::Id::new("edit-in-ask")).open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
        for (v, l) in [("original", "Edit Original"), ("copy", "Edit a Copy"), ("copyWithAdjustments", "Edit a Copy with Adjustments")] {
            ui.radio_value(&mut mode, v.to_string(), tr(l));
        }
        ui.label(egui::RichText::new(tr("Edit Original opens the file itself; saving it updates this photo.")).small());
        ui.separator();
        ui.horizontal(|ui| {
            go = ui.button(tr("Edit")).clicked();
            cancel = ui.button(tr("Cancel")).clicked();
        });
    });
    if go {
        let mut p = params;
        if let Some(o) = p.as_object_mut() {
            o.insert("mode".into(), json!(mode));
        }
        if let Err(e) = app.run("photo.editIn", p) {
            app.ui.status = e;
        }
    } else if open && !cancel {
        st.ask = Some((params, mode));
    }
}

fn actions_window(app: &mut LccApp, ctx: &egui::Context, st: &mut WorkflowUi) {
    let list = app.session.execute("actions.list", &json!({})).map_err(|e| e.to_string());
    let mut open = true;
    egui::Window::new(tr("Actions")).id(egui::Id::new("actions-panel")).open(&mut open).default_width(360.0).show(ctx, |ui| {
        let (actions, recording) = match &list {
            Ok(l) => (l["actions"].as_array().cloned().unwrap_or_default(), l["recording"].as_str().map(str::to_string)),
            Err(e) => {
                ui.colored_label(ui.visuals().error_fg_color, e);
                (Vec::new(), None)
            }
        };
        egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
            if actions.is_empty() {
                ui.label(tr("No actions yet: record one below."));
            }
            for a in &actions {
                let name = a["name"].as_str().unwrap_or_default().to_string();
                ui.horizontal(|ui| {
                    let sc = a["shortcut"].as_str().map(|s| format!("  {s}")).unwrap_or_default();
                    let text = format!("{name}  ·  {} {}{sc}", a["steps"].as_u64().unwrap_or(0), tr("steps"));
                    if ui.selectable_label(st.selected.as_deref() == Some(&name), text).clicked() {
                        st.shortcut = a["shortcut"].as_str().unwrap_or_default().to_string();
                        st.selected = Some(name.clone());
                    }
                    if ui.small_button(tr("Play")).on_hover_text(tr("Play on the selected photos")).clicked() {
                        let r = app.run("actions.play", json!({"name": name}));
                        outcome(st, r, "Action played");
                    }
                });
            }
        });
        if let Some(sel) = st.selected.clone() {
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(tr("Shortcut"));
                let r = ui.add(egui::TextEdit::singleline(&mut st.shortcut).hint_text("Cmd+Alt+1").desired_width(110.0));
                crate::access::label(&r, "Shortcut");
                if ui.button(tr("Set")).clicked() {
                    let sc = st.shortcut.trim().to_string();
                    let r = if !sc.is_empty() && crate::shortcuts::parse(&sc).is_none() {
                        Err(format!("{}: {sc}", tr("Not a shortcut")))
                    } else {
                        app.session.execute("actions.save", &json!({"name": sel, "shortcut": sc})).map_err(|e| e.to_string())
                    };
                    outcome(st, r, "Shortcut saved");
                }
                if ui.button(tr("Delete")).clicked() {
                    let r = app.session.execute("actions.delete", &json!({"name": sel})).map_err(|e| e.to_string());
                    if outcome(st, r, "Action deleted").is_some() {
                        st.selected = None;
                    }
                }
            });
        }
        ui.separator();
        match recording {
            Some(name) => {
                ui.label(format!("● {} {name}", tr("Recording")));
                ui.horizontal(|ui| {
                    if ui.button(tr("Stop Recording")).clicked() {
                        let r = app.run("actions.stop", json!({}));
                        outcome(st, r, "Action saved");
                    }
                    if ui.button(tr("Cancel")).clicked() {
                        let r = app.run("actions.cancel", json!({}));
                        outcome(st, r, "Recording cancelled");
                    }
                });
            }
            None => {
                ui.horizontal(|ui| {
                    let r = ui.add(egui::TextEdit::singleline(&mut st.record_name).hint_text(tr("Action name")).desired_width(180.0));
                    crate::access::label(&r, "Action name");
                    if ui.button(tr("Record")).clicked() {
                        let r = app.run("actions.record", json!({"name": st.record_name.trim()}));
                        outcome(st, r, "Recording: edit a photo, then stop");
                    }
                });
            }
        }
        if !st.message.is_empty() {
            ui.label(egui::RichText::new(&st.message).small());
        }
    });
    st.actions_open = open;
}

/// The actions whose saved shortcut was pressed this frame (`(shortcut, action)` pairs).
fn play_shortcuts(app: &mut LccApp, ctx: &egui::Context) {
    if ctx.egui_wants_keyboard_input() {
        return;
    }
    let Ok(l) = app.session.execute("actions.list", &json!({})) else { return };
    let mut hit = Vec::new();
    for a in l["actions"].as_array().into_iter().flatten() {
        let (Some(sc), Some(name)) = (a["shortcut"].as_str(), a["name"].as_str()) else { continue };
        let Some((mods, key)) = crate::shortcuts::parse(sc) else { continue };
        if ctx.input_mut(|i| i.consume_key(mods, key)) {
            hit.push(name.to_string());
        }
    }
    for name in hit {
        if let Err(e) = app.run("actions.play", json!({"name": name})) {
            app.ui.status = e;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_round_trips_a_preset() {
        let p = json!({"name": "G", "app": "/usr/bin/gimp", "args": ["-n", "{file}"], "format": "psd", "colorSpace": "srgb", "bitDepth": 8, "mode": "copy", "naming": "{name}-G", "stack": false});
        let d = Draft::from_preset(&p);
        assert_eq!(d.args, "-n {file}");
        assert_eq!(d.to_params(), p);
    }

    #[test]
    fn commands_open_the_windows_and_save_a_preset() {
        let app = LccApp::new(lcc_engine::Session::with_demo(), crate::Services::default());
        let mut h = crate::headless::Headless::new(app, [1000.0, 700.0], 1.0);
        let r = h.request("engine.execute", json!({"command": PRESETS_DIALOG, "params": {}}), std::time::Duration::from_secs(20));
        assert_eq!(r["ok"], true, "{r}");
        let r = h.request("engine.execute", json!({"command": ACTIONS_PANEL, "params": {}}), std::time::Duration::from_secs(20));
        assert_eq!(r["ok"], true, "{r}");
        h.settle(std::time::Duration::from_secs(5));
        let s = state();
        assert!(s.presets_open && s.actions_open);
        // an action with a shortcut plays from the keyboard
        h.app.session.workflow = lcc_engine::cmd::actions::Workflow::in_dir(None);
        h.app
            .session
            .execute(
                "actions.save",
                &json!({"action": {"name": "Five", "shortcut": "Alt+F5", "steps": [{"command": "photo.rate", "params": {"rating": 5}}]}}),
            )
            .unwrap();
        let id = h.app.session.active().unwrap();
        let r = h.request("ui.key", json!({"key": "F5", "alt": true}), std::time::Duration::from_secs(20));
        assert_eq!(r["ok"], true, "{r}");
        h.settle(std::time::Duration::from_secs(5));
        assert_eq!(h.app.session.catalog.photo(id).unwrap().rating, 5);
    }

    fn png(path: &std::path::Path, w: usize, h: usize, seed: u8) {
        let data: Vec<[u8; 4]> = (0..w * h).map(|i| [(i % w * 6) as u8, seed, 90, 255]).collect();
        let img = lcc_raster::Rgba8 { width: w, height: h, data };
        std::fs::write(path, lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &Default::default()).unwrap()).unwrap();
    }

    fn temp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("ui-editrt-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn headless(dir: &std::path::Path) -> crate::headless::Headless {
        let mut s = lcc_engine::Session::new().with_fs();
        s.workflow = lcc_engine::cmd::actions::Workflow::in_dir(Some(dir.join("settings")));
        crate::headless::Headless::new(LccApp::new(s, crate::Services::default()), [900.0, 600.0], 1.0)
    }

    /// RT.B1: Edit In on a PSD from the menu asks first (Edit Original preselected); the choice runs
    /// the command; scripted calls with a mode never ask.
    #[test]
    fn edit_in_on_a_psd_asks_for_the_mode() {
        let dir = temp("ask");
        let src = dir.join("a.png");
        png(&src, 20, 12, 1);
        let mut h = headless(&dir);
        h.app.session.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
        let r = h.app.run("photo.editIn", json!({"preset": "PhotoCraft"})).unwrap();
        assert!(r["path"].as_str().unwrap().ends_with("a-Edit.psd"), "a PNG doesn't ask: {r}");
        let psd = lcc_catalog::PhotoId(r["id"].as_u64().unwrap());
        assert_eq!(watched_files(), 1, "the PSD is out for editing");
        let n = h.app.session.catalog.photos().count();
        let r = h.app.run("photo.editIn", json!({})).unwrap();
        assert_eq!(r["asked"], true, "{r}");
        assert_eq!(state().ask.as_ref().map(|(p, m)| (p["id"].as_u64(), m.as_str())), Some((Some(psd.0), "original")));
        h.settle(std::time::Duration::from_secs(5));
        assert!(h.painted_text().iter().any(|t| t.contains("Edit Original")), "the question shows");
        // the user confirms Edit Original: no new photo
        let (p, _) = state().ask.unwrap();
        let mut p = p;
        p["mode"] = json!("original");
        update(|s| s.ask = None);
        let r = h.app.run("photo.editIn", p).unwrap();
        assert_eq!((r["id"].as_u64(), h.app.session.catalog.photos().count()), (Some(psd.0), n));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// RT.B2 fallback: a save to a file out for editing is picked up by the modification-time poll,
    /// without the app losing and regaining the focus.
    #[test]
    fn a_save_is_picked_up_without_focus() {
        let dir = temp("poll");
        let src = dir.join("b.png");
        png(&src, 20, 12, 1);
        let mut h = headless(&dir);
        h.app.session.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
        let id = h.app.session.active().unwrap();
        watch(&mut h.app, &src, &[id.0]);
        h.step();
        png(&src, 30, 18, 200);
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(3);
        std::fs::File::options().write(true).open(&src).unwrap().set_modified(later).unwrap();
        // (wait for the reload, bounded by time: a loaded machine polls and decodes slowly)
        h.step_until(std::time::Duration::from_secs(60), |h| h.app.session.catalog.photo(id).is_some_and(|p| p.width == 30));
        assert_eq!(h.app.session.catalog.photo(id).unwrap().width, 30, "reloaded by the poll");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// RT.B2: a preset with PhotoCraft's control channel opens the file there and reloads the photo
    /// on the channel's `document.saved` event.
    #[test]
    fn control_events_reload_the_photo() {
        use std::io::{BufRead, BufReader, Write};
        let dir = temp("events");
        let src = dir.join("c.png");
        png(&src, 20, 12, 1);
        let token = dir.join("token");
        std::fs::write(&token, "t").unwrap();
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let (go_tx, go_rx) = std::sync::mpsc::channel::<String>();
        let go_rx = std::sync::Arc::new(std::sync::Mutex::new(go_rx));
        std::thread::spawn(move || {
            for conn in l.incoming().take(2) {
                let conn = conn.unwrap();
                let go_rx = go_rx.clone();
                std::thread::spawn(move || {
                    let mut w = conn.try_clone().unwrap();
                    let mut r = BufReader::new(conn);
                    let mut line = String::new();
                    while r.read_line(&mut line).unwrap_or(0) > 0 {
                        let v: Value = serde_json::from_str(line.trim()).unwrap();
                        line.clear();
                        writeln!(w, "{}", json!({"id": v["id"], "ok": true, "result": {}})).unwrap();
                        if v["method"] == "events.subscribe" {
                            // the test says when the editor "saves"
                            let rel = go_rx.lock().unwrap().recv().unwrap();
                            writeln!(w, "{}", json!({"event": "document.saved", "params": {"path": rel, "format": "png", "docId": 1}})).unwrap();
                            std::thread::sleep(std::time::Duration::from_secs(2));
                            return;
                        }
                    }
                });
            }
        });
        let mut h = headless(&dir);
        h.app.session.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
        let id = h.app.session.active().unwrap();
        let control = json!({"port": port, "tokenFile": token.to_string_lossy(), "root": dir.to_string_lossy()});
        let r = h.app.run("photo.editIn", json!({"mode": "original", "control": control})).unwrap();
        assert_eq!(r["openWith"]["control"]["port"], port);
        // the "editor" saves a different picture, keeping the file's time (only the event tells)
        let before = std::fs::metadata(&src).unwrap().modified().unwrap();
        png(&src, 32, 16, 9);
        std::fs::File::options().write(true).open(&src).unwrap().set_modified(before).unwrap();
        go_tx.send("c.png".into()).unwrap();
        let t0 = std::time::Instant::now();
        while h.app.session.catalog.photo(id).unwrap().width != 32 && t0.elapsed() < std::time::Duration::from_secs(60) {
            h.step();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(h.app.session.catalog.photo(id).unwrap().width, 32, "reloaded on document.saved");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
