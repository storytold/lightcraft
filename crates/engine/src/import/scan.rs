use super::files::{Seen, expand, is_supported, probe_all, reusable_probes};
use super::types::ImportCandidate;
use super::{HashMap, Path, PathBuf, PhotoId, ProbeInfo, Session, Source};

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
}

/// The result of [`scan_with`]: the candidates, and the probes to keep for the import that follows.
pub struct ScanOutput {
    pub candidates: Vec<ImportCandidate>,
    pub probes: HashMap<String, ProbeInfo>,
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

    /// The probes of earlier scans this input took from the session ([`ScanInput::new`]), to hand back with a
    /// scan's output that probes nothing itself (browsing), so a later import doesn't read those files again.
    pub fn take_probes(&mut self) -> HashMap<String, ProbeInfo> {
        std::mem::take(&mut self.cache)
    }
}

/// What browsing a folder (Local) has to read: the files of `files` the library has no photo at,
/// in the order the view will show them (see [`browse_order`]). Unlike [`scan_with`] nothing is
/// read, so the import that follows adds the photos batch by batch as it reads them, and the first
/// screen of a 3,000-raw wedding shows within a second instead of after every file was read
/// (measured on 2,933 EOS R5 C CR3s: 30.6 s to the first photo before).
pub fn browse_queue(input: &ScanInput, files: Vec<String>, sort: &lightcraft_catalog::Sort) -> Vec<String> {
    browse_queue_until(input, files, sort, &std::sync::atomic::AtomicBool::new(false)).unwrap_or_default()
}

/// [`browse_queue`] that stops reading the files' times or sizes once `cancel` is set (another folder was
/// chosen): `None` then.
pub fn browse_queue_until(
    input: &ScanInput,
    files: Vec<String>,
    sort: &lightcraft_catalog::Sort,
    cancel: &std::sync::atomic::AtomicBool,
) -> Option<Vec<String>> {
    let known = library_files(&input.library, &files);
    let mut files: Vec<String> = files.into_iter().filter(|f| !known.contains_key(Path::new(f))).collect();
    browse_order_until(&mut files, sort, cancel).then_some(files)
}

/// The files browsing `root` (Local) lists: the supported, non-hidden files in it, or below it
/// with `subfolders`, never inside the library's own folder (as [`scan_with`] skips it).
///
/// A folder that can't be listed whole (gone, no access, a network share dropping out) is an
/// error, not an empty or partial listing: the browse bookkeeping would take the files missing
/// from it for files gone. (With `subfolders` the walk skips the folders below it that it can't
/// read, as the import's scan does.)
pub fn browse_files(input: &ScanInput, root: &str, subfolders: bool) -> std::io::Result<Vec<String>> {
    // one spelling for the folder, and so for its files: `lib/../outside` lists `outside`'s files as
    // the catalog stores them, not as new photos under another name
    let root_path = browse_path(Path::new(root))?;
    let root = root_path.to_string_lossy().to_string();
    // the library's folder itself, or a folder inside it: nothing to browse
    if input.skip.as_deref().is_some_and(|lib| inside(&root_path, lib)) {
        return Ok(Vec::new());
    }
    if subfolders {
        // (the root itself must be there to be walked)
        std::fs::read_dir(&root)?;
        return Ok(expand(&[root], input.skip.as_deref()));
    }
    let mut v = Vec::new();
    for entry in std::fs::read_dir(&root)? {
        let entry = entry?;
        if crate::walk::is_hidden_entry(&entry) {
            continue;
        }
        let f = entry.path();
        if f.is_file() && is_supported(&f) {
            v.push(f.to_string_lossy().to_string());
        }
    }
    v.sort();
    Ok(v)
}

/// The folder `path` names, as one spelling for the view, its listing and the catalog's records:
/// absolute (`.` is the current folder), with `.` and `..` taken out the way the system reads them.
/// Windows takes `..` out by name; elsewhere `link/..` is the folder above where the link points,
/// so a link before a `..` is resolved first. Everything else keeps its spelling (a mapped drive
/// stays a drive letter, a link not followed by `..` stays the link), as the records were made.
///
/// It may read the disk (a link on a stalled network mount can block it): call it off the UI thread.
/// [`browse_spelling`] is the UI's spelling meanwhile.
pub fn browse_path(path: &Path) -> std::io::Result<PathBuf> {
    normalized(path, true)
}

/// [`browse_path`] without reading the disk: `.` and `..` taken out by name. What the view shows
/// until the worker has resolved the folder; the same folder unless a link comes before a `..`
/// (not on Windows, which takes `..` out by name too).
pub fn browse_spelling(path: &Path) -> std::io::Result<PathBuf> {
    normalized(path, false)
}

