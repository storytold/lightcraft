//! File ▸ Import Lightroom Catalog…: where the catalog goes (issue #2672).
//!
//! Before the import starts, a window asks whether the catalog's photos, collections and edits
//! merge into the library open now or go into a new library, named after the catalog and put next
//! to the open one. The new library is made and opened as File ▸ New Library… does
//! (`app.newLibrary`: refused in a folder that isn't empty, or while another import runs), then the
//! catalog is imported into it. Settings ▸ General ▸ Library (or the window's "Don't ask again")
//! turns the question off: the catalog then merges into the open library, as it always did.
//!
//! Only the app command `file.importLightroom` changes: it takes `{into?: "open" | "new", name?,
//! location?}` so the control channel can make the choice without the window. The import itself
//! (`library.importLightroom`, [`crate::lightroom_import`]) is unchanged. A session with no
//! library on disk (in memory) has nothing to put a new library next to: it merges without asking.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::LightcraftApp;
use crate::state::{Dialog, LightroomImport};

/// Where `file.importLightroom {into}` puts the catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Destination {
    /// `"open"`: merge into the library open now.
    Open,
    /// `"new"`: a new library, opened before the import.
    New,
}

/// The destination params of `file.importLightroom`, checked.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Request {
    /// `into`; `None`: ask (or merge, as the setting says).
    pub into: Option<Destination>,
    /// `name`: the new library's name (default: the catalog file's name without `.lrcat`).
    pub name: Option<String>,
    /// `location`: the folder the new library goes in (default: the open library's parent folder).
    pub location: Option<String>,
}

impl Request {
    /// Read `{into?, name?, location?}` from `file.importLightroom`'s params. A wrong type or an
    /// unknown `into` is an error, before any file dialog opens.
    pub fn parse(p: &Value) -> Result<Request, String> {
        let into = match p.get("into") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if s == "open" => Some(Destination::Open),
            Some(Value::String(s)) if s == "new" => Some(Destination::New),
            Some(v) => return Err(format!("file.importLightroom: `into` must be \"open\" or \"new\", not {v}")),
        };
        let text = |key: &str| match p.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(v) => Err(format!("file.importLightroom: `{key}` must be a string, not {v}")),
        };
        Ok(Request { into, name: text("name")?, location: text("location")? })
    }
}

/// What `file.importLightroom` does with a catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plan {
    /// Import into the library open now.
    Merge,
    /// Make a new library, open it, import into it.
    NewLibrary,
    /// Show the window; `new_library`: "Create a new library" is the choice it opens with.
    Ask { new_library: bool },
}

/// The decision, from the setting, the open library (`on_disk`: saved in a folder, not in memory;
/// `photos`: how many it holds) and the command's `into`. `into` always wins; without it a library
/// in memory, or the setting "Always merge", merges; otherwise the window asks, opening on "Merge"
/// when the library is empty (there is nothing to merge with) and on "Create a new library" when
/// it holds photos.
pub fn decide(setting: LightroomImport, on_disk: bool, photos: usize, into: Option<Destination>) -> Plan {
    match into {
        Some(Destination::Open) => Plan::Merge,
        Some(Destination::New) => Plan::NewLibrary,
        None if !on_disk || setting == LightroomImport::AlwaysMerge => Plan::Merge,
        None => Plan::Ask { new_library: photos > 0 },
    }
}

/// The photos in the open library (not those in Recently Deleted or browsed under Local).
fn library_photos(app: &LightcraftApp) -> usize {
    app.session.catalog.photos().filter(|p| p.in_library()).count()
}

/// The open library's folder, when it is saved on disk.
fn library_dir(app: &LightcraftApp) -> Option<PathBuf> {
    app.session.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.clone())
}

/// Where a new library goes by default: next to the open one (its parent folder).
pub fn default_location(app: &LightcraftApp) -> Option<PathBuf> {
    library_dir(app).and_then(|d| d.parent().map(Path::to_path_buf))
}

/// The new library's default name: the catalog's file name without `.lrcat` (`Travel.lrcat` →
/// `Travel`), or `Travel 2`, `Travel 3`… when `location` has something by that name already.
pub fn default_name(catalog: &Path, location: Option<&Path>) -> String {
    let stem = catalog.file_stem().map(|s| s.to_string_lossy().trim().to_string()).filter(|s| !s.is_empty());
    let stem = stem.unwrap_or_else(|| "Lightroom Catalog".to_string());
    match location {
        Some(loc) => crate::panels::settings::free_name(loc, &stem).file_name().map_or(stem.clone(), |n| n.to_string_lossy().into_owned()),
        None => stem,
    }
}

