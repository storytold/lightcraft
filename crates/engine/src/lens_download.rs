//! Settings ▸ Lens profiles: which lensfun-format database the app uses, and downloading lensfun's own on request.
//!
//! The folder is an app setting (`lens-profiles.json` in the settings folder), not a library one: it applies to every
//! library, the CLI and the MCP server alike. Download fetches lensfun's database (the `data/db` XML files of the
//! lensfun project, CC-BY-SA 3.0, <https://github.com/lensfun/lensfun>) only when the user asks: the address is the
//! pinned constant [`URL`], and nothing is ever bundled with the app or committed. The archive (the project's source
//! tarball, of which only `data/db/*.xml` is kept) is unpacked into `<settings>/lensfun/db`, with bounded sizes and
//! file names checked so nothing is written outside that folder; the old database stays in place until the new one
//! has been read and holds lenses. Failure is a message, never a panic.

// the web build downloads nothing: the download half is unused there
#![cfg_attr(target_arch = "wasm32", allow(dead_code, unused_imports))]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Value, json};

use crate::activity::{Activity, Cancel, Unit};

/// lensfun's database, as the source archive of its main branch (where database updates land).
pub const URL: &str = "https://github.com/lensfun/lensfun/archive/master.tar.gz";
/// Where it comes from, for the attribution line.
pub const ATTRIBUTION: &str = "Lens profiles from the lensfun project (lensfun.github.io), CC-BY-SA 3.0";
/// Settings file name (in the settings folder).
const SETTINGS_FILE: &str = "lens-profiles.json";
/// Folder (in the settings folder) the database is downloaded into.
pub const DOWNLOAD_DIR: &str = "lensfun";

/// Largest archive accepted.
const MAX_ARCHIVE: u64 = 64 << 20;
/// Largest unpacked tar stream.
const MAX_UNPACKED: usize = 256 << 20;
/// Largest single database file kept, and how many at most.
const MAX_FILE: u64 = 8 << 20;
const MAX_FILES: usize = 1000;
/// Tar entries looked at, at most (a hostile archive of empty entries can't loop for long).
const MAX_ENTRIES: usize = 200_000;

/// Where a download stands.
#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Running {
        bytes: u64,
        total: u64,
    },
    /// Installed: `lenses` profiles, in use now.
    Done {
        lenses: usize,
    },
    Failed(String),
    Cancelled,
}

struct Job {
    state: Mutex<State>,
    cancel: Arc<AtomicBool>,
}

