//! The two workspace tabs, Import and Library (OneSlips, LR-IMP-WINDOW), with Settings ▸ Import ▸
//! "Import as a tab" (on by default; off, Import… opens the import review as before).
//!
//! **Import** browses a folder or card without adding it (the Local view of a folder), for culling
//! with the grid and the loupe: the sources on the left, a bar over the photos (where they are,
//! what the big button takes: Picks / Selected / All, and the button), the import settings on the
//! right. **Library** is the catalog, where everything imported is edited.
//!
//! Browsed photos are only looked at: nothing here writes into, moves or deletes their files (the
//! engine refuses deleting or renaming them). Importing hands the files to `library.import`, in the
//! background (the activity stack shows it), switches to the Library tab where they come in, and
//! then builds their previews unattended (`library.finishImport`).
//!
//! The layout follows OneSlips' Import tab; the look is this app's own (its tokens, buttons and
//! cards). On a narrow window (Compact, Modern on a phone) the three columns become one, with the
//! sources and the settings a tap away in the bar. Ported by hand from OneSlips dd7bb41aa,
//! db061a0a7, 437ed003f, 6e4fa44d0, 12d31e4fc.

use egui::{Align2, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::LccApp;
use crate::theme::Tokens;
use crate::widgets::register;
use lcc_engine::LibrarySource;
pub use lcc_engine::cmd::import_tab::Take;

/// The workspace tab on show.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Tab {
    Import,
    #[default]
    Library,
}

/// The Import tab's settings: what the big button takes, and what the import does to the photos
/// (the import review's options, with the preset stack), kept between sessions.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ImportSettings {
    /// `picks` | `selected` | `all`: what the big button imports.
    pub take: String,
    /// `none` | `standard` | `full`: previews built after the import.
    pub previews: String,
    /// The import preset these settings came from ("" = none).
    pub preset_name: String,
    /// Transfer, destination, renaming, album, presets, keywords, metadata (never files: the
    /// folder's photos are the Import tab's).
    pub opts: crate::import::ImportDialog,
}

impl Default for ImportSettings {
    fn default() -> Self {
        // add in place, as the review starts: copying is a choice the user makes
        ImportSettings { take: "picks".into(), previews: "standard".into(), preset_name: String::new(), opts: Default::default() }
    }
}

impl ImportSettings {
    pub fn take(&self) -> Take {
        Take::parse(&self.take).unwrap_or(Take::Picks)
    }

    /// These settings from an import preset's (unknown fields keep their defaults).
    pub fn from_preset(name: &str, v: &Value) -> ImportSettings {
        let mut opts: crate::import::ImportDialog = serde_json::from_value(v.clone()).unwrap_or_default();
        opts.fold_preset();
        let previews = v.get("previews").and_then(Value::as_str).filter(|p| matches!(*p, "none" | "standard" | "full")).unwrap_or("standard");
        ImportSettings { take: "picks".into(), previews: previews.to_string(), preset_name: name.to_string(), opts: settings_only(opts) }
    }

    /// What an import preset keeps of these settings (not what to take, nor any file).
    pub fn to_preset(&self) -> Value {
        let mut v = serde_json::to_value(settings_only(self.opts.clone())).unwrap_or_default();
        if let Some(o) = v.as_object_mut() {
            for k in ["candidates", "checked", "trashed", "sources", "onDeleted", "recipe", "recipeName", "immich"] {
                o.remove(k);
            }
            o.insert("previews".into(), json!(self.previews));
        }
        v
    }
}

/// The options of `d` without the files a review scanned.
pub fn settings_only(mut d: crate::import::ImportDialog) -> crate::import::ImportDialog {
    d.candidates.clear();
    d.checked.clear();
    d.trashed.clear();
    d.sources.clear();
    d.recipe.clear();
    d.immich = false;
    d
}

/// Whether Import is a tab (Settings ▸ Import ▸ Import as a tab). Browsing folders needs a file
/// system: the web build keeps the import review.
pub fn enabled(app: &LccApp) -> bool {
    app.ui.settings.import_as_tab && !cfg!(target_arch = "wasm32")
}

/// Whether the Import tab is on show.
pub fn showing(app: &LccApp) -> bool {
    enabled(app) && app.ui.tab == Tab::Import
}

/// Entering Import: put the Library's filter aside (as Library ▸ Enable Filters off does). A card
/// or folder being culled for import must show all of its files, and the tab has no filter bar to
/// clear one with. The filter comes back when the Library tab does ([`leave`]).
pub(crate) fn enter(app: &mut LccApp) {
    app.ui.import_develop = false;
    if !app.session.filters_off() && app.session.filter != lcc_catalog::Filter::default() && app.run("library.toggleFilters", json!({})).is_ok() {
        app.ui.import_shelved_filter = true;
    }
}

/// Leaving Import: bring back the filter the tab put aside — unless something else already
/// touched the filter state (cleared, or toggled back on by hand), which wins.
pub(crate) fn leave(app: &mut LccApp) {
    if app.ui.import_shelved_filter && app.session.filters_off() {
        let _ = app.run("library.toggleFilters", json!({}));
    }
    app.ui.import_shelved_filter = false;
}

