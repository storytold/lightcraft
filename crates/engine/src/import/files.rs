#[cfg(not(target_arch = "wasm32"))]
use super::commit::workers;
#[cfg(test)]
use super::job::{ImportJob, Prepared, PreparedItem, ReadyFile};
use super::scan::ScanProgress;
#[cfg(test)]
use super::types::{ImportMode, ImportOptions};
use super::{Path, PathBuf, ProbeInfo, Session};

/// File extensions offered by import discovery and file pickers (lower case).
/// This includes preview-only raws; the file probe decides whether sensor decoding is available.
pub const EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "tif", "tiff", "webp", "dng", "cr2", "cr3", "nef", "nrw", "arw", "sr2", "srf", "raf", "orf", "rw2", "rwl", "raw", "pef",
    "srw", "psd", "psb", "jxl", "gif", "bmp",
    // Decoded when built with the codecs' `heif` feature; otherwise the import reports them as
    // failed with the reason ("HEIC/HEIF support isn't included in this build").
    "heic", "heif", "avif", // no AV1 decoder yet: reported as failed, like a HEIC without the feature
    "iiq", "crw", "mrw", "x3f", "kdc", "mos", "erf", "3fr", "fff", "mef",
];

pub fn is_supported(path: &Path) -> bool {
    path.extension().is_some_and(|e| EXTENSIONS.contains(&e.to_string_lossy().to_lowercase().as_str()))
}

/// The web file picker's accept list, derived from [`EXTENSIONS`]. Each extension
/// is listed in both cases: browsers' `accept` matching is case-insensitive per
/// spec, but some implementations (e.g. Windows/Edge) match case-sensitively and
/// rejected uppercase `.NEF` files, so a lowercase-only list silently hid raws
/// such as CR3, NRW, RAF, ORF, RW2, RWL, RAW, PEF and SRW from the picker.
///
/// The picker only offers files this build can actually import: `.avif` has no
/// AV1 decoder in any build, and `.heic`/`.heif` decode only when the codecs'
/// `heif` feature is enabled ([`lightcraft_codecs::Format::Heif::can_decode`]).
/// [`EXTENSIONS`] itself stays complete — files on disk are still recognised,
/// and import reports them as failed, saying why.
pub fn accept_list() -> String {
    let heif_decodable = lightcraft_codecs::Format::Heif.can_decode();
    let mut out = String::new();
    for &ext in EXTENSIONS {
        // Skip extensions a web user could pick but this build could not import.
        let pickable = match ext {
            "avif" => false,
            "heic" | "heif" => heif_decodable,
            _ => true,
        };
        if !pickable {
            continue;
        }
        if !out.is_empty() {
            out.push(',');
        }
        out.push('.');
        out.push_str(ext);
        out.push(',');
        out.push('.');
        out.push_str(&ext.to_uppercase());
    }
    out
}

/// Expand files and folders (recursively) into supported files. `skip` (e.g. the library folder)
/// is never descended into. Each folder's walk is bounded ([`crate::walk::Limits::default`]); a
/// walk that stops at a bound is logged.
pub fn expand(paths: &[String], skip: Option<&Path>) -> Vec<String> {
    expand_within(paths, skip, crate::walk::Limits::default()).0
}

/// [`expand`] with the walk bounded by `limits` → (files, whether a walk stopped at a bound).
pub fn expand_within(paths: &[String], skip: Option<&Path>, limits: crate::walk::Limits) -> (Vec<String>, bool) {
    expand_noting(paths, skip, limits, None)
}

