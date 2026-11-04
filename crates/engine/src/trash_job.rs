//! The work of `photo.trashFiles` (see [`crate::trash`]), in steps, so that the files can be moved
//! without the session in hand (the command runs them one after the other):
//!
//! 1. [`TrashPlan::new`] (session): the checks, and what stays in the library that may be one of
//!    the files: the photos in each file's own folder, and the photos of the same name elsewhere.
//! 2. [`TrashPlan::survey`] (any thread): each folder's real path, and whether its disk has a
//!    Trash, before any file moves, so the person can be asked about erasing first.
//! 3. [`TrashPlan::resolve`] (session): the photos recorded under a folder's real path count as
//!    being in that folder too.
//! 4. [`TrashPlan::work`] (any thread, never the catalog): the files go, one by one, until it is
//!    cancelled; then the sidecars of those that went.
//! 5. [`TrashWork::finish`] (session): the photos whose files went leave the library, as one undo
//!    step.
//!
//! Whether a file is another photo's too is asked only of the photos in its own folder (any
//! letter case, and that folder reached through a link), so a name that is common across a big
//! library costs nothing. Before a file is erased, every photo of that name in the library is
//! checked, links included.
//!
//! `photo.trashFiles` and `library.emptyRecentlyDeleted {files: "trash"}`, the commands that run
//! these steps, come in the next pull request; until then nothing calls them, so no file is
//! moved or erased by the app, and the unit tests are their only caller.

// (`photo.trashFiles`, the next pull request, is the caller of `work` and everything it reaches;
// until then only the tests use them)
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use lightcraft_catalog::{PhotoId, Source, file_name_index_key, folder_index_key, photo_folder_index_key};
use serde_json::{Value, json};

use crate::activity::TaskHandle;
use crate::cmd::bad;
use crate::rename::MoveFs;
use crate::sidecar::{SidecarNaming, sidecar_path};
use crate::trash::{Bin, BinError, TrashHere, TrashStep, Trashed, Whereabouts};
use crate::{EngineError, Result, Session};

/// From this many originals gone from one folder, the folder is listed once instead of probed per
/// photo: a cold SMB listing took a median 98 ms, one photo's 4 sidecar probes 12 ms (2026-10-20).
const LIST_FOLDER_AT: usize = 8;

/// What a panic in the work is reported as.
const WHAT: &str = "move files to the Trash";

/// What stays in the library, by folder (its folder-index key): the names and the stems (name
/// without the last extension) of its photos' files, lower-cased. A sidecar one of them may use
/// stays, also when that photo's file is not there right now (a disk that is away).
#[derive(Default)]
struct Kept {
    names: HashSet<(String, String)>,
    stems: HashSet<(String, String)>,
}

impl Kept {
    fn add_under(&mut self, key: &str, path: &Path) {
        if let Some(name) = path.file_name() {
            self.names.insert((key.to_string(), name.to_string_lossy().to_lowercase()));
        }
        if let Some(stem) = path.file_stem() {
            self.stems.insert((key.to_string(), stem.to_string_lossy().to_lowercase()));
        }
    }

    fn add(&mut self, path: &str) {
        if let Some(key) = photo_folder_index_key(path) {
            self.add_under(&key, Path::new(path));
        }
    }

    /// A photo that stays in folder `key` is named `of` or has the stem `of`.
    fn holds(&self, key: &str, of: &str) -> bool {
        let k = (key.to_string(), of.to_string());
        self.names.contains(&k) || self.stems.contains(&k)
    }
}

/// A photo named whose file is to go.
struct Planned {
    id: PhotoId,
    path: String,
    file: PathBuf,
    folder: PathBuf,
    /// [`photo_folder_index_key`] of `path`.
    key: String,
    /// [`file_name_index_key`] of `path`.
    name: String,
}
/// The path of photo `id`'s file (`None`: no such photo, or no file).
fn file_of(s: &Session, id: PhotoId) -> Option<&str> {
    match &s.catalog.photo(id)?.source {
        Source::File { path } => Some(path),
        Source::Demo { .. } => None,
    }
}

