//! Locating a library folder that is somewhere else now (`folder.updateLocation`, issues #1112 and #1116).
//!
//! A folder of the library can stop being where the library looks for it without LightCraft
//! having moved it: it was renamed or moved in the file manager, its disk is mounted under
//! another name, or its catalog came from another computer (a Lightroom catalog written on
//! Windows names `D:\Photos\…`, which is nowhere on Linux). Synchronize Folder can only say the
//! folder isn't there. Locating it says where it is now: every photo below the old folder (those
//! in the folders inside it and in Recently Deleted too) points at the same place below the new
//! one, and what the library knows about the folders (their colour labels) goes along. Only the
//! catalog changes; no file is touched, and it is one undo step.
//!
//! It is done in three parts, so the app never waits on a disk in a frame:
//!
//! 1. [`Relocation::new`] reads the request against the session: it works out where each photo's
//!    file would be and refuses what needs no disk to refuse (no file-system access). It keeps
//!    the places and what the library recorded of the files, not the library.
//! 2. [`Relocation::look`] reads the disk, never the session, so it can run on a worker thread:
//!    it tells for each place whether the photo's file is there.
//! 3. [`Located::commit`] relinks the photos, on the session as it is by then.
//!
//! Looking is the guard against choosing the wrong folder (one level too high or too low is the
//! likely mistake), and a file being at a place does not make it the photo's ([`Sought`]):
//!
//! * a place that is, as the library has it, the file of another photo of the folder (the new
//!   place is inside the folder, or the folder inside it) is taken only when the file's content
//!   is the photo's. Otherwise the step is refused, whatever was asked: the photo would show
//!   the other photo's picture under its own edits. The same file can be reached by many paths
//!   (other letter case on a disk that doesn't tell case apart, a link at or below either
//!   folder), so while the folder is still on disk where the library has it, the photos' files
//!   are told by what they are on disk ([`FileId`]), not by how their paths are spelled; a
//!   place written with `..` is refused. (One share mounted twice is two disks to this.)
//! * a file inside the folder that was only looked at in Local is taken for the library's photo
//!   coming to its place when its content is the photo's, where the library recorded that;
//! * a file of another size than the library recorded is not the file it knew: it is not counted
//!   as found;
//! * a folder with none of the files is not the folder, and one with only some of them (or with
//!   files of another size) is taken only on request (`incomplete`). Files that aren't there
//!   then stay missing at the new place, where Synchronize Folder finds them.
//!
//! Paths are compared by folder identity ([`lightcraft_catalog::query::folder_key`]), not by
//! [`std::path::Path`]: on Linux and macOS `D:\Photos\a.jpg` is one file name to `Path`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, Mutex, PoisonError};

use lightcraft_catalog::query::{folder_key, folder_rest};
use lightcraft_catalog::{Catalog, Filter, Op, PhotoId, Sort, Source};
use serde_json::{Value, json};

use crate::import::ScanProgress;
use crate::{EngineError, FolderMove, Result, Session};

fn invalid(msg: impl Into<String>) -> EngineError {
    EngineError::Other(msg.into())
}

/// A request to locate folder `from` at `to`, checked as far as the session alone can tell.
pub struct Relocation {
    /// The folder as the library knows it.
    pub from: String,
    /// Where it is said to be now (absolute, spelled the way this computer spells paths).
    pub to: String,
    /// The photos that would follow the folder.
    photos: Vec<RelocatedPhoto>,
    /// The places [`Relocation::look`] looks at.
    files: Vec<Sought>,
    /// Which library that is ([`Located::commit`] is for no other).
    library: Arc<()>,
}

