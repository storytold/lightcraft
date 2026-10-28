//! Bounded folder walks (issue #374): every recursive listing of the disk — import, Local's
//! Include Subfolders, Find Missing Photos, preset and LUT folders, MCP — goes through
//! [`files_in`], which stops at a depth and a number of directory entries instead of walking a
//! whole drive, follows a symbolic link or junction to a folder at most once (no loops), and
//! decides what is a folder from the directory listing itself (no extra open per entry, which
//! is what makes a large walk slow on Windows).
//!
//! It never chooses where to walk: callers pass a folder the user named. Nothing in the app walks
//! at startup or on the UI thread.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};

/// How far a walk may go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Folder levels below the starting folder that are listed (its own entries are level 1).
    pub max_depth: usize,
    /// Directory entries looked at in all (files and folders, hidden ones included).
    pub max_entries: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { max_depth: MAX_DEPTH, max_entries: MAX_ENTRIES }
    }
}

/// Folder levels a walk descends at most; photo trees are a handful of levels deep.
pub const MAX_DEPTH: usize = 32;
/// Directory entries a walk looks at at most: far more than any photo folder holds, far fewer
/// than a whole system drive.
pub const MAX_ENTRIES: usize = 1_000_000;

/// What a walk found.
#[derive(Debug, Default, PartialEq)]
pub struct Walked {
    /// The files `keep` accepted, in path order, folder by folder (depth first).
    pub files: Vec<PathBuf>,
    /// The walk stopped at a [`Limits`] bound: there may be more files.
    pub truncated: bool,
    /// Directory entries looked at.
    pub entries: usize,
}

/// Whether the entry has a dot-prefixed name or a Windows or macOS hidden attribute.
pub fn is_hidden_entry(entry: &std::fs::DirEntry) -> bool {
    hidden_with_metadata(&entry.path(), || entry.metadata())
}

/// Whether the path has a dot-prefixed name or a Windows or macOS hidden attribute.
pub fn is_hidden_path(path: &Path) -> bool {
    hidden_with_metadata(path, || std::fs::symlink_metadata(path))
}

fn hidden_with_metadata(path: &Path, metadata: impl FnOnce() -> std::io::Result<std::fs::Metadata>) -> bool {
    if path.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')) {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
        metadata().is_ok_and(|m| m.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0)
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::darwin::fs::MetadataExt;
        const UF_HIDDEN: u32 = 0x0000_8000;
        metadata().is_ok_and(|m| m.st_flags() & UF_HIDDEN != 0)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = metadata;
        false
    }
}

/// What a folder's listing says an entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Folder,
    /// A link, or an entry whose kind the listing did not say.
    Other,
}

/// What [`files_in_noting`] hands over for each folder it lists: the folder, the name and kind
/// of every entry in it (hidden ones too), and whether that is all of them (not when the walk
/// stopped inside the folder, or an entry could not be read). It answers whether to go on: `false`
/// ends the walk there (a scan cancelled; [`Walked::truncated`] is left alone: who stopped it knows).
pub type NoteFolder<'a> = &'a mut dyn FnMut(&Path, Vec<(std::ffi::OsString, EntryKind)>, bool) -> bool;

/// The files below folder `root` that `keep` accepts. Hidden entries (names starting with `.`)
/// are left out, the folder `skip` (e.g. the library's own) is never entered, and the walk stops
/// at `limits` (see [`Walked::truncated`]). Unreadable folders are skipped.
pub fn files_in(root: &Path, skip: Option<&Path>, limits: Limits, keep: impl FnMut(&Path) -> bool) -> Walked {
    walk(root, skip, limits, keep, None)
}

/// [`files_in`], handing the listing of every folder it reads to `note`: who walks a folder to
/// find its files has, with that, the answer to "is this name there" for everything in it, and
/// need not ask the disk again (a round trip for each name on a network share).
pub fn files_in_noting(root: &Path, skip: Option<&Path>, limits: Limits, keep: impl FnMut(&Path) -> bool, note: NoteFolder) -> Walked {
    walk(root, skip, limits, keep, Some(note))
}

/// Folder listings read at the same time. A folder on a network share costs a round trip, so
/// many folders listed one after another cost a chain of them; the listing is latency, not work,
/// and a few threads waiting at once cut it by about that many.
const WORKERS: usize = 8;
/// Listed entries held ahead of the walk: the readers pause when this many wait to be used.
const READ_AHEAD: usize = 200_000;

/// An entry as the folder's listing gave it, with what the walk would otherwise ask the disk for
/// one entry at a time (the kind, where a link leads).
struct Entry {
    name: std::ffi::OsString,
    path: PathBuf,
    hidden: bool,
    /// `None`: not asked for (a hidden entry nobody wants listed) or could not be read.
    file_type: Option<std::fs::FileType>,
    link: Link,
}

/// Where a link leads.
enum Link {
    /// Not a link.
    No,
    /// A link to something that is not a folder.
    File,
    /// A link to this (canonical) folder.
    Folder(PathBuf),
    /// A link to a folder whose canonical path could not be had.
    Unresolved,
}

/// One folder as read from the disk.
struct Listing {
    /// The folder is the one to skip (or inside it).
    skipped: bool,
    /// Its entries in listing order (`Err`: one that could not be read); `None` if the folder
    /// could not be opened.
    entries: Option<Vec<Result<Entry, ()>>>,
    /// How many of `entries` could be read (are not `Err`).
    oks: usize,
    /// The folder holds more entries than were read: reading stopped at the cap.
    cut: bool,
}

impl Listing {
    fn none(skipped: bool) -> Self {
        Listing { skipped, entries: None, oks: 0, cut: false }
    }

    /// Folders (or links, which may lead to one) in it.
    fn folders(&self) -> usize {
        let entries = self.entries.as_deref().unwrap_or_default();
        entries.iter().flatten().filter(|e| !e.hidden && e.file_type.is_some_and(|t| t.is_dir() || t.is_symlink())).count()
    }

    fn held(&self) -> usize {
        self.entries.as_ref().map_or(0, Vec::len)
    }
}

#[derive(Clone, Copy)]
struct Skip<'a> {
    path: Option<&'a Path>,
    canonical: Option<&'a PathBuf>,
}

impl Skip<'_> {
    fn skips(&self, path: &Path) -> bool {
        self.path.is_some_and(|s| path == s) || self.canonical.is_some_and(|s| std::fs::canonicalize(path).is_ok_and(|p| p.starts_with(s)))
    }
}

