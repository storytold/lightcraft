//! Catalog-level UI (P1.5): File ▸ New / Open / Open Recent Catalog, the catalog chooser
//! (at startup when asked for, or with Alt held while the window opens), Catalog Settings,
//! Export as Catalog, Import from Another Catalog with its change preview, and the
//! Read / Save Metadata conflict question, and the welcome dialog of a first start (no catalog
//! yet: create one, or open one).
//!
//! The work is done by engine commands (`catalog.*`, `photo.xmpStatus`, `library.previewSettings`);
//! this module only holds the dialogs and the UI commands that switch catalogs.

use std::path::{Path, PathBuf};

use lcc_catalog::library::RecentCatalogs;
use serde_json::{Value, json};

use crate::LccApp;
use crate::pick::Picked;

/// The catalog dialogs (one at a time, in their own window).
#[derive(Clone, Debug, PartialEq)]
pub enum CatalogDialog {
    /// First start, no catalog to open yet: create one (name, location, sample photos or not) or
    /// open one. `foreign`: libraries of another app found where this app's would be (e.g. the
    /// upstream app's), which can't be opened here; named so the user knows why they don't open.
    /// `updates`: the user ticked "Check for updates when the app starts" (unticked at first).
    /// Each action carries it as `checkAtStartup`, and it is saved through `update.prefs` only
    /// when that action opens a catalog; closing the dialog saves nothing.
    Welcome {
        parent: String,
        name: String,
        demo: bool,
        updates: bool,
        foreign: Vec<String>,
    },
    /// Pick a catalog: the recent ones, New…, Open….
    Chooser,
    NewCatalog {
        parent: String,
        name: String,
    },
    Settings {
        settings: Value,
    },
    ExportCatalog {
        parent: String,
        name: String,
        scope: String,
        originals: bool,
        previews: bool,
    },
    /// `plan`: the engine's change preview (`catalog.import` with `preview`), or the error.
    ImportCatalog {
        path: String,
        plan: Result<Value, String>,
        rule: String,
    },
    /// Read / Save Metadata would overwrite changes on the other side.
    XmpConflict {
        command: String,
        ids: Vec<u64>,
        affected: usize,
    },
}

/// Catalog UI state on [`LccApp`].
#[derive(Default)]
pub struct CatalogUi {
    pub dialog: Option<CatalogDialog>,
    /// `recent-catalogs.json`; `None` (tests, scripts with `<PREFIX>_NO_PREFS`): no list is kept.
    pub recent_file: Option<PathBuf>,
    /// The list, read once.
    recent: Option<RecentCatalogs>,
    /// Frames since the window opened (Alt held during the first ones opens the chooser).
    frames: u32,
    /// Files from the command line of a first start, imported once a catalog is open.
    pub pending_import: Vec<String>,
    /// Why the welcome dialog's "Check for updates when the app starts" couldn't be turned on once
    /// its catalog opened: said in a toast on the next frame.
    pub update_error: Option<String>,
}

/// The welcome dialog: a new catalog under the Pictures folder by default.
pub fn welcome(app: &mut LccApp, foreign: Vec<String>) {
    let parent = lcc_engine::library::default_dir()
        .and_then(|d| d.parent().map(Path::to_path_buf))
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_default();
    app.catalog_ui.dialog =
        Some(CatalogDialog::Welcome { parent, name: lcc_brand::LIBRARY_DEFAULT.to_string(), demo: false, updates: false, foreign });
}

impl CatalogUi {
    pub fn recent(&mut self) -> &mut RecentCatalogs {
        let file = self.recent_file.clone();
        self.recent.get_or_insert_with(|| file.map(|f| RecentCatalogs::load(&f)).unwrap_or_default())
    }

    /// The list without loading it into the cache (menus are built from `&LccApp`).
    pub fn recent_view(&self) -> RecentCatalogs {
        match (&self.recent, &self.recent_file) {
            (Some(r), _) => r.clone(),
            (None, Some(f)) => RecentCatalogs::load(f),
            (None, None) => RecentCatalogs::default(),
        }
    }

    fn save_recent(&mut self) {
        let Some(file) = self.recent_file.clone() else { return };
        if let Some(r) = &self.recent
            && let Err(e) = r.save(&file)
        {
            log::warn!("recent catalogs: {e}");
        }
    }

    /// Put the catalog in `dir` at the top of Open Recent.
    pub fn touch(&mut self, dir: &Path) {
        let entry = lcc_catalog::library::find_entry(dir).unwrap_or_else(|| dir.to_path_buf());
        self.recent().touch(&entry);
        self.save_recent();
    }
}

/// Open a catalog chosen by the user (entry point or folder): validated first, so a folder that
/// isn't a catalog is an error rather than a new, empty library.
fn open_catalog(app: &mut LccApp, path: &str, p: &Value) -> Result<Value, String> {
    let dir = lcc_catalog::library::resolve(Path::new(path)).map_err(|e| e.to_string())?;
    open_dir(app, &dir, false, p)
}

