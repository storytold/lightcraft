//! Synchronize Folder: bring a folder of the library up to date with what is on disk, as
//! Lightroom Classic's Synchronize Folder does.
//!
//! A [scan](scan_with) compares the folder (and the folders inside it) with the library and finds
//! three kinds of change, touching nothing:
//!
//! - **new** files: supported files the library doesn't have, by path or by content (the import
//!   scan's duplicate rules, [`crate::import::scan_with`]);
//! - **missing** photos: library photos in the folder whose file is gone (the Missing Photos rule,
//!   [`crate::cmd::missing::checked_path`]);
//! - **metadata** updates: photos whose XMP sidecar file was saved after the photo came into the
//!   library or was last edited here, and says something the library doesn't (rating, flag,
//!   label, metadata, capture time or develop settings). Reading one makes the sidecar win, as
//!   Read Metadata from File does. Embedded XMP in a raw is not a sidecar and is not looked at.
//!
//! [`synchronize`] then imports the new files (in place), relinks the moved photos, moves the
//! missing photos to Recently Deleted and reads the metadata updates — each only when chosen — as
//! one undo step. It runs in halves ([`SyncJob`]) so that, in the app, every file it reads is
//! read on a worker thread and only the catalog changes happen on the UI thread — with what every
//! command does after them there: the library's durable save and, with XMP auto-write on, the
//! sidecars of the photos whose metadata was read.
//!
//! The scan runs without the session ([`SyncInput`] + [`scan_with`]), so the app can run it on a
//! worker thread: a folder on a sleeping network share must never stall a frame.
//!
//! Limits of telling an update from a sidecar that is merely older:
//! - a sidecar the library itself wrote (auto-write on) agrees with it and is never an update;
//! - the library records when a photo came in and when its develop settings last changed, not
//!   when its rating, flag, label or metadata did: a sidecar another app saved, followed by such
//!   a change made here (with auto-write off), is listed as an update, and reading it puts the
//!   older value back. Reading updates is therefore never the default;
//! - times are compared as the clock the session runs with; headless sessions (CLI, MCP) start
//!   with a fixed clock, so their own times say little;
//! - a sidecar is found as Read Metadata from File finds it, including the other naming
//!   scheme's file (`IMG_1.xmp` for `IMG_1.JPG` beside `IMG_1.CR3`).
//!
//! The walk for new files stops at the import's limits on depth and entries (logged), and is
//! not cancelled midway.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use lightcraft_catalog::{Catalog, Filter, Op, Photo, PhotoId, Sort, Source};
use serde::Serialize;

use crate::import::{ImportCandidate, ImportMode, ImportOptions, ScanInput, ScanProgress};
use crate::media::ProbeInfo;
use crate::sidecar::SidecarNaming;
use crate::{EngineError, Result, Session};

/// The largest XMP sidecar read when looking for metadata updates; a bigger one is skipped.
const MAX_SIDECAR_BYTES: u64 = 16 * 1024 * 1024;

/// How far a scan is: the folder's files probed ([`ScanProgress`], whose `cancel` stops the
/// whole scan) and the library's photos checked.
#[derive(Default)]
pub struct SyncProgress {
    pub files: ScanProgress,
    pub checked: std::sync::atomic::AtomicUsize,
    pub to_check: std::sync::atomic::AtomicUsize,
}

impl SyncProgress {
    /// (done, of how many), both halves together.
    pub fn counts(&self) -> (usize, usize) {
        use std::sync::atomic::Ordering::Relaxed;
        let done = self.files.done.load(Relaxed).saturating_add(self.checked.load(Relaxed));
        (done, self.files.total.load(Relaxed).saturating_add(self.to_check.load(Relaxed)))
    }
}

/// What a scan of a folder needs, taken from the session so it can run on a worker thread.
pub struct SyncInput {
    folder: String,
    scan: ScanInput,
    /// The library's photos in the folder, each with how its sidecar is named.
    photos: Vec<(Arc<Photo>, SidecarNaming)>,
    /// For the colour label names a sidecar may use.
    labels: Catalog,
}

