//! Backing up a library's catalog: a dated folder that opens as the library it was taken of.
//!
//! A backup holds `catalog.snap`, the segments it lists (in `segments/`, as in the library),
//! `catalog.log` and the settings files beside them (not the photos, and not the caches). Its
//! files are a library folder's, so it reads as the catalog as it was; to work in that again it is
//! restored ([`crate::restore`]), not opened where it is, which would change it.
//!
//! **Each backup shares with the one before it the files that didn't change.** A segment is
//! never changed once written (a snapshot writes a changed one under a new name), so a segment
//! of the same name, size and CRC in the previous backup is the same file: it is hard-linked,
//! not copied. A backup of a large library after a day's work copies a few megabytes, and every
//! backup still stands on its own: removing an older one, or the library, takes nothing from it.
//! Where the folder can't hold hard links (another volume than the last backup's, a file system
//! without them), files are copied. A file is only shared once it was read and still has its
//! CRC: damage in an older backup is not handed on to the next (it is copied afresh from the
//! library instead).
//!
//! **A backup is one moment of the library, also while the library is in use.** The files are
//! read as they are on disk, without the journal's help: `catalog.snap` first, then the segments
//! it lists, then the log, then `catalog.snap` again. If it changed meanwhile (the library was
//! compacted), the backup starts over, keeping what it already has. Everything a snapshot lists
//! is checked against the size and CRC it states before it is kept.
//!
//! **A backup says what it holds.** Its marker file, `backup.json`, lists every file with its
//! size and CRC-32 (the segments' are also in `catalog.snap`). [`check`] reads a backup against
//! that list: a file that was edited, damaged, removed or added is named (not what Finder or
//! Explorer write into a folder somebody looked at: `.DS_Store`, `Thumbs.db`, `desktop.ini`, `._…`).
//!
//! **Nothing half-made counts.** A backup is written as `<name>.partial` and renamed when it is
//! whole; a backup that fails or is cancelled removes it. One cut short by the program ending
//! stays as `<name>.partial` (never taken for a backup) and is removed by a later backup to the
//! same folder, once it is an hour old.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::journal::{LOG, SNAPSHOT};
use crate::segments::{self, Segment};

/// The file that marks a finished backup: what it is of, as JSON.
pub const MARKER: &str = "backup.json";
/// A backup being written is `<name>.partial`.
const PARTIAL: &str = ".partial";
/// How often a backup starts over because the library was compacted under it.
const ATTEMPTS: usize = 8;
/// How old a `<name>.partial` folder is before a later backup removes it.
const STALE: std::time::Duration = std::time::Duration::from_secs(3600);

#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("the backup was cancelled")]
    Cancelled,
    #[error("{0:?} can't be the name of a backup")]
    Name(String),
    #[error("there is a backup named {0} already")]
    Exists(String),
    /// The library can't be read as one: no catalog, a damaged or missing segment.
    #[error("the library can't be backed up: {0}")]
    Library(String),
    /// A backup that was checked is not what it was when it was made ([`check`]).
    #[error("the backup is not as it was made: {0}")]
    Changed(String),
    /// A library can't be restored from a backup ([`crate::restore`]).
    #[error("the library can't be restored: {0}")]
    Restore(String),
    #[error("the backup can't be written: {0}")]
    Io(String),
}

/// How to back up.
#[derive(Clone, Copy, Debug)]
pub struct BackupOptions<'a> {
    /// The backup's folder name (the time it is made).
    pub name: &'a str,
    /// Other files of the library folder to copy when they are there (presets, preferences):
    /// plain file names, none of them the catalog's own.
    pub also: &'a [&'a str],
}

/// A backup that was made.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupReport {
    pub dir: PathBuf,
    /// The op the backup's snapshot is of, and the photos that snapshot holds (changes since are
    /// in the backup's log, and not counted here).
    pub seq: u64,
    pub photos: u64,
    pub segments: u64,
    /// Files in the backup; how many were copied, and how many are shared with the backup before.
    pub files: u64,
    pub copied: u64,
    pub linked: u64,
    /// Their size in all, and of the ones copied.
    pub bytes: u64,
    pub copied_bytes: u64,
    /// Every file of the backup but the marker itself, by name: its size and CRC-32.
    #[serde(default)]
    pub contents: BTreeMap<String, Held>,
}