/// [`expand_within`], handing the listing of every folder that is walked to `note`
/// ([`crate::walk::files_in_noting`]).
pub(crate) fn expand_noting(
    paths: &[String],
    skip: Option<&Path>,
    limits: crate::walk::Limits,
    mut note: Option<crate::walk::NoteFolder>,
) -> (Vec<String>, bool) {
    let mut out = Vec::new();
    let mut truncated = false;
    for p in paths {
        let p = Path::new(p);
        if skip.is_some_and(|s| p == s) {
            continue;
        }
        if p.is_dir() {
            let w = match note.as_mut() {
                Some(note) => crate::walk::files_in_noting(p, skip, limits, is_supported, &mut **note),
                None => crate::walk::files_in(p, skip, limits, is_supported),
            };
            if w.truncated {
                log::warn!(
                    "import: stopped looking in {} after {} entries (limits: {} entries, {} folder levels)",
                    p.display(),
                    w.entries,
                    limits.max_entries,
                    limits.max_depth
                );
            }
            truncated |= w.truncated;
            out.extend(w.files.into_iter().map(|f| f.to_string_lossy().to_string()));
        } else {
            // explicitly named files are attempted even with an unknown extension (sniffed)
            out.push(p.to_string_lossy().to_string());
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|p| seen.insert(p.clone()));
    (out, truncated)
}

/// What a path given on the command line at launch becomes ([`launch_path`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaunchPath {
    /// Import this (absolute) file or folder, in the background.
    Import(String),
    /// Not imported: `path` (absolute) is a whole drive or the whole home folder, which is never
    /// walked without being chosen in the app (issue #374).
    Refused { path: String, why: String },
}

/// Resolve a launch argument for importing (issue #374). An empty argument is ignored (`None`);
/// a relative one (`.`, `..`, `photos`; `/` or `\` on Windows, the current drive's root) is
/// resolved against `cwd` — the working folder a launcher picked, often a drive root or
/// `C:\Windows\System32` — and refused when that leaves no folder (`cwd` unknown). A drive
/// or file-system root is replaced by its camera folder (`DCIM`, from `dcim`: a memory card) or
/// refused, and so is the home folder `home`: either would walk most of the disk.
pub fn launch_path(arg: &str, cwd: Option<&Path>, home: Option<&Path>, dcim: impl Fn(&Path) -> Option<PathBuf>) -> Option<LaunchPath> {
    if arg.trim().is_empty() {
        return None;
    }
    let given = Path::new(arg);
    let joined = if given.is_absolute() {
        given.to_path_buf()
    } else {
        match cwd {
            Some(c) => c.join(given),
            None => return Some(LaunchPath::Refused { path: arg.to_string(), why: "a relative path, and the working folder is unknown".into() }),
        }
    };
    // `.` and `..` resolved by name (no file-system access)
    let mut abs = PathBuf::new();
    for c in joined.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                abs.pop();
            }
            other => abs.push(other.as_os_str()),
        }
    }
    let shown = abs.to_string_lossy().to_string();
    if abs.parent().is_none() {
        return Some(match dcim(&abs) {
            Some(d) => LaunchPath::Import(d.to_string_lossy().to_string()),
            None => LaunchPath::Refused { path: shown, why: "a whole drive".into() },
        });
    }
    let key = lightcraft_catalog::query::folder_key;
    if home.is_some_and(|h| key(&h.to_string_lossy()) == key(&shown)) {
        return Some(LaunchPath::Refused { path: shown, why: "the whole home folder".into() });
    }
    Some(LaunchPath::Import(shown))
}

/// Probe files (headers + content hash) as an import would.
pub(crate) fn probe_paths(s: &Session, paths: &[String]) -> Vec<Result<ProbeInfo, String>> {
    let progress = ScanProgress::default();
    probe_all(s.media.file_probe.as_ref(), paths, &progress.done, &progress.cancel)
}

/// Probe files with `probe` (a clone of the session's file probe), off the session: on a worker thread.
pub(crate) fn probe_with(probe: &crate::media::FileProbe, paths: &[String]) -> Vec<Result<ProbeInfo, String>> {
    let progress = ScanProgress::default();
    probe_all(Some(probe), paths, &progress.done, &progress.cancel)
}

