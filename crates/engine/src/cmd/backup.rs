//! `library.backup`: back up the library's catalog to a dated folder (see
//! [`lightcraft_catalog::backup`]): a folder that holds the library as it was taken, and that
//! shares with the backup before it the files that didn't change.
//!
//! The backup reads the library's files as they are on disk, with a row in the activity stack.
//! It runs to its end before the command returns, or (`background`: what the app asks for) on a
//! worker thread while the library stays in use. Not the photos: the catalog, its log, and the
//! presets, view and preferences files.
//!
//! `library.restore` puts a backup back ([`lightcraft_catalog::restore`]): the library becomes
//! what the backup holds, what it was is kept in `Replaced` in its folder, and the session goes
//! on in the restored library. A backup is not opened where it is. With `background` the backup
//! is copied on a worker thread ([`copy`]) and moved into place between commands ([`settle`]).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use lightcraft_catalog::backup::{self, BackupOptions, BackupReport, Own, Watch};
use lightcraft_catalog::restore::{self, RestoreOptions};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd};
use crate::activity::{Cancel, TaskGuard, Unit};
use crate::{Result, Session};

/// The folder in the library that backups go to unless another is asked for.
pub const BACKUPS: &str = "Backups";
/// The library's other files a backup takes along: every one it has.
const ALSO: &[&str] = &crate::library::SETTINGS_FILES;

/// A backup under way, or the last one made.
#[derive(Default)]
pub struct BackupJob {
    /// The library it is of.
    pub library: PathBuf,
    /// The folder it is made in.
    pub dest: PathBuf,
    /// Shared with the backup's row in the activity stack (its ✕ sets it).
    pub cancel: Arc<AtomicBool>,
    pub finished: AtomicBool,
    /// How it ended.
    pub outcome: std::sync::Mutex<Option<std::result::Result<BackupReport, String>>>,
    /// When it was started (the session clock's time).
    pub started: String,
    /// It goes to the folder the library's backups go to (not to one given for once).
    pub own: bool,
    /// The session has taken note of how it ended (see [`note_finished`]).
    noted: AtomicBool,
    /// How many days the library keeps its backups, when this one is of its own folder: older
    /// ones are removed once it is made (`None`: none are).
    pub keep_days: Option<u32>,
    /// The library's own id, which the backup says it is of.
    pub library_id: Option<String>,
    /// The backups that were removed after it, by name, and the ones that could not be.
    pub removed: std::sync::Mutex<Vec<String>>,
    pub not_removed: std::sync::Mutex<Vec<String>>,
}

impl BackupJob {
    /// How it ended (`None` while it runs).
    pub fn outcome(&self) -> Option<std::result::Result<BackupReport, String>> {
        self.outcome.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    pub fn json(&self) -> Value {
        let outcome = self.outcome();
        let made = outcome.as_ref().is_some_and(|o| o.is_ok());
        json!({
            "running": !self.finished.load(Ordering::Relaxed),
            // (stopped: not a backup that was whole before the stop was seen)
            "cancelled": self.cancel.load(Ordering::Relaxed) && !made,
            "library": self.library.to_string_lossy(),
            "dest": self.dest.to_string_lossy(),
            "report": outcome.as_ref().and_then(|o| o.as_ref().ok()).and_then(|r| serde_json::to_value(r).ok()),
            "error": outcome.as_ref().and_then(|o| o.as_ref().err()),
            // (older than the library keeps them, removed once this backup was made)
            "removed": *self.removed.lock().unwrap_or_else(std::sync::PoisonError::into_inner),
            "notRemoved": *self.not_removed.lock().unwrap_or_else(std::sync::PoisonError::into_inner),
        })
    }
}

/// Reports to the backup's row, and stops when its ✕ was pressed.
struct Row<'a> {
    task: &'a TaskGuard,
    cancel: &'a AtomicBool,
}

impl Watch for Row<'_> {
    fn progress(&self, name: &str, done: u64, total: u64) {
        self.task.progress(done, total);
        self.task.detail(name);
    }
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

/// A name for a backup made at `now` (ISO 8601; the session's clock is UTC, and the name says
/// so) that no folder in `dest` has.
fn free_name(dest: &Path, now: &str) -> String {
    let time: String = now
        .chars()
        .take(19)
        .map(|c| {
            if c == 'T' {
                ' '
            } else if c == ':' {
                '.'
            } else {
                c
            }
        })
        .collect();
    let base = if time.trim().is_empty() { "backup".to_string() } else { format!("{time} UTC") };
    let taken = |name: &str| dest.join(name).exists() || dest.join(format!("{name}.partial")).exists();
    if !taken(&base) {
        return base;
    }
    (2..1000u32).map(|n| format!("{base} ({n})")).find(|name| !taken(name)).unwrap_or(base)
}

fn run(name: String, job: &BackupJob, task: &TaskGuard, hold: &AtomicBool) {
    // However this ends, the job is over: also if something below should panic (it then says so,
    // and the next backup is not refused as "already running").
    struct Over<'a>(&'a BackupJob);
    impl Drop for Over<'_> {
        fn drop(&mut self) {
            let mut outcome = self.0.outcome.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if outcome.is_none() {
                *outcome = Some(Err("the backup stopped on an internal error".into()));
            }
            self.0.finished.store(true, Ordering::Relaxed);
        }
    }
    let _over = Over(job);
    // (tests hold a backup here, to look at its row)
    while hold.load(Ordering::SeqCst) && !job.cancel.load(Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let options = BackupOptions {
        name: &name,
        also: ALSO,
        made: Some(job.started.as_str()).filter(|at| !at.trim().is_empty()),
        library_id: job.library_id.as_deref(),
    };
    let made = backup::back_up(&job.library, &job.dest, &options, &Row { task, cancel: &job.cancel });
    match &made {
        Ok(r) => log::info!("library: backed up to {} ({} files, {} shared with the backup before)", r.dir.display(), r.files, r.linked),
        Err(e) => log::warn!("library: backup: {e}"),
    }
    // Now that there is a new backup, the ones older than the library keeps them go: only after
    // one that was made, and never that one.
    if let (Ok(new), Some(days), Some(id)) = (&made, job.keep_days, job.library_id.as_deref()) {
        let own = Own { library: &job.library, id };
        // (the backup is made, whatever becomes of this: a fault here is not the backup's)
        let (removed, not_removed) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            remove_outdated(&job.dest, own, &job.started, days, Some(&new.dir), &|| job.cancel.load(Ordering::Relaxed))
        }))
        .unwrap_or_else(|_| (Vec::new(), vec!["older backups: stopped on an internal error".to_string()]));
        *job.removed.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = removed;
        *job.not_removed.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = not_removed;
    }
    *job.outcome.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(made.map_err(|e| e.to_string()));
}

