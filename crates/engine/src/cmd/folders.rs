//! The Classic Folders panel's disk and catalog operations: create / delete a folder on disk
//! (undoable), synchronise a folder with the catalog (new files, missing files, sidecars changed
//! by other programs), update a moved folder's location, and the disks with their free space.

use std::path::{Path, PathBuf};

use dac_catalog::{Op, PhotoId, Source};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, bool_or, cmd, str_param};
use crate::{Result, Session};

/// Depth and count limits of a folder scan (a hostile or huge tree can't run away).
const MAX_DEPTH: usize = 32;
const MAX_FILES: usize = 200_000;

/// Do a step's folder change on disk: rename `from` → `to`; with `to` empty, remove the (empty)
/// folder `from`; with `from` empty, create the folder `to`. Undo steps store the reverse.
pub(crate) fn folder_on_disk(from: &str, to: &str) -> std::result::Result<(), String> {
    match (from.is_empty(), to.is_empty()) {
        (false, false) => super::browse::rename_folder_on_disk(from, to),
        (false, true) => remove_empty(from),
        (true, false) => {
            if Path::new(to).exists() {
                return Err(format!("{to} already exists"));
            }
            std::fs::create_dir_all(to).map_err(|e| format!("{to}: {e}"))
        }
        (true, true) => Ok(()),
    }
}

/// Remove an empty folder; a folder with anything in it (hidden files too) is refused.
fn remove_empty(path: &str) -> std::result::Result<(), String> {
    let p = Path::new(path);
    if !p.is_dir() {
        return Err(format!("{path} is not a folder"));
    }
    let mut entries = std::fs::read_dir(p).map_err(|e| format!("{path}: {e}"))?;
    if entries.next().is_some() {
        return Err(format!("{path} is not empty"));
    }
    std::fs::remove_dir(p).map_err(|e| format!("{path}: {e}"))
}

/// The library photos stored at or below `folder`.
fn photos_in(s: &Session, folder: &Path) -> Vec<(PhotoId, PathBuf)> {
    s.catalog
        .photos()
        .filter(|p| !p.local && !p.deleted)
        .filter_map(|p| match &p.source {
            Source::File { path } => {
                let pb = PathBuf::from(path);
                pb.starts_with(folder).then_some((p.id, pb))
            }
            _ => None,
        })
        .collect()
}

/// Every supported file below `dir` (depth- and count-limited; unreadable folders are skipped).
fn scan(dir: &Path, subfolders: bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), 0usize)];
    while let Some((d, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            if out.len() >= MAX_FILES {
                return out;
            }
            let p = e.path();
            let hidden = p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with('.'));
            if hidden {
                continue;
            }
            match e.file_type() {
                Ok(t) if t.is_dir() => {
                    if subfolders && depth < MAX_DEPTH {
                        stack.push((p, depth + 1));
                    }
                }
                Ok(t) if t.is_file() && crate::import::is_supported(&p) => out.push(p),
                _ => {}
            }
        }
    }
    out.sort();
    out
}

/// The sidecar of `path` as it is now (either naming, `.xmp` or `.XMP`).
fn sidecar_stat(path: &str) -> Option<dac_catalog::SidecarStat> {
    for naming in [crate::sidecar::SidecarNaming::Stem, crate::sidecar::SidecarNaming::Full] {
        let p = crate::sidecar::sidecar_path(path, naming);
        for q in [p.clone(), p.with_extension("XMP")] {
            if let Ok(m) = std::fs::metadata(&q)
                && m.is_file()
            {
                let mtime = m
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
                    .unwrap_or(0);
                return Some(dac_catalog::SidecarStat { mtime, size: m.len() });
            }
        }
    }
    None
}

fn folder_param(p: &Value, c: &str) -> Result<String> {
    let path = str_param(p, "path").map(|x| x.trim_end_matches(['/', '\\'])).filter(|x| !x.is_empty()).ok_or_else(|| bad(c, "missing path"))?;
    Ok(path.to_string())
}