fn normalized(path: &Path, resolve_links: bool) -> std::io::Result<PathBuf> {
    use std::path::Component;
    // (no disk access: the current folder is the process's)
    let path = std::path::absolute(path)?;
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                // a link before `..` is left through where it points; a broken one is an error, not
                // its parent folder by name (CodeRabbit on #3577)
                #[cfg(not(windows))]
                if resolve_links && std::fs::symlink_metadata(&out).is_ok_and(|m| m.file_type().is_symlink()) {
                    out = std::fs::canonicalize(&out)?;
                }
                #[cfg(windows)]
                let _ = resolve_links;
                // (above the root: still the root)
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                }
            }
            c => out.push(c.as_os_str()),
        }
    }
    Ok(out)
}

/// Whether `root` (a [`browse_path`]) is `lib` or a folder inside it: as written, or once both are
/// resolved (a symlink to the library names the same folder), as the recursive walk tells. A root
/// that can't be resolved (gone, or a network share that won't say) is compared as written when it
/// is there; one that isn't there is not inside, and listing it reports why.
fn inside(root: &Path, lib: &Path) -> bool {
    let as_written = || root.starts_with(browse_path(lib).unwrap_or_else(|_| lib.to_path_buf()));
    match std::fs::canonicalize(root) {
        Ok(resolved) => as_written() || std::fs::canonicalize(lib).is_ok_and(|lib| resolved.starts_with(lib)),
        Err(_) => root.exists() && as_written(),
    }
}

/// Order `files` the way a view sorted by `sort` will show them, as far as the file system can
/// tell before the files are read: by modification time for the capture date (a camera's file
/// time is its capture time, near enough to fill the first screen first), by name, or by size.
/// New photos share one import time and have no edit time, so the import- and edit-date sorts
/// show them by name. Ties go by name in the view's direction, as the catalog breaks them
/// ([`lightcraft_catalog::Sort::order`]); new photos tie on rating too. Random keeps the given order. Files whose
/// time or size can't be read go last.
pub fn browse_order(files: &mut [String], sort: &lightcraft_catalog::Sort) {
    browse_order_until(files, sort, &std::sync::atomic::AtomicBool::new(false));
}

/// [`browse_order`] that stops reading the files' times or sizes once `cancel` is set: `false` then, and
/// `files` is left as it was.
fn browse_order_until(files: &mut [String], sort: &lightcraft_catalog::Sort, cancel: &std::sync::atomic::AtomicBool) -> bool {
    use lightcraft_catalog::SortKey;
    use lightcraft_catalog::query::cmp_name;
    use std::cmp::Ordering;
    use std::sync::atomic::Ordering::Relaxed;
    let asc = sort.ascending;
    let name = |f: &str| Path::new(f).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let directed = |o: Ordering| if asc { o } else { o.reverse() };
    // `key` per file (`None`: unreadable, last in either direction), then the name, in the view's direction
    let by_key = |files: &mut [String], key: &dyn Fn(&str) -> Option<u64>| {
        let mut keyed: Vec<(Option<u64>, String, String)> = Vec::with_capacity(files.len());
        for f in files.iter() {
            if cancel.load(Relaxed) {
                return false;
            }
            keyed.push((key(f), name(f), f.clone()));
        }
        keyed.sort_by(|a, b| match (a.0, b.0) {
            (Some(x), Some(y)) => directed(x.cmp(&y).then_with(|| cmp_name(&a.1, &b.1))),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => directed(cmp_name(&a.1, &b.1)),
        });
        for (slot, (_, _, f)) in files.iter_mut().zip(keyed) {
            *slot = f;
        }
        true
    };
    match sort.key {
        SortKey::CaptureDate => by_key(files, &|f| {
            let t = std::fs::metadata(f).and_then(|m| m.modified()).ok()?;
            // (nanoseconds since the epoch fit a u64 until 2554; earlier times count as the epoch)
            Some(t.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)))
        }),
        SortKey::ImportDate | SortKey::EditDate | SortKey::Rating | SortKey::FileName => {
            files.sort_by(|a, b| directed(cmp_name(&name(a), &name(b))));
            !cancel.load(Relaxed)
        }
        SortKey::FileSize => by_key(files, &|f| std::fs::metadata(f).ok().map(|m| m.len())),
        SortKey::Random => !cancel.load(Relaxed),
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
    // probes from a preceding `scan` are reused when the file is unchanged (same size); a file that
    // can't be looked at counts as unchanged (✕ set while they are checked: no more round trips to the
    // disk; what is not reached counts as not probed, like the files after it)
    let cached = reusable_probes(&todo, &mut input.cache, &progress.cancel, |seen, info| match seen {
        Seen::At { len, .. } => len == info.file_size,
        Seen::Gone => true,
        Seen::Unchecked => false,
    });
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
    ScanOutput { candidates: out, probes: kept }
}