/// The plan of one Move Files to Trash: made on the session's thread, then surveyed and worked off
/// it.
pub struct TrashPlan {
    named: Vec<Planned>,
    /// Named photos without a file (demo scenes): they just leave.
    no_file: Vec<PhotoId>,
    /// Photos only browsed in Local, left alone.
    browsed: usize,
    going: HashSet<PhotoId>,
    /// (folder key, lower-cased name) → the files of photos that stay in that folder (or in it
    /// reached through a link) with that name.
    near: HashMap<(String, String), Vec<PathBuf>>,
    /// Lower-cased name → the files of photos that stay, with their folder key, wherever they are.
    far: HashMap<String, Vec<(PathBuf, String)>>,
    beside: Kept,
    bin: Arc<dyn Bin>,
}

/// What [`TrashPlan::survey`] found out about each folder, before any file moved.
pub struct Survey {
    folders: HashMap<PathBuf, Surveyed>,
}

struct Surveyed {
    trash: TrashHere,
    canonical: Option<PathBuf>,
}

impl Survey {
    fn trash_of(&self, folder: &Path) -> TrashHere {
        self.folders.get(folder).map_or(TrashHere::Unknown, |f| f.trash)
    }
}

impl TrashPlan {
    /// The checks `photo.trashFiles` makes before anything (in its words), and what stays.
    pub fn new(s: &Session, ids: &[PhotoId], c: &str) -> Result<TrashPlan> {
        if !s.trash.works_here() {
            return Err(bad(c, "moving files to the Trash is not available here yet: nothing was changed"));
        }
        if ids.is_empty() {
            return Err(bad(c, "no photos to move to the Trash"));
        }
        if let Some(why) = crate::trash::too_many(ids.len()) {
            return Err(bad(c, why));
        }
        if let Some(gone) = ids.iter().find(|id| s.catalog.photo(**id).is_none()) {
            return Err(bad(c, format!("no such photo {}", gone.0)));
        }
        // each photo once; photos only browsed in Local are not the library's to take out
        let mut named_ids: Vec<PhotoId> = Vec::new();
        let mut once: HashSet<PhotoId> = HashSet::new();
        let mut browsed = 0usize;
        for id in ids.iter().filter(|id| once.insert(**id)) {
            if s.catalog.photo(*id).is_some_and(|p| p.local) {
                browsed += 1;
            } else {
                named_ids.push(*id);
            }
        }
        if named_ids.is_empty() {
            return Err(bad(c, "photos browsed in Local are not in the library: add them to My Photos first"));
        }
        let going: HashSet<PhotoId> = named_ids.iter().copied().collect();
        let (mut named, mut no_file) = (Vec::new(), Vec::new());
        for id in &named_ids {
            match file_of(s, *id) {
                Some(path) => named.push(Planned {
                    id: *id,
                    path: path.to_string(),
                    file: PathBuf::from(path),
                    folder: Path::new(path).parent().map(Path::to_path_buf).unwrap_or_default(),
                    key: photo_folder_index_key(path).unwrap_or_default(),
                    name: file_name_index_key(path).unwrap_or_default(),
                }),
                None => no_file.push(*id),
            }
        }
        // what stays in each file's own folder (every other record, browsed ones too): a file one
        // of them is stays with it, and so does a stem sidecar one of them may share
        let (mut near, mut beside) = (HashMap::new(), Kept::default());
        let mut keys: HashSet<&str> = HashSet::new();
        for p in &named {
            if !keys.insert(&p.key) {
                continue;
            }
            for id in s.catalog.photos_in_folder_any_case(&p.key) {
                if going.contains(&id) {
                    continue;
                }
                let Some(path) = file_of(s, id) else { continue };
                near.entry((p.key.clone(), file_name_index_key(path).unwrap_or_default())).or_insert_with(Vec::new).push(PathBuf::from(path));
                beside.add_under(&p.key, Path::new(path));
            }
        }
        // the photos of the same name elsewhere: asked about only before a file is erased
        let mut far: HashMap<String, Vec<(PathBuf, String)>> = HashMap::new();
        for p in &named {
            if far.contains_key(&p.name) {
                continue;
            }
            let mut seen: HashSet<&str> = HashSet::new();
            let list = s
                .catalog
                .photos_named_any_case(&p.name)
                .into_iter()
                .filter(|id| !going.contains(id))
                .filter_map(|id| file_of(s, id))
                .filter(|path| seen.insert(path))
                .filter_map(|path| photo_folder_index_key(path).map(|key| (PathBuf::from(path), key)))
                .collect();
            far.insert(p.name.clone(), list);
        }
        Ok(TrashPlan { named, no_file, browsed, going, near, far, beside, bin: s.trash.clone() })
    }

