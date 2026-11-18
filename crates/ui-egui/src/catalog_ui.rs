//! Library-level UI (P1.5): the `catalog.*` UI commands that create and open libraries, Library
//! Settings, Export as Library, Import from Another Library with its change preview, the
//! Read / Save Metadata conflict question, and the welcome dialog of a first start (no library
//! yet: create one, or open one). The ids keep their older `catalog` name for agents and scripts.
//!
//! One recent list and one startup choice serve every way in (issue #871): Settings ▸ General ▸
//! At launch, and `settings.recent_libraries`, which File ▸ Libraries and the launch list
//! (`panels::library_problem`) show. `catalog.chooser` opens that launch list. A recent list
//! kept by older versions in `recent-catalogs.json` is carried over once
//! ([`carry_over_recent_catalogs`]).
//!
//! The work is done by engine commands (`catalog.*`, `photo.xmpStatus`, `library.previewSettings`);
//! this module only holds the dialogs and the UI commands that switch libraries.

use std::path::{Path, PathBuf};

use lcc_catalog::library::RecentCatalogs;
use serde_json::{Value, json};

use crate::LccApp;
use crate::pick::Picked;
use crate::state::{AppSettings, LaunchLibrary};

/// The library dialogs (one at a time, in their own window).
#[derive(Clone, Debug, PartialEq)]
pub enum CatalogDialog {
    /// First start, no library to open yet: create one (name, location, sample photos or not) or
    /// open one. `foreign`: libraries of another app found where this app's would be (e.g. the
    /// upstream app's), which can't be opened here; named so the user knows why they don't open.
    /// `updates`: the user ticked "Check for updates when the app starts" (unticked at first).
    /// Each action carries it as `checkAtStartup`, and it is saved through `update.prefs` only
    /// when that action opens a library; closing the dialog saves nothing.
    Welcome {
        parent: String,
        name: String,
        demo: bool,
        updates: bool,
        foreign: Vec<String>,
    },
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
    /// Refine Photos asks first: how many photos of the view it unflags and rejects.
    Refine {
        picks: usize,
        unflagged: usize,
    },
}

/// Library UI state on [`LccApp`].
#[derive(Default)]
pub struct CatalogUi {
    pub dialog: Option<CatalogDialog>,
    /// Files from the command line of a first start, imported once a library is open.
    pub pending_import: Vec<String>,
    /// Why the welcome dialog's "Check for updates when the app starts" couldn't be turned on once
    /// its library opened: said in a toast on the next frame.
    pub update_error: Option<String>,
}

/// The welcome dialog: a new library under the Pictures folder by default.
pub fn welcome(app: &mut LccApp, foreign: Vec<String>) {
    let parent = lcc_engine::library::default_dir()
        .and_then(|d| d.parent().map(Path::to_path_buf))
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_default();
    app.catalog_ui.dialog =
        Some(CatalogDialog::Welcome { parent, name: lcc_brand::LIBRARY_DEFAULT.to_string(), demo: false, updates: false, foreign });
}

/// What [`carry_over_recent_catalogs`] did.
#[derive(Debug, PartialEq)]
pub enum CarryOver {
    /// No `recent-catalogs.json`: nothing to carry over.
    Nothing,
    /// Its libraries that still exist were added to the recent list (`added` of them were new), and
    /// its "always show" choice became At launch: *Ask me* (`ask`). The file is still there: set it
    /// aside ([`set_aside_recent_catalogs`]) once the settings are saved.
    Carried { added: usize, ask: bool },
    /// It couldn't be read or understood: left out, with this reason in the log.
    Skipped(String),
}