/// Whether the call carries the welcome dialog's ticked "Check for updates when the app starts"
/// (`checkAtStartup: true`). It travels in the call's own parameters, so a folder dialog that
/// answers later brings it back with its answer, and no other call ever takes it.
fn check_at_startup(p: &Value) -> bool {
    p.get("checkAtStartup").and_then(Value::as_bool).unwrap_or(false)
}

/// Open the catalog in `dir`; with `checkAtStartup` in `p`, the start-up update check is turned
/// on (through `update.prefs`) once it is open.
fn open_dir(app: &mut LccApp, dir: &Path, demo: bool, p: &Value) -> Result<Value, String> {
    let r = crate::panels::settings::open_library(app, &json!({"path": dir.to_string_lossy(), "demo": demo}))?;
    app.catalog_ui.touch(dir);
    if check_at_startup(p)
        && let Err(e) = app.session.execute("update.prefs", &json!({"checkAtStartup": true}))
    {
        log::warn!("update.prefs: {e}");
        app.catalog_ui.update_error = Some(e.to_string());
    }
    app.catalog_ui.dialog = None;
    Ok(r)
}

fn str_of<'a>(p: &'a Value, key: &str) -> Option<&'a str> {
    p.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

/// Ask for a folder for `command` (answer under `key`).
fn pick_folder(app: &mut LccApp, command: &str, p: &Value, key: &'static str, title: &str) -> Result<Option<String>, String> {
    let req = crate::pick::PickRequest::folder(crate::i18n::tr(title));
    match crate::pick::ask(app, command, p, key, req, |s| s.pick_folder.as_mut().and_then(|f| f()).map(|x| vec![x])) {
        Picked::Now(v) => Ok(v.into_iter().next()),
        Picked::Later => Ok(None),
        Picked::Unavailable => Err("no folder dialog on this platform: pass the path".into()),
    }
}

/// The UI commands of this module; `None` for other ids.
pub fn run(app: &mut LccApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    Some(match id {
        "catalog.new" => new_catalog(app, p),
        "catalog.open" => match str_of(p, "path") {
            Some(path) => open_catalog(app, path, p),
            None => match pick_folder(app, id, p, "path", "Open Catalog") {
                Ok(Some(path)) => open_catalog(app, &path, p),
                Ok(None) => Ok(Value::Null),
                Err(e) => Err(e),
            },
        },
        "catalog.openRecent" => match str_of(p, "path") {
            Some(path) => {
                let r = open_catalog(app, path, p);
                if r.is_err() && !Path::new(path).exists() {
                    // gone: drop it from the list
                    app.catalog_ui.recent().remove(Path::new(path));
                    app.catalog_ui.save_recent();
                }
                r
            }
            None => Ok(json!({"recent": app.catalog_ui.recent().existing()})),
        },
        "catalog.welcome" => {
            welcome(app, Vec::new());
            Ok(Value::Null)
        }
        "catalog.chooser" => {
            app.catalog_ui.dialog = Some(CatalogDialog::Chooser);
            Ok(Value::Null)
        }
        "catalog.promptAtStartup" => {
            let on = p.get("on").and_then(Value::as_bool).unwrap_or(!app.catalog_ui.recent().prompt_at_startup);
            app.catalog_ui.recent().prompt_at_startup = on;
            app.catalog_ui.save_recent();
            Ok(json!({"promptAtStartup": on}))
        }
        "dialog.catalogSettings" => {
            let settings = app.session.execute("catalog.info", &json!({})).map(|v| v["settings"].clone()).unwrap_or_default();
            app.catalog_ui.dialog = Some(CatalogDialog::Settings { settings });
            Ok(Value::Null)
        }
        "dialog.exportCatalog" => {
            let scope = if app.session.selection.ids.len() > 1 { "selected" } else { "all" };
            let parent = app.session.catalog_dir().and_then(Path::parent).map(|d| d.to_string_lossy().into_owned()).unwrap_or_default();
            app.catalog_ui.dialog =
                Some(CatalogDialog::ExportCatalog { parent, name: "Exported Catalog".into(), scope: scope.into(), originals: false, previews: true });
            Ok(Value::Null)
        }
        "dialog.importCatalog" => match str_of(p, "path") {
            Some(path) => {
                let plan = app.session.execute("catalog.import", &json!({"path": path, "preview": true})).map_err(|e| e.to_string());
                app.catalog_ui.dialog = Some(CatalogDialog::ImportCatalog { path: path.into(), plan, rule: "replaceSettingsAndMetadata".into() });
                Ok(Value::Null)
            }
            None => match pick_folder(app, id, p, "path", "Import from Another Catalog") {
                Ok(Some(path)) => run(app, id, &json!({"path": path}))?,
                Ok(None) => Ok(Value::Null),
                Err(e) => Err(e),
            },
        },
        "photo.readMetadataFromFile" | "photo.saveMetadataToFile" if !p.get("confirmed").and_then(Value::as_bool).unwrap_or(false) => {
            return xmp_conflict(app, id, p);
        }
        _ => return None,
    })
}

