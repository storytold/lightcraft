//! Nikon High Efficiency NEF through Adobe DNG Converter (issue #3921).
//!
//! HE / HE★ NEFs carry intoPIX TicoRAW data, a licensed codec with no public description, so LightCraft shows them
//! from the camera's embedded JPEG ([`lightcraft_raw::is_nef_high_efficiency`]; issues #2169, #1334). Adobe's free DNG
//! Converter has a licence for it. When the user installed it, LightCraft can run it as a separate program on such a
//! file and develop the DNG it writes with its own DNG decoder. Nothing of the converter is bundled, linked or read:
//! LightCraft starts the program the user installed, with its documented command-line options, and reads the DNG.
//!
//! - The NEF stays the photo's file and the source of its metadata and XMP. The DNG is a cache entry, never written
//!   next to the original: `<cache>/<key>.dng`, where the key is a hash of the photo's content hash (the hash of the
//!   whole NEF the probe already computes), so a moved or renamed file keeps its DNG, a changed one is converted
//!   again, and no NEF is read to find its DNG. [`cache_dir`] is the operating system's cache folder.
//! - The converter writes into a temporary folder in the cache. The DNG moves into the cache only after it decodes,
//!   so a converter that fails, times out or is stopped never leaves a broken DNG to be read later. Temporary folders
//!   left by a process that was killed are swept when the next conversion starts.
//! - A photo developed from its DNG carries [`lightcraft_catalog::CONVERTED_DNG_STAMP`]: the render sources read the
//!   DNG for it ([`photo_dng`]), its render and thumbnail keys change (`media::quick::decoder_key`), and an undo of the
//!   reload that set it puts the photo back on its embedded preview.
//! - The probe describes a preview-only NEF whose DNG is cached from the DNG, so Reload from Disk (`photo.reload`)
//!   brings the catalog up to date.
//! - [`Settings::auto`] (off by default) lets the app convert an HE NEF when it is opened in Edit;
//!   `photo.convertHighEfficiency` converts on request. `app.dngConverter` reads and changes the settings (process-wide:
//!   a headless CLI or MCP process starts with the defaults and needs its own `app.dngConverter` call),
//!   `library.clearDngCache` empties the cache.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

/// The macOS default install location of the converter's program.
pub const MAC_DEFAULT: &str = "/Applications/Adobe DNG Converter.app/Contents/MacOS/Adobe DNG Converter";
/// The Windows default install location of the converter's program.
pub const WINDOWS_DEFAULT: &str = r"C:\Program Files\Adobe\Adobe DNG Converter\Adobe DNG Converter.exe";
/// How long one conversion may take before it is stopped (a 45 MP HE★ NEF takes a few seconds).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
/// Conversions run at once: one is bound by start-up and disk, not by cores (issue #3921 measured ~4 as best).
pub const PARALLEL: usize = 4;
/// The largest DNG read back from the converter (a 61 MP lossless DNG without a preview is about 100 MB).
const MAX_DNG: u64 = 256 << 20;
/// Prefix of the converter's temporary folders inside the cache.
const TMP_PREFIX: &str = ".tmp-";
/// A temporary folder of another process older than this is left over from a process that ended mid-conversion.
const STALE_TMP: Duration = Duration::from_secs(2 * 60 * 60);

/// What the user chose (Settings ▸ Performance, `app.dngConverter`). Process-wide, like the hot pixel limit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// Convert a High Efficiency NEF when it is opened in Edit, without asking. Off by default: the converter is a
    /// separate program, and each DNG takes about 1.7 × the NEF's size on disk.
    pub auto: bool,
    /// The converter's program (or, on macOS, its `.app`) when it isn't installed in the default location.
    pub path: Option<PathBuf>,
    /// How long one conversion may take.
    pub timeout: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { auto: false, path: None, timeout: DEFAULT_TIMEOUT }
    }
}

static SETTINGS: Mutex<Option<Settings>> = Mutex::new(None);

/// The current settings.
pub fn settings() -> Settings {
    SETTINGS.lock().unwrap_or_else(PoisonError::into_inner).clone().unwrap_or_default()
}