/// What changed in a folder since it came into the library.
#[derive(Clone, Debug, Default, Serialize)]
pub struct FolderChanges {
    /// The folder, as asked for.
    pub path: String,
    /// Files to import.
    pub new: Vec<ImportCandidate>,
    /// Files whose content the library already has (under another path).
    pub duplicates: usize,
    /// Supported files that couldn't be read.
    pub unreadable: Vec<ImportCandidate>,
    /// Photos whose file is gone.
    pub missing: Vec<FolderPhoto>,
    /// Photos whose sidecar has news.
    pub metadata: Vec<FolderPhoto>,
    /// Photos whose file was renamed or moved within the folder (found by its content).
    pub moved: Vec<FolderMoved>,
    /// The folder itself is not there (moved or renamed whole, or on a disk that isn't
    /// connected): nothing else is reported, rather than every photo as missing.
    pub offline: bool,
    /// The probes of the new files, kept for the import that follows.
    #[serde(skip)]
    probes: HashMap<String, ProbeInfo>,
    /// The folder's photos as the scan saw them: the scan is current while they are the same
    /// records (any change to one replaces it in the catalog).
    #[serde(skip)]
    seen: Vec<Arc<Photo>>,
}

/// A photo of the folder and its file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FolderPhoto {
    pub id: u64,
    pub path: String,
}

/// A photo whose file is now somewhere else in the folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FolderMoved {
    pub id: u64,
    pub from: String,
    pub to: String,
}

/// What [`synchronize`] does with the changes found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SyncChoice {
    pub import_new: bool,
    pub relink_moved: bool,
    pub remove_missing: bool,
    pub read_metadata: bool,
}

impl Default for SyncChoice {
    /// Lightroom Classic's defaults: import the new files, leave the rest to be looked at; and
    /// photos whose file moved follow it (nothing is lost, and undo puts them back).
    fn default() -> Self {
        SyncChoice { import_new: true, relink_moved: true, remove_missing: false, read_metadata: false }
    }
}

/// What [`synchronize`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SyncReport {
    pub imported: usize,
    pub relinked: usize,
    pub removed: usize,
    pub read: usize,
    /// (path or photo, why) for what couldn't be done.
    pub failed: Vec<(String, String)>,
}

fn invalid(msg: impl Into<String>) -> EngineError {
    EngineError::Other(msg.into())
}

impl SyncInput {
    /// The scan of folder `path`: a folder the library holds photos in, not the startup disk. A
    /// whole disk or share, or a folder that holds disks (`/Volumes`, `/mnt`…), only with `disk`
    /// (as for Remove Disk from Library: its missing photos can be a whole disk's).
    pub fn new(s: &mut Session, path: &str, disk: bool) -> Result<SyncInput> {
        let path = path.trim();
        if path.is_empty() {
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
            return Err(invalid(format!("{path} is a whole disk or holds disks: pass `disk: true` to synchronize it")));
        }
        let namings = s.sidecar_namings(&ids);
        let photos = ids.iter().zip(namings).filter_map(|(id, naming)| s.catalog.photo(*id).map(|p| (Arc::clone(p), naming))).collect();
        let (scan, _) = ScanInput::new(s, &[path.to_string()]);
        Ok(SyncInput { folder: path.to_string(), scan, photos, labels: s.catalog.clone() })
    }
}