/// Read / Save Metadata: ask first when it would overwrite newer changes on the other side
/// (Read: changes made in the catalog; Save: changes another app made to the file).
fn xmp_conflict(app: &mut LccApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    let st = app.session.execute("photo.xmpStatus", p).ok()?;
    let lost = if id == "photo.readMetadataFromFile" { ["changedInCatalog", "conflict"] } else { ["changedOnDisk", "conflict"] };
    let photos = st["photos"].as_array()?;
    let ids: Vec<u64> = photos.iter().filter_map(|x| x["id"].as_u64()).collect();
    let affected = photos.iter().filter(|x| x["status"].as_str().is_some_and(|s| lost.contains(&s))).count();
    if affected == 0 {
        return None;
    }
    app.catalog_ui.dialog = Some(CatalogDialog::XmpConflict { command: id.into(), ids, affected });
    Some(Ok(json!({"confirm": "dialog", "affected": affected})))
}

fn new_catalog(app: &mut LccApp, p: &Value) -> Result<Value, String> {
    match (str_of(p, "parent"), str_of(p, "name")) {
        (Some(parent), Some(name)) => {
            let r = if p.get("demo").and_then(Value::as_bool).unwrap_or(false) {
                // an empty folder, opened as a new library: it starts with the demo photos
                let dir = lcc_catalog::library::create_folder(Path::new(parent), name).map_err(|e| e.to_string())?;
                let mut r = open_dir(app, &dir, true, p)?;
                r["entry"] = json!(lcc_catalog::library::find_entry(&dir));
                r
            } else {
                let entry = lcc_catalog::library::create(Path::new(parent), name).map_err(|e| e.to_string())?;
                let dir = entry.parent().map(Path::to_path_buf).ok_or("catalog without a folder")?;
                let mut r = open_dir(app, &dir, false, p)?;
                r["entry"] = json!(entry);
                r
            };
            Ok(r)
        }
        _ => {
            let parent = str_of(p, "parent")
                .map(str::to_string)
                .or_else(|| app.session.catalog_dir().and_then(Path::parent).map(|d| d.to_string_lossy().into_owned()))
                .unwrap_or_default();
            app.catalog_ui.dialog = Some(CatalogDialog::NewCatalog { parent, name: str_of(p, "name").unwrap_or("New Catalog").into() });
            Ok(Value::Null)
        }
    }
}

/// Whether a UI command of this module is available.
pub fn enabled(app: &LccApp, id: &str) -> Option<bool> {
    let busy = crate::lightroom_import::is_running(app);
    Some(match id {
        "catalog.new" | "catalog.open" | "catalog.chooser" | "catalog.openRecent" | "catalog.welcome" => !busy,
        "dialog.importCatalog" => !busy && app.services.pick_folder.is_some(),
        "dialog.catalogSettings" => app.session.catalog_dir().is_some(),
        "dialog.exportCatalog" => !app.session.catalog.is_empty(),
        _ => return None,
    })
}

/// File ▸ Open Recent Catalog entries: (label, params).
pub fn recent_items(app: &LccApp) -> Vec<(String, Value)> {
    let current = app.session.catalog_dir().map(Path::to_path_buf);
    app.catalog_ui
        .recent_view()
        .existing()
        .into_iter()
        .filter(|p| current.as_deref().is_none_or(|c| lcc_catalog::library::resolve(p).ok().as_deref() != Some(c)))
        .map(|p| (display_name(&p), json!({"path": p})))
        .collect()
}

fn display_name(p: &Path) -> String {
    let dir = if p.is_file() { p.parent().unwrap_or(p) } else { p };
    let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| dir.display().to_string());
    format!("{name} — {}", dir.parent().map(|d| d.display().to_string()).unwrap_or_default())
}

/// Whether Open Catalog…'s folder dialog is still up.
fn picking_catalog(app: &LccApp) -> bool {
    app.pending_picks.iter().any(|p| p.command == "catalog.open")
}