    /// The photos named that have a file (for erasing all of them that are on a disk without a
    /// Trash, as `noTrash: "delete"` says).
    pub fn file_ids(&self) -> HashSet<PhotoId> {
        self.named.iter().map(|p| p.id).collect()
    }

    /// Each folder's real path, and whether its disk has a Trash. Folders not reached before
    /// `cancel` stay unknown. No file is moved.
    pub fn survey(&self, cancel: &AtomicBool, row: Option<&TaskHandle>) -> Survey {
        let mut order: Vec<&Path> = Vec::new();
        let mut seen: HashSet<&Path> = HashSet::new();
        for p in &self.named {
            if seen.insert(&p.folder) {
                order.push(&p.folder);
            }
        }
        let total = order.len() as u64;
        let mut folders = HashMap::new();
        for (done, folder) in (1u64..).zip(order) {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let surveyed = match std::fs::canonicalize(folder) {
                Ok(c) => Surveyed { trash: self.bin.trash_here(folder), canonical: Some(c) },
                // (unreachable now: each file says so when it is tried)
                Err(_) => Surveyed { trash: TrashHere::Unknown, canonical: None },
            };
            folders.insert(folder.to_path_buf(), surveyed);
            if let Some(r) = row {
                r.progress(done, total);
            }
        }
        Survey { folders }
    }

    /// The photos recorded under a folder's real path (the folder named through a link) count as
    /// being in that folder too. (A photo recorded through a link while the real path is named is
    /// not looked for: the file goes to the Trash, from where Undo brings it back.)
    pub fn resolve(&mut self, s: &Session, survey: &Survey) {
        let mut done: HashSet<&Path> = HashSet::new();
        for p in &self.named {
            if !done.insert(&p.folder) {
                continue;
            }
            let Some(real) = survey.folders.get(&p.folder).and_then(|f| f.canonical.as_ref()) else { continue };
            let real_key = folder_index_key(&real.to_string_lossy());
            if real_key == p.key {
                continue;
            }
            for id in s.catalog.photos_in_folder_any_case(&real_key) {
                if self.going.contains(&id) {
                    continue;
                }
                let Some(path) = file_of(s, id) else { continue };
                self.near.entry((p.key.clone(), file_name_index_key(path).unwrap_or_default())).or_default().push(PathBuf::from(path));
                self.beside.add_under(&p.key, Path::new(path));
            }
        }
    }

