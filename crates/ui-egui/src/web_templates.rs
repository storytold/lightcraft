//! The Web module's template store: the four built-in layouts plus the user's own templates
//! (`GallerySettings` saved under a name, in folders), the Template Browser that lists them and the
//! `webui.*Template*` commands behind Web ▸ New Template and New Template Folder. The user's
//! templates, their folders and the Show Guides switch are saved with `ui.json`.
//!
//! Folders are [`lcc_engine::template_folders`] (labels in a preferences file, never paths).

use lcc_engine::template_folders::{self as folders, Foldered};
use lcc_webgallery::{GallerySettings, Template};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::LccApp;

/// Most user templates kept.
const MAX_TEMPLATES: usize = 500;
/// Longest template name, characters.
const MAX_NAME: usize = 80;

/// A saved web template.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UserTemplate {
    pub name: String,
    /// The folder the template is listed in; `None`: "User Templates".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    pub settings: GallerySettings,
}

impl Foldered for UserTemplate {
    fn name(&self) -> &str {
        &self.name
    }
    fn folder(&self) -> Option<&str> {
        self.folder.as_deref()
    }
    fn set_folder(&mut self, folder: Option<String>) {
        self.folder = folder;
    }
}

/// What the Web module keeps in `ui.json`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct WebState {
    /// The user's templates (the built-in ones are the layouts).
    pub templates: Vec<UserTemplate>,
    /// The Template Browser's folders besides "User Templates" (empty ones too), sorted by name.
    pub template_folders: Vec<String>,
    /// The template last applied or saved ("" until one is).
    pub template: String,
    /// View ▸ Show Guides: cell and margin outlines on the preview.
    pub show_guides: bool,
}

const CMDS: crate::template_browser::Cmds = crate::template_browser::Cmds {
    apply: "webui.applyTemplate",
    delete: "webui.deleteTemplate",
    move_to: "webui.moveTemplate",
    create_folder: "webui.createTemplateFolder",
    rename_folder: "webui.renameTemplateFolder",
    delete_folder: "webui.deleteTemplateFolder",
};

/// The commands of this file.
pub fn is_command(id: &str) -> bool {
    matches!(
        id,
        "webui.applyTemplate"
            | "webui.saveTemplate"
            | "webui.deleteTemplate"
            | "webui.moveTemplate"
            | "webui.createTemplateFolder"
            | "webui.renameTemplateFolder"
            | "webui.deleteTemplateFolder"
            | "webui.templates"
            | "webui.guides"
    )
}

fn builtin(name: &str) -> Option<Template> {
    let name = name.trim();
    Template::ALL.into_iter().find(|t| t.label().eq_ignore_ascii_case(name) || t.key().eq_ignore_ascii_case(name))
}

fn name_param(p: &Value) -> Result<String, String> {
    let n = p.get("name").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).ok_or("missing name")?;
    if n.chars().count() > MAX_NAME || n.chars().any(char::is_control) {
        return Err(format!("a template name is 1 to {MAX_NAME} characters without control characters"));
    }
    Ok(n.to_string())
}

fn state_json(app: &LccApp) -> Value {
    let w = &app.ui.web;
    json!({
        "template": w.template,
        "templates": Template::ALL.iter().map(|t| t.label().to_string()).chain(w.templates.iter().map(|t| t.name.clone())).collect::<Vec<_>>(),
        "templateFolders": crate::template_browser::listing(&w.template_folders, &w.templates),
        "showGuides": w.show_guides,
    })
}

/// Runs a command of this file; `None` if `id` is not one.
pub fn run(app: &mut LccApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    if !is_command(id) {
        return None;
    }
    let w = &mut app.ui.web;
    folders::tidy_folders(&mut w.template_folders, &mut w.templates);
    Some(run_inner(app, id, p))
}