/// Reads folder `path`: everything the walk needs from the disk for it, in the order the
/// listing gives, but no more than `cap` entries (a folder with more is `cut`: nothing past the
/// cap is asked of the disk). `want_all`: hidden entries' kinds too (the listing is handed on).
fn load(path: &Path, skip: Skip, want_all: bool, cap: usize) -> Listing {
    if skip.skips(path) {
        return Listing::none(true);
    }
    let Ok(rd) = std::fs::read_dir(path) else { return Listing::none(false) };
    let mut entries = Vec::new();
    let mut oks = 0usize;
    let mut cut = false;
    for e in rd {
        let Ok(e) = e else {
            entries.push(Err(()));
            continue;
        };
        if oks >= cap {
            cut = true;
            break;
        }
        oks = oks.saturating_add(1);
        let path = e.path();
        let hidden = is_hidden_entry(&e);
        let file_type = if hidden && !want_all { None } else { e.file_type().ok() };
        let link = match file_type {
            Some(t) if t.is_symlink() && !hidden => match std::fs::metadata(&path) {
                Ok(m) if m.is_dir() => std::fs::canonicalize(&path).map_or(Link::Unresolved, Link::Folder),
                _ => Link::File,
            },
            _ => Link::No,
        };
        entries.push(Ok(Entry { name: e.file_name(), path, hidden, file_type, link }));
    }
    Listing { skipped: false, entries: Some(entries), oks, cut }
}

enum Slot {
    Queued,
    Running,
    Done(Listing),
}

#[derive(Default)]
struct Pool {
    /// Folders for the readers to read, with their depth: the folders in listings, in the order
    /// the listings came back (the walk may take any of them first).
    queue: VecDeque<(PathBuf, usize)>,
    slots: HashMap<PathBuf, Slot>,
    /// Entries in finished listings the walk has not used yet.
    buffered: usize,
    /// What reading has cost in all, by anyone: the entries read, and at least one for each folder
    /// (an empty folder still costs a listing).
    read_entries: usize,
    stop: bool,
    /// Folders the readers (not the walk itself) have listed: opened, not the skipped one, whether
    /// or not the walk went on to use the listing.
    read_by_workers: usize,
    /// Readers of [`files_in_parallel`] still running.
    live: usize,
    /// Folders whose listing has just been handed over, in the order they finished, for
    /// [`files_in_parallel`] (only filled when [`ReadCfg::completion`] is set).
    finished: VecDeque<(PathBuf, usize)>,
}

struct Shared {
    pool: Mutex<Pool>,
    changed: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Pool> {
        self.pool.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues the folders in `listing` (a folder at `depth`) for the readers, in path order. Only
    /// plain folders: a link to a folder is entered, if at all, by the walk itself, in the order it
    /// goes through them (which of several links to one folder wins depends on that order).
    fn enqueue(&self, pool: &mut Pool, listing: &Listing, depth: usize, max_depth: usize) {
        let below = depth.saturating_add(1);
        if pool.stop || below >= max_depth {
            return; // (the walk does not list those, it only reports it stopped)
        }
        let Some(entries) = listing.entries.as_ref() else { return };
        let mut dirs: Vec<PathBuf> = entries
            .iter()
            .flatten()
            .filter(|e| !e.hidden && matches!(e.link, Link::No) && e.file_type.is_some_and(|t| t.is_dir()))
            .map(|e| e.path.clone())
            .collect();
        dirs.sort();
        for d in dirs {
            if pool.slots.insert(d.clone(), Slot::Queued).is_none() {
                pool.queue.push_back((d, below));
            }
        }
        self.changed.notify_all();
    }

    /// The walk is done or needs no more: nothing more is read ahead.
    fn halt(&self) {
        let mut pool = self.lock();
        pool.stop = true;
        pool.queue.clear();
        self.changed.notify_all();
    }
}

/// Held by the walk for as long as readers may run: when it ends, however it ends (a panic in
/// `keep` or `note` too), the readers are told to stop, so the scope can end and the panic reach
/// the caller.
struct Halt<'a>(&'a Shared);

impl Drop for Halt<'_> {
    fn drop(&mut self) {
        self.0.halt();
    }
}

/// Counts a reader of [`files_in_parallel`] as alive until it ends, however it ends.
struct Live<'a>(&'a Shared);

impl Drop for Live<'_> {
    fn drop(&mut self) {
        let mut pool = self.0.lock();
        pool.live = pool.live.saturating_sub(1);
        self.0.changed.notify_all();
    }
}

/// Held while a reader lists a folder: if the reader dies before handing the listing over, the
/// ordered walk is told, and lists the folder itself instead of waiting for ever; with `requeue`
/// (the folder's depth; [`files_in_parallel`]) the folder goes back in the queue for whoever is next.
struct Reading<'a> {
    shared: &'a Shared,
    path: PathBuf,
    requeue: Option<usize>,
    handed_over: bool,
}

impl Drop for Reading<'_> {
    fn drop(&mut self) {
        if !self.handed_over {
            let mut pool = self.shared.lock();
            match self.requeue {
                Some(depth) => {
                    pool.slots.insert(self.path.clone(), Slot::Queued);
                    pool.queue.push_back((self.path.clone(), depth));
                }
                None => {
                    pool.slots.remove(&self.path);
                }
            }
            self.shared.changed.notify_all();
        }
    }
}

/// What the readers need to know about the walk.
#[derive(Clone, Copy)]
struct ReadCfg<'a> {
    skip: Skip<'a>,
    want_all: bool,
    /// Note each listing as it finishes ([`Pool::finished`]): the consumer takes them in that order.
    completion: bool,
    max_depth: usize,
    max_entries: usize,
}