/// Refuse while a Lightroom catalog task or another import runs (as the import itself would),
/// before a window asks a question whose answer couldn't be carried out.
fn busy(app: &LightcraftApp) -> Result<(), String> {
    if crate::lightroom_import::is_running(app) {
        return Err("a Lightroom catalog task is already running; wait for it to finish".into());
    }
    if app.import.is_some() || app.scan.is_some() {
        return Err("an import is running; wait for it to finish before importing a Lightroom catalog".into());
    }
    Ok(())
}

/// `file.importLightroom` once the catalog `path` is known: merge, make a new library first, or
/// ask, as [`decide`] says. Returns the import's start (`{running, kind, path}`), with
/// `library: {path, name, photos}` when a new library was made, or `{asking: true, path, into}`
/// while the window asks (`into`: the choice it opens with).
pub fn import(app: &mut LightcraftApp, path: &str, req: &Request) -> Result<Value, String> {
    // the browser build can't import a catalog at all: the import says so (and no library is made)
    if cfg!(target_arch = "wasm32") {
        return app.run("library.importLightroom", json!({"path": path}));
    }
    let on_disk = library_dir(app).is_some();
    match decide(app.ui.settings.lightroom_import, on_disk, library_photos(app), req.into) {
        Plan::Merge => app.run("library.importLightroom", json!({"path": path})),
        Plan::NewLibrary => into_new_library(app, path, req),
        Plan::Ask { new_library } => {
            busy(app)?;
            if !Path::new(path).is_file() {
                return Err(format!("{path}: there is no Lightroom catalog file here"));
            }
            let location = req.location.clone().or_else(|| default_location(app).map(|d| d.to_string_lossy().into_owned())).unwrap_or_default();
            let name = req.name.clone().unwrap_or_else(|| default_name(Path::new(path), (!location.is_empty()).then_some(Path::new(&location))));
            app.ui.dialog = Some(Dialog::LightroomDestination {
                catalog: path.to_string(),
                library: crate::panels::settings::open_library_name(app).unwrap_or_default(),
                photos: library_photos(app),
                new_library,
                name,
                location,
                dont_ask: false,
                generation: app.session.library_generation(),
            });
            Ok(json!({"asking": true, "path": path, "into": if new_library { "new" } else { "open" }}))
        }
    }
}

/// Make the new library (`app.newLibrary {path: location, name}`: its rules, its errors), open it
/// and import the catalog there. The catalog is checked first, so a mistyped path doesn't leave an
/// empty library behind.
fn into_new_library(app: &mut LightcraftApp, path: &str, req: &Request) -> Result<Value, String> {
    busy(app)?;
    if !Path::new(path).is_file() {
        return Err(format!("{path}: there is no Lightroom catalog file here"));
    }
    let location = match req.location.as_deref().map(str::trim) {
        Some("") => return Err("file.importLightroom: choose where to put the new library (`location`)".into()),
        Some(l) => PathBuf::from(l),
        None => default_location(app).ok_or("file.importLightroom: no library is open on disk to put the new one next to; give `location`")?,
    };
    if !location.is_absolute() {
        return Err(format!("file.importLightroom: {}: the location must be a full folder path", location.display()));
    }
    let name = req.name.clone().unwrap_or_else(|| default_name(Path::new(path), Some(&location)));
    let library = app.run("app.newLibrary", json!({"path": location.to_string_lossy(), "name": name}))?;
    let mut started = app.run("library.importLightroom", json!({"path": path}))?;
    if let Some(o) = started.as_object_mut() {
        o.insert("library".into(), library);
    }
    Ok(started)
}

/// The window's choice as `file.importLightroom` params.
fn params(catalog: &str, new_library: bool, name: &str, location: &str) -> Value {
    if new_library { json!({"path": catalog, "into": "new", "name": name, "location": location}) } else { json!({"path": catalog, "into": "open"}) }
}