/// A place a file of the folder would be, and what tells whether the file there is the photo's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sought {
    pub path: String,
    /// The size the library recorded for the file (0: none, as for photos of a catalog that
    /// came from another app).
    size: u64,
    /// The contents the library recorded for the photos that would be here (a virtual copy
    /// shares its photo's file); empty when it recorded none.
    hashes: Vec<String>,
    /// Where the photos that would be here are now, as the library has them.
    now: Vec<String>,
    /// The place is, as the library has it, the file of another photo of the folder: only its
    /// content tells whose the file there is.
    claimed: bool,
    /// The place is a file of the folder that was only looked at in Local: nothing says it is
    /// the photo's, so its content is asked where the library recorded the photo's.
    browsed: bool,
    /// A library photo's (not only one's in Recently Deleted or browsed in Local, which nobody
    /// is looking for): counted in [`Located::total`].
    counted: bool,
}

/// What is at a place ([`Sought`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Seen {
    /// No file.
    Missing,
    /// A file of the size the library recorded (of the photo's content, at a claimed place).
    Same,
    /// A file, and the library recorded no size to tell by.
    Unverified,
    /// A file of another size.
    Different,
    /// A file at a claimed place that is not known to be the photo's.
    Claimed,
}

/// What a file is on disk, however its path is spelled.
#[cfg(unix)]
type FileId = (u64, u64);
#[cfg(not(unix))]
type FileId = PathBuf;

/// What the file or folder at `path` is on disk (links followed), if it is there.
fn file_id(path: &Path, meta: Option<&std::fs::Metadata>) -> Option<FileId> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let read;
        let meta = match meta {
            Some(m) => m,
            None => {
                read = std::fs::metadata(path).ok()?;
                &read
            }
        };
        Some((meta.dev(), meta.ino()))
    }
    #[cfg(not(unix))]
    {
        // (no file id in std here: the path as the system spells it, links and letter case resolved)
        let _ = meta;
        std::fs::canonicalize(path).ok()
    }
}

/// The files of the folder's photos where the library has them now, by what they are on disk:
/// a file at the new place can be one of them under another path (a place inside the folder, a
/// link at or below either folder). Empty when the folder is not on disk where the library has
/// it (the usual case: nothing of `from` is looked at then).
fn files_now(photos: &[RelocatedPhoto], from: &str, progress: &ScanProgress) -> HashMap<FileId, String> {
    let mut out = HashMap::new();
    if file_id(Path::new(from), None).is_none() {
        return out;
    }
    for r in photos {
        if progress.cancel.load(Relaxed) {
            break;
        }
        if let Some(id) = file_id(Path::new(&r.from), None) {
            out.entry(id).or_insert_with(|| r.from.clone());
        }
    }
    out
}

impl Sought {
    /// What is at the place. `theirs`: the files of the folder's photos as they are on disk now
    /// ([`files_now`]).
    fn seen(&self, theirs: &HashMap<FileId, String>) -> Seen {
        let Ok(meta) = std::fs::metadata(&self.path) else { return Seen::Missing };
        if !meta.is_file() {
            return Seen::Missing;
        }
        // another photo's file under another spelling of its path (a photo's own file, where
        // the new place is another name for the folder itself, is its own)
        let theirs = || {
            !theirs.is_empty() && file_id(Path::new(&self.path), Some(&meta)).and_then(|id| theirs.get(&id)).is_some_and(|at| !self.now.contains(at))
        };
        if self.claimed || theirs() || (self.browsed && !self.hashes.is_empty()) {
            // the photo's file has the photo's size and content (read only here, and in pieces:
            // files shared by the two folders are rare, and reading every file of a folder on a
            // network share takes hours)
            let proven = !self.hashes.is_empty()
                && (self.size == 0 || self.size == meta.len())
                && crate::cmd::missing::file_hash(&self.path).is_some_and(|c| self.hashes.iter().all(|h| *h == c));
            return if proven { Seen::Same } else { Seen::Claimed };
        }
        match self.size {
            0 => Seen::Unverified,
            n if n == meta.len() => Seen::Same,
            _ => Seen::Different,
        }
    }
}

