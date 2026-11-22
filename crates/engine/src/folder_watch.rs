//! Folder polling (issue #123): notice photos added to the library's folders by other apps.
//!
//! Polling is opt-in ([`WatchPrefs::poll`], off by default: on a VPN or a slow link even a
//! listing costs something). A poll lists each of the library's folders once — one directory
//! read per folder, no file is opened or stat'ed — and keeps a [`Stamp`] of it: the folder's
//! modification time and a hash of its entries' names. Only a folder whose stamp changed since
//! the last poll is scanned again, with Synchronize Folder's scan ([`crate::sync`]), so a tree of
//! tens of thousands of files in hundreds of folders costs one listing per folder when nothing
//! changed.
//!
//! What happens to the new files a scan finds is the [`NewFilePolicy`]: added at once (the
//! import half of Synchronize Folder), or — the default — counted per folder
//! ([`crate::Session::folder_new`]) for the Folders panel's "N new" badge, until the person adds
//! them (Synchronize Folder).
//!
//! A folder that holds no photo itself but whose subfolders do (`Photos/2026` above
//! `Photos/2026/11-20`) is listed too, from the folder the library's photos branch out of
//! downwards ([`WatchKind::Parent`]): only its subfolders' names count there, so a new day folder
//! that another app (a sync tool, a camera app) creates beside the old ones is noticed, while files
//! and hidden folders that come and go in it, and subfolders that go away, are not. A folder whose
//! visible subfolders are mostly not the library's (a home folder above two photo trees) is not
//! watched at all: a change there would mean scanning everything inside it.
//!
//! The first poll of a library (no stamps yet) only records the stamps: its folders were just
//! imported or were already looked at. Stamps are kept in the library (`folder-stamps.json`), so
//! a poll on open notices what changed while the app was closed. A folder that isn't there (a
//! disconnected disk, a share that is offline) keeps its old stamp and is not scanned.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};

use lcc_catalog::Catalog;
use lcc_catalog::folders::FolderNode;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// What happens to new files a poll finds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NewFilePolicy {
    /// Count them for the folder's "N new" badge; the person adds them.
    #[default]
    Ask,
    /// Add them to the library at once (in place).
    Add,
}

impl NewFilePolicy {
    pub fn parse(s: &str) -> Option<NewFilePolicy> {
        match s {
            "ask" => Some(NewFilePolicy::Ask),
            "add" => Some(NewFilePolicy::Add),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            NewFilePolicy::Ask => "ask",
            NewFilePolicy::Add => "add",
        }
    }
}

/// The shortest and longest time between polls, in minutes.
pub const MIN_INTERVAL: u32 = 1;
pub const MAX_INTERVAL: u32 = 24 * 60;

/// Library preferences for folder polling (saved with the library).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct WatchPrefs {
    /// Poll the library's folders on open, when the app comes to the front and every
    /// `interval_minutes`.
    pub poll: bool,
    pub interval_minutes: u32,
    pub new_files: NewFilePolicy,
}

impl Default for WatchPrefs {
    fn default() -> Self {
        WatchPrefs { poll: false, interval_minutes: 5, new_files: NewFilePolicy::Ask }
    }
}

impl WatchPrefs {
    /// With the interval in range (a hand-edited file may say anything).
    pub fn sanitized(self) -> Self {
        WatchPrefs { interval_minutes: self.interval_minutes.clamp(MIN_INTERVAL, MAX_INTERVAL), ..self }
    }
}

/// What a folder's listing looked like.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    /// The folder's modification time (ms since the epoch), when the file system says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtime: Option<u64>,
    /// A hash of its entries' names (and whether each is a folder).
    pub entries: u64,
    /// How many entries it has.
    pub count: u64,
    /// A [`WatchKind::Parent`]'s visible subfolder names (so a poll can tell which are new; empty
    /// in stamps kept by an older version).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subdirs: Vec<String>,
}