/// Replace the settings.
pub fn set_settings(s: Settings) {
    *SETTINGS.lock().unwrap_or_else(PoisonError::into_inner) = Some(s);
}

/// The places the converter is looked for when no path is set, in order.
pub fn default_locations() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if cfg!(target_os = "macos") {
        v.push(PathBuf::from(MAC_DEFAULT));
        if let Some(home) = std::env::var_os("HOME") {
            v.push(PathBuf::from(home).join(MAC_DEFAULT.trim_start_matches('/')));
        }
    } else if cfg!(windows) {
        if let Some(pf) = std::env::var_os("ProgramFiles") {
            v.push(PathBuf::from(pf).join(r"Adobe\Adobe DNG Converter\Adobe DNG Converter.exe"));
        }
        v.push(PathBuf::from(WINDOWS_DEFAULT));
    }
    v
}

/// The converter program to run: `path` when set (a macOS `.app` bundle is resolved to the program inside it), else
/// the first of `defaults` that `is_file` finds. A set path that doesn't exist gives `None`, not a default: the user
/// chose that one.
pub fn resolve(path: Option<&Path>, defaults: &[PathBuf], is_file: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    match path {
        Some(p) if p.as_os_str().is_empty() => resolve(None, defaults, is_file),
        Some(p) => {
            if is_file(p) {
                return Some(p.to_path_buf());
            }
            let inner = p.join("Contents/MacOS/Adobe DNG Converter");
            (p.extension().is_some_and(|e| e.eq_ignore_ascii_case("app")) && is_file(&inner)).then_some(inner)
        }
        None => defaults.iter().find(|p| is_file(p)).cloned(),
    }
}

/// The converter the current settings point to, if it is installed.
pub fn converter() -> Option<PathBuf> {
    resolve(settings().path.as_deref(), &default_locations(), Path::is_file)
}

/// The converter's arguments for one file, from Adobe's documented command-line options: `-c` lossless compressed
/// output, `-p0` no embedded preview (LightCraft shows the NEF's own camera JPEG, and the DNG stays smaller), `-d` the
/// output folder, `-o` the output file name, then the input file.
pub fn command_args(input: &Path, out_dir: &Path, out_name: &str) -> Vec<OsString> {
    vec!["-c".into(), "-p0".into(), "-d".into(), out_dir.as_os_str().to_owned(), "-o".into(), out_name.into(), input.as_os_str().to_owned()]
}

static CACHE_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Held by tests that change the process-wide settings or cache folder, so they don't run at once.
#[cfg(test)]
pub(crate) static TEST_SERIAL: Mutex<()> = Mutex::new(());

/// Keep converted DNGs in `dir` instead of the default folder (tests, hosts with their own cache); `None`: the default.
pub fn set_cache_dir(dir: Option<PathBuf>) {
    *CACHE_DIR.lock().unwrap_or_else(PoisonError::into_inner) = dir;
}

/// Where converted DNGs are kept, as an absolute path: the folder [`set_cache_dir`] chose, else
/// `$LIGHTCRAFT_DNG_CACHE`, else the operating system's cache folder (macOS `~/Library/Caches/LightCraft/dng-cache`,
/// Windows `%LOCALAPPDATA%\LightCraft\dng-cache`, elsewhere `$XDG_CACHE_HOME/lightcraft/dng-cache` or
/// `~/.cache/lightcraft/dng-cache`).
pub fn cache_dir() -> Option<PathBuf> {
    let chosen = CACHE_DIR.lock().unwrap_or_else(PoisonError::into_inner).clone();
    let dir = chosen.or_else(|| std::env::var_os("LIGHTCRAFT_DNG_CACHE").filter(|v| !v.is_empty()).map(PathBuf::from)).or_else(|| {
        let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        if cfg!(target_os = "macos") {
            env("HOME").map(|h| h.join("Library/Caches/LightCraft/dng-cache"))
        } else if cfg!(windows) {
            env("LOCALAPPDATA").map(|d| d.join("LightCraft").join("dng-cache"))
        } else {
            env("XDG_CACHE_HOME").or_else(|| env("HOME").map(|h| h.join(".cache"))).map(|c| c.join("lightcraft").join("dng-cache"))
        }
    })?;
    // (the converter runs with this as its output folder: a relative one would depend on its working folder)
    std::path::absolute(&dir).ok()
}