/// Probe `paths` (several at once), counting each in `done`. Stops before the next file once
/// `cancel` is set: the files not reached come back as errors.
pub(crate) fn probe_all(
    probe: Option<&crate::media::FileProbe>,
    paths: &[String],
    done: &std::sync::atomic::AtomicUsize,
    cancel: &std::sync::atomic::AtomicBool,
) -> Vec<Result<ProbeInfo, String>> {
    use std::sync::atomic::Ordering::Relaxed;
    let Some(probe) = probe.cloned() else {
        return paths
            .iter()
            .map(|p| {
                Ok(ProbeInfo {
                    format: Path::new(p).extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_default(),
                    ..Default::default()
                })
            })
            .collect();
    };
    #[cfg(not(target_arch = "wasm32"))]
    {
        let n = workers(paths.len());
        if n > 1 {
            let mut results: Vec<Option<Result<ProbeInfo, String>>> = vec![None; paths.len()];
            let next = std::sync::atomic::AtomicUsize::new(0);
            let out = std::sync::Mutex::new(&mut results);
            std::thread::scope(|sc| {
                for _ in 0..n {
                    sc.spawn(|| {
                        loop {
                            let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            if i >= paths.len() {
                                break;
                            }
                            if cancel.load(Relaxed) {
                                break;
                            }
                            let r = probe(&paths[i]);
                            done.fetch_add(1, Relaxed);
                            out.lock().unwrap_or_else(|e| e.into_inner())[i] = Some(r);
                        }
                    });
                }
            });
            return results.into_iter().map(|r| r.unwrap_or_else(|| Err("not probed".into()))).collect();
        }
    }
    paths
        .iter()
        .map(|p| {
            if cancel.load(Relaxed) {
                return Err("cancelled".into());
            }
            let r = probe(p);
            done.fetch_add(1, Relaxed);
            r
        })
        .collect()
}

/// Copy `src` into `dir` as `name` (default: its own name), made unique with -1, -2…; returns the
/// new path. The copy is verified ([`crate::import_move::copy_new`]: a new file, synced and
/// checked against `probe_hash`, the content hash the probe computed — or compared byte for byte
/// with the source when there is none); a bad one is removed and the photo reported as failed.
pub(super) fn copy_into(dir: &Path, src: &str, name: Option<&str>, probe_hash: Option<&str>) -> Result<String, String> {
    let name = name
        .map(str::to_string)
        .unwrap_or_else(|| Path::new(src).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "photo".into()));
    let expect = probe_hash.and_then(lightcraft_preview::Hash128::parse);
    crate::import_move::copy_new(Path::new(src), dir, &name, expect).map(|p| p.to_string_lossy().to_string())
}

#[cfg(test)]
mod prepared_tests {
    use super::*;
    use lightcraft_catalog::{Op, Photo, PhotoId, Source};