/// What [`look_at`] saw at the places of the library's files.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    /// Files that are there and not known to be other files…
    pub found: usize,
    /// …those among them the library recorded no size for.
    pub unverified: usize,
    /// Files there of another size than the library recorded.
    pub different: usize,
    /// Claimed places whose file is not known to be the photo's, and one of them.
    claimed: usize,
    claimed_example: Option<String>,
}

/// One photo of the folder and where its file is once the folder is located.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelocatedPhoto {
    pub id: PhotoId,
    /// Its file as the library has it; a photo that has left it by [`Located::commit`] is skipped.
    pub from: String,
    pub to: String,
}

/// What [`Relocation::look`] found: the photos that would follow the folder, and how many of
/// the library's files are at the new place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Located {
    pub from: String,
    pub to: String,
    pub photos: Vec<RelocatedPhoto>,
    /// Files of library photos (not those in Recently Deleted or only browsed in Local; a
    /// virtual copy shares its photo's file) that are at the new place and not known to be
    /// other files…
    pub found: usize,
    /// …of this many.
    pub total: usize,
    /// Those of `found` the library recorded no size for: a file is there, and nothing tells
    /// whether it is the photo's.
    pub unverified: usize,
    /// Files at the new place of another size than the library recorded (not in `found`).
    pub different: usize,
    /// The library the photos are of: one opened meanwhile is not touched.
    library: Arc<()>,
}

impl Relocation {
    /// Read a request to locate folder `path` of the library at `to`. Refused: the startup disk,
    /// a folder no library photo was imported from, a whole disk or a folder that holds disks
    /// without `disk` (as for Synchronize Folder), the place the library already looks, and the
    /// library's own folder, and a place where a file is already another photo's (one not of
    /// this folder; two photos would share one file). No file-system access.
    pub fn new(s: &Session, path: &str, to: &str, disk: bool) -> Result<Relocation> {
        if cfg!(target_arch = "wasm32") {
            // (no disk to look at: every folder would be "not a folder")
            return Err(invalid("locating a folder is not supported in the web version: it has no disk to look for the photos' files on"));
        }
        if path.trim().is_empty() {
            return Err(invalid("missing `folder`"));
        }
        if to.trim().is_empty() {
            return Err(invalid("missing `path`"));
        }
        if lightcraft_catalog::folders::is_startup_disk(path) {
            return Err(invalid("choose a folder, not the whole startup disk"));
        }
        let f = Filter { library_folder: Some(path.to_string()), ..Default::default() };
        let ids = s.catalog.query(&f, &Sort::default());
        if ids.is_empty() {
            return Err(invalid(format!("{path}: no photo in the library was imported from it")));
        }
        if !disk && (lightcraft_catalog::folders::is_disk_root(path) || crate::cmd::library::covers_other_disks(s, path, &ids)) {
            return Err(invalid(format!("{path} is a whole disk or holds disks: pass `disk: true` to locate it")));
        }
        let abs = std::path::absolute(to).map_err(|e| invalid(format!("{to}: {e}")))?;
        // (one place has one spelling to compare by: `a/b/../b` is not told from `a/b` as written)
        if abs.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            return Err(invalid(format!("{to}: give the folder's full path, without `..`")));
        }
        // (rebuilt from its parts: no trailing separator, so the photos' paths are joined cleanly)
        let abs: PathBuf = abs.components().collect();
        let to = abs.to_string_lossy().to_string();
        if folder_key(path) == folder_key(&to) {
            return Err(invalid(format!("{to} is where the library already looks for this folder")));
        }
        if s.library.as_ref().is_some_and(|l| l.on_disk && abs.starts_with(&l.dir)) {
            return Err(invalid(format!("{to} is inside the library's own folder, which is no place for photos")));
        }
        let Destinations { photos, files } = destinations(&s.catalog, path, &to);
        let moving: HashSet<PhotoId> = photos.iter().map(|r| r.id).collect();
        let places: HashSet<String> = photos.iter().map(|r| r.to.clone()).collect();
        // (said before any file is looked for; [`Located::commit`] asks again of the library as it is then)
        giving_way(&s.catalog, &moving, &places, &to).map_err(invalid)?;
        Ok(Relocation { from: path.to_string(), to, photos, files, library: Arc::clone(&s.library_identity) })
    }

    /// Whether the folder's files are at the new place. Reads the disk, never the session.
    /// Refused when `to` is not a folder, or when a file there is another photo's of this
    /// folder and not known to be the photo's that would point at it ([`Sought`]).
    ///
    /// `progress` counts the places looked at; with its `cancel` set the looking stops early
    /// and is refused: what was seen until then says nothing.
    pub fn look(self, progress: &ScanProgress) -> std::result::Result<Located, String> {
        let Relocation { from, to, photos, files, library } = self;
        if !Path::new(&to).is_dir() {
            return Err(format!("{to} is not a folder"));
        }
        let total = files.iter().filter(|f| f.counted).count();
        let theirs = files_now(&photos, &from, progress);
        // (a place only a photo in Recently Deleted or a kept Local record would point at is
        // looked at where a file there can be another photo's)
        let files: Vec<Sought> = files.into_iter().filter(|f| f.counted || f.claimed || !theirs.is_empty()).collect();
        let Tally { found, unverified, different, claimed, claimed_example } = look_at(&files, &theirs, progress);
        if progress.cancel.load(Relaxed) {
            // (places not looked at may be other photos' files: what was seen says nothing)
            return Err("locating the folder was stopped: nothing was changed".to_string());
        }
        if let Some(first) = claimed_example {
            let n = claimed;
            let (s, verb, is) = if n == 1 { ("", "belongs", "is") } else { ("s", "belong", "are") };
            return Err(format!(
                "{n} file{s} in {to} {verb} to other photos of this folder, or {is} another picture than the library recorded for the photo, as far as the library can tell (such as {first}): choose the folder its files are in now. \
                 (The folder as the library has it and the chosen one share files; a file there is taken for a photo only when the library recorded the photo's content and the file has it.)"
            ));
        }
        Ok(Located { from, to, photos, found, total, unverified, different, library })
    }

    /// The places [`Relocation::look`] looks at (tests).
    #[cfg(test)]
    pub(crate) fn files(&self) -> &[Sought] {
        &self.files
    }
}