/// Whether `path` names a NEF (only those are converted).
pub fn is_nef_path(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("nef"))
}

/// The cache file name for the NEF whose content hash is `content_hash`: a hash of the content hash (hex digits, so it
/// never starts with `-`, which the converter would read as an option). Neither the path's folder nor its file name
/// counts, so a moved or renamed file keeps its DNG.
pub fn cache_name(content_hash: &str) -> String {
    format!("{}.dng", lightcraft_preview::hash_bytes(content_hash.as_bytes()))
}

/// Where the DNG of the NEF at `path` with content hash `content_hash` is (or would be) cached. No file-system call.
pub fn dng_path(path: &Path, content_hash: &str) -> Option<PathBuf> {
    (is_nef_path(path) && !content_hash.is_empty()).then(|| Some(cache_dir()?.join(cache_name(content_hash)))).flatten()
}

/// The DNG photo `p` develops from: a photo marked converted ([`lightcraft_catalog::Photo::converted_dng`]) with a
/// file and a content hash. No file-system call (the loader falls back to the file when the DNG is gone).
pub fn photo_dng(p: &lightcraft_catalog::Photo) -> Option<PathBuf> {
    if !p.converted_dng() {
        return None;
    }
    match (&p.source, &p.content_hash) {
        (lightcraft_catalog::Source::File { path }, Some(hash)) => dng_path(Path::new(path), hash),
        _ => None,
    }
}

/// NEFs being converted now (cache file names), so one file is never converted twice at once.
static IN_FLIGHT: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// A claim on converting one file: [`IN_FLIGHT`] holds its name until it is dropped.
struct InFlight(String);

impl InFlight {
    fn claim(name: &str) -> Option<InFlight> {
        let mut v = IN_FLIGHT.lock().unwrap_or_else(PoisonError::into_inner);
        if v.iter().any(|n| n == name) {
            return None;
        }
        v.push(name.to_string());
        Some(InFlight(name.to_string()))
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        IN_FLIGHT.lock().unwrap_or_else(PoisonError::into_inner).retain(|n| *n != self.0);
    }
}

/// This process's temporary folder name for cache file `name`.
fn tmp_name(name: &str) -> String {
    format!("{TMP_PREFIX}{}-{name}", std::process::id())
}

/// Whether the temporary folder `entry` (a name in the cache) belongs to a conversion running now: one of this
/// process's in flight, or another process's younger than [`STALE_TMP`].
fn tmp_in_use(entry: &str, modified: Option<std::time::SystemTime>, running: &[String]) -> bool {
    let own = format!("{TMP_PREFIX}{}-", std::process::id());
    match entry.strip_prefix(&own) {
        Some(name) => running.iter().any(|r| r == name),
        None => modified.and_then(|m| m.elapsed().ok()).is_none_or(|age| age < STALE_TMP),
    }
}