impl Job {
    fn set(&self, s: State) {
        *self.state.lock().unwrap_or_else(PoisonError::into_inner) = s;
    }
    fn get(&self) -> State {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

/// A session's lens database settings and download.
pub struct LensSettings {
    /// The settings folder (`None`: nothing is saved and nothing can be downloaded, e.g. the web or a test session).
    pub settings_dir: Option<PathBuf>,
    /// The download address ([`URL`]; tests point it at a local server).
    pub(crate) url: String,
    job: Option<Arc<Job>>,
}

impl Default for LensSettings {
    fn default() -> Self {
        LensSettings { settings_dir: None, url: URL.to_string(), job: None }
    }
}

impl Drop for LensSettings {
    fn drop(&mut self) {
        if let Some(j) = &self.job {
            j.cancel.store(true, Ordering::Relaxed);
        }
    }
}

impl LensSettings {
    /// Use `settings_dir` and apply the folder saved there.
    pub fn load(settings_dir: Option<PathBuf>) -> Self {
        let s = LensSettings { settings_dir, url: URL.to_string(), job: None };
        if let Some(dir) = s.saved_dir() {
            crate::lens_profiles::set_dir(Some(dir));
        }
        s
    }

    /// The folder saved in the settings file.
    pub fn saved_dir(&self) -> Option<PathBuf> {
        let file = self.settings_dir.as_ref()?.join(SETTINGS_FILE);
        let text = std::fs::read_to_string(file).ok().filter(|t| t.len() < 64 << 10)?;
        let v: Value = serde_json::from_str(&text).ok()?;
        v.get("dir").and_then(Value::as_str).filter(|d| !d.is_empty()).map(PathBuf::from)
    }

    /// Where Download puts the database.
    pub fn download_target(&self) -> Option<PathBuf> {
        self.settings_dir.as_ref().map(|d| d.join(DOWNLOAD_DIR).join("db"))
    }

    /// Save `dir` (or none) as the folder and use it now.
    pub fn set_dir(&self, dir: Option<PathBuf>) -> Result<(), String> {
        save(self.settings_dir.as_deref(), dir.as_deref())?;
        crate::lens_profiles::set_dir(dir);
        Ok(())
    }

    pub fn download_state(&self) -> Option<State> {
        self.job.as_ref().map(|j| j.get())
    }

    pub fn running(&self) -> bool {
        matches!(self.download_state(), Some(State::Running { .. }))
    }

    /// Start downloading lensfun's database (a re-download when there is one); it is used as soon as it is in.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn start_download(&mut self, activity: &Activity) -> Result<(), String> {
        if self.running() {
            return Err("the lens database is already downloading".into());
        }
        let settings = self.settings_dir.clone().ok_or("there is no settings folder to download the lens database into")?;
        let cancel = Arc::new(AtomicBool::new(false));
        let task = activity.start("download", "Downloading lens database", Cancel::Flag(cancel.clone()));
        task.set_unit(Unit::Bytes);
        task.detail("lensfun");
        let job = Arc::new(Job { state: Mutex::new(State::Running { bytes: 0, total: 0 }), cancel });
        let worker = job.clone();
        let url = self.url.clone();
        let handle = task.handle();
        std::thread::Builder::new()
            .name("lens-db-download".into())
            .spawn(move || {
                let _task = task;
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut progress = |bytes: u64, total: u64| {
                        handle.progress(bytes, total);
                        worker.set(State::Running { bytes, total });
                    };
                    install(&url, &settings, &worker.cancel, &mut progress)
                }));
                worker.set(match outcome {
                    Ok(Ok(lenses)) => State::Done { lenses },
                    Ok(Err(_)) if worker.cancel.load(Ordering::Relaxed) => State::Cancelled,
                    Ok(Err(e)) => State::Failed(e),
                    Err(_) => State::Failed("the download stopped unexpectedly".into()),
                });
            })
            .map_err(|e| format!("could not start the download: {e}"))?;
        self.job = Some(job);
        Ok(())
    }

    #[cfg(target_arch = "wasm32")]
    pub fn start_download(&mut self, _: &Activity) -> Result<(), String> {
        Err("the web build downloads no lens database".into())
    }

    /// Stop a running download (or forget a finished one). `true` if there was one.
    pub fn cancel_download(&mut self) -> bool {
        let Some(j) = &self.job else { return false };
        j.cancel.store(true, Ordering::Relaxed);
        if !self.running() {
            self.job = None;
        }
        true
    }

    /// Wait for a download to end (tests).
    #[cfg(test)]
    pub(crate) fn wait(&self) -> Option<State> {
        let t = std::time::Instant::now();
        while self.running() && t.elapsed() < std::time::Duration::from_secs(30) {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        self.download_state()
    }

    /// Stop using a database; a downloaded one is deleted too. → whether files were deleted.
    pub fn remove(&mut self) -> Result<bool, String> {
        self.cancel_download();
        self.job = None;
        let current = crate::lens_profiles::configured_dir();
        self.set_dir(None)?;
        let target = self.download_target();
        match (current, target) {
            (Some(c), Some(t)) if c == t => {
                let root = t.parent().map(Path::to_path_buf).unwrap_or(t);
                std::fs::remove_dir_all(&root).map_err(|e| format!("could not delete {}: {e}", root.display()))?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// Settings ▸ Lens profiles, as JSON (`lens.db.status`).
    pub fn status(&self) -> Value {
        let source = crate::lens_profiles::source();
        let db = crate::lens_profiles::database();
        let downloaded = matches!((&source, self.download_target()), (Some((d, _)), Some(t)) if *d == t);
        let download = self.download_state().map(|s| match s {
            State::Running { bytes, total } => json!({"state": "running", "bytes": bytes, "total": total}),
            State::Done { lenses } => json!({"state": "done", "lenses": lenses}),
            State::Failed(e) => json!({"state": "failed", "error": e}),
            State::Cancelled => json!({"state": "cancelled"}),
        });
        json!({
            "dir": source.as_ref().map(|(d, _)| d.display().to_string()),
            "origin": source.as_ref().map(|(_, o)| *o),
            "env": lcc_brand::env_var(crate::lens_profiles::ENV),
            "configured": crate::lens_profiles::configured_dir().map(|d| d.display().to_string()),
            "downloaded": downloaded,
            "lenses": db.as_ref().map_or(0, |d| d.lenses.len()),
            "cameras": db.as_ref().map_or(0, |d| d.cameras.len()),
            "updated": source.as_ref().and_then(|(d, _)| newest(d)).map(date),
            "download": download,
            "canDownload": self.settings_dir.is_some() && cfg!(not(target_arch = "wasm32")),
            "attribution": ATTRIBUTION,
        })
    }
}

/// Write the settings file (no settings folder: nothing to save).
fn save(settings_dir: Option<&Path>, dir: Option<&Path>) -> Result<(), String> {
    let Some(s) = settings_dir else { return Ok(()) };
    std::fs::create_dir_all(s).map_err(|e| format!("could not create {}: {e}", s.display()))?;
    let text = json!({"dir": dir.map(|d| d.display().to_string())}).to_string();
    let file = s.join(SETTINGS_FILE);
    std::fs::write(&file, text).map_err(|e| format!("could not save {}: {e}", file.display()))
}

/// Newest modification time (Unix seconds) of the database's XML files.
fn newest(dir: &Path) -> Option<u64> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .take(10_000)
        .filter(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("xml")))
        .filter_map(|e| e.metadata().ok()?.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs()))
        .max()
}

