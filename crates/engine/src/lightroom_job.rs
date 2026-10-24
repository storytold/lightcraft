//! Lightroom catalog import in batches: read off the owner thread, added on it.
//!
//! [`LightroomJob::new`] takes only an in-memory session snapshot. On a worker,
//! [`LightroomJob::start`] reads the catalog, stages its recovery archive and loads the import
//! index; each [`LightroomJob::next_batch`] then probes the originals of the next photos — from
//! their headers, so a raw's pixel data is never read — and parses their XMP and maps their develop
//! settings. On the owner thread [`LightroomImport::commit`] adds each batch as it arrives: photos
//! appear while the rest is read, and a cancel or a crash keeps what was added (importing the
//! catalog again continues where it stopped). [`LightroomImport::finish`] adds the collections,
//! makes the import one undo step, and returns the archive-index write for a worker.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use lightcraft_catalog::{Photo, PhotoId, Source};
use serde_json::Value;

use crate::import::{ImportJob, ImportMode, ImportOptions, Prepared};
use crate::lightroom_catalog::{self, CatalogImport, CatalogPhoto, ImportIndex, LightroomApply, MappedPhoto, Row, Shape, catalog_photo, key};

const COMMAND: &str = "library.importLightroom";
const MAX_INDEX_BYTES: usize = 16 * 1024 * 1024;
/// Catalog photos per batch: each batch is added in one short owner-thread step.
pub const BATCH: usize = 256;

fn error(message: impl Into<String>) -> crate::EngineError {
    crate::EngineError::BadParams { cmd: COMMAND.into(), msg: message.into() }
}

/// The import was cancelled ([`crate::ErrorKind::Cancelled`]: not a failure). The message is the
/// one it always had, so control-channel and MCP clients see the same `error` text.
fn cancelled() -> crate::EngineError {
    crate::EngineError::Cancelled(error("Lightroom import cancelled").to_string())
}

fn stage_archive(archive_dir: Option<&Path>, data: &mut CatalogImport) -> crate::Result<Option<PathBuf>> {
    let archive_path = archive_dir.map(|dir| crate::lightroom_archive::store_best_effort(dir, data).map_err(error)).transpose()?.flatten();
    if archive_dir.is_some() && archive_path.is_none() {
        data.warnings.push("Lightroom source archive exceeded 32 MiB; archive skipped".into());
    }
    Ok(archive_path)
}

/// Memory identity of the library which owns a prepared completion.
///
/// Each successful open/close refreshes session identity, including reopening the same path.
/// The owner check deliberately does not compare catalog revisions: ordinary edits while preparation
/// runs must survive and are merged by the owner commit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LightroomLibraryToken {
    library_dir: Option<PathBuf>,
    identity: Arc<()>,
}

impl LightroomLibraryToken {
    fn capture(s: &crate::Session) -> Self {
        Self {
            library_dir: s.library.as_ref().filter(|library| library.on_disk).map(|library| library.dir.clone()),
            identity: Arc::clone(&s.library_identity),
        }
    }

    /// Whether `s` still refers to the library which started preparation.
    pub fn matches(&self, s: &crate::Session) -> bool {
        Arc::ptr_eq(&self.identity, &s.library_identity)
            && self.library_dir == s.library.as_ref().filter(|library| library.on_disk).map(|library| library.dir.clone())
    }

    pub fn library_dir(&self) -> Option<&Path> {
        self.library_dir.as_deref()
    }
}

/// What reading the catalog gives the owner thread before the first batch ([`LightroomImport::begin`]).
pub struct LightroomStart {
    source: String,
    index: ImportIndex,
    archive_path: Option<PathBuf>,
    warnings: Vec<String>,
    /// The catalog's images the reader skipped (incomplete records), for the report.
    skipped: Vec<lightroom_catalog::SkippedImage>,
    collections: Vec<Row>,
    members: Vec<Row>,
    photos: usize,
    token: LightroomLibraryToken,
    update_existing: bool,
    now: String,
    progress: LightroomProgress,
}

impl LightroomStart {
    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn photo_count(&self) -> usize {
        self.photos
    }

    pub fn token(&self) -> &LightroomLibraryToken {
        &self.token
    }
}

/// The next photos of an import, probed and mapped, for [`LightroomImport::commit`]. Nothing in the
/// library has changed yet.
pub struct LightroomBatch {
    photos: Vec<MappedPhoto>,
    normal: Prepared,
    missing_sources: HashSet<String>,
    token: LightroomLibraryToken,
}

impl LightroomBatch {
    pub fn len(&self) -> usize {
        self.photos.len()
    }

    pub fn is_empty(&self) -> bool {
        self.photos.is_empty()
    }

    pub fn token(&self) -> &LightroomLibraryToken {
        &self.token
    }

    /// Undo the batch's file placement (none for an import that adds files where they are).
    pub fn rollback(&self) {
        self.normal.rollback();
    }
}

/// Index write returned by owner commit.  `finish` is intended for a worker after the owner has
/// returned, and performs the only archive-index filesystem operation in this pipeline.
pub struct LightroomArchiveFinalization {
    path: Option<PathBuf>,
    bytes: Option<Vec<u8>>,
}

impl LightroomArchiveFinalization {
    pub fn finish(self) -> crate::Result<()> {
        let (Some(path), Some(bytes)) = (self.path, self.bytes) else { return Ok(()) };
        if bytes.len() > MAX_INDEX_BYTES {
            return Err(error("Lightroom import index exceeds 16 MiB"));
        }
        // (the archive's staging makes the folder; an import whose archive was skipped may not have it)
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| error(format!("{}: {e}", dir.display())))?;
        }
        lightcraft_catalog::safe_file::write_atomic(&path, &bytes).map_err(|e| error(e.to_string()))
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