/// Remove temporary folders no running conversion uses (left by a process that was killed or crashed).
fn sweep_tmp(dir: &Path) {
    // (the web build has no process ids and runs no converter)
    if cfg!(target_arch = "wasm32") {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let running = IN_FLIGHT.lock().unwrap_or_else(PoisonError::into_inner).clone();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with(TMP_PREFIX) && !tmp_in_use(&name, e.metadata().and_then(|m| m.modified()).ok(), &running) {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// Convert the NEF at `nef` (content hash `content_hash`) with `converter` into the cache, unless its DNG is there
/// already → the cached DNG. Waits at most `timeout` and stops the converter when `cancel` is set. The DNG is checked
/// (a DNG this build decodes) before it enters the cache.
pub fn convert(converter: &Path, nef: &Path, content_hash: &str, timeout: Duration, cancel: &AtomicBool) -> Result<PathBuf, String> {
    let target = dng_path(nef, content_hash).ok_or_else(|| format!("{}: not a NEF with a content hash, or no cache folder", nef.display()))?;
    if target.is_file() {
        return Ok(target);
    }
    let name = target.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let _claim = InFlight::claim(&name).ok_or_else(|| format!("{}: already being converted", nef.display()))?;
    let dir = target.parent().ok_or("no cache folder")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    sweep_tmp(dir);
    let tmp = dir.join(tmp_name(&name));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let r = run_into(converter, nef, &tmp, &name, timeout, cancel).and_then(|out| {
        // (another process may have converted the same file meanwhile: its DNG is as good)
        if target.is_file() {
            return Ok(target.clone());
        }
        match std::fs::rename(&out, &target) {
            Ok(()) => Ok(target.clone()),
            Err(_) if target.is_file() => Ok(target.clone()),
            Err(e) => Err(format!("{}: {e}", target.display())),
        }
    });
    let _ = std::fs::remove_dir_all(&tmp);
    r
}

/// Run the converter on `nef` into `tmp` as `name` and check what it wrote → the DNG's path in `tmp`.
fn run_into(converter: &Path, nef: &Path, tmp: &Path, name: &str, timeout: Duration, cancel: &AtomicBool) -> Result<PathBuf, String> {
    // the converter wants absolute paths
    let input = std::path::absolute(nef).map_err(|e| format!("{}: {e}", nef.display()))?;
    let mut child = std::process::Command::new(converter)
        .args(command_args(&input, tmp, name))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("Adobe DNG Converter could not be started ({}): {e}", converter.display()))?;
    let start = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("Adobe DNG Converter: {e}"));
            }
        }
        if cancel.load(Ordering::Relaxed) || start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(if cancel.load(Ordering::Relaxed) {
                "the conversion was stopped".to_string()
            } else {
                format!(
                    "Adobe DNG Converter took longer than {} s and was stopped (it may have been waiting in a window of its own, e.g. an update or licence prompt: open it once by itself)",
                    timeout.as_secs()
                )
            });
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    if !status.success() {
        return Err(format!("Adobe DNG Converter failed ({status})"));
    }
    let out = tmp.join(name);
    check_dng(&out)?;
    Ok(out)
}

/// The file at `path` is a DNG this build can decode (header and raw image description), and no larger than
/// [`MAX_DNG`].
pub fn check_dng(path: &Path) -> Result<(), String> {
    let len = std::fs::metadata(path).map_err(|_| "Adobe DNG Converter wrote no DNG".to_string())?.len();
    if len > MAX_DNG {
        return Err(format!("the converted DNG is too large ({len} bytes)"));
    }
    let usize_len = usize::try_from(len).unwrap_or(usize::MAX);
    let _permit = crate::memory::work_gate().acquire(usize_len);
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if lightcraft_raw::probe(&bytes) != Some(lightcraft_raw::RawFormat::Dng) {
        return Err("Adobe DNG Converter wrote a file that is not a DNG".into());
    }
    lightcraft_raw::probe_info(&bytes).map(|_| ()).map_err(|e| format!("the converted DNG can't be read: {e}"))
}

/// One file to convert: its path and content hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    pub path: PathBuf,
    pub content_hash: String,
}

