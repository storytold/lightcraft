use super::files::{ReadJob, expand, read_files};
use super::types::ImportCandidate;
use super::{HashMap, Path, PathBuf, PhotoId, ProbeInfo, ReadWatch, Session, Source, StuckReaders};

/// What a [`scan`] needs from the session, so it can run on another thread (a folder on a network
/// share can take minutes to read).
pub struct ScanInput {
    probe: Option<crate::media::FileProbe>,
    skip: Option<PathBuf>,
    /// The library as it is now (photos are shared with the session, not copied): looked up for
    /// the files the scan finds ([`library_files`]) and the contents it probes
    /// ([`library_hashes`]), on the scan's thread.
    library: std::sync::Arc<lightcraft_catalog::Catalog>,
    cache: HashMap<String, ProbeInfo>,
}

/// The library's photos of `files`: path → (photo, its summary as a candidate that is there
/// already). One pass over the library, and a record only for a file that was found: a folder of
/// a hundred files in a library of millions costs a hundred records.
pub(super) fn library_files(library: &lightcraft_catalog::Catalog, files: &[String]) -> HashMap<PathBuf, (PhotoId, ImportCandidate)> {
    let found: std::collections::HashSet<&Path> = files.iter().map(Path::new).collect();
    let mut by_path = HashMap::new();
    if found.is_empty() {
        return by_path;
    }
    for p in library.photos() {
        if let Source::File { path } = &p.source
            && found.contains(Path::new(path))
        {
            let c = ImportCandidate {
                path: path.clone(),
                format: p.format.clone(),
                kind: p.kind,
                width: p.width,
                height: p.height,
                file_size: p.file_size,
                captured: p.captured.clone(),
                duplicate: Some("path".into()),
                existing: Some(p.id.0),
                ..Default::default()
            };
            by_path.insert(PathBuf::from(path), (p.id, c));
        }
    }
    by_path
}

/// Which photo of the library has each of the contents `hashes` (the later one where several
/// have it). One pass over the library, and an entry only for a content that was asked about.
pub(super) fn library_hashes<'a>(library: &lightcraft_catalog::Catalog, hashes: impl Iterator<Item = &'a str>) -> HashMap<String, PhotoId> {
    let wanted: std::collections::HashSet<&str> = hashes.collect();
    let mut by_hash = HashMap::new();
    if wanted.is_empty() {
        return by_hash;
    }
    for p in library.photos() {
        if let Some(h) = &p.content_hash
            && wanted.contains(h.as_str())
        {
            by_hash.insert(h.clone(), p.id);
        }
    }
    by_hash
}

/// Progress and cancellation of a running [`scan_with`].
#[derive(Default)]
pub struct ScanProgress {
    /// Files to probe, set once the folders are expanded.
    pub total: std::sync::atomic::AtomicUsize,
    pub done: std::sync::atomic::AtomicUsize,
    /// Shared with the scan's row in the activity stack (its ✕ sets it).
    pub cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Which file the probes wait for, and Skip for it (the scan's row shows it; see [`ReadWatch`]).
    pub reads: std::sync::Arc<ReadWatch>,
}

impl ScanProgress {
    /// Progress of a scan whose probes count as stalled after `stall_after` ([`Session::import_stall_after`]) and
    /// whose abandoned reads are counted in `stuck` ([`Session::stuck_readers`]).
    pub fn new(stall_after: std::time::Duration, stuck: std::sync::Arc<StuckReaders>) -> Self {
        ScanProgress { reads: std::sync::Arc::new(ReadWatch::with_stuck(stall_after, stuck)), ..Default::default() }
    }
}

/// A probe a review scan kept still holds for the scan that follows while the file is as big as it was (or can't be
/// looked at: it is then probed again, or fails there).
fn cache_holds_for_scan(path: &str, info: &ProbeInfo) -> bool {
    let size = std::fs::metadata(path).map(|m| m.len()).ok();
    size.is_none() || size == Some(info.file_size)
}

/// A probe a scan kept still holds for the import while the file is still there, as it was (the web's files are not
/// `std::fs` files: kept as probed).
pub(super) fn cache_holds_for_import(path: &str, info: &ProbeInfo) -> bool {
    cfg!(target_arch = "wasm32") || std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() == info.file_size)
}

/// The result of [`scan_with`]: the candidates, and the probes to keep for the import that follows.
pub struct ScanOutput {
    pub candidates: Vec<ImportCandidate>,
    pub probes: HashMap<String, ProbeInfo>,
    /// Why the scan ended early for a reason of its own, if it did: the drive stopped answering (the fourth read was
    /// skipped). The candidates not reached say so as their error.
    pub stopped: Option<String>,
}