/// Carry the recent list of older versions (`recent-catalogs.json`, behind File ▸ Open Recent
/// Catalog and the catalog chooser) over into the one recent list, once: the libraries it names
/// that still exist go at the end of `settings.recent_libraries` (no duplicates, at most
/// [`crate::state::RECENT_LIBRARIES`]), and its "Always show this when the app starts" turns At
/// launch into *Ask me*. A file that can't be read or parsed is never an error: it is logged and
/// (if it can be) set aside, and the settings are unchanged. The host calls this before it decides
/// what to open at launch, saves `settings`, and only then sets a carried-over file aside
/// ([`set_aside_recent_catalogs`]), so a library removed from the list later doesn't come back, and
/// a start that ends before the save carries it over again (adding nothing twice).
pub fn carry_over_recent_catalogs(settings: &mut AppSettings, file: &Path) -> CarryOver {
    let bytes = match std::fs::read(file) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return CarryOver::Nothing,
        Err(e) => {
            let why = format!("{}: can't be read: {e}", file.display());
            log::warn!("recent catalogs: {why}; not carried over");
            return CarryOver::Skipped(why);
        }
    };
    let old: RecentCatalogs = match serde_json::from_slice(&bytes) {
        Ok(r) => r,
        Err(e) => {
            let why = format!("{}: not a recent-catalogs list: {e}", file.display());
            log::warn!("recent catalogs: {why}; not carried over");
            set_aside_recent_catalogs(file);
            return CarryOver::Skipped(why);
        }
    };
    let mut added = 0;
    for path in &old.paths {
        if settings.recent_libraries.len() >= crate::state::RECENT_LIBRARIES {
            break;
        }
        // an entry point or a folder; this app's libraries only, and only those still there
        let Ok(dir) = lcc_catalog::library::resolve(path) else { continue };
        if lcc_catalog::library::foreign_format(&dir).is_some() || settings.recent_libraries.iter().any(|r| Path::new(r) == dir) {
            continue;
        }
        settings.recent_libraries.push(dir.to_string_lossy().into_owned());
        added += 1;
    }
    if old.prompt_at_startup {
        settings.at_launch = LaunchLibrary::Ask;
    }
    CarryOver::Carried { added, ask: old.prompt_at_startup }
}

/// Rename a carried-over (or unreadable) `recent-catalogs.json` to `<file>.carried-over`, out of
/// the way of later starts; a failure is logged.
pub fn set_aside_recent_catalogs(file: &Path) {
    let mut name = file.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".carried-over");
    if let Err(e) = std::fs::rename(file, file.with_file_name(name)) {
        log::warn!("recent catalogs: can't set {} aside: {e}", file.display());
    }
}

/// Open a library chosen by the user (catalog entry point or folder): validated first, so a folder
/// that isn't a library is an error rather than a new, empty one. Opening puts it at the top of
/// the recent list (`panels::settings::open_library`).
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

/// Open the library in `dir`; with `checkAtStartup` in `p`, the start-up update check is turned
/// on (through `update.prefs`) once it is open.
fn open_dir(app: &mut LccApp, dir: &Path, demo: bool, p: &Value) -> Result<Value, String> {
    let r = crate::panels::settings::open_library(app, &json!({"path": dir.to_string_lossy(), "demo": demo}))?;
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
pub(crate) fn pick_folder(app: &mut LccApp, command: &str, p: &Value, key: &'static str, title: &str) -> Result<Option<String>, String> {
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
                    crate::panels::settings::forget_library(&mut app.ui.settings, path);
                    app.recent_checks.forget(path);
                }
                r
            }
            None => Ok(json!({"recent": recent_existing(app)})),
        },
        "catalog.welcome" => {
            welcome(app, Vec::new());
            Ok(Value::Null)
        }
        // the launch list, over the library open now
        "catalog.chooser" => crate::panels::library_problem::offer_list(app).map(|()| Value::Null),
        // Settings ▸ General ▸ At launch: *Ask me* (on) or the last library (off)
        "catalog.promptAtStartup" => {
            let on = p.get("on").and_then(Value::as_bool).unwrap_or(app.ui.settings.at_launch != LaunchLibrary::Ask);
            app.ui.settings.at_launch = if on { LaunchLibrary::Ask } else { LaunchLibrary::Last };
            Ok(json!({"promptAtStartup": on, "atLaunch": app.ui.settings.at_launch}))
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
        // a library in the catalog format of this app's earlier name (`crate::legacy`)
        crate::legacy::IMPORT_LIBRARY_ID => crate::legacy::import_library(app, id, p),
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
        "library.refine" if !p.get("confirmed").and_then(Value::as_bool).unwrap_or(false) => return refine_question(app),
        _ => return None,
    })
}

