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
//! - Edit In from a menu, shortcut or the context menu asks, as Lightroom does, whether to edit a
//!   copy with the adjustments, a copy, or the original ([`ask_edit`]; Edit Original preselected
//!   on a PSD or TIFF, the copies of the file itself only offered where an editor can open it).
//!   "Don't ask again" keeps the choice in Settings ▸ External Editing (issue #616).
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
    /// Edit In waiting for the user's choice.
    pub ask: Option<Ask>,
}

/// The Edit Photo window's question.
#[derive(Clone, Debug, PartialEq)]
pub struct Ask {
    /// The command's parameters (`photo.editIn`: with `id`).
    pub params: Value,
    /// The mode picked so far.
    pub mode: String,
    /// Edit in PhotoCraft (`Some(true)`), in the other editor (`Some(false)`), or `photo.editIn`
    /// with its preset (`None`).
    pub editor: Option<bool>,
    /// An editor can open the file itself, so Edit a Copy and Edit Original are offered.
    pub file_editable: bool,
    /// How many photos the edit covers.
    pub count: usize,
    /// "Don't ask again".
    pub remember: bool,
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
    let file_editable = lcc_engine::cmd::edit_in::file_editable(&app.session, id);
    update(|s| s.ask = Some(Ask { params, mode: "original".into(), editor: None, file_editable, count: 1, remember: false }));
    Some(Ok(json!({"asked": true})))
}

/// Edit in PhotoCraft / Edit in Other Editor from a menu, shortcut or the context menu (no
/// parameters): ask which file the editor gets, as Lightroom does, or use the choice Settings
/// remembers (issue #616). `None` = run the command as it is (scripted, nothing selected, or the
/// editor missing: the command then says why).
pub(crate) fn ask_edit(app: &mut LccApp, ctx: &egui::Context, p: &Value, photocraft: bool) -> Option<Result<Value, String>> {
    if p.as_object().is_some_and(|o| !o.is_empty()) {
        return None;
    }
    let ids = app.session.targets(&json!({})).ok().filter(|ids| !ids.is_empty())?;
    let editor = editor_for(app, photocraft);
    if let Some(check) = app.services.check_editor.as_ref()
        && check(&editor).is_err()
    {
        return None;
    }
    let file_editable = ids.iter().all(|id| lcc_engine::cmd::edit_in::file_editable(&app.session, *id));
    let remembered = app.ui.settings.edit_in_mode.mode();
    if let Some(mode) = remembered {
        // a remembered Copy / Original on a raw: the copy with the adjustments is all there is
        let mode = if file_editable { mode } else { "copyWithAdjustments" };
        return Some(run_choice(app, ctx, &json!({}), Some(photocraft), mode, &ids));
    }
    let first = app.session.catalog.photo(ids[0]).map(|ph| ph.file_name.clone()).unwrap_or_default();
    let mode = if file_editable && lcc_engine::cmd::edit_in::suggested_mode(&first) == lcc_engine::cmd::edit_in::Mode::Original {
        "original"
    } else {
        "copyWithAdjustments"
    };
    let ask = Ask { params: json!({}), mode: mode.into(), editor: Some(photocraft), file_editable, count: ids.len(), remember: false };
    update(|s| s.ask = Some(ask));
    Some(Ok(json!({"asked": true, "mode": mode, "fileEditable": file_editable})))
}

/// The application Edit in PhotoCraft / Other Editor opens (Settings ▸ External Editing).
fn editor_for(app: &LccApp, photocraft: bool) -> String {
    let settings = &app.ui.settings;
    if photocraft {
        let path = settings.photocraft_path.trim();
        if path.is_empty() { crate::state::PHOTOCRAFT.to_string() } else { path.to_string() }
    } else {
        settings.external_editor.clone()
    }
}