impl ScanInput {
    pub fn new(s: &mut Session, paths: &[String]) -> (Self, Vec<String>) {
        let skip = s.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.clone());
        // (no table of the library here: this is the session's thread, and the library may hold
        // millions of photos. The scan looks up what it finds)
        let library = std::sync::Arc::new(s.catalog.clone());
        let input = ScanInput { probe: s.media.file_probe.clone(), skip, library, cache: std::mem::take(&mut s.import_probes) };
        (input, paths.to_vec())
    }

    /// The folder a scan never enters (the library's own).
    pub(crate) fn skip(&self) -> Option<&Path> {
        self.skip.as_deref()
    }

    /// The library as the scan sees it (shared, for [`crate::sync`], which looks at the same one).
    pub(crate) fn library(&self) -> std::sync::Arc<lightcraft_catalog::Catalog> {
        std::sync::Arc::clone(&self.library)
    }
}

/// Find and probe what importing `paths` would add, marking duplicates (by path, or by content
/// against the library and earlier candidates). Nothing is added; the probes are kept for the
/// import that follows.
pub fn scan(s: &mut Session, paths: &[String]) -> Vec<ImportCandidate> {
    let progress = ScanProgress::new(s.import_stall_after, s.stuck_readers.clone());
    let (input, paths) = ScanInput::new(s, paths);
    let out = scan_with(input, &paths, &progress);
    s.import_probes = out.probes;
    out.candidates
}

/// [`scan`] without the session, so it can run on a worker thread. Stops early (returning what it
/// has) when `progress.cancel` is set.
pub fn scan_with(input: ScanInput, paths: &[String], progress: &ScanProgress) -> ScanOutput {
    let files = expand(paths, input.skip.as_deref());
    scan_files(input, files, progress)
}

/// [`scan_with`] for files that were found already (`paths` expanded, [`expand`]).
pub(crate) fn scan_files(mut input: ScanInput, files: Vec<String>, progress: &ScanProgress) -> ScanOutput {
    use std::sync::atomic::Ordering::Relaxed;
    let by_path = library_files(&input.library, &files);
    let todo: Vec<String> = files.iter().filter(|f| !by_path.contains_key(Path::new(f))).cloned().collect();
    progress.total.store(todo.len(), Relaxed);
    // probes from a preceding `scan` are reused when the file is unchanged (same size): checked by the readers
    let jobs: Vec<ReadJob> = todo.iter().map(|f| ReadJob { path: f.clone(), cached: input.cache.remove(f) }).collect();
    let probed: Vec<Result<ProbeInfo, String>> =
        read_files(input.probe.as_ref(), jobs, cache_holds_for_scan, None, &progress.done, &progress.cancel, &progress.reads)
            .into_iter()
            .map(|r| r.info)
            .collect();
    crate::memory::release();
    let mut probes: HashMap<String, Result<ProbeInfo, String>> = todo.into_iter().zip(probed).collect();
    // (who in the library has a content that was probed: asked once the probes are in)
    let probed_hashes = probes.values().filter_map(|r| r.as_ref().ok()).filter_map(|info| info.content_hash.as_deref());
    let mut seen_hash: HashMap<String, Option<u64>> =
        library_hashes(&input.library, probed_hashes).into_iter().map(|(h, id)| (h, Some(id.0))).collect();
    let mut out = Vec::with_capacity(files.len());
    let mut kept = HashMap::new();
    for f in files {
        if let Some((_, c)) = by_path.get(Path::new(&f)) {
            let mut c = c.clone();
            c.name = Path::new(&f).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| f.clone());
            out.push(c);
            continue;
        }
        let name = Path::new(&f).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| f.clone());
        let mut c = ImportCandidate { path: f.clone(), name, ..Default::default() };
        match probes.remove(&f) {
            Some(Ok(info)) => {
                (c.format, c.kind, c.width, c.height, c.file_size, c.captured) =
                    (info.format.clone(), info.kind, info.width, info.height, info.file_size, info.captured.clone());
                // as the import will date it (a sidecar's date, when there is one, still wins there)
                c.captured = c.captured.or_else(|| info.file_modified.clone());
                c.preview_only = info.preview_only.clone();
                if let Some(h) = &info.content_hash {
                    match seen_hash.get(h) {
                        Some(existing) => {
                            c.duplicate = Some("content".into());
                            c.existing = *existing;
                        }
                        None => {
                            seen_hash.insert(h.clone(), None);
                        }
                    }
                }
                kept.insert(f, info);
            }
            Some(Err(e)) => c.error = Some(e),
            None => c.error = Some("not probed".into()),
        }
        out.push(c);
    }
    ScanOutput { candidates: out, probes: kept, stopped: progress.reads.ended() }
}