/// Where the photos of a folder would be, and the places to look at.
struct Destinations {
    photos: Vec<RelocatedPhoto>,
    /// Each place once (a virtual copy shares its photo's file).
    files: Vec<Sought>,
}

fn destinations(catalog: &Catalog, from: &str, to: &str) -> Destinations {
    let base = PathBuf::from(to);
    let mut photos: Vec<RelocatedPhoto> = Vec::new();
    let mut files: Vec<Sought> = Vec::new();
    let mut at: HashMap<String, usize> = HashMap::new();
    // A record of only having looked at a file in Local is no photo of the folder: it stays (the
    // place on disk did not move), and gives way where a photo of the library comes to its file
    // ([`giving_way`]) — also when that file is in a folder inside this one.
    let browsed: HashSet<String> = catalog
        .photos()
        .filter(|p| p.local && !p.deleted)
        .filter_map(|p| match &p.source {
            Source::File { path } if folder_rest(path, from).is_some() => Some(path.clone()),
            _ => None,
        })
        .collect();
    let looked: HashSet<PhotoId> = catalog.local_records_of(&browsed).untouched.into_iter().collect();
    let mut looked_at: HashSet<&str> = HashSet::new();
    for p in catalog.photos() {
        let Source::File { path } = &p.source else { continue };
        if looked.contains(&p.id) {
            looked_at.insert(path.as_str());
            continue;
        }
        let Some(rest) = folder_rest(path, from).filter(|r| !r.is_empty()) else { continue };
        // joined by the platform, so the result is spelled like a path read from this disk
        let dest = rest.iter().fold(base.clone(), |d, name| d.join(name)).to_string_lossy().to_string();
        let i = *at.entry(dest.clone()).or_insert_with(|| {
            files.push(Sought { path: dest.clone(), size: 0, hashes: Vec::new(), now: Vec::new(), claimed: false, browsed: false, counted: false });
            files.len() - 1
        });
        if let Some(f) = files.get_mut(i) {
            f.counted |= p.in_library();
            if !f.now.contains(path) {
                f.now.push(path.clone());
            }
            if f.size == 0 {
                f.size = p.file_size;
            }
            if let Some(h) = crate::cmd::missing::stored_hash(p).filter(|h| !f.hashes.iter().any(|x| x == h)) {
                f.hashes.push(h.to_string());
            }
        }
        photos.push(RelocatedPhoto { id: p.id, from: path.clone(), to: dest });
    }
    // a place one of the photos is at now is that photo's file, as far as the library knows
    let now: HashSet<&str> = photos.iter().map(|r| r.from.as_str()).collect();
    for f in &mut files {
        f.claimed = now.contains(f.path.as_str());
        f.browsed = looked_at.contains(f.path.as_str());
    }
    Destinations { photos, files }
}