/// A reader: takes the next queued folder, lists it, queues the folders in it.
fn read_ahead(shared: &Shared, cfg: ReadCfg) {
    let _live = cfg.completion.then(|| Live(shared));
    // (no more than the walk could use of one folder, and nothing once as much has been read as
    // the walk may count: what is left it lists itself, or stops at)
    let cap = cfg.max_entries.min(READ_AHEAD);
    loop {
        let (path, depth) = {
            let mut pool = shared.lock();
            loop {
                if pool.stop {
                    return;
                }
                let paused = pool.buffered >= READ_AHEAD || pool.read_entries >= cfg.max_entries;
                match if paused { None } else { pool.queue.pop_front() } {
                    // (the walk may have taken it to list itself)
                    Some((path, depth)) if matches!(pool.slots.get(&path), Some(Slot::Queued)) => {
                        pool.slots.insert(path.clone(), Slot::Running);
                        break (path, depth);
                    }
                    Some(_) => {}
                    None => pool = shared.changed.wait(pool).unwrap_or_else(PoisonError::into_inner),
                }
            }
        };
        let mut reading = Reading { shared, path, requeue: cfg.completion.then_some(depth), handed_over: false };
        let listing = load(&reading.path, cfg.skip, cfg.want_all, cap);
        let mut pool = shared.lock();
        pool.read_entries = pool.read_entries.saturating_add(listing.oks.max(1));
        if !listing.cut {
            // (a cut one is not worth the walk's folders: it stops in it, or lists it again)
            shared.enqueue(&mut pool, &listing, depth, cfg.max_depth);
        }
        pool.buffered = pool.buffered.saturating_add(listing.held());
        if listing.entries.is_some() && !listing.skipped {
            pool.read_by_workers = pool.read_by_workers.saturating_add(1);
        }
        pool.slots.insert(reading.path.clone(), Slot::Done(listing));
        if cfg.completion {
            pool.finished.push_back((reading.path.clone(), depth));
        }
        reading.handed_over = true;
        shared.changed.notify_all();
    }
}

fn walk(root: &Path, skip: Option<&Path>, limits: Limits, keep: impl FnMut(&Path) -> bool, note: Option<NoteFolder>) -> Walked {
    walk_with(root, skip, limits, keep, note, WORKERS).0
}

/// How long [`files_in_parallel`] waits for a listing before it looks at `cancel` again.
const CANCEL_POLL: std::time::Duration = std::time::Duration::from_millis(100);

/// What the readers made of `path`, once they have: waits while one is still listing it. `None`
/// when they did not get to it (a queued folder they now pass by, or one they never had).
fn take_read(shared: &Shared, path: &Path) -> Option<Listing> {
    let mut pool = shared.lock();
    loop {
        match pool.slots.get(path) {
            Some(Slot::Running) => pool = shared.changed.wait(pool).unwrap_or_else(PoisonError::into_inner),
            Some(Slot::Done(_)) => {
                if let Some(Slot::Done(l)) = pool.slots.remove(path) {
                    pool.buffered = pool.buffered.saturating_sub(l.held());
                    shared.changed.notify_all();
                    return Some(l);
                }
            }
            Some(Slot::Queued) => {
                pool.slots.remove(path); // the readers pass it by
                return None;
            }
            None => return None,
        }
    }
}

/// [`files_in_parallel`]'s next listing: the next one to come back; or, when no reader can take
/// the next folder (none started, or they stop reading at the entry limit), the next folder to
/// list on the calling thread (no listing). `None` when `cancel` is set or nothing is left.
fn next_listing(shared: &Shared, cancel: &std::sync::atomic::AtomicBool, max_entries: usize) -> Option<(PathBuf, usize, Option<Listing>)> {
    use std::sync::atomic::Ordering::Relaxed;
    let mut pool = shared.lock();
    loop {
        if cancel.load(Relaxed) {
            return None;
        }
        if let Some((p, d)) = pool.finished.pop_front() {
            if let Some(Slot::Done(l)) = pool.slots.remove(&p) {
                pool.buffered = pool.buffered.saturating_sub(l.held());
                shared.changed.notify_all();
                return Some((p, d, Some(l)));
            }
            continue;
        }
        if pool.slots.is_empty() {
            return None; // nothing left to read, nothing being read
        }
        if (pool.live == 0 || pool.read_entries >= max_entries)
            && let Some((p, d)) = pool.queue.pop_front()
        {
            if matches!(pool.slots.get(&p), Some(Slot::Queued)) {
                pool.slots.remove(&p);
                return Some((p, d, None));
            }
            continue;
        }
        // (woken when a listing ends; the timeout notices `cancel`)
        pool = shared.changed.wait_timeout(pool, CANCEL_POLL).unwrap_or_else(PoisonError::into_inner).0;
    }
}