/// The open library's name, as the Import button names it.
pub fn library_name(app: &LccApp) -> String {
    app.session
        .library
        .as_ref()
        .filter(|l| l.on_disk)
        .and_then(|l| l.dir.file_stem().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_else(|| crate::i18n::tr("Library").to_string())
}

/// Show a tab (`view.importTab` / `view.libraryTab`). Import shows the folder browsed last (or
/// the sources to choose one from); Library goes back to the view it had.
pub fn switch(app: &mut LccApp, tab: Tab) -> Result<Value, String> {
    if !enabled(app) {
        return Err(crate::i18n::tr("Import is not a tab: turn on Settings ▸ Import ▸ Import as a tab").to_string());
    }
    if app.ui.tab == tab {
        return Ok(json!({"tab": tab}));
    }
    app.ui.tab = tab;
    match tab {
        Tab::Import => enter(app),
        Tab::Library => leave(app),
    }
    match tab {
        Tab::Import => {
            if app.session.source != LibrarySource::Folder {
                app.ui.library_source = Some(app.session.source);
            }
            // culling happens in Library's grid and loupe
            app.ui.module = crate::module::ModuleId::Library;
            if !matches!(app.ui.view, crate::state::ViewMode::PhotoGrid | crate::state::ViewMode::SquareGrid | crate::state::ViewMode::Detail) {
                app.ui.view = app.ui.grid_style;
            }
            if let Some(b) = app.session.browse.clone()
                && std::path::Path::new(&b.path).is_dir()
                && app.session.source != LibrarySource::Folder
            {
                let _ = crate::import::browse(app, &b.path, Some(b.subfolders), None);
            }
        }
        Tab::Library => {
            // developing a browsed photo without importing it keeps the folder view on: the grid
            // comes back to the Browse tab when it ends ([`keep_in_step`])
            if app.session.source == LibrarySource::Folder && !app.ui.import_develop {
                let back = app.ui.library_source.take().filter(|s| *s != LibrarySource::Folder).unwrap_or(LibrarySource::All);
                app.session.source = back;
                app.session.refresh_view();
                app.session.keep_active_in_view();
            }
        }
    }
    app.ui.import_was_browsing = app.session.source == LibrarySource::Folder;
    Ok(json!({"tab": tab}))
}

/// Browsed photos show only in the Import tab, as in Lightroom (OneSlips db061a0a7): a browsed
/// folder on show, however it got there (the Local panel, a command, an agent, the view a library
/// reopens on), brings up the Import tab; the view leaving the folder (All Photos, an album…)
/// brings back the Library tab. Checked every frame. Off (or on the web), the Library tab is all
/// there is.
pub fn keep_in_step(app: &mut LccApp) {
    if !enabled(app) {
        app.ui.tab = Tab::Library;
        leave(app);
        return;
    }
    // Show Alternatives looks at a folder too, but in the Library tab, under its own bar
    if app.session.alternatives.is_some() {
        app.ui.tab = Tab::Library;
        app.ui.import_was_browsing = false;
        leave(app);
        return;
    }
    let browsing = app.session.source == LibrarySource::Folder;
    // "Develop without Import" (and Print, Export, Slideshow): the Library tab shows the module on
    // the browsed folder's photos, the folder view (and so the Browse tab) waiting until it ends
    let developing = app.ui.import_develop && acts_in_place(app.ui.module);
    if !browsing || !developing {
        app.ui.import_develop = browsing && developing;
    }
    if browsing && app.ui.tab != Tab::Import && !developing {
        app.ui.tab = Tab::Import;
        app.ui.module = crate::module::ModuleId::Library;
        enter(app);
    } else if !browsing && app.ui.import_was_browsing && app.ui.tab == Tab::Import {
        app.ui.tab = Tab::Library;
        leave(app);
    }
    app.ui.import_was_browsing = browsing;
}

/// The big button: import what the settings take from the browsed folder, in the background,
/// then show the Library tab as the photos come in (`import.start {what?}`).
pub fn start(app: &mut LccApp, p: &Value) -> Result<Value, String> {
    if app.session.source != LibrarySource::Folder {
        return Err(crate::i18n::tr("Choose a folder or card to import from first").to_string());
    }
    crate::import::busy_synchronizing(app)?;
    if app.import.as_ref().is_some_and(|t| !t.is_browse()) || app.scan.as_ref().is_some_and(|t| !t.browse) {
        return Err(crate::i18n::tr("An import is running").to_string());
    }
    if let Some(take) = p.get("what").and_then(Value::as_str) {
        if Take::parse(take).is_none() {
            return Err(format!("unknown what `{take}` (picks|selected|all)"));
        }
        app.ui.settings.import_tab.take = take.to_string();
    }
    let settings = app.ui.settings.import_tab.clone();
    let take = settings.take();
    // a folder still being read: what is in so far is what there is
    let paths = app.session.import_paths(take);
    if paths.is_empty() {
        return Err(crate::i18n::tr(match take {
            Take::Picks => "No picks to import: flag the photos you want with P first",
            Take::Selected => "No photos selected to import",
            Take::All => "Every photo here is in the catalog already",
        })
        .to_string());
    }
    let mut params = settings.opts.params()?;
    // a new album named in the settings is made with the first batch (a later batch joins it)
    if settings.opts.album.is_none() && !settings.opts.new_album.trim().is_empty() {
        params["albumName"] = json!(settings.opts.new_album.trim());
    }
    // checked now, not halfway through: an unknown preset or template refuses the import
    let mut check = params.clone();
    check["paths"] = json!(paths.first().cloned().into_iter().collect::<Vec<_>>());
    lcc_engine::cmd::library::import_params(&app.session, &check).map_err(|e| e.to_string())?;
    // a browse still reading the folder stops: its files are what is imported now
    if app.import.as_ref().is_some_and(crate::import::ImportTask::is_browse) {
        app.import = None;
    }
    if let Some(t) = app.scan.take() {
        t.progress.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    let n = paths.len();
    let undo0 = app.session.undo.len();
    let mut task = crate::import::ImportTask::new(paths, params, undo0, false);
    task.finish_previews = Some(settings.previews.clone());
    app.import = Some(task);
    app.renderer.forget_imports();
    // the Library tab, where the photos come in (Recently Added shows them as they arrive)
    app.ui.tab = Tab::Library;
    leave(app);
    app.ui.library_source = None;
    app.session.source = LibrarySource::RecentlyAdded;
    app.session.refresh_view();
    app.ui.import_was_browsing = false;
    Ok(json!({"importing": n, "into": library_name(app)}))
}

/// Develop the browsed folder's active photo without importing it (`import.develop`): the
/// Library tab's Develop module on the local photo, the folder view kept on so returning to the
/// Browse tab (or any change of source) brings it back. The edits go to the photo's XMP sidecar
/// like any photo added in place; the library catalog stays untouched.
pub fn develop_without_import(app: &mut LccApp) -> Result<Value, String> {
    open_without_import(app, crate::module::ModuleId::Develop)
}

/// The modules that work on browsed photos where they are, without importing them: Develop, Print,
/// Export and Slideshow act on the photo (or the selection) itself. Map, Book and Web are about the
/// library (its places, its collections), so they leave the folder for the library.
pub fn acts_in_place(m: crate::module::ModuleId) -> bool {
    use crate::module::ModuleId;
    matches!(m, ModuleId::Develop | ModuleId::Print | ModuleId::Export | ModuleId::Slideshow)
}

/// Open module `to` on the Import tab's selection without importing it (Develop, Print, Export…
/// from the Browse tab: the module bar, a shortcut, an agent). The browsed photos stay browsed:
/// the Library tab shows the module with the folder view kept on, and going back to Library (or
/// to the Browse tab) brings the folder back. When everything selected is in the library already
/// (Show All shows those files too), the module opens on the library's own photos instead.
pub fn open_without_import(app: &mut LccApp, to: crate::module::ModuleId) -> Result<Value, String> {
    if app.session.source != LibrarySource::Folder {
        return Err(crate::i18n::tr("Choose a folder or card to browse first").to_string());
    }
    let Some(active) = app.session.active() else {
        return Err(crate::i18n::tr("Select a photo to develop").to_string());
    };
    let selected = app.session.selected();
    let in_library = !selected.is_empty() && selected.iter().all(|id| app.session.catalog.photo(*id).is_some_and(|p| p.in_library()));
    if in_library {
        // the library's own photos: the Library tab on the whole library, the same selection
        app.ui.import_develop = false;
        app.ui.tab = Tab::Library;
        leave(app);
        app.ui.library_source = None;
        app.session.source = LibrarySource::All;
        app.session.refresh_view();
        app.ui.import_was_browsing = false;
        let ids: Vec<u64> = selected.iter().map(|id| id.0).collect();
        app.run("library.select", json!({"ids": ids, "active": active.0}))?;
        crate::module::switch(app, to)?;
        return Ok(json!({"tab": "library", "module": to.key(), "photo": app.session.active(), "inLibrary": true}));
    }
    app.ui.import_develop = true;
    app.ui.import_was_browsing = true;
    leave(app);
    app.ui.tab = Tab::Library;
    crate::module::switch(app, to)?;
    Ok(json!({"tab": "library", "module": to.key(), "photo": app.session.active(), "inLibrary": false}))
}

/// Leave browsed photos opened without import (see [`open_without_import`]) for the library: a module
/// that is about the library (Map, Book, Web) was chosen from one of them.
pub(crate) fn end_without_import(app: &mut LccApp) {
    app.ui.import_develop = false;
    app.ui.import_was_browsing = false;
    if app.session.source == LibrarySource::Folder {
        let back = app.ui.library_source.take().filter(|s| *s != LibrarySource::Folder).unwrap_or(LibrarySource::All);
        app.session.source = back;
        app.session.refresh_view();
        app.session.keep_active_in_view();
    }
}

/// Apply an import preset to the Import tab's settings (`import.usePreset {name}`).
pub fn use_preset(app: &mut LccApp, name: &str) -> Result<Value, String> {
    let Some(p) = app.session.import_presets.iter().find(|p| p.name.eq_ignore_ascii_case(name)).cloned() else {
        return Err(format!("no import preset `{name}`"));
    };
    let take = app.ui.settings.import_tab.take.clone();
    app.ui.settings.import_tab = ImportSettings::from_preset(&p.name, &p.settings);
    app.ui.settings.import_tab.take = take;
    Ok(json!({"name": p.name}))
}

/// Keep the Import tab's settings as an import preset (`import.saveSettings {name}`).
pub fn save_preset(app: &mut LccApp, name: &str) -> Result<Value, String> {
    let settings = app.ui.settings.import_tab.to_preset();
    let r = app.session.execute("import.savePreset", &json!({"name": name, "settings": settings})).map_err(|e| e.to_string())?;
    app.ui.settings.import_tab.preset_name = r["name"].as_str().unwrap_or(name).to_string();
    Ok(r)
}

/// Change the Import tab's settings (`import.configure`): any of `take`, `previews`, and the
/// import review's options (`copy`, `moveFiles`, `destination`, `organize`, `folderTemplate`,
/// `rename`, `preset`, `presets`, `presetProfile`, `keywords`, `metadataPreset`, `album`, `newAlbum`, `dng`), by
/// name. Returns the settings.
pub fn configure(app: &mut LccApp, p: &Value) -> Result<Value, String> {
    let Some(given) = p.as_object() else { return Err("settings must be an object".into()) };
    let mut s = app.ui.settings.import_tab.clone();
    let mut opts = serde_json::to_value(&s.opts).map_err(|e| e.to_string())?;
    for (k, v) in given {
        match k.as_str() {
            "take" => {
                let t = v.as_str().filter(|t| Take::parse(t).is_some()).ok_or("take: picks|selected|all")?;
                s.take = t.to_string();
            }
            "previews" => {
                let t = v.as_str().filter(|t| matches!(*t, "none" | "standard" | "full")).ok_or("previews: none|standard|full")?;
                s.previews = t.to_string();
            }
            "candidates" | "checked" | "trashed" | "sources" | "recipe" | "immich" => return Err(format!("`{k}` is not a setting")),
            _ if opts.get(k).is_some() => opts[k] = v.clone(),
            _ => return Err(format!("unknown import setting `{k}`")),
        }
    }
    s.opts = serde_json::from_value(opts).map_err(|e| format!("import settings: {e}"))?;
    s.opts.params()?;
    s.preset_name.clear();
    app.ui.settings.import_tab = s;
    serde_json::to_value(&app.ui.settings.import_tab).map_err(|e| e.to_string())
}

/// The UI commands of the tabs; `None` for another id.
pub fn run(app: &mut LccApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    Some(match id {
        "view.importTab" => switch(app, Tab::Import),
        "view.libraryTab" => switch(app, Tab::Library),
        "import.start" => start(app, p),
        "import.develop" => develop_without_import(app),
        "import.configure" => configure(app, p),
        "import.usePreset" => match p.get("name").and_then(Value::as_str) {
            Some(n) => use_preset(app, n),
            None => Err("missing name".into()),
        },
        "import.saveSettings" => match p.get("name").and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty()) {
            Some(n) => save_preset(app, n),
            None => Err("give the preset a name".into()),
        },
        "import.settings" => serde_json::to_value(&app.ui.settings.import_tab).map_err(|e| e.to_string()),
        _ => return None,
    })
}

// ------------------------------------------------------------------------------------- drawing

/// From this width the sources and the settings stand beside the photos.
const WIDE: f32 = 900.0;
const BAR_H: f32 = 48.0;

/// What a narrow window shows in the one column.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Pane {
    #[default]
    Photos,
    Sources,
    Settings,
}