/// Owner-thread catalog result.  The report is ready for the UI immediately; index finalization
/// can remain on the worker while its task stays alive.
pub struct LightroomCompletion {
    pub report: Value,
    pub finalization: LightroomArchiveFinalization,
}

/// Alias used by hosts that call the result a commit.
pub type LightroomCommit = LightroomCompletion;

/// The step of preparing a Lightroom import that is under way ([`LightroomProgress::step`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LightroomStep {
    /// Reading the catalog's tables.
    Catalog,
    /// Reading the photos the catalog names (headers, content hash): nearly all of the time.
    ReadPhotos,
    /// Adding the photos read (batch by batch, as they arrive) and then the collections.
    AddPhotos,
}

/// Progress of a Lightroom import step by step, shared by its worker ([`LightroomJob`]) and owner
/// ([`LightroomImport`]) and watched by the host.
#[derive(Clone, Default)]
pub struct LightroomProgress {
    /// Catalog tables to read and read so far ([`lightroom_catalog::read_with_progress`]).
    pub catalog_total: Arc<AtomicUsize>,
    pub catalog_done: Arc<AtomicUsize>,
    read_total: Arc<AtomicUsize>,
    read_done: Arc<AtomicUsize>,
    add_total: Arc<AtomicUsize>,
    add_done: Arc<AtomicUsize>,
    /// The catalog is read: the photos are next.
    photos: Arc<AtomicBool>,
}

impl LightroomProgress {
    /// Reading the photos is nearly all of an import's time; the catalog and readying the photos
    /// take the rest of [`LightroomProgress::fraction`].
    const CATALOG_SHARE: f32 = 0.05;
    const READ_SHARE: f32 = 0.85;

    /// The step under way, with how much of it is done and its total (0 while not yet known).
    pub fn step(&self) -> (LightroomStep, usize, usize) {
        let load = |n: &AtomicUsize| n.load(Ordering::Relaxed);
        if !self.photos.load(Ordering::Relaxed) {
            return (LightroomStep::Catalog, load(&self.catalog_done), load(&self.catalog_total));
        }
        let (read_done, read_total) = (load(&self.read_done), load(&self.read_total));
        let (add_done, add_total) = (load(&self.add_done), load(&self.add_total));
        // nothing to read (every photo is already in the library) goes straight to adding
        if read_done < read_total || (read_total == 0 && add_done == 0) {
            (LightroomStep::ReadPhotos, read_done, read_total)
        } else {
            (LightroomStep::AddPhotos, add_done, add_total)
        }
    }

    /// The whole preparation, 0..=1.
    pub fn fraction(&self) -> f32 {
        let (step, done, total) = self.step();
        let part = if total == 0 { 0.0 } else { (done as f32 / total as f32).clamp(0.0, 1.0) };
        match step {
            LightroomStep::Catalog => Self::CATALOG_SHARE * part,
            LightroomStep::ReadPhotos => Self::CATALOG_SHARE + Self::READ_SHARE * part,
            LightroomStep::AddPhotos => Self::CATALOG_SHARE + Self::READ_SHARE + (1.0 - Self::CATALOG_SHARE - Self::READ_SHARE) * part,
        }
    }
}

/// The worker half of a Lightroom import ([`LightroomJob::start`], then [`LightroomJob::next_batch`]).
pub struct LightroomJob {
    path: PathBuf,
    update_existing: bool,
    import: ImportJob,
    token: LightroomLibraryToken,
    archive_dir: Option<PathBuf>,
    /// Photos already in the library, by path: the shape each catalog record's mapping is for.
    existing: HashMap<String, Shape>,
    progress: LightroomProgress,
    /// The photos still to prepare, masters ahead of their virtual copies.
    queue: VecDeque<CatalogPhoto>,
    /// The shape of each master prepared so far (its virtual copies share it).
    shapes: HashMap<i64, Shape>,
    /// The originals looked for so far (by [`key`]).
    seen: HashSet<String>,
}

impl LightroomJob {
    /// Snapshot session state only.  This constructor performs no filesystem work.
    pub fn new(s: &mut crate::Session, path: PathBuf, update_existing: bool) -> crate::Result<Self> {
        // originals stay where they are and are described from their headers (no content hash);
        // a photo deleted here stays deleted: the catalog's record is matched, not restored
        let opts =
            ImportOptions { mode: ImportMode::Add, on_deleted: crate::import::OnDeleted::Skip, headers_only: true, ..ImportOptions::default() };
        let import = ImportJob::new(s, opts)?;
        // an original counts as soon as it is read (the slow part, file by file); the whole
        // import's photos are counted from the start, not batch by batch
        let progress = LightroomProgress { read_done: Arc::clone(&import.read_done), ..LightroomProgress::default() };
        let archive_dir = s.library.as_ref().filter(|library| library.on_disk).map(|library| library.dir.join("Interop"));
        let existing = s
            .catalog
            .photos()
            .filter(|p| p.copy_of.is_none())
            .filter_map(|p| match &p.source {
                Source::File { path } => Some((key(path), Shape::of(p))),
                _ => None,
            })
            .collect();
        Ok(Self {
            path,
            update_existing,
            import,
            token: LightroomLibraryToken::capture(s),
            archive_dir,
            existing,
            progress,
            queue: VecDeque::new(),
            shapes: HashMap::new(),
            seen: HashSet::new(),
        })
    }

