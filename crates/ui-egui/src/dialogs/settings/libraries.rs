use super::{LightcraftApp, Value, json};

// ------------------------------------------------------------------------------- Open Library…

/// Switching libraries is refused while an import (or the folder scan before one) is running: its
/// remaining batches would be committed into the new library (issue #1509). So is a switch while
/// Move Folder To… waits for its destination: the move runs when the dialog closes, and in another
/// library it would move the folder on disk with nothing to relink, leaving this library's photos
/// missing (issue #3140).
pub(crate) fn can_switch_library(app: &LightcraftApp) -> Result<(), String> {
    if crate::lightroom_import::is_running(app) {
        return Err("wait for Lightroom catalog import to finish before switching libraries".into());
    }
    if app.import.is_some() || app.scan.is_some() {
        return Err("wait for the import to finish before switching libraries".into());
    }
    if app.pending_picks.iter().any(|p| p.command == "folder.move") {
        return Err("wait for Move Folder To… to finish before switching libraries".into());
    }
    // they were meant for the library open now (issue #2779)
    if !app.waiting_import.files.is_empty() {
        return Err("wait for the photos waiting to be imported before switching libraries".into());
    }
    Ok(())
}

/// `app.openLibrary {path?}`: close the current library and open (or create) the one at `path`,
/// or a folder chosen in a dialog. Remembered as the library to open at launch.
///
/// A Lightroom Classic catalog (its folder, an `.lrdata` / `.lrcat-data` folder or the `.lrcat`
/// itself, see [`lightcraft_engine::lightroom_folder`]) is not opened: the current library stays
/// and a dialog offers Import Lightroom Catalog… instead; returns `{opened: false,
/// lightroomCatalog: <.lrcat path or null>, path}`.
pub fn open_library(app: &mut LightcraftApp, p: &Value) -> Result<Value, String> {
    can_switch_library(app)?;
    let path = match p.get("path").and_then(Value::as_str) {
        Some(x) => x.to_string(),
        None => {
            let req = crate::pick::PickRequest::folder(crate::i18n::tr("Open Library"));
            match crate::pick::ask(app, "app.openLibrary", p, "path", req, |s| s.pick_folder.as_mut().and_then(|f| f()).map(|x| vec![x])) {
                crate::pick::Picked::Now(v) => match v.into_iter().next() {
                    Some(x) => x,
                    None => return Ok(Value::Null),
                },
                crate::pick::Picked::Later => return Ok(Value::Null),
                crate::pick::Picked::Unavailable => return Err("no folder dialog on this platform".into()),
            }
        }
    };
    // A Lightroom catalog folder (or part of one) is not a library: before closing the library
    // open now, offer to import it instead (issue #891). The engine refuses it too.
    if let Some(place) = lightcraft_engine::lightroom_folder::lightroom_catalog_at(std::path::Path::new(&path)) {
        let catalog = place.catalog.map(|c| c.to_string_lossy().into_owned());
        let err = lightcraft_engine::EngineError::LightroomCatalog { path: path.clone().into(), catalog: catalog.clone().map(Into::into) };
        // the library problem window (no library could be opened) shows the error instead
        if app.library_problem.is_some() {
            return Err(err.to_string());
        }
        app.ui.dialog = Some(crate::state::Dialog::LightroomCatalog { folder: path.clone(), catalog: catalog.clone() });
        return Ok(json!({"opened": false, "lightroomCatalog": catalog, "path": path}));
    }
    // a remembered library that is gone: say so (and forget it) instead of creating an empty one there
    if app.ui.settings.recent_libraries.contains(&path) && !std::path::Path::new(&path).exists() {
        app.ui.settings.recent_libraries.retain(|r| r != &path);
        return Err(format!("{path}: the library is no longer there (moved, renamed or on a disconnected drive); removed from Open Recent Library"));
    }
    switch_library(app, &path)
}