/// The time `days` before `now` (ISO 8601): backups made before it are older than a library
/// that keeps them that long keeps them. `None`: there is no such time to tell (a clock that
/// can't be read), and then nothing is old.
fn kept_since(now: &str, days: u32) -> Option<String> {
    lightcraft_catalog::dates::shift_iso(now, -i64::from(days).checked_mul(24 * 60 * 60)?)
}

/// The library's own backups in `dest` that are older than it keeps them at `now`
/// ([`backup::outdated`]: only what is certainly an old backup of it, never its newest few).
fn outdated(dest: &Path, own: Own<'_>, now: &str, days: u32) -> Vec<BackupReport> {
    kept_since(now, days).map(|before| backup::outdated(dest, own, &before, now)).unwrap_or_default()
}

/// A backup's name: its folder's.
fn name_of(b: &BackupReport) -> String {
    b.dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Remove the library's backups that are older than it keeps them ([`outdated`]), oldest first,
/// until told to stop, and never `spare` (the backup just made): the names of the ones removed,
/// and of the ones that could not be (each with why).
fn remove_outdated(dest: &Path, own: Own<'_>, now: &str, days: u32, spare: Option<&Path>, stop: &dyn Fn() -> bool) -> (Vec<String>, Vec<String>) {
    let (mut removed, mut not_removed) = (Vec::new(), Vec::new());
    for old in outdated(dest, own, now, days) {
        if stop() {
            break;
        }
        if spare.is_some_and(|new| same_folder(new, &old.dir)) {
            continue;
        }
        match backup::remove(&old.dir) {
            Ok(()) => {
                log::info!("library: removed the backup {} (older than {days} days)", old.dir.display());
                removed.push(name_of(&old));
            }
            Err(e) => {
                log::warn!("library: {e}");
                not_removed.push(format!("{}: {e}", name_of(&old)));
            }
        }
    }
    (removed, not_removed)
}

/// An id for a library that has none: 128 bits that no other library has (the standard
/// library's per-process random keys, and the time).
fn new_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    let part = |salt: u64| {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(salt);
        #[cfg(not(target_arch = "wasm32"))]
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default());
        h.finish()
    };
    format!("{:016x}{:016x}", part(1), part(2))
}

/// A library restored into another folder is another library from then on: it does not keep
/// the id of the one the backup was made of (whose backups are not its own to remove, should
/// both ever back up to one folder from the same path), and is given its own at its first backup.
fn forget_id(library: &Path) {
    let path = library.join(crate::library::SettingsFile::Prefs.name());
    let Some(mut prefs) = std::fs::read(&path).ok().and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok()) else { return };
    if prefs.as_object_mut().and_then(|p| p.remove("libraryId")).is_none() {
        return;
    }
    // (temp file, fsync, rename, folder fsync: a crash leaves the old or the new file whole)
    let written = serde_json::to_vec_pretty(&prefs)
        .map_err(|e| e.to_string())
        .and_then(|bytes| lightcraft_catalog::safe_file::write_atomic(&path, &bytes).map_err(|e| e.to_string()));
    if let Err(e) = written {
        log::warn!("library: {}: {e}", path.display());
    }
}

/// The open library's own id; it is given one, kept in its preferences, when it has none.
fn library_id(s: &mut Session) -> String {
    if let Some(id) = &s.library_id {
        return id.clone();
    }
    let id = new_id();
    s.library_id = Some(id.clone());
    // (not saved: the library's backups say this id only for this session, and then none of
    // them is its own to a later one. Nothing is removed that should stay.)
    if let Err(e) = s.save_prefs() {
        log::warn!("library: {e}");
    }
    id
}

/// A folder given as a parameter: an absolute path (a relative one would mean another folder to
/// the app, the command line and an agent).
fn folder(cmd: &str, key: &str, value: &Value) -> Result<PathBuf> {
    match value.as_str().map(str::trim).filter(|d| !d.is_empty()).map(PathBuf::from) {
        Some(dir) if dir.is_absolute() => Ok(dir),
        Some(dir) => Err(bad(cmd, format!("`{key}` must be an absolute path, not {}", dir.display()))),
        None => Err(bad(cmd, format!("`{key}` is a folder's path"))),
    }
}

fn flag(cmd: &str, p: &Value, key: &str) -> Result<bool> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(bad(cmd, format!("`{key}` is true or false"))),
    }
}

/// The longest a library can be told to keep its backups: a hundred years, in days.
const KEEP_DAYS_MAX: u64 = 36_500;

/// `keepDays`: how many days backups are kept. `None`: not given; `Some(None)`: forever (null).
fn keep_days(cmd: &str, p: &Value) -> Result<Option<Option<u32>>> {
    match p.get("keepDays") {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(v) => match v.as_u64().filter(|days| (1..=KEEP_DAYS_MAX).contains(days)).and_then(|days| u32::try_from(days).ok()) {
            Some(days) => Ok(Some(Some(days))),
            None => {
                Err(bad(cmd, format!("`keepDays` is a whole number of days from 1 to {KEEP_DAYS_MAX}, or null to keep backups forever, not {v}")))
            }
        },
    }
}

/// Say which of the library's backups are older than it keeps them, and remove them when told
/// to (`remove`). `keepDays` given here is for this once.
fn prune(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "library.pruneBackups";
    let remove = flag(C, p, "remove")?;
    let Some(days) = keep_days(C, p)?.unwrap_or(s.backup_keep_days) else {
        return Err(bad(C, "this library keeps its backups forever: say how many days with `keepDays` (or set it with `library.backupPreferences`)"));
    };
    let Some(library) = s.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.clone()) else {
        return Err(bad(C, "this library is not a folder on disk; it has no backups in a folder"));
    };
    let Some((dest, _)) = backup_folder(s) else { return Err(bad(C, "this library has no folder for backups")) };
    // (set before the rule for where backups go, or in the preferences file by hand)
    if let Some(why) = not_for_backups(s, &dest) {
        return Err(bad(C, format!("the folder set for this library's backups can't be used: {why} (`library.backupPreferences`)")));
    }
    let now = (s.clock)();
    // (a library that was never backed up has no id, and no backups of its own)
    let old = match s.library_id.as_deref() {
        Some(id) => outdated(&dest, Own { library: &library, id }, &now, days),
        None => Vec::new(),
    };
    let listed: Vec<Value> =
        old.iter().map(|b| json!({"name": name_of(b), "dir": b.dir.to_string_lossy(), "made": b.made, "bytes": b.bytes})).collect();
    let (mut removed, mut not_removed) = (Vec::new(), Vec::new());
    if remove && !old.is_empty() {
        let task = s.activity.start("backup", "Removing old backups", Cancel::No);
        task.progress(0, old.len() as u64);
        // (the ones that were just listed, one by one: each must still be a backup that holds
        // nothing but its own files when it goes)
        for (done, b) in old.iter().enumerate() {
            match backup::remove(&b.dir) {
                Ok(()) => {
                    log::info!("library: removed the backup {} (older than {days} days)", b.dir.display());
                    removed.push(name_of(b));
                }
                Err(e) => {
                    log::warn!("library: {e}");
                    not_removed.push(format!("{}: {e}", name_of(b)));
                }
            }
            task.progress(done as u64 + 1, old.len() as u64);
        }
    }
    Ok(json!({
        "dir": dest.to_string_lossy(),
        "keepDays": days,
        "before": kept_since(&now, days),
        "keptAtLeast": backup::KEEP_AT_LEAST,
        "outdated": listed,
        "removed": removed,
        "notRemoved": not_removed,
    }))
}