/// Refine Photos changes many flags at once: say how many first (nothing to change: no question).
fn refine_question(app: &mut LccApp) -> Option<Result<Value, String>> {
    let ids = app.session.visible_cloned();
    let flag_of = |id: &lcc_catalog::PhotoId| app.session.catalog.photo(*id).map(|p| p.flag);
    let picks = ids.iter().filter(|id| flag_of(id) == Some(lcc_catalog::Flag::Pick)).count();
    let unflagged = ids.iter().filter(|id| flag_of(id) == Some(lcc_catalog::Flag::None)).count();
    if picks + unflagged == 0 {
        return None;
    }
    app.catalog_ui.dialog = Some(CatalogDialog::Refine { picks, unflagged });
    Some(Ok(json!({"confirm": "dialog", "picks": picks, "unflagged": unflagged})))
}

/// Read / Save Metadata: ask first when it would overwrite newer changes on the other side
/// (Read: changes made in the library; Save: changes another app made to the file).
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
                let dir = entry.parent().map(Path::to_path_buf).ok_or("library without a folder")?;
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
        "dialog.importCatalog" | crate::legacy::IMPORT_LIBRARY_ID => !busy && app.services.pick_folder.is_some(),
        "dialog.catalogSettings" => app.session.catalog_dir().is_some(),
        "dialog.exportCatalog" => !app.session.catalog.is_empty(),
        _ => return None,
    })
}

/// The recent libraries that are still there (`catalog.openRecent` without a path), newest first.
fn recent_existing(app: &LccApp) -> Vec<String> {
    app.ui.settings.recent_libraries.iter().filter(|p| Path::new(p).exists()).cloned().collect()
}

fn display_name(p: &Path) -> String {
    let dir = if p.is_file() { p.parent().unwrap_or(p) } else { p };
    let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| dir.display().to_string());
    format!("{name}: {}", dir.parent().map(|d| d.display().to_string()).unwrap_or_default())
}

/// Whether Open Library…'s folder dialog (of the welcome dialog) is still up.
fn picking_catalog(app: &LccApp) -> bool {
    app.pending_picks.iter().any(|p| p.command == "catalog.open")
}

