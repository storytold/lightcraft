//! Import from a camera or phone that doesn't mount as a disk (PTP/MTP, read through
//! `lightcraft-cameras`): Lightroom's Import from Device for cameras that only talk PTP (issue #1005).
//!
//! The import review and the copy/move pipeline work on files, so a camera's photos are first
//! **fetched** into a staging folder (`<temp>/lightcraft-camera-import/<camera>/`), mirroring the
//! card's folders; the review then opens on them with Move checked, so each one leaves the staging
//! folder once it is verified and catalogued (`import_move`). The folder is outside the library on
//! purpose: a file already inside the library is added where it is, never moved (a library can be
//! a whole drive, so "next to it" isn't always possible). Only what the library doesn't have yet is
//! fetched:
//!
//! - **Already in the library:** a photo with the same file name (any case), the same byte size and
//!   a capture time within [`SAME_SHOT`] of the camera's date for the file is skipped without
//!   reading it. Cameras date a file when it is written, which can be a couple of seconds after
//!   the EXIF capture time (an X100T: 09:17:10 for a shot captured at 09:17:08). Files the camera
//!   doesn't date are fetched, and the review's content check marks any duplicate among them.
//! - **Already staged** (an earlier fetch that was cancelled, or a review that was closed): reused
//!   when the staging folder's record ([`MANIFEST`]) says it came from this camera, at this path,
//!   with the size and date the camera reports for it now, so a second try doesn't read the card
//!   again. Without such a record (no date from the camera, another camera with the same name, a
//!   different shot under the same name and size) it is read again. Anything else in the
//!   staging folder (files no longer on the camera, or now in the library) is removed, except a
//!   file a library photo still uses: a review set to Add (in place) catalogs the staged file
//!   itself, so it is that photo's original.
//!
//! Every file is written as `<name>.part`, flushed to disk, checked against the size the camera
//! reported, and only then renamed; a failed file leaves nothing behind and the others carry on.
//! Names and folders come from the device, so they are made safe path components first. Nothing
//! on the camera is ever changed (the session is read-only).

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use lightcraft_cameras::{CameraFile, Error};
use serde::Serialize;

/// The most a camera's date for a file may differ from the photo's capture time for the two to
/// count as one shot (with the same name and size): cameras date files when they are written.
pub const SAME_SHOT: i64 = 120;

/// The staging folders' parent, in the system's temp folder.
pub const STAGING: &str = "lightcraft-camera-import";

/// In each staging folder: one JSON line per staged file, written once the file is complete
/// (`{"camera", "path", "size", "created"}`), so a later fetch reuses only what it can tell is the
/// same file from the same camera.
pub const MANIFEST: &str = ".lightcraft-staged";

/// A record larger than this is not read (it would take a card of millions of files).
const MANIFEST_MAX: u64 = 64 << 20;

/// What the record says about one staged file.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, Serialize)]
struct StagedFile {
    camera: String,
    /// The file's path in the staging folder, `/`-joined.
    path: String,
    size: u64,
    created: String,
}

/// `path` relative to `dir`, `/`-joined (`None` outside it).
fn rel(dir: &Path, path: &Path) -> Option<String> {
    let r = path.strip_prefix(dir).ok()?;
    let parts: Vec<String> = r.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect();
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// The staging folder's record, by path (unreadable or damaged lines are skipped: those files are
/// read again).
fn read_manifest(dir: &Path) -> std::collections::HashMap<String, StagedFile> {
    let path = dir.join(MANIFEST);
    if std::fs::metadata(&path).map_or(true, |m| !m.is_file() || m.len() > MANIFEST_MAX) {
        return Default::default();
    }
    let Ok(text) = std::fs::read_to_string(&path) else { return Default::default() };
    text.lines().filter_map(|l| serde_json::from_str::<StagedFile>(l).ok()).map(|e| (e.path.clone(), e)).collect()
}

/// Rewrite the record with just `entries` (written next to it, then renamed over it).
fn write_manifest(dir: &Path, entries: &[&StagedFile]) {
    let mut text = String::new();
    for e in entries {
        if let Ok(line) = serde_json::to_string(e) {
            text.push_str(&line);
            text.push('\n');
        }
    }
    let tmp = dir.join(format!("{MANIFEST}.tmp"));
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, dir.join(MANIFEST));
    }
}

/// Add one complete file to the record (best effort: without it the file is only read again).
fn record(dir: &Path, e: &StagedFile) {
    let Ok(line) = serde_json::to_string(e) else { return };
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join(MANIFEST)) {
        let _ = writeln!(f, "{line}").and_then(|_| f.sync_data());
    }
}

