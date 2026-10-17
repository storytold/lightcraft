//! `photo.trashFiles`: photos leave the library for good and their files go to the system Trash
//! (see [`crate::trash`]). The one command that touches a photo's file on the way out.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use lightcraft_catalog::{PhotoId, Source};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd};
use crate::rename::MoveFs;
use crate::trash::{BinError, TrashStep, Trashed, Whereabouts};
use crate::{Result, Session};

/// How many photos one call may name.
const MAX_PHOTOS: usize = 1_000_000;

/// What `noTrash` says to do with files on a disk without a Trash.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NoTrash {
    /// The photo and its file stay; the answer names them.
    Keep,
    /// Their files are erased for good.
    Erase,
}

fn no_trash_param(p: &Value, c: &str) -> Result<NoTrash> {
    match p.get("noTrash") {
        None | Some(Value::Null) => Ok(NoTrash::Keep),
        Some(v) => match v.as_str() {
            Some("keep") => Ok(NoTrash::Keep),
            Some("delete") => Ok(NoTrash::Erase),
            _ => Err(bad(c, "`noTrash` is \"keep\" (the default) or \"delete\"")),
        },
    }
}

/// What stays in the library, by folder: the names and the stems (name without the last
/// extension) of its photos' files, lower-cased. A sidecar one of them may use stays, also
/// when that photo's file is not there right now (a disk that is away).
#[derive(Default)]
struct Kept {
    names: HashSet<(PathBuf, String)>,
    stems: HashSet<(PathBuf, String)>,
}

impl Kept {
    fn add(&mut self, path: &Path) {
        let Some(folder) = path.parent() else { return };
        if let Some(name) = path.file_name() {
            self.names.insert((folder.to_path_buf(), name.to_string_lossy().to_lowercase()));
        }
        if let Some(stem) = path.file_stem() {
            self.stems.insert((folder.to_path_buf(), stem.to_string_lossy().to_lowercase()));
        }
    }
}

/// The sidecars beside `original` that are there now and are this photo's alone, and how many
/// are there but may be another file's (those stay). `.xmp` and `.XMP` are one file on a disk
/// that ignores case.
fn own_sidecars(original: &str, kept: &Kept) -> (Vec<PathBuf>, usize) {
    let fs = crate::rename::RealFs;
    let (mut own, mut left): (Vec<PathBuf>, usize) = (Vec::new(), 0);
    let mut seen: Vec<PathBuf> = Vec::new();
    for naming in [crate::sidecar::SidecarNaming::Stem, crate::sidecar::SidecarNaming::Full] {
        let lower = crate::sidecar::sidecar_path(original, naming);
        for path in [lower.clone(), lower.with_extension("XMP")] {
            if !fs.is_file(&path) || seen.iter().any(|o| fs.same_file(o, &path)) {
                continue;
            }
            seen.push(path.clone());
            if sidecar_is_free(Path::new(original), &path, kept) {
                own.push(path);
            } else {
                left += 1;
            }
        }
    }
    (own, left)
}