/// Close the current library and open the one at `path`. If it can't be opened (locked by
/// another program, unreadable, a file), the error is returned and the current library stays open:
/// the target is checked before anything is closed, and if opening it still fails the previous
/// library is opened again (both errors are reported if that fails too).
fn switch_library(app: &mut LightcraftApp, path: &str) -> Result<Value, String> {
    let dir = app.session.check_library(std::path::Path::new(path)).map_err(|e| format!("{path}: {e}"))?;
    let path = dir.to_string_lossy().into_owned();
    // the library being left goes in the list too (also when it was opened at launch)
    let previous_dir = app.session.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.clone());
    let previous = previous_dir.as_ref().map(|d| d.to_string_lossy().into_owned());
    app.session.close_library().map_err(|e| e.to_string())?;
    let opened = app.session.open_library(&dir, false).map(|_| ()).map_err(|e| format!("{path}: {e}"));
    app.lightroom_last = None;
    app.lightroom_result = None;
    if matches!(app.ui.dialog.as_ref(), Some(crate::state::Dialog::LightroomResult { .. })) {
        app.ui.dialog = None;
    }
    app.renderer.forget_all();
    if let Err(err) = opened {
        let Some(prev) = previous_dir else { return Err(err) };
        return match app.session.open_library(&prev, false) {
            Ok(_) => Err(err),
            Err(e) => Err(format!("{err}; reopening the previous library {} failed too: {e}", prev.display())),
        };
    }
    // Photo IDs are local to a catalog: pins cannot follow them into another library (a refused
    // or failed switch reopens the same catalog, so its pins stay).
    app.ui.reference = None;
    app.ui.compare = None;
    app.ui.brief_info = crate::state::BriefInfo::default();
    // a keyword picked in the Keyword List belongs to the library being left
    app.ui.keyword_list_selected = None;
    if app.ui.view == crate::state::ViewMode::Reference {
        app.ui.view = crate::state::ViewMode::PhotoGrid;
    }
    app.ui.settings.library_path = path.clone();
    if let Some(prev) = previous.filter(|p| p != &path && !app.ui.settings.recent_libraries.contains(p)) {
        remember_library(&mut app.ui.settings.recent_libraries, &prev);
    }
    remember_library(&mut app.ui.settings.recent_libraries, &path);
    Ok(json!({"path": path, "name": library_name(&dir), "photos": app.session.catalog.len()}))
}

/// Put `path` first in the recent list, without duplicates, capped at [`RECENT_LIBRARIES`].
pub fn remember_library(recent: &mut Vec<String>, path: &str) {
    recent.retain(|r| r != path);
    recent.insert(0, path.to_string());
    recent.truncate(crate::state::RECENT_LIBRARIES);
}

/// A library's name as people see it: its folder's name.
pub fn library_name(dir: &std::path::Path) -> String {
    dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| dir.display().to_string())
}

/// The open library's name (`None`: in memory, nothing is saved).
pub fn open_library_name(app: &LightcraftApp) -> Option<String> {
    app.session.library.as_ref().filter(|l| l.on_disk).map(|l| library_name(&l.dir))
}