/// The record for `f` staged at `path` from `camera`, when the camera dates the file (without a
/// date a different shot of the same name and size can't be told apart).
fn identity(camera: &str, dir: &Path, f: &CameraFile, path: &Path) -> Option<StagedFile> {
    Some(StagedFile { camera: camera.to_string(), path: rel(dir, path)?, size: f.size?, created: f.created.clone()? })
}

/// What a camera can do for an import: [`lightcraft_cameras::Session`], or a fake in tests.
pub trait Source {
    fn files(&self, cancel: &AtomicBool, found: &mut dyn FnMut(usize)) -> Result<Vec<CameraFile>, Error>;
    fn copy_to(&self, file: &CameraFile, out: &mut dyn Write, cancel: &AtomicBool, progress: &mut dyn FnMut(u64)) -> Result<u64, Error>;
}

impl Source for lightcraft_cameras::Session {
    fn files(&self, cancel: &AtomicBool, found: &mut dyn FnMut(usize)) -> Result<Vec<CameraFile>, Error> {
        lightcraft_cameras::Session::files(self, cancel, found)
    }
    fn copy_to(&self, file: &CameraFile, out: &mut dyn Write, cancel: &AtomicBool, progress: &mut dyn FnMut(u64)) -> Result<u64, Error> {
        lightcraft_cameras::Session::copy_to(self, file, out, cancel, progress)
    }
}

/// The library's photos as (file name in lower case, byte size, capture time in seconds), for
/// [`Known::has`]. Photos in Recently Deleted and browsed (Local) photos don't count: importing
/// their file again is how a deleted photo is restored.
#[derive(Default)]
pub struct Known {
    photos: HashSet<(String, u64)>,
    times: std::collections::HashMap<(String, u64), Vec<i64>>,
    /// Staging-folder files that library photos use as their originals (added in place): never
    /// cleaned away. As stored and, when they exist, with links resolved (macOS's temp folder is
    /// under `/var`, a link to `/private/var`).
    in_use: HashSet<PathBuf>,
}

impl Known {
    pub fn of(catalog: &lightcraft_catalog::Catalog) -> Known {
        let mut k = Known::default();
        for p in catalog.photos() {
            if !p.deleted && !p.local {
                k.add(&p.file_name, p.file_size, p.captured.as_deref());
            }
            // every photo that has its file in a staging folder, also in Recently Deleted (restoring
            // it needs the file) and browsed ones
            if let lightcraft_catalog::Source::File { path } = &p.source
                && Path::new(path).components().any(|c| c.as_os_str() == STAGING)
            {
                k.uses(Path::new(path));
            }
        }
        k
    }

    /// A library photo uses the file at `path` as its original.
    pub fn uses(&mut self, path: &Path) {
        if let Ok(real) = path.canonicalize() {
            self.in_use.insert(real);
        }
        self.in_use.insert(path.to_path_buf());
    }

    /// A library photo uses the file at `path` (as given or with links resolved).
    fn in_use(&self, path: &Path) -> bool {
        self.in_use.contains(path) || path.canonicalize().is_ok_and(|real| self.in_use.contains(&real))
    }

    pub fn add(&mut self, file_name: &str, size: u64, captured: Option<&str>) {
        let key = (file_name.to_lowercase(), size);
        if let Some(t) = captured.and_then(lightcraft_cameras::local_seconds) {
            self.times.entry(key.clone()).or_default().push(t);
        }
        self.photos.insert(key);
    }

    /// The library has this camera file: same name and size, and a capture time within
    /// [`SAME_SHOT`] of the camera's date. Without a size or a date on either side, it doesn't
    /// (the file is fetched, and the review's content check decides).
    pub fn has(&self, f: &CameraFile) -> bool {
        let (Some(size), Some(when)) = (f.size, f.created.as_deref().and_then(lightcraft_cameras::local_seconds)) else {
            return false;
        };
        let key = (f.name.to_lowercase(), size);
        self.photos.contains(&key) && self.times.get(&key).is_some_and(|ts| ts.iter().any(|t| t.abs_diff(when) <= SAME_SHOT as u64))
    }
}

