//! Downloading the SAM 3 model, only when the user asked for it: from an ordered list of mirrors
//! (the next one when a mirror fails), resuming partial files, with connect and stall timeouts,
//! a size cap, progress and cancellation, and checked (exact size, SHA-256 where pinned) before
//! each file is moved into the model folder. Runs on whatever thread calls [`download`] (the
//! engine uses a background thread; the UI thread never waits for the network).
//!
//! Files are written to `<name>.part` next to their final place and renamed when verified, so a
//! model folder never holds a half-written file under a real name, and an interrupted download
//! resumes from where it stopped. A file that fails its check is deleted, never kept.
//!
//! The weights are never part of LightCraft (SAM License, see docs/ai-masks.md); where they are
//! downloaded from is configured by [`DEFAULT_MIRRORS`] and the user's own list.

pub mod http;

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use sha2::{Digest, Sha256};

use http::{HttpError, Limits, Url};

/// One file of the model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileSpec {
    pub name: &'static str,
    /// Exact size in bytes, when pinned (then the download resumes and is checked against it).
    pub size: Option<u64>,
    /// SHA-256 (lower-case hex), when pinned.
    pub sha256: Option<&'static str>,
    /// Largest size accepted when `size` is not pinned.
    pub max: u64,
}

/// The `facebook/sam3` checkpoint files [`crate::Sam3::load`] needs.
///
/// `model.safetensors` is pinned to the official checkpoint (Hugging Face LFS SHA-256). The two
/// tokenizer files are small text files without a pinned hash yet: they are only parsed by the
/// tokenizer (never executed) and capped in size.
// TODO(maintainer): pin `vocab.json` and `merges.txt` (size + SHA-256) when the files are
// uploaded to LightCraft's CDN.
pub const SAM3_FILES: &[FileSpec] = &[
    FileSpec { name: "vocab.json", size: None, sha256: None, max: 16 << 20 },
    FileSpec { name: "merges.txt", size: None, sha256: None, max: 16 << 20 },
    FileSpec {
        name: crate::WEIGHTS_FILE,
        size: Some(SAM3_WEIGHTS_SIZE),
        sha256: Some("6d06f0a5f84e435071fe6603e61d0b4cc7b40e0d39d487cfd4d67d8cc11cc14a"),
        max: SAM3_WEIGHTS_SIZE,
    },
];

/// Size of the official `model.safetensors` (3.44 GB).
pub const SAM3_WEIGHTS_SIZE: u64 = 3_439_938_512;

/// Where LightCraft downloads the model from, in order: each is a base URL, and file `f` is at
/// `<base>/<f>`. **Empty for now**: LightCraft has no download location of its own yet, so the
/// in-app download needs the user's list (`LIGHTCRAFT_SAM3_MIRRORS` or the mirrors file, see
/// [`mirrors`] and docs/ai-masks.md) until this is filled in.
///
/// Hugging Face's `facebook/sam3` can't be a default: it is gated (each user must accept the
/// SAM License there and download with their own token).
// TODO(maintainer): add LightCraft's CDN locations (https, primary first), e.g.
// "https://<cdn-host>/models/sam3/<revision>" — the files there must match `SAM3_FILES`.
pub const DEFAULT_MIRRORS: &[&str] = &[];

/// Environment variable with extra mirrors (base URLs separated by commas, spaces or newlines),
/// tried before the defaults.
pub const MIRRORS_ENV: &str = "LIGHTCRAFT_SAM3_MIRRORS";

/// Most mirrors used (a hostile list can't make a download loop forever).
const MAX_MIRRORS: usize = 16;
/// Redirects followed per request.
const MAX_REDIRECTS: usize = 8;

