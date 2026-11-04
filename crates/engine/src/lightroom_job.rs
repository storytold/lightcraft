//! Detached Lightroom catalog preparation and owner-thread commit.
//!
//! `LightroomJob::new` takes only an in-memory session snapshot.  `prepare` owns all catalog
//! reading, probing and archive staging, while `commit_prepared` only applies catalog operations
//! to the session that created the job.  Archive index bytes are returned for a worker to finish
//! after the owner has accepted the catalog operation.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use serde_json::Value;

use crate::import::{ImportJob, ImportMode, ImportOptions, Prepared};
use crate::lightroom_catalog::{self, CatalogImport, ImportIndex};

const COMMAND: &str = "library.importLightroom";
const MAX_INDEX_BYTES: usize = 16 * 1024 * 1024;

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

/// Filesystem-free result of catalog preparation.
pub struct PreparedLightroom {
    pub(crate) data: CatalogImport,
    pub(crate) normal: Prepared,
    pub(crate) index: ImportIndex,
    pub(crate) archive_path: Option<PathBuf>,
    pub(crate) token: LightroomLibraryToken,
    pub(crate) missing_sources: HashSet<String>,
    pub(crate) update_existing: bool,
    pub(crate) now: String,
    cancelled: bool,
}

impl PreparedLightroom {
    pub fn source(&self) -> &str {
        &self.data.source
    }

    pub fn photo_count(&self) -> usize {
        self.data.photos.len()
    }

    pub fn token(&self) -> &LightroomLibraryToken {
        &self.token
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled
    }

    /// Roll back any normal-import filesystem placement held by this prepared value.
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
    /// Readying the photos that were read for the catalog.
    AddPhotos,
}

/// Progress of [`LightroomJob::prepare`] step by step, shared with the thread that runs it.
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

/// A Lightroom import detached from the owner thread.
pub struct LightroomJob {
    path: PathBuf,
    update_existing: bool,
    import: ImportJob,
    token: LightroomLibraryToken,
    archive_dir: Option<PathBuf>,
    progress: LightroomProgress,
}

impl LightroomJob {
    /// Snapshot session state only.  This constructor performs no filesystem work.
    pub fn new(s: &mut crate::Session, path: PathBuf, update_existing: bool) -> crate::Result<Self> {
        let import = ImportJob::new(s, file_import_options())?;
        let archive_dir = s.library.as_ref().filter(|library| library.on_disk).map(|library| library.dir.join("Interop"));
        let progress = LightroomProgress {
            read_total: Arc::clone(&import.read_total),
            read_done: Arc::clone(&import.read_done),
            add_total: Arc::clone(&import.total),
            add_done: Arc::clone(&import.done),
            ..LightroomProgress::default()
        };
        Ok(Self { path, update_existing, import, token: LightroomLibraryToken::capture(s), archive_dir, progress })
    }

    /// The job's progress, to watch from another thread while [`LightroomJob::prepare`] runs.
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

    /// Read/probe/stage all source data off the owner thread.
    pub fn prepare(&mut self, cancel: &AtomicBool) -> crate::Result<PreparedLightroom> {
        if cancel.load(Ordering::Relaxed) {
            return Err(cancelled());
        }
        // (reading stops with a message when `cancel` is set: the flag says it was cancelled)
        let mut data = lightroom_catalog::read_source(
            &self.path,
            self.archive_dir.as_deref(),
            cancel,
            &self.progress.catalog_total,
            &self.progress.catalog_done,
        )
        .map_err(|e| if cancel.load(Ordering::Relaxed) { cancelled() } else { error(e) })?;
        lightroom_catalog::validate_for_job(&data).map_err(error)?;
        if cancel.load(Ordering::Relaxed) {
            return Err(cancelled());
        }
        let (paths, missing_sources) = original_paths(&data, cancel)?;
        // Catalog entries name files, never folders. Missing originals already have enough
        // catalog metadata for placeholders; probing them again only adds failed I/O and errors.
        // Complete the catalog step before the separate read/add photo progress starts.
        self.progress.catalog_done.fetch_add(1, Ordering::Relaxed);
        self.progress.photos.store(true, Ordering::Relaxed);
        self.import.total.fetch_add(paths.len(), Ordering::Relaxed);
        let normal = self.import.prepare_files(paths, cancel);
        if cancel.load(Ordering::Relaxed) {
            normal.rollback();
            return Err(cancelled());
        }
        let archive_path = stage_archive(self.archive_dir.as_deref(), &mut data)?;
        let index_path = self.archive_dir.as_ref().map(|dir| dir.join("lightroom-index.json"));
        let index = lightroom_catalog::load_index(index_path.as_deref())?;
        Ok(PreparedLightroom {
            data,
            normal,
            index,
            archive_path,
            token: self.token.clone(),
            missing_sources,
            update_existing: self.update_existing,
            now: self.import.now().to_string(),
            cancelled: cancel.load(Ordering::Relaxed),
        })
    }
}