    /// Move the files, one by one: to the Trash, or erased when their photo is in `erase` and
    /// their disk has none. Stops after the file in hand when `cancel` is set. Never touches the
    /// catalog, and always returns (a panic is one photo's failure).
    pub(crate) fn work(self, survey: Survey, erase: &HashSet<PhotoId>, cancel: &AtomicBool, row: Option<&TaskHandle>, fs: &dyn MoveFs) -> TrashWork {
        let TrashPlan { named, no_file, browsed, going: _, near, far, mut beside, bin } = self;
        let mut w = Worker {
            bin: bin.as_ref(),
            fs,
            survey: &survey,
            near: &near,
            far: &far,
            erase,
            cancel,
            confirmed: HashMap::new(),
            canon: HashMap::new(),
            done: HashSet::new(),
            removed: no_file,
            groups: Vec::new(),
            gone: Vec::new(),
            stayed: Vec::new(),
            missing: HashSet::new(),
            shared: HashSet::new(),
            no_trash: Vec::new(),
            failed: Vec::new(),
            stopped: 0,
        };
        let total = named.len() as u64;
        for (i, p) in named.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                for q in named.iter().skip(i) {
                    w.stopped += 1;
                    w.stayed.push(q.path.clone());
                }
                break;
            }
            if let Err(why) = crate::guard::catch(WHAT, || w.one(p)) {
                w.failed.push(json!({"id": p.id.0, "path": p.path, "why": why}));
                w.stayed.push(p.path.clone());
            }
            if let Some(r) = row {
                r.progress(i as u64 + 1, total);
            }
        }
        // the sidecars of the files that went to the Trash follow them there: those that are
        // theirs alone. One that can't go stays behind (and is counted); nothing is lost by that.
        for path in &w.stayed {
            beside.add(path);
        }
        let mut sidecars =
            Sidecars { fs, bin: bin.as_ref(), beside: &beside, listings: HashMap::new(), taken: HashSet::new(), per_folder: HashMap::new(), left: 0 };
        for g in &w.gone {
            *sidecars.per_folder.entry(g.folder.clone()).or_default() += 1;
        }
        for g in &w.gone {
            let mut found = Vec::new();
            if crate::guard::catch(WHAT, || sidecars.of(g, &mut found)).is_err() {
                // (the lookup broke off: at least one sidecar may be left)
                sidecars.left += 1;
            }
            if let Some((_, group)) = w.groups.get_mut(g.group) {
                group.append(&mut found);
            }
        }
        TrashWork {
            removed: w.removed,
            groups: w.groups,
            missing: w.missing,
            shared: w.shared,
            sidecars_left: sidecars.left,
            no_trash: w.no_trash,
            failed: w.failed,
            stopped: w.stopped,
            browsed,
            bin,
        }
    }
}

/// An original that went, to the Trash or erased.
struct Gone {
    path: String,
    folder: PathBuf,
    key: String,
    erased: bool,
    /// Its place in [`Worker::groups`].
    group: usize,
}

struct Worker<'a> {
    bin: &'a dyn Bin,
    fs: &'a dyn MoveFs,
    survey: &'a Survey,
    near: &'a HashMap<(String, String), Vec<PathBuf>>,
    far: &'a HashMap<String, Vec<(PathBuf, String)>>,
    erase: &'a HashSet<PhotoId>,
    cancel: &'a AtomicBool,
    /// What a real attempt said about a folder's Trash this run (over the survey's hint).
    confirmed: HashMap<PathBuf, TrashHere>,
    /// Real paths of folders, asked once each.
    canon: HashMap<PathBuf, Option<PathBuf>>,
    /// Files that went (another photo named may be the same file).
    done: HashSet<PathBuf>,
    removed: Vec<PhotoId>,
    groups: Vec<(PhotoId, Vec<Trashed>)>,
    gone: Vec<Gone>,
    /// Paths of the photos named that stay after all: keepers for the sidecars.
    stayed: Vec<String>,
    /// Photos that leave without a file moved: gone from their folder, or another photo's file.
    missing: HashSet<PhotoId>,
    shared: HashSet<PhotoId>,
    no_trash: Vec<u64>,
    failed: Vec<Value>,
    stopped: usize,
}

