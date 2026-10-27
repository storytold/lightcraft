use super::*;

/// The Delete key in an album: a normal album loses the selected photos (`album.removePhotos`),
/// which stay in the library and their folder; a smart album keeps them (its rules decide) and
/// says so. True when the key was handled here, false outside albums (then it deletes the photos
/// from the library). Photo ▸ Delete Photo still deletes from the library everywhere.
pub fn delete_in_album(app: &mut LightcraftApp, ctx: &egui::Context) -> bool {
    let lightcraft_engine::LibrarySource::Album(aid) = app.session.source else { return false };
    let Some(album) = app.session.catalog.album(aid) else { return false };
    if album.is_smart() {
        app.toast(ctx, crate::i18n::tr("Smart albums update automatically: change their rules, or use Photo ▸ Delete Photo"));
        return true;
    }
    if app.session.selected().is_empty() {
        return true;
    }
    match app.run("album.removePhotos", json!({"id": aid.0})) {
        Ok(r) => {
            let n = r["removed"].as_u64().unwrap_or(0);
            let name = r["album"].as_str().unwrap_or_default().to_string();
            let what = crate::i18n::tr_format!("{n} photo{}", if n == 1 { "" } else { "s" }, n = n);
            app.toast(ctx, crate::i18n::tr_format!("Removed {what} from {name}", what = what, name = name));
        }
        Err(e) => app.toast(ctx, e),
    }
    true
}

/// With Settings → General → "Confirm before deleting" on, open the confirmation dialog instead
/// of deleting; true when it did (the dialog's OK runs `photo.delete`).
pub fn confirm_delete(app: &mut LightcraftApp) -> bool {
    if !app.ui.settings.confirm_delete {
        return false;
    }
    let count = app.session.selected().len();
    if count == 0 {
        return false;
    }
    app.ui.dialog = Some(Dialog::ConfirmDelete { count });
    true
}

/// Platform-appropriate label for revealing a file in the system file manager.
pub fn reveal_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "Show in Finder"
    } else if cfg!(target_os = "windows") {
        "Show in Explorer"
    } else {
        "Show in File Manager"
    }
}

/// Help ▸ Open Log Folder: reveal the host's log file in the system file manager, so it can be
/// attached to a report without hunting for the settings folder (#260).
pub(super) fn open_log_folder(app: &mut LightcraftApp) -> Result<Value, String> {
    let path = app.services.log_file.clone().ok_or("this session keeps no log file")?;
    let reveal = app.services.reveal.as_mut().ok_or("not available here")?;
    reveal(&path)?;
    Ok(json!({"path": path}))
}

/// Reveal the active photo's original in the system file manager.
pub(super) fn show_in_finder(app: &mut LightcraftApp) -> Result<Value, String> {
    let id = app.session.active().ok_or("no photo selected")?;
    let path = match app.session.catalog.photo(id).map(|p| p.source.clone()) {
        Some(lightcraft_engine::catalog::Source::File { path }) => path,
        _ => return Err("this photo has no file (demo scene)".into()),
    };
    let reveal = app.services.reveal.as_mut().ok_or("not available here")?;
    reveal(&path)?;
    Ok(json!({"path": path}))
}

/// The folder the active photo was imported from, as the sidebar's Folders section counts it (a
/// photo straight in a disk's root is in the root). `None` for a demo scene, a photo only browsed
/// in Local or in Recently Deleted, and one on the startup disk's root, which has no row to show.
fn photo_folder(app: &LightcraftApp, id: Option<lightcraft_catalog::PhotoId>) -> Option<String> {
    let photo = id.or_else(|| app.session.active()).and_then(|id| app.session.catalog.photo(id)).filter(|p| p.in_library())?;
    let lightcraft_engine::catalog::Source::File { path } = &photo.source else { return None };
    let dir = path.rfind(['/', '\\']).and_then(|i| path.get(..i.max(1)))?;
    (!lightcraft_catalog::folders::is_startup_disk(dir)).then(|| dir.to_string())
}

/// Whether Show in Folders has a row to show for photo `id` (the active one when `None`): its
/// folder must be a row of the tree, which stops `MAX_DEPTH` levels down. With no fresh tree to
/// look in, the folder alone decides, and the command still checks.
pub fn show_in_folders_enabled(app: &LightcraftApp, id: Option<lightcraft_catalog::PhotoId>) -> bool {
    let Some(dir) = photo_folder(app, id) else { return false };
    app.caches.folder_tree_if_fresh(&app.session.catalog).is_none_or(|tree| crate::panels::left::folder_row_of(&tree, &dir).is_some())
}

