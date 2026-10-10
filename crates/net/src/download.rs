//! Streaming a download to a file: resumable (`<dest>.part` plus `Range`), size-capped, and
//! checked against an expected size and SHA-256 before it is renamed into place. A file that
//! fails its check is deleted, never kept under its real name.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use sha2::{Digest, Sha256};

use crate::{Client, NetError};

/// What [`Client::download`] checks and reports.
#[derive(Default)]
pub struct DownloadOptions<'a> {
    /// Exact size in bytes, when known.
    pub size: Option<u64>,
    /// SHA-256 (hex, any case), when known.
    pub sha256: Option<String>,
    /// Refuse anything larger (0: no cap besides `size`).
    pub max_size: u64,
    /// Extra headers (`secret`: dropped on cross-origin redirects).
    pub headers: Vec<(String, String, bool)>,
    pub cancel: Option<Arc<AtomicBool>>,
    /// `(bytes so far, total when known)`.
    pub progress: Option<&'a mut dyn FnMut(u64, Option<u64>)>,
}

/// A finished download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Downloaded {
    pub bytes: u64,
    /// Lower-case hex SHA-256 of the file.
    pub sha256: String,
}

fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".part");
    dest.with_file_name(name)
}

fn io(path: &Path, e: std::io::Error) -> NetError {
    NetError::Io(format!("{}: {e}", path.display()))
}

/// Parse `Content-Range: bytes START-END/TOTAL` → (start, total).
fn content_range(v: &str) -> Option<(u64, Option<u64>)> {
    let rest = v.trim().strip_prefix("bytes")?.trim();
    let (range, total) = rest.split_once('/')?;
    let start = range.split_once('-')?.0.trim().parse().ok()?;
    Some((start, total.trim().parse().ok()))
}

impl Client {
    /// Download `url` to `dest`, resuming a previous `<dest>.part`.
    pub fn download(&self, url: &str, dest: &Path, mut opts: DownloadOptions<'_>) -> Result<Downloaded, NetError> {
        let part = part_path(dest);
        let cap = match (opts.size, opts.max_size) {
            (Some(s), _) => s,
            (None, 0) => u64::MAX,
            (None, m) => m,
        };
        // hash what is already there
        let mut hasher = Sha256::new();
        let mut have = 0u64;
        if let Ok(mut f) = File::open(&part) {
            let mut buf = vec![0u8; 256 * 1024];
            loop {
                let n = f.read(&mut buf).map_err(|e| io(&part, e))?;
                if n == 0 {
                    break;
                }
                hasher.update(buf.get(..n).unwrap_or_default());
                have = have.saturating_add(n as u64);
            }
        }
        if have > cap {
            // left over from something else: start again
            have = 0;
            hasher = Sha256::new();
        }

        let mut req = self.get(url).identity_encoding();
        for (k, v, secret) in &opts.headers {
            req = if *secret { req.secret_header(k, v) } else { req.header(k, v) };
        }
        if let Some(c) = &opts.cancel {
            req = req.cancel(c.clone());
        }
        let done_already = opts.size.is_some_and(|s| s == have && s > 0);
        if have > 0 && !done_already {
            req = req.header("Range", &format!("bytes={have}-"));
        }
        let (mut resp, mut file) = if done_already {
            (None, None)
        } else {
            let resp = req.send()?;
            let append = match resp.status {
                206 if have > 0 => match resp.header("content-range").and_then(content_range) {
                    Some((start, _)) if start == have => true,
                    _ => return Err(NetError::Protocol("server resumed at the wrong offset".into())),
                },
                200 => false,
                416 if have > 0 => {
                    let _ = std::fs::remove_file(&part);
                    return Err(NetError::Protocol("server refused to resume; the partial file was removed, try again".into()));
                }
                code => return Err(NetError::Status { code, reason: resp.reason.clone() }),
            };
            if !append {
                have = 0;
                hasher = Sha256::new();
            }
            if let Some(parent) = part.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent).map_err(|e| io(parent, e))?;
            }
            let file = OpenOptions::new().create(true).write(true).append(append).truncate(!append).open(&part).map_err(|e| io(&part, e))?;
            (Some(resp), Some(file))
        };

        let total = opts.size.or_else(|| {
            let r = resp.as_ref()?;
            let len: u64 = r.header("content-length")?.trim().parse().ok()?;
            Some(len.saturating_add(have))
        });
        if let (Some(t), true) = (total, cap != u64::MAX)
            && t > cap
        {
            return Err(NetError::TooLarge(cap));
        }
        let mut too_large = false;
        if let (Some(resp), Some(file)) = (resp.as_mut(), file.as_mut()) {
            let mut buf = vec![0u8; 256 * 1024];
            loop {
                let n = resp.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                let chunk = buf.get(..n).unwrap_or_default();
                have = have.saturating_add(n as u64);
                if have > cap {
                    too_large = true;
                    break;
                }
                file.write_all(chunk).map_err(|e| io(&part, e))?;
                hasher.update(chunk);
                if let Some(p) = opts.progress.as_mut() {
                    p(have, total);
                }
            }
            if !too_large {
                file.sync_all().map_err(|e| io(&part, e))?;
            }
        }
        drop(file);
        if too_large {
            let _ = std::fs::remove_file(&part);
            return Err(NetError::TooLarge(cap));
        }

        let sha = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect::<String>();
        let fail = |why: String| {
            let _ = std::fs::remove_file(&part);
            Err(NetError::Verify(why))
        };
        if let Some(size) = opts.size
            && size != have
        {
            return fail(format!("expected {size} bytes, got {have}"));
        }
        if let Some(want) = &opts.sha256
            && !want.trim().eq_ignore_ascii_case(&sha)
        {
            return fail("SHA-256 does not match".into());
        }
        std::fs::rename(&part, dest).map_err(|e| io(dest, e))?;
        Ok(Downloaded { bytes: have, sha256: sha })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_content_range() {
        assert_eq!(content_range("bytes 10-99/100"), Some((10, Some(100))));
        assert_eq!(content_range("bytes 10-99/*"), Some((10, None)));
        assert_eq!(content_range("items 1-2/3"), None);
        assert_eq!(part_path(Path::new("/a/b.bin")), PathBuf::from("/a/b.bin.part"));
    }
}
