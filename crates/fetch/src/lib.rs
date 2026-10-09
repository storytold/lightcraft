//! Downloading a model's files, only when the user asked for it: from an ordered list of mirrors
//! (the next one when a mirror fails), resuming partial files, with connect and stall timeouts,
//! a size cap, progress and cancellation, and checked (exact size, SHA-256 where pinned) before
//! each file is moved into the model folder. Runs on whatever thread calls [`download`] (the
//! engine uses a background thread; the UI thread never waits for the network). Pure Rust: the
//! HTTPS client is rustls with the RustCrypto provider (see [`http`]).
//!
//! Files are written to `<name>.part` next to their final place and renamed when verified, so a
//! model folder never holds a half-written file under a real name, and an interrupted download
//! resumes from where it stopped. A file that fails its check is deleted, never kept.
//!
//! What to download and where from belongs to the caller: it passes the [`FileSpec`]s and the
//! default mirrors (see [`mirrors`]). `dac-segment` does so for the SAM 3 weights, which
//! are never part of LightCraft (SAM License, see docs/ai-masks.md).
//!
//! Native only: on wasm32 this crate is empty (the web build downloads no models).

#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

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
pub struct FileSpec<'a> {
    pub name: &'a str,
    /// Exact size in bytes, when pinned (then the download resumes and is checked against it).
    pub size: Option<u64>,
    /// SHA-256 (lower-case hex), when pinned.
    pub sha256: Option<&'a str>,
    /// Largest size accepted when `size` is not pinned.
    pub max: u64,
}

/// Most mirrors used (a hostile list can't make a download loop forever).
const MAX_MIRRORS: usize = 16;
/// Redirects followed per request.
const MAX_REDIRECTS: usize = 8;

/// The mirrors to try, in order: `env`'s (base URLs separated by commas, spaces or newlines), then
/// the mirrors file's (one base URL per line, `#` comments), then `defaults`; duplicates and
/// unusable URLs dropped, at most 16.
pub fn mirrors(env: Option<&str>, file: Option<&Path>, defaults: &[&str]) -> Vec<String> {
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
    defaults.iter().for_each(|m| add(m));
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
    /// The environment variable holding a bearer token, sent to each configured mirror's own host (only over https,
    /// never to a host a redirect leads to), so list only mirrors that may see it. `None`: nothing is ever sent, so one model's token cannot leak to another's server.
    pub token_env: Option<&'static str>,
}

impl Default for Options {
    fn default() -> Self {
        Options { connect_timeout: Duration::from_secs(15), stall_timeout: Duration::from_secs(30), attempts: 3, token_env: None }
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
    /// Invalid filename, size, hash or aggregate bound supplied by the caller.
    Invalid(String),
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
            DownloadError::Invalid(e) => write!(f, "invalid download: {e}"),
            DownloadError::NoMirrors => write!(f, "no download location is configured for this model in this build"),
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

/// Fetch a single file from its exact URL, preserving any query string. The local filename
/// remains `file.name`; it never comes from redirects or response headers.
pub fn download_url(
    file: &FileSpec<'_>,
    url: &str,
    dir: &Path,
    opts: &Options,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&Progress),
) -> Result<(), DownloadError> {
    download_source(std::slice::from_ref(file), &[url.to_string()], dir, opts, cancel, progress, true)
}

/// Download `files` into `dir` from `mirrors` (in order; see [`mirrors`]). Verified files
/// are kept. `progress` is called as bytes arrive (often: throttle in it).
pub fn download(
    files: &[FileSpec<'_>],
    mirrors: &[String],
    dir: &Path,
    opts: &Options,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&Progress),
) -> Result<(), DownloadError> {
    download_source(files, mirrors, dir, opts, cancel, progress, false)
}

fn download_source(
    files: &[FileSpec<'_>],
    mirrors: &[String],
    dir: &Path,
    opts: &Options,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&Progress),
    exact_urls: bool,
) -> Result<(), DownloadError> {
    let total = validate_files(files)?;
    if mirrors.len() > MAX_MIRRORS {
        return Err(DownloadError::Invalid("too many mirrors".into()));
    }
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
    let mut before: u64 = files.iter().filter(|f| !todo.contains(f)).filter_map(|f| f.size).sum();
    for f in todo {
        let mut errors = Vec::new();
        let mut ok = false;
        'mirrors: for m in mirrors {
            let url = if exact_urls { m.clone() } else { format!("{m}/{}", f.name) };
            for _ in 0..opts.attempts.clamp(1, 16) {
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
                        log::warn!("download of {}: {e} (retrying)", short(&url));
                        errors.push(format!("{}: {e}", short(m)));
                    }
                    Err(Fail::NextMirror(e)) => {
                        log::warn!("download of {}: {e}", short(&url));
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

/// Validate before touching disk, including names supplied by a user model catalog.
fn validate_files(files: &[FileSpec<'_>]) -> Result<u64, DownloadError> {
    if files.len() > 256 {
        return Err(DownloadError::Invalid("too many files".into()));
    }
    let mut seen = std::collections::HashSet::new();
    let mut total = 0u64;
    for f in files {
        let stem = f.name.split('.').next().unwrap_or_default().to_ascii_uppercase();
        let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || stem
                .strip_prefix("COM")
                .or_else(|| stem.strip_prefix("LPT"))
                .is_some_and(|n| matches!(n, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"));
        if reserved
            || f.name.ends_with(['.', ' '])
            || f.name.ends_with(".part")
            || f.name.is_empty()
            || f.name.len() > 255
            || f.name.starts_with('.')
            || f.name.contains(['/', '\\', ':'])
            || f.name.chars().any(char::is_control)
            || !seen.insert(f.name)
        {
            return Err(DownloadError::Invalid("a filename must be a unique, ordinary filename".into()));
        }
        if f.max == 0
            || f.size.is_some_and(|n| n == 0 || n > f.max)
            || f.sha256.is_some_and(|h| h.len() != 64 || !h.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
        {
            return Err(DownloadError::Invalid("invalid file size or SHA-256".into()));
        }
        total = total.checked_add(f.size.unwrap_or(0)).ok_or_else(|| DownloadError::Invalid("total size overflow".into()))?;
    }
    Ok(total)
}

/// The bearer token to send to `url`: only a non-empty one, only over https, and only to the host the download began
/// at (`origin`), never to the host a redirect leads to.
fn bearer<'a>(token: Option<&'a str>, url: &Url, origin: &str) -> Option<&'a str> {
    token.map(str::trim).filter(|t| !t.is_empty() && url.tls && url.host == origin)
}

/// A mirror for messages (no query string: it may hold a token).
fn short(m: &str) -> String {
    Url::parse(m).map(|u| u.display()).unwrap_or_else(|_| "mirror".into())
}

/// Whether `path` is already the right file (exact size and hash when pinned).
fn verified(f: &FileSpec<'_>, path: &Path) -> Result<bool, DownloadError> {
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
fn fetch_file(f: &FileSpec<'_>, url: &str, dir: &Path, opts: &Options, cancel: &AtomicBool, report: &mut dyn FnMut(u64, u64)) -> Result<(), Fail> {
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
            if let Some(t) = bearer(opts.token_env.and_then(|name| std::env::var(name).ok()).as_deref(), &url, &origin) {
                headers.push(("Authorization", format!("Bearer {t}")));
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
