//! `library.backup`: back up the library's catalog to a dated folder (see
//! [`lightcraft_catalog::backup`]): a folder that opens as the library it was taken of, and that
//! shares with the backup before it the files that didn't change.
//!
//! The backup reads the library's files as they are on disk, with a row in the activity stack.
//! It runs to its end before the command returns, or (`background`: what the app asks for) on a
//! worker thread while the library stays in use. Not the photos: the catalog, its log, and the
//! presets, view and preferences files.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use lightcraft_catalog::backup::{self, BackupOptions, BackupReport, Watch};
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
fn folder(cmd: &str, value: &Value) -> Result<PathBuf> {
    match value.as_str().map(str::trim).filter(|d| !d.is_empty()).map(PathBuf::from) {
        Some(dir) if dir.is_absolute() => Ok(dir),
        Some(dir) => Err(bad(cmd, format!("`dir` must be an absolute path, not {}", dir.display()))),
        None => Err(bad(cmd, "`dir` is a folder's path")),
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
        Some(dir) => folder(C, dir)?,
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

fn preferences(s: &mut Session, p: &Value) -> Result<Value> {
    if let Some(dir) = p.get("dir") {
        s.backup_dir = match dir {
            Value::Null => None,
            dir => Some(folder("library.backupPreferences", dir)?),
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
            "{dir?: absolute folder path, background?: bool} — back up the library's catalog (not the photos) to a folder named after the time (UTC) in `dir`, else in the library's backup folder (`library.backupPreferences`), else in Backups in the library folder. The backup opens as a library; files that didn't change since the backup before are shared with it (hard links, each read and checked first), so it copies what changed. Has a row in the activity stack (kind `backup`) that can stop it. Returns when the backup is made, or with `background` at once (then see `library.backupStatus`) → {running, cancelled, library, dest, report: {dir, seq, photos, segments, files, copied, linked, bytes, copiedBytes, contents: {file: {bytes, crc}}} | null, error}",
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
                let dir = folder("library.checkBackup", p.get("dir").unwrap_or(&Value::Null))?;
                Ok(match backup::check(&dir) {
                    Ok(report) => json!({"ok": true, "report": report}),
                    Err(e) => json!({"ok": false, "error": e.to_string()}),
                })
            }
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