/// Stamps by folder, as the catalog spells it.
pub type Stamps = BTreeMap<String, Stamp>;

/// What the library keeps between runs (`folder-stamps.json`): the last poll's stamps and the
/// "N new" counts not answered yet (else a restart would forget them: the next poll sees no
/// change).
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct WatchState {
    pub stamps: Stamps,
    pub new: BTreeMap<String, usize>,
}

/// [`WatchState`] borrowed, for saving.
#[derive(Serialize)]
pub struct WatchStateRef<'a> {
    pub stamps: &'a Stamps,
    pub new: &'a BTreeMap<String, usize>,
}

/// The most entries of one folder a stamp reads (a bigger folder is stamped by its first ones
/// and its modification time).
const MAX_ENTRIES: usize = 1_000_000;

/// FNV-1a, which is the same in every build (stamps are kept between runs).
fn fnv(h: &mut u64, bytes: &[u8]) {
    for b in bytes {
        *h ^= u64::from(*b);
        *h = h.wrapping_mul(0x0100_0000_01b3);
    }
}

/// The stamp of folder `dir`; `None` when it can't be listed (not there, offline, no access).
pub fn stamp(dir: &str) -> Option<Stamp> {
    stamp_of(dir, WatchKind::Photos)
}

/// The stamp of folder `dir` as watched as `kind`: for a [`WatchKind::Parent`], its subfolders'
/// names only (and no modification time, which files coming and going change too).
pub fn stamp_of(dir: &str, kind: WatchKind) -> Option<Stamp> {
    if cfg!(target_arch = "wasm32") {
        return None;
    }
    let path = std::path::Path::new(dir);
    let rd = std::fs::read_dir(path).ok()?;
    let mut names: Vec<(String, bool)> = Vec::new();
    for e in rd.take(MAX_ENTRIES).flatten() {
        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
        // a parent: its visible subfolders only (apps' hidden folders come and go)
        if kind == WatchKind::Parent && (!is_dir || e.file_name().to_string_lossy().starts_with('.')) {
            continue;
        }
        names.push((e.file_name().to_string_lossy().into_owned(), is_dir));
    }
    if kind == WatchKind::Parent {
        names.sort();
        let mut h = 0xcbf2_9ce4_8422_2325_u64;
        for (n, _) in &names {
            fnv(&mut h, n.as_bytes());
            fnv(&mut h, b"/\0");
        }
        let subdirs = names.iter().map(|(n, _)| n.clone()).collect();
        return Some(Stamp { mtime: None, entries: h, count: names.len() as u64, subdirs });
    }
    names.sort();
    let mut h = 0xcbf2_9ce4_8422_2325_u64;
    for (n, d) in &names {
        fnv(&mut h, n.as_bytes());
        fnv(&mut h, if *d { b"/\0" } else { b"\0" });
    }
    let mtime = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    Some(Stamp { mtime, entries: h, count: names.len() as u64, subdirs: Vec::new() })
}

/// How a folder is watched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchKind {
    /// It holds photos: any change to its listing counts.
    Photos,
    /// It holds none itself, only folders that do: a subfolder added or gone counts.
    Parent,
}

/// A folder a poll lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchedDir {
    pub path: String,
    pub kind: WatchKind,
    /// How many of its subfolders hold library photos (for a [`WatchKind::Parent`]: one whose
    /// folders are mostly not the library's, a home folder, is too broad to watch).
    pub folders: usize,
}