/// Each frame: the dialog, if any. (Alt held as the window appears is the launch list's:
/// `panels::library_problem`.)
pub fn show(app: &mut LccApp, ctx: &egui::Context) {
    if let Some(e) = app.catalog_ui.update_error.take() {
        app.toast_error(ctx, format!("{}: {e}", crate::i18n::tr("Checking for updates when the app starts is still off")));
    }
    let Some(mut dlg) = app.catalog_ui.dialog.clone() else { return };
    let welcoming = matches!(dlg, CatalogDialog::Welcome { .. });
    // a dialog opened over the welcome card (Help ▸ Check for Updates, About, Settings…) comes
    // first: the card stands aside until it closes, so it never covers that dialog (issue #929)
    if welcoming && app.ui.dialog.is_some() {
        return;
    }
    if welcoming {
        // nothing behind it can be used before a library is open
        crate::panels::library_problem::dim(ctx);
    }
    let title = match &dlg {
        CatalogDialog::Welcome { .. } => "Welcome to {app}",
        CatalogDialog::NewCatalog { .. } => "New Catalog",
        CatalogDialog::Settings { .. } => "Catalog Settings",
        CatalogDialog::ExportCatalog { .. } => "Export as Catalog",
        CatalogDialog::ImportCatalog { .. } => "Import from Another Catalog",
        CatalogDialog::XmpConflict { command, .. } if command == "photo.readMetadataFromFile" => "Read Metadata from File",
        CatalogDialog::XmpConflict { .. } => "Save Metadata to File",
        CatalogDialog::Refine { .. } => "Refine Photos",
    };
    let mut close = false;
    let mut action: Option<(String, Value)> = None;
    let t = crate::theme::Tokens::get(ctx);
    let frame = crate::dialog_frame::card_frame(ctx);
    let window_id = egui::Id::new("catalog-dialog");
    let shown = egui::Window::new(crate::i18n::tr(title))
        .id(window_id)
        .title_bar(false)
        .order(if welcoming { egui::Order::Foreground } else { egui::Order::Middle })
        .collapsible(false)
        .resizable(false)
        .frame(frame)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .default_width(440.0)
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            crate::dialog_frame::card_header(ui, crate::i18n::tr(title), None);
            let dim = |s: &str| egui::RichText::new(crate::i18n::tr(s)).color(t.text_dim);
            // capped to the screen; the buttons are part of each variant's content, so they scroll
            // with it in a window too short for the whole dialog
            let mut layout = crate::dialog_frame::ModalLayout::begin(ui, window_id);
            layout.body(ui, |ui| match &mut dlg {
                CatalogDialog::Welcome { parent, name, demo, updates, foreign } => {
                    welcome_ui(app, ui, &t, parent, name, demo, updates, foreign, &mut action);
                }
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
                CatalogDialog::Refine { picks, unflagged } => {
                    ui.label(crate::i18n::tr("Refine Photos unflags the picked photos in view and rejects the unflagged ones. Rejected photos stay as they are."));
                    ui.label(egui::RichText::new(crate::i18n::tr_format!("Picks to unflag: {n}", n = picks)).strong());
                    ui.label(egui::RichText::new(crate::i18n::tr_format!("Unflagged photos to reject: {n}", n = unflagged)).strong());
                    if buttons(ui, "Refine", "button:refineConfirm", &mut close) {
                        action = Some(("library.refine".into(), json!({"confirmed": true})));
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
    // the welcome dialog stays until a library is open (or the app quits)
    if !welcoming && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        close = true;
    }
    if close {
        app.catalog_ui.dialog = None;
    }
    if let Some((cmd, params)) = action {
        // the command may open the next dialog; on an error, or for the Library Settings buttons,
        // this one stays
        let prev = app.catalog_ui.dialog.take();
        let r = app.run(&cmd, params);
        // the welcome dialog also stays (waiting) while its Open Library… folder dialog is up and
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
    // a first start's command-line files, once a library is open
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
    // Open Library…'s folder dialog is up: everything here waits for it
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
    // this app's libraries only: another app's never open here
    let recent: Vec<PathBuf> = recent_existing(app)
        .into_iter()
        .map(PathBuf::from)
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
        ui.label(dim("A catalog of another app was found here. {app} can't open its format, and leaves it untouched:"));
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
        // Back Up, Test Integrity and Optimise say how they went wherever they run from (`LccApp::run_typed`)
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
        ("optimize", "Optimize catalog after backing up", "check:catalogOptimize"),
    ] {
        let mut b = s[key].as_bool().unwrap_or(false);
        let r = ui.checkbox(&mut b, crate::i18n::tr(label));
        crate::widgets::register(ui.ctx(), widget, r.rect);
        s[key] = json!(b);
    }
    if let Some(last) = s["lastBackup"].as_i64() {
        let when = lcc_catalog::dates::local(last).unwrap_or_else(|| lcc_catalog::dates::civil(last));
        let when = when.get(..19).unwrap_or(&when).replace('T', " ");
        ui.label(egui::RichText::new(format!("{} {}", crate::i18n::tr("Last backup:"), when)).weak());
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
        // the default folder stays "the default" (it moves with the library)
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
        (crate::i18n::tr("New collections"), n("newAlbums").to_string()),
        (crate::i18n::tr("Collections photos are added to"), n("extendedAlbums").to_string()),
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