/// [`files_in`] for a folder that may be on a slow (network) share, as browsing it with its
/// subfolders reads it: `threads` readers (the pool's, as the ordered walk's) list folders at the
/// same time, and each folder's files that `keep` accepts are handed to `found` (sorted) as soon
/// as that folder's listing has been read, in the order the listings finish, not path order.
/// `keep` and `found` run on the calling thread. The same limits, hidden entries and links as
/// [`files_in`] (no folder is skipped; which of several links to one folder is entered, and which
/// entries a [`Limits::max_entries`] stop leaves out, follow the completion order). Nothing more
/// is handed over once `cancel` is set, though a listing already being read ends first. Returns
/// whether the walk stopped at a [`Limits`] bound.
pub fn files_in_parallel(
    root: &Path,
    limits: Limits,
    threads: usize,
    mut keep: impl FnMut(&Path) -> bool,
    cancel: &std::sync::atomic::AtomicBool,
    mut found: impl FnMut(Vec<PathBuf>),
) -> bool {
    use std::sync::atomic::Ordering::Relaxed;
    if limits.max_depth == 0 {
        return true;
    }
    let skipper = Skip { path: None, canonical: None };
    let cfg = ReadCfg { skip: skipper, want_all: false, completion: true, max_depth: limits.max_depth, max_entries: limits.max_entries };
    let shared = Shared { pool: Mutex::new(Pool::default()), changed: Condvar::new() };
    // canonical folders reached through a link, so a link loop is walked at most once
    let mut linked: HashSet<PathBuf> = HashSet::new();
    if let Ok(c) = std::fs::canonicalize(root) {
        linked.insert(c);
    }
    let mut truncated = false;
    let mut counted = 0usize;
    std::thread::scope(|scope| {
        let _halt = Halt(&shared);
        {
            let mut pool = shared.lock();
            pool.slots.insert(root.to_path_buf(), Slot::Queued);
            pool.queue.push_back((root.to_path_buf(), 0));
        }
        let mut readers = Vec::new();
        for _ in 0..threads.max(1) {
            let shared = &shared;
            shared.lock().live += 1;
            match std::thread::Builder::new().name("walk-read".into()).spawn_scoped(scope, move || read_ahead(shared, cfg)) {
                Ok(h) => readers.push(h),
                Err(_) => shared.lock().live -= 1,
            }
        }
        'walk: loop {
            let Some((path, depth, taken)) = next_listing(&shared, cancel, limits.max_entries) else { break 'walk };
            // (as many entries as may still be counted, and one more to know it stops)
            let room = limits.max_entries.saturating_sub(counted);
            let (listing, own) = match taken {
                Some(l) if !l.cut || l.oks >= room => (l, false),
                _ => {
                    let l = load(&path, skipper, false, room);
                    let mut pool = shared.lock();
                    pool.read_entries = pool.read_entries.saturating_add(l.oks.max(1));
                    (l, true)
                }
            };
            // (a blocking read that ended after `cancel` was set: nothing more is queued or handed over)
            if cancel.load(Relaxed) {
                break 'walk;
            }
            if own && !listing.cut {
                // (the readers queue the folders in the listings they read; this one they did not)
                let mut pool = shared.lock();
                shared.enqueue(&mut pool, &listing, depth, limits.max_depth);
            }
            let cut = listing.cut;
            let Some(entries) = listing.entries else { continue };
            let below = depth.saturating_add(1);
            let mut files: Vec<PathBuf> = Vec::new();
            let mut linked_dirs: Vec<PathBuf> = Vec::new();
            let mut tripped = false;
            for e in entries.into_iter().flatten() {
                if counted >= limits.max_entries {
                    tripped = true;
                    break;
                }
                counted = counted.saturating_add(1);
                if e.hidden {
                    continue;
                }
                let Some(ft) = e.file_type else { continue };
                if ft.is_symlink() {
                    // a link (or a Windows junction): a folder is entered once, a file is a file
                    match e.link {
                        Link::Folder(c) => {
                            if linked.insert(c) {
                                linked_dirs.push(e.path);
                            }
                        }
                        Link::Unresolved => {}
                        Link::File | Link::No => {
                            if keep(&e.path) {
                                files.push(e.path);
                            }
                        }
                    }
                } else if ft.is_dir() {
                    // (a plain folder the readers queued, unless it is below the depth limit)
                    truncated |= below >= limits.max_depth;
                } else if keep(&e.path) {
                    files.push(e.path);
                }
            }
            if below >= limits.max_depth {
                truncated |= !linked_dirs.is_empty();
            } else if !linked_dirs.is_empty() {
                let mut pool = shared.lock();
                for d in linked_dirs {
                    if pool.slots.insert(d.clone(), Slot::Queued).is_none() {
                        pool.queue.push_back((d, below));
                    }
                }
                shared.changed.notify_all();
            }
            // (a listing that ended, or `keep` that ran, while `cancel` was set is not handed over)
            if cancel.load(Relaxed) {
                break 'walk;
            }
            if !files.is_empty() {
                files.sort();
                found(files);
            }
            // (more entries than could be read: the limit is next)
            if tripped || cut {
                truncated = true;
                break 'walk;
            }
        }
        shared.halt();
        for r in readers {
            let _ = r.join(); // (a reader that died has put its folder back, and this thread lists it)
        }
    });
    truncated
}