/// A parent folder is watched only while at least half of its visible subfolders hold library
/// photos, and never when it is a whole disk or share, or the user's home folder (however few
/// folders those hold): a new folder there would otherwise mean scanning, and with new files
/// added, importing everything inside it.
fn too_broad(w: &WatchedDir) -> bool {
    if w.kind != WatchKind::Parent || cfg!(target_arch = "wasm32") {
        return false;
    }
    // (Windows: USERPROFILE first; Git Bash/MSYS set HOME to a `/c/Users/x` path that never matches)
    let var = |k: &str| std::env::var(k).ok().filter(|h| !h.is_empty());
    let home = if cfg!(windows) { var("USERPROFILE").or_else(|| var("HOME")) } else { var("HOME").or_else(|| var("USERPROFILE")) };
    if broad_place(&w.path, home.as_deref()) {
        return true;
    }
    let Ok(rd) = std::fs::read_dir(&w.path) else { return false };
    let on_disk = rd
        .take(MAX_ENTRIES)
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()) && !e.file_name().to_string_lossy().starts_with('.'))
        .count();
    on_disk > w.folders.saturating_mul(2)
}

/// Whether `path` is a whole disk or share, or `home` (compared ignoring slash direction, a
/// trailing slash and, on Windows, case).
pub(crate) fn broad_place(path: &str, home: Option<&str>) -> bool {
    if lcc_catalog::folders::is_disk_root(path) {
        return true;
    }
    let norm = |p: &str| {
        let p = p.replace('\\', "/");
        let p = p.trim_end_matches('/').to_string();
        if cfg!(windows) { p.to_lowercase() } else { p }
    };
    home.is_some_and(|h| !norm(h).is_empty() && norm(h) == norm(path))
}

/// The folders a poll lists: every folder the library holds photos in directly, and the folders
/// between them and the folder they branch out of (the deepest folder above all of a branch's
/// library folders; [`WatchKind::Parent`]). Not the disks themselves, the startup disk, folders
/// holding disks, nor anything above where the library's folders branch out (a home folder above
/// a single `Pictures/…` tree); [`poll_dirs`] also skips a parent whose subfolders are mostly not
/// the library's (a home folder above two trees): anything that changes there would mean scanning everything inside
/// it. So a new folder inside or beside the library's folders is noticed (its parent's listing
/// changes), but one in an unrelated place is not: that one is imported.
pub fn watched_dirs(cat: &Catalog) -> Vec<WatchedDir> {
    fn walk(nodes: &[FolderNode], out: &mut Vec<WatchedDir>, depth: usize, inside: bool) {
        if depth > 64 {
            return;
        }
        for n in nodes {
            let eligible = n.selectable && !n.volume && !lcc_catalog::folders::is_startup_disk(&n.path);
            // the photos branch out here (or are here): from here down the folders are watched
            let inside = inside || (eligible && (n.own > 0 || n.children.len() != 1));
            if eligible && inside && n.count > 0 {
                let kind = if n.own > 0 { WatchKind::Photos } else { WatchKind::Parent };
                out.push(WatchedDir { path: n.path.clone(), kind, folders: n.children.len() });
            }
            walk(&n.children, out, depth + 1, inside);
        }
    }
    let mut out = Vec::new();
    walk(&cat.folder_tree(), &mut out, 0, false);
    out
}

/// What a poll of the folders found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Polled {
    /// The stamps to keep: the new ones, and the old ones of folders that couldn't be listed.
    pub stamps: Stamps,
    /// Folders whose listing changed since the last poll.
    pub changed: Vec<String>,
    /// Folders listed.
    pub listed: usize,
    /// Folders that couldn't be listed (offline).
    pub offline: usize,
    /// For a changed [`WatchKind::Parent`]: its subfolders that are new since the last poll. Only
    /// new files inside them (or inside another changed folder) count from the parent's scan, so
    /// a new day folder doesn't import new files from a folder beside it that isn't the library's.
    pub narrow: BTreeMap<String, Vec<String>>,
}

/// Stamp each of `dirs` and compare with `prev` (reads only the folders' listings). A folder
/// with no previous stamp is recorded, not reported. Stops when `cancel` is set (the stamps
/// not taken yet stay as they were).
pub fn poll_dirs(dirs: &[WatchedDir], prev: &Stamps, cancel: &AtomicBool) -> Polled {
    let dirs: Vec<WatchedDir> = dirs.iter().filter(|d| !too_broad(d)).cloned().collect();
    poll_with(&dirs, prev, cancel, |d| stamp_of(&d.path, d.kind))
}