fn create(s: &mut Session, p: &Value) -> Result<Value> {
    let c = "folder.create";
    let parent = str_param(p, "parent").filter(|x| !x.is_empty()).ok_or_else(|| bad(c, "missing parent"))?;
    let name = str_param(p, "name")
        .map(str::trim)
        .filter(|n| !n.is_empty() && !n.contains(['/', '\\']) && *n != "." && *n != "..")
        .ok_or_else(|| bad(c, "give a folder name (no slashes)"))?;
    if !Path::new(parent).is_dir() {
        return Err(bad(c, format!("{parent} is not a folder")));
    }
    let path = Path::new(parent).join(name).to_string_lossy().to_string();
    folder_on_disk("", &path).map_err(|e| bad(c, e))?;
    // undo removes it again (only while it is empty)
    let undo = crate::FolderMove { from: path.clone(), to: String::new() };
    if let Err(e) = s.commit_with_folder(&format!("Create Folder “{name}”"), Op::Batch { ops: vec![] }, undo) {
        let _ = remove_empty(&path);
        return Err(e);
    }
    Ok(json!({"path": path}))
}

fn delete(s: &mut Session, p: &Value) -> Result<Value> {
    let c = "folder.delete";
    let path = folder_param(p, c)?;
    if !photos_in(s, Path::new(&path)).is_empty() {
        return Err(bad(c, "the folder holds library photos: remove them from the library first"));
    }
    remove_empty(&path).map_err(|e| bad(c, e))?;
    let name = Path::new(&path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let undo = crate::FolderMove { from: String::new(), to: path.clone() };
    if let Err(e) = s.commit_with_folder(&format!("Delete Folder “{name}”"), Op::Batch { ops: vec![] }, undo) {
        let _ = std::fs::create_dir_all(&path);
        return Err(e);
    }
    Ok(json!({"path": path}))
}

fn relocate(s: &mut Session, p: &Value) -> Result<Value> {
    let c = "folder.relocate";
    let from = folder_param(p, c)?;
    let to = str_param(p, "to").map(|x| x.trim_end_matches(['/', '\\'])).filter(|x| !x.is_empty()).ok_or_else(|| bad(c, "missing to"))?;
    if !Path::new(to).is_dir() {
        return Err(bad(c, format!("{to} is not a folder")));
    }
    let (src, dst) = (PathBuf::from(&from), PathBuf::from(to));
    let ops: Vec<Op> = photos_in(s, &src)
        .into_iter()
        .filter_map(|(id, path)| {
            let rel = path.strip_prefix(&src).ok()?;
            let np = dst.join(rel);
            let file_name = s.catalog.photo(id)?.file_name.clone();
            Some(Op::Relink { id, file_name, source: Source::File { path: np.to_string_lossy().to_string() }, format: None })
        })
        .collect();
    let n = ops.len();
    if n == 0 {
        return Err(bad(c, format!("no library photo is in {from}")));
    }
    let found = ops.iter().filter(|o| matches!(o, Op::Relink { source: Source::File { path }, .. } if Path::new(path).is_file())).count();
    s.commit("Update Folder Location", Op::Batch { ops })?;
    super::browse::follow_folder(s, &from, to);
    Ok(json!({"relinked": n, "found": found, "path": to}))
}

fn sync(s: &mut Session, p: &Value) -> Result<Value> {
    let c = "folder.sync";
    let path = folder_param(p, c)?;
    let dir = PathBuf::from(&path);
    if !dir.is_dir() {
        return Err(bad(c, format!("{path} is not a folder (offline or moved? see folder.relocate)")));
    }
    let subfolders = bool_or(p, "subfolders", true);
    let dry = bool_or(p, "dryRun", false);
    let known = photos_in(s, &dir);
    let known_paths: std::collections::HashSet<PathBuf> = known.iter().map(|(_, p)| p.clone()).collect();
    // anything the catalog has under the folder, Local records included, isn't new
    let any_known: std::collections::HashSet<PathBuf> = s
        .catalog
        .photos()
        .filter_map(|p| match &p.source {
            Source::File { path } => Some(PathBuf::from(path)),
            _ => None,
        })
        .filter(|p| p.starts_with(&dir))
        .collect();
    let new: Vec<String> = scan(&dir, subfolders).into_iter().filter(|f| !any_known.contains(f)).map(|f| f.to_string_lossy().to_string()).collect();
    let missing: Vec<u64> =
        known.iter().filter(|(_, p)| subfolders || p.parent() == Some(dir.as_path())).filter(|(_, p)| !p.is_file()).map(|(id, _)| id.0).collect();
    let changed: Vec<u64> = known
        .iter()
        .filter(|(_, p)| p.is_file())
        .filter_map(|(id, p)| {
            let ph = s.catalog.photo(*id)?;
            let st = ph.xmp_status(sidecar_stat(&p.to_string_lossy()));
            matches!(st, dac_catalog::XmpStatus::ChangedOnDisk | dac_catalog::XmpStatus::Conflict).then_some(id.0)
        })
        .collect();
    let _ = known_paths;
    let mut out = json!({"path": path, "new": new.len(), "newFiles": new.iter().take(50).collect::<Vec<_>>(), "missing": missing, "metadataChanged": changed, "dryRun": dry});
    if dry {
        return Ok(out);
    }
    if bool_or(p, "import", true) && !new.is_empty() {
        let r = s.execute("library.import", &json!({"paths": new, "mode": "add"}))?;
        out["imported"] = r.get("imported").map(|v| json!(v.as_array().map_or(0, Vec::len))).unwrap_or(json!(0));
    }
    if bool_or(p, "removeMissing", false) && !missing.is_empty() {
        s.execute("photo.delete", &json!({"ids": missing}))?;
        out["removed"] = json!(missing.len());
    }
    if bool_or(p, "readMetadata", false) && !changed.is_empty() {
        let r = s.execute("photo.readMetadataFromFile", &json!({"ids": changed}))?;
        out["read"] = r.get("read").cloned().unwrap_or(json!(0));
    }
    Ok(out)
}

/// Free and total bytes of the file system holding `path` (Unix, Windows; `None` elsewhere).
pub fn disk_space(path: &str) -> Option<(u64, u64)> {
    #[cfg(all(unix, not(target_arch = "wasm32")))]
    {
        let st = rustix::fs::statvfs(path).ok()?;
        let unit = st.f_frsize.max(1);
        let free = st.f_bavail.saturating_mul(unit);
        let total = st.f_blocks.saturating_mul(unit);
        return Some((free, total));
    }
    // Windows: GetDiskFreeSpaceExW behind fs4's safe API (no unsafe here)
    #[cfg(windows)]
    {
        let free = fs4::available_space(path).ok()?;
        let total = fs4::total_space(path).ok()?;
        return Some((free, total));
    }
    #[allow(unreachable_code)]
    {
        let _ = path;
        None
    }
}

fn volumes(s: &mut Session, _p: &Value) -> Result<Value> {
    let tree = s.catalog.folder_tree();
    let mut out = Vec::new();
    // a disk is a volume row of the tree; the startup disk (`/`) is the top row
    for n in tree.iter().take(256) {
        let space = disk_space(&n.path);
        out.push(json!({
            "path": n.path,
            "name": n.name,
            "photos": n.count,
            "online": Path::new(&n.path).is_dir(),
            "free": space.map(|x| x.0),
            "total": space.map(|x| x.1),
        }));
    }
    Ok(json!(out))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "folder.create",
            "Create Folder",
            [],
            None,
            "{parent, name} — create a folder on disk; one undo step (undo removes it again while it is empty) → {path}",
            always,
            create
        ),
        cmd!(
            "folder.delete",
            "Delete Empty Folder",
            [],
            None,
            "{path} — remove an empty folder from disk (one holding anything, or library photos, is refused); one undo step (undo creates it again) → {path}",
            always,
            delete
        ),
        cmd!(
            "folder.relocate",
            "Update Folder Location",
            [],
            None,
            "{path, to} — the folder was moved or renamed outside the app: point its library photos at `to` (nothing on disk changes); one undo step → {relinked, found, path}",
            always,
            relocate
        ),
        cmd!(
            "folder.sync",
            "Synchronize Folder",
            [],
            None,
            "{path, subfolders?: true, dryRun?: false, import?: true, removeMissing?: false, readMetadata?: false} — compare a folder with the library: files not in it are added (in place), photos whose file is gone are listed (moved to Recently Deleted with removeMissing), sidecars changed by another program are listed (read with readMetadata) → {new, newFiles, missing, metadataChanged, imported?, removed?, read?}",
            always,
            sync
        ),
        cmd!(query "library.volumes", "Volumes", [], None, "{} → [{path, name, photos, online, free, total}] the disks the library's photos are on (free/total bytes where the system tells)", always, volumes),
    ]
}