/// Whether `sidecar` (`S.xmp`) is nobody's but the photo's that was at `original` (gone from
/// the folder by now). A sidecar is `S`'s when a file is named `S` (`IMG_1.jpg.xmp` for
/// `IMG_1.jpg`) and the sidecar of everything whose name without its extension is `S`
/// (`IMG_1.xmp` for `IMG_1.dng`, `IMG_1.mov`…: other apps write those for files LightCraft
/// does not import). So it is free only when no other file in the folder is named `S` or has
/// the stem `S`, and no photo that stays in the library is such a file there. When in doubt (a
/// folder that can't be listed): not free.
fn sidecar_is_free(original: &Path, sidecar: &Path, kept: &Kept) -> bool {
    let (Some(folder), Some(of)) = (sidecar.parent(), sidecar.file_stem()) else { return false };
    let of = of.to_string_lossy().to_lowercase();
    let key = (folder.to_path_buf(), of.clone());
    if kept.names.contains(&key) || kept.stems.contains(&key) {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(folder) else { return false };
    for entry in entries {
        let Ok(entry) = entry else { return false };
        let path = entry.path();
        if path == sidecar || path == original || path.extension().is_some_and(|e| e.eq_ignore_ascii_case("xmp")) {
            continue;
        }
        let is = |part: Option<&std::ffi::OsStr>| part.is_some_and(|p| p.to_string_lossy().to_lowercase() == of);
        if is(path.file_name()) || is(path.file_stem()) {
            return false;
        }
    }
    true
}

/// Take `ids` out of the library for good and move their files to the Trash, as one undo step.
/// Everything that can go goes; what can't is named in the answer and stays, photo and file.
pub(crate) fn trash_files(s: &mut Session, ids: &[PhotoId], no_trash: NoTrashParam, c: &str) -> Result<Value> {
    let no_trash = no_trash.0;
    if !s.trash.works_here() {
        return Err(bad(c, "moving files to the Trash is not available here yet: nothing was changed"));
    }
    if ids.is_empty() {
        return Err(bad(c, "no photos to move to the Trash"));
    }
    if ids.len() > MAX_PHOTOS {
        return Err(bad(c, format!("more than {MAX_PHOTOS} photos in one call")));
    }
    if let Some(gone) = ids.iter().find(|id| s.catalog.photo(**id).is_none()) {
        return Err(bad(c, format!("no such photo {}", gone.0)));
    }
    // each photo once; photos only browsed in Local are not the library's to take out
    let mut named: Vec<PhotoId> = Vec::new();
    let mut once: HashSet<PhotoId> = HashSet::new();
    let mut browsed = 0usize;
    for id in ids.iter().filter(|id| once.insert(**id)) {
        if s.catalog.photo(*id).is_some_and(|p| p.local) {
            browsed += 1;
        } else {
            named.push(*id);
        }
    }
    if named.is_empty() {
        return Err(bad(c, "photos browsed in Local are not in the library: add them to My Photos first"));
    }
    let going: HashSet<PhotoId> = named.iter().copied().collect();
    // what stays (every other record, browsed ones too): a file one of them is stays with it,
    // and so does a stem sidecar one of them may share
    let fs = crate::rename::RealFs;
    let mut kept_names: HashMap<String, Vec<PathBuf>> = HashMap::new();
    let mut kept_beside = Kept::default();
    for p in s.catalog.photos().filter(|p| !going.contains(&p.id)) {
        let Source::File { path } = &p.source else { continue };
        let path = PathBuf::from(path);
        kept_beside.add(&path);
        if let Some(name) = path.file_name() {
            kept_names.entry(name.to_string_lossy().to_lowercase()).or_default().push(path);
        }
    }
    // (the same file under another spelling of its path counts: file identity, not text)
    let kept = |path: &Path| {
        let Some(name) = path.file_name() else { return false };
        kept_names.get(&name.to_string_lossy().to_lowercase()).is_some_and(|same| same.iter().any(|k| k == path || fs.same_file(k, path)))
    };
    let bin = s.trash.clone();
    let (mut removed, mut no_trash_ids, mut failed): (Vec<PhotoId>, Vec<u64>, Vec<Value>) = (Vec::new(), Vec::new(), Vec::new());
    let (mut taken, mut gone_paths): (Vec<Trashed>, Vec<String>) = (Vec::new(), Vec::new());
    let (mut missing, mut shared, mut sidecars_left) = (0usize, 0usize, 0usize);
    let mut done: Vec<PathBuf> = Vec::new();
    // photos named that stay after all (could not go, or no Trash): keepers for the sidecars
    let mut stayed: Vec<PathBuf> = Vec::new();
    // the photos' own files first; the sidecars after, once it is known which photos stay
    for id in &named {
        let Some(p) = s.catalog.photo(*id) else { continue };
        let Source::File { path } = &p.source else {
            // no file of its own (a demo scene): it just leaves
            removed.push(*id);
            continue;
        };
        let file = Path::new(path);
        let fail = |failed: &mut Vec<Value>, why: String| failed.push(json!({"id": id.0, "path": path, "why": why}));
        if kept(file) {
            shared += 1;
            removed.push(*id);
            continue;
        }
        if done.iter().any(|d| d == file) {
            // another of the photos named is this file, which went with it
            removed.push(*id);
            continue;
        }
        match std::fs::symlink_metadata(file) {
            Ok(_) => {}
            // gone from a folder that is there: the photo just leaves. A folder that isn't
            // there says nothing about the file (a disk that is away): the photo stays
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if file.parent().is_some_and(Path::is_dir) {
                    missing += 1;
                    removed.push(*id);
                } else {
                    fail(&mut failed, format!("{path}: its folder can't be reached (is the disk connected?)"));
                    stayed.push(file.to_path_buf());
                }
                continue;
            }
            // (it may well be there: only "not found" means gone)
            Err(e) => {
                fail(&mut failed, format!("{path}: can't be read: {e}"));
                stayed.push(file.to_path_buf());
                continue;
            }
        }
        if !fs.is_file(file) {
            fail(&mut failed, format!("{path}: not a file"));
            stayed.push(file.to_path_buf());
            continue;
        }
        match bin.trash(file) {
            Ok(at) => {
                taken
                    .push(Trashed { from: path.clone(), now: at.map_or(Whereabouts::InTrash, |a| Whereabouts::At(a.to_string_lossy().to_string())) });
                gone_paths.push(path.clone());
            }
            Err(BinError::NoTrash) if no_trash == NoTrash::Erase => match bin.erase(file) {
                // (its sidecars stay: only the file that was asked about is erased)
                Ok(()) => {
                    taken.push(Trashed { from: path.clone(), now: Whereabouts::Erased });
                    let (own, left) = own_sidecars(path, &kept_beside);
                    sidecars_left += own.len() + left;
                }
                Err(e) => {
                    fail(&mut failed, format!("{path}: can't be erased: {e}"));
                    stayed.push(file.to_path_buf());
                    continue;
                }
            },
            Err(BinError::NoTrash) => {
                no_trash_ids.push(id.0);
                stayed.push(file.to_path_buf());
                continue;
            }
            Err(BinError::Failed(why)) => {
                fail(&mut failed, why);
                stayed.push(file.to_path_buf());
                continue;
            }
        }
        done.push(file.to_path_buf());
        removed.push(*id);
    }
    // the sidecars of the files that went to the Trash follow them there: those that are theirs
    // alone. One that can't go stays behind (and is counted); nothing is lost by that.
    for path in &stayed {
        kept_beside.add(path);
    }
    for original in &gone_paths {
        let (own, left) = own_sidecars(original, &kept_beside);
        sidecars_left += left;
        for sidecar in own {
            match bin.trash(&sidecar) {
                Ok(at) => taken.push(Trashed {
                    from: sidecar.to_string_lossy().to_string(),
                    now: at.map_or(Whereabouts::InTrash, |a| Whereabouts::At(a.to_string_lossy().to_string())),
                }),
                Err(_) => sidecars_left += 1,
            }
        }
    }
    let erased = taken.iter().filter(|t| t.now == Whereabouts::Erased).count();
    let trashed = taken.len() - erased;
    if removed.is_empty() {
        // nothing went. A disk without a Trash is a question to ask, not an error; anything
        // else says why
        if failed.is_empty() {
            return Ok(json!({
                "removed": 0, "trashed": 0, "erased": 0, "missing": 0, "shared": 0, "sidecarsLeft": 0, "noTrash": no_trash_ids, "failed": [], "skipped": browsed
            }));
        }
        let why = failed.first().and_then(|f| f["why"].as_str()).unwrap_or("the files could not be moved to the Trash").to_string();
        let more = if failed.len() > 1 { format!(" (and {} more)", failed.len() - 1) } else { String::new() };
        return Err(bad(c, format!("{why}{more}: nothing was changed")));
    }
    let before = s.visible_cloned();
    let n = removed.len();
    let whose = if n == 1 { "Photo's".to_string() } else { format!("{n} Photos'") };
    let label = if trashed == 0 && erased > 0 { format!("Erase {whose} Files") } else { format!("Move {whose} Files to Trash") };
    let op = s.catalog.delete_photos_permanently_ops(&removed);
    if let Err(e) = s.commit(&label, op) {
        // the library did not take the change: the files go back where they were
        let (_, note) = TrashStep::PutBack(taken).run(bin.as_ref());
        return Err(crate::EngineError::Other(match note {
            Some(note) => format!("{e}; {note}"),
            None => e.to_string(),
        }));
    }
    if let Some(entry) = s.undo.last_mut() {
        entry.trash = Some(TrashStep::PutBack(taken));
    }
    super::library::select_after_deletion(s, &before);
    Ok(json!({
        "removed": n,
        "trashed": trashed,
        "erased": erased,
        "missing": missing,
        "shared": shared,
        "sidecarsLeft": sidecars_left,
        "noTrash": no_trash_ids,
        "failed": failed,
        "skipped": browsed,
    }))
}