/// Each frame: Alt held while the window opens shows the chooser; then the dialog, if any.
pub fn show(app: &mut LccApp, ctx: &egui::Context) {
    if let Some(e) = app.catalog_ui.update_error.take() {
        app.toast_error(ctx, format!("{}: {e}", crate::i18n::tr("Checking for updates when the app starts is still off")));
    }
    if app.catalog_ui.frames < 30 {
        app.catalog_ui.frames += 1;
        let alt = ctx.input(|i| i.modifiers.alt);
        let asked = app.catalog_ui.frames == 1 && app.catalog_ui.recent_file.is_some() && app.catalog_ui.recent().prompt_at_startup;
        if (alt && app.catalog_ui.recent_file.is_some() || asked) && app.catalog_ui.dialog.is_none() {
            app.catalog_ui.dialog = Some(CatalogDialog::Chooser);
            app.catalog_ui.frames = 30;
        }
    }
    let Some(mut dlg) = app.catalog_ui.dialog.clone() else { return };
    let welcoming = matches!(dlg, CatalogDialog::Welcome { .. });
    if welcoming {
        // nothing behind it can be used before a catalog is open
        crate::panels::library_problem::dim(ctx);
    }
    let title = match &dlg {
        CatalogDialog::Welcome { .. } => "Welcome to {app}",
        CatalogDialog::Chooser => "Select a Catalog",
        CatalogDialog::NewCatalog { .. } => "New Catalog",
        CatalogDialog::Settings { .. } => "Catalog Settings",
        CatalogDialog::ExportCatalog { .. } => "Export as Catalog",
        CatalogDialog::ImportCatalog { .. } => "Import from Catalog",
        CatalogDialog::XmpConflict { command, .. } if command == "photo.readMetadataFromFile" => "Read Metadata from File",
        CatalogDialog::XmpConflict { .. } => "Save Metadata to File",
    };
    let mut close = false;
    let mut action: Option<(String, Value)> = None;
    let t = crate::theme::Tokens::get(ctx);
    let frame = egui::Frame::window(&ctx.global_style()).inner_margin(egui::Margin::symmetric(16, 12));
    let window_id = egui::Id::new("catalog-dialog");
    let shown = egui::Window::new(crate::i18n::tr(title))
        .id(window_id)
        .order(if welcoming { egui::Order::Foreground } else { egui::Order::Middle })
        .collapsible(false)
        .resizable(false)
        .frame(frame)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .default_width(440.0)
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            let dim = |s: &str| egui::RichText::new(crate::i18n::tr(s)).color(t.text_dim);
            // capped to the screen; the buttons are part of each variant's content, so they scroll
            // with it in a window too short for the whole dialog
            let mut layout = crate::dialog_frame::ModalLayout::begin(ui, window_id);
            layout.body(ui, |ui| match &mut dlg {
                CatalogDialog::Welcome { parent, name, demo, updates, foreign } => {
                    welcome_ui(app, ui, &t, parent, name, demo, updates, foreign, &mut action);
                }
                CatalogDialog::Chooser => chooser(app, ui, &mut action, &mut close),
                CatalogDialog::NewCatalog { parent, name } => {
                    ui.label(dim("A catalog is a folder: its photos' records, settings and history. The photos themselves stay where they are."));
                    text_row(ui, "Name", name, "field:catalogName");
                    folder_row(app, ui, "Location", parent, "field:catalogParent", "button:catalogParent");
                    if buttons(ui, "Create", "button:catalogCreate", &mut close) {
                        action = Some(("catalog.new".into(), json!({"parent": parent, "name": name})));
                    }
                }
                CatalogDialog::Settings { settings } => settings_ui(app, ui, settings, &mut action, &mut close),
                CatalogDialog::ExportCatalog { parent, name, scope, originals, previews } => {
                    let n = app.session.selection.ids.len().max(usize::from(app.session.active().is_some()));
                    ui.horizontal(|ui| {
                        let r = ui.radio_value(scope, "selected".to_string(), format!("{} ({n})", crate::i18n::tr("Selected photos")));
                        crate::widgets::register(ui.ctx(), "radio:exportCatalogSelected", r.rect);
                        let r = ui.radio_value(scope, "all".to_string(), crate::i18n::tr("All photos"));
                        crate::widgets::register(ui.ctx(), "radio:exportCatalogAll", r.rect);
                    });
                    text_row(ui, "Name", name, "field:exportCatalogName");
                    folder_row(app, ui, "Location", parent, "field:exportCatalogParent", "button:exportCatalogParent");
                    let r = ui.checkbox(originals, crate::i18n::tr("Export the original files (copied into the new catalog's Originals folder)"));
                    crate::widgets::register(ui.ctx(), "check:exportCatalogOriginals", r.rect);
                    let r = ui.checkbox(previews, crate::i18n::tr("Include available previews"));
                    crate::widgets::register(ui.ctx(), "check:exportCatalogPreviews", r.rect);
                    if buttons(ui, "Export Catalog", "button:exportCatalog", &mut close) {
                        action = Some((
                            "catalog.export".into(),
                            json!({"parent": parent, "name": name, "scope": scope, "originals": originals, "previews": previews}),
                        ));
                    }
                }
                CatalogDialog::ImportCatalog { path, plan, rule } => {
                    ui.label(egui::RichText::new(path.as_str()).color(t.text_label));
                    match plan {
                        Err(e) => {
                            ui.label(egui::RichText::new(e.as_str()).color(egui::Color32::from_rgb(230, 90, 80)));
                            if buttons(ui, "Close", "button:importCatalogClose", &mut close) {
                                close = true;
                            }
                        }
                        Ok(v) => {
                            import_preview(ui, v, rule, &t);
                            if buttons(ui, "Import", "button:importCatalog", &mut close) {
                                action = Some(("catalog.import".into(), json!({"path": path, "rule": rule})));
                            }
                        }
                    }
                }
                CatalogDialog::XmpConflict { command, ids, affected } => {
                    let read = command == "photo.readMetadataFromFile";
                    ui.label(crate::i18n::tr(if read {
                            "Photos changed in the catalog since their metadata file was last read or written: reading replaces those changes with what the file says."
                        } else {
                            "Photos whose metadata file another program changed: saving replaces those changes with the catalog's."
                        }));
                    ui.label(egui::RichText::new(format!("{}: {affected}", crate::i18n::tr("Photos affected"))).strong());
                    if buttons(ui, if read { "Read" } else { "Overwrite" }, "button:xmpConflictConfirm", &mut close) {
                        action = Some((command.clone(), json!({"ids": ids, "confirmed": true})));
                    }
                }
            });
            layout.footer(ui, |_| {});
        });
    crate::dialog_frame::ModalLayout::finish(ctx, window_id, shown.as_ref().map(|w| w.response.rect));
    if app.catalog_ui.dialog.is_some() {
        app.catalog_ui.dialog = Some(dlg);
    }
    // the welcome dialog stays until a catalog is open (or the app quits)
    if !welcoming && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        close = true;
    }
    if close {
        app.catalog_ui.dialog = None;
    }
    if let Some((cmd, params)) = action {
        // the command may open the next dialog (Chooser ▸ New Catalog…); on an error, or for the
        // Catalog Settings buttons, this one stays
        let prev = app.catalog_ui.dialog.take();
        let r = app.run(&cmd, params);
        // the welcome dialog also stays (waiting) while its Open Catalog… folder dialog is up and
        // nothing is open: cancelling that leaves the user where they were
        let keep = r.is_err()
            || (welcoming && cmd == "catalog.open" && picking_catalog(app) && app.session.library.is_none())
            || matches!(cmd.as_str(), "catalog.backup" | "catalog.checkIntegrity" | "catalog.optimize");
        if keep && app.catalog_ui.dialog.is_none() {
            app.catalog_ui.dialog = prev;
        }
        match r {
            Ok(v) => {
                if let Some(msg) = done_message(&cmd, &v) {
                    app.toast(ctx, msg);
                }
            }
            Err(e) => app.toast_error(ctx, e),
        }
    }
    // a first start's command-line files, once a catalog is open
    if !app.catalog_ui.pending_import.is_empty() && app.session.library.is_some() {
        let files = std::mem::take(&mut app.catalog_ui.pending_import);
        if let Err(e) = crate::import::start_paths(app, files) {
            log::warn!("import: {e}");
        }
    }
}