/// Find what changed in the folder (see the module docs). Reads the disk, never the session.
///
/// Both halves wait on the disk (a network share answers each file slowly), so they run at once:
/// the look for new files (which probes them on its own threads) beside the checks of the
/// library's photos, which run on several threads too. Stops early when `progress.files.cancel`
/// is set.
pub fn scan_with(input: SyncInput, progress: &SyncProgress) -> FolderChanges {
    let SyncInput { folder, scan, photos, labels } = input;
    let seen: Vec<Arc<Photo>> = photos.iter().map(|(p, _)| Arc::clone(p)).collect();
    if !cfg!(target_arch = "wasm32") && !Path::new(&folder).is_dir() {
        return FolderChanges { path: folder, seen, offline: true, ..Default::default() };
    }
    // (no threads on the web: one after the other there)
    #[cfg(target_arch = "wasm32")]
    let (out, checks) = (crate::import::scan_with(scan, std::slice::from_ref(&folder), &progress.files), check_photos(&photos, &labels, progress));
    #[cfg(not(target_arch = "wasm32"))]
    let (out, checks) = std::thread::scope(|sc| {
        let photos = &photos;
        let labels = &labels;
        let checks = sc.spawn(move || check_photos(photos, labels, progress));
        let out = crate::import::scan_with(scan, std::slice::from_ref(&folder), &progress.files);
        // (a check that panicked counts as nothing found; the scan still reports the rest)
        (out, checks.join().unwrap_or_default())
    });
    let mut changes = FolderChanges { path: folder, seen, ..Default::default() };
    for ((p, _), check) in photos.iter().zip(checks) {
        let Some(file) = crate::cmd::missing::checked_path(p) else { continue };
        let found = FolderPhoto { id: p.id.0, path: file.to_string() };
        match check {
            Check::Missing => changes.missing.push(found),
            Check::News => changes.metadata.push(found),
            Check::Nothing => {}
        }
    }
    let moved_from = match_moved(&changes.missing, &out.candidates, &out.probes, &labels);
    for c in out.candidates {
        if moved_from.values().any(|to| *to == c.path) {
            continue;
        }
        let browsed = c.existing.and_then(|id| labels.photo(PhotoId(id))).is_some_and(|p| p.local && !p.deleted);
        match (c.duplicate.as_deref(), &c.error) {
            // a file only looked at in Local is not in the library yet: it is new
            (Some("path"), _) if browsed => changes.new.push(ImportCandidate { duplicate: None, existing: None, ..c }),
            (Some("path"), _) => {}
            (Some(_), _) => changes.duplicates += 1,
            (None, Some(_)) => changes.unreadable.push(c),
            (None, None) => changes.new.push(c),
        }
    }
    let missing = std::mem::take(&mut changes.missing);
    for m in missing {
        match moved_from.get(&m.path) {
            Some(to) => changes.moved.push(FolderMoved { id: m.id, from: m.path, to: to.clone() }),
            None => changes.missing.push(m),
        }
    }
    changes.probes = out.probes;
    changes
}

/// Whether a photo is the same as far as synchronizing goes: its file, its place in the library
/// and what a sidecar is compared with. Face and pet regions found in the background meanwhile
/// don't make a scan stale.
fn same_for_sync(a: &Arc<Photo>, b: &Arc<Photo>) -> bool {
    if Arc::ptr_eq(a, b) {
        return true;
    }
    let meta = |p: &Photo| lightcraft_catalog::Meta { regions: Vec::new(), ..p.meta.clone() };
    (&a.source, a.deleted, a.local, a.copy_of, &a.content_hash, a.file_size, a.rating, a.flag, a.label, &a.captured, &a.edited, &a.develop)
        == (&b.source, b.deleted, b.local, b.copy_of, &b.content_hash, b.file_size, b.rating, b.flag, b.label, &b.captured, &b.edited, &b.develop)
        && meta(a) == meta(b)
}

/// The content a hash stands for: a Duplicate's own hash is the original's plus `:dup<id>`.
fn base_hash(h: &str) -> &str {
    h.split(':').next().unwrap_or(h)
}

/// Missing photos' files found elsewhere in the folder (old path → new path): the file was
/// renamed or moved, and the photo (with its edits) follows it rather than going missing.
///
/// By content first: each missing file's hash claims one file with that content (two missing
/// files with the same content claim two). A photo with no hash on record (taken over from a
/// Lightroom catalog) is matched by file size and dimensions, and only when exactly one missing
/// file and exactly one unclaimed file of the folder agree on them.
fn match_moved(
    missing: &[FolderPhoto],
    candidates: &[ImportCandidate],
    probes: &HashMap<String, ProbeInfo>,
    labels: &Catalog,
) -> HashMap<String, String> {
    // distinct missing files (a photo's virtual copies share its file), with what is known of them
    let mut files: Vec<(&str, Option<&Photo>)> = Vec::new();
    for m in missing {
        if !files.iter().any(|(p, _)| *p == m.path) {
            files.push((m.path.as_str(), labels.photo(PhotoId(m.id)).map(|p| &**p)));
        }
    }
    let mut by_hash: HashMap<&str, std::collections::VecDeque<&str>> = HashMap::new();
    for (path, p) in &files {
        if let Some(h) = p.and_then(|p| p.content_hash.as_deref()) {
            by_hash.entry(base_hash(h)).or_default().push_back(path);
        }
    }
    let usable = |c: &&ImportCandidate| c.error.is_none() && c.duplicate.as_deref() != Some("path");
    let mut moved: HashMap<String, String> = HashMap::new();
    for c in candidates.iter().filter(usable) {
        let hash = probes.get(&c.path).and_then(|i| i.content_hash.as_deref());
        if let Some(from) = hash.and_then(|h| by_hash.get_mut(base_hash(h))).and_then(std::collections::VecDeque::pop_front) {
            moved.insert(from.to_string(), c.path.clone());
        }
    }
    let key = |size: u64, w: u32, h: u32| (size > 0 && w > 0 && h > 0).then_some((size, w, h));
    let mut hashless: HashMap<(u64, u32, u32), Vec<&str>> = HashMap::new();
    for (path, p) in &files {
        if let Some(p) = p.filter(|p| p.content_hash.is_none())
            && let Some(k) = key(p.file_size, p.width, p.height)
        {
            hashless.entry(k).or_default().push(path);
        }
    }
    let mut found: HashMap<(u64, u32, u32), Vec<&str>> = HashMap::new();
    for c in candidates.iter().filter(usable).filter(|c| !moved.values().any(|to| *to == c.path)) {
        if let Some(k) = key(c.file_size, c.width, c.height) {
            found.entry(k).or_default().push(c.path.as_str());
        }
    }
    for (k, from) in hashless {
        if let ([from], Some([to])) = (from.as_slice(), found.get(&k).map(Vec::as_slice)) {
            moved.insert(from.to_string(), to.to_string());
        }
    }
    moved
}