/// Convert several NEFs, [`PARALLEL`] at a time → one result per job, in order. `progress(done)` is called from
/// this thread while they run. A worker that stops unexpectedly fails its file with an error, never the caller.
pub fn convert_all(converter: &Path, jobs: &[Job], timeout: Duration, cancel: &AtomicBool, progress: &dyn Fn(usize)) -> Vec<Result<PathBuf, String>> {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (converter, timeout, cancel, progress);
        jobs.iter().map(|_| Err("Adobe DNG Converter can't run in the web build".to_string())).collect()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use std::sync::atomic::AtomicUsize;
        let next = AtomicUsize::new(0);
        let done = AtomicUsize::new(0);
        let results: Vec<Mutex<Option<Result<PathBuf, String>>>> = jobs.iter().map(|_| Mutex::new(None)).collect();
        let worker = || {
            loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(job) = jobs.get(i) else { break };
                let r = if cancel.load(Ordering::Relaxed) {
                    Err("the conversion was stopped".to_string())
                } else {
                    convert(converter, &job.path, &job.content_hash, timeout, cancel)
                };
                if let Some(slot) = results.get(i) {
                    *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(r);
                }
                done.fetch_add(1, Ordering::Relaxed);
            }
        };
        std::thread::scope(|sc| {
            let handles: Vec<_> = (0..PARALLEL.min(jobs.len()))
                .filter_map(|i| std::thread::Builder::new().name(format!("lc-dng-{i}")).spawn_scoped(sc, worker).ok())
                .collect();
            if handles.is_empty() {
                // no thread could be started: convert here
                worker();
            }
            while !handles.iter().all(|h| h.is_finished()) {
                progress(done.load(Ordering::Relaxed));
                std::thread::sleep(Duration::from_millis(50));
            }
            // joined here, each one: a worker that panicked is logged and leaves its file unconverted (an error
            // below), instead of the scope passing its panic on
            for h in handles {
                if h.join().is_err() {
                    log::warn!("a DNG conversion worker stopped unexpectedly");
                }
            }
        });
        progress(done.load(Ordering::Relaxed));
        results
            .into_iter()
            .map(|m| m.into_inner().unwrap_or_else(PoisonError::into_inner).unwrap_or_else(|| Err("the conversion stopped unexpectedly".to_string())))
            .collect()
    }
}

/// What the cache holds: (DNGs, bytes).
pub fn cache_usage() -> (usize, u64) {
    let Some(dir) = cache_dir() else { return (0, 0) };
    let Ok(entries) = std::fs::read_dir(dir) else { return (0, 0) };
    entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("dng")))
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .fold((0, 0), |(n, b), m| (n + 1, b.saturating_add(m.len())))
}

