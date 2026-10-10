//! Edit In and Actions, the desktop half (P4.4 / P4.5).
//!
//! - After `photo.editIn` / `photo.openAsLayers` made a file, open it in the preset's application
//!   (the host's `open_with`) and reload the photo when the app regains focus, as Edit in External
//!   Editor does ([`after_command`]).
//! - Photo ▸ Edit In ▸ Edit In Presets… (`dialog.editInPresets`): a preset editor over the
//!   `editIn.*` commands.
//! - Window ▸ Actions (`panel.actions`): record / stop / play / delete actions and set their
//!   shortcuts, over the `actions.*` commands.
//! - Saved action shortcuts play their action ([`show`] checks them every frame while no text
//!   field has the keyboard).
//!
//! The windows' state is per UI thread (the app has one), so no shared app state is touched.

use std::cell::RefCell;

use serde_json::{Value, json};

use crate::DacApp;
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
pub(crate) fn intercept(_app: &mut DacApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
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
        _ => None,
    }
}

/// Called with every engine command's successful result.
pub(crate) fn after_command(app: &mut DacApp, id: &str, r: &Value) {
    if !matches!(id, "photo.editIn" | "photo.openAsLayers") || !r["opened"].is_null() {
        return;
    }
    let w = &r["openWith"];
    let Some(path) = w["path"].as_str() else { return };
    for id in w["reload"].as_array().into_iter().flatten().filter_map(Value::as_u64) {
        if !app.ui.external_edits.contains(&id) {
            app.ui.external_edits.push(id);
        }
    }
    let editor = w["app"].as_str().filter(|a| !a.is_empty()).map(str::to_string).unwrap_or_else(|| app.ui.settings.external_editor.clone());
    if let Some(f) = app.services.open_with.as_mut()
        && let Err(e) = f(path, &editor)
    {
        app.ui.status = format!("{}: {e}", tr("Couldn't open the editor"));
    }
}

/// Every frame: the windows that are open, and the saved action shortcuts.
pub(crate) fn show(app: &mut DacApp, ctx: &egui::Context) {
    play_shortcuts(app, ctx);
    let mut st = STATE.with_borrow_mut(std::mem::take);
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

fn presets_window(app: &mut DacApp, ctx: &egui::Context, st: &mut WorkflowUi) {
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

fn actions_window(app: &mut DacApp, ctx: &egui::Context, st: &mut WorkflowUi) {
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
fn play_shortcuts(app: &mut DacApp, ctx: &egui::Context) {
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
        let app = DacApp::new(dac_engine::Session::with_demo(), crate::Services::default());
        let mut h = crate::headless::Headless::new(app, [1000.0, 700.0], 1.0);
        let r = h.request("engine.execute", json!({"command": PRESETS_DIALOG, "params": {}}), std::time::Duration::from_secs(20));
        assert_eq!(r["ok"], true, "{r}");
        let r = h.request("engine.execute", json!({"command": ACTIONS_PANEL, "params": {}}), std::time::Duration::from_secs(20));
        assert_eq!(r["ok"], true, "{r}");
        h.settle(std::time::Duration::from_secs(5));
        let s = state();
        assert!(s.presets_open && s.actions_open);
        // an action with a shortcut plays from the keyboard
        h.app.session.workflow = dac_engine::cmd::actions::Workflow::in_dir(None);
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
}