fn run_inner(app: &mut LccApp, id: &str, p: &Value) -> Result<Value, String> {
    match id {
        "webui.templates" => Ok(state_json(app)),
        "webui.guides" => {
            let show = match p.get("show") {
                None | Some(Value::Null) => !app.ui.web.show_guides,
                Some(Value::Bool(b)) => *b,
                Some(_) => return Err("`show` must be true or false".into()),
            };
            app.ui.web.show_guides = show;
            Ok(state_json(app))
        }
        "webui.applyTemplate" => {
            let name = name_param(p)?;
            if let Some(t) = builtin(&name) {
                let mut s = crate::web_module::shown_settings();
                s.template = t;
                crate::web_module::queue_settings(s);
                app.ui.web.template = t.label().to_string();
            } else {
                let t =
                    app.ui.web.templates.iter().find(|t| t.name.eq_ignore_ascii_case(&name)).ok_or_else(|| format!("no template named {name}"))?;
                let (settings, shown) = (t.settings.sanitized(), t.name.clone());
                crate::web_module::queue_settings(settings);
                app.ui.web.template = shown;
            }
            Ok(state_json(app))
        }
        "webui.saveTemplate" => {
            let name = name_param(p)?;
            if builtin(&name).is_some() {
                return Err(format!("{name} is a built-in template; choose another name"));
            }
            // the settings shown, unless the call brings its own
            let settings = match p.get("settings") {
                None | Some(Value::Null) => crate::web_module::shown_settings(),
                Some(v) => GallerySettings::from_json(v)?,
            };
            let given = crate::template_browser::folder_param(&app.ui.web.template_folders, p)?;
            let list = &mut app.ui.web.templates;
            let stored = match list.iter_mut().find(|t| t.name.eq_ignore_ascii_case(&name)) {
                // a template saved over its own name stays in its folder unless the call names another
                Some(t) => {
                    t.settings = settings;
                    if let Some(f) = given {
                        t.folder = f;
                    }
                    t.name.clone()
                }
                None => {
                    if list.len() >= MAX_TEMPLATES {
                        return Err(format!("at most {MAX_TEMPLATES} templates; delete one first"));
                    }
                    list.push(UserTemplate { name: name.clone(), folder: given.flatten(), settings });
                    name
                }
            };
            app.ui.web.template = stored;
            Ok(state_json(app))
        }
        "webui.deleteTemplate" => {
            let name = name_param(p)?;
            let list = &mut app.ui.web.templates;
            let before = list.len();
            list.retain(|t| !t.name.eq_ignore_ascii_case(&name));
            if before == list.len() {
                return Err(format!("no user template named {name}"));
            }
            Ok(state_json(app))
        }
        _ => {
            let op = match id {
                "webui.createTemplateFolder" => "create",
                "webui.renameTemplateFolder" => "rename",
                "webui.deleteTemplateFolder" => "delete",
                _ => "move",
            };
            let w = &mut app.ui.web;
            crate::template_browser::folder_command(op, &mut w.template_folders, &mut w.templates, p)
        }
    }
}