fn pane_id() -> egui::Id {
    egui::Id::new("import-tab-pane")
}

/// The Import | Library switch, in the top bar (Classic, Compact) or the Modern library bar.
/// Nothing when Import is not a tab.
/// The width [`tabs`] takes in the top bar, its spacing before included (0 when the tabs are off),
/// so the in-window menus and the search field leave it room.
pub fn tabs_width(app: &LccApp, ui: &egui::Ui) -> f32 {
    if !enabled(app) {
        return 0.0;
    }
    let t = Tokens::get(ui.ctx());
    let finger = crate::dialog_frame::finger_sized(ui);
    let font = t.semibold(if finger { 14.0 } else { 12.5 });
    6.0 + 4.0
        + ["Browse", "Library"]
            .iter()
            .map(|l| ui.painter().layout_no_wrap(crate::i18n::tr(l).to_string(), font.clone(), t.text).size().x + 24.0)
            .sum::<f32>()
}

pub fn tabs(app: &mut LccApp, ui: &mut egui::Ui) {
    if !enabled(app) {
        return;
    }
    let t = Tokens::get(ui.ctx());
    let finger = crate::dialog_frame::finger_sized(ui);
    let h = if finger { 36.0 } else { 26.0 };
    let font = t.semibold(if finger { 14.0 } else { 12.5 });
    let labels = [(Tab::Import, "Browse", "tab-import"), (Tab::Library, "Library", "tab-library")];
    let widths: Vec<f32> =
        labels.iter().map(|(_, l, _)| ui.painter().layout_no_wrap(crate::i18n::tr(l).to_string(), font.clone(), t.text).size().x + 24.0).collect();
    let (outer, _) = ui.allocate_exact_size(vec2(widths.iter().sum::<f32>() + 4.0, h), Sense::hover());
    ui.painter().rect(outer, t.radius(h / 2.0), t.field, Stroke::new(1.0, t.field_border), StrokeKind::Inside);
    let mut x = outer.left() + 2.0;
    for ((tab, label, wid), w) in labels.iter().zip(widths) {
        let r = Rect::from_min_size(pos2(x, outer.top() + 2.0), vec2(w, h - 4.0));
        x += w;
        let resp = ui.interact(r, ui.id().with(*wid), Sense::click());
        register(ui.ctx(), format!("button:{wid}"), r);
        let on = app.ui.tab == *tab;
        crate::access::choice(&resp, label, on);
        if on {
            ui.painter().rect_filled(r, t.radius((h - 4.0) / 2.0), t.accent);
        } else if resp.hovered() {
            ui.painter().rect_filled(r, t.radius((h - 4.0) / 2.0), t.hover);
        }
        let color = if on { t.canvas } else { t.text };
        ui.painter().text(r.center(), Align2::CENTER_CENTER, crate::i18n::tr(label), font.clone(), color);
        if resp.clicked() && !on {
            let id = if *tab == Tab::Import { "view.importTab" } else { "view.libraryTab" };
            if let Err(e) = app.run(id, json!({})) {
                app.toast(ui.ctx(), e);
            }
        }
    }
}