/// One of a few choices given by its id (`None`: not given).
fn choice<T>(cmd: &str, p: &Value, key: &str, from_id: fn(&str) -> Option<T>, ids: &[&str]) -> Result<Option<T>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => match v.as_str().and_then(from_id) {
            Some(choice) => Ok(Some(choice)),
            None => Err(bad(cmd, format!("`{key}` is one of {}, not {v}", ids.join(", ")))),
        },
    }
}

/// The backup of the open library that is under way, or the last one made of it.
pub fn status(s: &Session) -> Option<&Arc<BackupJob>> {
    let library = s.library.as_ref().map(|l| &l.dir)?;
    s.backup.as_ref().filter(|b| b.library == *library)
}

fn back_up(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "library.backup";
    let Some(library) = s.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.clone()) else {
        return Err(bad(C, "this library is not a folder on disk; there is nothing to back up to a folder"));
    };
    if status(s).is_some_and(|b| !b.finished.load(Ordering::Relaxed)) {
        return Err(bad(C, "a backup of this library is already running"));
    }
    let background = flag(C, p, "background")?;
    let dest = match p.get("dir") {
        None | Some(Value::Null) => match s.backup_dir.clone() {
            // (set before this rule, or in the preferences file by hand)
            Some(chosen) => match not_for_backups(s, &chosen) {
                Some(why) => {
                    return Err(bad(C, format!("the folder set for this library's backups can't be used: {why} (`library.backupPreferences`)")));
                }
                None => chosen,
            },
            None => library.join(BACKUPS),
        },
        Some(dir) => destination(s, C, "dir", dir)?,
    };
    // (the library's id is in its preferences before they are backed up)
    let library_id = Some(library_id(s));
    // everything changed so far is in the log before it is read
    s.persist()?;
    let started = (s.clock)();
    let name = free_name(&dest, &started);
    let own = backup_folder(s).is_some_and(|(folder, _)| folder == dest);
    // (old backups are removed from the library's own folder only, by a backup made there)
    let keep_days = s.backup_keep_days.filter(|_| own);
    let job = Arc::new(BackupJob { library, dest, started, own, keep_days, library_id, ..Default::default() });
    s.backup = Some(job.clone());
    let task = s.activity.start("backup", "Backing up the library", Cancel::Flag(job.cancel.clone()));
    task.set_unit(Unit::Bytes);
    let hold = s.backup_hold.clone();
    if !background || cfg!(target_arch = "wasm32") {
        run(name, &job, &task, &hold);
        note_finished(s);
        return Ok(job.json());
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        // the row stays while the worker runs (it is dropped with the closure, however that ends)
        let worker = job.clone();
        if let Err(e) = std::thread::Builder::new().name("lc-backup".into()).spawn(move || run(name, &worker, &task, &hold)) {
            *job.outcome.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Err(format!("could not start: {e}")));
            job.finished.store(true, Ordering::Relaxed);
            return Err(bad(C, format!("could not start: {e}")));
        }
    }
    Ok(job.json())
}

/// Whether two paths are the same folder.
fn same_folder(a: &Path, b: &Path) -> bool {
    a == b || matches!((std::fs::canonicalize(a), std::fs::canonicalize(b)), (Ok(a), Ok(b)) if a == b)
}

/// Restore the backup in `from` into the folder `to`, which is not the open library: a folder
/// that isn't there or is empty, or a library nothing has open. Nothing is opened.
fn restore_into(s: &Session, from: &Path, to: &Path, row: &Row<'_>) -> Result<Value> {
    const C: &str = "library.restore";
    use lightcraft_catalog::lock::{LOCK, OWNER};
    let other = |e: backup::BackupError| crate::EngineError::Other(e.to_string());
    // not into the backup, and not into the open library's own folders
    if restore::within(to, from) {
        return Err(bad(C, format!("`to` ({}) is in the backup; restore into a folder of its own", to.display())));
    }
    if let Some(open) = s.library.as_ref().filter(|l| l.on_disk)
        && restore::within(to, &open.dir)
    {
        return Err(bad(C, format!("`to` ({}) is in the open library's folder; restore into a folder of its own", to.display())));
    }
    if backup::is_backup(to) {
        return Err(bad(C, format!("`to` ({}) is a backup itself; restore into a library folder", to.display())));
    }
    // a folder with things in it that is no library is not rearranged
    let holds: Vec<String> = std::fs::read_dir(to).map(|d| d.filter_map(|e| e.ok()?.file_name().into_string().ok()).collect()).unwrap_or_default();
    let library = holds.iter().any(|n| n == "catalog.snap" || n == "catalog.log");
    if !holds.is_empty() && !library {
        return Err(bad(C, format!("`to` ({}) holds other files and is not a library; restore into an empty folder", to.display())));
    }
    // the backup is sound before anything is made for it
    backup::check(from).map_err(other)?;
    let existed = to.exists();
    let had_lock = [LOCK, OWNER].map(|n| to.join(n).exists());
    std::fs::create_dir_all(to).map_err(|e| crate::EngineError::Other(format!("can't make the folder {}: {e}", to.display())))?;
    let aside = free_name(&to.join(restore::REPLACED), &(s.clock)());
    let made = {
        // the folder is nobody's while it is restored into
        let _lock = lightcraft_catalog::LibraryLock::acquire(to, &crate::library::program_name())
            .map_err(|e| crate::EngineError::LibraryInUse(e.to_string()));
        match _lock {
            Ok(_held) => {
                let made = restore::restore(from, to, &RestoreOptions { also: ALSO, aside: &aside }, row).map_err(other);
                // (with the folder's lock still held: nobody else has prefs.json to save meanwhile)
                if made.is_ok() {
                    forget_id(to);
                }
                made
            }
            Err(e) => Err(e),
        }
    };
    if made.is_err() {
        // nothing of a restore that was refused stays: the lock files and the folder it made
        for (name, had) in [LOCK, OWNER].iter().zip(had_lock) {
            if !had {
                let _ = std::fs::remove_file(to.join(name));
            }
        }
        if !existed {
            let _ = std::fs::remove_dir(to);
        }
    }
    let made = made?;
    log::info!("library: restored {} into {}", from.display(), to.display());
    Ok(json!({"restored": made, "opened": false}))
}

