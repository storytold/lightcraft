use super::files::{expand_knowing, probe_all};
use super::types::ImportCandidate;
use super::{HashMap, ListedFolders, Path, PathBuf, PhotoId, ProbeInfo, Session, Source};

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
    /// The folders the scan listed, for the import that follows.
    listed: ListedFolders,
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
}

/// The result of [`scan_with`]: the candidates, and the probes to keep for the import that follows.
pub struct ScanOutput {
    pub candidates: Vec<ImportCandidate>,
    pub probes: HashMap<String, ProbeInfo>,
    /// The folders the scan listed (kept with the probes, for the import that follows).
    pub listed: ListedFolders,
}

impl ScanInput {
    pub fn new(s: &mut Session, paths: &[String]) -> (Self, Vec<String>) {
        let skip = s.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.clone());
        // (no table of the library here: this is the session's thread, and the library may hold
        // millions of photos. The scan looks up what it finds)
        let library = std::sync::Arc::new(s.catalog.clone());
        let input = ScanInput {
            probe: s.media.file_probe.clone(),
            skip,
            library,
            cache: std::mem::take(&mut s.import_probes),
            listed: ListedFolders::default(),
        };
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
    let (input, paths) = ScanInput::new(s, paths);
    let out = scan_with(input, &paths, &ScanProgress::default());
    s.import_probes = out.probes;
    out.candidates
}

/// [`scan`] without the session, so it can run on a worker thread. Stops early (returning what it
/// has) when `progress.cancel` is set.
pub fn scan_with(mut input: ScanInput, paths: &[String], progress: &ScanProgress) -> ScanOutput {
    let listed = &mut input.listed;
    let (files, _) = expand_knowing(
        paths,
        input.skip.as_deref(),
        crate::walk::Limits::default(),
        // (always `true`: nothing here stops the walk. `progress.cancel` is read only once it is over, in
        // `scan_files`, which stops the probing and returns what it has)
        Some(&mut |dir, names, complete, modified| {
            listed.note(dir, names, complete, modified);
            true
        }),
        true,
        &|_| false,
    );
    scan_files(input, files, progress)
}

/// [`scan_with`] for files that were found already (`paths` expanded, [`expand`]).
pub(crate) fn scan_files(mut input: ScanInput, files: Vec<String>, progress: &ScanProgress) -> ScanOutput {
    use std::sync::atomic::Ordering::Relaxed;
    let by_path = library_files(&input.library, &files);
    let todo: Vec<String> = files.iter().filter(|f| !by_path.contains_key(Path::new(f))).cloned().collect();
    progress.total.store(todo.len(), Relaxed);
    // probes from a preceding `scan` are reused when the file is unchanged (same size)
    let cached: Vec<Option<ProbeInfo>> = todo
        .iter()
        .map(|f| {
            let info = input.cache.remove(f)?;
            // (✕ set while the files' cached probes are checked: no more round trips to the disk; what is
            // not reached counts as not probed, like the files after it)
            if progress.cancel.load(Relaxed) {
                return None;
            }
            let size = std::fs::metadata(f).map(|m| m.len()).ok();
            (size.is_none() || size == Some(info.file_size)).then_some(info)
        })
        .collect();
    let missing: Vec<String> = todo.iter().zip(&cached).filter(|(_, c)| c.is_none()).map(|(f, _)| f.clone()).collect();
    progress.done.store(todo.len() - missing.len(), Relaxed);
    let mut fresh = probe_all(input.probe.as_ref(), &missing, &progress.done, &progress.cancel).into_iter();
    let probed: Vec<Result<ProbeInfo, String>> =
        cached.into_iter().map(|c| c.map(Ok).unwrap_or_else(|| fresh.next().unwrap_or_else(|| Err("not probed".into())))).collect();
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
    ScanOutput { candidates: out, probes: kept, listed: input.listed }
}
