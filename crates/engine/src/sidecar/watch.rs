//! Re-reading XMP sidecars that other apps changed while LightCraft runs (issue #3175; the opt-in
//! `rereadChanged` preference). The host lists the `.xmp` files of the library photos' folders every few
//! seconds (on a worker thread: they may be on a network share) and passes the listing to
//! [`Session::scan_sidecars`], which follows each file's (size, mtime), waits until it has held still for
//! [`SETTLE_SECS`] (sync tools write in several steps), and re-reads the photos it belongs to with
//! `photo.readMetadataFromFile {onlyChanged}`: one undo step, never written back to the sidecar.
//!
//! LightCraft's own sidecar writes record their fingerprint ([`Session::note_own_sidecar`]) so they aren't
//! read back. A photo whose own automatic write is still waiting ([`Session::unsaved_sidecars`]) is left
//! alone and reported: what happens when both sides changed is issue #2603's policy.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde_json::{Value, json};

use super::{file_path, sidecar_candidates};
use crate::{Result, Session};

/// How long a sidecar's (size, mtime) must stay the same before it is read.
pub const SETTLE_SECS: f64 = 3.0;

/// At most this many sidecars are read per scan; the rest wait for the next one (a folder of thousands
/// that arrive together mustn't stall a frame).
const MAX_PER_SCAN: usize = 64;

/// A sidecar's size and modification time (µs since the Unix epoch).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Fingerprint {
    size: u64,
    mtime_us: u64,
}

#[derive(Clone, Debug)]
struct Watched {
    /// What the last scan saw, and when (the caller's clock, seconds) it first saw exactly that.
    seen: Fingerprint,
    since: f64,
    /// What was last read, or written by LightCraft itself; `None` for a file that appeared after the first scan.
    handled: Option<Fingerprint>,
    /// A skipped photo (pending write) was already mentioned for this `seen`.
    reported: bool,
}

/// The sidecars being followed, by path.
#[derive(Debug, Default)]
pub(crate) struct SidecarWatch {
    files: HashMap<String, Watched>,
    /// The first scan only records what is there: sidecars that predate it were not changed "while running".
    primed: bool,
}

/// The `.xmp` files in `folders` with their size and modification time (µs since the Unix epoch): the
/// file-system half of `library.sidecarScan` (the app lists on a worker thread and passes `listing`).
/// A folder that can't be listed is skipped, like a missing file.
pub fn list_sidecars(folders: &[String]) -> Vec<(String, u64, u64)> {
    let mut out = Vec::new();
    for folder in folders {
        let Ok(rd) = std::fs::read_dir(folder) else { continue };
        for e in rd.flatten() {
            let path = e.path();
            if e.file_name().to_string_lossy().starts_with('.') || !path.extension().is_some_and(|x| x.eq_ignore_ascii_case("xmp")) {
                continue;
            }
            if let Some(f) = std::fs::metadata(&path).ok().filter(|m| m.is_file()).and_then(|m| fingerprint(&m)) {
                out.push((path.to_string_lossy().to_string(), f.size, f.mtime_us));
            }
        }
    }
    out
}

fn fingerprint(m: &std::fs::Metadata) -> Option<Fingerprint> {
    let mtime = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(Fingerprint { size: m.len(), mtime_us: u64::try_from(mtime.as_micros()).ok()? })
}

/// Seconds on a clock the scans share when the caller gives none (the web has no clock: 0).
pub(crate) fn now_secs() -> f64 {
    #[cfg(not(target_arch = "wasm32"))]
    return std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
    #[cfg(target_arch = "wasm32")]
    0.0
}

impl Session {
    /// The folders whose sidecars [`Session::scan_sidecars`] follows: where the library's photos (not their
    /// virtual copies) live. Empty with the preference off.
    pub fn sidecar_watch_folders(&self) -> Vec<String> {
        if !self.xmp.reread_changed {
            return Vec::new();
        }
        let folders: std::collections::BTreeSet<&str> = self
            .catalog
            .photos()
            .filter(|p| p.copy_of.is_none())
            .filter_map(|p| file_path(p))
            .filter_map(|f| Path::new(f).parent()?.to_str())
            .collect();
        folders.into_iter().map(str::to_string).collect()
    }

