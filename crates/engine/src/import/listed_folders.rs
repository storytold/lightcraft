//! What an import (or the review before it) learned of the folders it listed: the files in them
//! and the `.xmp` names.
//!
//! Looking for a photo's sidecar asks the disk about up to nine names ([`sidecar_candidates`]).
//! On a network share a name that is not there costs a round trip as much as one that is, so a
//! folder with no `.xmp` at all cost nine for each of its photos. Telling a file from a folder
//! (`is_dir`) is a round trip too, for each of the thousands of files a review hands over. The
//! listing that found those files already has both answers.
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::SystemTime;

use crate::sidecar::{SidecarNaming, find_sidecar, sidecar_candidates};
use crate::walk::EntryKind;

/// One listed folder.
#[derive(Debug, Default)]
struct Folder {
    /// The names listed as files (links are not: the disk says where they lead).
    files: HashSet<String>,
    /// The `.xmp` names, lower-cased. Only for a folder that was listed completely, with plain
    /// ASCII names (the names a file system treats alike in other cases are the ones we can fold
    /// ourselves): for any other the disk decides, as before.
    xmp: Option<HashSet<String>>,
    /// The folder's modification time when it was listed: a file added or removed since changes
    /// it, and the `.xmp` names are then not trusted (a listing taken by a review can be hours old).
    modified: Option<SystemTime>,
}

/// The folders listed so far (see the module docs). The review's scan hands its listing to the
/// import that review starts ([`crate::import::ImportJob::with_listed`]), and nowhere else.
/// The `.xmp` names of a folder are trusted only while its modification time is what it was
/// when it was listed (looked at once per folder per batch: [`ListedFolders::new_batch`]); a
/// volume whose folders keep no usable time (it can't be read) is probed as before.
#[derive(Debug, Default)]
pub struct ListedFolders {
    folders: HashMap<PathBuf, Folder>,
    /// folder → whether its time is still the listed one, for the batch being handled.
    checked: Mutex<HashMap<PathBuf, bool>>,
    /// How often the disk was asked after all about a sidecar (for the tests).
    pub(crate) asked: AtomicUsize,
    /// How many folders' times were looked at (for the tests).
    pub(crate) looked: AtomicUsize,
}

impl ListedFolders {
    /// Take the listing of `folder` from the walk ([`crate::walk::files_in_noting`]), with the
    /// folder's time as the walk read it before listing (`None`: too recent to vouch for, or unreadable).
    pub(crate) fn note(&mut self, folder: &Path, entries: Vec<(OsString, EntryKind)>, complete: bool, modified: Option<SystemTime>) {
        let mut listed = Folder { modified, ..Default::default() };
        let mut xmp = (complete && listed.modified.is_some()).then(HashSet::new);
        for (name, kind) in entries {
            // (a name that is no text is not one a path can be compared with)
            let Some(name) = name.to_str() else {
                xmp = None;
                continue;
            };
            if let Some(set) = &mut xmp {
                if name.is_ascii() {
                    let lower = name.to_ascii_lowercase();
                    if lower.ends_with(".xmp") {
                        set.insert(lower);
                    }
                } else {
                    // (not ours to say what another name's other case is)
                    xmp = None;
                }
            }
            if kind == EntryKind::File && crate::import::is_supported(Path::new(name)) {
                listed.files.insert(name.to_string());
            }
        }
        listed.xmp = xmp;
        self.folders.insert(folder.to_path_buf(), listed);
    }

    /// A new batch of files is handled: look at the folders' times again.
    pub(crate) fn new_batch(&self) {
        self.checked.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    /// The names of files are needed to take a path for a file (once the files are expanded, no more).
    pub(crate) fn forget_files(&mut self) {
        for f in self.folders.values_mut() {
            f.files = HashSet::new();
        }
    }

    /// Whether `dir` is as it was listed.
    fn unchanged(&self, dir: &Path, listed: Option<SystemTime>) -> bool {
        let mut checked = self.checked.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(known) = checked.get(dir) {
            return *known;
        }
        self.looked.fetch_add(1, Ordering::Relaxed);
        let now = std::fs::metadata(dir).and_then(|m| m.modified()).ok();
        let same = now.is_some() && now == listed;
        checked.insert(dir.to_path_buf(), same);
        same
    }

    /// Take over `walked`'s folders, which are newer than the ones held for them.
    pub(crate) fn absorb(&mut self, walked: ListedFolders) {
        self.folders.extend(walked.folders);
    }

    /// Whether a listing says `path` is a file, as it was found: the disk need not be asked.
    pub(crate) fn is_known_file(&self, path: &Path) -> bool {
        let (Some(dir), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str())) else { return false };
        // (the path ends in that name: no `/.` or separator after it, which the disk would resolve)
        path.to_str().is_some_and(|p| p.ends_with(name)) && self.folders.get(dir).is_some_and(|f| f.files.contains(name))
    }

    /// Whether the listing of `original`'s folder shows that none of its sidecar names is there.
    /// `false` where it can't say: the folder was not listed (or not completely), or a name is
    /// one the disk may resolve another way.
    fn rules_out(&self, original: &str, naming: SidecarNaming) -> bool {
        let path = Path::new(original);
        let Some(dir) = path.parent() else { return false };
        let Some((xmp, modified)) = self.folders.get(dir).and_then(|f| Some((f.xmp.as_ref()?, f.modified))) else { return false };
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { return false };
        if !original.ends_with(name) || crate::sync::asks_the_disk(name) || !crate::sync::plain_name(name) || !self.unchanged(dir, modified) {
            return false;
        }
        sidecar_candidates(original, naming)
            .all(|c| c.file_name().and_then(|n| n.to_str()).is_some_and(|n| crate::sync::plain_name(n) && !xmp.contains(&n.to_ascii_lowercase())))
    }

    /// [`crate::sidecar::find_sidecar_for_import`], without asking the disk where the listing shows
    /// there is none: a file that owns its stem sidecar ([`crate::sidecar::ImportStems`]) is looked
    /// up as [`ListedFolders::find`] does; one that doesn't reads only its own `<file>.xmp`.
    pub(crate) fn find_for_import(&self, stems: &crate::sidecar::ImportStems, naming: SidecarNaming, original: &str) -> Option<PathBuf> {
        if stems.owns(original) {
            return self.find(original, naming);
        }
        self.asked.fetch_add(1, Ordering::Relaxed);
        crate::sidecar::find_sidecar_for_import(stems, naming, original)
    }

    /// [`find_sidecar`], without asking the disk where the listing shows there is none.
    pub(crate) fn find(&self, original: &str, naming: SidecarNaming) -> Option<PathBuf> {
        if self.rules_out(original, naming) {
            return None;
        }
        self.asked.fetch_add(1, Ordering::Relaxed);
        find_sidecar(original, naming)
    }
}