/// The options of the job's file import, for both its halves (prepare and commit): a photo
/// deleted here stays deleted (the catalog's record is matched, not restored), and a duplicate by
/// content becomes a record of its own, so its file and sidecar are read (#2589).
fn file_import_options() -> ImportOptions {
    ImportOptions {
        mode: ImportMode::Add,
        on_deleted: crate::import::OnDeleted::Skip,
        same_content: crate::import::SameContent::Create,
        ..ImportOptions::default()
    }
}

/// Apply prepared data on the owner thread.  No source, archive or index filesystem work occurs.
pub fn commit_prepared(s: &mut crate::Session, mut prepared: PreparedLightroom) -> crate::Result<LightroomCompletion> {
    if prepared.cancelled {
        prepared.normal.rollback();
        return Err(cancelled());
    }
    if !prepared.token.matches(s) {
        prepared.normal.rollback();
        return Err(error("Lightroom import belongs to a different open library"));
    }
    // A command-scoped Lightroom import must never rewrite source XMP sidecars.  Direct depth-0
    // callers do not set this transient flag, so they cannot affect a later command.
    if s.depth > 0 {
        s.skip_auto_write = true;
    }

    let undo0 = s.undo.len();
    prepared.normal.revalidate_add(s);
    let now = prepared.now.clone();
    let files = crate::import::commit_prepared(s, &file_import_options(), &now, prepared.normal)?;
    let applied = lightroom_catalog::apply_prepared(
        s,
        prepared.data,
        lightroom_catalog::ApplyPrepared {
            update_existing: prepared.update_existing,
            files,
            index: &mut prepared.index,
            archive_path: prepared.archive_path,
            missing_sources: &prepared.missing_sources,
            undo0,
        },
    )?;
    let path = prepared.token.library_dir().map(|dir| dir.join("Interop/lightroom-index.json"));
    let bytes = if path.is_some() { Some(serde_json::to_vec(&applied.index).map_err(|e| error(e.to_string()))?) } else { None };
    Ok(LightroomCompletion { report: applied.report, finalization: LightroomArchiveFinalization { path, bytes } })
}

fn original_paths(data: &CatalogImport, cancel: &AtomicBool) -> crate::Result<(Vec<String>, HashSet<String>)> {
    let mut paths = Vec::new();
    let mut missing = HashSet::new();
    let mut seen = HashSet::new();
    for photo in &data.photos {
        if cancel.load(Ordering::Relaxed) {
            return Err(cancelled());
        }
        if lightroom_catalog::number_for_job(&photo.image, "masterImage") != 0 {
            continue;
        }
        let key = normalize(&photo.path);
        if !seen.insert(key.clone()) {
            continue;
        }
        if photo.original_is_file() {
            paths.push(photo.path.clone());
        } else {
            missing.insert(key);
        }
    }
    Ok((paths, missing))
}