/// The window's confirm button: import as chosen. "Don't ask again" is kept once a merge has
/// started (it only applies to merging). Another library opened while the window was up (by an
/// agent, the control channel) makes the window stale for both choices: nothing is merged into
/// it, and no new library is made from defaults that described the library it replaced.
pub fn confirm(app: &mut LightcraftApp, dialog: &Dialog) -> Result<Value, String> {
    let Dialog::LightroomDestination { catalog, new_library, name, location, dont_ask, generation, .. } = dialog else {
        return Err("not the Lightroom catalog destination window".into());
    };
    if *generation != app.session.library_generation() {
        return Err(crate::i18n::tr("Another library was opened since this window appeared: nothing was imported").to_string());
    }
    let r = app.run("file.importLightroom", params(catalog, *new_library, name, location))?;
    if !*new_library && *dont_ask {
        app.ui.settings.lightroom_import = LightroomImport::AlwaysMerge;
    }
    Ok(r)
}

/// After a failed confirm the window stays open to be corrected (a folder that isn't empty, a
/// name a folder can't have), unless the library it named has gone.
pub fn keeps_open_on_error(app: &LightcraftApp, dialog: &Dialog) -> bool {
    matches!(dialog, Dialog::LightroomDestination { generation, .. } if *generation == app.session.library_generation())
}

/// The confirm button's label.
pub fn ok_label(dialog: &Dialog) -> &'static str {
    match dialog {
        Dialog::LightroomDestination { new_library: true, .. } => "Create Library and Import",
        _ => "Import",
    }
}

