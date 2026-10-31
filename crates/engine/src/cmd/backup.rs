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
//! on in the restored library. A backup is not opened where it is.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use lightcraft_catalog::backup::{self, BackupOptions, BackupReport, Watch};
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
    let options = BackupOptions { name: &name, also: ALSO };
    let made = backup::back_up(&job.library, &job.dest, &options, &Row { task, cancel: &job.cancel });
    match &made {
        Ok(r) => log::info!("library: backed up to {} ({} files, {} shared with the backup before)", r.dir.display(), r.files, r.linked),
        Err(e) => log::warn!("library: backup: {e}"),
    }
    *job.outcome.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(made.map_err(|e| e.to_string()));
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
        None | Some(Value::Null) => s.backup_dir.clone().unwrap_or_else(|| library.join(BACKUPS)),
        Some(dir) => folder(C, "dir", dir)?,
    };
    // everything changed so far is in the log before it is read
    s.persist()?;
    let name = free_name(&dest, &(s.clock)());
    let job = Arc::new(BackupJob { library, dest, ..Default::default() });
    s.backup = Some(job.clone());
    let task = s.activity.start("backup", "Backing up the library", Cancel::Flag(job.cancel.clone()));
    task.set_unit(Unit::Bytes);
    let hold = s.backup_hold.clone();
    if !background || cfg!(target_arch = "wasm32") {
        run(name, &job, &task, &hold);
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
            Ok(_) => restore::restore(from, to, &RestoreOptions { also: ALSO, aside: &aside }, row).map_err(other),
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
    if in_place {
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
    let task = s.activity.start("restore", "Restoring the library", Cancel::No);
    task.set_unit(Unit::Bytes);
    #[cfg(test)]
    {
        // (tests hold a restore here, to look at its row)
        let t0 = std::time::Instant::now();
        while s.restore_hold.load(Ordering::SeqCst) && t0.elapsed() < std::time::Duration::from_secs(20) {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
    let never = AtomicBool::new(false);
    let row = Row { task: &task, cancel: &never };
    if !in_place {
        return restore_into(s, &from, &target, &row);
    }
    let other = |e: backup::BackupError| crate::EngineError::Other(e.to_string());
    let aside = free_name(&target.join(restore::REPLACED), &(s.clock)());
    // the backup is copied into the library folder and checked while the library is as it is
    let staged = restore::stage(&from, &target, &RestoreOptions { also: ALSO, aside: &aside }, &row).map_err(other)?;
    task.detail("catalog.snap");
    // what of the session is the library's (it is put back if the restore is taken back), and
    // what stays: the folder backups go to is the library's choice now, not the backup's from then
    let (backups, active) = (s.backup_dir.clone(), s.active());
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
        Err(e) => match restore::finish(&target) {
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
    if let Err(e) = s.open_library(&target, false) {
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
    if s.backup_dir != backups {
        s.backup_dir = backups;
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
    Ok(json!({"dir": dir.to_string_lossy(), "backups": backup::list(&dir)}))
}

fn preferences(s: &mut Session, p: &Value) -> Result<Value> {
    if let Some(dir) = p.get("dir") {
        s.backup_dir = match dir {
            Value::Null => None,
            dir => Some(folder("library.backupPreferences", "dir", dir)?),
        };
        s.save_prefs()?;
    }
    Ok(json!({"dir": s.backup_dir.as_ref().map(|d| d.to_string_lossy())}))
}

fn on_disk(s: &Session) -> std::result::Result<(), String> {
    if s.library.as_ref().is_some_and(|l| l.on_disk) { Ok(()) } else { Err("the library is not a folder on disk".into()) }
}

/// [`on_disk`], and no backup of the library under way.
fn can_back_up(s: &Session) -> std::result::Result<(), String> {
    on_disk(s)?;
    if status(s).is_some_and(|b| !b.finished.load(Ordering::Relaxed)) { Err("a backup of this library is running".into()) } else { Ok(()) }
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query
            "library.backup",
            "Back Up Library",
            ["File"],
            None,
            "{dir?: absolute folder path, background?: bool} — back up the library's catalog (not the photos) to a folder named after the time (UTC) in `dir`, else in the library's backup folder (`library.backupPreferences`), else in Backups in the library folder. The backup holds a whole library (`library.restore` puts it back; it is not opened where it is); files that didn't change since the backup before are shared with it (hard links, each read and checked first), so it copies what changed. Has a row in the activity stack (kind `backup`) that can stop it. Returns when the backup is made, or with `background` at once (then see `library.backupStatus`) → {running, cancelled, library, dest, report: {dir, seq, photos, segments, files, copied, linked, bytes, copiedBytes, contents: {file: {bytes, crc}}} | null, error}",
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
            |s, _| Ok(status(s).map_or(Value::Null, |b| b.json()))
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
            "{dir?: absolute folder path} — the finished backups in `dir`, else in the folder the open library's backups go to, newest first → {dir, backups: [report as `library.backup` returns it]}",
            always,
            backups
        ),
        cmd!(query
            "library.restore",
            "Restore Library from Backup",
            [],
            None,
            "{dir: a backup's folder (absolute path), to?: absolute folder path} — put a backup back. Without `to` (or with `to` naming the open library): the open library becomes what the backup holds (catalog, log, presets, view and preferences; not the photos, and the folder its backups go to stays), and the session goes on in it: undo history is gone, as after opening a library. What the library held is kept, also changes not yet saved, in Replaced/<the time (UTC)> in its folder, which is a library folder itself. Refused while a backup or other background work is under way. With another `to`: the backup is restored into that folder (one that isn't there or is empty, or a library nothing has open, which is replaced the same way; not a folder in the backup or in the open library) and nothing is opened. The backup is checked against its marker first and is only read; one that was edited or damaged, or whose catalog or log a newer LightCraft wrote, is refused with the library untouched. A restored catalog that then doesn't open is taken back (the library is as it was; the backup's files are in Replaced/<the time> (not opened)); with `to` that is not found out, since nothing is opened. Has a row in the activity stack (kind `restore`); the command returns when it is done. A backup can't be opened as a library where it is → {restored: {backup, library, aside | null, seq, photos, files, bytes, skipped, placed, replaced}, opened}",
            always,
            restore_library
        ),
        cmd!(
            "library.backupPreferences",
            "Backup Preferences",
            [],
            None,
            "{dir?: absolute folder path | null} — the folder the library's backups go to (saved with the library); null: Backups in the library folder → {dir}",
            on_disk,
            preferences
        ),
    ]
}