/// The Import tab, in the window's centre (below the top bar): sources, bar and photos, settings.
pub fn show(app: &mut LccApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let area = ui.max_rect();
    register(ui.ctx(), "panel:importTab", area);
    let wide = area.width() >= WIDE;
    let mut pane: Pane = ui.data(|d| d.get_temp(pane_id())).unwrap_or_default();
    if wide {
        pane = Pane::Photos;
    }
    // the Folders panel's right edge drags it wider (long folder names), as Library's left panel does; the width is
    // kept in the UI state
    let (sources_r, rest) = if wide {
        let mut rest = area;
        let limits = crate::state::IMPORT_SOURCES_WIDTH;
        let r = crate::panels::split_side(ui, &mut rest, true, "importSources", &mut app.ui.import_sources_width, limits);
        (Some(r), rest)
    } else {
        (None, area)
    };
    // (the settings column's left edge is dragged to resize it, as the side panels are; the width
    // is kept in the UI state)
    let (middle, settings_r) = if wide {
        let mut middle = rest;
        let limits = crate::state::IMPORT_SETTINGS_WIDTH;
        let r = crate::panels::split_side(ui, &mut middle, false, "importSettings", &mut app.ui.import_settings_width, limits);
        (middle, Some(r))
    } else {
        (rest, None)
    };
    if let Some(r) = sources_r {
        ui.painter().rect_filled(r, 0.0, t.chrome);
        ui.painter().vline(r.right(), r.y_range(), Stroke::new(1.0, t.divider));
        // (its rows and scroll bar stop short of the right edge, which is the resize handle's: they took the drag)
        let inner = Rect::from_min_max(r.min, pos2((r.right() - 4.0).max(r.left()), r.bottom()));
        let mut child = ui.new_child(egui::UiBuilder::new().id_salt("import-tab-sources").max_rect(inner));
        let mut lap = crate::now_ms();
        sources(app, &mut child);
        app.lap(&mut lap, "import.sources");
    }
    if let Some(r) = settings_r {
        ui.painter().rect_filled(r, 0.0, t.chrome);
        ui.painter().vline(r.left(), r.y_range(), Stroke::new(1.0, t.divider));
        let mut child = ui.new_child(egui::UiBuilder::new().id_salt("import-tab-settings").max_rect(r.shrink2(vec2(10.0, 0.0))));
        let mut lap = crate::now_ms();
        settings(app, &mut child);
        app.lap(&mut lap, "import.settings");
    }
    // Library's toolbar under the browsed photos (View ▸ Show Toolbar): view modes, sort, flags,
    // rating, colour labels and thumbnail size, for culling the card in place
    let mut middle = middle;
    if app.ui.toolbar
        && app.ui.screen_mode != crate::module::ScreenMode::FullScreenHidePanels
        && pane == Pane::Photos
        && app.session.source == LibrarySource::Folder
        && middle.height() > 2.0 * t.bottom_bar_h + BAR_H
    {
        let r = Rect::from_min_max(pos2(middle.left(), middle.bottom() - t.bottom_bar_h), middle.max);
        let mut child = ui.new_child(egui::UiBuilder::new().id_salt("import-tab-toolbar").max_rect(r));
        child.set_clip_rect(r);
        let mut lap = crate::now_ms();
        crate::panels::bottombar::show(app, &mut child);
        app.lap(&mut lap, "import.toolbar");
        register(ui.ctx(), "importToolbar", r);
        middle.max.y = r.top();
    }
    let bar_r = Rect::from_min_max(middle.min, pos2(middle.right(), middle.top() + BAR_H));
    let body = Rect::from_min_max(pos2(middle.left(), bar_r.bottom()), middle.max);
    {
        let mut child = ui.new_child(egui::UiBuilder::new().id_salt("import-tab-bar").max_rect(bar_r));
        let mut lap = crate::now_ms();
        bar(app, &mut child, wide, &mut pane);
        app.lap(&mut lap, "import.bar");
    }
    let mut child = ui.new_child(egui::UiBuilder::new().id_salt(("import-tab-body", pane as u8)).max_rect(body));
    child.set_clip_rect(body);
    match pane {
        Pane::Sources => {
            child.painter().rect_filled(body, 0.0, t.chrome);
            sources(app, &mut child);
        }
        Pane::Settings => {
            child.painter().rect_filled(body, 0.0, t.chrome);
            let mut inner = child.new_child(egui::UiBuilder::new().max_rect(body.shrink2(vec2(12.0, 0.0))));
            settings(app, &mut inner);
        }
        Pane::Photos if app.session.source == LibrarySource::Folder => {
            let mut lap = crate::now_ms();
            photos(app, &mut child);
            app.lap(&mut lap, "import.photos");
        }
        Pane::Photos => choose_source(app, &mut child),
    }
    if !wide {
        ui.data_mut(|d| d.insert_temp(pane_id(), pane));
    }
}

/// The browsed folder's photos: the grid, or the loupe for a closer look.
fn photos(app: &mut LccApp, ui: &mut egui::Ui) {
    use crate::state::ViewMode;
    match app.ui.view {
        ViewMode::Detail => crate::panels::detail::show(app, ui),
        _ => crate::panels::grid::show(app, ui),
    }
}