impl Worker<'_> {
    fn fail(&mut self, p: &Planned, why: String) {
        self.failed.push(json!({"id": p.id.0, "path": p.path, "why": why}));
        self.stayed.push(p.path.clone());
    }

    fn went(&mut self, p: &Planned, now: Whereabouts) {
        let erased = now == Whereabouts::Erased;
        self.gone.push(Gone { path: p.path.clone(), folder: p.folder.clone(), key: p.key.clone(), erased, group: self.groups.len() });
        self.groups.push((p.id, vec![Trashed { from: p.path.clone(), now }]));
        self.done.insert(p.file.clone());
        self.removed.push(p.id);
    }

    fn canonical(&mut self, folder: &Path) -> Option<PathBuf> {
        if let Some(c) = self.canon.get(folder) {
            return c.clone();
        }
        let c = match self.survey.folders.get(folder).and_then(|f| f.canonical.clone()) {
            Some(c) => Some(c),
            None => std::fs::canonicalize(folder).ok(),
        };
        self.canon.insert(folder.to_path_buf(), c.clone());
        c
    }

    /// One photo's file.
    fn one(&mut self, p: &Planned) {
        let survey_says = self.survey.trash_of(&p.folder);
        let trash = self.confirmed.get(&p.folder).copied().unwrap_or(survey_says);
        let erasing = self.erase.contains(&p.id);
        // a disk without a Trash, and erasing was not asked for: the file is not looked at
        if trash == TrashHere::No && !erasing {
            self.no_trash.push(p.id.0);
            self.stayed.push(p.path.clone());
            return;
        }
        // a photo that stays in its folder is this file (another spelling of its name, or the
        // folder reached through a link): it stays with it
        if self.near.get(&(p.key.clone(), p.name.clone())).is_some_and(|same| same.iter().any(|k| *k == p.file || self.fs.same_file(k, &p.file))) {
            self.shared.insert(p.id);
            self.removed.push(p.id);
            return;
        }
        if self.done.contains(&p.file) {
            // another of the photos named is this file, which went with it
            self.removed.push(p.id);
            return;
        }
        match std::fs::symlink_metadata(&p.file) {
            Ok(meta) => {
                let t = meta.file_type();
                if !(t.is_file() || (t.is_symlink() && std::fs::metadata(&p.file).is_ok_and(|m| m.is_file()))) {
                    self.fail(p, format!("{}: not a file", p.path));
                    return;
                }
            }
            // gone from a folder that is there: the photo just leaves. A folder that isn't
            // there says nothing about the file (a disk that is away): the photo stays
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if p.folder.is_dir() {
                    self.missing.insert(p.id);
                    self.removed.push(p.id);
                } else {
                    self.fail(p, format!("{}: its folder can't be reached (is the disk connected?)", p.path));
                }
                return;
            }
            // (it may well be there: only "not found" means gone)
            Err(e) => {
                self.fail(p, format!("{}: can't be read: {e}", p.path));
                return;
            }
        }
        // a disk known to have no Trash this run: erased (it is in `erase`, see above)
        if trash == TrashHere::No && self.confirmed.contains_key(&p.folder) {
            self.erase_one(p);
            return;
        }
        // otherwise the Trash is tried; for a folder the survey said has none, once, to confirm
        match self.bin.trash(&p.file) {
            Ok(at) => {
                self.confirmed.insert(p.folder.clone(), TrashHere::Yes);
                self.went(p, at.map_or(Whereabouts::InTrash, |a| Whereabouts::At(a.to_string_lossy().to_string())));
            }
            Err(BinError::NoTrash) => {
                self.confirmed.insert(p.folder.clone(), TrashHere::No);
                if erasing {
                    self.erase_one(p);
                } else {
                    self.no_trash.push(p.id.0);
                    self.stayed.push(p.path.clone());
                }
            }
            Err(BinError::Failed(why)) => self.fail(p, why),
        }
    }

    /// Erase one file, unless a photo of the same name elsewhere that stays is this file (its
    /// folder leads to this one through a link): then it stays with that photo.
    fn erase_one(&mut self, p: &Planned) {
        let mine = self.canonical(&p.folder);
        let far = self.far;
        for (k, key) in far.get(&p.name).into_iter().flatten() {
            // (those in its own folder were asked about already)
            if *key == p.key {
                continue;
            }
            if self.cancel.load(Ordering::Relaxed) {
                self.stopped += 1;
                self.stayed.push(p.path.clone());
                return;
            }
            let Some(parent) = k.parent() else { continue };
            if mine.is_some() && self.canonical(parent) == mine && self.fs.same_file(k, &p.file) {
                self.shared.insert(p.id);
                self.removed.push(p.id);
                return;
            }
        }
        match self.bin.erase(&p.file) {
            // (its sidecars stay: only the file that was asked about is erased)
            Ok(()) => self.went(p, Whereabouts::Erased),
            Err(e) => self.fail(p, format!("{}: can't be erased: {e}", p.path)),
        }
    }
}

/// A folder's entries: name, and whether it is a file (or a link to one). `None`: it can't be
/// listed.
type Listing = Option<Vec<(OsString, bool)>>;

fn list(folder: &Path) -> Listing {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(folder).ok()? {
        let entry = entry.ok()?;
        // (a link counts when it leads to a file, as the probes see it)
        let file = entry.file_type().is_ok_and(|t| t.is_file() || (t.is_symlink() && std::fs::metadata(entry.path()).is_ok_and(|m| m.is_file())));
        out.push((entry.file_name(), file));
    }
    Some(out)
}