/// [`poll_dirs`] with the stamping supplied (tests).
pub fn poll_with(dirs: &[WatchedDir], prev: &Stamps, cancel: &AtomicBool, mut stamp: impl FnMut(&WatchedDir) -> Option<Stamp>) -> Polled {
    let mut out = Polled { stamps: Stamps::new(), ..Default::default() };
    for w in dirs {
        let d = &w.path;
        let old = prev.get(d).cloned();
        if cancel.load(Ordering::Relaxed) {
            if let Some(o) = old {
                out.stamps.insert(d.clone(), o);
            }
            continue;
        }
        match stamp(w) {
            Some(s) => {
                out.listed += 1;
                if let Some(o) = old.as_ref().filter(|o| **o != s) {
                    if w.kind != WatchKind::Parent {
                        out.changed.push(d.clone());
                    } else if o.subdirs.is_empty() && !s.subdirs.is_empty() {
                        // (a stamp from an older version, without names: a parent that only lost
                        // subfolders has nothing new to find; otherwise its whole scan counts)
                        if s.count >= o.count {
                            out.changed.push(d.clone());
                        }
                    } else {
                        // the subfolders new since the last poll (folders lost in the same poll
                        // don't hide them); one that only lost folders has nothing new to find
                        let new: Vec<String> = s
                            .subdirs
                            .iter()
                            .filter(|n| !o.subdirs.contains(n))
                            .map(|n| std::path::Path::new(d).join(n).to_string_lossy().into_owned())
                            .collect();
                        if !new.is_empty() {
                            out.changed.push(d.clone());
                            out.narrow.insert(d.clone(), new);
                        }
                    }
                }
                out.stamps.insert(d.clone(), s);
            }
            None => {
                out.offline += 1;
                if let Some(o) = old {
                    out.stamps.insert(d.clone(), o);
                }
            }
        }
    }
    out
}

/// The identity of the open library: work readied for one library is dropped when another is
/// open by the time it ends (compare with [`std::sync::Arc::ptr_eq`]).
pub fn library_token(s: &crate::Session) -> std::sync::Arc<()> {
    std::sync::Arc::clone(&s.library_identity)
}

/// The "N new" count of folder `path` (a poll found that many files the library doesn't have).
pub fn new_in(s: &crate::Session, path: &str) -> Option<usize> {
    let key = lcc_catalog::query::folder_key(path);
    s.folder_new.iter().find(|(f, _)| lcc_catalog::query::folder_key(f) == key).map(|(_, n)| *n)
}

/// Whether `inner` is `outer` or a folder inside it.
pub fn within(inner: &str, outer: &str) -> bool {
    let (i, o) = (lcc_catalog::query::folder_key(inner), lcc_catalog::query::folder_key(outer));
    i == o || i.strip_prefix(o.as_str()).is_some_and(|rest| rest.starts_with('/'))
}

/// The folders to scan for `changed`: each changed folder, without those inside another one (a
/// scan takes in the folders inside it).
pub fn scan_targets(changed: &[String]) -> Vec<String> {
    let mut v: Vec<&String> = changed.iter().collect();
    v.sort_by_key(|p| p.len());
    let mut out: Vec<String> = Vec::new();
    for p in v {
        if !out.iter().any(|o| within(p, o)) {
            out.push(p.clone());
        }
    }
    out
}