/// The welcome dialog's body.
#[allow(clippy::too_many_arguments)]
fn welcome_ui(
    app: &mut LccApp,
    ui: &mut egui::Ui,
    t: &crate::theme::Tokens,
    parent: &mut String,
    name: &mut String,
    demo: &mut bool,
    updates: &mut bool,
    foreign: &[String],
    action: &mut Option<(String, Value)>,
) {
    ui.set_width(460.0);
    let dim = |s: &str| egui::RichText::new(crate::i18n::tr(s)).color(t.text_dim);
    ui.label(dim(
        "{app} keeps the records of your photos, their edits and history in a catalog. Create one to start, or open one you already have. Your photos stay where they are.",
    ));
    // Open Catalog…'s folder dialog is up: everything here waits for it
    if picking_catalog(app) {
        ui.label(egui::RichText::new(crate::i18n::tr("Choose a folder…")).strong());
        ui.disable();
    }
    ui.add_space(4.0);
    ui.label(egui::RichText::new(crate::i18n::tr("Create a new catalog")).strong());
    ui.horizontal(|ui| {
        ui.add_sized([80.0, 20.0], egui::Label::new(crate::i18n::tr("Name")));
        crate::text_field::TextField::singleline("field:welcomeName", name).width(360.0).show(ui);
    });
    ui.horizontal(|ui| {
        ui.add_sized([80.0, 20.0], egui::Label::new(crate::i18n::tr("Location")));
        crate::text_field::TextField::singleline("field:welcomeParent", parent).width(280.0).show(ui);
        let r = ui.add_enabled(app.services.pick_folder.is_some(), egui::Button::new(crate::i18n::tr("Choose…")));
        crate::widgets::register(ui.ctx(), "button:welcomeParent", r.rect);
        if r.clicked()
            && let Some(d) = app.services.pick_folder.as_mut().and_then(|f| f())
        {
            *parent = d;
        }
    });
    let r = ui.checkbox(demo, crate::i18n::tr("Add sample photos to try things out"));
    crate::widgets::register(ui.ctx(), "check:welcomeDemo", r.rect);
    let r = ui.checkbox(updates, crate::i18n::tr("Check for updates when the app starts"));
    crate::widgets::register(ui.ctx(), "check:welcomeUpdates", r.rect);
    let ready = !name.trim().is_empty() && !parent.trim().is_empty();
    let r = ui.add_enabled(ready, egui::Button::new(egui::RichText::new(crate::i18n::tr("Create Catalog")).strong()).min_size(egui::vec2(0.0, 26.0)));
    crate::widgets::register(ui.ctx(), "button:welcomeCreate", r.rect);
    if r.clicked() {
        *action = Some(("catalog.new".into(), json!({"parent": parent.trim(), "name": name.trim(), "demo": *demo, "checkAtStartup": *updates})));
    }
    ui.separator();
    ui.label(egui::RichText::new(crate::i18n::tr("Open an existing catalog")).strong());
    // this app's catalogs only: another app's never open here
    let recent: Vec<PathBuf> = app
        .catalog_ui
        .recent()
        .existing()
        .into_iter()
        .filter(|p| lcc_catalog::library::resolve(p).is_ok_and(|d| lcc_catalog::library::foreign_format(&d).is_none()))
        .collect();
    for (i, p) in recent.iter().enumerate() {
        let r = ui.selectable_label(false, display_name(p));
        crate::widgets::register(ui.ctx(), format!("item:welcomeRecent{i}"), r.rect);
        if r.clicked() {
            *action = Some(("catalog.openRecent".into(), json!({"path": p, "checkAtStartup": *updates})));
        }
    }
    if !foreign.is_empty() {
        ui.label(dim("A library of another app was found here. {app} can't open its catalog format, and leaves it untouched:"));
        for f in foreign {
            ui.label(egui::RichText::new(f.as_str()).color(t.text_label));
        }
    }
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        let r = ui.add_enabled(app.services.pick_folder.is_some(), egui::Button::new(crate::i18n::tr("Open Catalog…")));
        crate::widgets::register(ui.ctx(), "button:welcomeOpen", r.rect);
        if r.clicked() {
            *action = Some(("catalog.open".into(), json!({"checkAtStartup": *updates})));
        }
        let r = ui.button(crate::i18n::tr("Quit"));
        crate::widgets::register(ui.ctx(), "button:welcomeQuit", r.rect);
        if r.clicked() {
            app.ui.quit = true;
        }
    });
}