struct Sidecars<'a> {
    fs: &'a dyn MoveFs,
    bin: &'a dyn Bin,
    beside: &'a Kept,
    /// One listing per folder, taken on first need after the originals moved.
    listings: HashMap<PathBuf, Listing>,
    /// Sidecars moved to the Trash: each goes once (a raw and a JPEG may share one).
    taken: HashSet<PathBuf>,
    /// How many originals went from each folder.
    per_folder: HashMap<PathBuf, usize>,
    left: usize,
}

impl Sidecars<'_> {
    fn listing(&mut self, folder: &Path) -> Option<&mut Vec<(OsString, bool)>> {
        self.listings.entry(folder.to_path_buf()).or_insert_with(|| list(folder)).as_mut()
    }

    /// The sidecars beside `original` that are there now (`.xmp` and `.XMP` are one file on a
    /// disk that ignores case), by their 4 possible names.
    fn probe(&self, original: &str) -> Vec<PathBuf> {
        let mut seen: Vec<PathBuf> = Vec::new();
        for naming in [SidecarNaming::Stem, SidecarNaming::Full] {
            let lower = sidecar_path(original, naming);
            for path in [lower.clone(), lower.with_extension("XMP")] {
                if !self.fs.is_file(&path) || seen.iter().any(|o| self.fs.same_file(o, &path)) {
                    continue;
                }
                seen.push(path);
            }
        }
        seen
    }

    /// The sidecars of original `g`: those of a file that went to the Trash and are its alone go
    /// there too (into `moved`); the rest are counted as left.
    fn of(&mut self, g: &Gone, moved: &mut Vec<Trashed>) {
        let original = Path::new(&g.path);
        let lower = |s: Option<&std::ffi::OsStr>| s.map(|s| s.to_string_lossy().to_lowercase());
        let (Some(stem), Some(name)) = (lower(original.file_stem()), lower(original.file_name())) else { return };
        let wanted = [format!("{stem}.xmp"), format!("{name}.xmp")];
        let many = self.per_folder.get(&g.folder).is_some_and(|n| *n >= LIST_FOLDER_AT);
        let listed: Option<Vec<PathBuf>> = if many {
            self.listing(&g.folder).map(|entries| {
                entries
                    .iter()
                    .filter(|(n, file)| *file && wanted.contains(&n.to_string_lossy().to_lowercase()))
                    .map(|(n, _)| g.folder.join(n))
                    .collect()
            })
        } else {
            None
        };
        let found = match listed {
            Some(found) => found,
            // few photos went from this folder, or it can't be listed: the 4 names
            None => self.probe(&g.path),
        };
        for sidecar in found {
            if self.taken.contains(&sidecar) {
                continue;
            }
            if g.erased || !self.is_free(g, &sidecar) {
                self.left += 1;
                continue;
            }
            match self.bin.trash(&sidecar) {
                Ok(at) => {
                    if let (Some(entries), Some(n)) = (self.listings.get_mut(&g.folder).and_then(Option::as_mut), sidecar.file_name()) {
                        entries.retain(|(e, _)| e != n);
                    }
                    moved.push(Trashed {
                        from: sidecar.to_string_lossy().to_string(),
                        now: at.map_or(Whereabouts::InTrash, |a| Whereabouts::At(a.to_string_lossy().to_string())),
                    });
                    self.taken.insert(sidecar);
                }
                Err(_) => self.left += 1,
            }
        }
    }

    /// Whether `sidecar` (`S.xmp`) is nobody's but the original's (gone from the folder by now). A
    /// sidecar is `S`'s when a file is named `S` (`IMG_1.jpg.xmp` for `IMG_1.jpg`) and the sidecar
    /// of everything whose name without its extension is `S` (`IMG_1.xmp` for `IMG_1.dng`,
    /// `IMG_1.mov`…: other apps write those for files LightCraft does not import). So it is free
    /// only when no other file in the folder is named `S` or has the stem `S`, and no photo that
    /// stays in the library is such a file there. When in doubt (a folder that can't be listed):
    /// not free.
    fn is_free(&mut self, g: &Gone, sidecar: &Path) -> bool {
        let Some(of) = sidecar.file_stem().map(|s| s.to_string_lossy().to_lowercase()) else { return false };
        if self.beside.holds(&g.key, &of) {
            return false;
        }
        let own = sidecar.file_name().map(OsString::from);
        let Some(entries) = self.listing(&g.folder) else { return false };
        !entries.iter().any(|(e, _)| {
            if Some(e) == own.as_ref() {
                return false;
            }
            let e = Path::new(e);
            if e.extension().is_some_and(|x| x.eq_ignore_ascii_case("xmp")) {
                return false;
            }
            let is = |part: Option<&std::ffi::OsStr>| part.is_some_and(|p| p.to_string_lossy().to_lowercase() == of);
            is(e.file_name()) || is(e.file_stem())
        })
    }
}

