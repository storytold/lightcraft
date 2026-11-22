//! The grouped Template Browser shared by the Print and Slideshow modules: built-in templates, the
//! user's templates in "User Templates" and one collapsible group per folder, with a template's
//! context menu moving it between folders and a folder's context menu renaming or deleting it.
//!
//! The model (folders, names, limits) is [`lcc_engine::template_folders`]; [`folder_command`] runs
//! the folder commands (`create`, `rename`, `delete`, `move`) for any module's template list, so a
//! module's `*.createTemplateFolder`, `*.renameTemplateFolder`, `*.deleteTemplateFolder` and
//! `*.moveTemplate` commands are one-liners over it.
#![allow(clippy::indexing_slicing, reason = "not yet converted to checked indexing: tracked in docs/gaps.md (indexing lint)")]

use egui::RichText;
use lcc_engine::template_folders::{self as folders, DEFAULT_FOLDER, Foldered};
use serde_json::{Value, json};

use crate::LccApp;
use crate::widgets::register;

/// One template row.
pub struct Row {
    pub name: String,
    /// The text shown (built-in templates show in the UI language, the user's own as named).
    pub shown: String,
    pub builtin: bool,
    /// The folder of a user template; `None`: User Templates.
    pub folder: Option<String>,
}

/// What the person did in the browser.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Apply(String),
    Delete(String),
    Move { name: String, folder: Option<String> },
    NewFolder,
    RenameFolder(String),
    DeleteFolder(String),
}

/// The ids the browser registers for the control channel and keys its open/closed state with.
pub struct Ids<'a> {
    /// Prefix of a template row's id (`<prefix><name>`).
    pub template: &'a str,
    /// Prefix of a folder header's id (`<prefix><folder>`); `<prefix>` alone is the New Folder button.
    pub folder: &'a str,
    /// Salt of the folder groups' open state.
    pub salt: &'a str,
}

/// The commands a module's browser runs.
pub struct Cmds {
    pub apply: &'static str,
    pub delete: &'static str,
    pub move_to: &'static str,
    pub create_folder: &'static str,
    pub rename_folder: &'static str,
    pub delete_folder: &'static str,
}

fn dim(ui: &egui::Ui, text: &str) -> RichText {
    RichText::new(crate::i18n::tr(text)).small().color(ui.visuals().weak_text_color())
}

fn row_ui(ui: &mut egui::Ui, ids: &Ids, r: &Row, current: Option<&str>, listed: &[String], out: &mut Vec<Action>) {
    let resp = ui.selectable_label(current == Some(r.name.as_str()), r.shown.as_str());
    register(ui.ctx(), format!("{}{}", ids.template, r.name), resp.rect);
    if resp.clicked() {
        out.push(Action::Apply(r.name.clone()));
    }
    if r.builtin {
        return;
    }
    resp.context_menu(|ui| {
        ui.menu_button(crate::i18n::tr("Move to Folder"), |ui| {
            let targets = std::iter::once((None, DEFAULT_FOLDER.to_string())).chain(listed.iter().map(|f| (Some(f.clone()), f.clone())));
            for (target, label) in targets {
                let here = r.folder == target;
                let shown = if target.is_none() { crate::i18n::tr(DEFAULT_FOLDER).to_string() } else { label.clone() };
                let b = ui.add_enabled(!here, egui::Button::new(shown).selected(here));
                register(ui.ctx(), format!("{}{}:moveTo:{}", ids.template, r.name, label), b.rect);
                if b.clicked() {
                    out.push(Action::Move { name: r.name.clone(), folder: target });
                    ui.close();
                }
            }
        });
        if ui.button(crate::i18n::tr("Delete")).clicked() {
            out.push(Action::Delete(r.name.clone()));
            ui.close();
        }
    });
}

/// The user's templates in `folder` (`None`: User Templates).
fn user_rows<'a>(rows: &'a [Row], folder: Option<&'a str>) -> impl Iterator<Item = &'a Row> {
    rows.iter().filter(move |r| !r.builtin && r.folder.as_deref() == folder)
}