/// Run the edit the user chose: a copy with the adjustments goes through the editor's own
/// command (its file settings); a copy or the original through `photo.editIn` for each photo.
fn run_choice(
    app: &mut LccApp,
    ctx: &egui::Context,
    params: &Value,
    editor: Option<bool>,
    mode: &str,
    ids: &[lcc_catalog::PhotoId],
) -> Result<Value, String> {
    let Some(photocraft) = editor else {
        let mut p = params.clone();
        if let Some(o) = p.as_object_mut() {
            o.insert("mode".into(), json!(mode));
        }
        return app.run("photo.editIn", p);
    };
    if mode == "copyWithAdjustments" {
        return crate::menus::edit_in(app, ctx, &json!({}), photocraft);
    }
    let tool = editor_for(app, photocraft);
    let mut results = Vec::new();
    for id in ids {
        let mut p = json!({"id": id.0, "mode": mode});
        if !tool.is_empty()
            && let Some(o) = p.as_object_mut()
        {
            o.insert("app".into(), json!(tool));
        }
        match app.run("photo.editIn", p) {
            Ok(r) => results.push(r),
            Err(e) => {
                app.toast(ctx, crate::i18n::tr_format!("Couldn't open the editor: {e}", e = e));
                return Err(e);
            }
        }
    }
    Ok(json!({"mode": mode, "edits": results}))
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
        let now = app.last_time;
        app.ui.show_toast((crate::i18n::tr_format!("Couldn't open the editor: {e}", e = e), now + 1.4, None));
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
        let now = app.last_time;
        app.ui.show_toast((crate::i18n::tr_format!("Couldn't open the editor: {e}", e = e), now + 1.4, None));
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
            Msg::Event(EditorEvent::Saved(p)) => {
                let ids = ROUND_TRIP.with_borrow_mut(|rt| {
                    let ids = rt.watched.ids(&p);
                    rt.watched.mark_seen(&p);
                    ids
                });
                reload(app, ctx, ids);
            }
            Msg::Exited(p) => {
                let ids = ROUND_TRIP.with_borrow_mut(|rt| {
                    let ids = rt.watched.ids(&p);
                    rt.watched.remove(&p);
                    ids
                });
                reload(app, ctx, ids);
            }
            Msg::Event(EditorEvent::Closed(p)) => ROUND_TRIP.with_borrow_mut(|rt| rt.watched.remove(&p)),
            Msg::NoEvents(port, why) => {
                log::info!("edit in: no editor events on port {port} ({why}); polling for saves instead");
                ROUND_TRIP.with_borrow_mut(|rt| rt.subscribed.remove(&port));
            }
            // a launch that failed on the worker (no such program, ...) is a toast, as a failed hand-off is
            Msg::Error(e) => app.toast(ctx, crate::i18n::tr_format!("Couldn't open the editor: {e}", e = e)),
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
    let open = !crate::dialog_frame::esc_closes(app.ui.dialog.is_some(), ctx);
    let mut closed = false;
    egui::Window::new(tr("Edit In Presets"))
        .id(egui::Id::new("edit-in-presets"))
        .title_bar(false)
        .frame(crate::dialog_frame::card_frame(ctx))
        .default_width(420.0)
        .collapsible(false)
        .show(ctx, |ui| {
            if crate::dialog_frame::card_header(ui, tr("Edit In Presets"), Some("edit-in-presets:close")) {
                closed = true;
            }
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
        });
    st.presets_open = open && !closed;
}