/// `YYYY-MM-DD` (UTC) for Unix seconds.
fn date(secs: u64) -> String {
    // days to civil date (Howard Hinnant's algorithm)
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Download, unpack, check and put in place; then use it. → lenses in the new database.
#[cfg(not(target_arch = "wasm32"))]
fn install(url: &str, settings: &Path, cancel: &AtomicBool, progress: &mut dyn FnMut(u64, u64)) -> Result<usize, String> {
    let root = settings.join(DOWNLOAD_DIR);
    let staging = root.join(".download");
    const NAME: &str = "lensfun-db.tar.gz";
    // always a fresh copy: an Update must not "resume" an older archive
    let _ = std::fs::remove_file(staging.join(NAME));
    let _ = std::fs::remove_file(staging.join(format!("{NAME}.part")));
    let file = lcc_fetch::FileSpec { name: NAME, size: None, sha256: None, max: MAX_ARCHIVE };
    let fetched = lcc_fetch::download_url(&file, url, &staging, &lcc_fetch::Options::default(), cancel, &mut |p| progress(p.done, p.total));
    if let Err(e) = fetched {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(match e {
            lcc_fetch::DownloadError::AllMirrorsFailed { errors, .. } => {
                let detail: String = errors.join("; ").chars().take(300).collect();
                format!(
                    "could not download the lens database ({detail}): check the internet connection and try again, or choose a folder holding lensfun's data/db files"
                )
            }
            other => other.to_string(),
        });
    }
    let bytes = std::fs::read(staging.join(NAME)).map_err(|e| format!("could not read the download: {e}"));
    let _ = std::fs::remove_dir_all(&staging);
    let files = unpack(&bytes?, cancel)?;
    let fresh = root.join("db.new");
    let _ = std::fs::remove_dir_all(&fresh);
    std::fs::create_dir_all(&fresh).map_err(|e| format!("could not create {}: {e}", fresh.display()))?;
    for (name, data) in &files {
        std::fs::write(fresh.join(name), data).map_err(|e| {
            let _ = std::fs::remove_dir_all(&fresh);
            format!("could not save the lens database (is the disk full?): {e}")
        })?;
    }
    let lenses = crate::lens_profiles::load(&fresh).map(|d| d.lenses.len()).unwrap_or(0);
    if lenses == 0 {
        let _ = std::fs::remove_dir_all(&fresh);
        return Err("the download holds no lens profiles: it is not lensfun's database".into());
    }
    if cancel.load(Ordering::Relaxed) {
        let _ = std::fs::remove_dir_all(&fresh);
        return Err("the download was cancelled".into());
    }
    let target = root.join("db");
    let old = root.join("db.old");
    let _ = std::fs::remove_dir_all(&old);
    if target.exists() {
        std::fs::rename(&target, &old).map_err(|e| format!("could not replace the lens database: {e}"))?;
    }
    std::fs::rename(&fresh, &target).map_err(|e| {
        let _ = std::fs::rename(&old, &target);
        format!("could not put the lens database in place: {e}")
    })?;
    let _ = std::fs::remove_dir_all(&old);
    save(Some(settings), Some(&target))?;
    crate::lens_profiles::set_dir(Some(target));
    Ok(lenses)
}

/// The `data/db/*.xml` files of a gzip-compressed tar archive: (file name, contents).
pub fn unpack(gz: &[u8], cancel: &AtomicBool) -> Result<Vec<(String, Vec<u8>)>, String> {
    let tar = gunzip(gz)?;
    if cancel.load(Ordering::Relaxed) {
        return Err("the download was cancelled".into());
    }
    let files = db_files(&tar)?;
    if files.is_empty() {
        return Err("the download holds no lens database files (data/db/*.xml)".into());
    }
    Ok(files)
}

fn gunzip(gz: &[u8]) -> Result<Vec<u8>, String> {
    const BAD: &str = "the download is not a gzip archive (it may be incomplete): try again";
    if gz.get(..3) != Some(&[0x1f, 0x8b, 8][..]) {
        return Err(BAD.into());
    }
    let flags = *gz.get(3).ok_or(BAD)?;
    let mut pos = 10usize;
    if flags & 4 != 0 {
        let n = gz.get(pos..pos + 2).ok_or(BAD)?;
        pos = pos.saturating_add(2).saturating_add(usize::from(u16::from_le_bytes([n[0], n[1]])));
    }
    for bit in [8u8, 16] {
        if flags & bit != 0 {
            let rest = gz.get(pos..).ok_or(BAD)?;
            pos = pos.saturating_add(rest.iter().position(|&b| b == 0).ok_or(BAD)?).saturating_add(1);
        }
    }
    if flags & 2 != 0 {
        pos = pos.saturating_add(2);
    }
    let body = gz.get(pos..).ok_or(BAD)?;
    miniz_oxide::inflate::decompress_to_vec_with_limit(body, MAX_UNPACKED).map_err(|e| {
        if matches!(e.status, miniz_oxide::inflate::TINFLStatus::HasMoreOutput) { "the archive is too large".into() } else { BAD.to_string() }
    })
}

/// A NUL-terminated header field.
fn field(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(b.get(..end).unwrap_or_default()).into_owned()
}

fn octal(b: &[u8]) -> Option<u64> {
    let s = field(b);
    let s = s.trim_matches(|c: char| c == ' ' || c == '\0');
    if s.is_empty() {
        return Some(0);
    }
    u64::from_str_radix(s, 8).ok()
}

/// A database file name kept as is (no folders, no hidden files, nothing a file system could read differently).
fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && name.to_ascii_lowercase().ends_with(".xml")
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// The regular files under a `data/db/` folder in tar stream `tar`, by file name.
fn db_files(tar: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    const BAD: &str = "the archive is damaged (it may be incomplete): try again";
    let mut out: Vec<(String, Vec<u8>)> = Vec::new();
    let mut off = 0usize;
    let mut long_name: Option<String> = None;
    for _ in 0..MAX_ENTRIES {
        let Some(h) = tar.get(off..off.saturating_add(512)) else { break };
        if h.iter().all(|&b| b == 0) {
            break;
        }
        let size = octal(h.get(124..136).ok_or(BAD)?).ok_or(BAD)?;
        let start = off.checked_add(512).ok_or(BAD)?;
        let len = usize::try_from(size).map_err(|_| BAD)?;
        let end = start.checked_add(len).ok_or(BAD)?;
        let data = tar.get(start..end).ok_or(BAD)?;
        let kind = h.get(156).copied().unwrap_or(0);
        let mut name = field(h.get(..100).unwrap_or_default());
        if h.get(257..262) == Some(b"ustar") {
            let prefix = field(h.get(345..500).unwrap_or_default());
            if !prefix.is_empty() {
                name = format!("{prefix}/{name}");
            }
        }
        if let Some(n) = long_name.take() {
            name = n;
        }
        match kind {
            // pax extended header: its `path` names the next entry
            b'x' => {
                let text = String::from_utf8_lossy(data);
                long_name = text.lines().find_map(|l| l.split_once(" path=").map(|(_, p)| p.to_string()));
            }
            // GNU long name
            b'L' => long_name = Some(field(data)),
            b'0' | 0 => {
                let path = name.trim_end_matches('/');
                let mut parts = path.rsplit('/');
                let (file, db, data_dir) = (parts.next().unwrap_or_default(), parts.next(), parts.next());
                if db == Some("db") && data_dir == Some("data") && safe_name(file) {
                    if size > MAX_FILE {
                        return Err(format!("{file} in the archive is too large"));
                    }
                    if out.len() >= MAX_FILES {
                        return Err("the archive holds too many database files".into());
                    }
                    if !out.iter().any(|(n, _)| n.eq_ignore_ascii_case(file)) {
                        out.push((file.to_string(), data.to_vec()));
                    }
                }
            }
            _ => {}
        }
        off = end.checked_add(511).ok_or(BAD)? / 512 * 512;
    }
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A tar entry (ustar header).
    pub(crate) fn entry(name: &str, data: &[u8], kind: u8) -> Vec<u8> {
        let mut h = vec![0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[100..107].copy_from_slice(b"0000644");
        h[124..135].copy_from_slice(format!("{:011o}", data.len()).as_bytes());
        h[156] = kind;
        h[257..263].copy_from_slice(b"ustar\0");
        h[148..156].copy_from_slice(b"        ");
        let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
        h[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
        h.extend_from_slice(data);
        h.resize(h.len().div_ceil(512) * 512, 0);
        h
    }

    pub(crate) fn gzip(tar: &[u8]) -> Vec<u8> {
        let mut v = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 255];
        v.extend(miniz_oxide::deflate::compress_to_vec(tar, 6));
        v.extend([0u8; 8]);
        v
    }

    pub(crate) const DB: &str = r#"<lensdatabase>
      <lens><maker>Testcam</maker><model>Testcam 18-55mm f/3.5-5.6</model><mount>T</mount><cropfactor>1.5</cropfactor>
        <calibration><distortion model="poly3" focal="18" k1="-0.03" /></calibration>
      </lens></lensdatabase>"#;

    /// An archive like lensfun's: a pax header, the database among other files, and hostile names.
    pub(crate) fn archive() -> Vec<u8> {
        let mut tar = Vec::new();
        tar.extend(entry("pax_global_header", b"52 comment=abc\n", b'g'));
        tar.extend(entry("lensfun-master/README.md", b"readme", b'0'));
        tar.extend(entry("lensfun-master/data/db/", b"", b'5'));
        tar.extend(entry("lensfun-master/data/db/slr-testcam.xml", DB.as_bytes(), b'0'));
        tar.extend(entry("lensfun-master/data/db/../../../evil.xml", b"<x/>", b'0'));
        tar.extend(entry("lensfun-master/data/db/.hidden.xml", b"<x/>", b'0'));
        tar.extend(entry("lensfun-master/data/db/link.xml", b"", b'2'));
        tar.extend(entry("lensfun-master/src/lib.c", b"code", b'0'));
        tar.extend([0u8; 1024]);
        gzip(&tar)
    }

    #[test]
    fn only_the_database_files_are_unpacked() {
        let files = unpack(&archive(), &AtomicBool::new(false)).unwrap();
        let names: Vec<_> = files.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["slr-testcam.xml"]);
    }

    #[test]
    fn bad_archives_are_errors_not_panics() {
        let no = AtomicBool::new(false);
        let good = archive();
        assert!(unpack(b"", &no).is_err());
        assert!(unpack(b"<html>not found</html>", &no).is_err());
        // truncated anywhere
        for cut in [5, 11, 40, good.len() / 2, good.len() - 9] {
            let _ = unpack(&good[..cut], &no);
        }
        assert!(unpack(&good[..good.len() / 2], &no).is_err());
        // a tar whose sizes lie
        let mut tar = entry("x/data/db/a.xml", DB.as_bytes(), b'0');
        tar[124..135].copy_from_slice(b"77777777777");
        assert!(unpack(&gzip(&tar), &no).is_err());
        let mut tar = entry("x/data/db/a.xml", DB.as_bytes(), b'0');
        tar[124..130].copy_from_slice(b"zzzzzz");
        assert!(unpack(&gzip(&tar), &no).is_err());
        // no database inside
        assert!(unpack(&gzip(&entry("x/README", b"hi", b'0')), &no).is_err());
        // garbage after a valid header
        for seed in 0..64u8 {
            let mut g = good.clone();
            let i = 10 + (seed as usize * 37) % (g.len() - 10);
            g[i] ^= 0x5a;
            let _ = unpack(&g, &no);
        }
    }

    #[test]
    fn dates() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(1_760_140_800), "2025-10-11");
    }
}