/// What [`TrashPlan::work`] did with the files, for the session to commit
/// ([`TrashWork::finish`]) or undo (the files are put back when it fails).
pub struct TrashWork {
    removed: Vec<PhotoId>,
    /// The files each photo took along (its own first, then its sidecars).
    groups: Vec<(PhotoId, Vec<Trashed>)>,
    /// Photos that leave without a file moved (counted in the answer while they leave).
    missing: HashSet<PhotoId>,
    shared: HashSet<PhotoId>,
    sidecars_left: usize,
    no_trash: Vec<u64>,
    failed: Vec<Value>,
    stopped: usize,
    browsed: usize,
    bin: Arc<dyn Bin>,
}

impl TrashWork {
    /// Take the photos whose files went out of the library, as one undo step that puts the files
    /// back, and say what happened (`photo.trashFiles`'s answer).
    pub fn finish(self, s: &mut Session, c: &str) -> Result<Value> {
        let TrashWork { removed, groups, missing, shared, sidecars_left, no_trash, failed, stopped, browsed, bin } = self;
        let bin = bin.as_ref();
        let mut groups: HashMap<PhotoId, Vec<Trashed>> = groups.into_iter().collect();
        let taken: Vec<Trashed> = removed.iter().filter_map(|id| groups.remove(id)).flatten().collect();
        // (whatever is left belongs to no photo leaving: back where it was)
        for (_, g) in groups.drain() {
            TrashStep::PutBack(g).run(bin);
        }
        let erased = taken.iter().filter(|t| t.now == Whereabouts::Erased).count();
        let trashed = taken.len() - erased;
        if removed.is_empty() {
            // nothing went. A disk without a Trash, or a stop, is not an error; anything else
            // says why
            if failed.is_empty() {
                return Ok(json!({
                    "removed": 0, "trashed": 0, "erased": 0, "missing": 0, "shared": 0, "sidecarsLeft": 0,
                    "noTrash": no_trash, "failed": [], "skipped": browsed, "stopped": stopped
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
        // a slider drag open on one of the photos that leave is its own undo step first, as for
        // Delete Permanently: undo then brings the photo back with its edit (a drag on a photo
        // that stays goes on: this runs after the files moved, which may have taken a while)
        let drag_leaves = s.interaction.as_ref().is_some_and(|i| removed.contains(&i.photo));
        let committed = (if drag_leaves { s.end_interaction() } else { Ok(()) }).and_then(|()| {
            let op = s.catalog.delete_photos_permanently_ops(&removed);
            s.commit(&label, op)
        });
        if let Err(e) = committed {
            // the library did not take the change: the files go back where they were
            let (_, note) = TrashStep::PutBack(taken).run(bin);
            return Err(EngineError::Other(match note {
                Some(note) => format!("{e}; {note}"),
                None => e.to_string(),
            }));
        }
        if let Some(entry) = s.undo.last_mut() {
            entry.trash = Some(TrashStep::PutBack(taken));
        }
        crate::cmd::library::select_after_deletion(s, &before);
        Ok(json!({
            "removed": n,
            "trashed": trashed,
            "erased": erased,
            "missing": missing.len(),
            "shared": shared.len(),
            "sidecarsLeft": sidecars_left,
            "noTrash": no_trash,
            "failed": failed,
            "skipped": browsed,
            "stopped": stopped,
        }))
    }
}
