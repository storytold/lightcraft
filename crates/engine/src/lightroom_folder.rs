//! Recognise a Lightroom Classic catalog where a LightCraft library was asked for (issue #891).
//!
//! A Lightroom catalog folder holds `Name.lrcat` (SQLite) next to `Name Previews.lrdata`,
//! `Name Smart Previews.lrdata`, `Name Helper.lrdata`, `Name.lrcat-data`, the `-wal` / `-shm` /
//! `.lock` files of an open catalog and `Backups/`. Opening any of these as a library would create
//! an empty LightCraft library inside Lightroom's folder; [`Session::open_library`] refuses with
//! [`EngineError::LightroomCatalog`] instead, and the app offers File ▸ Import Lightroom Catalog….
//!
//! What counts (only names are looked at, and a backup zip's list of files; no catalog is opened
//! or read):
//! - a LightCraft library (`catalog.snap` or `catalog.log` in the folder) is never a Lightroom
//!   catalog, whatever else the folder holds: it opens as before;
//! - a path named `*.lrcat` or one of its companions (`*.lrcat-wal`, `*.lrcat-shm`,
//!   `*.lrcat-journal`, `*.lrcat.lock`);
//! - a folder named `*.lrdata` or `*.lrcat-data` (Lightroom's previews and data);
//! - a folder holding a `*.lrcat` at its top level (the catalog folder itself), or `Backups/`
//!   next to one;
//! - Lightroom's backups (issue #2592), even with the catalog itself gone: a `Backups/` folder or
//!   one of its dated folders, `Backups/YYYY-MM-DD HHMM/`, holding a `*.zip` with one `.lrcat` in
//!   it ([`crate::lightroom_backup::holds_catalog`]). A dated folder names
//!   its zip; `Backups/` names one only when it holds a single backup, since the person has to choose
//!   among several (the import then asks for the file).
//!
//! The folder scan is bounded ([`SCAN_LIMIT`] entries, top level only) and every I/O error just
//! means "not found": this never fails and never panics.
//!
//! [`Session::open_library`]: crate::Session::open_library
//! [`EngineError::LightroomCatalog`]: crate::EngineError::LightroomCatalog

use std::path::{Path, PathBuf};

/// The most directory entries looked at in one folder.
pub const SCAN_LIMIT: usize = 10_000;

/// A Lightroom Classic catalog (or part of one) found where a library was asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LightroomPlace {
    /// The `.lrcat` file to import, when one was found.
    pub catalog: Option<PathBuf>,
}

/// Companion files of an open catalog, after the `.lrcat` (lower case).
const COMPANIONS: [&str; 4] = ["-wal", "-shm", "-journal", ".lock"];
/// Lightroom's own `.lrdata` folders: `Name<suffix>.lrdata` belongs to `Name.lrcat`.
const LRDATA_SUFFIXES: [&str; 4] = [" Smart Previews", " Previews", " Helper", " Sync"];

/// `path`'s file name, lower-cased (lossy: only used to compare with ASCII suffixes).
fn lower_name(path: &Path) -> Option<String> {
    path.file_name().map(|n| n.to_string_lossy().to_lowercase())
}

/// `name` ends with `suffix`, ignoring ASCII case; returns the part before it (`None` for a
/// non-UTF-8 name, or when the cut would not be on a character boundary).
fn strip_suffix_nocase<'a>(name: &'a str, suffix: &str) -> Option<&'a str> {
    let cut = name.len().checked_sub(suffix.len())?;
    let tail = name.get(cut..)?;
    tail.eq_ignore_ascii_case(suffix).then(|| name.get(..cut)).flatten()
}

/// `name` is `*.lrcat` (any case).
fn is_lrcat_name(name: &str) -> bool {
    name.to_lowercase().ends_with(".lrcat")
}

/// The first `*.lrcat` file (by name) at the top level of `dir`, looking at no more than
/// [`SCAN_LIMIT`] entries. Unreadable folders and entries are skipped.
pub fn find_lrcat(dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut found: Option<PathBuf> = None;
    for entry in entries.take(SCAN_LIMIT).flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !is_lrcat_name(name) {
            continue;
        }
        let path = entry.path();
        // (follows a symlink; a folder named `*.lrcat` is not a catalog)
        if !path.is_file() {
            continue;
        }
        if found.as_ref().is_none_or(|f| path < *f) {
            found = Some(path);
        }
    }
    found
}