/// Draws the browser; the actions are for [`dispatch`].
pub fn show(ui: &mut egui::Ui, ids: &Ids, rows: &[Row], listed: &[String], current: Option<&str>) -> Vec<Action> {
    let mut out = Vec::new();
    if rows.iter().any(|r| r.builtin) {
        ui.label(dim(ui, "Built-in"));
        for r in rows.iter().filter(|r| r.builtin) {
            row_ui(ui, ids, r, current, listed, &mut out);
        }
        ui.add_space(6.0);
    }
    if user_rows(rows, None).next().is_some() || !listed.is_empty() {
        ui.label(dim(ui, DEFAULT_FOLDER));
        for r in user_rows(rows, None) {
            row_ui(ui, ids, r, current, listed, &mut out);
        }
    }
    for f in listed {
        let n = user_rows(rows, Some(f.as_str())).count();
        let header = egui::CollapsingHeader::new(format!("{f} ({n})")).id_salt((ids.salt, f)).default_open(true).show(ui, |ui| {
            for r in user_rows(rows, Some(f.as_str())) {
                row_ui(ui, ids, r, current, listed, &mut out);
            }
        });
        register(ui.ctx(), format!("{}{}", ids.folder, f), header.header_response.rect);
        header.header_response.context_menu(|ui| {
            if ui.button(crate::i18n::tr("Rename Folder…")).clicked() {
                out.push(Action::RenameFolder(f.clone()));
                ui.close();
            }
            if ui.button(crate::i18n::tr("Delete Folder")).clicked() {
                out.push(Action::DeleteFolder(f.clone()));
                ui.close();
            }
        });
    }
    let b = ui.small_button(crate::i18n::tr("New Template Folder"));
    register(ui.ctx(), ids.folder.to_string(), b.rect);
    if b.clicked() {
        out.push(Action::NewFolder);
    }
    out
}

/// Opens the dialog that asks for a new folder's name and then runs `create` (`{name}`).
pub fn prompt_new_folder(app: &mut LccApp, create: &str) {
    crate::panels::dialogs::prompt(app, crate::i18n::tr("New Template Folder"), "Folder name", "", create, json!({}), "name");
}

/// Opens the dialog that asks for `folder`'s new name and then runs `rename` (`{name, to}`).
pub fn prompt_rename_folder(app: &mut LccApp, rename: &str, folder: &str) {
    crate::panels::dialogs::prompt(app, crate::i18n::tr("Rename Folder"), "Folder name", folder, rename, json!({"name": folder}), "to");
}

/// Runs what the person did in the browser; a command's error is shown as a toast.
pub fn dispatch(app: &mut LccApp, ctx: &egui::Context, actions: Vec<Action>, c: &Cmds) {
    for a in actions {
        let r = match a {
            Action::Apply(name) => app.run(c.apply, json!({"name": name})),
            Action::Delete(name) => app.run(c.delete, json!({"name": name})),
            Action::Move { name, folder } => app.run(c.move_to, json!({"name": name, "folder": folder.as_deref().unwrap_or(DEFAULT_FOLDER)})),
            Action::NewFolder => {
                prompt_new_folder(app, c.create_folder);
                Ok(Value::Null)
            }
            Action::RenameFolder(f) => {
                prompt_rename_folder(app, c.rename_folder, &f);
                Ok(Value::Null)
            }
            Action::DeleteFolder(f) => app.run(c.delete_folder, json!({"name": f})),
        };
        if let Err(e) = r {
            app.toast_error(ctx, e);
        }
    }
}

/// `{folders: [{name, default, templates}], templates: [{name, folder}]}` for a module's user
/// templates, the way `print.template.list` lists them ("User Templates" first).
pub fn listing<T: Foldered>(listed: &[String], templates: &[T]) -> Value {
    let folders_json = std::iter::once(json!({"name": DEFAULT_FOLDER, "default": true, "templates": folders::count_in(templates, None)}))
        .chain(listed.iter().map(|f| json!({"name": f, "default": false, "templates": folders::count_in(templates, Some(f))})));
    json!({
        "folders": folders_json.collect::<Vec<_>>(),
        "templates": templates.iter().map(|t| json!({"name": t.name(), "folder": t.folder().unwrap_or(DEFAULT_FOLDER)})).collect::<Vec<_>>(),
    })
}

/// The folder a command's `folder` parameter names: `None` (not given), `Some(None)` for User
/// Templates, `Some(Some(spelling))` for a listed folder. Anything but a string is refused.
pub fn folder_param(listed: &[String], p: &Value) -> Result<Option<Option<String>>, String> {
    match p.get("folder") {
        None => Ok(None),
        Some(Value::String(f)) => folders::find_folder(listed, f).map(Some).ok_or_else(|| format!("no template folder `{}`", f.trim())),
        Some(_) => Err("`folder` must be a folder name".into()),
    }
}