/// Lightroom's "Edit Photo" question: which file the editor gets.
fn ask_window(app: &mut LccApp, ctx: &egui::Context, st: &mut WorkflowUi) {
    let Some(mut ask) = st.ask.take() else { return };
    let mut go = false;
    let mut cancel = crate::dialog_frame::esc_closes(false, ctx);
    egui::Window::new(tr("Edit Photo"))
        .id(egui::Id::new("edit-in-ask"))
        .title_bar(false)
        .frame(crate::dialog_frame::card_frame(ctx))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .default_width(380.0)
        .show(ctx, |ui| {
            if crate::dialog_frame::card_header(ui, tr("Edit Photo"), Some("edit-in-ask:close")) {
                cancel = true;
            }
            if ask.count > 1 {
                ui.label(tr("Every selected photo is edited the same way."));
                ui.add_space(4.0);
            }
            let choices = [
                (
                    "copyWithAdjustments",
                    "Edit a Copy with Adjustments",
                    "Renders a copy of the photo with your edits applied and opens it in the editor.",
                ),
                ("copy", "Edit a Copy", "Opens a copy of the original file, without your edits."),
                ("original", "Edit Original", "Opens the original file itself; saving it updates this photo. Your edits here are not visible there."),
            ];
            for (v, label, what) in choices {
                let enabled = v == "copyWithAdjustments" || ask.file_editable;
                ui.add_enabled_ui(enabled, |ui| {
                    let r = ui.radio_value(&mut ask.mode, v.to_string(), egui::RichText::new(tr(label)).strong());
                    crate::widgets::register(ui.ctx(), format!("radio:editIn-{v}"), r.rect);
                    ui.indent(v, |ui| ui.label(egui::RichText::new(tr(what)).small()));
                });
            }
            if !ask.file_editable {
                ui.add_space(2.0);
                ui.label(egui::RichText::new(tr("Raw files and generated photos can only be edited as a copy with adjustments.")).small().weak());
            }
            if ask.editor.is_some() && ask.mode == "copyWithAdjustments" {
                ui.label(egui::RichText::new(tr("File format, colour space and bit depth: Settings → External Editing.")).small().weak());
            }
            ui.add_space(6.0);
            if ask.editor.is_some() {
                let c = crate::dialog_frame::checkbox(ui, &mut ask.remember, tr("Don't ask again"));
                crate::widgets::register(ui.ctx(), "check:editInRemember", c.rect);
                c.on_hover_text(tr("Settings → External Editing → Edit In turns the question back on"));
            }
            ui.add_space(6.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                use crate::button::{Button, Kind};
                go = Button::new("Edit").kind(Kind::Primary).widget("button:editInGo").show(ui).clicked();
                if Button::new("Cancel").widget("button:editInCancel").show(ui).clicked() {
                    cancel = true;
                }
            });
        });
    if go {
        let _ = choose(app, ctx, ask);
    } else if !cancel {
        st.ask = Some(ask);
    }
}

/// The Edit Photo window's Edit button (and `editIn.choose` in tests): run the chosen edit, and
/// keep the choice when "Don't ask again" is ticked.
fn choose(app: &mut LccApp, ctx: &egui::Context, ask: Ask) -> Result<Value, String> {
    let mode = if ask.file_editable { ask.mode.as_str() } else { "copyWithAdjustments" };
    if ask.remember && ask.editor.is_some() {
        app.ui.settings.edit_in_mode = crate::state::EditInMode::from_mode(mode);
    }
    let ids = match ask.params.get("id").and_then(Value::as_u64) {
        Some(id) => vec![lcc_catalog::PhotoId(id)],
        None => app.session.targets(&json!({})).unwrap_or_default(),
    };
    let r = run_choice(app, ctx, &ask.params, ask.editor, mode, &ids);
    if let Err(e) = &r {
        app.ui.status = e.clone();
    }
    r
}

