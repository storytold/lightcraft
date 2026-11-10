use super::{Dialog, LightcraftApp, Value, json};
use lightcraft_catalog::{PhotoId, Source};
use std::collections::HashSet;
use std::path::Path;

/// Ask before destroying the originals. The selected IDs and paths are frozen in the
/// dialog so an intervening selection change cannot redirect the deletion.
pub(super) fn request_delete_from_disk(app: &mut LightcraftApp) -> Result<Value, String> {
    if cfg!(target_arch = "wasm32") {
        return Err("Deleting originals from disk is available only in the desktop app".into());
    }
    let selected = app.session.selected();
    if selected.is_empty() {
        return Err("Select at least one photo to delete from disk".into());
    }
    let mut ids = Vec::with_capacity(selected.len());
    let mut paths = Vec::with_capacity(selected.len());
    let mut unique = HashSet::new();
    let mut skipped_no_original = 0usize;
    for id in selected {
        let Some(photo) = app.session.catalog.photo(id) else {
            return Err(format!("Photo {} no longer exists in the library", id.0));
        };
        // Generated demo scenes cannot have an original on disk. Skip those
        // instead of disabling disk deletion of the real photographs in the selection.
        let Source::File { path } = &photo.source else {
            skipped_no_original += 1;
            continue;
        };
        if !Path::new(path).is_absolute() || !unique.insert(path.clone()) {
            return Err("A selected photo has an invalid or duplicate original file path".into());
        }
        // Do not destroy an original referenced by another photo/virtual copy in the catalog.
        if app
            .session
            .catalog
            .photos()
            .any(|other| other.id != id && matches!(&other.source, Source::File { path: other_path } if other_path == path))
        {
            return Err(format!("The original {path} is referenced by another library photo; remove that reference first"));
        }
        ids.push(id.0);
        paths.push(path.clone());
    }
    let count = ids.len();
    if count == 0 {
        return Err("None of the selected photos has an original file on disk".into());
    }
    app.ui.dialog = Some(Dialog::ConfirmDeleteFromDisk { ids, paths, library: app.session.library_generation(), skipped_no_original });
    Ok(json!({"confirmationRequired": true, "count": count, "skippedNoOriginal": skipped_no_original}))
}

/// Called ONLY after the irreversible-deletion confirmation. Keep failed photos in the
/// catalog, including files that could not be removed due to permissions or a disconnected drive.
/// Sidecars and neighboring files are deliberately not removed.
pub(crate) fn delete_from_disk(app: &mut LightcraftApp, ids: &[u64], paths: &[String], library: u64) -> Result<Value, String> {
    if cfg!(target_arch = "wasm32") {
        return Err("Deleting originals from disk is available only in the desktop app".into());
    }
    if library != app.session.library_generation() {
        return Err("The library changed since the confirmation was opened; no files were deleted".into());
    }
    if ids.is_empty() || ids.len() != paths.len() {
        return Err("There are no valid original files to delete".into());
    }

    // Revalidate every record *before* removing anything. A catalog operation or an
    // automation could have changed the paths while the confirmation was open.
    let mut unique = HashSet::new();
    for (&id, path) in ids.iter().zip(paths) {
        let pid = PhotoId(id);
        let source_matches =
            app.session.catalog.photo(pid).is_some_and(|photo| matches!(&photo.source, Source::File { path: current } if current == path));
        if !source_matches || !Path::new(path).is_absolute() || !unique.insert(path) {
            return Err("The selected originals changed since the confirmation was opened; nothing was deleted".into());
        }
        if app
            .session
            .catalog
            .photos()
            .any(|other| other.id != pid && matches!(&other.source, Source::File { path: other_path } if other_path == path))
        {
            return Err(format!("{path} is still referenced by another photo; nothing was deleted"));
        }
    }

    let mut removed_ids = Vec::new();
    let mut failed = Vec::new();
    for (&id, path) in ids.iter().zip(paths) {
        // Never recursively delete a directory or follow a symlink/reparse-point.
        // A missing file is a failure, not permission to remove its catalog record.
        let disk_result = std::fs::symlink_metadata(path).and_then(|m| {
            if m.file_type().is_file() {
                std::fs::remove_file(path)
            } else {
                Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a regular file"))
            }
        });
        match disk_result {
            Ok(()) => removed_ids.push(id),
            Err(e) => failed.push(format!("{path}: {e}")),
        }
    }
    // A successful disk deletion is permanent. Remove only those photos from the catalog;
    // if a catalog write fails, report that fact rather than claiming the files still exist.
    if !removed_ids.is_empty() {
        app.run("photo.deletePermanently", json!({"ids": removed_ids}))
            .map_err(|e| format!("Original files were deleted, but their library records could not be removed: {e}"))?;
    }
    if !failed.is_empty() {
        return Err(format!(
            "Deleted {} original file(s). {} failed and remain in the library:\n{}",
            removed_ids.len(),
            failed.len(),
            failed.join("\n")
        ));
    }
    Ok(json!({"deletedFromDisk": removed_ids.len()}))
}

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

#[cfg(all(test, not(target_arch = "wasm32")))]
mod disk_deletion_tests {
    use super::*;
    use lightcraft_catalog::{Op, Photo};

    fn original_path() -> std::path::PathBuf {
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
        std::env::temp_dir().join(format!("lightcraft-delete-original-{}-{stamp}.jpg", std::process::id()))
    }