/// Keep a poll's stamps, then scan its changed folders (`scan_folder`) and add or count their
/// new files as the preferences say.
pub fn apply(
    s: &mut crate::Session,
    polled: Polled,
    mut scan_folder: impl FnMut(&mut crate::Session, &str) -> std::result::Result<crate::sync::FolderChanges, String>,
) -> Value {
    let Polled { stamps, changed, listed, offline, narrow } = polled;
    s.folder_stamps = stamps;
    let mut failed: Vec<(String, String)> = Vec::new();
    if let Err(e) = s.save_folder_stamps() {
        failed.push((String::new(), e.to_string()));
    }
    let mut new = Vec::new();
    let mut imported = 0;
    for target in scan_targets(&changed) {
        let changes = match scan_folder(s, &target) {
            Ok(mut c) => {
                keep_new_in_scope(&mut c, &changed, &narrow);
                c
            }
            Err(e) => {
                failed.push((target, e));
                continue;
            }
        };
        let n = record_scan(s, &target, changes.new.len(), &narrow);
        new.push(json!({"path": target, "count": n}));
        if n > 0 && s.folder_watch.new_files == NewFilePolicy::Add {
            match crate::sync::synchronize(s, changes, only_new()) {
                Ok(r) => {
                    imported += r.imported;
                    failed.extend(r.failed);
                }
                Err(e) => failed.push((target, e.to_string())),
            }
        }
    }
    json!({"listed": listed, "offline": offline, "changed": changed, "new": new, "imported": imported, "failed": failed})
}

/// Keep of a scan's new files those a poll found news for: a narrowed parent's scan
/// ([`Polled::narrow`]) keeps the files inside its new subfolders or inside another changed
/// folder; any other scan keeps all.
pub fn keep_new_in_scope(c: &mut crate::sync::FolderChanges, changed: &[String], narrow: &BTreeMap<String, Vec<String>>) {
    let Some((parent, subs)) = narrow.iter().find(|(p, _)| lcc_catalog::query::folder_key(p) == lcc_catalog::query::folder_key(&c.path)) else {
        return;
    };
    let scope: Vec<&String> = subs.iter().chain(changed.iter().filter(|o| *o != parent && within(o, parent))).collect();
    c.new.retain(|f| {
        let dir = std::path::Path::new(&f.path).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        scope.iter().any(|o| within(&dir, o))
    });
}

/// What a poll's synchronize does: add the new files, nothing else.
pub fn only_new() -> crate::sync::SyncChoice {
    crate::sync::SyncChoice { import_new: true, relink_moved: false, remove_missing: false, read_metadata: false, import_duplicates: false }
}

/// Note that a poll's scan of `folder` found `n` new files: its badge (newFiles: ask) says so,
/// replacing those of the folders inside it. Returns `n`.
pub fn record_new(s: &mut crate::Session, folder: &str, n: usize) -> usize {
    let before = s.folder_new.clone();
    s.folder_new.retain(|f, _| !within(f, folder));
    if n > 0 && s.folder_watch.new_files == NewFilePolicy::Ask {
        s.folder_new.insert(folder.to_string(), n);
    }
    save_if_changed(s, &before);
    n
}

/// [`record_new`] for a poll's scan of `folder`: a narrowed parent's scan ([`Polled::narrow`])
/// looked only at its new subfolders, so its `n` add to the folder's count and the counts of the
/// folders inside it stay.
pub fn record_scan(s: &mut crate::Session, folder: &str, n: usize, narrow: &BTreeMap<String, Vec<String>>) -> usize {
    let key = lcc_catalog::query::folder_key(folder);
    if !narrow.keys().any(|p| lcc_catalog::query::folder_key(p) == key) {
        return record_new(s, folder, n);
    }
    if n > 0 && s.folder_watch.new_files == NewFilePolicy::Ask {
        let before = s.folder_new.clone();
        let e = s.folder_new.entry(folder.to_string()).or_insert(0);
        *e = e.saturating_add(n);
        save_if_changed(s, &before);
    }
    n
}

/// Forget the "N new" of `folder` and the folders inside it (the person added them).
pub fn answered(s: &mut crate::Session, folder: &str) {
    let before = s.folder_new.clone();
    s.folder_new.retain(|f, _| !within(f, folder));
    save_if_changed(s, &before);
}