    /// Remember a sidecar LightCraft just wrote, so the scan doesn't read it back as another app's change.
    pub(crate) fn note_own_sidecar(&mut self, path: &Path) {
        if !self.xmp.reread_changed {
            return;
        }
        let Some(f) = std::fs::metadata(path).ok().and_then(|m| fingerprint(&m)) else { return };
        let w = Watched { seen: f, since: 0.0, handled: Some(f), reported: false };
        self.sidecar_watch.files.insert(path.to_string_lossy().to_string(), w);
    }

    /// One pass of the sidecar watch over `listing` (path, size, mtime µs), at `at` seconds on the caller's
    /// clock. Reads the photos of the sidecars that changed and then held still for [`SETTLE_SECS`].
    /// → `{updated: [id], unchanged, failed: [{id, error}], skipped: [id] (newly left alone), waiting}`
    pub fn scan_sidecars(&mut self, listing: &[(String, u64, u64)], at: f64) -> Result<Value> {
        let mut result = json!({"updated": [], "unchanged": 0, "failed": [], "skipped": [], "waiting": 0});
        if !self.xmp.reread_changed {
            self.sidecar_watch = SidecarWatch::default();
            return Ok(result);
        }
        let primed = self.sidecar_watch.primed;
        let w = &mut self.sidecar_watch;
        w.primed = true;
        let listed: HashSet<&str> = listing.iter().map(|(p, ..)| p.as_str()).collect();
        w.files.retain(|p, _| listed.contains(p.as_str()));
        let mut due: Vec<&str> = Vec::new();
        let mut waiting = 0;
        for (path, size, mtime_us) in listing {
            let f = Fingerprint { size: *size, mtime_us: *mtime_us };
            let entry =
                w.files.entry(path.clone()).or_insert_with(|| Watched { seen: f, since: at, handled: (!primed).then_some(f), reported: false });
            if entry.seen != f {
                *entry = Watched { seen: f, since: at, handled: entry.handled, reported: false };
            }
            if entry.handled == Some(f) {
                continue;
            }
            if at - entry.since >= SETTLE_SECS {
                due.push(path);
            } else {
                waiting += 1;
            }
        }
        result["waiting"] = json!(waiting);
        due.sort_unstable();
        due.truncate(MAX_PER_SCAN);
        if due.is_empty() {
            return Ok(result);
        }

        // the photos behind the due sidecars: only photos in a due folder are looked at
        let due_set: HashSet<&str> = due.iter().copied().collect();
        let dirs: HashSet<&Path> = due.iter().filter_map(|p| Path::new(p).parent()).collect();
        let mut photos: Vec<(lightcraft_catalog::PhotoId, Vec<&str>)> = Vec::new();
        for p in self.catalog.photos().filter(|p| p.copy_of.is_none()) {
            let Some(orig) = file_path(p).filter(|f| Path::new(f).parent().is_some_and(|d| dirs.contains(d))) else { continue };
            let paths: Vec<&str> =
                sidecar_candidates(orig, self.xmp.naming).filter_map(|c| c.to_str().and_then(|c| due_set.get(c).copied())).collect();
            if !paths.is_empty() {
                photos.push((p.id, paths));
            }
        }
        photos.sort_unstable_by_key(|(id, _)| id.0);

        let mut read_ids = Vec::new();
        let mut skipped = Vec::new();
        // paths with a skipped photo stay due (and are mentioned once)
        let mut held: HashSet<&str> = HashSet::new();
        for (id, paths) in &photos {
            if self.unsaved_sidecars.contains(id) {
                held.extend(paths.iter().copied());
                if paths.iter().any(|p| self.sidecar_watch.files.get(*p).is_some_and(|w| !w.reported)) {
                    skipped.push(id.0);
                }
            } else {
                read_ids.push(id.0);
            }
        }
        for path in &due {
            if let Some(w) = self.sidecar_watch.files.get_mut(*path) {
                if held.contains(path) {
                    w.reported = true;
                } else {
                    // read below, or no photo of the library: either way this version is dealt with
                    w.handled = Some(w.seen);
                }
            }
        }
        result["skipped"] = json!(skipped);
        if read_ids.is_empty() {
            return Ok(result);
        }
        let r = crate::cmd::xmp::read(self, &json!({"ids": read_ids, "onlyChanged": true}))?;
        let ids = |k: &str| r[k].as_array().into_iter().flatten().filter_map(|e| e["id"].as_u64()).collect::<Vec<_>>();
        result["updated"] = json!(ids("read"));
        result["unchanged"] = json!(ids("unchanged").len());
        result["failed"] = r["failed"].clone();
        Ok(result)
    }
}