/// The window's body: the two choices, the new library's name and location, and "Don't ask again".
pub fn body(app: &mut LightcraftApp, ui: &mut egui::Ui, dialog: &mut Dialog) {
    let Dialog::LightroomDestination { catalog, library, photos, new_library, name, location, dont_ask, .. } = dialog else { return };
    let t = crate::theme::Tokens::get(ui.ctx());
    let tr = crate::i18n::tr;
    let file = Path::new(catalog.as_str()).file_name().map_or_else(|| catalog.clone(), |n| n.to_string_lossy().into_owned());
    ui.label(crate::i18n::tr_format!("Where should the Lightroom catalog “{catalog}” go?", catalog = file));
    ui.label(egui::RichText::new(catalog.as_str()).color(t.text_dim));
    ui.add_space(4.0);
    let n = *photos;
    let merge = crate::i18n::tr_format!(
        "Merge into the open library “{library}” ({count} photo{})",
        if n == 1 { "" } else { "s" },
        library = library,
        count = n
    );
    let r = ui.radio_value(new_library, false, merge);
    crate::widgets::register(ui, "radio:lightroomDestination-open", r.rect);
    let indent = ui.spacing().icon_width + ui.spacing().icon_spacing;
    let note = |ui: &mut egui::Ui, text: &str| {
        ui.horizontal(|ui| {
            ui.add_space(indent);
            ui.add(egui::Label::new(egui::RichText::new(text).color(t.text_dim)).wrap());
        });
    };
    note(
        ui,
        if n == 0 {
            tr("The library is empty: the catalog's photos, collections and edits become its first.")
        } else {
            tr("The catalog's photos, collections and edits join the ones already in this library.")
        },
    );
    let r = ui.radio_value(new_library, true, tr("Create a new library"));
    crate::widgets::register(ui, "radio:lightroomDestination-new", r.rect);
    ui.add_enabled_ui(*new_library, |ui| {
        // a field's caption, left-aligned in a column as wide as the longer one (in any language)
        let font = egui::TextStyle::Body.resolve(ui.style());
        let caption_w = [tr("Library name"), tr("Location")]
            .iter()
            .map(|c| ui.painter().layout_no_wrap(c.to_string(), font.clone(), t.text_label).size().x)
            .fold(60.0_f32, f32::max)
            + 8.0;
        let caption = |ui: &mut egui::Ui, text: &str| {
            ui.allocate_ui_with_layout(egui::vec2(caption_w, 20.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.set_min_width(caption_w);
                ui.label(egui::RichText::new(text).color(t.text_label));
            });
        };
        ui.horizontal(|ui| {
            ui.add_space(indent);
            caption(ui, tr("Library name"));
            crate::text_field::TextField::singleline("field:lightroomLibraryName", name)
                .label(tr("Library name"))
                .width(ui.available_width().max(120.0))
                .show(ui);
        });
        ui.horizontal(|ui| {
            ui.add_space(indent);
            caption(ui, tr("Location"));
            let choose = app.services.pick_folder.is_some();
            let button_w = if choose { 80.0 } else { 0.0 };
            crate::text_field::TextField::singleline("field:lightroomLibraryLocation", location)
                .label(tr("Location"))
                .width((ui.available_width() - button_w).max(120.0))
                .show(ui);
            if choose
                && crate::widgets::text_button(ui, "lightroomLibraryChoose", tr("Choose…"), false).clicked()
                && let Some(pick) = app.services.pick_folder.as_mut()
                && let Some(d) = pick()
            {
                *location = d;
            }
        });
        note(ui, tr("LightCraft opens the new library, then imports the catalog into it. Its folder must be new or empty."));
    });
    ui.add_space(4.0);
    // (it only speaks of merging: unticked and greyed out while a new library is chosen)
    let mut ticked = *dont_ask && !*new_library;
    let c = ui.add_enabled(!*new_library, egui::Checkbox::new(&mut ticked, tr("Don't ask again (always merge into the open library)")));
    crate::widgets::register(ui, "check:lightroomDontAsk", c.rect);
    if !*new_library {
        *dont_ask = ticked;
    }
    ui.label(egui::RichText::new(tr("To be asked again: Settings → General → When importing a Lightroom catalog.")).size(11.0).color(t.text_dim));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every combination of the setting, an empty or full library on disk or in memory, and `into`.
    #[test]
    fn the_setting_the_library_and_into_decide() {
        use Destination::{New, Open};
        use LightroomImport::{AlwaysMerge, Ask};
        for setting in [Ask, AlwaysMerge] {
            for on_disk in [true, false] {
                for photos in [0, 5] {
                    // `into` always wins: the control channel acts without asking
                    assert_eq!(decide(setting, on_disk, photos, Some(Open)), Plan::Merge);
                    assert_eq!(decide(setting, on_disk, photos, Some(New)), Plan::NewLibrary);
                }
            }
        }
        // without it: ask, opening on Merge for an empty library and on a new library for a full one
        assert_eq!(decide(Ask, true, 0, None), Plan::Ask { new_library: false });
        assert_eq!(decide(Ask, true, 5, None), Plan::Ask { new_library: true });
        // "Always merge" doesn't ask
        assert_eq!(decide(AlwaysMerge, true, 0, None), Plan::Merge);
        assert_eq!(decide(AlwaysMerge, true, 5, None), Plan::Merge);
        // a library in memory has no folder to put a new one next to: it merges, as before
        assert_eq!(decide(Ask, false, 0, None), Plan::Merge);
        assert_eq!(decide(Ask, false, 5, None), Plan::Merge);
    }

    /// `into`, `name` and `location` are read, and a wrong one is an error naming the command.
    #[test]
    fn params_are_checked_before_anything_happens() {
        assert_eq!(Request::parse(&json!({})).unwrap(), Request::default());
        assert_eq!(Request::parse(&json!({"into": null})).unwrap(), Request::default());
        let r = Request::parse(&json!({"into": "new", "name": "Travel", "location": "/photos"})).unwrap();
        assert_eq!(r, Request { into: Some(Destination::New), name: Some("Travel".into()), location: Some("/photos".into()) });
        assert_eq!(Request::parse(&json!({"into": "open"})).unwrap().into, Some(Destination::Open));
        for bad in [json!({"into": "elsewhere"}), json!({"into": 1}), json!({"name": 3}), json!({"location": ["a"]})] {
            let err = Request::parse(&bad).unwrap_err();
            assert!(err.starts_with("file.importLightroom:"), "{err}");
        }
    }

    /// The catalog's file name, or the next free one in the location.
    #[test]
    fn the_new_library_is_named_after_the_catalog() {
        assert_eq!(default_name(Path::new("/x/Travel.lrcat"), None), "Travel");
        assert_eq!(default_name(Path::new("/x/Lightroom Catalog-v13-3.lrcat"), None), "Lightroom Catalog-v13-3");
        // nothing to name it after
        assert_eq!(default_name(Path::new(""), None), "Lightroom Catalog");
        // a name taken in the location: the next free one
        let dir = std::env::temp_dir().join(format!("lc-ui-lrdest-name-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Travel")).unwrap();
        assert_eq!(default_name(Path::new("/x/Travel.lrcat"), Some(&dir)), "Travel 2");
        assert_eq!(default_name(Path::new("/x/Family.lrcat"), Some(&dir)), "Family");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