    /// The import's progress, to watch from another thread while the job runs.
    pub fn progress(&self) -> LightroomProgress {
        self.progress.clone()
    }

    pub fn token(&self) -> &LightroomLibraryToken {
        &self.token
    }

    /// Finish an owner commit's deferred archive-index write on a worker.
    pub fn finish_archive(finalization: LightroomArchiveFinalization) -> crate::Result<()> {
        finalization.finish()
    }

    /// Read and validate the catalog, stage its recovery archive and load the import index (off the
    /// owner thread). Progress: the catalog's tables, then one unit per photo.
    pub fn start(&mut self, cancel: &AtomicBool) -> crate::Result<LightroomStart> {
        if cancel.load(Ordering::Relaxed) {
            return Err(cancelled());
        }
        // (reading stops with a message when `cancel` is set: the flag says it was cancelled)
        let mut data = lightroom_catalog::read_with_progress(&self.path, cancel, &self.progress.catalog_total, &self.progress.catalog_done)
            .map_err(|e| if cancel.load(Ordering::Relaxed) { cancelled() } else { error(e) })?;
        lightroom_catalog::validate_for_job(&data).map_err(error)?;
        if cancel.load(Ordering::Relaxed) {
            return Err(cancelled());
        }
        let archive_path = stage_archive(self.archive_dir.as_deref(), &mut data)?;
        let index_path = self.archive_dir.as_ref().map(|dir| dir.join("lightroom-index.json"));
        let index = lightroom_catalog::load_index(index_path.as_deref())?;
        let mut photos = std::mem::take(&mut data.photos);
        photos.sort_by_key(|p| lightroom_catalog::number_for_job(&p.image, "masterImage") != 0);
        // history and snapshots were for the archive only
        for p in &mut photos {
            p.history = Vec::new();
            p.snapshots = Vec::new();
        }
        let n = photos.len();
        let masters = photos.iter().filter(|p| lightroom_catalog::number_for_job(&p.image, "masterImage") == 0).count();
        // the catalog step's last unit (validation, archive, index), then the photos
        self.progress.read_total.store(masters, Ordering::Relaxed);
        self.progress.add_total.store(n, Ordering::Relaxed);
        self.progress.catalog_done.fetch_add(1, Ordering::Relaxed);
        self.progress.photos.store(true, Ordering::Relaxed);
        self.queue = photos.into();
        Ok(LightroomStart {
            source: data.source,
            index,
            archive_path,
            warnings: data.warnings,
            skipped: data.skipped,
            collections: data.collections,
            members: data.members,
            photos: n,
            token: self.token.clone(),
            update_existing: self.update_existing,
            now: self.import.now().to_string(),
            progress: self.progress.clone(),
        })
    }

    /// Probe and map the next photos (off the owner thread); `None` once every photo was prepared.
    /// Stops at the next original once `cancel` is set.
    pub fn next_batch(&mut self, cancel: &AtomicBool) -> Option<crate::Result<LightroomBatch>> {
        if self.queue.is_empty() {
            return None;
        }
        if cancel.load(Ordering::Relaxed) {
            return Some(Err(cancelled()));
        }
        let n = self.queue.len().min(BATCH);
        let photos: Vec<CatalogPhoto> = self.queue.drain(..n).collect();
        let before = self.progress.read_done.load(Ordering::Relaxed);
        let is_master = |p: &CatalogPhoto| lightroom_catalog::number_for_job(&p.image, "masterImage") == 0;
        // Each original is looked for once. A missing one is not read: its catalog record is
        // enough for a placeholder, and probing it would only add failed I/O and errors.
        let (mut paths, mut missing_sources, mut masters) = (Vec::new(), HashSet::new(), 0);
        for p in photos.iter().filter(|p| is_master(p)) {
            if cancel.load(Ordering::Relaxed) {
                return Some(Err(cancelled()));
            }
            masters += 1;
            let k = key(&p.path);
            if !self.seen.insert(k.clone()) {
                continue;
            }
            if p.original_is_file() {
                paths.push(p.path.clone());
            } else {
                missing_sources.insert(k);
            }
        }
        let normal = self.import.prepare_files(paths, cancel);
        if cancel.load(Ordering::Relaxed) {
            normal.rollback();
            return Some(Err(cancelled()));
        }
        // the shape of each photo-to-be: the library's photo, else the probed file, else the catalog's record
        let mut probe = Photo::new(PhotoId(0), Source::File { path: String::new() }, "", "", 0, 0, "");
        let probed: HashMap<String, Shape> = normal
            .readied()
            .map(|(path, info)| {
                probe.kind = info.kind;
                probe.format.clone_from(&info.format);
                probe.preview_only.clone_from(&info.preview_only);
                probe.meta.camera.clone_from(&info.meta.camera);
                (probe.width, probe.height) = (info.width, info.height);
                (key(path), Shape::of(&probe))
            })
            .collect();
        let mut mapped = Vec::with_capacity(photos.len());
        for src in photos {
            let master = lightroom_catalog::number_for_job(&src.image, "masterImage");
            let from_catalog = || Shape::of(&catalog_photo(&src, PhotoId(0), ""));
            let shape = if master == 0 {
                let k = key(&src.path);
                let shape = self.existing.get(&k).or_else(|| probed.get(&k)).copied().unwrap_or_else(from_catalog);
                self.shapes.insert(src.source_id, shape);
                shape
            } else {
                self.shapes.get(&master).copied().unwrap_or_else(from_catalog)
            };
            mapped.push(MappedPhoto::new(src, shape));
        }
        // (an original already in the library is not read, but is done with)
        self.progress.read_done.store(before + masters, Ordering::Relaxed);
        Some(Ok(LightroomBatch { photos: mapped, normal, missing_sources, token: self.token.clone() }))
    }
}