fn normalize(path: &str) -> String {
    let path = path.replace('\\', "/");
    if cfg!(windows) { path.to_lowercase() } else { path }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_data() -> CatalogImport {
        CatalogImport {
            source: "fixture.lrcat".into(),
            catalog_id: None,
            catalog_path: None,
            photos: Vec::new(),
            collections: Vec::new(),
            members: Vec::new(),
            collection_content: Vec::new(),
            warnings: Vec::new(),
            skipped: Vec::new(),
            images: 0,
            not_supported: Default::default(),
        }
    }

    fn cancelled_prepared(s: &crate::Session) -> PreparedLightroom {
        PreparedLightroom {
            data: empty_data(),
            normal: Prepared::default(),
            index: ImportIndex::default(),
            archive_path: None,
            token: LightroomLibraryToken::capture(s),
            missing_sources: HashSet::new(),
            update_existing: false,
            now: "2026-01-01T00:00:00".into(),
            cancelled: true,
        }
    }

    fn prepared_for(s: &crate::Session, path: &str, rating: i64) -> PreparedLightroom {
        let mut image = std::collections::BTreeMap::new();
        image.insert("id_local".into(), serde_json::json!(1));
        image.insert("rating".into(), serde_json::json!(rating));
        image.insert("pick".into(), serde_json::json!(0));
        image.insert("fileFormat".into(), serde_json::json!("JPG"));
        image.insert("fileWidth".into(), serde_json::json!(1));
        image.insert("fileHeight".into(), serde_json::json!(1));
        PreparedLightroom {
            data: CatalogImport {
                source: "fixture.lrcat".into(),
                catalog_id: None,
                catalog_path: None,
                photos: vec![lightroom_catalog::CatalogPhoto {
                    source_id: 1,
                    uuid: "fixture-photo".into(),
                    path: path.into(),
                    image,
                    settings: String::new(),
                    xmp: String::new(),
                    keywords: Vec::new(),
                    history: Vec::new(),
                    snapshots: Vec::new(),
                    original: lightroom_catalog::Original::Local,
                }],
                collections: Vec::new(),
                members: Vec::new(),
                collection_content: Vec::new(),
                warnings: Vec::new(),
                skipped: Vec::new(),
                images: 1,
                not_supported: Default::default(),
            },
            normal: Prepared::default(),
            index: ImportIndex::default(),
            archive_path: None,
            token: LightroomLibraryToken::capture(s),
            missing_sources: [normalize(path)].into_iter().collect(),
            update_existing: false,
            now: "2026-01-01T00:00:00".into(),
            cancelled: false,
        }
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
    fn cancellation_before_commit_leaves_owner_catalog_unchanged() {
        let mut session = crate::Session::new();
        let before = session.catalog.to_snapshot();
        let prepared = cancelled_prepared(&session);
        let Err(e) = commit_prepared(&mut session, prepared) else { panic!("a cancelled import commits nothing") };
        assert_eq!(e.kind(), crate::ErrorKind::Cancelled, "{e}");
        assert_eq!(session.catalog.to_snapshot(), before);
        assert!(session.undo.is_empty());
    }

    /// Cancelling while the catalog is read is [`crate::ErrorKind::Cancelled`] whatever the message
    /// says, and a failure that is not a cancellation is not one, even when its text says "cancelled".
    #[test]
    fn a_cancelled_read_is_cancelled_by_kind() {
        let mut session = crate::Session::new();
        let mut job = LightroomJob::new(&mut session, PathBuf::from("/nonexistent/cancelled.lrcat"), false).unwrap();
        let Err(e) = job.prepare(&AtomicBool::new(true)) else { panic!("cancelled before it started") };
        assert_eq!(e.kind(), crate::ErrorKind::Cancelled, "{e}");
        // the `error` text clients see is unchanged
        assert_eq!(e.to_string(), "invalid parameters for `library.importLightroom`: Lightroom import cancelled");
        let Err(e) = job.prepare(&AtomicBool::new(false)) else { panic!("no such catalog") };
        assert_ne!(e.kind(), crate::ErrorKind::Cancelled, "{e}");
    }

    #[test]
    fn library_identity_rejects_late_completion_after_reopen() {
        let mut session = crate::Session::new();
        let token = LightroomLibraryToken::capture(&session);
        assert!(token.matches(&session));
        session.library_identity = Arc::new(());
        assert!(!token.matches(&session));
    }

    #[test]
    fn prepared_reimport_preserves_current_edit_and_uses_one_undo_per_import() {
        let mut session = crate::Session::new();
        let path = "/missing/lightroom-job-edit.jpg";
        let prepared = prepared_for(&session, path, 4);
        let first = commit_prepared(&mut session, prepared).unwrap();
        assert_eq!(first.report["mapping"]["1"].as_u64(), Some(1));
        assert_eq!(session.undo.len(), 1);
        let id = lightcraft_catalog::PhotoId(1);
        let mut personal = lightcraft_develop::DevelopSettings::default();
        personal.light.exposure = 1.5;
        session.set_develop(id, personal, "Personal").unwrap();
        let before = session.catalog.photo(id).map(|photo| photo.develop.clone());
        let prepared = prepared_for(&session, path, 5);
        commit_prepared(&mut session, prepared).unwrap();
        assert_eq!(session.catalog.photo(id).map(|photo| photo.develop.clone()), before);
        assert_eq!(session.catalog.photo(id).map(|photo| photo.rating), Some(5));
        assert_eq!(session.undo.len(), 3);
    }

    #[test]
    fn oversized_history_archive_keeps_prepared_import_successful_with_warning() {
        let archive_dir = std::env::temp_dir().join(format!("lightcraft-lr-prepared-scale-{}", std::process::id()));
        let mut data = empty_data();
        data.photos = (0..5_000)
            .map(|source_id| {
                let image = std::collections::BTreeMap::from([
                    ("id_local".into(), serde_json::json!(source_id)),
                    ("fileFormat".into(), serde_json::json!("JPG")),
                    ("fileWidth".into(), serde_json::json!(1)),
                    ("fileHeight".into(), serde_json::json!(1)),
                ]);
                lightroom_catalog::CatalogPhoto {
                    source_id,
                    uuid: format!("scale-{source_id}"),
                    path: format!("/missing/scale-{source_id}.jpg"),
                    image,
                    settings: String::new(),
                    xmp: String::new(),
                    keywords: Vec::new(),
                    history: vec![std::collections::BTreeMap::from([(
                        "text".into(),
                        serde_json::json!(format!("history-{source_id}-{}", "x".repeat(8 * 1024))),
                    )])],
                    snapshots: Vec::new(),
                    original: lightroom_catalog::Original::Local,
                }
            })
            .collect();
        let archive_path = stage_archive(Some(&archive_dir), &mut data).unwrap();
        assert!(archive_path.is_none());
        let mut session = crate::Session::new();
        let token = LightroomLibraryToken::capture(&session);
        let prepared = PreparedLightroom {
            data,
            normal: Prepared::default(),
            index: ImportIndex::default(),
            archive_path,
            token,
            missing_sources: HashSet::new(),
            update_existing: false,
            now: "2026-01-01T00:00:00".into(),
            cancelled: false,
        };
        let completion = commit_prepared(&mut session, prepared).unwrap();
        assert_eq!(completion.report["photos"], 5_000);
        assert!(
            completion.report["warnings"]
                .as_array()
                .is_some_and(|warnings| warnings.iter().any(|warning| warning == "Lightroom source archive exceeded 32 MiB; archive skipped"))
        );
        assert_eq!(session.catalog.len(), 5_000);
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
        let result = session.execute_fn("library.importLightroom", |s| {
            let prepared = prepared_for(s, &path.to_string_lossy(), 5);
            let completion = commit_prepared(s, prepared)?;
            Ok(completion.report)
        });
        assert!(result.is_ok());
        assert_eq!(std::fs::read(&sidecar).unwrap(), original);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A catalog photo whose file this same import adds gets the catalog's develop settings, even
    /// though the file's XMP sidecar gave the new record develop settings of its own: nothing was
    /// in the library before, so there is no edit to keep. A record that was there before the
    /// import, in the library or browsed in Local (which the import promotes), keeps its edit.
    #[test]
    fn the_catalog_develops_a_record_this_import_made_from_a_file_with_a_sidecar() {
        // what the library had at the photo's path before the import: nothing, a photo in the
        // library, a photo browsed in Local
        for before in [None, Some(false), Some(true)] {
            let dir = crate::tests_xmp::temp_dir(&format!("lr-job-made-{before:?}"));
            let file = dir.join("made.jpg");
            std::fs::write(&file, b"made bytes").unwrap();
            std::fs::write(
                dir.join("made.xmp"),
                r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:Exposure2012="+2.00" crs:Vibrance="+30"/>
</rdf:RDF></x:xmpmeta>"#,
            )
            .unwrap();
            let path = file.to_string_lossy().into_owned();
            let mut session = crate::Session::new();
            session.media.file_probe = Some(Arc::new(|_| {
                Ok(crate::media::ProbeInfo {
                    format: "JPEG".into(),
                    width: 20,
                    height: 10,
                    content_hash: Some("made-bytes".into()),
                    ..Default::default()
                })
            }));
            let personal = before.map(|local| {
                let id = session.catalog.alloc_photo_id();
                let mut photo =
                    lightcraft_catalog::Photo::new(id, lightcraft_catalog::Source::File { path: path.clone() }, "made.jpg", "JPEG", 20, 10, "before");
                photo.local = local;
                session.commit("Before", crate::Op::AddPhoto { photo: Box::new(photo) }).unwrap();
                let mut settings = lightcraft_develop::DevelopSettings::default();
                settings.light.exposure = 0.75;
                session.set_develop(id, settings, "Personal edit").unwrap();
                id
            });
            let options = ImportOptions { mode: ImportMode::Add, on_deleted: crate::import::OnDeleted::Skip, ..ImportOptions::default() };
            let mut import = ImportJob::new(&mut session, options).unwrap();
            let mut prepared = prepared_for(&session, &path, 3);
            prepared.normal = import.prepare_files(vec![path.clone()], &AtomicBool::new(false));
            prepared.missing_sources.clear();
            prepared.data.photos[0].settings = "s = { Exposure2012 = -1.0 }".into();
            let report = commit_prepared(&mut session, prepared).unwrap().report;
            let id = lightcraft_catalog::PhotoId(report["mapping"]["1"].as_u64().unwrap());
            let photo = session.catalog.photo(id).unwrap();
            assert_eq!(session.catalog.len(), 1, "{before:?}");
            assert!(!photo.local, "{before:?}");
            assert_eq!(photo.rating, 3, "{before:?}: the catalog's rating applies either way");
            match personal {
                None => {
                    assert_eq!(photo.develop.light.exposure, -1.0, "the catalog's exposure, not the sidecar's +2");
                    assert_eq!(report["preservedExistingEdits"], 0, "nothing existed to preserve: {report}");
                    assert_eq!(report["imported"], 1);
                }
                Some(personal) => {
                    assert_eq!(id, personal, "{before:?}");
                    assert_eq!(photo.develop.light.exposure, 0.75, "{before:?}: the edit made before the import stays");
                    assert_eq!(report["preservedExistingEdits"], 1, "{before:?}: {report}");
                }
            }
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    /// A file beside an XMP sidecar with develop settings (`Exposure2012 +2`, and `Clarity2012 +30`,
    /// which the catalog's settings leave out), and a session whose probe reads it with `hash`.
    fn file_with_develop_sidecar(name: &str, hash: &'static str) -> (PathBuf, String, crate::Session) {
        let dir = crate::tests_xmp::temp_dir(name);
        let file = dir.join("photo.jpg");
        std::fs::write(&file, b"photo bytes").unwrap();
        std::fs::write(
            dir.join("photo.xmp"),
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:Exposure2012="+2.00" crs:Clarity2012="+30"/>
</rdf:RDF></x:xmpmeta>"#,
        )
        .unwrap();
        let mut session = crate::Session::new();
        session.media.file_probe = Some(Arc::new(move |_| {
            Ok(crate::media::ProbeInfo { format: "JPEG".into(), width: 20, height: 10, content_hash: Some(hash.into()), ..Default::default() })
        }));
        (dir, file.to_string_lossy().into_owned(), session)
    }

    /// A library record with an edit (exposure 0.75), browsed in Local or not, with `hash`.
    fn edited_record(session: &mut crate::Session, path: &str, local: bool, hash: Option<&str>) -> lightcraft_catalog::PhotoId {
        let id = session.catalog.alloc_photo_id();
        let mut photo =
            lightcraft_catalog::Photo::new(id, lightcraft_catalog::Source::File { path: path.into() }, "photo.jpg", "JPEG", 20, 10, "before");
        photo.local = local;
        photo.content_hash = hash.map(str::to_string);
        session.commit("Before", crate::Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        let mut settings = lightcraft_develop::DevelopSettings::default();
        settings.light.exposure = 0.75;
        session.set_develop(id, settings, "Personal edit").unwrap();
        id
    }

    /// The job's real route: the Lightroom job's own file import prepares `paths`, then the
    /// catalog (`data`, settings `Exposure2012 = -1.0` on every photo) is committed on top of it.
    fn run_job(session: &mut crate::Session, prepared: PreparedLightroom, paths: Vec<String>, update_existing: bool) -> Value {
        run_job_with(session, prepared, paths, update_existing, "s = { Exposure2012 = -1.0 }")
    }

    /// [`run_job`] with `settings` on every catalog photo.
    fn run_job_with(
        session: &mut crate::Session,
        mut prepared: PreparedLightroom,
        paths: Vec<String>,
        update_existing: bool,
        settings: &str,
    ) -> Value {
        let mut job = LightroomJob::new(session, PathBuf::from("fixture.lrcat"), update_existing).unwrap();
        prepared.normal = job.import.prepare_files(paths, &AtomicBool::new(false));
        prepared.missing_sources.clear();
        prepared.update_existing = update_existing;
        for photo in &mut prepared.data.photos {
            photo.settings = settings.into();
        }
        commit_prepared(session, prepared).unwrap().report
    }

    /// Each origin a record can have in the job decides, on its own, whether its develop settings
    /// are an edit to keep, whether the report counts it as preserved, and where the breakdown puts
    /// it: a record the job made from the file and its sidecar (Created) takes the catalog's
    /// settings and is added; a photo browsed in Local (Promoted) is added and keeps its edit; a
    /// library record (Existing) is already there and keeps its edit. "Update existing" replaces
    /// the edits of the last two only.
    #[test]
    fn each_record_origin_decides_its_develop_and_where_the_report_counts_it() {
        // (record before the job: none, browsed in Local, in the library)
        for (before, update_existing) in [None, Some(true), Some(false)].into_iter().flat_map(|b| [(b, false), (b, true)]) {
            let case = format!("{before:?} update {update_existing}");
            let (dir, path, mut session) = file_with_develop_sidecar(&format!("lr-job-origin-{before:?}-{update_existing}"), "origin-bytes");
            let personal = before.map(|local| edited_record(&mut session, &path, local, None));
            let prepared = prepared_for(&session, &path, 3);
            let report = run_job(&mut session, prepared, vec![path.clone()], update_existing);
            let id = lightcraft_catalog::PhotoId(report["mapping"]["1"].as_u64().unwrap());
            let photo = session.catalog.photo(id).unwrap();
            assert_eq!(session.catalog.len(), 1, "{case}: {report}");
            assert!(!photo.local, "{case}");
            assert_eq!(photo.rating, 3, "{case}");
            if let Some(personal) = personal {
                assert_eq!(id, personal, "{case}");
            }
            let kept = before.is_some() && !update_existing;
            let exposure = if kept { 0.75 } else { -1.0 };
            assert_eq!(photo.develop.light.exposure, exposure, "{case}: never the sidecar's +2");
            assert_eq!(photo.develop.effects.clarity, 0.0, "{case}: a setting the catalog leaves out is not the sidecar's +30");
            assert_eq!(report["preservedExistingEdits"], u64::from(kept), "{case}: {report}");
            let added = before != Some(false);
            assert_eq!(
                (report["added"].as_u64(), report["alreadyInLibrary"].as_u64()),
                (Some(u64::from(added)), Some(u64::from(!added))),
                "{case}: {report}"
            );
            assert_eq!(report["imported"], u64::from(added), "{case}: {report}");
            assert_eq!(report["inCatalog"], 1, "{case}");
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    /// A catalog photo whose file has the same bytes as another library photo gets a record of its
    /// own, which the job makes (Created) from its file and XMP sidecar, the sidecar read by the
    /// job's own file import: the catalog's settings apply on top, the other photo's edit isn't
    /// taken or counted as preserved, and the photo is added.
    #[test]
    fn a_content_duplicate_is_a_record_the_job_made() {
        let (dir, path, mut session) = file_with_develop_sidecar("lr-job-origin-duplicate", "duplicate-bytes");
        let other = edited_record(&mut session, "/elsewhere/other.jpg", false, Some("duplicate-bytes"));
        let prepared = prepared_for(&session, &path, 3);
        let report = run_job(&mut session, prepared, vec![path.clone()], false);
        let id = lightcraft_catalog::PhotoId(report["mapping"]["1"].as_u64().unwrap());
        assert_ne!(id, other, "{report}");
        let new = session.catalog.photo(id).unwrap();
        assert_eq!(new.develop.light.exposure, -1.0, "the catalog's settings, not the sidecar's +2 or the other photo's 0.75: {report}");
        assert_eq!(new.develop.effects.clarity, 0.0, "a setting the catalog leaves out is not the sidecar's +30: {report}");
        assert_eq!(session.catalog.photo(other).unwrap().develop.light.exposure, 0.75, "the other photo is untouched");
        assert_eq!(report["preservedExistingEdits"], 0, "{report}");
        assert_eq!((report["added"].as_u64(), report["alreadyInLibrary"].as_u64()), (Some(1), Some(0)), "{report}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Two catalog photos at the same path whose file is a content duplicate: the second lands on
    /// no library record (the first one's is still being made), so the job makes another one from
    /// the file and its sidecar. That record is the job's own too: the catalog's settings apply and
    /// nothing counts as preserved. (Before, finding the path among the records being made read as
    /// "there before", and the sidecar's develop settings were kept as an existing edit.)
    #[test]
    fn a_second_catalog_photo_at_a_path_the_job_is_adding_takes_the_catalog_develop() {
        let (dir, path, mut session) = file_with_develop_sidecar("lr-job-origin-same-path", "same-path-bytes");
        edited_record(&mut session, "/elsewhere/other.jpg", false, Some("same-path-bytes"));
        let mut prepared = prepared_for(&session, &path, 3);
        let mut second = prepared.data.photos[0].clone();
        second.source_id = 2;
        second.uuid = "fixture-photo-2".into();
        second.image.insert("id_local".into(), serde_json::json!(2));
        prepared.data.photos.push(second);
        prepared.data.images = 2;
        let report = run_job(&mut session, prepared, vec![path.clone()], false);
        for source in ["1", "2"] {
            let id = lightcraft_catalog::PhotoId(report["mapping"][source].as_u64().unwrap());
            let photo = session.catalog.photo(id).unwrap();
            assert_eq!(photo.develop.light.exposure, -1.0, "catalog photo {source}: {report}");
            assert_eq!(photo.develop.effects.clarity, 0.0, "catalog photo {source}: not the sidecar's +30: {report}");
        }
        assert_eq!(report["preservedExistingEdits"], 0, "{report}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// `prepared` with a second catalog photo (source id 2) at `path`.
    fn with_second_photo(mut prepared: PreparedLightroom, path: &str) -> PreparedLightroom {
        let mut second = prepared.data.photos[0].clone();
        second.source_id = 2;
        second.uuid = "fixture-photo-2".into();
        second.path = path.into();
        second.image.insert("id_local".into(), serde_json::json!(2));
        prepared.data.photos.push(second);
        prepared.data.images = 2;
        prepared
    }

    /// Two catalog photos at one path land on one record. The report counts that record once
    /// (`added` for one the job made or promoted, the other photo `alreadyInLibrary`), and
    /// `imported` counts it once. `preservedExistingEdits` counts catalog photos, not records: both
    /// photos keep the record's edit when it was there before the job.
    #[test]
    fn two_catalog_photos_at_one_path_count_their_record_once() {
        // (record before the job: none, browsed in Local, in the library)
        for before in [None, Some(true), Some(false)] {
            let (dir, path, mut session) = file_with_develop_sidecar(&format!("lr-job-origin-twice-{before:?}"), "twice-bytes");
            if let Some(local) = before {
                edited_record(&mut session, &path, local, None);
            }
            let prepared = with_second_photo(prepared_for(&session, &path, 3), &path);
            let report = run_job(&mut session, prepared, vec![path.clone()], false);
            assert_eq!(session.catalog.len(), 1, "{before:?}: {report}");
            assert_eq!(report["mapping"]["1"], report["mapping"]["2"], "{before:?}: one record");
            let id = lightcraft_catalog::PhotoId(report["mapping"]["1"].as_u64().unwrap());
            let photo = session.catalog.photo(id).unwrap();
            assert!(!photo.local, "{before:?}");
            let (exposure, preserved, added, already, imported) = match before {
                None => (-1.0, 0, 1, 1, 1),
                Some(true) => (0.75, 2, 1, 1, 1),
                Some(false) => (0.75, 2, 0, 2, 0),
            };
            assert_eq!(photo.develop.light.exposure, exposure, "{before:?}: {report}");
            assert_eq!(report["preservedExistingEdits"], preserved, "{before:?}: {report}");
            assert_eq!((report["added"].as_u64(), report["alreadyInLibrary"].as_u64()), (Some(added), Some(already)), "{before:?}: {report}");
            assert_eq!(report["imported"], imported, "{before:?}: {report}");
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    /// A catalog photo whose original is offline never reaches the job's file import, so the
    /// catalog step records the origin of the library record it finds there: a photo browsed in
    /// Local (Promoted) joins the library and is added, a library record (Existing) is already
    /// there, and both keep their edit unless "update existing" is on.
    #[test]
    fn an_offline_original_s_record_keeps_its_origin() {
        let path = "/missing/lr-job-offline.jpg";
        for (local, update_existing) in [(true, false), (true, true), (false, false), (false, true)] {
            let case = format!("local {local} update {update_existing}");
            let mut session = crate::Session::new();
            let personal = edited_record(&mut session, path, local, None);
            let mut prepared = prepared_for(&session, path, 3);
            prepared.update_existing = update_existing;
            prepared.data.photos[0].settings = "s = { Exposure2012 = -1.0 }".into();
            let report = commit_prepared(&mut session, prepared).unwrap().report;
            assert_eq!(report["mapping"]["1"].as_u64(), Some(personal.0), "{case}: {report}");
            let photo = session.catalog.photo(personal).unwrap();
            assert!(!photo.local, "{case}: the import brings it into the library");
            assert_eq!(photo.develop.light.exposure, if update_existing { -1.0 } else { 0.75 }, "{case}: {report}");
            assert_eq!(report["preservedExistingEdits"], u64::from(!update_existing), "{case}: {report}");
            assert_eq!(
                (report["added"].as_u64(), report["alreadyInLibrary"].as_u64()),
                (Some(u64::from(local)), Some(u64::from(!local))),
                "{case}: {report}"
            );
            assert_eq!(report["imported"], u64::from(local), "{case}: {report}");
        }
    }

    /// Two new files with the same bytes, each beside its own XMP sidecar, and each with a capture
    /// time of its own: the job's file import adds the first and reads the second's sidecar too,
    /// since the job makes a record of it (`SameContent::Create`). Each record has its own caption.
    #[test]
    fn two_new_files_with_the_same_bytes_each_keep_their_own_sidecar() {
        let dir = crate::tests_xmp::temp_dir("lr-job-origin-same-bytes");
        let caption = |text: &str| {
            format!(
                r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/">
<dc:description><rdf:Alt><rdf:li xml:lang="x-default">{text}</rdf:li></rdf:Alt></dc:description>
</rdf:Description></rdf:RDF></x:xmpmeta>"#
            )
        };
        let paths: Vec<String> = ["a", "b"]
            .iter()
            .map(|name| {
                let file = dir.join(format!("{name}.jpg"));
                std::fs::write(&file, b"same bytes").unwrap();
                std::fs::write(dir.join(format!("{name}.xmp")), caption(&format!("Caption {name}"))).unwrap();
                file.to_string_lossy().into_owned()
            })
            .collect();
        let mut session = crate::Session::new();
        session.media.file_probe = Some(Arc::new(|_| {
            Ok(crate::media::ProbeInfo {
                format: "JPEG".into(),
                width: 20,
                height: 10,
                content_hash: Some("same-bytes".into()),
                captured: Some("2024-05-06T07:08:09".into()),
                ..Default::default()
            })
        }));
        let prepared = with_second_photo(prepared_for(&session, &paths[0], 3), &paths[1]);
        let report = run_job(&mut session, prepared, paths.clone(), false);
        assert_eq!(session.catalog.len(), 2, "{report}");
        for (source, name) in [("1", "a"), ("2", "b")] {
            let id = lightcraft_catalog::PhotoId(report["mapping"][source].as_u64().unwrap());
            let photo = session.catalog.photo(id).unwrap();
            assert_eq!(photo.meta.caption, format!("Caption {name}"), "catalog photo {source}: {report}");
            assert_eq!(photo.develop.light.exposure, -1.0, "catalog photo {source}: {report}");
        }
        assert_eq!(
            (report["added"].as_u64(), report["imported"].as_u64(), report["preservedExistingEdits"].as_u64()),
            (Some(2), Some(2), Some(0)),
            "{report}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Settings a file's XMP gives the record the job makes of it (embedded in a DNG, or a sidecar)
    /// are not edits the library had: the catalog's settings win without "update existing", and
    /// nothing counts as preserved. They start from the camera's defaults, so a setting the catalog
    /// leaves out (Lightroom omits those at zero) is taken neither from the file nor from a default
    /// import preset. (Adapted from #3435.)
    #[test]
    fn a_photo_the_job_adds_takes_the_catalogs_settings_over_its_files_xmp() {
        let dir = crate::tests_xmp::temp_dir("lr-job-embedded-edits");
        let saved = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:Exposure2012="+1.00" crs:Dehaze="0" crs:Clarity2012="+20"/></rdf:RDF></x:xmpmeta>"#;
        let embedded = dir.join("embedded.dng");
        std::fs::write(&embedded, crate::tests_xmp::synthetic_dng_with(Some(saved), Default::default())).unwrap();
        let beside = dir.join("sidecar.dng");
        std::fs::write(&beside, crate::tests_xmp::synthetic_dng_with(None, Default::default())).unwrap();
        std::fs::write(dir.join("sidecar.xmp"), saved).unwrap();
        let paths: Vec<String> = [&embedded, &beside].iter().map(|p| p.to_string_lossy().into_owned()).collect();
        let mut session = crate::Session::new().with_fs();
        // a default import preset sets a value the catalog leaves out too
        session.presets.push(lightcraft_develop::Preset {
            id: "look".into(),
            name: "Look".into(),
            group: "g".into(),
            settings: serde_json::json!({"effects": {"texture": 12.0}}),
            favorite: false,
            builtin: false,
        });
        session.import_defaults.raw_preset = Some("look".into());
        let mut prepared = with_second_photo(prepared_for(&session, &paths[0], 0), &paths[1]);
        for photo in &mut prepared.data.photos {
            for (key, value) in
                [("fileFormat", serde_json::json!("DNG")), ("fileWidth", serde_json::json!(32)), ("fileHeight", serde_json::json!(24))]
            {
                photo.image.insert(key.into(), value);
            }
        }
        let report = run_job_with(&mut session, prepared, paths, false, "s = { Exposure2012 = 1, Dehaze = -50 }");
        assert_eq!(report["imported"], 2, "{report}");
        assert_eq!(report["preservedExistingEdits"], 0, "the library held nothing before the import: {report}");
        assert_eq!(session.catalog.len(), 2, "{report}");
        for p in session.catalog.photos() {
            assert_eq!(p.develop.light.exposure, 1.0, "{}: the file's saved exposure and the catalog's agree", p.file_name);
            assert_eq!(p.develop.effects.dehaze, -50.0, "{}: the catalog's later edit is kept, not the file's", p.file_name);
            assert_eq!(p.develop.effects.clarity, 0.0, "{}: a setting the catalog leaves out is not taken from the file", p.file_name);
            assert_eq!(p.develop.effects.texture, 0.0, "{}: nor from a default import preset", p.file_name);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A new virtual copy is a record the job makes too: its catalog settings start from the
    /// camera's defaults, not from its master's, so a setting the copy's develop text leaves out
    /// (Lightroom omits those at zero) is zero, while the master keeps its own.
    #[test]
    fn a_new_virtual_copy_takes_none_of_its_masters_settings_the_catalog_leaves_out() {
        let mut session = crate::Session::new();
        let mut prepared = prepared_for(&session, "/missing/lr-job-copy.jpg", 3);
        prepared.data.photos[0].settings = "s = { Exposure2012 = -1.0, Clarity2012 = 30 }".into();
        let mut copy = prepared.data.photos[0].clone();
        copy.source_id = 2;
        copy.uuid = "fixture-copy".into();
        copy.image.insert("id_local".into(), serde_json::json!(2));
        copy.image.insert("masterImage".into(), serde_json::json!(1));
        copy.image.insert("copyName".into(), serde_json::json!("Copy 1"));
        copy.settings = "s = { Exposure2012 = 0.5 }".into();
        prepared.data.photos.push(copy);
        prepared.data.images = 2;
        let report = commit_prepared(&mut session, prepared).unwrap().report;
        let develop =
            |source: &str| session.catalog.photo(lightcraft_catalog::PhotoId(report["mapping"][source].as_u64().unwrap())).unwrap().develop.clone();
        let (master, copy) = (develop("1"), develop("2"));
        assert_eq!((master.light.exposure, master.effects.clarity), (-1.0, 30.0), "{report}");
        assert_eq!((copy.light.exposure, copy.effects.clarity), (0.5, 0.0), "the copy's own settings, not its master's clarity: {report}");
        assert_eq!(report["preservedExistingEdits"], 0, "{report}");
    }
}