/// Delete every cached DNG and every temporary folder no running conversion uses → (DNGs deleted, bytes freed, DNGs
/// that could not be deleted). The cache is shared by every library on this machine.
pub fn clear_cache() -> (usize, u64, usize) {
    let Some(dir) = cache_dir() else { return (0, 0, 0) };
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        // (no cache folder yet: nothing to delete)
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (0, 0, 0),
        Err(e) => {
            log::warn!("DNG cache {}: {e}", dir.display());
            return (0, 0, 1);
        }
    };
    let (mut n, mut bytes, mut failed) = (0, 0u64, 0);
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().is_some_and(|x| x.eq_ignore_ascii_case("dng")) {
            let len = e.metadata().map(|m| m.len()).unwrap_or(0);
            match std::fs::remove_file(&path) {
                Ok(()) => {
                    n += 1;
                    bytes = bytes.saturating_add(len);
                }
                Err(e) => {
                    log::warn!("DNG cache: {} could not be deleted: {e}", path.display());
                    failed += 1;
                }
            }
        }
    }
    sweep_tmp(&dir);
    (n, bytes, failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_set_path_wins_and_is_not_replaced_by_a_default() {
        let defaults = vec![PathBuf::from("/a/conv"), PathBuf::from("/b/conv")];
        let only_b = |p: &Path| p == Path::new("/b/conv");
        assert_eq!(resolve(None, &defaults, only_b), Some(PathBuf::from("/b/conv")));
        assert_eq!(resolve(Some(Path::new("")), &defaults, only_b), Some(PathBuf::from("/b/conv")), "an empty path is no path");
        assert_eq!(resolve(Some(Path::new("/b/conv")), &defaults, only_b), Some(PathBuf::from("/b/conv")));
        assert_eq!(resolve(Some(Path::new("/elsewhere/conv")), &defaults, only_b), None, "a set path that's missing isn't swapped for a default");
        assert_eq!(resolve(None, &defaults, |_| false), None);
    }

    #[test]
    fn a_mac_app_bundle_resolves_to_its_program() {
        let app = Path::new("/Apps/Adobe DNG Converter.app");
        let inner = app.join("Contents/MacOS/Adobe DNG Converter");
        let found = |p: &Path| p == inner;
        assert_eq!(resolve(Some(app), &[], found), Some(inner.clone()));
        // a folder that isn't an app bundle isn't looked into
        assert_eq!(resolve(Some(Path::new("/Apps/Converter")), &[], |p: &Path| p.ends_with("Contents/MacOS/Adobe DNG Converter")), None);
    }

    #[test]
    fn the_command_line_uses_adobes_documented_options() {
        let args = command_args(Path::new("/photos/DSC_0001.NEF"), Path::new("/cache/.tmp-1"), "DSC_0001-ab.dng");
        let args: Vec<String> = args.iter().map(|a| a.to_string_lossy().to_string()).collect();
        assert_eq!(args, ["-c", "-p0", "-d", "/cache/.tmp-1", "-o", "DSC_0001-ab.dng", "/photos/DSC_0001.NEF"]);
    }

    #[test]
    fn the_cache_name_follows_the_content_not_the_place() {
        let _serial = TEST_SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
        set_cache_dir(Some(PathBuf::from("/cache")));
        let at = |p: &str, h: &str| dng_path(Path::new(p), h);
        let a = at("/photos/DSC 0001.NEF", "0123abcd");
        let name = a.as_ref().and_then(|d| d.file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        assert!(name.ends_with(".dng") && name.len() == 32 + 4 && !name.starts_with('-'), "{name}");
        assert_eq!(a, at("/photos/DSC 0001.NEF", "0123abcd"));
        assert_ne!(a, at("/photos/DSC 0001.NEF", "0123abce"), "a changed file gets a new DNG");
        assert_eq!(a, at("/elsewhere/DSC 0001.NEF", "0123abcd"), "a moved file keeps its DNG");
        assert_eq!(a, at("/photos/Renamed-2024.nef", "0123abcd"), "a renamed file keeps its DNG");
        set_cache_dir(None);
    }

    #[test]
    fn only_nefs_with_a_hash_have_a_dng() {
        assert!(is_nef_path(Path::new("/x/a.NEF")) && is_nef_path(Path::new("a.nef")));
        assert!(!is_nef_path(Path::new("a.dng")) && !is_nef_path(Path::new("nef")));
        assert_eq!(dng_path(Path::new("/x/a.dng"), "h"), None);
        assert_eq!(dng_path(Path::new("/x/a.nef"), ""), None);
    }

    #[test]
    fn settings_are_off_by_default() {
        let s = Settings::default();
        assert!(!s.auto && s.path.is_none() && s.timeout == DEFAULT_TIMEOUT);
    }

    #[test]
    fn one_file_is_never_claimed_twice() {
        let a = InFlight::claim("claim-test-a.dng");
        assert!(a.is_some());
        assert!(InFlight::claim("claim-test-a.dng").is_none(), "already being converted");
        assert!(InFlight::claim("claim-test-b.dng").is_some(), "another file is free");
        drop(a);
        assert!(InFlight::claim("claim-test-a.dng").is_some(), "free again once the first is done");
    }

    #[test]
    fn temporary_folders_in_use_are_kept() {
        let mine = tmp_name("x.dng");
        assert!(tmp_in_use(&mine, None, &["x.dng".to_string()]), "this process, in flight");
        assert!(!tmp_in_use(&mine, None, &[]), "this process, finished: left over");
        let other = format!("{TMP_PREFIX}1-y.dng");
        let now = std::time::SystemTime::now();
        assert!(tmp_in_use(&other, Some(now), &[]), "another process's recent folder");
        let old = now.checked_sub(STALE_TMP + Duration::from_secs(60)).unwrap();
        assert!(!tmp_in_use(&other, Some(old), &[]), "another process's old folder: left over");
    }

    #[test]
    fn the_cache_dir_is_absolute() {
        let _serial = TEST_SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
        set_cache_dir(Some(PathBuf::from("relative-cache")));
        let d = cache_dir();
        set_cache_dir(None);
        assert!(d.is_some_and(|d| d.is_absolute() && d.ends_with("relative-cache")));
    }
}