/// A fetch in progress, for its activity row (read from another thread).
#[derive(Default)]
pub struct Progress {
    pub cancel: Arc<AtomicBool>,
    /// Files listed on the camera so far.
    pub listed: AtomicU64,
    /// Bytes to fetch and fetched (the reused and skipped files aren't counted).
    pub bytes_total: AtomicU64,
    pub bytes_done: AtomicU64,
    /// Files to fetch and fetched.
    pub files_total: AtomicU64,
    pub files_done: AtomicU64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Failure {
    pub name: String,
    pub error: String,
}

/// What a fetch did.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Fetched {
    /// The staged files to review, in the camera's order (fetched now, or reused).
    pub staged: Vec<String>,
    pub fetched: usize,
    pub reused: usize,
    /// Skipped: the library already has them.
    pub known: usize,
    /// Skipped: not a photo or video LightCraft imports (the camera's database files, …).
    pub unsupported: usize,
    pub failed: Vec<Failure>,
    /// Bytes read from the camera.
    pub bytes: u64,
    /// The staging folder.
    pub folder: String,
}

/// Where a camera's photos are staged: `<temp>/lightcraft-camera-import/<camera>`.
pub fn staging_dir(camera: &str) -> PathBuf {
    std::env::temp_dir().join(STAGING).join(safe_component(camera))
}

/// A device-supplied name as one safe file name: no separators, no characters Windows forbids, no
/// control characters, no trailing dots or spaces, not `.`/`..` or a reserved device name, at most
/// 120 characters. Never empty.
pub fn safe_component(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') { '_' } else { c })
        .take(120)
        .collect();
    while s.ends_with(['.', ' ']) {
        s.pop();
    }
    let s = s.trim_start().to_string();
    let stem = s.split('.').next().unwrap_or_default().to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4 && (stem.starts_with("COM") || stem.starts_with("LPT")) && stem.as_bytes().get(3).is_some_and(u8::is_ascii_digit));
    if s.is_empty() || s == "." || s == ".." {
        "_".into()
    } else if reserved {
        format!("_{s}")
    } else {
        s
    }
}

/// Where camera file `f` is staged under `dir`: its folders (each made safe, at most
/// [`lightcraft_cameras::MAX_DEPTH`]) and name.
pub fn staged_path(dir: &Path, f: &CameraFile) -> PathBuf {
    let mut p = dir.to_path_buf();
    for part in f.folder.split(['/', '\\']).filter(|s| !s.is_empty()).take(lightcraft_cameras::MAX_DEPTH) {
        p.push(safe_component(part));
    }
    p.push(safe_component(&f.name));
    p
}

/// Fetch what the library doesn't have from `source` (camera `camera`) into `dir` (see the module
/// docs).
pub fn fetch(source: &dyn Source, camera: &str, known: &Known, dir: &Path, progress: &Progress) -> Result<Fetched, Error> {
    let cancel = progress.cancel.clone();
    let files = source.files(&cancel, &mut |n| progress.listed.store(n as u64, Ordering::Relaxed))?;
    let mut out = Fetched { folder: dir.to_string_lossy().to_string(), ..Default::default() };
    let mut wanted: Vec<(CameraFile, PathBuf)> = Vec::new();
    let mut seen_paths = HashSet::new();
    for f in files {
        if !crate::import::is_supported(Path::new(&f.name)) {
            out.unsupported += 1;
        } else if known.has(&f) {
            out.known += 1;
        } else {
            let path = staged_path(dir, &f);
            // two device names that sanitize to one path: the first wins, the other is reported
            if seen_paths.insert(path.clone()) {
                wanted.push((f, path));
            } else {
                out.failed.push(Failure { name: f.name.clone(), error: "same name as another file on the camera".into() });
            }
        }
    }
    clean_staging(dir, &seen_paths, known);
    // what can be reused: staged, complete, and recorded as this very file from this camera
    let manifest = read_manifest(dir);
    let reusable: Vec<bool> = wanted
        .iter()
        .map(|(f, p)| identity(camera, dir, f, p).is_some_and(|id| manifest.get(&id.path) == Some(&id)) && already_staged(f, p))
        .collect();
    if dir.is_dir() {
        // keep the record to what is still staged and reusable
        let keep: Vec<&StagedFile> =
            wanted.iter().zip(&reusable).filter(|(_, r)| **r).filter_map(|((_, p), _)| rel(dir, p).and_then(|r| manifest.get(&r))).collect();
        write_manifest(dir, &keep);
    }
    let to_fetch: Vec<&(CameraFile, PathBuf)> = wanted.iter().zip(&reusable).filter(|(_, r)| !**r).map(|(w, _)| w).collect();
    progress.files_total.store(to_fetch.len() as u64, Ordering::Relaxed);
    progress.bytes_total.store(to_fetch.iter().filter_map(|(f, _)| f.size).sum(), Ordering::Relaxed);
    for ((f, path), reuse) in wanted.iter().zip(reusable) {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        if reuse {
            out.reused += 1;
            out.staged.push(path.to_string_lossy().to_string());
            continue;
        }
        let before = progress.bytes_done.load(Ordering::Relaxed);
        match fetch_one(source, f, path, &cancel, &mut |n| progress.bytes_done.store(before.saturating_add(n), Ordering::Relaxed)) {
            Ok(n) => {
                if let Some(id) = identity(camera, dir, f, path) {
                    record(dir, &id);
                }
                out.fetched += 1;
                out.bytes = out.bytes.saturating_add(n);
                out.staged.push(path.to_string_lossy().to_string());
            }
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(e) => out.failed.push(Failure { name: f.name.clone(), error: e.to_string() }),
        }
        progress.bytes_done.store(before.saturating_add(f.size.unwrap_or(0)), Ordering::Relaxed);
        progress.files_done.fetch_add(1, Ordering::Relaxed);
    }
    Ok(out)
}