/// `app.newLibrary {path?, name?}`: create a library and switch to it. With `name`, the library
/// is the folder `name` inside `path` (or inside a location chosen in a dialog); without it,
/// `path` itself, or a new folder named after the default library. The folder must not hold a
/// library already, nor anything else.
pub fn new_library(app: &mut LightcraftApp, p: &Value) -> Result<Value, String> {
    can_switch_library(app)?;
    let name = match p.get("name") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(valid_library_name(s)?),
        Some(_) => return Err("app.newLibrary: name must be a string".into()),
    };
    // from the menu: ask for the name first (default prefilled), then for the location
    if name.is_none() && p.get("path").is_none() && app.services.pick_folder.is_some() {
        crate::panels::dialogs::prompt(
            app,
            "New Library",
            "Library name",
            lightcraft_engine::library::DEFAULT_NAME,
            "app.newLibrary",
            json!({}),
            "name",
        );
        return Ok(Value::Null);
    }
    let (location, picked) = match p.get("path").and_then(Value::as_str) {
        Some(x) => (std::path::PathBuf::from(x), false),
        None => {
            let req = crate::pick::PickRequest::folder(crate::i18n::tr("New Library: Choose Where to Put It"));
            match crate::pick::ask(app, "app.newLibrary", p, "path", req, |s| s.pick_folder.as_mut().and_then(|f| f()).map(|x| vec![x])) {
                crate::pick::Picked::Now(v) => match v.into_iter().next() {
                    Some(x) => (std::path::PathBuf::from(x), true),
                    None => return Ok(Value::Null),
                },
                crate::pick::Picked::Later => return Ok(Value::Null),
                crate::pick::Picked::Unavailable => return Err("no folder dialog on this platform".into()),
            }
        }
    };
    let dir = match name {
        Some(n) => location.join(n),
        // a chosen location gets a new folder in it, named after the default library
        None if picked => free_name(&location, lightcraft_engine::library::DEFAULT_NAME),
        None => location,
    };
    if lightcraft_engine::library::is_library(&dir) {
        return Err(format!("{}: there is a library here already; open it with File ▸ Open Library…", dir.display()));
    }
    if dir.is_file() {
        return Err(format!("{}: is a file; a library is a folder", dir.display()));
    }
    if std::fs::read_dir(&dir).is_ok_and(|mut d| d.next().is_some()) {
        return Err(format!("{}: the folder isn't empty; choose an empty folder or a new name", dir.display()));
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: can't create the library folder: {e}", dir.display()))?;
    switch_library(app, &dir.to_string_lossy())
}

/// Characters a folder name can't hold on Windows (and `/`, which no system allows).
const RESERVED_NAME_CHARS: [char; 9] = ['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

fn valid_library_name(s: &str) -> Result<&str, String> {
    let n = s.trim();
    // Windows drops trailing dots and spaces from a folder name: refuse them rather than
    // creating a folder whose name differs from the one asked for (spaces around the name
    // are trimmed first)
    let bad = n.is_empty() || n == "." || n == ".." || n.ends_with(['.', ' ']) || n.contains(RESERVED_NAME_CHARS) || n.chars().any(char::is_control);
    if bad {
        return Err(format!(
            "app.newLibrary: {s:?} can't be a library name (a folder name without < > : \" / \\ | ? * and not ending in a dot or space)"
        ));
    }
    Ok(n)
}

/// `base` inside `location`, or `base 2`, `base 3`… if that is taken.
pub(crate) fn free_name(location: &std::path::Path, base: &str) -> std::path::PathBuf {
    let first = location.join(base);
    if !first.exists() {
        return first;
    }
    (2..1000u32).map(|i| location.join(format!("{base} {i}"))).find(|p| !p.exists()).unwrap_or(first)
}

/// `app.recentLibraries {remove?: path, clear?: bool}`: the recently opened libraries, newest
/// first, as `[{path, name, exists, checking, open, lastOpened}]`. `exists` is what the last
/// background check found (`false`: moved, on a disconnected drive, not a folder or no longer a
/// library; `null`: not checked yet, or the check couldn't tell — a folder that can't be listed, an
/// unreadable catalog, a lookup or check that failed; `checking` says whether a check is running): the command never
/// looks at a folder itself, as one on an offline network share can take seconds to answer.
/// `lastOpened`: ISO 8601 time, `null` when not known. `remove` forgets that
/// one entry (an error if the list doesn't hold it) and `clear` forgets them all; neither touches
/// anything on disk (issue #1290).
pub fn recent_libraries(app: &mut LightcraftApp, p: &Value) -> Result<Value, String> {
    match p.get("remove") {
        None | Some(Value::Null) => {}
        Some(Value::String(path)) => {
            if !forget_library(&mut app.ui.settings, path) {
                return Err(format!("app.recentLibraries: {path:?} is not in the recent libraries"));
            }
            app.recent_checks.forget(path);
        }
        Some(_) => return Err("app.recentLibraries: remove must be a library path (a string)".into()),
    }
    if p.get("clear").and_then(Value::as_bool) == Some(true) {
        for path in std::mem::take(&mut app.ui.settings.recent_libraries) {
            app.recent_checks.forget(&path);
        }
        app.ui.settings.recent_opened.clear();
    }
    let open = app.session.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.clone());
    let (settings, checks) = (&app.ui.settings, &app.recent_checks);
    let list: Vec<Value> = settings
        .recent_libraries
        .iter()
        .map(|r| {
            let d = std::path::Path::new(r);
            let exists = checks.exists(r);
            json!({"path": r, "name": library_name(d), "exists": exists, "checking": checks.is_checking(r), "open": open.as_deref() == Some(d), "lastOpened": settings.recent_opened.get(r)})
        })
        .collect();
    Ok(Value::Array(list))
}