/// Put a backup back: as the open library (which the session then reopens), or into the folder
/// `to`.
fn restore_library(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "library.restore";
    let from = folder(C, "dir", p.get("dir").unwrap_or(&Value::Null))?;
    let open = s.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.clone());
    let to = match p.get("to") {
        None | Some(Value::Null) => None,
        Some(to) => Some(folder(C, "to", to)?),
    };
    let (target, in_place) = match (to, open) {
        (Some(to), Some(open)) if same_folder(&to, &open) => (open, true),
        (Some(to), _) => (to, false),
        (None, Some(open)) => (open, true),
        (None, None) => return Err(bad(C, "no library folder is open: say where to restore the backup with `to`")),
    };
    if !backup::is_backup(&from) {
        return Err(bad(C, format!("{} is not a backup (it has no {})", from.display(), backup::MARKER)));
    }
    // (`restore.partial` of the folder is the worker's until it is over, whichever restore asks)
    if copying(s, &target) {
        return Err(bad(C, "a restore of this library is still being stopped; restore again in a moment"));
    }
    if in_place {
        if restore_status(s).is_some_and(|r| r.running()) {
            return Err(bad(C, "a restore of this library is already under way"));
        }
        if s.library.as_ref().is_some_and(|l| l.stuck.is_some()) {
            return Err(bad(C, "a restore of this library stopped part-way; open the library again to finish it"));
        }
        if status(s).is_some_and(|b| !b.finished.load(Ordering::Relaxed)) {
            return Err(bad(C, "a backup of this library is running; restore once it is made"));
        }
        // nothing else is at work in the library while its files are replaced under it
        if let Some(task) = s.activity.list().first() {
            return Err(bad(C, format!("background work is under way ({}); restore once it is done, or stop it first", task.label)));
        }
    }
    let background = flag(C, p, "background")?;
    let aside = free_name(&target.join(restore::REPLACED), &(s.clock)());
    #[cfg(test)]
    let hold = s.restore_hold.clone();
    // (tests hold a restore here, to look at its row)
    let held = move || {
        #[cfg(test)]
        {
            let t0 = std::time::Instant::now();
            while hold.load(Ordering::SeqCst) && t0.elapsed() < std::time::Duration::from_secs(20) {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
    };
    // (a browser library has no folder: nothing runs in the background there)
    let _ = background;
    #[cfg(not(target_arch = "wasm32"))]
    if in_place && background {
        // the backup is copied and checked on a worker while the library stays in use; its files
        // are moved into place by `settle`, between commands
        let job = Arc::new(RestoreJob { library: target, from, aside, alive: AtomicBool::new(true), ..Default::default() });
        s.restore = Some(job.clone());
        let task = s.activity.start("restore", "Restoring the library", Cancel::Flag(job.cancel.clone()));
        task.set_unit(Unit::Bytes);
        let worker = job.clone();
        let spawned = std::thread::Builder::new().name("lc-restore".into()).spawn(move || {
            held();
            copy(&worker, task);
        });
        if let Err(e) = spawned {
            job.alive.store(false, Ordering::SeqCst);
            // (said once, by the command: there is no restore to report on)
            job.end(Err(format!("could not start: {e}")));
            s.restore = None;
            return Err(bad(C, format!("could not start: {e}")));
        }
        return Ok(job.json());
    }
    let task = s.activity.start("restore", "Restoring the library", Cancel::No);
    task.set_unit(Unit::Bytes);
    held();
    let never = AtomicBool::new(false);
    let row = Row { task: &task, cancel: &never };
    if !in_place {
        return restore_into(s, &from, &target, &row);
    }
    // the backup is copied into the library folder and checked while the library is as it is
    let staged =
        restore::stage(&from, &target, &RestoreOptions { also: ALSO, aside: &aside }, &row).map_err(|e| crate::EngineError::Other(e.to_string()))?;
    task.detail("catalog.snap");
    place(s, staged, &from, &target, &aside)
}

/// A restore of the open library whose backup is copied in the background, or the last one made
/// that way.
#[derive(Default)]
pub struct RestoreJob {
    /// The library it is of, and the backup it is from.
    pub library: PathBuf,
    pub from: PathBuf,
    /// The folder in `Replaced` for what it replaces.
    aside: String,
    /// Shared with the restore's row in the activity stack (its ✕ sets it).
    pub cancel: Arc<AtomicBool>,
    /// The backup's files, copied into the library folder and checked: waiting to be moved into
    /// place ([`settle`]), with the row that stays until then.
    staged: std::sync::Mutex<Option<(restore::Staged, TaskGuard)>>,
    /// How it ended: what `library.restore` returns, or why not.
    outcome: std::sync::Mutex<Option<std::result::Result<Value, String>>>,
    /// Its files are being moved into place (the library is closed and opened again for that,
    /// which is no reason to give it up).
    placing: AtomicBool,
    /// Its worker is at work (it started, and has not ended): what it copies into the library
    /// folder is its own until then, also when the restore was given up meanwhile.
    alive: AtomicBool,
}

/// Why a restore that was stopped, or given up, made no change.
const STOPPED: &str = "the restore was stopped; the library is as it was";
const GIVEN_UP: &str = "the library was closed or opened again before the backup was moved into place; it is as it was";

/// Reports the copy to the restore's row, and stops it when the row's ✕ was pressed, when the
/// restore was given up, or when nobody holds the job any more (the session is gone).
struct Copying<'a> {
    task: &'a TaskGuard,
    job: &'a Arc<RestoreJob>,
}

impl Watch for Copying<'_> {
    fn progress(&self, name: &str, done: u64, total: u64) {
        self.task.progress(done, total);
        self.task.detail(name);
    }
    fn cancelled(&self) -> bool {
        self.job.cancel.load(Ordering::Relaxed) || !self.job.running() || Arc::strong_count(self.job) <= 1
    }
}