fn heading(ui: &mut egui::Ui, text: &str) {
    let t = Tokens::get(ui.ctx());
    ui.add_space(10.0);
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
    ui.painter().text(pos2(r.left() + 14.0, r.center().y), Align2::LEFT_CENTER, crate::i18n::tr(text).to_uppercase(), t.semibold(11.0), t.text_dim);
}

/// A row of the sources: a label that browses `path` when clicked.
fn source_row(app: &mut LccApp, ui: &mut egui::Ui, wid: &str, label: &str, path: &str, on: bool) {
    let t = Tokens::get(ui.ctx());
    let h = if crate::dialog_frame::finger_sized(ui) { 40.0 } else { 26.0 };
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click());
    register(ui.ctx(), format!("button:{wid}"), r);
    if on {
        ui.painter().rect_filled(r.shrink2(vec2(6.0, 1.0)), t.radius(4.0), t.cell_selected);
    } else if resp.hovered() {
        ui.painter().rect_filled(r.shrink2(vec2(6.0, 1.0)), t.radius(4.0), t.hover);
    }
    let shown = crate::widgets::elide_head(label, r.width() - 30.0, |s| ui.painter().layout_no_wrap(s.to_string(), t.font(12.5), t.text).size().x);
    ui.painter().text(pos2(r.left() + 16.0, r.center().y), Align2::LEFT_CENTER, shown, t.font(12.5), t.text);
    let resp = resp.on_hover_text(path);
    if resp.clicked() {
        browse(app, ui.ctx(), path);
    }
}

fn browse(app: &mut LccApp, ctx: &egui::Context, path: &str) {
    ctx.data_mut(|d| d.insert_temp(pane_id(), Pane::Photos));
    if let Err(e) = app.run("library.browse", json!({"path": path})) {
        app.toast(ctx, e);
    }
}

/// The sources: cards and cameras, the Local places (the folders kept there, with their trees),
/// the folders browsed lately, and a file picker for single files (with the import review).
/// The drives and volumes of this computer as (label, path), read on a worker thread
/// ([`crate::panels::left::fs_cached`]): on Windows every drive letter that answers (local, USB,
/// cards, mapped network drives); elsewhere the root and the mounted volumes.
/// The one drive tests see: a folder of their own in the temporary folder.
pub(crate) fn test_drive() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("app-test-drive-{}", std::process::id()))
}

pub(crate) fn list_drives(_: &str) -> Vec<(String, String)> {
    // tests never touch this machine's drives (mapped network drives can be slow to answer)
    if cfg!(test) {
        return vec![("Test drive".into(), test_drive().to_string_lossy().to_string())];
    }
    let mut v: Vec<(String, String)> = Vec::new();
    if cfg!(windows) {
        for l in b'A'..=b'Z' {
            let p = format!("{}:\\", l as char);
            if std::fs::metadata(&p).is_ok() {
                v.push((format!("{}:", l as char), p));
            }
        }
    } else {
        v.push(("/".into(), "/".into()));
        let user = std::env::var("USER").unwrap_or_default();
        for base in ["/Volumes".to_string(), format!("/media/{user}"), format!("/run/media/{user}"), "/mnt".to_string()] {
            let Ok(rd) = std::fs::read_dir(&base) else { continue };
            let mut found: Vec<(String, String)> = rd
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir() || t.is_symlink()))
                .map(|e| (e.file_name().to_string_lossy().to_string(), e.path().to_string_lossy().to_string()))
                .collect();
            found.sort();
            v.extend(found);
        }
    }
    v
}

fn sources(app: &mut LccApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    ui.spacing_mut().item_spacing.y = 0.0;
    let (hr, _) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::hover());
    register(ui.ctx(), "label:importFrom", hr);
    ui.painter().text(pos2(hr.left() + 14.0, hr.center().y - 7.0), Align2::LEFT_CENTER, crate::i18n::tr("Import from"), t.semibold(14.0), t.text);
    ui.painter().text(
        pos2(hr.left() + 14.0, hr.center().y + 10.0),
        Align2::LEFT_CENTER,
        crate::i18n::tr("A card, a drive or a folder"),
        t.font(11.0),
        t.text_dim,
    );
    let current = app.session.browse.clone().filter(|_| app.session.source == LibrarySource::Folder).map(|b| b.path);
    egui::ScrollArea::vertical().id_salt("import-tab-sources-scroll").auto_shrink([false, false]).show(ui, |ui| {
        let cards = crate::import_screen::devices(ui.ctx());
        if !cards.is_empty() {
            heading(ui, "Cards and Cameras");
            for (i, (name, path)) in cards.iter().enumerate() {
                // a card opens at its DCIM folder
                let dcim = std::path::Path::new(path).join("DCIM");
                let open_at = if dcim.is_dir() { dcim.to_string_lossy().to_string() } else { path.clone() };
                let on = current.as_deref().is_some_and(|c| crate::panels::left::same_folder(c, &open_at));
                source_row(app, ui, &format!("importCard-{i}"), name, &open_at, on);
            }
        }
        // This PC: every drive, each a folder tree to open down to the folder to import from
        heading(ui, "This PC");
        let drives = crate::panels::left::fs_cached(ui, "import-drives", "", 5.0, list_drives).unwrap_or_default();
        if drives.is_empty() {
            ui.horizontal(|ui| {
                ui.add_space(18.0);
                ui.label(egui::RichText::new(crate::i18n::tr("Looking for drives…")).color(t.text_dim));
            });
        }
        for (label, path) in &drives {
            crate::panels::left::folder_tree(app, ui, "drives", label, path, 0.0, current.as_deref(), true, false);
        }
        heading(ui, "Folders");
        crate::panels::left::local_body(app, ui);
        // the folders browsed lately, newest first
        let mut recent: Vec<(String, String)> = app.session.catalog.browsed_folders().iter().map(|(f, at)| (at.clone(), f.clone())).collect();
        recent.sort_by(|a, b| b.cmp(a));
        let recent: Vec<String> = recent.into_iter().map(|(_, f)| f).filter(|f| std::path::Path::new(f).is_dir()).take(6).collect();
        if !recent.is_empty() {
            heading(ui, "Recent");
            for (i, f) in recent.iter().enumerate() {
                let name = std::path::Path::new(f).file_name().map_or_else(|| f.clone(), |n| n.to_string_lossy().to_string());
                let on = current.as_deref().is_some_and(|c| crate::panels::left::same_folder(c, f));
                source_row(app, ui, &format!("importRecent-{i}"), &name, f, on);
            }
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            if crate::button::Button::new(crate::i18n::tr("Import Individual Files…")).widget("importFiles").show(ui).clicked()
                && let Err(e) = app.run("file.addPhotos", json!({}))
            {
                app.toast(ui.ctx(), e);
            }
        });
        ui.add_space(16.0);
    });
}