/// A complete earlier copy is staged at `path` (its size is the camera's).
fn already_staged(f: &CameraFile, path: &Path) -> bool {
    f.size.is_some_and(|size| std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() == size))
}

/// Copy one file: into `<path>.part`, synced, size-checked, then renamed into place.
fn fetch_one(source: &dyn Source, f: &CameraFile, path: &Path, cancel: &AtomicBool, progress: &mut dyn FnMut(u64)) -> Result<u64, Error> {
    let write = |e: std::io::Error| Error::Write(e.to_string());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(write)?;
    }
    let mut part = path.as_os_str().to_owned();
    part.push(".part");
    let part = PathBuf::from(part);
    let r = (|| {
        let mut file = std::fs::File::create(&part).map_err(write)?;
        let n = {
            let mut buf = std::io::BufWriter::with_capacity(1 << 20, &mut file);
            let n = source.copy_to(f, &mut buf, cancel, progress)?;
            buf.flush().map_err(write)?;
            n
        };
        file.sync_all().map_err(write)?;
        if let Some(size) = f.size
            && n != size
        {
            return Err(Error::Device(format!("read {n} of {size} bytes")));
        }
        drop(file);
        std::fs::rename(&part, path).map_err(write)?;
        Ok(n)
    })();
    if r.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    r
}

/// Remove what an earlier fetch left in `dir` that isn't wanted now (leftover `.part` files, files
/// no longer on the camera or now in the library), then empty folders. Only regular files inside
/// `dir` are touched, and only when `dir` is a staging folder of ours
/// (`lightcraft-camera-import/<camera>`): never a folder someone pointed it at by mistake. A file a
/// library photo uses (added in place) stays.
fn clean_staging(dir: &Path, keep: &HashSet<PathBuf>, known: &Known) {
    let ours = dir.parent().and_then(|p| p.file_name()).is_some_and(|n| n == STAGING);
    if !ours || !dir.is_dir() {
        return;
    }
    let files = crate::import::expand_within(&[dir.to_string_lossy().to_string()], None, crate::walk::Limits::default()).0;
    let parts = walk_parts(dir);
    for f in files.into_iter().map(PathBuf::from).chain(parts) {
        if f.starts_with(dir) && !keep.contains(&f) && !known.in_use(&f) && f.symlink_metadata().is_ok_and(|m| m.is_file()) {
            let _ = std::fs::remove_file(&f);
        }
    }
    remove_empty_dirs(dir, 0);
}

/// `.part` files under `dir` (the photo walk skips them: not a supported extension).
fn walk_parts(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), 0usize)];
    while let Some((d, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(t) = e.file_type() else { continue };
            if t.is_dir() && depth < lightcraft_cameras::MAX_DEPTH {
                stack.push((p, depth + 1));
            } else if t.is_file() && p.extension().is_some_and(|x| x == "part") {
                out.push(p);
            }
        }
    }
    out
}

fn remove_empty_dirs(dir: &Path, depth: usize) {
    if depth > lightcraft_cameras::MAX_DEPTH {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        if e.file_type().is_ok_and(|t| t.is_dir()) {
            let p = e.path();
            remove_empty_dirs(&p, depth + 1);
            let _ = std::fs::remove_dir(&p); // only succeeds when empty
        }
    }
}