impl RestoreJob {
    /// How it ended (`None` while it runs).
    pub fn outcome(&self) -> Option<std::result::Result<Value, String>> {
        self.outcome.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    pub fn running(&self) -> bool {
        self.outcome().is_none()
    }

    /// It is over (the first end counts); nothing of a copy that was not moved in stays.
    fn end(&self, outcome: std::result::Result<Value, String>) {
        let mut ended = self.outcome.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if ended.is_none() {
            *ended = Some(outcome);
        }
        drop(ended);
        // (dropped: the copy is removed, and the row goes)
        self.staged.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
    }

    pub fn json(&self) -> Value {
        let outcome = self.outcome();
        let made = outcome.as_ref().and_then(|o| o.as_ref().ok());
        json!({
            // (also while its worker still clears away its copy: another restore is refused till then)
            "running": outcome.is_none() || self.alive.load(Ordering::SeqCst),
            "cancelled": self.cancel.load(Ordering::Relaxed) && outcome.as_ref().is_some_and(|o| o.is_err()),
            "library": self.library.to_string_lossy(),
            "backup": self.from.to_string_lossy(),
            "restored": made.map(|v| v["restored"].clone()),
            "error": outcome.as_ref().and_then(|o| o.as_ref().err()),
        })
    }
}

/// The worker's part of a restore in the background: copy the backup into the library folder
/// and check it. The library is not touched.
fn copy(job: &Arc<RestoreJob>, task: TaskGuard) {
    // however this ends, the restore is not left "running" for good
    struct Over<'a>(&'a RestoreJob, bool);
    impl Drop for Over<'_> {
        fn drop(&mut self) {
            if !self.1 {
                self.0.end(Err("the restore stopped on an internal error".into()));
            }
            // (last: from here on the library folder's `restore.partial` is nobody's)
            self.0.alive.store(false, Ordering::SeqCst);
        }
    }
    let mut over = Over(job, false);
    let staged = restore::stage(&job.from, &job.library, &RestoreOptions { also: ALSO, aside: &job.aside }, &Copying { task: &task, job });
    match staged {
        Ok(staged) => {
            // all of it is copied; what is left waits for the library to be at rest
            let bytes = staged.report().bytes;
            task.progress(bytes, bytes);
            task.detail("copied");
            *job.staged.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some((staged, task));
            // (ended meanwhile, by another library being opened: nothing of the copy stays)
            if !job.running() {
                job.staged.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
            }
        }
        Err(backup::BackupError::Cancelled) => job.end(Err(STOPPED.into())),
        Err(e) => {
            log::warn!("library: restore: {e}");
            job.end(Err(e.to_string()));
        }
    }
    over.1 = true;
}

/// The restore of the open library that is under way in the background, or the last one made.
pub fn restore_status(s: &Session) -> Option<&Arc<RestoreJob>> {
    let library = s.library.as_ref().map(|l| &l.dir)?;
    s.restore.as_ref().filter(|r| r.library == *library)
}

/// Move a restore whose backup is copied into place, when the library is at rest: no slider
/// held, no backup or other background work under way. Called between commands (the app's frame
/// loop, `library.restoreStatus`); cheap when there is nothing to do. A restore of a library that
/// is no longer the open one is given up.
pub fn settle(s: &mut Session) {
    note_finished(s);
    let Some(job) = s.restore.clone() else { return };
    if !job.running() {
        return;
    }
    if !s.library.as_ref().is_some_and(|l| l.dir == job.library) {
        job.end(Err(GIVEN_UP.into()));
        return;
    }
    // (while the backup is still being copied, the worker sees a stop itself)
    let ready = job.staged.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref().map(|(_, task)| task.id());
    let Some(row) = ready else { return };
    if job.cancel.load(Ordering::Relaxed) {
        job.end(Err(STOPPED.into()));
        return;
    }
    let busy = s.interaction.is_some()
        || s.library.as_ref().is_some_and(|l| l.stuck.is_some())
        || status(s).is_some_and(|b| !b.finished.load(Ordering::Relaxed))
        || s.activity.list().iter().any(|t| t.id != row);
    if busy {
        return;
    }
    let Some((staged, task)) = job.staged.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() else { return };
    // from here on it is not stopped
    task.set_cancellable(false);
    job.placing.store(true, Ordering::SeqCst);
    // (the frame loop calls this outside any command: a panic here is the restore's error too)
    let made = crate::guard::catch("the restore", || place(s, staged, &job.from, &job.library, &job.aside));
    job.placing.store(false, Ordering::SeqCst);
    drop(task);
    job.end(match made {
        Ok(made) => made.map_err(|e| e.to_string()),
        Err(panic) => Err(panic),
    });
}

/// Give up a restore in the background whose backup is not moved into place yet: the library is
/// being opened (another one, or this one again), so it is no longer the library the restore
/// was asked of as it was open then. The copy stops, and nothing of it stays.
pub(crate) fn give_up(s: &Session) {
    if let Some(job) = &s.restore
        && job.running()
        && !job.placing.load(Ordering::SeqCst)
    {
        job.end(Err(GIVEN_UP.into()));
    }
}

/// Whether the worker of a restore of the library `dir` is still at work in its folder: also
/// after the restore was given up or stopped, until the worker sees that and removes its copy.
/// While it is, the folder's `restore.partial` is its (the copy is removed when it ends), so no
/// other restore is started there and nothing clears it away.
pub(crate) fn copying(s: &Session, dir: &Path) -> bool {
    s.restore.as_ref().is_some_and(|job| job.alive.load(Ordering::SeqCst) && same_folder(&job.library, dir))
}

/// Whether a restore of the library `dir` that is not over is in `s` (copying, or copied and
/// waiting to be moved into place): its copy is the restore's own to remove.
pub(crate) fn restoring(s: &Session, dir: &Path) -> bool {
    s.restore.as_ref().is_some_and(|job| (job.running() || job.alive.load(Ordering::SeqCst)) && same_folder(&job.library, dir))
}

/// `library.restore` can run: no restore or backup of the open library is under way, and none
/// has left it half moved.
fn can_restore(s: &Session) -> std::result::Result<(), String> {
    if restore_status(s).is_some_and(|r| r.running()) {
        return Err("a restore of this library is under way".into());
    }
    if s.library.as_ref().is_some_and(|l| copying(s, &l.dir)) {
        return Err("a restore of this library is still being stopped".into());
    }
    if status(s).is_some_and(|b| !b.finished.load(Ordering::Relaxed)) {
        return Err("a backup of this library is running".into());
    }
    match s.library.as_ref().and_then(|l| l.stuck.as_ref()) {
        Some(_) => Err("a restore of this library stopped part-way; open the library again".into()),
        None => Ok(()),
    }
}