    // Test setup should fail immediately if it cannot build the in-memory fixture.
    #[allow(clippy::unwrap_used)]
    fn app_for(path: &Path) -> LightcraftApp {
        let mut session = lightcraft_engine::Session::new();
        let photo = Photo::new(PhotoId(1), Source::File { path: path.to_string_lossy().into_owned() }, "test.jpg", "JPEG", 1, 1, "2026-11-09");
        session.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        session.execute("library.select", &json!({"ids": [1]})).unwrap();
        LightcraftApp::new(session, crate::Services { png: None, ..Default::default() })
    }

    #[test]
    fn shift_delete_is_the_default_shortcut_and_opens_confirmation_without_deleting() {
        let path = original_path();
        std::fs::write(&path, b"test").unwrap();
        let mut app = app_for(&path);
        assert_eq!(crate::shortcuts::shortcut_of(&app.ui.settings.keymap, "photo.deleteFromDisk"), Some("Shift+Delete"));
        request_delete_from_disk(&mut app).unwrap();
        assert!(matches!(app.ui.dialog, Some(Dialog::ConfirmDeleteFromDisk { .. })));
        assert!(path.exists(), "requesting deletion must never delete the file");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn confirmed_delete_removes_original_and_catalog_record() {
        let path = original_path();
        std::fs::write(&path, b"test").unwrap();
        let mut app = app_for(&path);
        request_delete_from_disk(&mut app).unwrap();
        let Some(Dialog::ConfirmDeleteFromDisk { ids, paths, library, .. }) = app.ui.dialog.take() else {
            panic!("expected disk deletion confirmation");
        };
        let result = delete_from_disk(&mut app, &ids, &paths, library).unwrap();
        assert_eq!(result["deletedFromDisk"], 1);
        assert!(!path.exists());
        assert!(app.session.catalog.photo(PhotoId(1)).is_none());
    }

    #[test]
    fn recently_deleted_photos_can_still_delete_originals_from_disk() {
        let path = original_path();
        std::fs::write(&path, b"trash-test").unwrap();
        let mut app = app_for(&path);
        // This is the state of the screen in the user's screenshot: the photo
        // is in Recently Deleted, and the selected original still exists.
        app.session.catalog.apply(Op::SetDeleted { id: PhotoId(1), deleted: true }).unwrap();
        app.session.source = lightcraft_engine::LibrarySource::RecentlyDeleted;
        assert!(app.session.catalog.photo(PhotoId(1)).unwrap().deleted);
        assert!(crate::menus::ui_enabled(&app, "photo.deleteFromDisk"));
        assert_eq!(crate::shortcuts::shortcut_of(&app.ui.settings.keymap, "photo.deleteFromDisk"), Some("Shift+Delete"));

        let request = app.run("photo.deleteFromDisk", json!({})).unwrap();
        assert_eq!(request["confirmationRequired"], true);
        assert!(path.exists(), "choosing the menu item must only open a confirmation");

        let Some(Dialog::ConfirmDeleteFromDisk { ids, paths, library, .. }) = app.ui.dialog.take() else {
            panic!("expected a confirmation dialog for the Recently Deleted selection");
        };
        let result = delete_from_disk(&mut app, &ids, &paths, library).unwrap();
        assert_eq!(result["deletedFromDisk"], 1);
        assert!(!path.exists(), "the confirmed action should delete the original on disk");
        assert!(app.session.catalog.photo(PhotoId(1)).is_none(), "the catalog record should also be removed");
    }

    #[test]
    fn mixed_recently_deleted_selection_skips_demo_scenes_and_deletes_real_files_only() {
        let path = original_path();
        std::fs::write(&path, b"real-image").unwrap();
        let mut app = app_for(&path);
        let demo = Photo::new(PhotoId(2), Source::Demo { scene: 1 }, "demo", "Demo", 1, 1, "2026-11-09");
        app.session.catalog.apply(Op::AddPhoto { photo: Box::new(demo) }).unwrap();
        for id in [PhotoId(1), PhotoId(2)] {
            app.session.catalog.apply(Op::SetDeleted { id, deleted: true }).unwrap();
        }
        app.session.source = lightcraft_engine::LibrarySource::RecentlyDeleted;
        app.session.execute("library.select", &json!({"ids": [1, 2]})).unwrap();

        assert!(crate::menus::ui_enabled(&app, "photo.deleteFromDisk"));
        let response = app.run("photo.deleteFromDisk", json!({})).unwrap();
        assert_eq!(response["count"], 1);
        assert_eq!(response["skippedNoOriginal"], 1);
        assert!(path.exists());
        let Some(Dialog::ConfirmDeleteFromDisk { ids, paths, library, .. }) = app.ui.dialog.take() else {
            panic!("expected confirmation for the single real original");
        };
        assert_eq!(ids, vec![1]);
        assert_eq!(delete_from_disk(&mut app, &ids, &paths, library).unwrap()["deletedFromDisk"], 1);
        assert!(!path.exists());
        assert!(app.session.catalog.photo(PhotoId(1)).is_none());
        assert!(app.session.catalog.photo(PhotoId(2)).is_some(), "demo scenes have no disk original and remain untouched");
    }

    #[test]
    fn failed_file_removal_keeps_photo_in_catalog() {
        let path = original_path();
        std::fs::write(&path, b"test").unwrap();
        let mut app = app_for(&path);
        request_delete_from_disk(&mut app).unwrap();
        let Some(Dialog::ConfirmDeleteFromDisk { ids, paths, library, .. }) = app.ui.dialog.take() else {
            panic!("expected disk deletion confirmation");
        };
        std::fs::remove_file(&path).unwrap();
        assert!(delete_from_disk(&mut app, &ids, &paths, library).is_err());
        assert!(app.session.catalog.photo(PhotoId(1)).is_some());
    }
}