/// Runs a folder command over a module's list: `op` is `create {name}`, `rename {name, to}`,
/// `delete {name}` or `move {name, folder}`. The folder named by `name` must be a listed folder
/// (User Templates can't be renamed or deleted). → [`listing`] plus what the operation says.
pub fn folder_command<T: Foldered>(op: &str, listed: &mut Vec<String>, templates: &mut [T], p: &Value) -> Result<Value, String> {
    let text = |key: &str| p.get(key).and_then(Value::as_str).ok_or_else(|| format!("missing `{key}`"));
    let user_folder = |listed: &[String]| -> Result<String, String> {
        let name = text("name")?;
        folders::find_folder(listed, name)
            .ok_or_else(|| format!("no template folder `{}`", name.trim()))?
            .ok_or_else(|| format!("“{DEFAULT_FOLDER}” can't be renamed or deleted"))
    };
    let mut extra = json!({});
    match op {
        "create" => extra["name"] = json!(folders::create_folder(listed, text("name")?)?),
        "rename" => {
            let from = user_folder(listed)?;
            let (to, _) = folders::rename_folder(listed, templates, &from, text("to")?)?;
            extra["name"] = json!(to);
        }
        "delete" => {
            let name = user_folder(listed)?;
            extra["moved"] = json!(folders::delete_folder(listed, templates, &name));
            extra["deleted"] = json!(name);
        }
        "move" => {
            let folder = folder_param(listed, p)?.ok_or("missing `folder`")?;
            extra["moved"] = json!(folders::move_template(templates, text("name")?, folder)?);
        }
        _ => return Err(format!("unknown folder operation {op}")),
    }
    let mut out = listing(listed, templates);
    if let (Some(o), Some(e)) = (out.as_object_mut(), extra.as_object()) {
        o.extend(e.clone());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct T(String, Option<String>);
    impl Foldered for T {
        fn name(&self) -> &str {
            &self.0
        }
        fn folder(&self) -> Option<&str> {
            self.1.as_deref()
        }
        fn set_folder(&mut self, folder: Option<String>) {
            self.1 = folder;
        }
    }

    #[test]
    fn folder_commands_make_move_rename_and_delete_and_refuse_nonsense() {
        let mut listed = Vec::new();
        let mut ts = vec![T("A".into(), None), T("B".into(), None)];
        let r = folder_command("create", &mut listed, &mut ts, &json!({"name": " Weddings "})).unwrap();
        assert_eq!(r["name"], "Weddings");
        let r = folder_command("move", &mut listed, &mut ts, &json!({"name": "a", "folder": "weddings"})).unwrap();
        assert_eq!((r["moved"].as_bool(), r["templates"][0]["folder"].as_str()), (Some(true), Some("Weddings")));
        let r = folder_command("move", &mut listed, &mut ts, &json!({"name": "A", "folder": "Weddings"})).unwrap();
        assert_eq!(r["moved"], false);
        let r = folder_command("rename", &mut listed, &mut ts, &json!({"name": "WEDDINGS", "to": "Proofs"})).unwrap();
        assert_eq!(r["templates"][0]["folder"], "Proofs");
        for (op, p) in [
            ("create", json!({"name": "user templates"})),
            ("create", json!({"name": "proofs"})),
            ("rename", json!({"name": "User Templates", "to": "X"})),
            ("delete", json!({"name": "User Templates"})),
            ("delete", json!({"name": "Nope"})),
            ("move", json!({"name": "A"})),
            ("move", json!({"name": "A", "folder": 3})),
            ("move", json!({"name": "A", "folder": null})),
            ("move", json!({"name": "A", "folder": "Nope"})),
            ("move", json!({"name": "Nope", "folder": "Proofs"})),
        ] {
            assert!(folder_command(op, &mut listed, &mut ts, &p).is_err(), "{op} {p}");
        }
        let r = folder_command("delete", &mut listed, &mut ts, &json!({"name": "proofs"})).unwrap();
        assert_eq!((r["deleted"].as_str(), r["moved"].as_u64()), (Some("Proofs"), Some(1)));
        assert_eq!(r["templates"][0]["folder"], "User Templates");
        assert!(listed.is_empty());
    }
}