/// The records that give way when the photos `moving` point at `places` (every place one of
/// them would be, the library's or not), so that no file has two records: Local records of
/// those files that are only a record of having looked ([`Catalog::local_records_of`]).
/// Refused when a place is another photo's already — a library photo's, or one's in Recently
/// Deleted (restoring it would make two) — or a Local record's that holds something of the
/// person's. Paths are compared as written, as everywhere in the library.
fn giving_way(catalog: &Catalog, moving: &HashSet<PhotoId>, places: &HashSet<String>, to: &str) -> std::result::Result<Vec<PhotoId>, String> {
    let taken: Vec<&str> = catalog
        .photos()
        .filter(|p| (!p.local || p.deleted) && !moving.contains(&p.id))
        .filter_map(|p| match &p.source {
            Source::File { path } => Some(path.as_str()),
            _ => None,
        })
        .filter(|f| places.contains(*f))
        .collect();
    if let Some(first) = taken.first() {
        let n = taken.len();
        let are = if n == 1 { "is" } else { "are" };
        return Err(format!(
            "{n} file{} in {to} {are} already in the library, Recently Deleted included (such as {first}): the folder can't be located there",
            plural(n)
        ));
    }
    let local = catalog.local_records_of(places);
    let kept: Vec<&(PhotoId, String)> = local.kept.iter().filter(|(id, _)| !moving.contains(id)).collect();
    if let Some((_, first)) = kept.first() {
        let n = kept.len();
        let were = if n == 1 { "was" } else { "were" };
        return Err(format!(
            "{n} file{} in {to} {were} rated, edited or put in an album while browsing it in Local (such as {first}): the library's photos can't take their place",
            plural(n)
        ));
    }
    Ok(local.untouched.into_iter().filter(|id| !moving.contains(id)).collect())
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// What is at each of `files`, looked at on several threads (a network share answers each
/// slowly). `progress.total` / `done` follow; stops early when `progress.cancel` is set, with
/// what was seen so far.
pub fn look_at(files: &[Sought], theirs: &HashMap<FileId, String>, progress: &ScanProgress) -> Tally {
    use std::sync::atomic::AtomicUsize;
    progress.total.store(files.len(), Relaxed);
    progress.done.store(0, Relaxed);
    if cfg!(target_arch = "wasm32") {
        return Tally::default();
    }
    let cancel = &progress.cancel;
    let (found, unverified, different, claimed, next) =
        (AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0));
    let example: Mutex<Option<String>> = Mutex::new(None);
    let work = || {
        while !cancel.load(Relaxed) {
            let Some(f) = files.get(next.fetch_add(1, Relaxed)) else { break };
            match f.seen(theirs) {
                Seen::Claimed => {
                    claimed.fetch_add(1, Relaxed);
                    example.lock().unwrap_or_else(PoisonError::into_inner).get_or_insert_with(|| f.path.clone());
                }
                // (a place only a photo in Recently Deleted or a Local record would point at is
                // looked at for whose file it is, not counted)
                _ if !f.counted => {}
                Seen::Same => {
                    found.fetch_add(1, Relaxed);
                }
                Seen::Unverified => {
                    found.fetch_add(1, Relaxed);
                    unverified.fetch_add(1, Relaxed);
                }
                Seen::Different => {
                    different.fetch_add(1, Relaxed);
                }
                Seen::Missing => {}
            }
            progress.done.fetch_add(1, Relaxed);
        }
    };
    #[cfg(not(target_arch = "wasm32"))]
    {
        let n = crate::import::workers(files.len());
        if n > 1 {
            std::thread::scope(|sc| {
                // (a thread the system won't start: the others, or this one, do its share)
                let started = (0..n).filter(|_| std::thread::Builder::new().spawn_scoped(sc, work).is_ok()).count();
                if started == 0 {
                    work();
                }
            });
        } else {
            work();
        }
    }
    #[cfg(target_arch = "wasm32")]
    work();
    Tally {
        found: found.into_inner(),
        unverified: unverified.into_inner(),
        different: different.into_inner(),
        claimed: claimed.into_inner(),
        claimed_example: example.into_inner().unwrap_or_else(PoisonError::into_inner),
    }
}