fn done_message(cmd: &str, v: &Value) -> Option<String> {
    Some(match cmd {
        "catalog.export" => format!("{}: {}", crate::i18n::tr("Photos exported as a catalog"), v["photos"]),
        "catalog.import" if v["imported"] == true => crate::i18n::tr("Imported from the other catalog").to_string(),
        "catalog.import" => crate::i18n::tr("Nothing to import").to_string(),
        "catalog.backup" => format!("{} {}", crate::i18n::tr("Catalog backed up to"), v["backup"].as_str().unwrap_or("")),
        "catalog.checkIntegrity" if v["ok"] == true => crate::i18n::tr("The catalog passed the integrity test").to_string(),
        "catalog.checkIntegrity" => crate::i18n::tr("The catalog failed the integrity test: back it up and optimise it").to_string(),
        "catalog.optimize" => crate::i18n::tr("Catalog optimised").to_string(),
        _ => return None,
    })
}

fn text_row(ui: &mut egui::Ui, label: &str, value: &mut String, widget: &str) {
    ui.horizontal(|ui| {
        ui.add_sized([80.0, 20.0], egui::Label::new(crate::i18n::tr(label)));
        let r = ui.add(egui::TextEdit::singleline(value).desired_width(300.0));
        crate::widgets::register(ui.ctx(), widget, r.rect);
    });
}

/// A folder field with Choose… (the host's folder dialog, when it has one).
fn folder_row(app: &mut LccApp, ui: &mut egui::Ui, label: &str, value: &mut String, widget: &str, button: &str) {
    ui.horizontal(|ui| {
        ui.add_sized([80.0, 20.0], egui::Label::new(crate::i18n::tr(label)));
        let r = ui.add(egui::TextEdit::singleline(value).desired_width(230.0));
        crate::widgets::register(ui.ctx(), widget, r.rect);
        let r = ui.button(crate::i18n::tr("Choose…"));
        crate::widgets::register(ui.ctx(), button, r.rect);
        if r.clicked()
            && let Some(d) = app.services.pick_folder.as_mut().and_then(|f| f())
        {
            *value = d;
        }
    });
}