/// A file of a backup, as its marker lists it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Held {
    pub bytes: u64,
    pub crc: u32,
}

/// Follows a backup as it is made, and can stop it.
pub trait Watch {
    /// About to back up `name`; `done` of `total` bytes are.
    fn progress(&self, name: &str, done: u64, total: u64) {
        let _ = (name, done, total);
    }
    fn cancelled(&self) -> bool {
        false
    }
}

impl Watch for () {}

pub(crate) fn io(what: &str, path: &Path, e: std::io::Error) -> BackupError {
    BackupError::Io(format!("{what} {}: {e}", path.display()))
}

/// Counts the entries of a map without keeping them.
struct Count(u64);

impl<'de> serde::Deserialize<'de> for Count {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Entries;
        impl<'de> serde::de::Visitor<'de> for Entries {
            type Value = Count;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("the photos of a catalog")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Count, A::Error> {
                let mut n = 0u64;
                while map.next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?.is_some() {
                    n = n.saturating_add(1);
                }
                Ok(Count(n))
            }
        }
        d.deserialize_map(Entries)
    }
}

#[derive(serde::Deserialize)]
struct InlinePhotos {
    photos: Count,
}

/// What a backup needs to know of a `catalog.snap`.
#[derive(serde::Deserialize)]
struct Head {
    format: String,
    seq: u64,
    #[serde(default)]
    segments: Vec<Segment>,
    #[serde(default)]
    photos: Option<u64>,
    catalog: InlinePhotos,
}

fn read_head(bytes: &[u8]) -> Result<Head, BackupError> {
    let head: Head = serde_json::from_slice(bytes).map_err(|e| BackupError::Library(format!("{SNAPSHOT}: {e}")))?;
    if head.format != crate::journal::FORMAT {
        return Err(BackupError::Library(format!("{SNAPSHOT} is not a LightCraft catalog (format {:?})", head.format)));
    }
    segments::check_list(&head.segments).map_err(|e| BackupError::Library(format!("{SNAPSHOT}: {e}")))?;
    Ok(head)
}

/// A name `options.also` may hold: a plain file name that isn't one of the catalog's or the
/// marker's (those are written by the backup itself, and a segment may be shared with another
/// backup: it is never written over).
pub(crate) fn valid_extra(name: &str) -> bool {
    valid_name(name) && name != SNAPSHOT && name != LOG && name != MARKER && name != segments::SEGMENT_DIR
}

/// The files of the folder `dir` that a backup may hold, by the names the catalog gives them:
/// the ones in it, and the ones in its segments folder (`segments/…`). An error names anything
/// else that is in it (another folder, a link).
pub(crate) fn held(dir: &Path) -> Result<Vec<String>, String> {
    let mut names = Vec::new();
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if regular(&entry.path()) {
            names.push(name);
        } else if name == segments::SEGMENT_DIR && std::fs::symlink_metadata(entry.path()).is_ok_and(|m| m.is_dir()) {
            for inner in std::fs::read_dir(entry.path()).map_err(|e| format!("{name}: {e}"))? {
                let inner = inner.map_err(|e| e.to_string())?;
                let inner_name = format!("{}{}", segments::SEGMENT_PREFIX, inner.file_name().to_string_lossy());
                if !regular(&inner.path()) {
                    return Err(format!("{inner_name} is not a file"));
                }
                names.push(inner_name);
            }
        } else {
            return Err(format!("{name} is not a file"));
        }
    }
    Ok(names)
}

/// Make the folder a file of a backup goes into (the segments folder, with its first file).
pub(crate) fn folder_for(path: &Path) -> Result<(), BackupError> {
    match path.parent() {
        Some(folder) if !folder.is_dir() => std::fs::create_dir_all(folder).map_err(|e| io("can't make the folder", folder, e)),
        _ => Ok(()),
    }
}