/// What checking one photo of the folder found.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Check {
    #[default]
    Nothing,
    Missing,
    News,
}

fn check_photo(p: &Photo, naming: SidecarNaming, labels: &Catalog) -> Check {
    let Some(file) = crate::cmd::missing::checked_path(p) else { return Check::Nothing };
    if !Path::new(file).exists() {
        Check::Missing
    } else if p.copy_of.is_none() && sidecar_has_news(labels, p, naming) {
        Check::News
    } else {
        Check::Nothing
    }
}

/// [`check_photo`] for each photo, in order, on several threads (each file is a wait on the
/// disk, not work for the CPU). Photos not reached before a cancel count as nothing found.
fn check_photos(photos: &[(Arc<Photo>, SidecarNaming)], labels: &Catalog, progress: &SyncProgress) -> Vec<Check> {
    use std::sync::atomic::Ordering::Relaxed;
    progress.to_check.store(photos.len(), Relaxed);
    let cancel = &progress.files.cancel;
    let check = |p: &Photo, naming: SidecarNaming| {
        let c = check_photo(p, naming, labels);
        progress.checked.fetch_add(1, Relaxed);
        c
    };
    if cfg!(target_arch = "wasm32") {
        return vec![Check::Nothing; photos.len()];
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let n = crate::import::workers(photos.len());
        if n > 1 {
            let next = std::sync::atomic::AtomicUsize::new(0);
            let found = std::sync::Mutex::new(vec![Check::Nothing; photos.len()]);
            std::thread::scope(|sc| {
                for _ in 0..n {
                    sc.spawn(|| {
                        while !cancel.load(Relaxed) {
                            let i = next.fetch_add(1, Relaxed);
                            let Some((p, naming)) = photos.get(i) else { break };
                            let c = check(p, *naming);
                            if let Some(slot) = found.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get_mut(i) {
                                *slot = c;
                            }
                        }
                    });
                }
            });
            return found.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
    photos.iter().map(|(p, naming)| if cancel.load(Relaxed) { Check::Nothing } else { check(p, *naming) }).collect()
}

/// [`scan_with`] on the calling thread.
pub fn scan(s: &mut Session, path: &str, disk: bool) -> Result<FolderChanges> {
    let input = SyncInput::new(s, path, disk)?;
    let changes = scan_with(input, &SyncProgress::default());
    if changes.offline {
        return Err(not_there(&changes.path));
    }
    Ok(changes)
}

fn not_there(path: &str) -> EngineError {
    invalid(format!("{path} is not there (moved, renamed, or on a disk that isn't connected)"))
}

impl Session {
    /// The last scan ([`Session::folder_changes`]) if it is of folder `path` and still current:
    /// the folder holds the same photos, none of them changed. What the person was shown is what
    /// gets done. Used once. (Changes elsewhere in the library don't matter.)
    pub fn take_folder_changes(&mut self, path: &str) -> Option<FolderChanges> {
        let c = self.folder_changes.take()?;
        if lightcraft_catalog::query::folder_key(&c.path) != lightcraft_catalog::query::folder_key(path) {
            return None;
        }
        let f = Filter { library_folder: Some(path.to_string()), ..Default::default() };
        let mut now: Vec<PhotoId> = self.catalog.query(&f, &Sort::default());
        let mut then: Vec<PhotoId> = c.seen.iter().map(|p| p.id).collect();
        now.sort();
        then.sort();
        let unchanged = now == then && c.seen.iter().all(|p| self.catalog.photo(p.id).is_some_and(|q| same_for_sync(p, q)));
        unchanged.then_some(c)
    }
}

/// Whether `p`'s XMP sidecar file was saved after the library last had its say about the photo
/// (imported, or edited here) and says something the library doesn't.
fn sidecar_has_news(labels: &Catalog, p: &Photo, naming: SidecarNaming) -> bool {
    let Source::File { path } = &p.source else { return false };
    let Some(file) = crate::sidecar::find_sidecar(path, naming) else { return false };
    let Ok(meta) = std::fs::metadata(&file) else { return false };
    if meta.len() > MAX_SIDECAR_BYTES {
        return false;
    }
    let saved = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs());
    let Some(saved) = saved.and_then(|s| i64::try_from(s).ok()) else { return false };
    let seen = [Some(p.imported.as_str()), p.edited.as_deref()].into_iter().flatten().filter_map(lightcraft_catalog::stacks::iso_seconds).max();
    if seen.is_some_and(|t| saved <= t) {
        return false;
    }
    let Ok(text) = std::fs::read_to_string(&file) else { return false };
    let Ok(sc) = crate::sidecar::parse_sidecar(&text, crate::crs::Target::for_photo(p)) else { return false };
    let sc = sc.resolve_label(labels);
    let mut q = p.clone();
    crate::sidecar::merge_into(&mut q, &sc, "");
    (q.rating, q.flag, q.label, &q.meta, &q.captured, &q.develop) != (p.rating, p.flag, p.label, &p.meta, &p.captured, &p.develop)
}

/// Act on `changes` as `choice` says, as one undo step ("Synchronize Folder"). A missing photo
/// whose file is back by now is left alone. Once the import is in, a part that fails is
/// reported in `failed` and the rest is still done (and still one step).
pub fn synchronize(s: &mut Session, changes: FolderChanges, choice: SyncChoice) -> Result<SyncReport> {
    let (work, mut commit) = SyncJob::start(s, changes, choice)?;
    let mut steps = Vec::new();
    work.run(&SyncProgress::default(), |step| {
        steps.push(step);
        true
    });
    for step in steps {
        commit.apply(s, step);
    }
    Ok(commit.finish(s))
}

/// Synchronizing in halves, so no file is touched on the UI thread: [`SyncJob::start`] takes what
/// it needs from the session (no file-system call); the [`SyncWork`] does every file read (the
/// import's probes, whether files are still where the scan found them, the sidecars) and hands
/// over [`SyncStep`]s — on a worker thread in the app; [`SyncCommit::apply`] puts each into the
/// catalog and [`SyncCommit::finish`] makes them one undo step.
pub struct SyncJob;

/// The file-system half of a synchronize (see [`SyncJob`]).
pub struct SyncWork {
    /// The import of the new files, and the files.
    import: Option<(crate::import::ImportJob, Vec<String>)>,
    moved: Vec<FolderMoved>,
    missing: Vec<FolderPhoto>,
    /// (photo, its file, how its sidecar is named)
    sidecars: Vec<(PhotoId, String, SidecarNaming, crate::crs::Target)>,
}

/// The catalog half of a synchronize (see [`SyncJob`]).
pub struct SyncCommit {
    opts: ImportOptions,
    now: String,
    /// The library the run was readied for: steps never land in another one (File › Open
    /// Library while the work runs), whose photo ids name other photos.
    library: Arc<()>,
    /// The run's own undo steps so far, and the history's length after the last of them: they
    /// are merged only while nothing else came in between (an Undo or an edit made meanwhile
    /// stays its own step, and the run's parts stay theirs).
    own: usize,
    after_last: Option<usize>,
    interleaved: bool,
    report: SyncReport,
}

/// A piece of work done, for [`SyncCommit::apply`].
pub enum SyncStep {
    /// A batch of new files, readied for the catalog.
    Import(crate::import::Prepared),
    /// Moved photos whose file is still at its new place (photo, new path).
    Relink(Vec<(PhotoId, String)>),
    /// Missing photos whose file is still gone.
    Remove(Vec<PhotoId>),
    /// Sidecars read (photo, its file, what the sidecar says or why it couldn't be read).
    Sidecars(Vec<(PhotoId, String, std::result::Result<crate::sidecar::SidecarData, String>)>),
}

impl SyncJob {
    /// Ready a synchronize of `changes` as `choice` says. Reads no file.
    pub fn start(s: &mut Session, changes: FolderChanges, choice: SyncChoice) -> Result<(SyncWork, SyncCommit)> {
        if changes.offline {
            return Err(not_there(&changes.path));
        }
        let FolderChanges { new, missing, metadata, moved, probes, .. } = changes;
        let opts = ImportOptions { mode: ImportMode::Add, ..Default::default() };
        let import = if choice.import_new && !new.is_empty() {
            s.import_probes = probes;
            let job = crate::import::ImportJob::new(s, opts.clone())?;
            Some((job, new.into_iter().map(|c| c.path).collect()))
        } else {
            None
        };
        let now = import.as_ref().map_or_else(|| (s.clock)(), |(j, _)| j.now().to_string());
        let sidecars = if choice.read_metadata {
            let namings = s.sidecar_namings(&metadata.iter().map(|m| PhotoId(m.id)).collect::<Vec<_>>());
            metadata
                .into_iter()
                .zip(namings)
                .filter_map(|(m, naming)| {
                    let p = s.catalog.photo(PhotoId(m.id))?;
                    Some((p.id, m.path, naming, crate::crs::Target::for_photo(p)))
                })
                .collect()
        } else {
            Vec::new()
        };
        let work = SyncWork {
            import,
            moved: if choice.relink_moved { moved } else { Vec::new() },
            missing: if choice.remove_missing { missing } else { Vec::new() },
            sidecars,
        };
        let commit = SyncCommit {
            opts,
            now,
            library: Arc::clone(&s.library_identity),
            own: 0,
            after_last: None,
            interleaved: false,
            report: SyncReport::default(),
        };
        Ok((work, commit))
    }
}

impl SyncWork {
    /// Do the work, handing each step to `send` (`false`: nobody takes them any more — stop).
    /// Stops before the next piece once `progress.files.cancel` is set; what was handed over
    /// stays handed over.
    pub fn run(self, progress: &SyncProgress, mut send: impl FnMut(SyncStep) -> bool) {
        use std::sync::atomic::Ordering::Relaxed;
        let cancel = &progress.files.cancel;
        let SyncWork { import, moved, missing, sidecars } = self;
        let pieces = import.as_ref().map_or(0, |(_, f)| f.len()) + moved.len() + missing.len() + sidecars.len();
        progress.to_check.store(pieces, Relaxed);
        progress.checked.store(0, Relaxed);
        let step = |n: usize| {
            progress.checked.fetch_add(n, Relaxed);
        };
        if let Some((mut job, files)) = import {
            for chunk in files.chunks(crate::import::batch_size()) {
                if cancel.load(Relaxed) {
                    return;
                }
                let prepared = job.prepare_files(chunk.to_vec(), cancel);
                step(chunk.len());
                // (the files are added in place: a batch nobody takes leaves nothing behind on disk)
                if !send(SyncStep::Import(prepared)) {
                    return;
                }
            }
        }
        if cancel.load(Relaxed) {
            return;
        }
        // still where the scan found it, and still gone from where the photo says
        let checked = moved.len();
        let relink: Vec<(PhotoId, String)> =
            moved.into_iter().filter(|m| Path::new(&m.to).is_file() && !Path::new(&m.from).exists()).map(|m| (PhotoId(m.id), m.to)).collect();
        step(checked);
        if !relink.is_empty() && !send(SyncStep::Relink(relink)) {
            return;
        }
        if cancel.load(Relaxed) {
            return;
        }
        let gone: Vec<PhotoId> = missing.iter().filter(|m| !Path::new(&m.path).exists()).map(|m| PhotoId(m.id)).collect();
        step(missing.len());
        if !gone.is_empty() && !send(SyncStep::Remove(gone)) {
            return;
        }
        let mut read = Vec::with_capacity(sidecars.len());
        for (id, path, naming, target) in sidecars {
            if cancel.load(Relaxed) {
                break;
            }
            read.push((id, path.clone(), read_sidecar(&path, naming, target)));
            step(1);
        }
        if !read.is_empty() {
            send(SyncStep::Sidecars(read));
        }
    }
}

/// Read and parse photo file `path`'s XMP sidecar file.
fn read_sidecar(path: &str, naming: SidecarNaming, target: crate::crs::Target) -> std::result::Result<crate::sidecar::SidecarData, String> {
    let file = crate::sidecar::find_sidecar(path, naming).ok_or_else(|| "no XMP sidecar".to_string())?;
    let len = std::fs::metadata(&file).map_err(|e| format!("{}: {e}", file.display()))?.len();
    if len > MAX_SIDECAR_BYTES {
        return Err(format!("{}: too large for an XMP sidecar", file.display()));
    }
    let text = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    crate::sidecar::parse_sidecar(&text, target).map_err(|e| format!("{}: {e}", file.display()))
}

impl SyncCommit {
    /// Put a step into the catalog (no file is touched). A part that fails is reported in the
    /// end, and the rest is still done.
    pub fn apply(&mut self, s: &mut Session, step: SyncStep) {
        if !Arc::ptr_eq(&self.library, &s.library_identity) {
            self.report.failed.push((String::new(), "another library was opened: the rest of the synchronize was dropped".into()));
            return;
        }
        let commits = s.commits();
        if self.after_last.is_some_and(|n| n != s.undo.len()) {
            self.interleaved = true;
        }
        self.apply_step(s, step);
        if s.commits() != commits {
            self.own += usize::try_from(s.commits().wrapping_sub(commits)).unwrap_or(0);
            self.after_last = Some(s.undo.len());
        }
    }

    fn apply_step(&mut self, s: &mut Session, step: SyncStep) {
        let report = &mut self.report;
        match step {
            SyncStep::Import(prepared) => match crate::import::commit_prepared(s, &self.opts, &self.now, prepared) {
                Ok(r) => {
                    report.imported += r.imported.len();
                    report.failed.extend(r.failed);
                }
                Err(e) => report.failed.push((String::new(), e.to_string())),
            },
            SyncStep::Relink(moved) => {
                let ops: Vec<Op> =
                    moved.iter().filter(|(id, _)| s.catalog.photo(*id).is_some()).map(|(id, to)| crate::cmd::missing::relink_op(*id, to)).collect();
                let n = ops.len();
                if n > 0 {
                    match s.commit("Relink Moved Photos", Op::Batch { ops }) {
                        Ok(()) => report.relinked += n,
                        Err(e) => report.failed.push((String::new(), e.to_string())),
                    }
                }
            }
            SyncStep::Remove(gone) => {
                let ops: Vec<Op> = gone
                    .iter()
                    .filter(|id| s.catalog.photo(**id).is_some_and(|p| p.in_library()))
                    .map(|id| Op::SetDeleted { id: *id, deleted: true })
                    .collect();
                let n = ops.len();
                if n > 0 {
                    match s.commit("Remove Missing Photos", Op::Batch { ops }) {
                        Ok(()) => report.removed += n,
                        Err(e) => report.failed.push((String::new(), e.to_string())),
                    }
                }
            }
            SyncStep::Sidecars(read) => {
                let mut ops = Vec::new();
                for (id, path, sc) in read {
                    match sc.map_err(EngineError::Other).and_then(|sc| s.sidecar_op(id, sc)) {
                        Ok(op) => ops.push(op),
                        Err(e) => report.failed.push((path, e.to_string())),
                    }
                }
                let n = ops.len();
                if n > 0 {
                    match s.commit("Read Metadata from File", Op::Batch { ops }) {
                        Ok(()) => report.read += n,
                        Err(e) => report.failed.push((String::new(), e.to_string())),
                    }
                }
            }
        }
    }

    /// What was done, as one undo step ("Synchronize Folder").
    pub fn finish(mut self, s: &mut Session) -> SyncReport {
        // a report line once, however many steps came after a library switch
        self.report.failed.dedup();
        let ours = Arc::ptr_eq(&self.library, &s.library_identity) && !self.interleaved && self.after_last == Some(s.undo.len());
        let steps = if ours { self.own.min(s.undo.len()) } else { 0 };
        s.merge_undo(steps, "Synchronize Folder");
        if let Some(last) = s.undo.last_mut().filter(|_| steps == 1) {
            last.label = "Synchronize Folder".into();
        }
        self.report
    }
}