/// OK / Cancel; true when OK was clicked.
fn buttons(ui: &mut egui::Ui, ok: &str, widget: &str, close: &mut bool) -> bool {
    let mut clicked = false;
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let r = ui.button(crate::i18n::tr("Cancel"));
        crate::widgets::register(ui.ctx(), "button:catalogCancel", r.rect);
        if r.clicked() {
            *close = true;
        }
        let r = ui.button(egui::RichText::new(crate::i18n::tr(ok)).strong());
        crate::widgets::register(ui.ctx(), widget, r.rect);
        clicked = r.clicked();
    });
    clicked
}

fn chooser(app: &mut LccApp, ui: &mut egui::Ui, action: &mut Option<(String, Value)>, close: &mut bool) {
    let current = app.session.catalog_dir().map(Path::to_path_buf);
    let recent = app.catalog_ui.recent().existing();
    if recent.is_empty() {
        ui.label(crate::i18n::tr("No recent catalogs."));
    }
    for (i, p) in recent.iter().enumerate() {
        let open_now = current.as_deref().is_some_and(|c| lcc_catalog::library::resolve(p).ok().as_deref() == Some(c));
        let label = if open_now { format!("{} ({})", display_name(p), crate::i18n::tr("open")) } else { display_name(p) };
        let r = ui.selectable_label(false, label);
        crate::widgets::register(ui.ctx(), format!("item:recentCatalog{i}"), r.rect);
        if r.clicked() {
            if open_now {
                *close = true;
            } else {
                *action = Some(("catalog.openRecent".into(), json!({"path": p})));
            }
        }
    }
    ui.separator();
    let mut prompt = app.catalog_ui.recent().prompt_at_startup;
    let r = ui.checkbox(&mut prompt, crate::i18n::tr("Always show this when the app starts (or hold Alt while it opens)"));
    crate::widgets::register(ui.ctx(), "check:catalogPrompt", r.rect);
    if r.changed() {
        app.catalog_ui.recent().prompt_at_startup = prompt;
        app.catalog_ui.save_recent();
    }
    ui.horizontal(|ui| {
        let r = ui.button(crate::i18n::tr("New Catalog…"));
        crate::widgets::register(ui.ctx(), "button:chooserNew", r.rect);
        if r.clicked() {
            *action = Some(("catalog.new".into(), json!({})));
        }
        let r = ui.button(crate::i18n::tr("Open Another Catalog…"));
        crate::widgets::register(ui.ctx(), "button:chooserOpen", r.rect);
        if r.clicked() {
            *action = Some(("catalog.open".into(), json!({})));
        }
        let r = ui.button(crate::i18n::tr("Continue"));
        crate::widgets::register(ui.ctx(), "button:chooserContinue", r.rect);
        if r.clicked() {
            *close = true;
        }
    });
}

const SCHEDULES: &[(&str, &str)] = &[
    ("never", "Never"),
    ("everyExit", "Every time the app exits"),
    ("daily", "Once a day, when exiting"),
    ("weekly", "Once a week, when exiting"),
    ("monthly", "Once a month, when exiting"),
];
const DISCARD: &[(&str, &str)] = &[("never", "Never"), ("day", "After one day"), ("week", "After one week"), ("month", "After 30 days")];
const AT_IMPORT: &[(&str, &str)] = &[("minimal", "Minimal"), ("standard", "Standard"), ("full", "1:1")];
const EDGES: &[u64] = &[1024, 1440, 1680, 2048, 2560, 3840, 5120];

fn combo(ui: &mut egui::Ui, id: &str, label: &str, value: &mut Value, options: &[(&str, &str)]) {
    ui.horizontal(|ui| {
        ui.add_sized([150.0, 20.0], egui::Label::new(crate::i18n::tr(label)));
        let cur = value.as_str().unwrap_or("").to_string();
        let shown = options.iter().find(|(k, _)| *k == cur).map_or(cur.as_str(), |(_, l)| l);
        let r = egui::ComboBox::from_id_salt(id).selected_text(crate::i18n::tr(shown)).width(220.0).show_ui(ui, |ui| {
            for (k, l) in options {
                if ui.selectable_label(cur == *k, crate::i18n::tr(l)).clicked() {
                    *value = json!(k);
                }
            }
        });
        crate::widgets::register(ui.ctx(), format!("combo:{id}"), r.response.rect);
    });
}