pub(crate) fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && name != "."
        && name != ".."
        && !name.ends_with(PARTIAL)
        && !name.chars().any(|c| c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
}

/// A file's bytes; `None` when there is no such file.
pub(crate) fn read(path: &Path) -> Result<Option<Vec<u8>>, BackupError> {
    match std::fs::read(path) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(BackupError::Library(format!("{}: {e}", path.display()))),
    }
}

/// Write a new file and make it durable. Never over one that is there: a file of a backup may
/// be shared with another backup.
pub(crate) fn write(path: &Path, bytes: &[u8]) -> Result<(), BackupError> {
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(path).map_err(|e| io("can't create", path, e))?;
    f.write_all(bytes).and_then(|()| f.sync_all()).map_err(|e| io("can't write", path, e))
}

pub(crate) fn sync_dir(dir: &Path) {
    // (makes the names in it durable; a folder can't be opened on Windows, where renames are journaled)
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
}

/// A regular file (not a link to one elsewhere).
pub(crate) fn regular(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file())
}

/// The newest finished backup in `dest` (by when its marker was written, then by name), and the
/// segments its snapshot lists by name.
fn previous(dest: &Path) -> Option<(PathBuf, BTreeMap<String, Segment>)> {
    let mut found: Vec<(std::time::SystemTime, String)> = std::fs::read_dir(dest)
        .ok()?
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|name| !name.ends_with(PARTIAL) && regular(&dest.join(name).join(MARKER)))
        .map(|name| (std::fs::metadata(dest.join(&name).join(MARKER)).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH), name))
        .collect();
    found.sort_unstable();
    found.into_iter().rev().find_map(|(_, name)| {
        let dir = dest.join(&name);
        let head = read_head(&std::fs::read(dir.join(SNAPSHOT)).ok()?).ok()?;
        Some((dir, head.segments.into_iter().map(|s| (s.name.clone(), s)).collect()))
    })
}

/// A file an operating system writes into a folder that somebody looked at (Finder, Explorer): not
/// the backup's, and no change to it, so it does not make a sound backup fail its check.
fn os_metadata(name: &str) -> bool {
    let name = name.rsplit('/').next().unwrap_or(name);
    matches!(name, ".DS_Store" | "Thumbs.db" | "desktop.ini") || name.starts_with("._")
}

/// Remove what a backup that was cut short left in `dest`: folders `<name>.partial` an hour old
/// or more (one being written now, by this or another program, is younger: the folder or its
/// `segments` folder, which is where a backup writes most, was changed less than an hour ago)
/// that hold nothing but what a backup holds. Anything else of that name is left alone.
fn sweep(dest: &Path, also: &[&str]) {
    let Ok(entries) = std::fs::read_dir(dest) else { return };
    // None: no age to go by (a time yet to come, or unreadable): not old. A folder that is not
    // there has no age either, and is told apart below.
    let age = |p: &Path| std::fs::symlink_metadata(p).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
    let quiet = |p: &Path| match std::fs::symlink_metadata(p) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        _ => age(p).is_some_and(|a| a >= STALE),
    };
    for entry in entries.flatten() {
        let (Ok(name), path) = (entry.file_name().into_string(), entry.path()) else { continue };
        let old = age(&path).is_some_and(|a| a >= STALE) && quiet(&path.join(segments::SEGMENT_DIR));
        if !name.ends_with(PARTIAL) || !old || !std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir()) {
            continue;
        }
        let ours = held(&path).is_ok_and(|names| {
            names.iter().all(|n| n == SNAPSHOT || n == LOG || n == MARKER || also.contains(&n.as_str()) || segments::valid_name(n) || os_metadata(n))
        });
        if ours && let Err(e) = std::fs::remove_dir_all(&path) {
            log::warn!("backup: can't remove {}: {e}", path.display());
        }
    }
}

fn is(bytes: &[u8], segment: &Segment) -> bool {
    bytes.len() as u64 == segment.bytes && crc32fast::hash(bytes) == segment.crc
}

/// What one pass over the library's files came to.
enum Pass {
    Done(BackupReport),
    /// `catalog.snap` changed while its files were read: start over.
    Again,
}