/// The mirrors to try, in order: the environment variable's, then the mirrors file's (one base
/// URL per line, `#` comments), then [`DEFAULT_MIRRORS`]; duplicates and unusable URLs dropped.
pub fn mirrors(env: Option<&str>, file: Option<&Path>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut add = |s: &str| {
        let s = s.trim().trim_end_matches('/');
        if !s.is_empty() && Url::parse(s).is_ok() && !out.iter().any(|m| m == s) && out.len() < MAX_MIRRORS {
            out.push(s.to_string());
        }
    };
    if let Some(env) = env {
        env.split([',', ' ', '\n', '\t']).for_each(&mut add);
    }
    // a small text file; anything larger isn't a mirror list
    if let Some(text) = file.and_then(|f| std::fs::metadata(f).ok().filter(|m| m.len() < 64 * 1024).and_then(|_| std::fs::read_to_string(f).ok())) {
        text.lines().map(|l| l.split('#').next().unwrap_or_default()).for_each(&mut add);
    }
    DEFAULT_MIRRORS.iter().for_each(|m| add(m));
    out
}

/// Download settings.
#[derive(Clone, Debug)]
pub struct Options {
    /// Longest wait to connect to a mirror.
    pub connect_timeout: Duration,
    /// Longest wait for the next bytes once connected.
    pub stall_timeout: Duration,
    /// Tries per mirror and file (each resumes where the last one stopped).
    pub attempts: u32,
}

impl Default for Options {
    fn default() -> Self {
        Options { connect_timeout: Duration::from_secs(15), stall_timeout: Duration::from_secs(30), attempts: 3 }
    }
}

/// Where a download is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    pub file: String,
    /// Bytes of all files on disk so far, and the total expected (pinned sizes + announced ones).
    pub done: u64,
    pub total: u64,
    /// The mirror in use (without query).
    pub mirror: String,
}

/// Why a download failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadError {
    /// No mirror is configured.
    NoMirrors,
    Cancelled,
    /// A local file problem (folder not writable, disk full…).
    Disk(String),
    /// Every mirror failed for `file`; the reason for each.
    AllMirrorsFailed {
        file: String,
        errors: Vec<String>,
    },
}

impl std::fmt::Display for DownloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DownloadError::NoMirrors => write!(
                f,
                "no download location is configured for the SAM 3 model in this build (set {MIRRORS_ENV}, or put the files in the model folder yourself; see docs/ai-masks.md)"
            ),
            DownloadError::Cancelled => write!(f, "the download was cancelled"),
            DownloadError::Disk(e) => write!(f, "{e}"),
            DownloadError::AllMirrorsFailed { file, errors } => write!(f, "could not download {file}: {}", errors.join("; ")),
        }
    }
}

/// One mirror's failure for one file; `Retry` resumes on the same mirror.
enum Fail {
    Retry(String),
    NextMirror(String),
    Cancelled,
    Disk(String),
}

/// Download `files` into `dir` from `mirrors` (in order; see [`mirrors`]). Files already there
/// and verified are kept. `progress` is called as bytes arrive (often: throttle in it).
pub fn download(
    files: &[FileSpec],
    mirrors: &[String],
    dir: &Path,
    opts: &Options,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&Progress),
) -> Result<(), DownloadError> {
    std::fs::create_dir_all(dir).map_err(|e| DownloadError::Disk(format!("can't create {}: {e}", dir.display())))?;
    // what's left to fetch
    let mut todo = Vec::new();
    for f in files {
        if !verified(f, &dir.join(f.name))? {
            todo.push(f);
        }
    }
    if todo.is_empty() {
        return Ok(());
    }
    if mirrors.is_empty() {
        return Err(DownloadError::NoMirrors);
    }
    let total: u64 = files.iter().map(|f| f.size.unwrap_or(0)).sum();
    let mut before: u64 = files.iter().filter(|f| !todo.contains(f)).filter_map(|f| f.size).sum();
    for f in todo {
        let mut errors = Vec::new();
        let mut ok = false;
        'mirrors: for m in mirrors {
            let url = format!("{m}/{}", f.name);
            for _ in 0..opts.attempts.max(1) {
                if cancel.load(Ordering::Relaxed) {
                    return Err(DownloadError::Cancelled);
                }
                let mut report = |n: u64, announced: u64| {
                    let mirror = Url::parse(m).map(|u| u.display()).unwrap_or_default();
                    progress(&Progress {
                        file: f.name.to_string(),
                        done: before.saturating_add(n),
                        total: total.max(before.saturating_add(announced)),
                        mirror,
                    })
                };
                match fetch_file(f, &url, dir, opts, cancel, &mut report) {
                    Ok(()) => {
                        ok = true;
                        break 'mirrors;
                    }
                    Err(Fail::Cancelled) => return Err(DownloadError::Cancelled),
                    Err(Fail::Disk(e)) => return Err(DownloadError::Disk(e)),
                    Err(Fail::Retry(e)) => {
                        log::warn!("SAM 3 download of {url}: {e} (retrying)");
                        errors.push(format!("{}: {e}", short(m)));
                    }
                    Err(Fail::NextMirror(e)) => {
                        log::warn!("SAM 3 download of {url}: {e}");
                        errors.push(format!("{}: {e}", short(m)));
                        continue 'mirrors;
                    }
                }
            }
        }
        if !ok {
            return Err(DownloadError::AllMirrorsFailed { file: f.name.to_string(), errors });
        }
        before = before.saturating_add(std::fs::metadata(dir.join(f.name)).map(|m| m.len()).unwrap_or(0));
    }
    Ok(())
}