/// Move a copied backup into place as the open library, and go on in it.
fn place(s: &mut Session, staged: restore::Staged, from: &Path, target: &Path, aside: &str) -> Result<Value> {
    // what of the session is the library's (it is put back if the restore is taken back), and
    // what stays: the folder backups go to is the library's choice now, not the backup's from then
    let (backups, active) = ((s.backup_dir.clone(), s.backup_schedule, s.last_backup.clone(), s.backup_keep_days), s.active());
    let kept = (s.presets.clone(), s.profile_favorites.clone(), s.profile_recent.clone(), s.media.smart_dir.clone());
    // everything this session changed is on disk, and nothing of it writes any more
    s.close_library()?;
    let stuck = |s: &mut Session, why: String| {
        let why = format!(
            "the restore stopped while it was moving the library's files ({why}). Nothing more is saved here: open the library again to finish the restore"
        );
        if let Some(lib) = s.library.as_mut() {
            lib.stuck = Some(why.clone());
        }
        crate::EngineError::Other(why)
    };
    let made = match staged.replace() {
        Ok(made) => made,
        Err(e) => match restore::finish(target) {
            Ok(Some(made)) => made,
            Ok(None) => {
                let e = e.to_string();
                let said = e.contains("the library is as it was");
                return Err(crate::EngineError::Other(if said { e } else { format!("{e}; the library is as it was") }));
            }
            Err(again) => return Err(stuck(s, format!("{e}; {again}"))),
        },
    };
    // the library's presets are the restored file's, not this session's and that file's together
    s.presets.retain(|p| p.builtin);
    for p in &mut s.presets {
        p.favorite = false;
    }
    s.profile_favorites.clear();
    s.profile_recent.clear();
    if let Err(e) = s.open_library(target, false) {
        // the restored catalog is not one this version opens: back to the library as it was
        // (the session still has it, and its files are the same ones once they are back)
        let e = e.to_string();
        let not_opened = format!("{aside} (not opened)");
        return Err(match restore::take_back(&made, &not_opened) {
            Ok(_) => {
                (s.presets, s.profile_favorites, s.profile_recent, s.media.smart_dir) = kept;
                crate::EngineError::Other(format!(
                    "the backup's catalog can't be opened ({e}); the library is as it was, and the backup's files are in {}",
                    target.join(restore::REPLACED).join(not_opened).display()
                ))
            }
            Err(back) => stuck(s, format!("the backup's catalog can't be opened: {e}; and the library could not be put back: {back}")),
        });
    }
    log::info!("library: restored from {}; what it held is in {}", from.display(), made.aside.as_deref().unwrap_or(Path::new("")).display());
    // (and so is when it is backed up by itself, and when it last was)
    if (s.backup_dir.clone(), s.backup_schedule, s.last_backup.clone(), s.backup_keep_days) != backups {
        (s.backup_dir, s.backup_schedule, s.last_backup, s.backup_keep_days) = backups;
        if let Err(e) = s.save_prefs() {
            log::warn!("library: {e}");
        }
    }
    // still on the photo it was on, when the restored library has it
    if let Some(id) = active.filter(|id| s.catalog.photo(*id).is_some())
        && s.selection.active != Some(id)
    {
        s.selection = crate::Selection { ids: vec![id], active: Some(id) };
    }
    Ok(json!({"restored": made, "opened": true}))
}

/// The finished backups in the folder the library's backups go to (or `dir`), newest first.
fn backups(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "library.backups";
    let dir = match p.get("dir") {
        None | Some(Value::Null) => match s.library.as_ref().filter(|l| l.on_disk) {
            Some(lib) => s.backup_dir.clone().unwrap_or_else(|| lib.dir.join(BACKUPS)),
            None => return Err(bad(C, "no library folder is open: say which folder with `dir`")),
        },
        Some(dir) => folder(C, "dir", dir)?,
    };
    // (a backup is of the open library when it says so by the library's id: a library made anew
    // in the same folder has another, and one that was moved has the one it had)
    let open = s.library.as_ref().filter(|l| l.on_disk).and(s.library_id.as_deref());
    let mut listed = backup::list(&dir);
    // newest first by when each says it was made (the ones that don't say come after, as listed)
    listed.sort_by(|a, b| b.made.cmp(&a.made));
    let listed: Vec<Value> = listed
        .iter()
        .filter_map(|b| {
            let mut one = serde_json::to_value(b).ok()?;
            one["name"] = json!(name_of(b));
            one["ofThisLibrary"] = json!(open.is_some() && b.library_id.as_deref() == open);
            Some(one)
        })
        .collect();
    Ok(json!({"dir": dir.to_string_lossy(), "isBackup": backup::is_made_backup(&dir), "backups": listed}))
}

/// Take note of a backup of the open library that has ended: one that was made, in the folder
/// the library's backups go to now, is when the library was last backed up (saved with it). One
/// that was made in the folder they went to before it was changed meanwhile is not, for the
/// library has no backup in the folder they go to now. Once per backup; between commands (see
/// [`settle`]).
pub fn note_finished(s: &mut Session) {
    let Some(job) = status(s).cloned() else { return };
    if !job.finished.load(Ordering::Relaxed) || job.noted.swap(true, Ordering::Relaxed) {
        return;
    }
    if job.own && backup_folder(s).is_some_and(|(folder, _)| folder == job.dest) && job.outcome().is_some_and(|o| o.is_ok()) {
        s.last_backup = Some(job.started.clone());
        if let Err(e) = s.save_prefs() {
            log::warn!("library: the time of the backup was not saved: {e}");
        }
    }
}

/// Back the open library up by itself if its schedule says it is due
/// ([`crate::backup_schedule::due`]): in the background, to the folder its backups go to, like
/// `library.backup`. Whether one was started; a backup that is due and can't be started is the
/// error, to tell the person (it is not tried again for an hour either). For the app to call
/// now and then while a library is open.
pub fn back_up_if_due(s: &mut Session) -> std::result::Result<bool, String> {
    use crate::backup_schedule::due;
    if s.backup_schedule.every == crate::backup_schedule::BackupEvery::Never || can_back_up(s).is_err() {
        return Ok(false);
    }
    // (a restore under way puts another library here: its backup is the restored library's)
    if s.restore.as_ref().is_some_and(|r| r.running()) {
        return Ok(false);
    }
    let now = (s.clock)();
    if !due(s.backup_schedule.every, None, s.backup_tried.as_deref(), &now) {
        // (tried within the hour)
        return Ok(false);
    }
    if !due(s.backup_schedule.every, s.last_backup.as_deref(), None, &now) {
        // Not due by the time of the last backup. Backups that are no longer there are no
        // backup, though: their folder was removed, or its drive wiped. (Looked at only here,
        // where everything else says "not due": a slow drive is not asked on every look.)
        if backup_folder(s).is_some_and(|(folder, _)| folder.is_dir()) {
            return Ok(false);
        }
        s.last_backup = None;
        if let Err(e) = s.save_prefs() {
            log::warn!("library: {e}");
        }
    }
    s.backup_tried = Some(now);
    match back_up(s, &json!({"background": true})) {
        Ok(_) => Ok(true),
        Err(e) => {
            log::warn!("library: the backup that was due was not made: {e}");
            Err(e.to_string())
        }
    }
}