/// `candidate` if it is a file, else the first catalog in `dir`.
fn catalog_or_scan(candidate: Option<PathBuf>, dir: Option<&Path>) -> Option<PathBuf> {
    candidate.filter(|c| c.is_file()).or_else(|| dir.and_then(find_lrcat))
}

/// Whether `path`, given as a library folder, is a Lightroom Classic catalog or part of one (see
/// the module docs). `None`: not Lightroom's, open it as a library.
pub fn lightroom_catalog_at(path: &Path) -> Option<LightroomPlace> {
    let is_dir = path.is_dir();
    if is_dir && crate::library::is_library(path) {
        return None;
    }
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    let name = path.file_name().and_then(|n| n.to_str());
    let lower = lower_name(path).unwrap_or_default();
    // the catalog file itself (a folder named `*.lrcat` is looked into below)
    if lower.ends_with(".lrcat") && !is_dir {
        return Some(LightroomPlace { catalog: path.is_file().then(|| path.to_path_buf()) });
    }
    // an open catalog's companion files: `Name.lrcat-wal` → `Name.lrcat`
    for c in COMPANIONS {
        if lower.ends_with(&format!(".lrcat{c}")) && !is_dir {
            let candidate = name.and_then(|n| strip_suffix_nocase(n, c)).and_then(|n| parent.map(|p| p.join(n)));
            return Some(LightroomPlace { catalog: catalog_or_scan(candidate, parent) });
        }
    }
    // `Name.lrcat-data` → `Name.lrcat`
    if let Some(stem) = name.and_then(|n| strip_suffix_nocase(n, "-data"))
        && is_lrcat_name(stem)
    {
        let candidate = parent.map(|p| p.join(stem));
        return Some(LightroomPlace { catalog: catalog_or_scan(candidate, parent) });
    }
    // `Name Previews.lrdata` (and Smart Previews, Helper, Sync) → `Name.lrcat`
    if let Some(stem) = name.and_then(|n| strip_suffix_nocase(n, ".lrdata")) {
        let base = LRDATA_SUFFIXES.iter().find_map(|s| strip_suffix_nocase(stem, s)).unwrap_or(stem);
        let candidate = parent.map(|p| p.join(format!("{base}.lrcat")));
        return Some(LightroomPlace { catalog: catalog_or_scan(candidate, parent) });
    }
    if lower.ends_with(".lrdata") || lower.ends_with(".lrcat-data") {
        // (a non-UTF-8 name)
        return Some(LightroomPlace { catalog: parent.and_then(find_lrcat) });
    }
    if is_dir {
        // the catalog folder
        if let Some(catalog) = find_lrcat(path) {
            return Some(LightroomPlace { catalog: Some(catalog) });
        }
        // `Backups/` next to a catalog
        if lower == "backups"
            && let Some(catalog) = parent.and_then(find_lrcat)
        {
            return Some(LightroomPlace { catalog: Some(catalog) });
        }
        // `Backups/` alone (the catalog deleted): its backup when there is only one; with several,
        // which one to import is the person's choice
        if lower == "backups"
            && let Some(backups) = backup_zips(path)
        {
            let catalog = if backups.len() == 1 { backups.into_iter().next() } else { None };
            return Some(LightroomPlace { catalog });
        }
        // a dated backup folder, `Backups/2023-11-07 0900/`
        if parent.and_then(lower_name).as_deref() == Some("backups")
            && let Some(zip) = find_backup_zip(path)
        {
            return Some(LightroomPlace { catalog: Some(zip) });
        }
    }
    None
}

/// The first `*.zip` file (by name) at the top level of `dir` that holds a Lightroom catalog,
/// looking at no more than [`SCAN_LIMIT`] entries.
fn find_backup_zip(dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    entries
        .take(SCAN_LIMIT)
        .flatten()
        .map(|e| e.path())
        .filter(|p| crate::lightroom_backup::is_backup(p) && p.is_file())
        .filter(|p| crate::lightroom_backup::holds_catalog(p))
        .min()
}

/// The backup zips in the dated folders of `backups`, one per folder; `None` when there are none.
fn backup_zips(backups: &Path) -> Option<Vec<PathBuf>> {
    let entries = std::fs::read_dir(backups).ok()?;
    let zips: Vec<PathBuf> =
        entries.take(SCAN_LIMIT).flatten().map(|e| e.path()).filter(|p| p.is_dir()).filter_map(|d| find_backup_zip(&d)).collect();
    (!zips.is_empty()).then_some(zips)
}