/// The owner half of a Lightroom import: batches added as they arrive, then the collections.
pub struct LightroomImport {
    apply: LightroomApply,
    collections: Vec<Row>,
    members: Vec<Row>,
    archive_path: Option<PathBuf>,
    token: LightroomLibraryToken,
    steps: lightroom_catalog::ImportSteps,
    photos: usize,
    now: String,
    progress: LightroomProgress,
}

impl LightroomImport {
    /// Begin adding `start`'s import to the session whose library started its job.
    pub fn begin(s: &crate::Session, start: LightroomStart) -> crate::Result<Self> {
        if !start.token.matches(s) {
            return Err(error("Lightroom import belongs to a different open library"));
        }
        let mut apply = LightroomApply::new(s, start.source, start.update_existing, start.index, start.warnings, start.now.clone());
        apply.skipped = start.skipped;
        Ok(Self {
            apply,
            collections: start.collections,
            members: start.members,
            archive_path: start.archive_path,
            token: start.token,
            steps: lightroom_catalog::ImportSteps::default(),
            photos: start.photos,
            now: start.now,
            progress: start.progress,
        })
    }

    pub fn token(&self) -> &LightroomLibraryToken {
        &self.token
    }

    /// Photos added so far, and in all.
    pub fn progress(&self) -> (usize, usize) {
        (self.apply.len(), self.photos)
    }

    /// Add one batch (its undo steps fold into the import's: [`lightroom_catalog::ImportSteps`]). No source,
    /// archive or index filesystem work; source XMP sidecars are never rewritten.
    ///
    /// A batch with virtual copies returns the import index's write, for the caller to run (on a
    /// worker, after the one before it) once the batch is saved: importing again after a crash
    /// finds a master by its path, but a virtual copy only through the index, and would add it
    /// again. (Copies come after every master, so this is the import's last few batches.)
    pub fn commit(&mut self, s: &mut crate::Session, mut batch: LightroomBatch) -> crate::Result<Option<LightroomArchiveFinalization>> {
        if !self.token.matches(s) || !batch.token.matches(s) {
            batch.normal.rollback();
            return Err(error("Lightroom import belongs to a different open library"));
        }
        // a command-scoped import must never rewrite source XMP sidecars (direct depth-0 callers
        // don't set this transient flag, so it can't affect a later command)
        if s.depth > 0 {
            s.skip_auto_write = true;
        }
        batch.normal.revalidate_add(s);
        let options =
            ImportOptions { mode: ImportMode::Add, on_deleted: crate::import::OnDeleted::Skip, headers_only: true, ..ImportOptions::default() };
        let n = batch.photos.len();
        let copies = batch.photos.iter().any(|m| lightroom_catalog::number_for_job(&m.src.image, "masterImage") != 0);
        let Self { steps, apply, now, .. } = self;
        steps.run(s, |s| {
            let files = crate::import::commit_prepared(s, &options, now, batch.normal)?;
            apply.batch(s, batch.photos, files, &batch.missing_sources)
        })?;
        self.progress.add_done.fetch_add(n, Ordering::Relaxed);
        if !copies || self.token.library_dir().is_none() {
            return Ok(None);
        }
        self.index_write().map(Some)
    }

    /// The import index's write: the identities of the photos added so far (none for a library
    /// that isn't on disk).
    fn index_write(&self) -> crate::Result<LightroomArchiveFinalization> {
        let path = self.token.library_dir().map(|dir| dir.join("Interop/lightroom-index.json"));
        let bytes = if path.is_some() { Some(serde_json::to_vec(&self.apply.index).map_err(|e| error(e.to_string()))?) } else { None };
        Ok(LightroomArchiveFinalization { path, bytes })
    }

