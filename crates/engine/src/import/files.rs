use std::collections::HashMap;

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
    "iiq", "crw", "mrw", "x3f", "kdc", "mos", "erf", "3fr", "fff", "mef", "lri",
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
    /// Open this (absolute) library folder: the argument was a library's `catalog.snap` or its folder, as a
    /// double-click on a catalog in the file manager gives. Never imported.
    Library(String),
}

/// The library folder `path` names: a library's `catalog.snap` (its folder), or a folder holding one. `None` for
/// anything else (photos, folders of photos). The folder must pass the library opener's own test
/// ([`crate::library::is_library`]), so a `CATALOG.SNAP` the opener would not read (a case-sensitive file system)
/// never opens its folder as a new, empty library.
pub fn library_dir_of(path: &Path) -> Option<PathBuf> {
    let snapshot = lightcraft_catalog::journal::SNAPSHOT;
    let named = path.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(snapshot));
    if named && path.is_file() {
        return path.parent().filter(|dir| crate::library::is_library(dir)).map(Path::to_path_buf);
    }
    (path.is_dir() && crate::library::is_library(path)).then(|| path.to_path_buf())
}

/// How long the launch arguments' library checks may take, all of them together, before LightCraft gives up and
/// treats the arguments not yet checked as photos to import (in the background). A path on a dead network mount
/// can block a file-system query for minutes, and these checks run before the window opens.
pub const LAUNCH_PROBE_LIMIT: std::time::Duration = std::time::Duration::from_secs(2);

/// When the launch arguments' library checks, starting now, must be done: [`LAUNCH_PROBE_LIMIT`] from now.
pub fn launch_deadline() -> std::time::Instant {
    let now = std::time::Instant::now();
    now.checked_add(LAUNCH_PROBE_LIMIT).unwrap_or(now)
}

/// `probe`'s answer, or `None` when it takes longer than `limit`: it runs on its own thread, which is left to
/// finish (or stay blocked) on its own. Also `None` when no thread can be started.
pub(crate) fn within<T: Send + 'static>(limit: std::time::Duration, probe: impl FnOnce() -> Option<T> + Send + 'static) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    let started = std::thread::Builder::new().name("launch-probe".into()).spawn(move || {
        let _ = tx.send(probe());
    });
    if let Err(e) = started {
        log::warn!("could not start the launch-path check: {e}");
        return None;
    }
    match rx.recv_timeout(limit) {
        Ok(found) => found,
        Err(_) => {
            log::warn!("the launch-path check took longer than {limit:?}; treating the argument as photos to import");
            None
        }
    }
}

/// Resolve a launch argument for importing (issue #374). An empty argument is ignored (`None`);
/// a relative one (`.`, `..`, `photos`; `/` or `\` on Windows, the current drive's root) is
/// resolved against `cwd` — the working folder a launcher picked, often a drive root or
/// `C:\Windows\System32` — and refused when that leaves no folder (`cwd` unknown). A drive
/// or file-system root is replaced by its camera folder (`DCIM`, from `dcim`: a memory card) or
/// refused, and so is the home folder `home`: either would walk most of the disk.
pub fn launch_path(arg: &str, cwd: Option<&Path>, home: Option<&Path>, dcim: impl Fn(&Path) -> Option<PathBuf>) -> Option<LaunchPath> {
    launch_path_until(arg, cwd, home, dcim, launch_deadline())
}