/// The bar over the photos: where they come from, what the big button takes and the button. On a
/// narrow window, Sources and Settings toggles too.
fn bar(app: &mut LccApp, ui: &mut egui::Ui, wide: bool, pane: &mut Pane) {
    let t = Tokens::get(ui.ctx());
    let r = ui.max_rect();
    ui.painter().rect_filled(r, 0.0, t.chrome);
    ui.painter().hline(r.x_range(), r.bottom() - 0.5, Stroke::new(1.0, t.divider));
    let browsing = app.session.source == LibrarySource::Folder;
    let (picks, selected, all) = if browsing { counts(app) } else { (0, 0, 0) };
    let take = app.ui.settings.import_tab.take();
    let n = match take {
        Take::Picks => picks,
        Take::Selected => selected,
        Take::All => all,
    };
    let mut row = ui.new_child(egui::UiBuilder::new().max_rect(r.shrink2(vec2(10.0, 0.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
    row.spacing_mut().item_spacing.x = 8.0;
    // whatever happens on a very narrow window, nothing of the bar paints over the panels beside it
    row.set_clip_rect(r.intersect(ui.clip_rect()));
    register(ui.ctx(), "importBar", r);
    // Modern draws no top bar over the tab: the way back to the library is here
    if crate::modern::on(app) && crate::widgets::text_button(&mut row, "importBackToLibrary", crate::i18n::tr("‹ Library"), false).clicked() {
        let _ = app.run("view.libraryTab", json!({}));
    }
    if !wide {
        for (p, label, wid) in [(Pane::Sources, "Sources", "importPane-sources"), (Pane::Settings, "Settings", "importPane-settings")] {
            let on = *pane == p;
            if crate::widgets::text_button(&mut row, wid, crate::i18n::tr(label), on).clicked() {
                *pane = if on { Pane::Photos } else { p };
            }
        }
    }
    // where: the folder's name, all of it in a tooltip
    let folder = app.session.browse.as_ref().filter(|_| browsing).map(|b| b.path.clone());
    let name = folder.as_ref().map(|f| std::path::Path::new(f).file_name().map_or_else(|| f.clone(), |n| n.to_string_lossy().to_string()));
    // the button says what it does; how many and where to are in its tooltip
    let import_label = crate::i18n::tr("Import").to_string();
    let import_tip = crate::i18n::tr_format!("Import {n} into {library}", n = n, library = library_name(app));
    let layout = bar_layout(&row, &import_label, name.as_deref(), folder.as_deref(), wide);
    let mut room = layout.room.max(40.0);
    // the folders above it as a breadcrumb (OneSlips LR-IMP-WINDOW 6e4fa44d0): each opens, those that don't fit
    // go under "…", whose menu lists them; the folder's own name is kept whole first
    if let Some(f) = &folder {
        room = (room - folder_crumbs(app, &mut row, f, (room - layout.name_w).max(0.0))).max(layout.name_w.min(room));
    }
    let text = match &name {
        Some(name) => crate::i18n::tr_format!("{name} · {all} not imported · {picks} picks", name = name, all = all, picks = picks),
        None => crate::i18n::tr("Choose where to import from").to_string(),
    };
    let width = |s: &str| row.painter().layout_no_wrap(s.to_string(), t.font(12.5), t.text).size().x;
    let shown = crate::widgets::elide_head(&text, room, width);
    // no wider than its room, so the right-hand group keeps the rest of the row (cut short at its end, the
    // folder's name first)
    let label_w = width(&shown).min(room);
    let lr = row.add_sized(vec2(label_w, 20.0), egui::Label::new(egui::RichText::new(shown).color(t.text).size(12.5)).truncate());
    register(row.ctx(), "label:importWhere", lr.rect);
    if let Some(f) = &folder {
        lr.on_hover_text(f);
    }
    row.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        // right of the Import button (first in the right-to-left row): develop the active photo
        // here, without importing it — the Develop module in the Library tab, edits to the
        // sidecar, the catalog untouched
        if browsing
            && crate::button::Button::new(crate::i18n::tr("Develop"))
                .enabled(app.session.active().is_some())
                .widget("importDevelop")
                .show(ui)
                .on_hover_text(crate::i18n::tr("Develop without Import"))
                .clicked()
        {
            match app.run("import.develop", json!({})) {
                Ok(_) => {}
                Err(e) => app.toast(ui.ctx(), e),
            }
        }
        if crate::button::Button::new(&import_label).primary().enabled(n > 0).widget("importGo").show(ui).on_hover_text(&import_tip).clicked() {
            match app.run("import.start", json!({})) {
                Ok(v) => {
                    let n = v["importing"].as_u64().unwrap_or(0);
                    app.toast(ui.ctx(), crate::i18n::tr_format!("Importing {n} photos…", n = n));
                }
                Err(e) => app.toast(ui.ctx(), e),
            }
        }
        if layout.show {
            show_segments(app, ui);
        }
        if layout.take {
            take_segments(app, ui);
        }
        if layout.sub {
            subfolders_check(app, ui);
        }
    });
}

const SHOW_LABELS: [&str; 2] = ["Show All", "Show Not Imported"];
const TAKE_LABELS: [&str; 3] = ["Picks", "Selected", "All"];

/// What the import bar shows, measured so it fits its row: the right-hand group in its fullest
/// form that leaves the browsed folder's own name whole, and the room left for the folder.
#[derive(Debug, PartialEq)]
pub(crate) struct BarLayout {
    /// Picks / Selected / All (also under Import Settings).
    pub take: bool,
    /// Show All / Show Not Imported.
    pub show: bool,
    /// Include subfolders.
    pub sub: bool,
    /// What the right-hand group leaves the breadcrumb and the folder's label.
    pub room: f32,
    /// The folder's own name, which the breadcrumb never takes (0 when nothing is browsed).
    pub name_w: f32,
}

/// Lay out the import bar's right-hand group in `row`: in order, Picks / Selected / All (also under Import
/// Settings), Show All / Show Not Imported and Include subfolders go
/// until what is left holds the browsed folder's whole name with "…" for its parents. Nothing is laid
/// out past the row, so the bar never runs over the panel beside it.
fn bar_layout(row: &egui::Ui, import_label: &str, name: Option<&str>, folder: Option<&str>, wide: bool) -> BarLayout {
    let t = Tokens::get(row.ctx());
    let browsing = folder.is_some();
    let gap = row.spacing().item_spacing.x;
    let avail = row.available_width();
    let measure = |s: &str| row.painter().layout_no_wrap(s.to_string(), t.font(12.5), t.text).size().x;
    let button = |label: &str, primary: bool| {
        let b = crate::button::Button::new(label);
        if primary { b.primary() } else { b }.desired_size(row).x
    };
    // the folder's own name and a little of what follows it, then "…" and a chevron for its parents
    let name_w = name.map_or(0.0, |n| measure(&format!("{n} …")) + gap);
    let parents = folder.map_or(0, |f| crate::panels::grid::crumbs(f).len().saturating_sub(1));
    let crumb_min = if parents > 0 { measure("…") + 14.0 + 3.0 * gap } else { 0.0 };
    let import_w = button(import_label, true);
    let develop_w = if browsing { button(crate::i18n::tr("Develop"), false) + gap } else { 0.0 };
    let show_w = crate::widgets::segments_width(row, &SHOW_LABELS) + gap;
    let take_w = crate::widgets::segments_width(row, &TAKE_LABELS) + gap;
    // the checkbox in its own text style (egui's Button), the icon and the gap between them
    let button_font = egui::TextStyle::Button.resolve(row.style());
    let sub_w = row.painter().layout_no_wrap(crate::i18n::tr("Include subfolders").to_string(), button_font, t.text).size().x + 26.0 + gap;
    let mut l = BarLayout { take: wide, show: browsing, sub: browsing, room: 0.0, name_w };
    let right = |l: &BarLayout| {
        import_w + develop_w + if l.show { show_w } else { 0.0 } + if l.take { take_w } else { 0.0 } + if l.sub { sub_w } else { 0.0 } + gap
    };
    let fits = |l: &BarLayout| avail - right(l) >= name_w + crumb_min;
    for step in 0..3 {
        if fits(&l) {
            break;
        }
        match step {
            0 => l.take = false,
            1 => l.show = false,
            _ => l.sub = false,
        }
    }
    l.room = (avail - right(&l)).max(0.0);
    l
}

/// The browsed folder's parents, nearest last, as a breadcrumb in the bar at most `max_w` wide (widgets
/// `importCrumb:{i}`, `i` the folder's place in the path; `importCrumb:more` for "…"): a click on one opens it. The
/// parents that don't fit, from the top of the path down, are listed by "…"'s menu. → the width it took.
fn folder_crumbs(app: &mut LccApp, ui: &mut egui::Ui, path: &str, max_w: f32) -> f32 {
    let t = Tokens::get(ui.ctx());
    let crumbs = crate::panels::grid::crumbs(path);
    // every folder above the browsed one (the last part is the bar's own label)
    let parents = crumbs.len().saturating_sub(1);
    if parents == 0 || !max_w.is_finite() || max_w <= 0.0 {
        return 0.0;
    }
    let font = t.font(12.5);
    let sep_w = 14.0 + 2.0 * ui.spacing().item_spacing.x;
    let painter = ui.painter().clone();
    let width = |s: &str| painter.layout_no_wrap(s.to_string(), font.clone(), t.text_label).size().x.min(140.0);
    let dots_w = width("…") + sep_w;
    // the nearest parents that fit, keeping room for "…" when any are left out
    let mut shown = 0;
    let mut used = 0.0;
    for (name, _) in crumbs.iter().take(parents).rev() {
        let w = width(name) + sep_w;
        let need = if shown + 1 < parents { dots_w } else { 0.0 };
        if used + w + need > max_w {
            break;
        }
        used += w;
        shown += 1;
    }
    let hidden = parents - shown;
    let start = ui.cursor().left();
    let separator = |ui: &mut egui::Ui| {
        let (r, _) = ui.allocate_exact_size(vec2(14.0, 20.0), Sense::hover());
        crate::icons::paint(ui.painter(), Rect::from_center_size(r.center(), vec2(12.0, 12.0)), crate::icons::Icon::ChevronRight, t.text_dim);
    };
    let mut open: Option<String> = None;
    if hidden > 0 {
        let dots = ui.add(egui::Label::new(egui::RichText::new("…").font(font.clone()).color(t.text_dim)).sense(Sense::click()));
        register(ui.ctx(), "importCrumb:more", dots.rect);
        if dots.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let dots = dots.on_hover_text(path);
        egui::Popup::menu(&dots).show(|ui| {
            for (i, (name, to)) in crumbs.iter().enumerate().take(hidden) {
                let item = ui.button(*name);
                register(ui.ctx(), format!("importCrumb:{i}"), item.rect);
                if item.on_hover_text(to).clicked() {
                    open = Some(to.clone());
                    ui.close();
                }
            }
        });
        separator(ui);
    }
    for (i, (name, to)) in crumbs.iter().enumerate().take(parents).skip(hidden) {
        let r = ui.add_sized(
            vec2(width(name), 20.0),
            egui::Label::new(egui::RichText::new(*name).font(font.clone()).color(t.text_label)).truncate().sense(Sense::click()),
        );
        register(ui.ctx(), format!("importCrumb:{i}"), r.rect);
        if r.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if r.on_hover_text(to).clicked() {
            open = Some(to.clone());
        }
        separator(ui);
    }
    if let Some(to) = open {
        browse(app, ui.ctx(), &to);
    }
    (ui.cursor().left() - start).max(0.0)
}

/// Show All / Show Not Imported: the browse quick filter (`library.browse`'s `show`). Not
/// imported is the default: the files an import would take. Show All adds the files the library
/// has already, each marked In library in the grid.
fn show_segments(app: &mut LccApp, ui: &mut egui::Ui) {
    let Some(b) = app.session.browse.clone() else { return };
    let chosen = if b.not_imported { 1 } else { 0 };
    let Some(i) = crate::widgets::segments(ui, "browseShow", &["Show All", "Show Not Imported"], Some(chosen)) else {
        return;
    };
    let show = if i == 0 { "all" } else { "notImported" };
    if (i == 1) != b.not_imported
        && let Err(e) = app.run("library.browse", json!({"path": b.path, "subfolders": b.subfolders, "show": show}))
    {
        app.toast(ui.ctx(), e);
    }
}

/// Include subfolders: whether the browse reaches into the folder's subfolders (`library.browse`'s
/// `subfolders`), as the Local folder header and the import review offer it. A change re-browses
/// the folder with the choice given, so the view — and the import that follows — take it at once.
fn subfolders_check(app: &mut LccApp, ui: &mut egui::Ui) {
    let Some(b) = app.session.browse.clone() else { return };
    let mut sub = b.subfolders;
    let c = ui.checkbox(&mut sub, crate::i18n::tr("Include subfolders"));
    register(ui.ctx(), "check:includeSubfolders", c.rect);
    if c.changed()
        && let Err(e) = app.run("library.browse", json!({"path": b.path, "subfolders": sub}))
    {
        app.toast(ui.ctx(), e);
    }
}

/// Picks / Selected / All: what the big button takes.
fn take_segments(app: &mut LccApp, ui: &mut egui::Ui) {
    let chosen = match app.ui.settings.import_tab.take() {
        Take::Picks => 0,
        Take::Selected => 1,
        Take::All => 2,
    };
    if let Some(i) = crate::widgets::segments(ui, "importTake", &["Picks", "Selected", "All"], Some(chosen)) {
        app.ui.settings.import_tab.take = ["picks", "selected", "all"].get(i).copied().unwrap_or("picks").to_string();
    }
}

/// (picks, selected, all) of the browsed folder not in the library yet, counted once a frame at
/// most (`import.targets`).
fn counts(app: &mut LccApp) -> (usize, usize, usize) {
    let (p, s, a) = app.session.import_targets();
    (p.len(), s.len(), a.len())
}

/// Nothing browsed yet: where an import can start.
fn choose_source(app: &mut LccApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let r = ui.max_rect();
    ui.painter().rect_filled(r, 0.0, t.grid_bg);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r.shrink(24.0)).layout(egui::Layout::top_down(egui::Align::Center)));
    child.add_space((r.height() * 0.25).min(160.0));
    child.label(egui::RichText::new(crate::i18n::tr("Choose a card, a drive or a folder")).color(t.text).size(18.0));
    child.add_space(6.0);
    child.label(
        egui::RichText::new(crate::i18n::tr(
            "Its photos show here without being added: cull them (P picks, X rejects), then import the picks, the selection or all of them.",
        ))
        .color(t.text_dim)
        .size(12.5),
    );
    child.add_space(16.0);
    if app.services.pick_folder.is_some()
        && crate::button::Button::new(crate::i18n::tr("Browse Folder…")).primary().widget("importBrowse").show(&mut child).clicked()
        && let Some(f) = app.services.pick_folder.as_mut().and_then(|f| f())
    {
        browse(app, child.ctx(), &f);
    }
    for (i, (name, path)) in crate::import_screen::devices(child.ctx()).iter().enumerate() {
        let dcim = std::path::Path::new(path).join("DCIM");
        let open_at = if dcim.is_dir() { dcim.to_string_lossy().to_string() } else { path.clone() };
        if crate::button::Button::new(name).widget(format!("importCardBig-{i}")).show(&mut child).clicked() {
            browse(app, child.ctx(), &open_at);
        }
    }
}