/// The walk, with up to `workers` threads reading folders ahead of it (none: one folder at a
/// time); the second value is how many folders they read that the walk used.
///
/// The walk itself is one thread going through the folders depth first, in path order, exactly as
/// a single-threaded walk would: it alone counts entries against the limits, enters linked
/// folders, and calls `keep` and `note`. The readers only fetch what it is about to ask the disk
/// for, so the result does not depend on how many there are or how fast each folder comes back. A
/// folder the readers have not got to, it lists itself, as it does every folder reached through a
/// link; and once a limit stops the walk, the readers stop too.
fn walk_with(
    root: &Path,
    skip: Option<&Path>,
    limits: Limits,
    mut keep: impl FnMut(&Path) -> bool,
    mut note: Option<NoteFolder>,
    workers: usize,
) -> (Walked, usize) {
    let mut out = Walked::default();
    // canonical folders reached through a link, so a link loop is walked at most once
    let mut linked: HashSet<PathBuf> = HashSet::new();
    if let Ok(c) = std::fs::canonicalize(root) {
        linked.insert(c);
    }
    // Include aliases to the library in the exclusion, not just its lexical path.
    let skip_canonical = skip.and_then(|p| std::fs::canonicalize(p).ok());
    let skipper = Skip { path: skip, canonical: skip_canonical.as_ref() };
    let want_all = note.is_some();
    let cfg = ReadCfg { skip: skipper, want_all, completion: false, max_depth: limits.max_depth, max_entries: limits.max_entries };
    let shared = Shared { pool: Mutex::new(Pool::default()), changed: Condvar::new() };
    let read_by_workers = std::thread::scope(|scope| {
        let _halt = Halt(&shared);
        let mut readers = Vec::new();
        let mut stopped = false;
        // (path, depth of the folder it is in, is a folder); popped in path order
        let mut stack: Vec<(PathBuf, usize, bool)> = vec![(root.to_path_buf(), 0, true)];
        while let Some((path, depth, is_dir)) = stack.pop() {
            if !is_dir {
                if keep(&path) {
                    out.files.push(path);
                }
                continue;
            }
            if depth >= limits.max_depth {
                if !skipper.skips(&path) {
                    out.truncated = true;
                }
                continue;
            }
            let taken = take_read(&shared, &path);
            // (as many entries as the walk may still count, and one more to know it stops)
            let room = limits.max_entries.saturating_sub(out.entries);
            let (listing, own) = match taken {
                Some(l) if !l.cut || l.oks >= room => (l, false),
                _ => {
                    let l = load(&path, skipper, want_all, room);
                    let mut pool = shared.lock();
                    pool.read_entries = pool.read_entries.saturating_add(l.oks.max(1));
                    (l, true)
                }
            };
            // a first folder with folders in it is worth readers; one without, or a chain of single
            // folders, is not; nor is one the walk stops in
            if own && !stopped && !listing.cut && (!readers.is_empty() || (workers > 0 && listing.folders() >= 2)) {
                if readers.is_empty() {
                    for _ in 0..workers {
                        let shared = &shared;
                        if let Ok(h) = std::thread::Builder::new().name("walk-read".into()).spawn_scoped(scope, move || read_ahead(shared, cfg)) {
                            readers.push(h);
                        }
                    }
                }
                let mut pool = shared.lock();
                shared.enqueue(&mut pool, &listing, depth, limits.max_depth);
            }
            if listing.skipped {
                continue;
            }
            let cut = listing.cut;
            let Some(entries) = listing.entries else { continue };
            let mut children: Vec<(PathBuf, bool)> = Vec::new();
            // (the folder's listing, for who asked for it; all of it unless something below says otherwise)
            let mut listed = note.is_some().then(Vec::new);
            let mut complete = true;
            let mut tripped = false;
            for e in entries {
                let Ok(e) = e else {
                    complete = false;
                    continue;
                };
                if out.entries >= limits.max_entries {
                    tripped = true;
                    break;
                }
                out.entries = out.entries.saturating_add(1);
                // (a hidden entry's kind is only asked for when the listing is wanted)
                if e.hidden && listed.is_none() {
                    continue;
                }
                if let Some(listed) = listed.as_mut() {
                    let kind = match &e.file_type {
                        Some(t) if t.is_symlink() => EntryKind::Other,
                        Some(t) if t.is_dir() => EntryKind::Folder,
                        Some(t) if t.is_file() => EntryKind::File,
                        _ => EntryKind::Other,
                    };
                    listed.push((e.name, kind));
                }
                if e.hidden {
                    continue;
                }
                let Some(ft) = e.file_type else { continue };
                let dir = if ft.is_symlink() {
                    // a link (or a Windows junction): a folder is entered once, a file is a file
                    match e.link {
                        Link::Folder(c) => {
                            if !linked.insert(c) {
                                continue;
                            }
                            true
                        }
                        Link::Unresolved => continue,
                        Link::File | Link::No => false,
                    }
                } else {
                    ft.is_dir()
                };
                children.push((e.path, dir));
            }
            // (more entries than could be read: the limit is next)
            tripped |= cut;
            if tripped {
                out.truncated = true;
                complete = false;
                if !stopped {
                    stopped = true;
                    shared.halt();
                }
            }
            // (`note` says whether to go on: `false` ends the walk here, and the readers stop below;
            // `Walked::truncated` is left alone, who stopped the walk knows)
            if let (Some(note), Some(listed)) = (note.as_mut(), listed)
                && !note(&path, listed, complete)
            {
                break;
            }
            children.sort();
            let below = depth.saturating_add(1);
            stack.extend(children.into_iter().rev().map(|(p, d)| (p, below, d)));
        }
        shared.halt();
        for r in readers {
            let _ = r.join(); // (a reader that died has told the walk, which listed its folder itself)
        }
        shared.lock().read_by_workers
    });
    (out, read_by_workers)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A parallel walk finds what a walk finds (the same files; the same limit and link rules),
    /// handing over each folder's files once, and stops when cancelled.
    /// (From #2480 by Matt Sivak, adapted to the shared reader pool: `found` runs on the caller.)
    #[test]
    fn a_parallel_walk_finds_what_a_walk_finds() {
        let root = tmp("parallel");
        for d in ["a/b/c", "d", "e/.hidden"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        for f in ["1.jpg", "a/2.jpg", "a/b/3.jpg", "a/b/c/4.jpg", "d/5.jpg", "e/.hidden/6.jpg", ".7.jpg", "a/8.txt"] {
            std::fs::write(root.join(f), b"x").unwrap();
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("a"), root.join("d/link-to-a")).unwrap();
        let keep = |p: &Path| p.extension().is_some_and(|e| e == "jpg");
        let walk_all = |limits: Limits, threads: usize| {
            let mut got = Vec::new();
            let mut calls = 0usize;
            let truncated = files_in_parallel(&root, limits, threads, keep, &std::sync::atomic::AtomicBool::new(false), |files| {
                calls += 1;
                got.extend(files);
            });
            got.sort();
            (got, truncated, calls)
        };
        for threads in [1, 4] {
            let (got, truncated, calls) = walk_all(Limits::default(), threads);
            assert_eq!(got, files_in(&root, None, Limits::default(), keep).files, "{threads} threads");
            assert!(!truncated);
            // once per folder with photos: root, a, a/b, a/b/c, d (and, through the link, a, a/b and a/b/c
            // again, as `files_in` walks them)
            assert_eq!(calls, if cfg!(unix) { 8 } else { 5 });
        }
        let shallow = Limits { max_depth: 2, ..Limits::default() };
        let (got, truncated, _) = walk_all(shallow, 4);
        assert_eq!(got, files_in(&root, None, shallow, keep).files);
        assert!(truncated);
        let cancelled = std::sync::atomic::AtomicBool::new(true);
        assert!(!files_in_parallel(&root, Limits::default(), 4, keep, &cancelled, |_| panic!("nothing is listed once cancelled")));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A walk cancelled while it runs hands over nothing more and returns; one that hits the
    /// entry limit says so, whatever order the folders finish in.
    #[test]
    fn a_parallel_walk_stops_when_cancelled_or_at_the_entry_limit() {
        let root = tmp("parallel-stop");
        for d in 0..6 {
            std::fs::create_dir_all(root.join(format!("d{d}"))).unwrap();
            for f in 0..5 {
                std::fs::write(root.join(format!("d{d}/{f}.jpg")), b"x").unwrap();
            }
        }
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let mut calls = 0usize;
        files_in_parallel(
            &root,
            Limits::default(),
            4,
            |_| true,
            &cancel,
            |_| {
                calls += 1;
                cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            },
        );
        assert_eq!(calls, 1);
        let mut got = 0usize;
        let limits = Limits { max_depth: 8, max_entries: 12 };
        let truncated = files_in_parallel(&root, limits, 4, |_| true, &std::sync::atomic::AtomicBool::new(false), |files| got += files.len());
        assert!(truncated);
        assert!(got <= 12, "{got}");
        let _ = std::fs::remove_dir_all(&root);
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lc-walk-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn hidden_names_and_platform_attributes_are_excluded_from_walks() {
        let root = tmp("hidden-attributes");
        for folder in [".dot", "Hidden"] {
            std::fs::create_dir_all(root.join(folder)).unwrap();
            std::fs::write(root.join(folder).join("inside.jpg"), b"x").unwrap();
        }
        for file in ["Shown.jpg", ".dot.jpg", "hidden.jpg"] {
            std::fs::write(root.join(file), b"x").unwrap();
        }
        for name in ["Hidden", "hidden.jpg"] {
            #[cfg(windows)]
            assert!(std::process::Command::new("attrib").arg("+h").arg(root.join(name)).status().unwrap().success());
            #[cfg(target_os = "macos")]
            assert!(std::process::Command::new("chflags").arg("hidden").arg(root.join(name)).status().unwrap().success());
            assert_eq!(is_hidden_path(&root.join(name)), cfg!(any(windows, target_os = "macos")));
        }
        let mut found = files_in(&root, None, Limits::default(), |_| true).files;
        found.sort();
        let mut expected = vec![root.join("Shown.jpg")];
        if !cfg!(any(windows, target_os = "macos")) {
            expected.extend([root.join("Hidden/inside.jpg"), root.join("hidden.jpg")]);
        }
        expected.sort();
        assert_eq!(found, expected);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Given a folder with files, a folder inside it, a hidden file and a link, when it is walked
    /// with its listings noted, then the walk finds what it finds without them, and each folder's
    /// listing holds every entry by name with its kind (hidden ones too, a link as neither file nor
    /// folder) and says it is all of them; a walk that stops inside a folder says it is not.
    #[test]
    fn a_walk_notes_each_folders_listing() {
        let root = tmp("noting");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        for name in ["a.jpg", ".hidden", "sub/b.jpg"] {
            std::fs::write(root.join(name), b"x").unwrap();
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("a.jpg"), root.join("link.jpg")).unwrap();
        let mut noted: Vec<(PathBuf, Vec<(String, EntryKind)>, bool)> = Vec::new();
        let walked = files_in_noting(&root, None, Limits::default(), |_| true, &mut |dir, entries, complete| {
            let mut entries: Vec<(String, EntryKind)> = entries.into_iter().map(|(n, k)| (n.to_string_lossy().to_string(), k)).collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            noted.push((dir.to_path_buf(), entries, complete));
            true
        });
        assert_eq!(walked, files_in(&root, None, Limits::default(), |_| true), "the same walk, noted or not");
        let named = |n: &str, k: EntryKind| (n.to_string(), k);
        let mut top = vec![named(".hidden", EntryKind::File), named("a.jpg", EntryKind::File), named("sub", EntryKind::Folder)];
        if cfg!(unix) {
            top.insert(2, named("link.jpg", EntryKind::Other));
        }
        assert_eq!(noted, [(root.clone(), top, true), (root.join("sub"), vec![named("b.jpg", EntryKind::File)], true)]);

        // stopped inside the first folder: its listing is not all of it
        let mut cut = Vec::new();
        let walked = files_in_noting(&root, None, Limits { max_depth: 8, max_entries: 2 }, |_| true, &mut |dir, entries, complete| {
            cut.push((dir.to_path_buf(), entries.len(), complete));
            true
        });
        assert!(walked.truncated);
        assert_eq!(cut.first().map(|(dir, n, complete)| (dir.clone(), *n, *complete)), Some((root.clone(), 2, false)));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn files_come_in_path_order_without_hidden_ones_or_the_skipped_folder() {
        let d = tmp("order");
        for f in ["b/2.jpg", "a/1.jpg", "a/x/0.jpg", "c.jpg", ".hidden/h.jpg", "a/.h.jpg", "lib/l.jpg", "a/n.txt"] {
            let p = d.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b"x").unwrap();
        }
        let lib = d.join("lib");
        let w = files_in(&d, Some(&lib), Limits::default(), |p| p.extension().is_some_and(|e| e == "jpg"));
        let rel: Vec<String> = w.files.iter().map(|p| p.strip_prefix(&d).unwrap().to_string_lossy().replace('\\', "/")).collect();
        assert_eq!(rel, ["a/1.jpg", "a/x/0.jpg", "b/2.jpg", "c.jpg"]);
        assert!(!w.truncated);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Issue #374: a walk never runs on without end — a tree deeper than the limit, or with more
    /// entries than the limit, stops and says so.
    #[test]
    fn deep_and_wide_trees_stop_at_the_limits() {
        let d = tmp("bounds");
        let mut deep = d.join("deep");
        for i in 0..12 {
            deep = deep.join(format!("l{i}"));
        }
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("bottom.jpg"), b"x").unwrap();
        let wide = d.join("wide");
        std::fs::create_dir_all(&wide).unwrap();
        for i in 0..50 {
            std::fs::write(wide.join(format!("{i:02}.jpg")), b"x").unwrap();
        }
        let all = |_: &Path| true;
        let w = files_in(&d.join("deep"), None, Limits { max_depth: 5, max_entries: 1000 }, all);
        assert!(w.files.is_empty() && w.truncated, "{w:?}");
        let w = files_in(&d.join("deep"), None, Limits { max_depth: 13, max_entries: 1000 }, all);
        assert_eq!((w.files.len(), w.truncated), (1, false), "{w:?}");
        let w = files_in(&wide, None, Limits { max_depth: 5, max_entries: 20 }, all);
        assert_eq!((w.files.len(), w.entries, w.truncated), (20, 20, true));
        let w = files_in(&wide, None, Limits::default(), all);
        assert_eq!((w.files.len(), w.truncated), (50, false));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Folder imports must not revisit the user's own library through symlinks
    /// or through an alias to the library's parent.
    #[cfg(unix)]
    #[test]
    fn skipped_library_stays_skipped_through_symlinks_and_ancestors() {
        let d = tmp("skip-alias");
        let root = d.join("incoming");
        let library = d.join("library");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(library.join("Originals")).unwrap();
        std::fs::write(root.join("ordinary.jpg"), b"photo").unwrap();
        std::fs::write(library.join("Originals").join("library-only.jpg"), b"original").unwrap();
        std::os::unix::fs::symlink(&library, root.join("library-link")).unwrap();
        std::os::unix::fs::symlink(&d, root.join("parent-link")).unwrap();

        let photos = files_in(&root, Some(&library), Limits::default(), |p| p.extension().is_some_and(|e| e == "jpg"));
        assert!(photos.files.contains(&root.join("ordinary.jpg")));
        assert!(!photos.files.iter().any(|p| p.ends_with("library-only.jpg")), "{photos:?}");

        // Scanning a subfolder already inside the excluded library is also forbidden.
        let inside = files_in(&library.join("Originals"), Some(&library), Limits::default(), |_| true);
        assert!(inside.files.is_empty(), "{inside:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A link back up the tree (a loop) is followed at most once: the walk ends.
    #[cfg(unix)]
    #[test]
    fn link_loops_end() {
        let d = tmp("loop");
        std::fs::create_dir_all(d.join("a/b")).unwrap();
        std::fs::write(d.join("a/b/p.jpg"), b"x").unwrap();
        std::os::unix::fs::symlink(&d, d.join("a/b/up")).unwrap();
        std::os::unix::fs::symlink(d.join("a"), d.join("alias")).unwrap();
        let w = files_in(&d, None, Limits::default(), |_| true);
        assert!(!w.truncated, "{w:?}");
        assert!(w.files.len() <= 2, "{w:?}");
        assert!(w.files.iter().any(|p| p.ends_with("a/b/p.jpg")), "{w:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// What a walk gives back and calls, to compare two walks of the same tree.
    type Seen = (Walked, Vec<(PathBuf, Vec<(std::ffi::OsString, EntryKind)>, bool)>);

    fn seen(root: &Path, skip: Option<&Path>, limits: Limits, workers: usize) -> (Seen, usize) {
        let mut noted = Vec::new();
        let (walked, by_workers) = walk_with(
            root,
            skip,
            limits,
            |p| p.extension().is_some_and(|e| e == "jpg"),
            Some(&mut |d, e, c| {
                noted.push((d.to_path_buf(), e, c));
                true
            }),
            workers,
        );
        ((walked, noted), by_workers)
    }

    /// A tree with wide and deep parts, hidden entries, a skipped folder, links to folders (twice
    /// to one, one up the tree) and to a file, and a folder that cannot be read.
    fn tree(tag: &str) -> (PathBuf, PathBuf) {
        let d = tmp(tag);
        for i in 0..40 {
            for j in 0..3 {
                let dir = d.join(format!("w{i:02}/s{j}"));
                std::fs::create_dir_all(&dir).unwrap();
                for k in 0..3 {
                    std::fs::write(dir.join(format!("{k}.jpg")), b"x").unwrap();
                }
                std::fs::write(dir.join(".hidden.jpg"), b"x").unwrap();
            }
            std::fs::write(d.join(format!("w{i:02}/top.jpg")), b"x").unwrap();
        }
        let mut deep = d.join("deep");
        for i in 0..10 {
            deep = deep.join(format!("l{i}"));
            std::fs::create_dir_all(&deep).unwrap();
            std::fs::write(deep.join("p.jpg"), b"x").unwrap();
        }
        std::fs::create_dir_all(d.join(".hidden/in")).unwrap();
        std::fs::write(d.join(".hidden/in/h.jpg"), b"x").unwrap();
        std::fs::create_dir_all(d.join("lib/x")).unwrap();
        std::fs::write(d.join("lib/x/l.jpg"), b"x").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            symlink(d.join("w05"), d.join("alias-a")).unwrap();
            symlink(d.join("w05"), d.join("alias-b")).unwrap();
            symlink(&d, d.join("w07/s1/up")).unwrap();
            symlink(d.join("deep/l0"), d.join("w39/to-deep")).unwrap();
            symlink(d.join("w01/top.jpg"), d.join("w39/file-link.jpg")).unwrap();
            symlink(d.join("nowhere"), d.join("w39/dangling")).unwrap();
        }
        (d.clone(), d.join("lib"))
    }

    /// Given a tree with every kind of entry the walk treats specially, when it is walked with
    /// readers at the same time, then the files, the counts, the stop flag and every listing it
    /// hands over (and their order) equal those of the walk that lists one folder at a time, for
    /// each way of limiting it.
    #[test]
    fn readers_at_the_same_time_give_the_one_at_a_time_result() {
        let (d, lib) = tree("equal");
        let (oracle, by_workers) = seen(&d, Some(&lib), Limits::default(), 0);
        assert_eq!(by_workers, 0);
        assert!(oracle.0.files.len() > 400 && !oracle.0.truncated, "{}", oracle.0.files.len());
        let all = oracle.0.entries;
        let mut limits = vec![Limits::default()];
        limits.extend([1, 2, 3, 5, 11, 12, 13].map(|max_depth| Limits { max_depth, max_entries: MAX_ENTRIES }));
        limits.extend([0, 1, 7, 100, 401, 777, all - 1, all, all + 1].map(|max_entries| Limits { max_depth: MAX_DEPTH, max_entries }));
        limits.push(Limits { max_depth: 4, max_entries: 500 });
        for limits in limits {
            let (serial, _) = seen(&d, Some(&lib), limits, 0);
            for workers in [1, 3, 8] {
                let (parallel, _) = seen(&d, Some(&lib), limits, workers);
                assert_eq!(parallel, serial, "{limits:?} with {workers} readers");
            }
        }
        // without the folder to skip, and without a listing wanted
        let (serial, _) = seen(&d, None, Limits::default(), 0);
        assert_eq!(seen(&d, None, Limits::default(), 8).0, serial);
        let plain = |w| walk_with(&d, Some(&lib), Limits::default(), |_| true, None, w).0;
        assert_eq!(plain(8), plain(0));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The readers do the listing of a tree with many folders (and the walk alone lists a tree
    /// that is one folder, or a chain, without them).
    #[test]
    fn many_folders_are_read_by_the_readers_and_a_small_tree_by_the_walk() {
        let (d, lib) = tree("who");
        let (_, by_workers) = seen(&d, Some(&lib), Limits::default(), 8);
        assert!(by_workers > 20, "{by_workers}");

        let flat = tmp("flat");
        std::fs::write(flat.join("a.jpg"), b"x").unwrap();
        assert_eq!(seen(&flat, None, Limits::default(), 8).1, 0);
        let chain = tmp("chain");
        std::fs::create_dir_all(chain.join("a/b/c")).unwrap();
        assert_eq!(seen(&chain, None, Limits::default(), 8).1, 0);
        for p in [d, flat, chain] {
            let _ = std::fs::remove_dir_all(p);
        }
    }

    /// A folder that cannot be opened is passed over by both, and its siblings still come.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_folder_is_skipped_by_both() {
        use std::os::unix::fs::PermissionsExt;
        let (d, lib) = tree("unreadable");
        let locked = d.join("w10/s1");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let readable = std::fs::read_dir(&locked).is_ok(); // (running as root)
        let (serial, _) = seen(&d, Some(&lib), Limits::default(), 0);
        let (parallel, _) = seen(&d, Some(&lib), Limits::default(), 8);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(parallel, serial);
        assert!(readable || !serial.0.files.iter().any(|p| p.starts_with(&locked)));
        assert!(serial.0.files.iter().any(|p| p.starts_with(d.join("w10/s2"))));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A walk that stops early leaves no reader running: it returns (the scope would not end
    /// otherwise) and a second walk is as good as the first.
    #[test]
    fn a_walk_that_stops_early_returns() {
        let (d, lib) = tree("early");
        let limits = Limits { max_depth: MAX_DEPTH, max_entries: 5 };
        let a = seen(&d, Some(&lib), limits, 8).0;
        assert!(a.0.truncated);
        assert_eq!(seen(&d, Some(&lib), limits, 8).0, a);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A listing noted that answers `false` ends the walk there, however many readers are ahead of
    /// it: the walk returns (the scope would not end otherwise), is not truncated, and has noted no
    /// folder after the one that said stop.
    #[test]
    fn a_note_that_says_stop_ends_a_parallel_walk() {
        let (d, lib) = tree("note-stop");
        let mut noted = 0;
        let (walked, _) = walk_with(
            &d,
            Some(&lib),
            Limits::default(),
            |p| p.extension().is_some_and(|e| e == "jpg"),
            Some(&mut |_, _, _| {
                noted += 1;
                noted < 4
            }),
            8,
        );
        assert_eq!(noted, 4, "no folder is noted after the one that said stop");
        assert!(!walked.truncated, "who stopped the walk knows: it is not a limit");
        assert!(walked.files.len() < 400, "the walk ended early: {} files", walked.files.len());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Given two links to one folder with subfolders, one deep in the tree and one in a folder
    /// that is listed sooner, when it is walked, then the folder is entered once, by the first link
    /// the walk meets, and the readers never list anything the walk does not use.
    #[cfg(unix)]
    #[test]
    fn readers_do_not_read_what_a_second_link_to_a_folder_leads_to() {
        let d = tmp("two-links");
        let target = d.join("t");
        for i in 0..4 {
            std::fs::create_dir_all(target.join(format!("s{i}/u"))).unwrap();
            std::fs::write(target.join(format!("s{i}/u/p.jpg")), b"x").unwrap();
        }
        std::fs::create_dir_all(d.join("a/x/y")).unwrap();
        std::fs::create_dir_all(d.join("b")).unwrap();
        std::fs::create_dir_all(d.join("c")).unwrap();
        std::os::unix::fs::symlink(&target, d.join("a/x/y/link1")).unwrap();
        std::os::unix::fs::symlink(&target, d.join("b/link2")).unwrap();
        let (serial, _) = seen(&d, None, Limits::default(), 0);
        for _ in 0..50 {
            let (parallel, by_workers) = seen(&d, None, Limits::default(), 8);
            assert_eq!(parallel, serial);
            assert!(by_workers <= parallel.1.len(), "{by_workers} read, {} listed", parallel.1.len());
        }
        let listed = |p: &PathBuf| serial.1.iter().filter(|(dir, ..)| dir.starts_with(p)).count();
        assert_eq!((listed(&d.join("a")), listed(&d.join("b"))), (3 + 4 + 4 + 1, 1), "entered through a/x/y/link1 only, by the walk's order");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The entry limit bounds what is read: a listing stops at its cap, and the readers start no
    /// folder once what has been read (an empty folder counts one) reaches the limit, so they list
    /// at most `max_entries + WORKERS` folders however the tree is shaped.
    #[test]
    fn the_entry_limit_bounds_what_is_read() {
        let d = tmp("cap");
        for i in 0..50 {
            std::fs::write(d.join(format!("{i:02}.jpg")), b"x").unwrap();
        }
        let none = Skip { path: None, canonical: None };
        let l = load(&d, none, false, 21);
        assert_eq!((l.oks, l.cut, l.held()), (21, true, 21));
        let l = load(&d, none, false, 50);
        assert_eq!((l.oks, l.cut), (50, false));
        let l = load(&d, none, false, 0);
        assert_eq!((l.oks, l.cut), (0, true));
        let _ = std::fs::remove_dir_all(&d);

        let (d, lib) = tree("cap-walk");
        for max_entries in [7, 100, 401] {
            let limits = Limits { max_depth: MAX_DEPTH, max_entries };
            for _ in 0..20 {
                let (parallel, by_workers) = seen(&d, Some(&lib), limits, 8);
                assert_eq!(parallel, seen(&d, Some(&lib), limits, 0).0, "{limits:?}");
                assert!(by_workers <= parallel.1.len() + 2 * WORKERS, "{limits:?}: {by_workers} read, {} listed", parallel.1.len());
            }
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Given a folder of many files next to one of thousands of empty folders, when the entry limit
    /// is reached in the files, then the readers have not listed the empty folders: they list at
    /// most `max_entries + WORKERS` folders (an empty one costs one entry's worth).
    #[test]
    fn readers_do_not_run_through_empty_folders_past_the_limit() {
        let d = tmp("empties");
        std::fs::create_dir_all(d.join("a")).unwrap();
        for i in 0..200 {
            std::fs::write(d.join(format!("a/{i:03}.jpg")), b"x").unwrap();
        }
        for i in 0..600 {
            std::fs::create_dir_all(d.join(format!("b/{i:03}"))).unwrap();
        }
        let limits = Limits { max_depth: MAX_DEPTH, max_entries: 100 };
        for _ in 0..20 {
            let (walked, by_workers) = seen(&d, None, limits, 8);
            assert_eq!(walked, seen(&d, None, limits, 0).0);
            assert!(by_workers <= limits.max_entries + WORKERS, "{by_workers} listed by readers");
            assert!(by_workers < 600, "{by_workers}");
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Given a tree wide enough to start readers, when `keep` panics, then the panic reaches the
    /// caller (the readers are stopped, so the walk's scope can end) instead of the walk hanging.
    #[test]
    fn a_panic_during_a_walk_reaches_the_caller() {
        let (d, lib) = tree("panic");
        let (tx, rx) = std::sync::mpsc::channel();
        let root = d.clone();
        std::thread::spawn(move || {
            let r = std::panic::catch_unwind(|| files_in(&root, Some(&lib), Limits::default(), |_| panic!("keep failed")));
            let _ = tx.send(r.is_err());
        });
        let got = rx.recv_timeout(std::time::Duration::from_secs(20));
        assert_eq!(got, Ok(true), "the walk hung or did not panic");
        let _ = std::fs::remove_dir_all(&d);
    }
}