/// [`launch_path`], with its library check bounded by `deadline` (from [`launch_deadline`]), which every argument
/// of one launch shares: several paths on a dead mount wait [`LAUNCH_PROBE_LIMIT`] in all, not each. Once it has
/// passed, an argument is imported without the check.
pub fn launch_path_until(
    arg: &str,
    cwd: Option<&Path>,
    home: Option<&Path>,
    dcim: impl Fn(&Path) -> Option<PathBuf>,
    deadline: std::time::Instant,
) -> Option<LaunchPath> {
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
    let left = deadline.saturating_duration_since(std::time::Instant::now());
    let probed = abs.clone();
    if !left.is_zero()
        && let Some(dir) = within(left, move || library_dir_of(&probed))
    {
        return Some(LaunchPath::Library(dir.to_string_lossy().to_string()));
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

/// What the disk says about a file ([`stat_all`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Seen {
    /// Not asked about: `cancel` was set first.
    Unchecked,
    /// Not there, or not readable.
    Gone,
    At {
        is_file: bool,
        len: u64,
    },
}

/// [`Seen`] of `path` from the file system.
fn stat_file(path: &str) -> Seen {
    match std::fs::metadata(path) {
        Ok(m) => Seen::At { is_file: m.is_file(), len: m.len() },
        Err(_) => Seen::Gone,
    }
}

/// Files asked about at once: a question to a network share (a stat, a name looked up) is a round
/// trip (50 ms and more under load) that waits for nothing but the server, so a few more than the
/// readers' threads.
#[cfg(not(target_arch = "wasm32"))]
const AT_ONCE: usize = 16;

/// Run `ask` on each of `items`, several at once (one at a time, a folder of thousands of photos on
/// a network share waited minutes for its questions alone). The answers come in the order of
/// `items`; `None` for an item not reached once `cancel` is set (or whose asker died).
fn at_once<T: Sync, R: Send>(items: &[T], cancel: &std::sync::atomic::AtomicBool, ask: impl Fn(&T) -> R + Sync) -> Vec<Option<R>> {
    use std::sync::atomic::Ordering::Relaxed;
    let mut answers: Vec<Option<R>> = items.iter().map(|_| None).collect();
    #[cfg(not(target_arch = "wasm32"))]
    {
        let n = AT_ONCE.min(items.len());
        if n > 1 {
            let next = std::sync::atomic::AtomicUsize::new(0);
            let mine: Vec<Vec<(usize, R)>> = std::thread::scope(|sc| {
                let askers: Vec<_> = (0..n)
                    .map(|_| {
                        sc.spawn(|| {
                            let mut mine = Vec::new();
                            loop {
                                let i = next.fetch_add(1, Relaxed);
                                let Some(item) = items.get(i) else { break };
                                if cancel.load(Relaxed) {
                                    break;
                                }
                                mine.push((i, ask(item)));
                            }
                            mine
                        })
                    })
                    .collect();
                askers.into_iter().map(|a| a.join().unwrap_or_default()).collect()
            });
            for (i, r) in mine.into_iter().flatten() {
                if let Some(slot) = answers.get_mut(i) {
                    *slot = Some(r);
                }
            }
            return answers;
        }
    }
    for (slot, item) in answers.iter_mut().zip(items) {
        if cancel.load(Relaxed) {
            break;
        }
        *slot = Some(ask(item));
    }
    answers
}

/// Ask `stat` about each of `paths`, several at once ([`at_once`]); the ones not reached once
/// `cancel` is set are [`Seen::Unchecked`].
pub(crate) fn stat_all(paths: &[&str], cancel: &std::sync::atomic::AtomicBool, stat: impl Fn(&str) -> Seen + Sync) -> Vec<Seen> {
    at_once(paths, cancel, |p| stat(p)).into_iter().map(|s| s.unwrap_or(Seen::Unchecked)).collect()
}

/// The sidecar of each of `files` (`find` says which: a few names looked up on the disk for each,
/// a round trip apiece on a network share), several files at once ([`at_once`]); `None` for a
/// file with no sidecar or not reached once `cancel` is set.
pub(super) fn sidecars_of(
    files: &[&str],
    cancel: &std::sync::atomic::AtomicBool,
    find: impl Fn(&str) -> Option<PathBuf> + Sync,
) -> Vec<Option<PathBuf>> {
    at_once(files, cancel, |f| find(f)).into_iter().map(Option::flatten).collect()
}

/// The probes in `cache` of `files` that are still true: the file is looked at ([`stat_all`]) and
/// `ok` says it is as it was probed. `None` for a file with no probe in `cache`, one that changed,
/// and one not looked at because `cancel` was set.
pub(super) fn reusable_probes(
    files: &[String],
    cache: &mut HashMap<String, ProbeInfo>,
    cancel: &std::sync::atomic::AtomicBool,
    ok: impl Fn(Seen, &ProbeInfo) -> bool,
) -> Vec<Option<ProbeInfo>> {
    let held: Vec<Option<ProbeInfo>> = files.iter().map(|f| cache.remove(f)).collect();
    let asked: Vec<&str> = files.iter().zip(&held).filter(|(_, h)| h.is_some()).map(|(f, _)| f.as_str()).collect();
    let mut seen = stat_all(&asked, cancel, stat_file).into_iter();
    held.into_iter().map(|h| h.filter(|info| seen.next().is_some_and(|s| s != Seen::Unchecked && ok(s, info)))).collect()
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

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lc-launch-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A launch argument names a library only when the library opener would read that folder: a `CATALOG.SNAP`
    /// on a case-sensitive file system (no `catalog.snap` or `catalog.log` beside it) is not one.
    #[test]
    fn a_launch_argument_names_a_library_only_when_the_opener_can_read_it() {
        let dir = temp_dir("upper");
        let upper = dir.join("CATALOG.SNAP");
        std::fs::write(&upper, b"x").unwrap();
        let found = library_dir_of(&upper);
        assert_eq!(found.is_some(), crate::library::is_library(&dir), "{found:?}");
        if let Some(d) = &found {
            assert!(crate::library::is_library(d), "{} opened as a library the opener can't read", d.display());
        }
        // a folder holding only the journal is a library to the opener, so it is one here too
        let journal = temp_dir("journal");
        std::fs::write(journal.join(lightcraft_catalog::journal::LOG), b"x").unwrap();
        assert_eq!(library_dir_of(&journal), Some(journal.clone()));
        // a folder of photos is not
        let photos = temp_dir("photos");
        std::fs::write(photos.join("a.jpg"), b"x").unwrap();
        assert_eq!(library_dir_of(&photos), None);
        for d in [dir, journal, photos] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    /// The library check runs before the window opens; one that blocks (a dead network mount) gives up after the
    /// limit and the argument is imported instead, so LightCraft still starts.
    #[test]
    fn a_blocked_launch_probe_gives_up_after_its_limit() {
        let start = std::time::Instant::now();
        let found = within(std::time::Duration::from_millis(50), || {
            std::thread::sleep(std::time::Duration::from_secs(5));
            Some(PathBuf::from("late"))
        });
        assert_eq!(found, None);
        assert!(start.elapsed() < std::time::Duration::from_secs(2), "waited {:?}", start.elapsed());
        assert_eq!(within(LAUNCH_PROBE_LIMIT, || Some(1)), Some(1), "a quick answer is kept");
    }

    /// The launch arguments share one deadline (CodeRabbit on #2979): once it has passed, an argument is imported
    /// without its library check, so several paths on a dead mount can't each wait the limit.
    #[test]
    fn launch_arguments_share_one_deadline() {
        let dir = temp_dir("deadline");
        std::fs::write(dir.join(lightcraft_catalog::journal::LOG), b"x").unwrap();
        let arg = dir.to_string_lossy().to_string();
        let none = |_: &Path| None;
        assert_eq!(launch_path_until(&arg, None, None, none, launch_deadline()), Some(LaunchPath::Library(arg.clone())));
        let start = std::time::Instant::now();
        assert_eq!(launch_path_until(&arg, None, None, none, start), Some(LaunchPath::Import(arg.clone())), "deadline passed");
        assert!(start.elapsed() < std::time::Duration::from_millis(500), "waited {:?}", start.elapsed());
        let _ = std::fs::remove_dir_all(dir);
    }

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

    /// Given files on a network share, when an import checks the probes a scan left, then it asks
    /// the disk about several files at once: one at a time, 5,990 photos spent 300 s on stats alone
    /// (99 % of the import's time), each a round trip to the server.
    #[test]
    fn stats_are_asked_for_several_files_at_once() {
        use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
        let (active, together) = (AtomicUsize::new(0), AtomicUsize::new(0));
        let names: Vec<String> = (0..6).map(|i| format!("file-{i}")).collect();
        let paths: Vec<&str> = names.iter().map(String::as_str).collect();
        let seen = stat_all(&paths, &std::sync::atomic::AtomicBool::new(false), |p| {
            active.fetch_add(1, Relaxed);
            // waits (at most a second) for another file to be asked about while this one is
            let end = std::time::Instant::now() + std::time::Duration::from_secs(1);
            while active.load(Relaxed) < 2 && std::time::Instant::now() < end {
                std::thread::yield_now();
            }
            together.fetch_max(active.load(Relaxed), Relaxed);
            active.fetch_sub(1, Relaxed);
            Seen::At { is_file: true, len: p.trim_start_matches("file-").parse().unwrap_or(u64::MAX) }
        });
        assert!(together.load(Relaxed) >= 2, "the files were asked about one at a time");
        let lens: Vec<Seen> = (0..6).map(|len| Seen::At { is_file: true, len }).collect();
        assert_eq!(seen, lens, "the answers keep the order of the files");
    }

    /// ✕ while the stats run: the files not asked about yet stay unchecked, and no more are asked.
    #[test]
    fn stats_stop_when_cancelled() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
        let (cancel, asked) = (AtomicBool::new(false), AtomicUsize::new(0));
        let names: Vec<String> = (0..400).map(|i| format!("file-{i}")).collect();
        let paths: Vec<&str> = names.iter().map(String::as_str).collect();
        let seen = stat_all(&paths, &cancel, |_| {
            asked.fetch_add(1, Relaxed);
            cancel.store(true, Relaxed);
            Seen::Gone
        });
        assert_eq!(seen.len(), 400);
        // at most the files the (up to 16) readers had already started
        assert!(asked.load(Relaxed) <= 16, "asked about {} files after cancel", asked.load(Relaxed));
        assert_eq!(seen.iter().filter(|s| **s == Seen::Gone).count(), asked.load(Relaxed));
        assert!(seen.iter().all(|s| matches!(s, Seen::Gone | Seen::Unchecked)));
    }

    /// A probe a scan left is used again for a file that is as it was, and only for it: a file that
    /// changed size, one that is gone and one with no probe are read.
    #[test]
    fn a_scans_probe_is_reused_only_for_a_file_as_it_was() {
        use std::sync::atomic::Ordering::Relaxed;
        let dir = std::env::temp_dir().join(format!("lightcraft-reuse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut files = Vec::new();
        for (name, bytes) in [("same.jpg", 10usize), ("grown.jpg", 10), ("fresh.jpg", 10)] {
            let p = dir.join(name);
            std::fs::write(&p, vec![0u8; bytes]).unwrap();
            files.push(p.to_string_lossy().to_string());
        }
        let gone = dir.join("gone.jpg").to_string_lossy().to_string();
        files.push(gone.clone());
        let mut s = Session::new();
        let mut job = ImportJob::new(&mut s, ImportOptions { mode: ImportMode::Add, ..ImportOptions::default() }).unwrap();
        let probed = |size: u64| ProbeInfo { format: "JPEG".into(), file_size: size, ..ProbeInfo::default() };
        job.cache.insert(files[0].clone(), probed(10));
        job.cache.insert(files[1].clone(), probed(5));
        job.cache.insert(gone.clone(), probed(10));
        let read = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let read_in_probe = read.clone();
        job.probe = Some(std::sync::Arc::new(move |p: &str| {
            read_in_probe.lock().unwrap().push(p.to_string());
            Ok(probed(10))
        }));
        let prepared = job.prepare_files(files.clone(), &std::sync::atomic::AtomicBool::new(false));
        let mut read = read.lock().unwrap().clone();
        read.sort();
        let mut expect = vec![files[1].clone(), files[2].clone(), gone];
        expect.sort();
        assert_eq!(read, expect, "only what was not as the scan saw it is read");
        assert_eq!(job.read_total.load(Relaxed), 3);
        assert_eq!(prepared.items.len(), 4);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Given photos on a network share, when an import looks for their sidecars, then it asks about
    /// several photos at once: one photo after another, nine names each, they were 77 % of the
    /// import's preparation (95 % where a folder listing could not vouch for the names).
    #[test]
    fn sidecars_are_looked_up_for_several_photos_at_once() {
        use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
        let (active, together) = (AtomicUsize::new(0), AtomicUsize::new(0));
        let names: Vec<String> = (0..6).map(|i| format!("photo-{i}.jpg")).collect();
        let files: Vec<&str> = names.iter().map(String::as_str).collect();
        let found = sidecars_of(&files, &std::sync::atomic::AtomicBool::new(false), |f| {
            active.fetch_add(1, Relaxed);
            let end = std::time::Instant::now() + std::time::Duration::from_secs(1);
            while active.load(Relaxed) < 2 && std::time::Instant::now() < end {
                std::thread::yield_now();
            }
            together.fetch_max(active.load(Relaxed), Relaxed);
            active.fetch_sub(1, Relaxed);
            f.ends_with("-3.jpg").then(|| PathBuf::from(f).with_extension("xmp"))
        });
        assert!(together.load(Relaxed) >= 2, "the photos were looked up one at a time");
        let expect: Vec<Option<PathBuf>> = (0..6).map(|i| (i == 3).then(|| PathBuf::from("photo-3.xmp"))).collect();
        assert_eq!(found, expect, "each sidecar stays with its photo");
    }

    /// The sidecars found for a batch go to the photos they belong to, whichever have one, and a
    /// photo that failed to probe takes none of the others'.
    #[test]
    fn a_batchs_sidecars_go_to_their_own_photos() {
        let dir = std::env::temp_dir().join(format!("lightcraft-sidecars-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let xmp = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="4"/></rdf:RDF></x:xmpmeta>"#;
        let files: Vec<String> = ["a.jpg", "broken.jpg", "c.jpg", "d.jpg"].iter().map(|n| dir.join(n).to_string_lossy().to_string()).collect();
        for f in &files {
            std::fs::write(f, b"x").unwrap();
        }
        std::fs::write(dir.join("c.xmp"), xmp).unwrap();
        let mut s = Session::new();
        let mut job = ImportJob::new(&mut s, ImportOptions { mode: ImportMode::Add, ..ImportOptions::default() }).unwrap();
        job.probe = Some(std::sync::Arc::new(|p: &str| {
            if p.ends_with("broken.jpg") {
                Err("unreadable".into())
            } else {
                Ok(ProbeInfo { format: "JPEG".into(), file_size: 1, ..ProbeInfo::default() })
            }
        }));
        let prepared = job.prepare_files(files, &std::sync::atomic::AtomicBool::new(false));
        let rating = |name: &str| {
            prepared.items.iter().find_map(|i| match i {
                PreparedItem::Ready(r) if r.path.ends_with(name) => Some(r.sidecar.as_ref().and_then(|sc| sc.rating)),
                _ => None,
            })
        };
        assert_eq!((rating("a.jpg"), rating("c.jpg"), rating("d.jpg")), (Some(None), Some(Some(4)), Some(None)));
        assert!(prepared.items.iter().any(|i| matches!(i, PreparedItem::Failed(p, _) if p.ends_with("broken.jpg"))));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