impl Located {
    /// Why this is not (or not plainly) the folder, if it isn't: none of the files are there, or
    /// only some (the others are not there, or are of another size than the library recorded)
    /// and `incomplete` wasn't said.
    pub fn refusal(&self, incomplete: bool) -> Option<String> {
        let Located { from, to, found, total, different, .. } = self;
        // files of another size are said: they are what tells a disk laid out alike from the folder
        let others = match *different {
            0 => String::new(),
            1 => "; 1 file there is not the one the library recorded (another size)".to_string(),
            n => format!("; {n} files there are not the ones the library recorded (another size)"),
        };
        if *found == 0 {
            return Some(format!(
                "none of the {total} file{} of {from} {} in {to}{others}: choose the folder it is now",
                plural(*total),
                if *total == 1 { "is" } else { "are" }
            ));
        }
        (found < total && !incomplete).then(|| {
            format!(
                "only {found} of the {total} files of {from} are in {to}{others}: pass `incomplete: true` to locate it there anyway (files that aren't there stay missing)"
            )
        })
    }

    /// Point the photos at the new place, with the folders' records, as one undo step (undo and
    /// redo move no file). Checked against the session as it is now: a photo that is no longer
    /// where [`Relocation::look`] saw it (relinked or removed meanwhile) is left alone, and what
    /// is at the new place is asked again. Records of files there that were only browsed in
    /// Local, untouched, go as the library's photos take their place (a file has one record;
    /// otherwise Synchronize Folder would offer every file as new); undo brings them back.
    /// → `{folder, path, relinked, found, unverified, different, notFound, replacedLocal}`.
    pub fn commit(self, s: &mut Session, incomplete: bool) -> Result<Value> {
        // (photo ids mean something else in another library)
        if !Arc::ptr_eq(&self.library, &s.library_identity) {
            return Err(invalid("another library was opened meanwhile: the folder was not located"));
        }
        if let Some(why) = self.refusal(incomplete) {
            return Err(invalid(why));
        }
        let Located { from, to, photos, found, total, unverified, different, .. } = self;
        let before = s.visible_cloned();
        let mut ops: Vec<Op> = Vec::new();
        let mut moving: HashSet<PhotoId> = HashSet::new();
        let mut places: HashSet<String> = HashSet::new();
        for r in photos {
            let Some(p) = s.catalog.photo(r.id) else { continue };
            if !matches!(&p.source, Source::File { path } if *path == r.from) {
                continue;
            }
            moving.insert(r.id);
            places.insert(r.to.clone());
            ops.push(Op::Relink { id: r.id, file_name: p.file_name.clone(), source: Source::File { path: r.to }, format: None });
        }
        let relinked = ops.len();
        if relinked == 0 {
            return Err(invalid(format!("{from} holds no photo any more: the library changed meanwhile")));
        }
        // the library may have changed while the files were looked for (a folder browsed, a photo
        // imported or relinked): what is at the new place is asked of it as it is now
        let replaced = giving_way(&s.catalog, &moving, &places, &to).map_err(invalid)?;
        // what the library knows about the folder (and those inside it) goes along, in the same
        // step; what it knows about folders already at the new place stays
        ops.extend(s.catalog.folder_records_located(&from, &to));
        ops.extend(replaced.iter().map(|id| Op::RemovePhoto { id: *id }));
        let name = lightcraft_catalog::folders::folder_label(&to);
        // (the undo direction: the new place → the old one; nothing is renamed on disk)
        let back = FolderMove { from: to.clone(), to: from.clone(), on_disk: false };
        s.commit_with_folder(&format!("Locate Folder “{name}”"), Op::Batch { ops }, back)?;
        follow_located(s, &from, &to);
        if !replaced.is_empty() {
            crate::cmd::library::select_after_deletion(s, &before);
        }
        Ok(json!({
            "folder": from,
            "path": to,
            "relinked": relinked,
            "found": found,
            "unverified": unverified,
            "different": different,
            "notFound": total.saturating_sub(found).saturating_sub(different),
            "replacedLocal": replaced.len(),
        }))
    }
}