struct Job<'a> {
    library: &'a Path,
    partial: &'a Path,
    options: &'a BackupOptions<'a>,
    watch: &'a dyn Watch,
    previous: Option<&'a (PathBuf, BTreeMap<String, Segment>)>,
    /// Segment files in `partial` already: each as it is listed, and whether it is shared with
    /// the backup before.
    have: BTreeMap<String, (Held, bool)>,
}

impl Job<'_> {
    /// Whether the library's `catalog.snap` is no longer `seen`.
    fn changed(&self, seen: &[u8]) -> Result<bool, BackupError> {
        Ok(read(&self.library.join(SNAPSHOT))?.as_deref() != Some(seen))
    }

    fn pass(&mut self) -> Result<Pass, BackupError> {
        let snapshot = read(&self.library.join(SNAPSHOT))?
            .ok_or_else(|| BackupError::Library(format!("there is no {SNAPSHOT} in {}", self.library.display())))?;
        let head = read_head(&snapshot)?;
        // (sizes a snapshot states are not to be trusted: no sum of them may overflow)
        let total = head.segments.iter().fold(snapshot.len() as u64, |sum, s| sum.saturating_add(s.bytes));
        let mut done = 0u64;
        for segment in &head.segments {
            if self.watch.cancelled() {
                return Err(BackupError::Cancelled);
            }
            self.watch.progress(&segment.name, done, total);
            done = done.saturating_add(segment.bytes);
            let listed = Held { bytes: segment.bytes, crc: segment.crc };
            let to = self.partial.join(&segment.name);
            folder_for(&to)?;
            match self.have.get(&segment.name) {
                Some((held, _)) if *held == listed => continue,
                // (a file of that name from an earlier pass that isn't this one)
                Some(_) => {
                    std::fs::remove_file(&to).map_err(|e| io("can't remove", &to, e))?;
                    self.have.remove(&segment.name);
                }
                None => {}
            }
            // the same file as in the backup before: shared with it, once it was read and is
            // what it should be (damage in an older backup is not handed on)
            if let Some((dir, before)) = self.previous
                && before.get(&segment.name).is_some_and(|p| p.bytes == segment.bytes && p.crc == segment.crc)
            {
                let from = dir.join(&segment.name);
                if regular(&from) && std::fs::read(&from).is_ok_and(|b| is(&b, segment)) && std::fs::hard_link(&from, &to).is_ok() {
                    self.have.insert(segment.name.clone(), (listed, true));
                    continue;
                }
            }
            let bytes = match read(&self.library.join(&segment.name))? {
                Some(bytes) if is(&bytes, segment) => bytes,
                // gone or different: because the library was compacted meanwhile, or damaged
                found => {
                    if self.changed(&snapshot)? {
                        return Ok(Pass::Again);
                    }
                    let what = if found.is_some() { "is damaged" } else { "is missing" };
                    return Err(BackupError::Library(format!("{} {what}", segment.name)));
                }
            };
            write(&to, &bytes)?;
            self.have.insert(segment.name.clone(), (listed, false));
        }
        self.watch.progress(LOG, done, total);
        // the log after the segments, and then the snapshot once more: if it is still the one
        // read first, the log is the one that goes with it
        let log = read(&self.library.join(LOG))?;
        if self.changed(&snapshot)? {
            return Ok(Pass::Again);
        }
        if self.watch.cancelled() {
            return Err(BackupError::Cancelled);
        }
        // what an earlier pass copied and this snapshot doesn't list
        let listed: std::collections::BTreeSet<&str> = head.segments.iter().map(|s| s.name.as_str()).collect();
        let stale: Vec<String> = self.have.keys().filter(|n| !listed.contains(n.as_str())).cloned().collect();
        for name in stale {
            let path = self.partial.join(&name);
            std::fs::remove_file(&path).map_err(|e| io("can't remove", &path, e))?;
            self.have.remove(&name);
        }
        let mut report = BackupReport {
            dir: PathBuf::new(),
            seq: head.seq,
            photos: head.photos.unwrap_or(head.catalog.photos.0),
            segments: head.segments.len() as u64,
            files: head.segments.len() as u64 + 1,
            copied: 1,
            linked: 0,
            bytes: total,
            copied_bytes: snapshot.len() as u64,
            contents: BTreeMap::new(),
        };
        report.contents.insert(SNAPSHOT.to_string(), Held { bytes: snapshot.len() as u64, crc: crc32fast::hash(&snapshot) });
        for segment in &head.segments {
            report.contents.insert(segment.name.clone(), Held { bytes: segment.bytes, crc: segment.crc });
            match self.have.get(&segment.name) {
                Some((_, true)) => report.linked += 1,
                _ => {
                    report.copied += 1;
                    report.copied_bytes = report.copied_bytes.saturating_add(segment.bytes);
                }
            }
        }
        let mut small: Vec<(&str, Vec<u8>)> = Vec::new();
        if let Some(log) = log {
            small.push((LOG, log));
        }
        for name in self.options.also {
            if let Some(bytes) = read(&self.library.join(name))? {
                small.push((name, bytes));
            }
        }
        for (name, bytes) in &small {
            report.contents.insert(name.to_string(), Held { bytes: bytes.len() as u64, crc: crc32fast::hash(bytes) });
            write(&self.partial.join(name), bytes)?;
            report.files += 1;
            report.copied += 1;
            report.bytes = report.bytes.saturating_add(bytes.len() as u64);
            report.copied_bytes = report.copied_bytes.saturating_add(bytes.len() as u64);
        }
        // the snapshot last: a folder with it has everything it lists
        write(&self.partial.join(SNAPSHOT), &snapshot)?;
        Ok(Pass::Done(report))
    }
}