/// Open camera `id` and fetch from it (see [`fetch`]); the session lives on this thread.
pub fn fetch_camera(id: &str, known: &Known, dir: &Path, progress: &Progress) -> Result<Fetched, Error> {
    let session = lightcraft_cameras::Session::open(id)?;
    fetch(&session, id, known, dir, progress)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A camera in memory: files and their bytes; `fail` names a file whose read breaks off.
    struct Fake {
        files: Vec<(CameraFile, Vec<u8>)>,
        fail: Option<String>,
        reads: Mutex<Vec<String>>,
        cancel_after: Option<usize>,
    }

    fn file(folder: &str, name: &str, bytes: usize, created: Option<&str>) -> (CameraFile, Vec<u8>) {
        let data: Vec<u8> = (0..bytes).map(|i| (i * 7 + name.len()) as u8).collect();
        (
            CameraFile {
                object: format!("o-{folder}-{name}"),
                name: name.into(),
                folder: folder.into(),
                size: Some(bytes as u64),
                created: created.map(str::to_string),
            },
            data,
        )
    }

    fn fake(files: Vec<(CameraFile, Vec<u8>)>) -> Fake {
        Fake { files, fail: None, reads: Mutex::new(Vec::new()), cancel_after: None }
    }

    impl Source for Fake {
        fn files(&self, _: &AtomicBool, found: &mut dyn FnMut(usize)) -> Result<Vec<CameraFile>, Error> {
            found(self.files.len());
            Ok(self.files.iter().map(|(f, _)| f.clone()).collect())
        }
        fn copy_to(&self, f: &CameraFile, out: &mut dyn Write, cancel: &AtomicBool, progress: &mut dyn FnMut(u64)) -> Result<u64, Error> {
            let mut reads = self.reads.lock().unwrap();
            reads.push(f.name.clone());
            if self.cancel_after.is_some_and(|n| reads.len() > n) {
                cancel.store(true, Ordering::Relaxed);
                return Err(Error::Cancelled);
            }
            let data = &self.files.iter().find(|(x, _)| x.object == f.object).unwrap().1;
            let take = if self.fail.as_deref() == Some(f.name.as_str()) { data.len() / 2 } else { data.len() };
            out.write_all(&data[..take]).map_err(|e| Error::Write(e.to_string()))?;
            progress(take as u64);
            Ok(take as u64)
        }
    }

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lc-camimport-{tag}-{}", std::process::id())).join(STAGING).join("X100T");
        let _ = std::fs::remove_dir_all(d.parent().unwrap().parent().unwrap());
        d
    }

    fn done(d: &Path) {
        let _ = std::fs::remove_dir_all(d.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn fetches_new_photos_mirroring_the_cards_folders() {
        let dir = temp("new");
        let cam = fake(vec![
            file("DCIM/123_FUJI", "DSCF3340.RAF", 3000, Some("2026-10-11T09:17:04")),
            file("DCIM/123_FUJI", "DSCF3341.JPG", 1000, Some("2026-10-11T09:17:06")),
            file("DCIM/124_FUJI", "DSCF3340.RAF", 2000, None), // same name, other folder
            file("DCIM", "INDEX.DAT", 10, None),
        ]);
        let p = Progress::default();
        let r = fetch(&cam, "cam-1", &Known::default(), &dir, &p).unwrap();
        assert_eq!((r.fetched, r.reused, r.known, r.unsupported, r.failed.len()), (3, 0, 0, 1, 0), "{r:?}");
        assert_eq!(r.staged.len(), 3);
        for (f, data) in cam.files.iter().take(3) {
            let path = staged_path(&dir, f);
            assert_eq!(std::fs::read(&path).unwrap(), *data, "{path:?}");
        }
        assert_eq!(p.files_done.load(Ordering::Relaxed), 3);
        assert_eq!(p.bytes_done.load(Ordering::Relaxed), 6000);
        assert_eq!(p.bytes_total.load(Ordering::Relaxed), 6000);
        done(&dir);
    }

    /// A review set to Add (in place) catalogs the staged file itself. Fetching the camera again
    /// skips that photo (the library has it), and the cleanup must not take its original away.
    #[test]
    fn staged_originals_a_library_photo_uses_are_kept() {
        let dir = temp("inplace");
        let cam = fake(vec![file("DCIM/100", "A.JPG", 16, Some("2026-10-11T09:17:08")), file("DCIM/100", "B.JPG", 16, Some("2026-10-11T09:17:09"))]);
        let r = fetch(&cam, "cam-1", &Known::default(), &dir, &Progress::default()).unwrap();
        let (a, b) = (PathBuf::from(&r.staged[0]), PathBuf::from(&r.staged[1]));
        let mut known = Known::default();
        known.add("A.JPG", 16, Some("2026-10-11T09:17:08"));
        known.add("B.JPG", 16, Some("2026-10-11T09:17:09"));
        known.uses(&a);
        let r = fetch(&cam, "cam-1", &known, &dir, &Progress::default()).unwrap();
        assert_eq!((r.known, r.fetched), (2, 0), "{r:?}");
        assert!(a.is_file(), "the library's original stays");
        assert!(!b.exists(), "a staged copy the library doesn't use is still cleaned away");
        done(&dir);
    }

    /// `Known::of` finds the staged files the catalog uses, also through a link to the temp
    /// folder, and also for a photo in Recently Deleted.
    #[test]
    fn the_catalog_tells_which_staged_files_are_in_use() {
        use lightcraft_catalog::{Op, Photo, PhotoId, Source};
        let dir = temp("catalog");
        std::fs::create_dir_all(dir.join("DCIM")).unwrap();
        let (a, b) = (dir.join("DCIM/A.JPG"), dir.join("DCIM/B.JPG"));
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();
        let mut c = lightcraft_catalog::Catalog::default();
        let elsewhere = std::env::temp_dir().join("A.JPG");
        for (id, path, deleted) in [(1, &a, false), (2, &b, true), (3, &elsewhere, false)] {
            let mut p =
                Photo::new(PhotoId(id), Source::File { path: path.to_string_lossy().into_owned() }, "A.JPG", "JPEG", 8, 8, "2026-10-11T09:17:08");
            p.deleted = deleted;
            c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        }
        let k = Known::of(&c);
        assert!(k.in_use(&a) && k.in_use(&b));
        assert!(k.in_use(&a.canonicalize().unwrap()), "found with links resolved");
        assert!(!k.in_use(&elsewhere), "only files in a staging folder are tracked");
        done(&dir);
    }

    #[test]
    fn photos_already_in_the_library_are_not_read() {
        let dir = temp("known");
        let cam = fake(vec![
            file("DCIM/123_FUJI", "DSCF3345.RAF", 500, Some("2026-10-11T09:17:10")),
            file("DCIM/123_FUJI", "DSCF3346.RAF", 500, Some("2026-10-11T09:20:00")),
        ]);
        let mut known = Known::default();
        // the camera dates the file 2 s after the EXIF capture time; any case of the name
        known.add("dscf3345.raf", 500, Some("2026-10-11T09:17:08"));
        let r = fetch(&cam, "cam-1", &known, &dir, &Progress::default()).unwrap();
        assert_eq!((r.fetched, r.known), (1, 1), "{r:?}");
        assert_eq!(*cam.reads.lock().unwrap(), ["DSCF3346.RAF"]);
        done(&dir);
    }

    #[test]
    fn a_same_name_photo_that_differs_is_fetched() {
        let mut known = Known::default();
        known.add("DSCF0001.RAF", 500, Some("2024-01-01T10:00:00"));
        let (f, _) = file("DCIM", "DSCF0001.RAF", 500, Some("2024-01-01T10:02:01"));
        assert!(!known.has(&f), "a different shot with a reused frame number (121 s apart)");
        let (f, _) = file("DCIM", "DSCF0001.RAF", 501, Some("2024-01-01T10:00:00"));
        assert!(!known.has(&f), "a different size");
        let (f, _) = file("DCIM", "DSCF0001.RAF", 500, None);
        assert!(!known.has(&f), "no camera date: let the review's content check decide");
        let mut no_time = Known::default();
        no_time.add("DSCF0001.RAF", 500, None);
        let (f, _) = file("DCIM", "DSCF0001.RAF", 500, Some("2024-01-01T10:00:00"));
        assert!(!no_time.has(&f), "no capture time in the library");
        let (f, _) = file("DCIM", "DSCF0001.RAF", 500, Some("2024-01-01T09:58:00"));
        assert!(known.has(&f), "within two minutes either way");
    }

    #[test]
    fn a_broken_read_leaves_nothing_and_the_rest_carry_on() {
        let dir = temp("broken");
        let mut cam = fake(vec![
            file("DCIM/100", "A.RAF", 1000, Some("2026-10-11T09:00:00")),
            file("DCIM/100", "B.RAF", 1000, Some("2026-10-11T09:00:01")),
            file("DCIM/100", "C.RAF", 1000, Some("2026-10-11T09:00:02")),
        ]);
        cam.fail = Some("B.RAF".into());
        let r = fetch(&cam, "cam-1", &Known::default(), &dir, &Progress::default()).unwrap();
        assert_eq!(r.fetched, 2);
        assert_eq!(r.failed, [Failure { name: "B.RAF".into(), error: "camera: read 500 of 1000 bytes".into() }]);
        let b = staged_path(&dir, &cam.files[1].0);
        assert!(!b.exists());
        let mut part = b.into_os_string();
        part.push(".part");
        assert!(!PathBuf::from(part).exists(), "no partial file left");
        done(&dir);
    }

    #[test]
    fn a_cancelled_fetch_resumes_without_reading_again() {
        let dir = temp("resume");
        let mut cam = fake(vec![
            file("DCIM/100", "A.RAF", 1000, Some("2026-10-11T09:00:00")),
            file("DCIM/100", "B.RAF", 1000, Some("2026-10-11T09:00:01")),
            file("DCIM/100", "C.RAF", 1000, Some("2026-10-11T09:00:02")),
        ]);
        cam.cancel_after = Some(1);
        assert_eq!(fetch(&cam, "cam-1", &Known::default(), &dir, &Progress::default()), Err(Error::Cancelled));
        assert!(staged_path(&dir, &cam.files[0].0).is_file(), "the finished file stays staged");
        cam.cancel_after = None;
        cam.reads.lock().unwrap().clear();
        let r = fetch(&cam, "cam-1", &Known::default(), &dir, &Progress::default()).unwrap();
        assert_eq!((r.fetched, r.reused), (2, 1), "{r:?}");
        assert_eq!(*cam.reads.lock().unwrap(), ["B.RAF", "C.RAF"]);
        assert_eq!(r.staged.len(), 3);
        done(&dir);
    }

    /// Review finding P2 on #1169: a staged file of the same folder, name and size as a different
    /// shot now on the camera (a review was closed, the card reformatted, a new shot took the
    /// number) must not stand in for it.
    #[test]
    fn a_different_shot_with_the_same_name_and_size_is_read_again() {
        let dir = temp("identity");
        let (mut first, a) = file("DCIM/100", "A.JPG", 16, Some("2026-10-11T09:00:00"));
        let cam = fake(vec![(first.clone(), a.clone())]);
        fetch(&cam, "cam-1", &Known::default(), &dir, &Progress::default()).unwrap();
        // the new shot: same folder, name and size, another object, a later date, other bytes
        first.object = "o-new".into();
        first.created = Some("2026-10-20T15:30:00".into());
        let b: Vec<u8> = a.iter().map(|x| x ^ 0xff).collect();
        let cam = fake(vec![(first.clone(), b.clone())]);
        let r = fetch(&cam, "cam-1", &Known::default(), &dir, &Progress::default()).unwrap();
        assert_eq!((r.fetched, r.reused), (1, 0), "{r:?}");
        assert_eq!(std::fs::read(staged_path(&dir, &first)).unwrap(), b, "the new shot's bytes");
        // and once staged with its record, the same shot is reused
        let r = fetch(&cam, "cam-1", &Known::default(), &dir, &Progress::default()).unwrap();
        assert_eq!((r.fetched, r.reused), (0, 1), "{r:?}");
        done(&dir);
    }

    /// Two cameras with the same name share a staging folder: one's files never stand in for the
    /// other's.
    #[test]
    fn another_camera_with_the_same_name_reads_its_own_files() {
        let dir = temp("othercam");
        let cam = fake(vec![file("DCIM/100", "A.JPG", 16, Some("2026-10-11T09:00:00"))]);
        fetch(&cam, "cam-1", &Known::default(), &dir, &Progress::default()).unwrap();
        cam.reads.lock().unwrap().clear();
        let r = fetch(&cam, "cam-2", &Known::default(), &dir, &Progress::default()).unwrap();
        assert_eq!((r.fetched, r.reused), (1, 0), "{r:?}");
        assert_eq!(*cam.reads.lock().unwrap(), ["A.JPG"]);
        done(&dir);
    }

    /// Without a date from the camera, or with a damaged record, nothing is reused; a damaged
    /// record is no error.
    #[test]
    fn files_that_cant_be_identified_are_read_again() {
        let dir = temp("noid");
        let cam = fake(vec![file("DCIM/100", "A.JPG", 16, None), file("DCIM/100", "B.JPG", 16, Some("2026-10-11T09:00:00"))]);
        fetch(&cam, "cam-1", &Known::default(), &dir, &Progress::default()).unwrap();
        std::fs::write(dir.join(MANIFEST), "{not json\n\u{0}\u{fffd}\n{\"camera\": 1}\n").unwrap();
        cam.reads.lock().unwrap().clear();
        let r = fetch(&cam, "cam-1", &Known::default(), &dir, &Progress::default()).unwrap();
        assert_eq!((r.fetched, r.reused), (2, 0), "{r:?}");
        // the record is rebuilt: the dated file is reused next time, the undated one never
        cam.reads.lock().unwrap().clear();
        let r = fetch(&cam, "cam-1", &Known::default(), &dir, &Progress::default()).unwrap();
        assert_eq!((r.fetched, r.reused), (1, 1), "{r:?}");
        assert_eq!(*cam.reads.lock().unwrap(), ["A.JPG"]);
        done(&dir);
    }

    #[test]
    fn staging_leftovers_are_removed_but_nothing_outside_it() {
        let dir = temp("clean");
        std::fs::create_dir_all(dir.join("DCIM/099_OLD")).unwrap();
        std::fs::write(dir.join("DCIM/099_OLD/GONE.RAF"), b"x").unwrap();
        std::fs::write(dir.join("DCIM/099_OLD/HALF.RAF.part"), b"x").unwrap();
        let outside = dir.parent().unwrap().parent().unwrap().join("Originals");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("KEEP.RAF"), b"x").unwrap();
        let cam = fake(vec![file("DCIM/100", "A.RAF", 10, None)]);
        let r = fetch(&cam, "cam-1", &Known::default(), &dir, &Progress::default()).unwrap();
        assert_eq!(r.fetched, 1);
        assert!(!dir.join("DCIM/099_OLD").exists(), "stale files and their empty folder are gone");
        assert!(outside.join("KEEP.RAF").is_file());
        // a folder that isn't a staging folder is never cleaned
        let other = std::env::temp_dir().join(format!("lc-camimport-notours-{}", std::process::id()));
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("MINE.RAF"), b"x").unwrap();
        clean_staging(&other, &HashSet::new(), &Known::default());
        assert!(other.join("MINE.RAF").is_file());
        let _ = std::fs::remove_dir_all(&other);
        done(&dir);
    }

    #[test]
    fn hostile_device_names_stay_inside_the_staging_folder() {
        let dir = PathBuf::from("staging");
        for (folder, name) in [
            ("../../Windows", "evil.RAF"),
            ("DCIM/..", ".."),
            ("C:\\Users", "a:b*c?.JPG"),
            ("/abs/path", "CON.RAF"),
            ("DCIM", "name. . ."),
            ("DCIM", "\u{0}\u{1}x.RAF"),
            ("", ""),
        ] {
            let (mut f, _) = file(folder, name, 1, None);
            f.name = name.into();
            let p = staged_path(&dir, &f);
            assert!(p.starts_with(&dir), "{p:?}");
            for c in p.strip_prefix(&dir).unwrap().components() {
                let std::path::Component::Normal(s) = c else { panic!("{p:?} has {c:?}") };
                let s = s.to_string_lossy();
                assert!(!s.is_empty() && s != "." && s != ".." && !s.ends_with(['.', ' ']), "{p:?}");
                assert!(!s.contains(['<', '>', ':', '"', '/', '\\', '|', '?', '*']) && !s.chars().any(char::is_control), "{p:?}");
            }
        }
        assert_eq!(safe_component("CON.RAF"), "_CON.RAF");
        assert_eq!(safe_component("com1"), "_com1");
        assert_eq!(safe_component("COMPANY.JPG"), "COMPANY.JPG");
        assert_eq!(safe_component(&"x".repeat(500)).len(), 120);
    }

    #[test]
    fn two_names_that_sanitize_alike_are_not_mixed_up() {
        let dir = temp("collide");
        let cam = fake(vec![file("DCIM", "A?.JPG", 10, None), file("DCIM", "A*.JPG", 20, None)]);
        let r = fetch(&cam, "cam-1", &Known::default(), &dir, &Progress::default()).unwrap();
        assert_eq!(r.fetched, 1);
        assert_eq!(r.failed.len(), 1, "{r:?}");
        done(&dir);
    }

    #[test]
    fn staging_lives_in_temp_outside_any_library() {
        assert_eq!(staging_dir("X100T"), std::env::temp_dir().join(STAGING).join("X100T"));
        assert_eq!(staging_dir("../x"), std::env::temp_dir().join(STAGING).join(".._x"));
    }
}