/// `noTrash` as a command read it.
pub(crate) struct NoTrashParam(NoTrash);

impl NoTrashParam {
    pub(crate) fn read(p: &Value, c: &str) -> Result<NoTrashParam> {
        no_trash_param(p, c).map(NoTrashParam)
    }
}

pub fn specs() -> Vec<CommandSpec> {
    vec![cmd!(
        "photo.trashFiles",
        "Move Files to Trash",
        [],
        None,
        "{ids?, noTrash?: keep|delete} → {removed, trashed, erased, missing, shared, sidecarsLeft, noTrash: [ids], failed: [{id, path, why}], skipped} — the photos (the selection, or `ids`; in the library or in Recently Deleted) leave the library for good and their files, with their XMP sidecars, go to the system Trash; one undo step, and undo moves the files back when the Trash says where they are. `trashed` and `erased` count files. A file other photos of the library are (copies, by file identity) stays with them (`shared`). A sidecar goes only when it is the photo's alone: one named for the stem (`IMG_1.xmp`) stays while any other file of that stem is beside it or a photo of that stem stays in the library, and the sidecars of erased files always stay (`sidecarsLeft`). A file gone from a folder that is there is `missing` and its photo leaves; a photo whose folder can't be reached (a disk that is away) stays and is in `failed`. Photos on a disk without a Trash stay, file and all, and are named in `noTrash`: only a second call with `noTrash: \"delete\"` erases their files for good (undo then brings the photos back as missing). A photo whose file can't be moved stays and is in `failed`; when nothing could go, that is the error. Photos only browsed in Local are skipped. Not available on Windows or in the browser yet. Delete, Delete Permanently, Empty Recently Deleted and Remove Folder from Library never touch a file",
        // (not `has_selection`: the photos are named by `ids` as often as selected, and what is
        // there to do is decided, and said, below)
        always,
        |s, p| {
            const C: &str = "photo.trashFiles";
            let no_trash = NoTrashParam::read(p, C)?;
            if let Some(v) = p.get("ids").filter(|v| !v.is_null()) {
                let list = v.as_array().ok_or_else(|| bad(C, "`ids` must be a list of photo ids"))?;
                if let Some(x) = list.iter().find(|x| x.as_u64().is_none()) {
                    return Err(bad(C, format!("`ids` must be a list of photo ids, not {x}")));
                }
            }
            let ids = s.targets(p)?;
            trash_files(s, &ids, no_trash, C)
        }
    )]
}