/// Back up the library in `library` as the folder `options.name` in `dest` (made if it isn't
/// there). See the module docs.
pub fn back_up(library: &Path, dest: &Path, options: &BackupOptions<'_>, watch: &dyn Watch) -> Result<BackupReport, BackupError> {
    if !valid_name(options.name) {
        return Err(BackupError::Name(options.name.to_string()));
    }
    if let Some(bad) = options.also.iter().find(|n| !valid_extra(n)) {
        return Err(BackupError::Name(bad.to_string()));
    }
    if !library.join(SNAPSHOT).is_file() {
        return Err(BackupError::Library(format!("there is no {SNAPSHOT} in {}", library.display())));
    }
    std::fs::create_dir_all(dest).map_err(|e| io("can't make the folder", dest, e))?;
    let dir = dest.join(options.name);
    let partial = dest.join(format!("{}{PARTIAL}", options.name));
    if dir.exists() || partial.exists() {
        return Err(BackupError::Exists(options.name.to_string()));
    }
    sweep(dest, options.also);
    let previous = previous(dest);
    std::fs::create_dir(&partial).map_err(|e| io("can't make the folder", &partial, e))?;
    // (this run made the folder: nothing of a backup that isn't whole is left, also if what
    // follows should panic)
    struct Unfinished<'a>(Option<&'a Path>);
    impl Drop for Unfinished<'_> {
        fn drop(&mut self) {
            if let Some(partial) = self.0
                && let Err(e) = std::fs::remove_dir_all(partial)
            {
                log::warn!("backup: can't remove {}: {e}", partial.display());
            }
        }
    }
    let mut unfinished = Unfinished(Some(&partial));
    let mut job = Job { library, partial: &partial, options, watch, previous: previous.as_ref(), have: BTreeMap::new() };
    let made = (|| {
        for _ in 0..ATTEMPTS {
            if let Pass::Done(mut report) = job.pass()? {
                report.dir = dir.clone();
                let marker = serde_json::to_vec_pretty(&report).map_err(|e| BackupError::Io(e.to_string()))?;
                write(&partial.join(MARKER), &marker)?;
                sync_dir(&partial.join(segments::SEGMENT_DIR));
                sync_dir(&partial);
                std::fs::rename(&partial, &dir).map_err(|e| io("can't name the backup", &dir, e))?;
                sync_dir(dest);
                return Ok(report);
            }
        }
        Err(BackupError::Library("it kept being saved while it was read; try again".into()))
    })();
    if made.is_ok() {
        // (renamed: there is no partial folder any more)
        unfinished.0 = None;
    }
    made
}