fn settings_ui(app: &mut LccApp, ui: &mut egui::Ui, s: &mut Value, action: &mut Option<(String, Value)>, close: &mut bool) {
    ui.label(egui::RichText::new(crate::i18n::tr("Backup")).strong());
    combo(ui, "catalogBackup", "Back up catalog", &mut s["backup"], SCHEDULES);
    let mut dir = s["backupDir"].as_str().unwrap_or("").to_string();
    folder_row(app, ui, "Backup folder", &mut dir, "field:catalogBackupDir", "button:catalogBackupDir");
    s["backupDir"] = json!(dir);
    for (key, label, widget) in [
        ("testIntegrity", "Test integrity before backing up", "check:catalogIntegrity"),
        ("optimize", "Optimise catalog after backing up", "check:catalogOptimize"),
    ] {
        let mut b = s[key].as_bool().unwrap_or(false);
        let r = ui.checkbox(&mut b, crate::i18n::tr(label));
        crate::widgets::register(ui.ctx(), widget, r.rect);
        s[key] = json!(b);
    }
    if let Some(last) = s["lastBackup"].as_i64() {
        let when = lcc_catalog::dates::civil(last);
        ui.label(egui::RichText::new(format!("{} {}", crate::i18n::tr("Last backup:"), when.replace('T', " "))).weak());
    }
    ui.horizontal(|ui| {
        for (cmd, label) in [("catalog.backup", "Back Up Now"), ("catalog.checkIntegrity", "Test Integrity"), ("catalog.optimize", "Optimise")] {
            let r = ui.button(crate::i18n::tr(label));
            crate::widgets::register(ui.ctx(), format!("button:{cmd}"), r.rect);
            if r.clicked() {
                *action = Some((cmd.into(), json!({})));
            }
        }
    });
    ui.separator();
    ui.label(egui::RichText::new(crate::i18n::tr("Previews")).strong());
    ui.horizontal(|ui| {
        ui.add_sized([150.0, 20.0], egui::Label::new(crate::i18n::tr("Standard preview size")));
        let cur = s["previews"]["standardEdge"].as_u64().unwrap_or(2048);
        let r = egui::ComboBox::from_id_salt("catalogEdge").selected_text(format!("{cur} px")).width(220.0).show_ui(ui, |ui| {
            for e in EDGES {
                if ui.selectable_label(cur == *e, format!("{e} px")).clicked() {
                    s["previews"]["standardEdge"] = json!(e);
                }
            }
        });
        crate::widgets::register(ui.ctx(), "combo:catalogEdge", r.response.rect);
    });
    combo(ui, "catalogDiscard", "Discard 1:1 previews", &mut s["previews"]["discardFull"], DISCARD);
    combo(ui, "catalogAtImport", "Build at import", &mut s["previews"]["atImport"], AT_IMPORT);
    if buttons(ui, "OK", "button:catalogSettingsOk", close) {
        // the default folder stays "the default" (it moves with the catalog)
        if s["backupDir"] == s["defaultBackupDir"] {
            s["backupDir"] = json!("");
        }
        let p = &s["previews"];
        *action = Some((
            "catalog.settings".into(),
            json!({
                "backup": s["backup"], "backupDir": s["backupDir"], "testIntegrity": s["testIntegrity"], "optimize": s["optimize"],
                "previews": {"standardEdge": p["standardEdge"], "discardFull": p["discardFull"], "atImport": p["atImport"]},
            }),
        ));
    }
}

fn import_preview(ui: &mut egui::Ui, v: &Value, rule: &mut String, t: &crate::theme::Tokens) {
    let n = |k: &str| v[k].as_array().map_or(0, Vec::len);
    let rows = [
        (crate::i18n::tr("New photos"), n("newPhotos").to_string()),
        (crate::i18n::tr("Changed photos"), n("changed").to_string()),
        (crate::i18n::tr("  with changed settings"), v["changedSettings"].to_string()),
        (crate::i18n::tr("  with changed metadata"), v["changedMetadata"].to_string()),
        (crate::i18n::tr("Unchanged photos"), v["unchanged"].to_string()),
        (crate::i18n::tr("New albums"), n("newAlbums").to_string()),
        (crate::i18n::tr("Albums photos are added to"), n("extendedAlbums").to_string()),
    ];
    egui::Grid::new("import-catalog-preview").num_columns(2).show(ui, |ui| {
        for (k, v) in rows {
            ui.label(egui::RichText::new(k).color(t.text_label));
            ui.label(v);
            ui.end_row();
        }
    });
    if let Some(changed) = v["changed"].as_array().filter(|c| !c.is_empty()) {
        egui::ScrollArea::vertical().max_height(120.0).show(ui, |ui| {
            for c in changed.iter().take(200) {
                let what = match (c["settingsDiffer"] == true, c["metadataDiffer"] == true) {
                    (true, true) => "settings, metadata",
                    (true, false) => "settings",
                    _ => "metadata",
                };
                ui.label(egui::RichText::new(format!("{} · {}", c["fileName"].as_str().unwrap_or(""), crate::i18n::tr(what))).weak());
            }
        });
    }
    let rules = [
        ("keep", "Nothing (keep this catalog's version)"),
        ("replaceSettings", "Develop settings only"),
        ("replaceMetadata", "Metadata only"),
        ("replaceSettingsAndMetadata", "Metadata and develop settings"),
    ];
    ui.label(egui::RichText::new(crate::i18n::tr("For changed photos, replace:")).strong());
    for (k, l) in rules {
        let r = ui.radio_value(rule, k.to_string(), crate::i18n::tr(l));
        crate::widgets::register(ui.ctx(), format!("radio:importRule.{k}"), r.rect);
    }
}