    /// Finish the import: its collections become albums when `complete` (every batch was added;
    /// a cancelled import adds them when imported again), and the report. The archive-index write (the photos added, so importing again continues) is
    /// returned for a worker.
    pub fn finish(mut self, s: &mut crate::Session, complete: bool) -> crate::Result<LightroomCompletion> {
        // (another library's undo steps and albums are not this import's to touch)
        if !self.token.matches(s) {
            return Err(error("Lightroom import belongs to a different open library"));
        }
        if s.depth > 0 {
            s.skip_auto_write = true;
        }
        let Self { steps, apply, collections, members, archive_path, .. } = &mut self;
        let report = steps.run(s, |s| apply.finish(s, collections, members, complete, archive_path.as_deref()))?;
        Ok(LightroomCompletion { report, finalization: self.index_write()? })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_catalog::Op;
    use std::collections::BTreeMap;

    fn image(id: i64, extra: &[(&str, Value)]) -> BTreeMap<String, Value> {
        let mut image = BTreeMap::from([
            ("id_local".into(), serde_json::json!(id)),
            ("fileFormat".into(), serde_json::json!("JPG")),
            ("fileWidth".into(), serde_json::json!(1)),
            ("fileHeight".into(), serde_json::json!(1)),
        ]);
        for (k, v) in extra {
            image.insert((*k).into(), v.clone());
        }
        image
    }

    fn photo(id: i64, path: &str, extra: &[(&str, Value)]) -> CatalogPhoto {
        CatalogPhoto {
            source_id: id,
            uuid: format!("fixture-{id}"),
            path: path.into(),
            image: image(id, extra),
            settings: String::new(),
            xmp: String::new(),
            keywords: Vec::new(),
            history: Vec::new(),
            snapshots: Vec::new(),
            original: lightroom_catalog::Original::Local,
        }
    }

    fn start(s: &crate::Session, photos: usize) -> LightroomStart {
        LightroomStart {
            source: "fixture.lrcat".into(),
            index: ImportIndex::default(),
            archive_path: None,
            warnings: Vec::new(),
            skipped: Vec::new(),
            collections: Vec::new(),
            members: Vec::new(),
            photos,
            token: LightroomLibraryToken::capture(s),
            update_existing: false,
            now: "2026-01-01T00:00:00".into(),
            progress: LightroomProgress::default(),
        }
    }

    /// A batch of photos whose originals are missing (as a worker hands it over).
    fn batch(s: &crate::Session, photos: Vec<CatalogPhoto>) -> LightroomBatch {
        let missing_sources = photos.iter().map(|p| key(&p.path)).collect();
        let photos = photos
            .into_iter()
            .map(|p| {
                let shape = Shape::of(&catalog_photo(&p, PhotoId(0), ""));
                MappedPhoto::new(p, shape)
            })
            .collect();
        LightroomBatch { photos, normal: Prepared::default(), missing_sources, token: LightroomLibraryToken::capture(s) }
    }

    fn import_all(s: &mut crate::Session, batches: Vec<Vec<CatalogPhoto>>) -> LightroomCompletion {
        let n = batches.iter().map(Vec::len).sum();
        let mut import = LightroomImport::begin(s, start(s, n)).unwrap();
        for photos in batches {
            let b = batch(s, photos);
            import.commit(s, b).unwrap();
        }
        import.finish(s, true).unwrap()
    }

    /// One count for the whole import read "14 of 24554" while the photos were read, then twice
    /// the number of photos: each step has its own count, and the whole only moves forward.
    #[test]
    fn progress_names_each_step_with_its_own_count() {
        let p = LightroomProgress::default();
        let set = |n: &AtomicUsize, v: usize| n.store(v, Ordering::Relaxed);
        assert_eq!(p.step(), (LightroomStep::Catalog, 0, 0));
        assert_eq!(p.fraction(), 0.0);
        set(&p.catalog_total, 14);
        set(&p.catalog_done, 7);
        assert_eq!(p.step(), (LightroomStep::Catalog, 7, 14));
        let mut last = p.fraction();
        assert!(last > 0.0);
        let mut forward = |p: &LightroomProgress| {
            let f = p.fraction();
            assert!(f >= last && f <= 1.0, "{f} after {last}");
            last = f;
        };
        set(&p.catalog_done, 14);
        p.photos.store(true, Ordering::Relaxed);
        assert_eq!(p.step(), (LightroomStep::ReadPhotos, 0, 0), "the photos are not counted yet");
        forward(&p);
        set(&p.add_total, 100);
        set(&p.read_total, 100);
        set(&p.read_done, 40);
        assert_eq!(p.step(), (LightroomStep::ReadPhotos, 40, 100));
        forward(&p);
        set(&p.read_done, 100);
        set(&p.add_done, 25);
        assert_eq!(p.step(), (LightroomStep::AddPhotos, 25, 100));
        forward(&p);
        set(&p.add_done, 100);
        forward(&p);
        assert!(p.fraction() > 0.999);
        // every photo already in the library: nothing to read
        let known = LightroomProgress::default();
        known.photos.store(true, Ordering::Relaxed);
        set(&known.add_total, 5);
        set(&known.add_done, 5);
        assert_eq!(known.step(), (LightroomStep::AddPhotos, 5, 5));
    }

    #[test]
    fn cancelled_import_before_any_batch_leaves_the_catalog_unchanged() {
        let mut session = crate::Session::new();
        let before = session.catalog.to_snapshot();
        let import = LightroomImport::begin(&session, start(&session, 3)).unwrap();
        let completion = import.finish(&mut session, false).unwrap();
        assert_eq!(completion.report["cancelled"], true);
        assert_eq!(completion.report["photos"], 0);
        assert_eq!(session.catalog.to_snapshot(), before);
        assert!(session.undo.is_empty());
    }

    /// Cancelling while the catalog is read is [`crate::ErrorKind::Cancelled`] whatever the message
    /// says, and a failure that is not a cancellation is not one, even when its text says "cancelled".
    #[test]
    fn a_cancelled_read_is_cancelled_by_kind() {
        let mut session = crate::Session::new();
        let mut job = LightroomJob::new(&mut session, PathBuf::from("/nonexistent/cancelled.lrcat"), false).unwrap();
        let Err(e) = job.start(&AtomicBool::new(true)) else { panic!("cancelled before it started") };
        assert_eq!(e.kind(), crate::ErrorKind::Cancelled, "{e}");
        // the `error` text clients see is unchanged
        assert_eq!(e.to_string(), "invalid parameters for `library.importLightroom`: Lightroom import cancelled");
        let Err(e) = job.start(&AtomicBool::new(false)) else { panic!("no such catalog") };
        assert_ne!(e.kind(), crate::ErrorKind::Cancelled, "{e}");
    }

    #[test]
    fn library_identity_rejects_late_batches_after_reopen() {
        let mut session = crate::Session::new();
        let token = LightroomLibraryToken::capture(&session);
        assert!(token.matches(&session));
        let mut import = LightroomImport::begin(&session, start(&session, 1)).unwrap();
        session.library_identity = Arc::new(());
        assert!(!token.matches(&session));
        let b = batch(&session, vec![photo(1, "/missing/late.jpg", &[])]);
        assert!(import.commit(&mut session, b).is_err());
        assert!(session.catalog.is_empty());
        session.commit("other library's step", lightcraft_catalog::Op::Batch { ops: Vec::new() }).unwrap();
        session.commit("and another", lightcraft_catalog::Op::Batch { ops: Vec::new() }).unwrap();
        assert!(import.finish(&mut session, false).is_err());
        assert_eq!(session.undo.len(), 2, "the other library's undo steps stay as they are");
        assert!(LightroomImport::begin(&session, LightroomStart { token, ..start(&session, 1) }).is_err());
    }

    #[test]
    fn reimport_preserves_current_edit_and_uses_one_undo_per_import() {
        let mut session = crate::Session::new();
        let path = "/missing/lightroom-job-edit.jpg";
        let first = import_all(&mut session, vec![vec![photo(1, path, &[("rating", serde_json::json!(4))])]]);
        assert_eq!(first.report["mapping"]["1"].as_u64(), Some(1));
        assert_eq!(session.undo.len(), 1);
        let id = PhotoId(1);
        let mut personal = lightcraft_develop::DevelopSettings::default();
        personal.light.exposure = 1.5;
        session.set_develop(id, personal, "Personal").unwrap();
        let before = session.catalog.photo(id).map(|photo| photo.develop.clone());
        import_all(&mut session, vec![vec![photo(1, path, &[("rating", serde_json::json!(5))])]]);
        assert_eq!(session.catalog.photo(id).map(|photo| photo.develop.clone()), before);
        assert_eq!(session.catalog.photo(id).map(|photo| photo.rating), Some(5));
        assert_eq!(session.undo.len(), 3);
    }

    /// A photo outside the import, to take undo steps on.
    fn rated_photo(session: &mut crate::Session) -> PhotoId {
        let p = Photo::new(session.catalog.alloc_photo_id(), Source::File { path: "/missing/other.jpg".into() }, "other.jpg", "JPEG", 1, 1, "now");
        let id = p.id;
        session.commit("Add", Op::AddPhoto { photo: Box::new(p) }).unwrap();
        id
    }

    /// The import is one undo step even with the undo stack at its cap, where its length stops
    /// growing: counting steps by that length merged nothing (review of #1735).
    #[test]
    fn a_full_undo_stack_still_gets_one_step() {
        let mut session = crate::Session::new();
        let other = rated_photo(&mut session);
        for i in 0..1000 {
            session.commit("earlier", Op::SetRating { id: other, rating: (i % 5) as u8 }).unwrap();
        }
        assert_eq!(session.undo.len(), 1000);
        let mut import = LightroomImport::begin(&session, start(&session, 3)).unwrap();
        for i in 1..=3 {
            let b = batch(&session, vec![photo(i, &format!("/missing/full-{i}.jpg"), &[])]);
            import.commit(&mut session, b).unwrap();
        }
        import.finish(&mut session, true).unwrap();
        assert_eq!(session.catalog.len(), 4);
        assert_eq!(session.undo.last().map(|e| e.label.as_str()), Some("Import Lightroom Catalog"));
        assert_eq!(session.undo.get(session.undo.len() - 2).map(|e| e.label.as_str()), Some("earlier"));
        session.undo_step().unwrap();
        assert_eq!(session.catalog.len(), 1, "one undo takes the whole import back");
    }

    /// An edit made while the import runs stays its own undo step: it isn't merged into the
    /// import's, and the import goes on in a new step after it (review of #1735).
    #[test]
    fn an_edit_during_the_import_stays_its_own_step() {
        let mut session = crate::Session::new();
        let mut import = LightroomImport::begin(&session, start(&session, 2)).unwrap();
        let b = batch(&session, vec![photo(1, "/missing/edit-1.jpg", &[])]);
        import.commit(&mut session, b).unwrap();
        let first = session.catalog.photos().next().map(|p| p.id).unwrap();
        session.commit("Rate", Op::SetRating { id: first, rating: 4 }).unwrap();
        let b = batch(&session, vec![photo(2, "/missing/edit-2.jpg", &[])]);
        import.commit(&mut session, b).unwrap();
        import.finish(&mut session, true).unwrap();
        let labels: Vec<_> = session.undo.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(labels, ["Import Lightroom Catalog", "Rate", "Import Lightroom Catalog"]);
        session.undo_step().unwrap();
        assert_eq!(session.catalog.len(), 1, "the second part of the import is undone");
        assert_eq!(session.catalog.photo(first).map(|p| p.rating), Some(4), "the edit is not");
        session.undo_step().unwrap();
        assert_eq!(session.catalog.photo(first).map(|p| p.rating), Some(0), "then the edit");
        session.undo_step().unwrap();
        assert_eq!(session.catalog.len(), 0);
    }

    /// An undo during the import ends the import's step: the next batch starts a new one instead
    /// of folding into whatever is on top of the stack now.
    #[test]
    fn an_undo_during_the_import_is_not_folded_into_it() {
        let mut session = crate::Session::new();
        let other = rated_photo(&mut session);
        session.undo.clear();
        session.commit("Rate", Op::SetRating { id: other, rating: 1 }).unwrap();
        let mut import = LightroomImport::begin(&session, start(&session, 2)).unwrap();
        let b = batch(&session, vec![photo(1, "/missing/undo-1.jpg", &[])]);
        import.commit(&mut session, b).unwrap();
        session.undo_step().unwrap();
        let b = batch(&session, vec![photo(2, "/missing/undo-2.jpg", &[])]);
        import.commit(&mut session, b).unwrap();
        import.finish(&mut session, true).unwrap();
        let labels: Vec<_> = session.undo.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(labels, ["Rate", "Import Lightroom Catalog"], "the earlier step stays its own");
    }

    #[test]
    fn batches_add_photos_as_they_come_and_one_undo_takes_all_back() {
        let mut session = crate::Session::new();
        // the virtual copy's master is in the first batch, the copy in the last
        let mut first: Vec<_> = (1..=BATCH as i64).map(|i| photo(i, &format!("/missing/batch-{i}.jpg"), &[])).collect();
        first[0].image.insert("rating".into(), serde_json::json!(3));
        let copy = photo(9999, "/missing/batch-1.jpg", &[("masterImage", serde_json::json!(1)), ("copyName", serde_json::json!("Mono"))]);
        let mut import = LightroomImport::begin(&session, start(&session, BATCH + 1)).unwrap();
        let b = batch(&session, first);
        import.commit(&mut session, b).unwrap();
        assert_eq!(session.catalog.len(), BATCH, "the first batch is in the library before the next arrives");
        assert_eq!(import.progress(), (BATCH, BATCH + 1));
        let b = batch(&session, vec![copy]);
        import.commit(&mut session, b).unwrap();
        let completion = import.finish(&mut session, true).unwrap();
        assert_eq!(completion.report["photos"], BATCH + 1);
        let copy = session.catalog.photos().find(|p| p.copy_of.is_some()).unwrap();
        assert_eq!(copy.copy_of, Some(PhotoId(completion.report["mapping"]["1"].as_u64().unwrap())));
        assert_eq!(session.undo.len(), 1);
        session.undo_step().unwrap();
        assert!(session.catalog.is_empty());
    }

    #[test]
    fn a_cancelled_import_keeps_what_was_added_without_collections() {
        let dir = std::env::temp_dir().join(format!("lightcraft-lr-cancel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut session = crate::Session::new();
        session.open_library(&dir, false).unwrap();
        let mut s0 = start(&session, 2);
        s0.collections = vec![BTreeMap::from([
            ("id_local".into(), serde_json::json!(7)),
            ("name".into(), serde_json::json!("Keepers")),
            ("creationId".into(), serde_json::json!("collection")),
        ])];
        s0.members = vec![BTreeMap::from([("collection".into(), serde_json::json!(7)), ("image".into(), serde_json::json!(1))])];
        let mut import = LightroomImport::begin(&session, s0).unwrap();
        let b = batch(&session, vec![photo(1, "/missing/kept.jpg", &[])]);
        import.commit(&mut session, b).unwrap();
        let completion = import.finish(&mut session, false).unwrap();
        assert_eq!(completion.report["cancelled"], true);
        assert_eq!((completion.report["photos"].as_u64(), completion.report["collections"].as_u64()), (Some(1), Some(0)));
        assert_eq!(session.catalog.len(), 1);
        assert!(session.catalog.albums().next().is_none());
        // the identity of what was added is kept, so importing again continues
        let index_path = completion.finalization.path().unwrap().to_path_buf();
        completion.finalization.finish().unwrap();
        let index = lightroom_catalog::load_index(Some(&index_path)).unwrap();
        assert_eq!(index.photos.get("fixture-1"), Some(&1));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Importing again after a crash finds a master by its path, but a virtual copy only through
    /// the import index: a batch of copies hands back the index's write, so a crash before the
    /// import finishes doesn't add the saved copies a second time.
    #[test]
    fn a_crash_after_a_batch_of_copies_keeps_them_matched() {
        let dir = std::env::temp_dir().join(format!("lightcraft-lr-crash-copies-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut session = crate::Session::new();
        session.open_library(&dir, false).unwrap();
        let master = || photo(1, "/missing/crash.jpg", &[]);
        let copy = || photo(2, "/missing/crash.jpg", &[("masterImage", serde_json::json!(1)), ("copyName", serde_json::json!("Mono"))]);
        let mut import = LightroomImport::begin(&session, start(&session, 2)).unwrap();
        let b = batch(&session, vec![master()]);
        assert!(import.commit(&mut session, b).unwrap().is_none(), "masters are found again by their path");
        let b = batch(&session, vec![copy()]);
        let write = import.commit(&mut session, b).unwrap().expect("a batch of copies writes the index");
        let index_path = write.path().unwrap().to_path_buf();
        write.finish().unwrap();
        session.persist().unwrap();
        // the app stops here: `finish` never runs
        drop(import);
        assert_eq!(session.catalog.len(), 2);
        let mut again = start(&session, 2);
        again.index = lightroom_catalog::load_index(Some(&index_path)).unwrap();
        let mut import = LightroomImport::begin(&session, again).unwrap();
        for photos in [vec![master()], vec![copy()]] {
            let b = batch(&session, photos);
            import.commit(&mut session, b).unwrap();
        }
        let completion = import.finish(&mut session, true).unwrap();
        assert_eq!(session.catalog.len(), 2, "the saved copy is matched, not added again: {}", completion.report);
        assert_eq!(completion.report["imported"], 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn oversized_history_archive_keeps_the_import_successful_with_warning() {
        let archive_dir = std::env::temp_dir().join(format!("lightcraft-lr-prepared-scale-{}", std::process::id()));
        let mut data = CatalogImport {
            source: "fixture.lrcat".into(),
            photos: Vec::new(),
            collections: Vec::new(),
            members: Vec::new(),
            collection_content: Vec::new(),
            warnings: Vec::new(),
            skipped: Vec::new(),
        };
        data.photos = (0..5_000)
            .map(|source_id| {
                let mut p = photo(source_id, &format!("/missing/scale-{source_id}.jpg"), &[]);
                p.history = vec![BTreeMap::from([("text".into(), serde_json::json!(format!("history-{source_id}-{}", "x".repeat(8 * 1024))))])];
                p
            })
            .collect();
        let archive_path = stage_archive(Some(&archive_dir), &mut data).unwrap();
        assert!(archive_path.is_none());
        let mut session = crate::Session::new();
        let mut s0 = start(&session, data.photos.len());
        s0.warnings = data.warnings.clone();
        let mut import = LightroomImport::begin(&session, s0).unwrap();
        for chunk in data.photos.chunks(BATCH) {
            let b = batch(&session, chunk.to_vec());
            import.commit(&mut session, b).unwrap();
        }
        let completion = import.finish(&mut session, true).unwrap();
        assert_eq!(completion.report["photos"], 5_000);
        assert!(
            completion.report["warnings"]
                .as_array()
                .is_some_and(|warnings| warnings.iter().any(|warning| warning == "Lightroom source archive exceeded 32 MiB; archive skipped"))
        );
        assert_eq!(session.catalog.len(), 5_000);
        assert_eq!(session.undo.len(), 1);
        std::fs::remove_dir_all(archive_dir).unwrap();
    }

    #[test]
    fn execute_fn_import_does_not_rewrite_source_sidecar() {
        let dir = std::env::temp_dir().join(format!("lightcraft-lr-job-sidecar-{}", std::process::id()));
        let path = dir.join("source.jpg");
        let sidecar = dir.join("source.xmp");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, b"fixture").unwrap();
        std::fs::write(&sidecar, b"original sidecar").unwrap();
        let original = std::fs::read(&sidecar).unwrap();
        let mut session = crate::Session::new();
        session.xmp.auto_write = true;
        let mut import = LightroomImport::begin(&session, start(&session, 1)).unwrap();
        let result = session.execute_fn("library.importLightroom", |s| {
            let b = batch(s, vec![photo(1, &path.to_string_lossy(), &[("rating", serde_json::json!(5))])]);
            import.commit(s, b)?;
            Ok(Value::Null)
        });
        assert!(result.is_ok());
        let result = session.execute_fn("library.importLightroom", |s| Ok(import.finish(s, true)?.report));
        assert!(result.is_ok());
        assert_eq!(std::fs::read(&sidecar).unwrap(), original);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A catalog of real files on disk read end to end: reading counts its tables, then each
    /// original as it is probed; a cancel between batches stops the worker.
    #[test]
    fn job_reads_in_batches_and_counts_each_original() {
        let dir = std::env::temp_dir().join(format!("lightcraft-lr-job-batches-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut session = crate::Session::new();
        let probes = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&probes);
        session.media.header_probe = Some(Arc::new(move |path: &str| {
            counter.fetch_add(1, Ordering::Relaxed);
            Ok(crate::media::ProbeInfo {
                format: "JPEG".into(),
                width: 4,
                height: 3,
                file_size: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
                ..Default::default()
            })
        }));
        let mut job = LightroomJob::new(&mut session, dir.join("none.lrcat"), false).unwrap();
        let photos: Vec<_> = (1..=(BATCH + 2) as i64)
            .map(|i| {
                let path = dir.join(format!("p{i}.jpg"));
                std::fs::write(&path, b"jpeg").unwrap();
                photo(i, &path.to_string_lossy(), &[])
            })
            .collect();
        let n = photos.len();
        job.queue = photos.into();
        job.progress.read_total.store(n, Ordering::Relaxed);
        job.progress.add_total.store(n, Ordering::Relaxed);
        job.progress.photos.store(true, Ordering::Relaxed);
        let cancel = AtomicBool::new(false);
        let first = job.next_batch(&cancel).unwrap().unwrap();
        assert_eq!(first.len(), BATCH);
        assert_eq!(probes.load(Ordering::Relaxed), BATCH, "the header probe describes the originals");
        assert_eq!(job.progress().step(), (LightroomStep::ReadPhotos, BATCH, n), "each original counts as it is read");
        cancel.store(true, Ordering::Relaxed);
        let Some(Err(e)) = job.next_batch(&cancel) else { panic!("a cancel stops before the next batch") };
        assert_eq!(e.kind(), crate::ErrorKind::Cancelled, "{e}");
        cancel.store(false, Ordering::Relaxed);
        let mut import = LightroomImport::begin(&session, LightroomStart { progress: job.progress(), ..start(&session, n) }).unwrap();
        import.commit(&mut session, first).unwrap();
        let second = job.next_batch(&cancel).unwrap().unwrap();
        assert_eq!(second.len(), 2);
        assert_eq!(job.progress().step(), (LightroomStep::AddPhotos, BATCH, n), "every original read: the adding is counted");
        import.commit(&mut session, second).unwrap();
        assert_eq!(job.progress().step(), (LightroomStep::AddPhotos, n, n));
        assert!(job.next_batch(&cancel).is_none());
        let report = import.finish(&mut session, true).unwrap().report;
        assert_eq!((report["photos"].as_u64(), report["imported"].as_u64()), (Some(n as u64), Some(n as u64)));
        assert!(session.catalog.photos().all(|p| (p.width, p.height) == (4, 3) && p.content_hash.is_none()));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