/// The settings on the right: the import preset, what to take, the import review's options (how,
/// apply, with the preset stack), the previews to build, and the big button again at the bottom.
fn settings(app: &mut LccApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    // (one ordered stack: a single preset from older settings goes first, the import preset's name stays)
    app.ui.settings.import_tab.opts.fold_preset();
    let mut s = app.ui.settings.import_tab.clone();
    egui::ScrollArea::vertical().id_salt("import-tab-settings-scroll").auto_shrink([false, false]).show(ui, |ui| {
        ui.add_space(12.0);
        ui.label(egui::RichText::new(crate::i18n::tr("Import Settings")).color(t.text).size(14.0).strong());
        ui.add_space(6.0);
        crate::dialog_frame::stacked_form(ui, 92.0, |ui| {
            use crate::dialog_frame::{form_row, group_begin};
            group_begin(ui, Some("Import Preset"));
            preset_row(app, ui, &mut s);
            form_row(ui, "Take", |ui| {
                let chosen = match s.take() {
                    Take::Picks => 0,
                    Take::Selected => 1,
                    Take::All => 2,
                };
                if let Some(i) = crate::widgets::segments(ui, "importTakeSettings", &["Picks", "Selected", "All"], Some(chosen)) {
                    s.take = ["picks", "selected", "all"].get(i).copied().unwrap_or("picks").to_string();
                }
            });
            crate::import_screen::options_for(app, ui, &mut s.opts, true);
            group_begin(ui, Some("After the Import"));
            form_row(ui, "Previews", |ui| {
                let opts = [("none", "None"), ("standard", "Standard"), ("full", "1:1")];
                let chosen = opts.iter().position(|(k, _)| *k == s.previews);
                let labels: Vec<&str> = opts.iter().map(|(_, l)| *l).collect();
                if let Some(i) = crate::widgets::segments(ui, "importPreviews", &labels, chosen)
                    && let Some((k, _)) = opts.get(i)
                {
                    s.previews = (*k).to_string();
                }
            });
            crate::dialog_frame::help(ui, "Built in the background once the photos are in, from the one on screen outward.");
        });
        ui.add_space(16.0);
    });
    if s != app.ui.settings.import_tab {
        if s.opts != app.ui.settings.import_tab.opts {
            s.preset_name.clear();
        }
        app.ui.settings.import_tab = s;
    }
}