/// The Template Browser: the built-in layouts, the user's templates and folders, and a button that
/// asks for a name and saves the settings shown as a template.
pub fn browser(ui: &mut egui::Ui, app: &mut LccApp) {
    let w = &app.ui.web;
    let mut rows: Vec<crate::template_browser::Row> = Template::ALL
        .iter()
        .map(|t| crate::template_browser::Row {
            name: t.label().to_string(),
            shown: crate::i18n::tr(t.label()).to_string(),
            builtin: true,
            folder: None,
        })
        .collect();
    rows.extend(w.templates.iter().map(|t| crate::template_browser::Row {
        name: t.name.clone(),
        shown: t.name.clone(),
        builtin: false,
        folder: t.folder.clone(),
    }));
    let ids = crate::template_browser::Ids { template: "webTemplate:", folder: "webTemplateFolder:", salt: "web-template-folder" };
    let current = if w.template.is_empty() { crate::web_module::shown_settings().template.label().to_string() } else { w.template.clone() };
    let actions = crate::template_browser::show(ui, &ids, &rows, &w.template_folders, Some(current.as_str()));
    let ctx = ui.ctx().clone();
    crate::template_browser::dispatch(app, &ctx, actions, &CMDS);
    let b = ui.small_button(crate::i18n::tr("Save Template"));
    crate::widgets::register(ui.ctx(), "webTemplateSave", b.rect);
    if b.clicked() {
        crate::panels::dialogs::prompt(app, crate::i18n::tr("New Template"), "Template name", "", "webui.saveTemplate", json!({}), "name");
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[allow(clippy::indexing_slicing, reason = "test code: clippy.toml allows indexing in tests, but its test check misses cfg(all(test, ...))")]
mod tests {
    use std::time::Duration;

    use serde_json::{Value, json};

    use crate::headless::Headless;
    use crate::{LccApp, Services};

    const T: Duration = Duration::from_secs(20);

    fn demo() -> Headless {
        let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
        let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
        h.settle(Duration::from_secs(120));
        h
    }

    fn exec(h: &mut Headless, id: &str, params: Value) -> Value {
        let r = h.request("engine.execute", json!({"command": id, "params": params}), T);
        h.step();
        r
    }

    fn ok(h: &mut Headless, id: &str, params: Value) -> Value {
        let r = exec(h, id, params);
        assert_eq!(r["ok"], true, "{id}: {r}");
        r["result"].clone()
    }

    fn shown(h: &Headless) -> lcc_webgallery::GallerySettings {
        crate::web_module::state(&h.view.ctx).settings
    }

    /// New Template asks for a name and stores the settings shown, applying it brings them back,
    /// folders group templates, a hostile saved list is tidied, and nonsense is refused.
    #[test]
    fn web_templates_save_apply_group_and_tidy() {
        let mut h = demo();
        ok(&mut h, "module.web", json!({}));
        let wait = Duration::from_secs(120);
        assert!(h.step_until(wait, |h| h.app.widgets.iter().any(|(w, _)| w == "webTemplate:Grid")), "the browser lists the built-ins");
        // the menu item asks for the name
        ok(&mut h, "ui.prompt", json!({"title": "New Template", "hint": "Template name", "command": "webui.saveTemplate", "key": "name"}));
        let Some(crate::state::Dialog::TextPrompt { value, .. }) = &mut h.app.ui.dialog else { panic!("{:?}", h.app.ui.dialog) };
        *value = " Mine ".into();
        assert_eq!(h.request("ui.dialog.confirm", json!({}), T)["ok"], true);
        assert_eq!(h.app.ui.web.templates.len(), 1);
        assert_eq!(h.app.ui.web.templates[0].name, "Mine");
        // a template stores the settings it was saved with and applying shows them
        let custom = json!({"site": {"title": "Wedding"}, "template": "track"});
        ok(&mut h, "webui.saveTemplate", json!({"name": "Wedding", "settings": custom}));
        ok(&mut h, "webui.applyTemplate", json!({"name": "Mine"}));
        h.step();
        assert_eq!(shown(&h).site.title, "");
        ok(&mut h, "webui.applyTemplate", json!({"name": "wedding"}));
        h.step();
        let s = shown(&h);
        assert_eq!((s.site.title.as_str(), s.template), ("Wedding", lcc_webgallery::Template::Track));
        // a built-in applies only its layout
        ok(&mut h, "webui.applyTemplate", json!({"name": "Square grid"}));
        h.step();
        let s = shown(&h);
        assert_eq!((s.site.title.as_str(), s.template), ("Wedding", lcc_webgallery::Template::Square));
        // folders
        ok(&mut h, "webui.createTemplateFolder", json!({"name": "Weddings"}));
        assert!(h.step_until(wait, |h| h.app.widgets.iter().any(|(w, _)| w == "webTemplateFolder:Weddings")), "the empty folder is listed");
        let r = ok(&mut h, "webui.moveTemplate", json!({"name": "Wedding", "folder": "weddings"}));
        assert_eq!(r["moved"], true);
        // saving over a name keeps the folder
        ok(&mut h, "webui.saveTemplate", json!({"name": "wedding"}));
        let folder = h.app.ui.web.templates.iter().find(|t| t.name == "Wedding").and_then(|t| t.folder.clone());
        assert_eq!(folder.as_deref(), Some("Weddings"));
        for (id, p) in [
            ("webui.saveTemplate", json!({"name": "Grid"})),
            ("webui.saveTemplate", json!({"name": "  "})),
            ("webui.saveTemplate", json!({"name": "a\u{7}b"})),
            ("webui.saveTemplate", json!({"name": "X", "folder": "Nope"})),
            ("webui.saveTemplate", json!({"name": "X", "settings": {"template": 7}})),
            ("webui.applyTemplate", json!({"name": "Nope"})),
            ("webui.deleteTemplate", json!({"name": "Grid"})),
            ("webui.moveTemplate", json!({"name": "Grid", "folder": "Weddings"})),
            ("webui.createTemplateFolder", json!({"name": "weddings"})),
            ("webui.createTemplateFolder", json!({"name": "User Templates"})),
            ("webui.deleteTemplateFolder", json!({"name": "User Templates"})),
            ("webui.guides", json!({"show": "yes"})),
        ] {
            assert_eq!(exec(&mut h, id, p.clone())["ok"], false, "{id} {p}");
        }
        // a hostile saved list is tidied when a command runs
        h.app.ui.web.template_folders = vec!["Weddings".into(), String::new(), "weddings".into(), "User Templates".into()];
        h.app.ui.web.templates[0].folder = Some("No\u{1}where".into());
        let r = ok(&mut h, "webui.templates", json!({}));
        assert_eq!(h.app.ui.web.template_folders, ["Weddings"]);
        assert_eq!(h.app.ui.web.templates[0].folder, None, "{r}");
        // deleting a folder keeps its templates; deleting a template removes it
        let r = ok(&mut h, "webui.deleteTemplateFolder", json!({"name": "Weddings"}));
        assert_eq!(r["moved"], 1);
        ok(&mut h, "webui.deleteTemplate", json!({"name": "mine"}));
        assert_eq!(h.app.ui.web.templates.len(), 1);
        // the store round-trips through JSON (ui.json)
        let back: super::WebState = serde_json::from_value(serde_json::to_value(&h.app.ui.web).unwrap()).unwrap();
        assert_eq!(back.templates, h.app.ui.web.templates);
    }

    /// Show Guides is a checked toggle.
    #[test]
    fn show_guides_is_a_checked_toggle() {
        let mut h = demo();
        ok(&mut h, "module.web", json!({}));
        h.settle(Duration::from_secs(60));
        assert_eq!(crate::menubar::checked(&h.app, "webui.guides"), Some(false));
        let r = ok(&mut h, "webui.guides", json!({}));
        assert_eq!(r["showGuides"], true);
        assert_eq!(crate::menubar::checked(&h.app, "webui.guides"), Some(true));
        // the preview draws with the guides on
        h.settle(Duration::from_secs(60));
        let r = ok(&mut h, "webui.guides", json!({"show": false}));
        assert_eq!(r["showGuides"], false);
    }
}