/// Photo ▸ Show in Folders (Lightroom's Go to Folder in Library): show the folder of photo `id`
/// (the active one when `None`; the grid's menu passes the photo right-clicked) as a click on its
/// row in the sidebar's Folders section does, and bring the row into view: the sidebar, the
/// section and the folders above it open, and it scrolls there (`reveal_folder`).
pub(super) fn show_in_folders(app: &mut LightcraftApp, id: Option<lightcraft_catalog::PhotoId>) -> Result<Value, String> {
    let dir = photo_folder(app, id).ok_or("this photo is not in a folder of the library")?;
    let tree = app.caches.folder_tree(&app.session.catalog);
    // the row's own spelling (forward slashes): what its click hands `library.source`, and its id
    let path = crate::panels::left::folder_row_of(&tree, &dir).map(|n| n.path.clone()).ok_or("this photo's folder is not in Folders")?;
    app.run("library.source", json!({"kind": "libraryFolder", "path": path}))?;
    app.ui.left_panel = true;
    app.ui.reveal_folder = Some(path.clone());
    Ok(json!({"path": path}))
}

/// Photo ▸ Edit In: render a copy of each selected photo with its edits (stacked on its original)
/// as Settings ▸ External Editing asks for that editor, then open them there. `p` may override the
/// file (`format`, `colorSpace`, `bitDepth`) and the editor (`app`). PhotoCraft is checked first,
/// so a missing editor leaves no copy behind. Once the copies exist, an editor that won't open them
/// is reported in the result's `failed` (and a toast), not as an error.
pub(super) fn edit_in(app: &mut LightcraftApp, ctx: &egui::Context, p: &Value, photocraft: bool) -> Result<Value, String> {
    // every way in (menu, shortcut, context menu, the control channel) says why nothing opened
    edit_in_inner(app, ctx, p, photocraft).inspect_err(|e| app.toast(ctx, crate::i18n::tr_format!("Couldn't open the editor: {e}", e = e)))
}

fn edit_in_inner(app: &mut LightcraftApp, ctx: &egui::Context, p: &Value, photocraft: bool) -> Result<Value, String> {
    let settings = &app.ui.settings;
    let (file, configured) = if photocraft {
        let path = settings.photocraft_path.trim();
        (settings.photocraft_file, if path.is_empty() { crate::state::PHOTOCRAFT.to_string() } else { path.to_string() })
    } else {
        (settings.external_file, settings.external_editor.clone())
    };
    let editor = p.get("app").and_then(Value::as_str).map_or(configured, str::to_string);
    if let Some(check) = app.services.check_editor.as_ref() {
        check(&editor)?;
    }
    let mut params = file.params();
    if let (Some(o), Some(given)) = (params.as_object_mut(), p.as_object()) {
        for (k, v) in given.iter().filter(|(k, _)| k.as_str() != "app") {
            o.insert(k.clone(), v.clone());
        }
    }
    let r = app.session.execute("photo.editExternal", &params).map_err(|e| e.to_string())?;
    // every selected photo got a copy (#920); each opens in the editor
    let copies: Vec<(String, Option<u64>)> = r["copies"]
        .as_array()
        .map(|c| c.iter().map(|c| (c["path"].as_str().unwrap_or_default().to_string(), c["id"].as_u64())).collect())
        .unwrap_or_else(|| vec![(r["path"].as_str().unwrap_or_default().to_string(), r["id"].as_u64())]);
    let mut failed = None;
    for (path, id) in &copies {
        if let Some(id) = id {
            app.ui.external_edits.push(*id);
        }
        // the copy is LightCraft's own render, but it goes through the same gate as any other
        // path handed to the system opener (#2135)
        match lightcraft_engine::open_path::openable(path, "") {
            Err(e) => failed = Some(e),
            Ok(absolute) => {
                if let Some(Err(e)) = app.services.open_with.as_mut().map(|f| f(&absolute, &editor)) {
                    failed = Some(e);
                }
            }
        }
    }
    let mut r = r;
    if let Some(o) = r.as_object_mut() {
        o.insert("failed".into(), json!(failed));
    }
    if let Some(e) = failed {
        // the copies are in the library already (one undo step): an error would invite a retry
        // that makes another set, so the result says what failed and the toast says why
        app.toast(ctx, crate::i18n::tr_format!("Couldn't open the editor: {e}", e = e));
        return Ok(r);
    }
    match copies.as_slice() {
        [(path, _)] => {
            let name = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            app.toast(ctx, crate::i18n::tr_format!("{name} opened for editing; it is stacked with the original", name = name));
        }
        _ => app.toast(ctx, crate::i18n::tr_format!("{n} photos opened for editing; each is stacked with its original", n = copies.len())),
    }
    Ok(r)
}