/// The import preset: choose one, or keep the settings as one.
fn preset_row(app: &mut LccApp, ui: &mut egui::Ui, s: &mut ImportSettings) {
    use crate::dialog_frame::form_row;
    let names: Vec<String> = app.session.import_presets.iter().map(|p| p.name.clone()).collect();
    let mut chosen: Option<String> = None;
    form_row(ui, "Preset", |ui| {
        let shown = if s.preset_name.is_empty() { crate::i18n::tr("None").to_string() } else { s.preset_name.clone() };
        let list_w = crate::import::combo_list_width(ui, names.iter().map(String::as_str));
        let combo = crate::widgets::combo_box("import-tab-preset").selected_text(shown).width(ui.available_width().max(120.0)).show_ui(ui, |ui| {
            ui.set_min_width(list_w);
            for (i, n) in names.iter().enumerate() {
                let r = ui.selectable_label(*n == s.preset_name, n);
                register(ui.ctx(), format!("button:importPreset-{i}"), r.rect);
                if r.clicked() {
                    chosen = Some(n.clone());
                }
            }
        });
        register(ui.ctx(), "combo:importPreset", combo.response.rect);
    });
    let name_id = egui::Id::new("import-tab-preset-name");
    let mut name: String = ui.data(|d| d.get_temp(name_id)).unwrap_or_default();
    form_row(ui, "Save as", |ui| {
        ui.horizontal(|ui| {
            let w = (ui.available_width() - 60.0).max(60.0);
            let r = ui.add(egui::TextEdit::singleline(&mut name).hint_text(crate::i18n::tr("Preset name")).desired_width(w));
            register(ui.ctx(), "field:importPresetName", r.rect);
            if crate::widgets::text_button(ui, "importPresetSave", crate::i18n::tr("Save"), false).clicked() && !name.trim().is_empty() {
                app.ui.settings.import_tab = s.clone();
                match save_preset(app, name.trim()) {
                    Ok(_) => {
                        s.preset_name = name.trim().to_string();
                        name.clear();
                    }
                    Err(e) => app.toast(ui.ctx(), e),
                }
            }
        });
    });
    ui.data_mut(|d| d.insert_temp(name_id, name));
    if let Some(n) = chosen
        && let Some(p) = app.session.import_presets.iter().find(|p| p.name == n)
    {
        let take = s.take.clone();
        *s = ImportSettings::from_preset(&p.name, &p.settings);
        s.take = take;
    }
}