/// Where the open library's backups go, and whether that folder was chosen for it (else it is
/// `Backups` in the library folder). `None`: the library is not a folder on disk, and is not
/// backed up to one. Reads only: what Settings shows on every frame.
pub fn backup_folder(s: &Session) -> Option<(PathBuf, bool)> {
    let library = s.library.as_ref().filter(|l| l.on_disk)?;
    Some(match &s.backup_dir {
        Some(chosen) => (chosen.clone(), true),
        None => (library.dir.join(BACKUPS), false),
    })
}

/// Why `dir` is no folder for the open library's backups to go to, if it is none: one of the
/// library's own (inside the library folder only `Backups` is for backups: its other folders
/// are the catalog's, and a restore moves them as such), or a backup or a folder inside one (it
/// would no longer be what its list of files says).
fn not_for_backups(s: &Session, dir: &Path) -> Option<String> {
    // (`..` names another folder than the path reads as, and which one can change as the folders
    // before it are made: a place for backups is given by its own path)
    if dir.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return Some(format!("{} must be the folder's own path, without `..`", dir.display()));
    }
    // (As the file system names them, as far as they exist: a link is not a way round, and a
    // folder that is yet to be made is where its nearest folder that exists puts it.)
    let real = |p: &Path| {
        p.ancestors()
            .find_map(|known| {
                let rest = p.strip_prefix(known).ok()?;
                Some(std::fs::canonicalize(known).ok()?.join(rest))
            })
            .unwrap_or_else(|| p.to_path_buf())
    };
    let at = real(dir);
    if let Some(library) = s.library.as_ref().filter(|l| l.on_disk).map(|l| real(&l.dir))
        && at.starts_with(&library)
        && at != library.join(BACKUPS)
    {
        return Some(format!(
            "{} is inside the library folder, where only {BACKUPS} itself is for backups (the rest is the catalog's own); give a folder outside it, or none for {BACKUPS} in it",
            dir.display()
        ));
    }
    let backup = at.ancestors().find(|a| backup::is_made_backup(a))?;
    Some(format!("{} is a backup, or inside one ({}); a backup is not added to", dir.display(), backup.display()))
}

/// A folder given for the open library's backups to go to ([`folder`], [`not_for_backups`]).
fn destination(s: &Session, cmd: &str, key: &str, value: &Value) -> Result<PathBuf> {
    let dir = folder(cmd, key, value)?;
    match not_for_backups(s, &dir) {
        Some(why) => Err(bad(cmd, format!("`{key}`: {why}"))),
        None => Ok(dir),
    }
}

fn preferences(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "library.backupPreferences";
    let choose = flag(C, p, "choose")?;
    // (both read before either is set: a call that is refused changes nothing)
    let every = choice(C, p, "every", crate::backup_schedule::BackupEvery::from_id, &crate::backup_schedule::BackupEvery::ALL.map(|e| e.id()))?;
    let on_close =
        choice(C, p, "onClose", crate::backup_schedule::BackupOnClose::from_id, &crate::backup_schedule::BackupOnClose::ALL.map(|c| c.id()))?;
    let keep = keep_days(C, p)?;
    let before = (s.backup_dir.clone(), s.backup_schedule, s.last_backup.clone(), s.backup_tried.clone(), s.backup_keep_days);
    if let Some(dir) = p.get("dir") {
        s.backup_dir = match dir {
            Value::Null => None,
            dir => {
                let dir = destination(s, C, "dir", dir)?;
                // (`Backups` in the library folder is the usual place, not a choice: it goes
                // with the library when the library is moved)
                let usual = s.library.as_ref().is_some_and(|l| same_folder(&l.dir.join(BACKUPS), &dir));
                (!usual).then_some(dir)
            }
        };
        if s.backup_dir != before.0 {
            // (when the library was last backed up is when it was backed up *there*)
            s.last_backup = None;
            s.backup_tried = None;
        }
    } else if choose {
        // (the app asks with a folder dialog before the command gets here)
        return Err(bad(C, "there is no folder dialog here to `choose` with: give the folder as `dir`"));
    }
    if let Some(every) = every {
        s.backup_schedule.every = every;
    }
    if let Some(on_close) = on_close {
        s.backup_schedule.on_close = on_close;
    }
    if let Some(keep) = keep {
        s.backup_keep_days = keep;
    }
    if p.get("dir").is_some() || every.is_some() || on_close.is_some() || keep.is_some() {
        // (a choice that was not saved is not made: Settings and the next backup would use what the
        // next session does not know)
        if let Err(e) = s.save_prefs() {
            (s.backup_dir, s.backup_schedule, s.last_backup, s.backup_tried, s.backup_keep_days) = before;
            return Err(e);
        }
    }
    // `dir`: what was chosen (null: nothing was). `folder`: where backups go, chosen or not.
    let folder = backup_folder(s).map(|(folder, _)| folder);
    Ok(json!({
        "dir": s.backup_dir.as_ref().map(|d| d.to_string_lossy()),
        "folder": folder.as_ref().map(|d| d.to_string_lossy()),
        "every": s.backup_schedule.every.id(),
        "onClose": s.backup_schedule.on_close.id(),
        "lastBackup": s.last_backup,
        "keepDays": s.backup_keep_days,
    }))
}

fn on_disk(s: &Session) -> std::result::Result<(), String> {
    if s.library.as_ref().is_some_and(|l| l.on_disk) { Ok(()) } else { Err("the library is not a folder on disk".into()) }
}