/// After a folder was located (`from` → `to`), or that was undone: a library folder being shown
/// (as the source or in the filter) at or below `from` is shown at the same place below `to`,
/// if its photos left it. One that still holds photos is another folder that was there all
/// along (a folder of the library at the new place, chosen since) and stays. A folder browsed
/// in Local is a place on disk, which did not move, and stays too.
pub(crate) fn follow_located(s: &mut Session, from: &str, to: &str) {
    let moved = |s: &Session, chosen: &Option<String>| -> Option<String> {
        let c = chosen.as_deref()?;
        let rest = folder_rest(c, from)?;
        let f = Filter { library_folder: Some(c.to_string()), ..Default::default() };
        if !s.catalog.query(&f, &Sort::default()).is_empty() {
            return None;
        }
        Some(rest.iter().fold(PathBuf::from(to), |p, name| p.join(name)).to_string_lossy().to_string())
    };
    if let Some(now) = moved(s, &s.library_folder) {
        s.library_folder = Some(now);
    }
    if let Some(now) = moved(s, &s.filter.library_folder) {
        s.filter.library_folder = Some(now);
    }
    if let Some(f) = s.shelved_filter.as_ref()
        && let Some(now) = moved(s, &f.library_folder)
        && let Some(f) = s.shelved_filter.as_mut()
    {
        f.library_folder = Some(now);
    }
}

/// The three parts in one go, on the calling thread, for a caller whose thread is its own to
/// spend: the `folder.updateLocation` command of the CLI and headless MCP (registered with the
/// app's part of this feature, which does the looking on a worker thread instead).
pub fn relocate(s: &mut Session, path: &str, to: &str, disk: bool, incomplete: bool) -> Result<Value> {
    let plan = Relocation::new(s, path, to, disk)?;
    let task = s.activity.start("relocate", "Locating folder", crate::activity::Cancel::No);
    task.detail(&lightcraft_catalog::folders::folder_label(&plan.from));
    let located = plan.look(&ScanProgress::default()).map_err(invalid)?;
    drop(task);
    located.commit(s, incomplete)
}