/// Most originals one Open Original hands the editor (each starts it once).
const MAX_ORIGINALS: usize = 50;

/// Photo ▸ Edit In ▸ Open Original in Other Editor: hand each selected photo's original file
/// itself (a raw stays a raw: nothing is rendered, copied or added to the library) to the editor
/// set in Settings ▸ External Editing, e.g. a raw developer. `p` may give `ids` and the editor
/// (`app`). Demo photos and missing files are skipped and listed; when no photo has a file to
/// open, it fails saying why.
pub(super) fn open_original_in(app: &mut LightcraftApp, ctx: &egui::Context, p: &Value) -> Result<Value, String> {
    let r = open_originals(app, p);
    if let Err(e) = &r {
        app.toast(ctx, e.clone());
    }
    let r = r?;
    let opened = r["opened"].as_array().map_or(0, Vec::len);
    match (r["failed"].as_str(), r["opened"][0]["path"].as_str()) {
        (Some(e), _) => app.toast(ctx, crate::i18n::tr_format!("Couldn't open the editor: {e}", e = e)),
        (None, Some(path)) if opened == 1 => {
            let name = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            app.toast(ctx, crate::i18n::tr_format!("{name} opened in the other editor", name = name));
        }
        _ => app.toast(ctx, crate::i18n::tr_format!("{n} originals opened in the other editor", n = opened)),
    }
    Ok(r)
}

fn open_originals(app: &mut LightcraftApp, p: &Value) -> Result<Value, String> {
    let mut ids = app.session.targets(p).map_err(|e| e.to_string())?;
    ids.dedup();
    if ids.is_empty() {
        return Err("no photo selected".into());
    }
    if ids.len() > MAX_ORIGINALS {
        return Err(format!("{} photos selected: open at most {MAX_ORIGINALS} originals at a time", ids.len()));
    }
    let mut files = Vec::new();
    let mut skipped = Vec::new();
    for id in ids {
        // the path may come from an imported catalog: only a photo file LightCraft reads is
        // handed to the opener, never a program (#2135)
        match app.session.catalog.photo(id).map(|ph| (ph.file_name.clone(), ph.source.clone())) {
            Some((name, lightcraft_engine::catalog::Source::File { path })) => match lightcraft_engine::open_path::openable(&path, &name) {
                Ok(absolute) => files.push((id, absolute)),
                Err(reason) => skipped.push(json!({"id": id.0, "reason": reason})),
            },
            Some(_) => skipped.push(json!({"id": id.0, "reason": "a demo photo has no original file"})),
            None => skipped.push(json!({"id": id.0, "reason": "no such photo"})),
        }
    }
    if files.is_empty() {
        let why = skipped.first().and_then(|s| s["reason"].as_str()).unwrap_or("no original file");
        return Err(if skipped.len() == 1 {
            format!("nothing to open: {why}")
        } else {
            format!("nothing to open: none of these photos has its original file ({why})")
        });
    }
    let editor = p.get("app").and_then(Value::as_str).map_or_else(|| app.ui.settings.external_editor.clone(), str::to_string);
    if let Some(check) = app.services.check_editor.as_ref() {
        check(&editor)?;
    }
    // all at once where the host can: an editor that keeps one window (DxO PhotoLab) loses the
    // files that arrive while it is still starting
    if let Some(open_all) = app.services.open_all_with.as_mut() {
        let paths: Vec<String> = files.iter().map(|(_, path)| path.clone()).collect();
        open_all(&paths, &editor).map_err(|e| format!("Couldn't open the editor: {e}"))?;
        let opened: Vec<Value> = files.iter().map(|(id, path)| json!({"id": id.0, "path": path})).collect();
        return Ok(json!({"opened": opened, "skipped": skipped, "app": editor, "failed": null}));
    }
    let open = app.services.open_with.as_mut().ok_or("opening another application isn't available here")?;
    let mut opened = Vec::new();
    let mut failed = None;
    for (id, path) in files {
        match open(&path, &editor) {
            Ok(()) => opened.push(json!({"id": id.0, "path": path})),
            Err(e) => failed = Some(e),
        }
    }
    if opened.is_empty()
        && let Some(e) = failed
    {
        return Err(format!("Couldn't open the editor: {e}"));
    }
    Ok(json!({"opened": opened, "skipped": skipped, "app": editor, "failed": failed}))
}