fn save_if_changed(s: &mut crate::Session, before: &BTreeMap<String, usize>) {
    if s.folder_new != *before
        && let Err(e) = s.save_folder_stamps()
    {
        log::warn!("folder poll: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(e: u64) -> Option<Stamp> {
        Some(Stamp { mtime: Some(1), entries: e, count: 1, subdirs: Vec::new() })
    }

    #[test]
    fn only_changed_listings_are_reported() {
        let dirs: Vec<WatchedDir> =
            ["/a", "/b", "/c", "/d"].iter().map(|s| WatchedDir { path: s.to_string(), kind: WatchKind::Photos, folders: 0 }).collect();
        let mut prev = Stamps::new();
        prev.insert("/a".into(), st(1).unwrap());
        prev.insert("/b".into(), st(2).unwrap());
        prev.insert("/c".into(), st(3).unwrap());
        let now = |d: &WatchedDir| match d.path.as_str() {
            "/a" => st(1),
            "/b" => st(20),
            "/c" => None,
            _ => st(4),
        };
        let p = poll_with(&dirs, &prev, &AtomicBool::new(false), now);
        assert_eq!(p.changed, vec!["/b".to_string()]);
        // offline keeps its old stamp; a new folder is recorded, not reported
        assert_eq!(p.stamps.get("/c"), prev.get("/c"));
        assert!(p.stamps.contains_key("/d"));
        assert_eq!((p.listed, p.offline), (3, 1));
        // cancelled: nothing listed, old stamps kept
        let p = poll_with(&dirs, &prev, &AtomicBool::new(true), |_| panic!("listed after cancel"));
        assert!(p.changed.is_empty());
        assert_eq!(p.stamps, prev);
    }

    #[test]
    fn targets_drop_folders_inside_others() {
        let c: Vec<String> = ["/p/a/b", "/p/a", "/p/ab", "/q"].iter().map(|s| s.to_string()).collect();
        let t = scan_targets(&c);
        assert_eq!(t.len(), 3);
        assert!(t.contains(&"/p/a".to_string()) && t.contains(&"/p/ab".to_string()) && !t.contains(&"/p/a/b".to_string()));
    }

    #[test]
    fn stamp_follows_the_listing() {
        let dir = std::env::temp_dir().join(format!("folder-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let d = dir.to_string_lossy().to_string();
        let a = stamp(&d).unwrap();
        assert_eq!(stamp(&d).unwrap().entries, a.entries);
        std::fs::write(dir.join("x.jpg"), b"x").unwrap();
        let b = stamp(&d).unwrap();
        assert_ne!(a.entries, b.entries);
        assert_eq!(b.count, 1);
        assert!(stamp(&dir.join("gone").to_string_lossy()).is_none());
        // as a parent: only subfolders count
        let p = stamp_of(&d, WatchKind::Parent).unwrap();
        std::fs::write(dir.join("y.jpg"), b"y").unwrap();
        assert_eq!(stamp_of(&d, WatchKind::Parent).unwrap(), p);
        std::fs::create_dir_all(dir.join("day2")).unwrap();
        assert_ne!(stamp_of(&d, WatchKind::Parent).unwrap(), p);
        assert!(stamp_of(&dir.join("gone").to_string_lossy(), WatchKind::Parent).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prefs_sanitize_and_parse() {
        let p = WatchPrefs { interval_minutes: 0, ..Default::default() }.sanitized();
        assert_eq!(p.interval_minutes, MIN_INTERVAL);
        assert!(!WatchPrefs::default().poll);
        assert_eq!(WatchPrefs::default().new_files, NewFilePolicy::Ask);
        assert_eq!(NewFilePolicy::parse("add"), Some(NewFilePolicy::Add));
        assert_eq!(NewFilePolicy::parse("nope"), None);
        let p: WatchPrefs = serde_json::from_str("{\"poll\": true, \"intervalMinutes\": 99999}").unwrap();
        assert_eq!(p.sanitized().interval_minutes, MAX_INTERVAL);
    }
}