/// [`on_disk`], and no backup or restore of the library under way (a restore reads a backup,
/// and then puts another library in this one's place: nothing is made or removed meanwhile).
fn can_back_up(s: &Session) -> std::result::Result<(), String> {
    on_disk(s)?;
    if restore_status(s).is_some_and(|r| r.running()) {
        return Err("a restore of this library is under way".into());
    }
    if s.library.as_ref().is_some_and(|l| l.stuck.is_some()) {
        return Err("a restore of this library stopped part-way; open the library again".into());
    }
    if status(s).is_some_and(|b| !b.finished.load(Ordering::Relaxed)) { Err("a backup of this library is running".into()) } else { Ok(()) }
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query
            "library.backup",
            "Back Up Library",
            ["File"],
            None,
            "{dir?: absolute folder path (not inside the library folder other than Backups, nor inside a backup), background?: bool} — back up the library's catalog (not the photos) to a folder named after the time (UTC) in `dir`, else in the library's backup folder (`library.backupPreferences`), else in Backups in the library folder. The backup holds a whole library (`library.restore` puts it back; it is not opened where it is); files that didn't change since the backup before are shared with it (hard links, each read and checked first), so it copies what changed. When the library keeps its backups for a time (`keepDays` in `library.backupPreferences`) and this one goes to its own folder, its older backups are removed afterwards (`removed`, `notRemoved`; see `library.pruneBackups`). Has a row in the activity stack (kind `backup`) that can stop it. Returns when the backup is made, or with `background` at once (then see `library.backupStatus`) → {running, cancelled, library, dest, report: {dir, seq, photos, segments, files, copied, linked, bytes, copiedBytes, made, library, libraryId, contents: {file: {bytes, crc}}} | null, error, removed: [older backups removed after it], notRemoved}",
            can_back_up,
            back_up
        ),
        cmd!(query
            "library.backupStatus",
            "Backup Status",
            [],
            None,
            "{} → the backup of the open library that is under way, or the last one made of it in this session, as `library.backup` returns it | null",
            always,
            |s, _| {
                // (a backup that ended is noted when anybody looks, not only when a frame loop calls `settle`)
                note_finished(s);
                Ok(status(s).map_or(Value::Null, |b| b.json()))
            }
        ),
        cmd!(query
            "library.checkBackup",
            "Check Backup",
            [],
            None,
            "{dir: a backup's folder (absolute path)} — read the backup against what its marker (backup.json) lists: every file there with the size and CRC-32 it had when the backup was made, and no other → {ok: true, report} | {ok: false, error} naming the first file that was edited, damaged, removed or added. A backup that was opened as a library is no longer as it was made",
            always,
            |_, p| {
                let dir = folder("library.checkBackup", "dir", p.get("dir").unwrap_or(&Value::Null))?;
                Ok(match backup::check(&dir) {
                    Ok(report) => json!({"ok": true, "report": report}),
                    Err(e) => json!({"ok": false, "error": e.to_string()}),
                })
            }
        ),
        cmd!(query
            "library.backups",
            "Backups",
            [],
            None,
            "{dir?: absolute folder path} — the finished backups in `dir`, else in the folder the open library's backups go to, newest first by when each was made → {dir, isBackup: `dir` is a backup itself (and holds none), backups: [report as `library.backup` returns it, with name: its folder's, ofThisLibrary: it was made of the open library (by the library's id: also where the library was before it was moved, not of one that was in its folder before)]}",
            always,
            backups
        ),
        cmd!(query
            "library.restore",
            "Restore Library from Backup…",
            ["File"],
            None,
            "{dir: a backup's folder (absolute path), to?: absolute folder path, background?: bool} — put a backup back (in the app, without `dir`: the library's backups are listed to choose from, `library.backups`; `choose: true` asks for the folder with a folder dialog, and lists the backups in it when it is a folder of backups, not one; `to` needs `dir`). Without `to` (or with `to` naming the open library): the open library becomes what the backup holds (catalog, log, presets, view and preferences; not the photos, and the folder its backups go to stays), and the session goes on in it: undo history is gone, as after opening a library. What the library held is kept, also changes not yet saved, in Replaced/<the time (UTC)> in its folder, which is a library folder itself. Refused while a backup or other background work is under way. With another `to`: the backup is restored into that folder (one that isn't there or is empty, or a library nothing has open, which is replaced the same way; not a folder in the backup or in the open library) and nothing is opened. The backup is checked against its marker first and is only read; one that was edited or damaged, or whose catalog or log a newer LightCraft wrote, is refused with the library untouched. A restored catalog that then doesn't open is taken back (the library is as it was; the backup's files are in Replaced/<the time> (not opened)); with `to` that is not found out, since nothing is opened. Has a row in the activity stack (kind `restore`). The command returns when it is done; with `background` (a restore of the open library only) it returns at once with what `library.restoreStatus` reports: the backup is copied and checked while the library stays in use (the row's ✕ stops it then), and its files are moved into place once the library is at rest (no slider held, no other background work), which the app sees to between frames and `library.restoreStatus` when asked; so `background` is for a session that goes on (the app, MCP, the control channel), not for a program that ends when the command returns. Opening a library (another, or this one again) before that gives the restore up, with nothing changed. A backup can't be opened as a library where it is → {restored: {backup, library, aside | null, seq, photos, files, bytes, skipped, placed, replaced}, opened}",
            can_restore,
            restore_library
        ),
        cmd!(query
            "library.restoreStatus",
            "Restore Status",
            [],
            None,
            "{} → the restore of the open library that is under way in the background (`library.restore` with `background`), or the last one made that way in this session | null: {running, cancelled, library, backup, restored: as `library.restore` returns it | null, error}. Also moves a backup that is copied into place when the library is at rest (as the app does between frames), so a script that asks until `running` is false sees the restore through",
            always,
            |s, _| {
                settle(s);
                Ok(restore_status(s).map_or(Value::Null, |r| r.json()))
            }
        ),
        cmd!(
            "library.pruneBackups",
            "Remove Old Backups",
            [],
            None,
            "{keepDays?: whole days, remove?: bool} — the library's own backups (in the folder its backups go to) that are older than it keeps them, or than `keepDays` given for this once: never one of its newest three, never another library's (a backup says which library it is of, by folder and id), never one that was renamed, never one whose time is missing or yet to come. Says which; removes them, each as a whole, only with `remove: true`. Not while the library is being restored → {dir, keepDays, before, keptAtLeast, outdated: [{name, dir, made, bytes}], removed: [name], notRemoved: [name: why]}",
            can_back_up,
            prune
        ),
        cmd!(
            "library.backupPreferences",
            "Backup Preferences",
            [],
            None,
            "{dir?: absolute folder path | null, choose?: bool, every?: never | day | week | month, onClose?: never | ask | always, keepDays?: whole days | null} — the folder the library's backups go to (saved with the library); null: Backups in the library folder; not a folder inside the library folder (other than Backups) or inside a backup; `choose: true` (in the app only, without `dir`): ask for it with a folder dialog, and the answer is null while the dialog is open `every`: back the library up by itself while it is open, when the last backup is that old (the app does; a month is thirty days); `onClose`: back it up when it is closed, asking first or not `keepDays`: how long the library keeps its backups (null: forever, the usual); older ones of its own are removed after a backup is made there, never its newest three → {dir: what was chosen | null, folder: where backups go, every, onClose, lastBackup: when it was last backed up there | null, keepDays}",
            on_disk,
            preferences
        ),
    ]
}
