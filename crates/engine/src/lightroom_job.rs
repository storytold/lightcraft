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

/// A Lightroom import detached from the owner thread.
pub struct LightroomJob {
    path: PathBuf,
    update_existing: bool,
    import: ImportJob,
    token: LightroomLibraryToken,
    archive_dir: Option<PathBuf>,
}

impl LightroomJob {
    /// Snapshot session state only.  This constructor performs no filesystem work.
    pub fn new(s: &mut crate::Session, path: PathBuf, update_existing: bool) -> crate::Result<Self> {
        let opts = ImportOptions { mode: ImportMode::Add, ..ImportOptions::default() };
        let import = ImportJob::new(s, opts)?;
        let archive_dir = s.library.as_ref().filter(|library| library.on_disk).map(|library| library.dir.join("Interop"));
        Ok(Self { path, update_existing, import, token: LightroomLibraryToken::capture(s), archive_dir })
    }

    pub fn total(&self) -> usize {
        self.import.total.load(Ordering::Relaxed)
    }

    pub fn done(&self) -> usize {
        self.import.done.load(Ordering::Relaxed)
    }

    pub fn progress(&self) -> (usize, usize) {
        (self.done(), self.total())
    }

    pub fn token(&self) -> &LightroomLibraryToken {
        &self.token
    }

    pub fn total_atomic(&self) -> std::sync::Arc<AtomicUsize> {
        std::sync::Arc::clone(&self.import.total)
    }

    pub fn done_atomic(&self) -> std::sync::Arc<AtomicUsize> {
        std::sync::Arc::clone(&self.import.done)
    }

    /// Finish an owner commit's deferred archive-index write on a worker.
    pub fn finish_archive(finalization: LightroomArchiveFinalization) -> crate::Result<()> {
        finalization.finish()
    }

    /// Read/probe/stage all source data off the owner thread.
    pub fn prepare(&mut self, cancel: &AtomicBool) -> crate::Result<PreparedLightroom> {
        if cancel.load(Ordering::Relaxed) {
            return Err(error("Lightroom import cancelled"));
        }
        self.import.total.store(1, Ordering::Relaxed);
        self.import.done.store(0, Ordering::Relaxed);
        let mut data = lightroom_catalog::read_with_progress(&self.path, cancel, &self.import.total, &self.import.done).map_err(error)?;
        lightroom_catalog::validate_for_job(&data).map_err(error)?;
        if cancel.load(Ordering::Relaxed) {
            return Err(error("Lightroom import cancelled"));
        }
        self.import.done.fetch_add(1, Ordering::Relaxed);
        let paths: Vec<String> = data
            .photos
            .iter()
            .filter(|photo| lightroom_catalog::number_for_job(&photo.image, "masterImage") == 0)
            .map(|photo| photo.path.clone())
            .collect();
        let missing_sources = data
            .photos
            .iter()
            .filter(|photo| lightroom_catalog::number_for_job(&photo.image, "masterImage") == 0)
            .filter(|photo| !Path::new(&photo.path).is_file())
            .map(|photo| normalize(&photo.path))
            .collect();
        let normal = self.import.prepare(&paths, cancel);
        if cancel.load(Ordering::Relaxed) {
            normal.rollback();
            return Err(error("Lightroom import cancelled"));
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

/// Apply prepared data on the owner thread.  No source, archive or index filesystem work occurs.
pub fn commit_prepared(s: &mut crate::Session, mut prepared: PreparedLightroom) -> crate::Result<LightroomCompletion> {
    if prepared.cancelled {
        prepared.normal.rollback();
        return Err(error("Lightroom import cancelled"));
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
    let options = ImportOptions { mode: ImportMode::Add, ..ImportOptions::default() };
    let now = prepared.now.clone();
    let files = crate::import::commit_prepared(s, &options, &now, prepared.normal)?;
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
            photos: Vec::new(),
            collections: Vec::new(),
            members: Vec::new(),
            collection_content: Vec::new(),
            warnings: Vec::new(),
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
                }],
                collections: Vec::new(),
                members: Vec::new(),
                collection_content: Vec::new(),
                warnings: Vec::new(),
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

    #[test]
    fn cancellation_before_commit_leaves_owner_catalog_unchanged() {
        let mut session = crate::Session::new();
        let before = session.catalog.to_snapshot();
        let prepared = cancelled_prepared(&session);
        assert!(commit_prepared(&mut session, prepared).is_err());
        assert_eq!(session.catalog.to_snapshot(), before);
        assert!(session.undo.is_empty());
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
}