/// A mirror for messages (no query string: it may hold a token).
fn short(m: &str) -> String {
    Url::parse(m).map(|u| u.display()).unwrap_or_else(|_| "mirror".into())
}

/// Whether `path` is already the right file (exact size and hash when pinned).
fn verified(f: &FileSpec, path: &Path) -> Result<bool, DownloadError> {
    let Ok(meta) = std::fs::metadata(path) else { return Ok(false) };
    if !meta.is_file() || meta.len() == 0 {
        return Ok(false);
    }
    if f.size.is_some_and(|s| s != meta.len()) || meta.len() > f.max {
        return Ok(false);
    }
    match f.sha256 {
        Some(want) => Ok(sha256_file(path).map_err(DownloadError::Disk)? == want),
        None => Ok(true),
    }
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf).map_err(|e| format!("{}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        h.update(buf.get(..n).unwrap_or_default());
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

fn part_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.part"))
}

/// Fetch one file from one mirror into `<name>.part` (resuming it when its size is pinned),
/// check it and move it into place.
fn fetch_file(f: &FileSpec, url: &str, dir: &Path, opts: &Options, cancel: &AtomicBool, report: &mut dyn FnMut(u64, u64)) -> Result<(), Fail> {
    let part = part_path(dir, f.name);
    let limit = f.size.unwrap_or(f.max);
    let disk = |e: std::io::Error| Fail::Disk(format!("{}: {e}", part.display()));
    // resume only a pinned file (an unpinned one can't be checked after resuming)
    let mut have = match std::fs::metadata(&part) {
        Ok(m) if f.size.is_some() && m.len() <= limit => m.len(),
        Ok(_) => {
            std::fs::remove_file(&part).map_err(disk)?;
            0
        }
        Err(_) => 0,
    };
    let fetched = f.size != Some(have) || have == 0;
    if fetched {
        let limits = Limits { connect: opts.connect_timeout, stall: opts.stall_timeout, cancel };
        let mut url = Url::parse(url).map_err(|e| Fail::NextMirror(e.to_string()))?;
        let origin = url.host.clone();
        let mut redirects = 0;
        let mut resp = loop {
            let mut headers = vec![];
            if have > 0 {
                headers.push(("Range", format!("bytes={have}-")));
            }
            // a token for the configured host only (never sent on to a redirect's host)
            if let Ok(t) = std::env::var("LIGHTCRAFT_SAM3_TOKEN")
                && !t.trim().is_empty()
                && url.tls
                && url.host == origin
            {
                headers.push(("Authorization", format!("Bearer {}", t.trim())));
            }
            let r = http::get(&url, &headers, &limits).map_err(http_fail)?;
            match r.status {
                301 | 302 | 303 | 307 | 308 => {
                    redirects += 1;
                    if redirects > MAX_REDIRECTS {
                        return Err(Fail::NextMirror("too many redirects".into()));
                    }
                    let loc = r.header("location").ok_or_else(|| Fail::NextMirror("redirect without a location".into()))?;
                    url = url.join(loc).map_err(|e| Fail::NextMirror(e.to_string()))?;
                }
                _ => break r,
            }
        };
        let mut file = match resp.status {
            200 => {
                // the whole file (from the start, also when a range was asked for)
                if resp.content_length().is_some_and(|n| n > limit || f.size.is_some_and(|s| s != n)) {
                    return Err(Fail::NextMirror(format!("the server's file has the wrong size ({:?} bytes)", resp.content_length())));
                }
                have = 0;
                File::create(&part).map_err(disk)?
            }
            206 if have > 0 => {
                let range = resp.header("content-range").unwrap_or_default();
                let (start, total) = parse_content_range(range).ok_or_else(|| Fail::NextMirror(format!("bad Content-Range `{range}`")))?;
                if start != have || total.is_some_and(|t| Some(t) != f.size) {
                    return Err(Fail::NextMirror(format!("the server resumed at the wrong place ({range})")));
                }
                OpenOptions::new().append(true).open(&part).map_err(disk)?
            }
            416 => {
                // the partial file doesn't fit what the server has: start over
                std::fs::remove_file(&part).map_err(disk)?;
                return Err(Fail::Retry("the partial download didn't match; starting over".into()));
            }
            401 | 403 => return Err(Fail::NextMirror(format!("access denied (HTTP {})", resp.status))),
            404 | 410 => return Err(Fail::NextMirror(format!("not found (HTTP {})", resp.status))),
            s if s >= 500 => return Err(Fail::Retry(format!("server error (HTTP {s})"))),
            s => return Err(Fail::NextMirror(format!("HTTP {s}"))),
        };
        let announced = f.size.unwrap_or_else(|| resp.content_length().unwrap_or(0));
        report(have, announced);
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            let n = match resp.read(&mut buf, &limits) {
                Ok(n) => n,
                Err(e) => {
                    let _ = file.flush();
                    if f.size.is_none() {
                        drop(file);
                        let _ = std::fs::remove_file(&part);
                    }
                    return Err(http_fail(e));
                }
            };
            if n == 0 {
                break;
            }
            if have.saturating_add(n as u64) > limit {
                drop(file);
                let _ = std::fs::remove_file(&part);
                return Err(Fail::NextMirror(format!("the server sent more than the expected {limit} bytes")));
            }
            file.write_all(buf.get(..n).unwrap_or_default()).map_err(disk)?;
            have = have.saturating_add(n as u64);
            report(have, announced);
        }
        file.sync_all().map_err(disk)?;
    }
    // checks: exact size, hash
    let len = std::fs::metadata(&part).map_err(disk)?.len();
    if f.size.is_some_and(|s| s != len) {
        return Err(Fail::Retry(format!("incomplete ({len} of {} bytes)", f.size.unwrap_or(0))));
    }
    if len == 0 {
        let _ = std::fs::remove_file(&part);
        return Err(Fail::NextMirror("the server sent an empty file".into()));
    }
    if let Some(want) = f.sha256 {
        let got = sha256_file(&part).map_err(Fail::Disk)?;
        if got != want {
            std::fs::remove_file(&part).map_err(disk)?;
            let why = format!("the file is damaged or not the official one (SHA-256 {got})");
            // a damaged partial file from an earlier run isn't this mirror's fault
            return Err(if fetched { Fail::NextMirror(why) } else { Fail::Retry(why) });
        }
    }
    std::fs::rename(&part, dir.join(f.name)).map_err(disk)?;
    Ok(())
}

fn http_fail(e: HttpError) -> Fail {
    match e {
        HttpError::Cancelled => Fail::Cancelled,
        HttpError::Stalled | HttpError::Io(_) | HttpError::Protocol(_) => Fail::Retry(e.to_string()),
        HttpError::BadUrl(_) | HttpError::Connect(_) | HttpError::Tls(_) => Fail::NextMirror(e.to_string()),
    }
}

/// `bytes a-b/total` → (a, total) (`*` total → None).
fn parse_content_range(v: &str) -> Option<(u64, Option<u64>)> {
    let v = v.trim().strip_prefix("bytes")?.trim();
    let (range, total) = v.split_once('/')?;
    let (a, b) = range.split_once('-')?;
    let (a, b) = (a.trim().parse::<u64>().ok()?, b.trim().parse::<u64>().ok()?);
    if b < a {
        return None;
    }
    let total = match total.trim() {
        "*" => None,
        t => Some(t.parse::<u64>().ok()?),
    };
    Some((a, total))
}

#[cfg(test)]
mod tests;
