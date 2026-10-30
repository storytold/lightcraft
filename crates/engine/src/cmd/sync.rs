//! Synchronize Folder commands (see [`crate::sync`]) and locating a folder that is somewhere
//! else now (see [`crate::relocate`]).

use serde_json::{Value, json};

use super::{CommandSpec, always, bad, bool_or, cmd, str_param};
use crate::sync::{SyncChoice, scan, synchronize};
use crate::{Result, Session};

fn path(p: &Value, c: &str) -> Result<String> {
    str_param(p, "path").filter(|d| !d.trim().is_empty()).map(str::to_string).ok_or_else(|| bad(c, "missing `path`"))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            query "folder.scanChanges",
            "Scan Folder for Changes",
            [],
            None,
            "{path, disk?: bool (a whole disk or share, or a folder holding disks)} — compare a folder of the library (and the folders inside it) with the disk; changes nothing → {path, new: [{path, name, format, …}] files the library doesn't have, duplicates: files whose content it already has, unreadable: [{path, error}], missing: [{id, path}] photos whose file is gone, moved: [{id, from, to}] photos whose file is elsewhere in the folder (same content), metadata: [{id, path}] photos whose XMP sidecar was saved after they came in or were last edited here and says something the library doesn't}",
            always,
            |s, p| {
                const C: &str = "folder.scanChanges";
                let path = path(p, C)?;
                let changes = scan(s, &path, bool_or(p, "disk", false)).map_err(|e| bad(C, e.to_string()))?;
                let v = serde_json::to_value(&changes).unwrap_or_default();
                s.folder_changes = Some(changes);
                Ok(v)
            }
        ),
        cmd!(
            "folder.synchronize",
            "Synchronize Folder",
            [],
            None,
            "{path, disk?: bool, scanned?: bool, importNew?: true, relinkMoved?: true, removeMissing?: false, readMetadata?: false} — bring a folder of the library up to date with the disk (see folder.scanChanges): import its new files in place, relink photos whose file was renamed or moved within it, move photos whose file is gone to Recently Deleted, read XMP sidecars saved by other apps (the sidecar wins); acts on the last folder.scanChanges of the folder while it is current (the folder's photos unchanged), else scans again — or, with `scanned: true`, refuses; one undo step, no file is touched → {imported, relinked, removed, read, failed: [[path, error]]}",
            always,
            sync
        ),
        cmd!(
            "folder.updateLocation",
            "Update Folder Location",
            [],
            None,
            "{folder, path, disk?: bool (a whole disk or share, or a folder holding disks), incomplete?: false} — say where a folder of the library is now (moved or renamed outside LightCraft, on a disk mounted elsewhere, or named by a catalog from another computer, such as `D:\\Photos` on Linux): its photos, those in the folders inside it and in Recently Deleted too, point at the same places below `path`, and the folders' colour labels follow; only the catalog changes, no file is touched; a file there counts as found when it has the size the library recorded (`unverified`: those it recorded no size for), and one of another size is `different`; refused when none of the folder's files are in `path`, when only some are (unless `incomplete`: files that aren't there stay missing there), when a file there is already in the library or was rated or edited while browsing it in Local (untouched Local records of the files give way to the library's photos), or, whatever `incomplete` says, when a file there is another photo's of this folder (`path` inside `folder` or the reverse) and its content is not known to be the photo's that would point at it; `path` is written without `..`; one undo step → {folder, path, relinked, found, unverified, different, notFound, replacedLocal} (in the running app the files are looked for in the background: → {background: true}, and `ui.inspect` → `relocate.last` is this result, `{error, folder, path}` or `{cancelled: true, folder, path}`)",
            always,
            |s, p| {
                const C: &str = "folder.updateLocation";
                let text = |key: &str| str_param(p, key).filter(|d| !d.trim().is_empty()).ok_or_else(|| bad(C, format!("missing `{key}`")));
                let (folder, to) = (text("folder")?, text("path")?);
                crate::relocate::relocate(s, folder, to, bool_or(p, "disk", false), bool_or(p, "incomplete", false))
                    .map_err(|e| bad(C, e.to_string()))
            }
        ),
    ]
}

fn sync(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "folder.synchronize";
    let path = path(p, C)?;
    let d = SyncChoice::default();
    let choice = SyncChoice {
        import_new: bool_or(p, "importNew", d.import_new),
        relink_moved: bool_or(p, "relinkMoved", d.relink_moved),
        remove_missing: bool_or(p, "removeMissing", d.remove_missing),
        read_metadata: bool_or(p, "readMetadata", d.read_metadata),
    };
    let changes = match s.take_folder_changes(&path) {
        Some(c) => c,
        // the person agreed to what a scan showed: never act on something else
        None if bool_or(p, "scanned", false) => return Err(bad(C, format!("{path} changed since it was scanned: scan again"))),
        None => scan(s, &path, bool_or(p, "disk", false)).map_err(|e| bad(C, e.to_string()))?,
    };
    let r = synchronize(s, changes, choice)?;
    Ok(json!({"imported": r.imported, "relinked": r.relinked, "removed": r.removed, "read": r.read, "failed": r.failed}))
}