    /// Extensions the picker may offer: every one in [`EXTENSIONS`], minus the
    /// files this build cannot import (see [`accept_list`]).
    fn pickable_extensions() -> Vec<&'static str> {
        let heif_decodable = lightcraft_codecs::Format::Heif.can_decode();
        EXTENSIONS
            .iter()
            .copied()
            .filter(|ext| match *ext {
                "avif" => false,
                "heic" | "heif" => heif_decodable,
                _ => true,
            })
            .collect()
    }

    #[test]
    fn accept_list_covers_every_pickable_extension_in_both_cases() {
        let list = accept_list();
        for ext in pickable_extensions() {
            assert!(list.contains(&format!(".{ext}")), "lowercase .{ext} missing from accept list");
            assert!(list.contains(&format!(".{}", ext.to_uppercase())), "uppercase .{} missing from accept list", ext.to_uppercase());
        }
    }

    /// The web picker must not offer files this build cannot import: `.avif` has
    /// no AV1 decoder in any build, so it is never listed, in either case.
    #[test]
    fn accept_list_never_offers_avif() {
        let list = accept_list();
        assert!(!list.contains(".avif"), "{list}");
        assert!(!list.contains(".AVIF"), "{list}");
    }

    /// `.heic`/`.heif` follow the codecs' `heif` feature: listed (in both cases)
    /// only when this build can decode them; otherwise hidden, so a web user
    /// cannot pick a file that would fail to import.
    #[test]
    fn accept_list_offers_heic_heif_only_when_decodable() {
        let list = accept_list();
        let listed = lightcraft_codecs::Format::Heif.can_decode();
        for ext in ["heic", "heif"] {
            assert_eq!(list.contains(&format!(".{ext}")), listed, "lowercase .{ext}");
            assert_eq!(list.contains(&format!(".{}", ext.to_uppercase())), listed, "uppercase .{ext}");
        }
    }

    #[test]
    fn revalidate_add_only_converts_matching_ready_item() {
        let mut s = Session::new();
        let existing_path = std::env::temp_dir().join("lightcraft-revalidate-existing.jpg");
        let other_path = std::env::temp_dir().join("lightcraft-revalidate-other.jpg");
        let existing = Photo::new(PhotoId(1), Source::File { path: existing_path.to_string_lossy().into() }, "existing.jpg", "JPG", 1, 1, "now");
        s.commit("existing", Op::AddPhoto { photo: Box::new(existing) }).unwrap();
        let ready = |path: &Path| ReadyFile {
            path: path.to_string_lossy().into(),
            stored: path.to_string_lossy().into(),
            info: ProbeInfo { format: "JPG".into(), ..ProbeInfo::default() },
            sidecar: None,
            placed: None,
        };
        let mut prepared = Prepared {
            scanned: 2,
            items: vec![PreparedItem::Ready(Box::new(ready(&existing_path))), PreparedItem::Ready(Box::new(ready(&other_path)))],
        };
        prepared.revalidate_add(&s);
        assert!(matches!(prepared.items.first(), Some(PreparedItem::Duplicate { reason: "path", .. })));
        assert!(matches!(prepared.items.get(1), Some(PreparedItem::Ready(_))));
    }

    fn unread_paths(n: usize) -> Vec<String> {
        (0..n).map(|i| std::env::temp_dir().join(format!("lightcraft-probe-progress-{i}.jpg")).to_string_lossy().into()).collect()
    }

    /// Reading the files of a large import (a Lightroom catalog's originals) reported nothing:
    /// the progress stood still at "14 of 24554" for as long as it took.
    #[test]
    fn prepare_counts_the_files_it_reads() {
        use std::sync::atomic::Ordering::Relaxed;
        let mut s = Session::new();
        let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (seen_in_probe, done_in_probe) = (seen.clone(), std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)));
        let mut job = ImportJob::new(&mut s, ImportOptions { mode: ImportMode::Add, ..ImportOptions::default() }).unwrap();
        let (done, last) = (job.read_done.clone(), done_in_probe.clone());
        job.probe = Some(std::sync::Arc::new(move |_: &str| {
            seen_in_probe.fetch_add(1, Relaxed);
            last.fetch_max(done.load(Relaxed), Relaxed);
            Ok(ProbeInfo { format: "JPEG".into(), ..ProbeInfo::default() })
        }));
        let prepared = job.prepare(&unread_paths(40), &std::sync::atomic::AtomicBool::new(false));
        assert_eq!(prepared.items.len(), 40);
        assert_eq!(seen.load(Relaxed), 40);
        assert!(done_in_probe.load(Relaxed) > 0, "progress moves while files are read");
        assert_eq!((job.read_done.load(Relaxed), job.read_total.load(Relaxed)), (40, 40), "every file read is counted");
        assert_eq!((job.done.load(Relaxed), job.total.load(Relaxed)), (40, 40), "handling them is counted on its own");
    }

    /// Cancel had no effect until every file had been read.
    #[test]
    fn cancel_stops_reading_files() {
        use std::sync::atomic::Ordering::Relaxed;
        let mut s = Session::new();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut job = ImportJob::new(&mut s, ImportOptions { mode: ImportMode::Add, ..ImportOptions::default() }).unwrap();
        let (seen_in_probe, cancel_in_probe) = (seen.clone(), cancel.clone());
        job.probe = Some(std::sync::Arc::new(move |_: &str| {
            seen_in_probe.fetch_add(1, Relaxed);
            cancel_in_probe.store(true, Relaxed);
            Ok(ProbeInfo { format: "JPEG".into(), ..ProbeInfo::default() })
        }));
        let prepared = job.prepare(&unread_paths(400), &cancel);
        // at most the files the (up to 8) readers had already started
        assert!(seen.load(Relaxed) <= 8, "read {} files after cancel", seen.load(Relaxed));
        assert!(prepared.items.is_empty());
    }
}