/// Forget `path` in File ▸ Open Recent Library (and when it was opened); the library itself is
/// left where it is. False if the list didn't hold it.
pub fn forget_library(settings: &mut crate::state::AppSettings, path: &str) -> bool {
    let before = settings.recent_libraries.len();
    settings.recent_libraries.retain(|r| r != path);
    settings.recent_opened.remove(path);
    settings.recent_libraries.len() != before
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn app_with_import() -> LightcraftApp {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services::default());
        app.run("photo.setReference", json!({})).unwrap();
        app.import = Some(crate::import::ImportTask::new(vec!["a.jpg".into()], json!({"mode": "add"}), 0, false));
        app
    }

    #[test]
    fn open_library_is_refused_while_an_import_runs() {
        let mut app = app_with_import();
        let pin = app.ui.reference;
        let err = open_library(&mut app, &json!({"path": "other"})).unwrap_err();
        assert!(err.contains("import to finish"), "{err}");
        assert!(app.import.is_some());
        assert_eq!(app.ui.reference, pin, "a refused Open Library keeps the reference pin");
    }

    #[test]
    fn new_library_is_refused_while_an_import_runs() {
        let mut app = app_with_import();
        let pin = app.ui.reference;
        let err = new_library(&mut app, &json!({"path": "other"})).unwrap_err();
        assert!(err.contains("import to finish"), "{err}");
        assert!(app.import.is_some());
        assert_eq!(app.ui.reference, pin, "a refused New Library keeps the reference pin");
    }

    /// The menu items that switch libraries are greyed out meanwhile, like during a Lightroom import.
    #[test]
    fn library_switching_menu_items_are_disabled_while_an_import_runs() {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), crate::Services::default());
        app.services.pick_folder = Some(Box::new(|| None));
        assert!(crate::menus::ui_enabled(&app, "app.openLibrary"));
        assert!(crate::menus::ui_enabled(&app, "app.newLibrary"));
        app.import = Some(crate::import::ImportTask::new(vec!["a.jpg".into()], json!({"mode": "add"}), 0, false));
        assert!(!crate::menus::ui_enabled(&app, "app.openLibrary"));
        assert!(!crate::menus::ui_enabled(&app, "app.newLibrary"));
    }

    /// The folder scan before an import's review counts too: it ends in an import into the open library.
    #[test]
    fn open_library_is_refused_while_an_import_scan_runs() {
        let dir = std::env::temp_dir().join(format!("lc-ui-switchscan-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services::default());
        app.run("photo.setReference", json!({})).unwrap();
        let pin = app.ui.reference;
        crate::import::open(&mut app, vec![dir.to_string_lossy().into_owned()]).unwrap();
        assert!(app.scan.is_some());
        let err = open_library(&mut app, &json!({"path": "other"})).unwrap_err();
        assert!(err.contains("import to finish"), "{err}");
        assert_eq!(app.ui.reference, pin, "a refused Open Library during a scan keeps the reference pin");
        let err = new_library(&mut app, &json!({"path": "other"})).unwrap_err();
        assert!(err.contains("import to finish"), "{err}");
        assert_eq!(app.ui.reference, pin, "a refused New Library during a scan keeps the reference pin");
        assert!(app.scan.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