fn actions_window(app: &mut LccApp, ctx: &egui::Context, st: &mut WorkflowUi) {
    let list = app.session.execute("actions.list", &json!({})).map_err(|e| e.to_string());
    let open = !crate::dialog_frame::esc_closes(app.ui.dialog.is_some(), ctx);
    let mut closed = false;
    egui::Window::new(tr("Actions"))
        .id(egui::Id::new("actions-panel"))
        .title_bar(false)
        .frame(crate::dialog_frame::card_frame(ctx))
        .default_width(360.0)
        .show(ctx, |ui| {
            if crate::dialog_frame::card_header(ui, tr("Actions"), Some("actions-panel:close")) {
                closed = true;
            }
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
    st.actions_open = open && !closed;
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

    fn temp(tag: &str) -> lcc_testkit::TempDir {
        let d = lcc_testkit::TempDir::new(&format!("ui-editrt-{tag}"));
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
        assert_eq!(state().ask.as_ref().map(|a| (a.params["id"].as_u64(), a.mode.as_str())), Some((Some(psd.0), "original")));
        // (settle alone can run out of time on a busy runner before the window's first visible frame)
        let shown = h.step_until(std::time::Duration::from_secs(60), |h| h.painted_text().iter().any(|t| t.contains("Edit Original")));
        assert!(shown, "the question shows");
        // the user confirms Edit Original: no new photo
        let mut p = state().ask.unwrap().params;
        p["mode"] = json!("original");
        update(|s| s.ask = None);
        let r = h.app.run("photo.editIn", p).unwrap();
        assert_eq!((r["id"].as_u64(), h.app.session.catalog.photos().count()), (Some(psd.0), n));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #616: Edit in Other Editor from the menu asks which file to edit (a copy with the
    /// adjustments preselected on a PNG); Edit a Copy copies the file itself without rendering;
    /// "Don't ask again" keeps the choice, so the next Edit In doesn't ask. A raw-like file the
    /// editor can't open only gets the copy with the adjustments, whatever was remembered.
    #[test]
    fn edit_in_asks_which_file_and_remembers_the_choice() {
        let dir = temp("choice");
        let src = dir.join("d.png");
        png(&src, 20, 12, 1);
        let opened = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let o = opened.clone();
        let mut s = lcc_engine::Session::new().with_fs();
        s.workflow = lcc_engine::cmd::actions::Workflow::in_dir(Some(dir.join("settings")));
        let services = crate::Services {
            open_with: Some(Box::new(move |path: &str, _app: &str| {
                o.lock().unwrap().push(path.to_string());
                Ok(())
            })),
            ..Default::default()
        };
        let mut h = crate::headless::Headless::new(LccApp::new(s, services), [900.0, 600.0], 1.0);
        h.app.session.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
        let n = h.app.session.catalog.photos().count();
        let r = h.app.run("photo.editInExternal", json!({})).unwrap();
        assert_eq!(
            (r["asked"].as_bool(), r["mode"].as_str(), r["fileEditable"].as_bool()),
            (Some(true), Some("copyWithAdjustments"), Some(true)),
            "{r}"
        );
        assert_eq!(h.app.session.catalog.photos().count(), n, "nothing is made before the choice");
        let shown = h.step_until(std::time::Duration::from_secs(60), |h| h.painted_text().iter().any(|t| t.contains("Edit a Copy")));
        assert!(shown, "the question shows");
        let mut ask = state().ask.unwrap();
        update(|s| s.ask = None);
        ask.mode = "copy".into();
        ask.remember = true;
        let r = choose(&mut h.app, &egui::Context::default(), ask).unwrap();
        assert_eq!(r["mode"], "copy", "{r}");
        let copy = r["edits"][0]["path"].as_str().unwrap().to_string();
        assert!(copy.ends_with(".png") && copy != src.to_string_lossy(), "a copy of the file itself: {copy}");
        assert_eq!(h.app.ui.settings.edit_in_mode, crate::state::EditInMode::Copy);
        assert_eq!(h.app.session.catalog.photos().count(), n + 1);
        // remembered: no question, another copy at once
        h.app.session.execute("photo.select", &json!({"ids": [r["edits"][0]["original"].clone()]})).ok();
        let r = h.app.run("photo.editInExternal", json!({})).unwrap();
        assert!(r["asked"].is_null() && r["mode"] == "copy", "{r}");
        assert!(state().ask.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #616: a photo whose file no editor opens (a generated demo photo, like a raw) only offers
    /// the copy with the adjustments, and a remembered Copy falls back to it.
    #[test]
    fn a_file_the_editor_cant_open_only_gets_the_rendered_copy() {
        let services = crate::Services { open_with: Some(Box::new(|_: &str, _: &str| Ok(()))), ..Default::default() };
        let mut h = crate::headless::Headless::new(LccApp::new(lcc_engine::Session::with_demo().with_fs(), services), [900.0, 600.0], 1.0);
        let r = h.app.run("photo.editInExternal", json!({})).unwrap();
        assert_eq!((r["mode"].as_str(), r["fileEditable"].as_bool()), (Some("copyWithAdjustments"), Some(false)), "{r}");
        let mut ask = state().ask.unwrap();
        update(|s| s.ask = None);
        ask.mode = "original".into();
        let id = h.app.session.active().unwrap();
        // the choice can't reach the file itself: the rendered copy is asked for (a demo photo
        // has no folder, so it fails saying so, without touching anything)
        let r = choose(&mut h.app, &egui::Context::default(), ask);
        assert!(r.as_ref().is_err_and(|e| e.contains("dir")), "{r:?}");
        assert_eq!(h.app.session.active(), Some(id));
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
    /// RT.B3 (P2 #1): when the host's open_with fails, an error toast is shown instead of
    /// silently succeeding (the toast appears in app.ui.toast, not just app.ui.status).
    #[test]
    fn open_failure_shows_an_error_toast() {
        let dir = temp("fail");
        let src = dir.join("fail.png");
        png(&src, 20, 12, 1);
        let failed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let f = failed.clone();
        let services = crate::Services {
            open_with: Some(Box::new(
                move |_path: &str, _app: &str| {
                    if f.load(std::sync::atomic::Ordering::SeqCst) { Err("no such editor".into()) } else { Ok(()) }
                },
            )),
            ..Default::default()
        };
        let mut h = crate::headless::Headless::new(LccApp::new(lcc_engine::Session::with_demo().with_fs(), services), [900.0, 600.0], 1.0);
        // Simulate after_command being called with a successful editIn result
        let result = json!({
            "path": "/tmp/fake-edit.tif",
            "id": 1,
            "original": 1,
            "opened": null,
            "openWith": {
                "path": "/tmp/fake-edit.tif",
                "app": "nonexistent-editor",
                "args": [],
                "reload": [1],
                "preset": "",
                "control": null
            }
        });
        // First try succeeds (no error toast)
        after_command(&mut h.app, "photo.editIn", &result);
        assert!(h.app.ui.toast().is_none(), "no error toast on success: {:?}", h.app.ui.toasts);
        // Second try fails
        failed.store(true, std::sync::atomic::Ordering::SeqCst);
        after_command(&mut h.app, "photo.editIn", &result);
        let toast = h.app.ui.toast().map(|t| t.0.clone()).unwrap_or_default();
        assert!(toast.contains("Couldn't open the editor") && toast.contains("no such editor"), "{toast:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
    /// A launch that fails on the editor's worker thread is reported as a toast, not only in the status line.
    #[test]
    fn worker_launch_failure_shows_an_error_toast() {
        let dir = temp("worker-fail");
        let mut h = headless(&dir);
        let tx = ROUND_TRIP.with_borrow(|rt| rt.tx.clone());
        tx.send(Msg::Error("no such editor".into())).unwrap();
        h.step();
        let toast = h.app.ui.toast().map(|t| t.0.clone()).unwrap_or_default();
        assert!(toast.contains("Couldn't open the editor") && toast.contains("no such editor"), "{toast:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
    /// RT.B4 (P2 #3): when an editor process exits, the watched entry is removed from the table
    /// so the 256-entry limit isn't reached by completed edits.
    #[test]
    fn editor_exit_removes_the_watched_entry() {
        let dir = temp("exit-clean");
        let src = dir.join("clean.png");
        png(&src, 20, 12, 1);
        let mut h = headless(&dir);
        h.app.session.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
        let id = h.app.session.active().unwrap();
        watch(&mut h.app, &src, &[id.0]);
        assert_eq!(watched_files(), 1, "the file is out for editing");
        // Simulate the editor exiting
        ROUND_TRIP.with_borrow(|rt| {
            assert_eq!(rt.watched.len(), 1);
        });
        // Send an Exited message via the channel
        let tx = ROUND_TRIP.with_borrow(|rt| rt.tx.clone());
        tx.send(Msg::Exited(src.clone())).unwrap();
        // Pump to process the message
        h.step();
        assert_eq!(watched_files(), 0, "the exited file is removed from the watch table");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Over the control channel (an agent, a script) the program Edit In starts comes only from
    /// what the user set up: a request that chooses one is refused and starts nothing, and so is
    /// `ui.set` changing the editors in Settings. The app's own dialogs and Settings still choose.
    #[test]
    fn the_control_channel_cannot_choose_the_program() {
        let dir = temp("agent");
        let src = dir.join("pic.png");
        png(&src, 20, 12, 4);
        let opened = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
        let o = opened.clone();
        let services = crate::Services {
            open_with: Some(Box::new(move |path: &str, app: &str| {
                o.lock().unwrap().push((path.to_string(), app.to_string()));
                Ok(())
            })),
            ..Default::default()
        };
        let mut s = lcc_engine::Session::new().with_fs();
        s.workflow = lcc_engine::cmd::actions::Workflow::in_dir(Some(dir.join("settings")));
        let mut h = crate::headless::Headless::new(LccApp::new(s, services), [900.0, 600.0], 1.0);
        h.app.session.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
        h.app.ui.settings.external_editor = "/usr/bin/gimp".into(); // the user's own, from Settings
        let t = std::time::Duration::from_secs(30);
        let files = |dir: &std::path::Path| std::fs::read_dir(dir).unwrap().count();
        let before = files(&dir);
        for (command, params) in [
            ("photo.editIn", json!({"mode": "copy", "app": "/bin/sh", "args": ["-c", "x"]})),
            ("photo.openAsLayers", json!({"control": {"port": 9, "tokenFile": "/t", "root": "/"}})),
            ("photo.editInExternal", json!({"app": "/bin/sh"})),
            ("photo.editInPhotoCraft", json!({"app": "/bin/sh"})),
            ("photo.openOriginalInExternal", json!({"app": "/bin/sh"})),
        ] {
            let r = h.request("engine.execute", json!({"command": command, "params": params}), t);
            assert_eq!(r["ok"], false, "{command} is refused: {r}");
            assert!(r["error"].as_str().unwrap_or_default().contains("preset"), "{r}");
        }
        assert!(opened.lock().unwrap().is_empty(), "nothing started: {:?}", opened.lock().unwrap());
        assert_eq!(files(&dir), before, "no copy written");

        // nor can it change the editors Settings starts; other settings it still can
        for key in ["externalEditor", "photocraftPath"] {
            let r = h.request("ui.set", json!({"settings": {key: "/bin/sh"}}), t);
            assert_eq!(r["ok"], false, "{key} is refused: {r}");
        }
        assert_eq!((h.app.ui.settings.external_editor.as_str(), h.app.ui.settings.photocraft_path.as_str()), ("/usr/bin/gimp", ""));
        let r = h.request("ui.set", json!({"settings": {"externalEditor": "/usr/bin/gimp", "askBeforeDelete": false}}), t);
        assert_eq!(r["ok"], true, "{r}");
        assert!(!h.app.ui.settings.confirm_delete);

        // without a program, Edit In asks which file (#616), then the editor from Settings opens
        let r = h.request("engine.execute", json!({"command": "photo.editInExternal", "params": {}}), t);
        assert_eq!(r["ok"], true, "{r}");
        let ask = state().ask.expect("the question is asked");
        update(|s| s.ask = None);
        assert_eq!(ask.mode, "copyWithAdjustments");
        choose(&mut h.app, &egui::Context::default(), ask).unwrap();
        assert!(matches!(opened.lock().unwrap().as_slice(), [(_, app)] if app == "/usr/bin/gimp"), "{:?}", opened.lock().unwrap());

        // a preset the user saved opens its program by name
        h.app.session.execute("editIn.savePreset", &json!({"name": "Mine", "app": "/opt/mine"})).unwrap();
        opened.lock().unwrap().clear();
        let r = h.request("engine.execute", json!({"command": "photo.editIn", "params": {"preset": "Mine", "mode": "copy"}}), t);
        assert_eq!(r["ok"], true, "{r}");
        assert!(matches!(opened.lock().unwrap().as_slice(), [(_, app)] if app == "/opt/mine"), "{:?}", opened.lock().unwrap());

        // the Edit In Presets dialog runs an unsaved draft through `app.run`, which still chooses
        opened.lock().unwrap().clear();
        h.app.run("photo.editIn", json!({"mode": "copy", "app": "/opt/ed"})).unwrap();
        assert!(matches!(opened.lock().unwrap().as_slice(), [(_, app)] if app == "/opt/ed"), "{:?}", opened.lock().unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