/// The finished backups in `dest`, newest first (by when their markers were written, then by
/// name): each as its marker reports it, with the folder it is in now. Folders whose marker
/// can't be read are left out.
pub fn list(dest: &Path) -> Vec<BackupReport> {
    let Ok(entries) = std::fs::read_dir(dest) else { return Vec::new() };
    let mut found: Vec<(std::time::SystemTime, String, BackupReport)> = entries
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|name| !name.ends_with(PARTIAL) && regular(&dest.join(name).join(MARKER)))
        .filter_map(|name| {
            let marker = dest.join(&name).join(MARKER);
            let made = std::fs::metadata(&marker).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
            let mut report: BackupReport = serde_json::from_slice(&std::fs::read(&marker).ok()?).ok()?;
            report.dir = dest.join(&name);
            Some((made, name, report))
        })
        .collect();
    found.sort_unstable_by(|a, b| (b.0, &b.1).cmp(&(a.0, &a.1)));
    found.into_iter().map(|(_, _, report)| report).collect()
}

/// Whether `dir` is a backup that was made: its marker is a file that reads as a backup's list.
/// (Stricter than [`is_backup`], for telling a backup from a folder that merely holds a file of
/// that name: an export called `backup.json` does not make a drive a backup.)
pub fn is_made_backup(dir: &Path) -> bool {
    let marker = dir.join(MARKER);
    regular(&marker) && std::fs::read(&marker).ok().is_some_and(|bytes| serde_json::from_slice::<BackupReport>(&bytes).is_ok())
}

/// Whether `dir` is a backup: it has a backup's marker (whatever that is: a folder with
/// something of that name is not worked in as a library).
pub fn is_backup(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir.join(MARKER)).is_ok()
}

/// Read the backup in `dir` against what its marker lists: every file there, with the size and
/// CRC-32 it had when the backup was made, and no other file (but what a file manager writes into
/// a folder somebody looked at: `.DS_Store`, `Thumbs.db`, `desktop.ini`, `._…`). The first thing that isn't so is
/// the error, naming the file. Returns the backup's report.
pub fn check(dir: &Path) -> Result<BackupReport, BackupError> {
    let changed = BackupError::Changed;
    let unreadable = |e: BackupError| BackupError::Changed(e.to_string());
    if !regular(&dir.join(MARKER)) {
        return Err(changed(format!("{} is not a backup (it has no {MARKER})", dir.display())));
    }
    let marker = read(&dir.join(MARKER)).map_err(unreadable)?.unwrap_or_default();
    let report: BackupReport = serde_json::from_slice(&marker).map_err(|e| changed(format!("{MARKER}: {e}")))?;
    if report.contents.is_empty() {
        return Err(changed(format!("{MARKER} doesn't list the backup's files")));
    }
    for (name, held) in &report.contents {
        // (only files of this folder: a marker is a file like any other, and may have been edited)
        if !(valid_name(name) || segments::valid_name(name)) || name == MARKER {
            return Err(changed(format!("{MARKER} lists {name:?}, which is not a file of the backup")));
        }
        let path = dir.join(name);
        if std::fs::symlink_metadata(&path).is_err() {
            return Err(changed(format!("{name} is missing")));
        }
        // (a file of its own, not a link to one elsewhere: the backup stands on its own)
        let same = regular(&path)
            && read(&path).map_err(unreadable)?.is_some_and(|bytes| bytes.len() as u64 == held.bytes && crc32fast::hash(&bytes) == held.crc);
        if !same {
            return Err(changed(format!("{name} is not as it was when the backup was made")));
        }
    }
    for name in held(dir).map_err(|e| changed(format!("{e}, and was not part of the backup")))? {
        if name != MARKER && !report.contents.contains_key(&name) && !os_metadata(&name) {
            return Err(changed(format!("{name} was not part of the backup")));
        }
    }
    Ok(report)
}
