//! Programs a plug-in may start (`permissions.programs`), and the runs it starts.
//!
//! A plug-in names a program by identity ([`ProgramDecl`]: macOS bundle ids, a Windows or Linux
//! executable name), never by path; the app finds the installed program itself
//! ([`crate::HostApi::find_program`]). Starting one is `program.start`:
//!
//! 1. the program must be declared, granted (only the user can grant it: see
//!    [`crate::Manager::set_grant_by_user`]) and installed;
//! 2. one run per program per plug-in at a time ([`Reservation`]);
//! 3. a fresh private folder for the run ([`RunDir`]) gets the plug-in's `files`, and `{tmpDir}`
//!    in the arguments becomes its path;
//! 4. the app starts the program with that folder as its temp folder and watches it
//!    ([`crate::HostApi::start_program`]); when it exits, or the user stops waiting, the plug-in's
//!    `program.end` hook gets the folder lent for reading ([`crate::Manager::program_end_call`]).
//!    The folder goes once the hook has returned and the program has exited.
//!
//! Run folders live under one parent in the system temp folder, so a start can sweep the ones an
//! earlier session left behind ([`sweep_old_runs`]). Each has a lock file beside it, locked while
//! its run lasts, so a sweep never takes the folder of a run another app instance is watching.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Most programs one plug-in may declare.
pub const MAX_PROGRAMS: usize = 8;
/// Most bundle ids one program may list (its versions).
pub const MAX_BUNDLE_IDS: usize = 8;
/// Most arguments `program.start` passes, and the longest one.
pub const MAX_ARGS: usize = 256;
pub const MAX_ARG_BYTES: usize = 4096;
/// Most files `program.start` writes into the run's folder, and their total size.
pub const MAX_FILES: usize = 64;
pub const MAX_FILES_BYTES: usize = crate::context::MAX_IO_BYTES;
/// A run folder an earlier session left behind is removed once it is this old.
pub const STALE_RUN_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// A program a plug-in may ask to start: an entry of `permissions.programs`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgramDecl {
    /// `[A-Za-z0-9._-]{1,64}`, unique within the plug-in; what `program.start` names.
    pub id: String,
    /// Display name ("DxO PureRAW").
    pub name: String,
    /// macOS: the bundle ids of the product's versions, newest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mac_bundle_ids: Vec<String>,
    /// Windows: the executable's file name (`PureRAW.exe`), found through App Paths, then `PATH`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub windows_exe: Option<String>,
    /// Linux and other systems: a command name found on `PATH`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linux_exe: Option<String>,
}

fn valid_bundle_id(id: &str) -> bool {
    (1..=255).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

/// A plain file name on every system: no folder part, no `..`, none of the characters or device
/// names Windows refuses or reads specially (`CON`, `NUL.txt`, `COM1`, a trailing dot or space).
pub(crate) fn valid_file_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).trim_end_matches(' ');
    let device = ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"].iter().any(|d| stem.eq_ignore_ascii_case(d))
        || ["COM", "LPT"].iter().any(|d| {
            stem.len() == 4
                && stem.get(..3).is_some_and(|p| p.eq_ignore_ascii_case(d))
                && stem.as_bytes().get(3).is_some_and(|b| (b'1'..=b'9').contains(b))
        });
    (1..=255).contains(&name.len())
        && !name.contains("..")
        && !name.ends_with(['.', ' '])
        && !device
        && !name.chars().any(|c| matches!(c, '/' | '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*') || c.is_control())
}

impl ProgramDecl {
    /// Does this entry name only what `granted` names? Its bundle ids are among `granted`'s, and
    /// each executable is `granted`'s or none. The name and the order don't count, so an update
    /// that renames, reorders or narrows a granted entry keeps the grant; one that widens it
    /// (another bundle id, another executable) doesn't: the user granted particular programs.
    pub fn within(&self, granted: &ProgramDecl) -> bool {
        let exe = |mine: &Option<String>, theirs: &Option<String>| mine.is_none() || mine == theirs;
        self.id == granted.id
            && self.mac_bundle_ids.iter().all(|b| granted.mac_bundle_ids.contains(b))
            && exe(&self.windows_exe, &granted.windows_exe)
            && exe(&self.linux_exe, &granted.linux_exe)
    }

    pub fn validate(&self) -> std::result::Result<(), String> {
        if !crate::manifest::valid_id(&self.id) {
            return Err(format!("program id {:?} must be 1-64 characters of A-Z a-z 0-9 . _ -", self.id));
        }
        if self.name.trim().is_empty() || self.name.len() > 128 {
            return Err(format!("program {:?}: name must be 1-128 bytes", self.id));
        }
        if self.mac_bundle_ids.is_empty() && self.windows_exe.is_none() && self.linux_exe.is_none() {
            return Err(format!("program {:?} must name at least one of macBundleIds, windowsExe or linuxExe", self.id));
        }
        if self.mac_bundle_ids.len() > MAX_BUNDLE_IDS {
            return Err(format!("program {:?}: at most {MAX_BUNDLE_IDS} bundle ids", self.id));
        }
        if let Some(b) = self.mac_bundle_ids.iter().find(|b| !valid_bundle_id(b)) {
            return Err(format!("program {:?}: bad bundle id {b:?}", self.id));
        }
        if self.mac_bundle_ids.iter().collect::<BTreeSet<_>>().len() != self.mac_bundle_ids.len() {
            return Err(format!("program {:?}: a bundle id is repeated", self.id));
        }
        for exe in [&self.windows_exe, &self.linux_exe].into_iter().flatten() {
            if !valid_file_name(exe) {
                return Err(format!("program {:?}: {exe:?} must be a file name, not a path", self.id));
            }
        }
        Ok(())
    }
}

/// Checks a manifest's list of programs.
pub(crate) fn validate_all(programs: &[ProgramDecl]) -> std::result::Result<(), String> {
    if programs.len() > MAX_PROGRAMS {
        return Err(format!("at most {MAX_PROGRAMS} programs"));
    }
    let mut seen = BTreeSet::new();
    for p in programs {
        p.validate()?;
        if !seen.insert(p.id.as_str()) {
            return Err(format!("program id {:?} is repeated", p.id));
        }
    }
    Ok(())
}

/// One installed copy of a declared program, as the app found it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Found {
    /// macOS: which of the entry's bundle ids this is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle_id: Option<String>,
    /// Where it is (the `.app` on macOS, the executable elsewhere).
    pub path: String,
    /// Its version, when the app could read it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// A run the app is asked to start ([`crate::HostApi::start_program`]).
#[derive(Debug)]
pub struct StartRequest {
    /// The plug-in.
    pub plugin: String,
    /// The plug-in's display name (its manifest's `name`), for the user.
    pub plugin_name: String,
    pub program: ProgramDecl,
    /// The installed copy to start.
    pub found: Found,
    /// The arguments, `{tmpDir}` already replaced.
    pub args: Vec<String>,
    pub dir: RunDir,
    /// Held for the whole run: dropping it lets the plug-in start the program again.
    pub reservation: Reservation,
}

impl StartRequest {
    /// The run's id (what `program.start` returns and `program.end` gets).
    pub fn run(&self) -> &str {
        self.reservation.run()
    }
}

/// How a program exited.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Exit {
    pub success: bool,
    /// Its exit code, when the app knows the program's own (not on macOS, where `open` starts it,
    /// nor for a program ended by a signal).
    pub code: Option<i32>,
}

/// A started program the app watches ([`wait_for_end`]).
pub trait Running: Send {
    /// `Some` once it has exited.
    fn try_wait(&mut self) -> std::result::Result<Option<Exit>, String>;
}

/// How a run ended, as `program.end` reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndStatus {
    /// The program exited.
    Exited,
    /// The user stopped waiting (the program may still be running).
    Cancelled,
    /// The app lost track of it (waiting failed).
    Failed,
}

impl EndStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            EndStatus::Exited => "exited",
            EndStatus::Cancelled => "cancelled",
            EndStatus::Failed => "failed",
        }
    }
}

/// How often a watched program is looked at.
const POLL: Duration = Duration::from_millis(100);

/// Waits until `child` exits or `cancel` is set (checked between looks). → how the run ended,
/// and the program's exit code when it exited and the app knows it.
pub fn wait_for_end(child: &mut dyn Running, cancel: &AtomicBool) -> (EndStatus, Option<i32>) {
    loop {
        match child.try_wait() {
            Ok(Some(exit)) => return (EndStatus::Exited, exit.code),
            Ok(None) => {}
            Err(e) => {
                log::warn!("lost track of a plug-in's program: {e}");
                return (EndStatus::Failed, None);
            }
        }
        if cancel.load(Ordering::Relaxed) {
            return (EndStatus::Cancelled, None);
        }
        std::thread::sleep(POLL);
    }
}

/// Waits until `child` exits (or can't be watched any more). For a cancelled run: its folder
/// stays until then.
pub fn wait_for_exit(child: &mut dyn Running) {
    while let Ok(None) = child.try_wait() {
        std::thread::sleep(POLL * 5);
    }
}

/// Waits up to `limit` for `child` to fail at once. → `Err` when it exited unsuccessfully in that
/// time (a program that can't start: the launcher's error), `Ok` otherwise (still running, or
/// done successfully: the run goes on and ends normally).
pub fn check_quick_failure(child: &mut dyn Running, limit: Duration) -> std::result::Result<(), String> {
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(Exit { success: false, code })) => {
                return Err(format!("it exited at once with an error{}", code.map(|c| format!(" (code {c})")).unwrap_or_default()));
            }
            Ok(Some(_)) => return Ok(()),
            Ok(None) if start.elapsed() >= limit => return Ok(()),
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(e),
        }
    }
}

/// The runs in progress: `(plug-in, program)` → run id. One each.
static RUNS: Mutex<Vec<(String, String, String)>> = Mutex::new(Vec::new());
static NEXT_RUN: AtomicU64 = AtomicU64::new(1);

fn runs() -> std::sync::MutexGuard<'static, Vec<(String, String, String)>> {
    RUNS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A plug-in's claim on one of its programs for one run; released when dropped.
#[derive(Debug)]
pub struct Reservation {
    plugin: String,
    program: String,
    run: String,
}

impl Reservation {
    /// Claims `program` for `plugin`, or says why it can't (a run of it is active).
    pub fn take(plugin: &str, program: &str) -> std::result::Result<Reservation, String> {
        let mut g = runs();
        if g.iter().any(|(p, q, _)| p == plugin && q == program) {
            return Err(format!("{program} is already running for this plug-in; wait for it to end"));
        }
        let run = format!("run-{}-{}", pid(), NEXT_RUN.fetch_add(1, Ordering::Relaxed));
        g.push((plugin.to_string(), program.to_string(), run.clone()));
        Ok(Reservation { plugin: plugin.to_string(), program: program.to_string(), run })
    }

    pub fn run(&self) -> &str {
        &self.run
    }

    pub fn plugin(&self) -> &str {
        &self.plugin
    }

    pub fn program(&self) -> &str {
        &self.program
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        runs().retain(|(_, _, r)| *r != self.run);
    }
}

/// The runs in progress, `(plug-in, program, run)` (`ui.inspect`, tests).
pub fn active_runs() -> Vec<(String, String, String)> {
    runs().clone()
}

fn pid() -> u32 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::process::id()
    }
    #[cfg(target_arch = "wasm32")]
    {
        0
    }
}

/// The folder run folders go in: the user's own temp folder on macOS and Windows; on Linux and
/// the BSDs, where the temp folder is usually shared, the user's runtime folder
/// (`XDG_RUNTIME_DIR`) when there is one. Either way [`RunDir::create`] checks it's the user's.
pub fn runs_root() -> PathBuf {
    #[cfg(all(unix, not(target_os = "macos")))]
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).filter(|d| d.is_absolute()) {
        return dir.join("lcc-plugin-runs");
    }
    std::env::temp_dir().join("lcc-plugin-runs")
}

/// Makes `root` (when missing) and checks only this user can use it: a real folder, not a link,
/// that the user can make private (only its owner can change its mode). Another user's folder
/// at that path, or a link, is refused, so no one else can reach into a run. Returns the folder's
/// canonical path, checked again, so swapping `root` for a link afterwards changes nothing.
fn private_root(root: &Path) -> std::result::Result<PathBuf, String> {
    let refuse = |why: &str| format!("the run folder parent {} {why}", root.display());
    if let Some(parent) = root.parent() {
        std::fs::create_dir_all(parent).map_err(|e| refuse(&format!("can't be made: {e}")))?;
    }
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut b = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
    match b.create(root) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(refuse(&format!("can't be made: {e}"))),
    }
    let real = std::fs::canonicalize(root).map_err(|e| refuse(&format!("can't be read: {e}")))?;
    for path in [root, real.as_path()] {
        let meta = std::fs::symlink_metadata(path).map_err(|e| refuse(&format!("can't be read: {e}")))?;
        if !meta.is_dir() {
            return Err(refuse("is not a folder (or is a link)"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).map_err(|e| refuse(&format!("belongs to someone else ({e})")))?;
        }
    }
    Ok(real)
}

/// A run's private folder: the program's temp folder, the plug-in's handoff files. Deleted when
/// dropped, so whoever owns it keeps it until the hook has returned and the program has exited. If
/// the app quits first, nothing drops it and a later start sweeps it ([`sweep_old_runs`]).
///
/// `<folder>.lock` beside it is locked for as long as the `RunDir` lives (released by the system
/// if the app quits), which tells a sweep the run is still watched.
#[derive(Debug)]
pub struct RunDir {
    path: PathBuf,
    lock: PathBuf,
    /// Holds the lock; closed after the folder and the lock file are gone.
    _held: std::fs::File,
}

fn lock_path(dir: &Path) -> PathBuf {
    let mut name = dir.file_name().unwrap_or_default().to_os_string();
    name.push(".lock");
    dir.with_file_name(name)
}

impl RunDir {
    /// A new, empty folder under `root` (created when missing) that only this user can open.
    pub fn create(root: &Path) -> std::result::Result<RunDir, String> {
        static N: AtomicU64 = AtomicU64::new(0);
        let root = &private_root(root)?;
        for _ in 0..1000 {
            let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
            let path = root.join(format!("{}-{}-{nanos:08x}", pid(), N.fetch_add(1, Ordering::Relaxed)));
            // the lock first, so a sweep never sees the folder unlocked
            let lock = lock_path(&path);
            let held = match std::fs::OpenOptions::new().write(true).create_new(true).open(&lock) {
                Ok(f) => f,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(format!("could not make the run's lock file {}: {e}", lock.display())),
            };
            if let Err(e) = held.try_lock() {
                // a file system without locks: the run still works, a sweep goes by age alone
                log::debug!("could not lock {}: {e}", lock.display());
            }
            #[cfg_attr(not(unix), allow(unused_mut))]
            let mut b = std::fs::DirBuilder::new();
            #[cfg(unix)]
            std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
            // `create`, not `create_all`: the folder must be new and ours
            match b.create(&path) {
                Ok(()) => return Ok(RunDir { path, lock, _held: held }),
                Err(e) => {
                    let _ = std::fs::remove_file(&lock);
                    if e.kind() == std::io::ErrorKind::AlreadyExists {
                        continue;
                    }
                    return Err(format!("could not make the run folder {}: {e}", path.display()));
                }
            }
        }
        Err(format!("no free run folder name in {}", root.display()))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Writes the plug-in's `files` (`{name: text}`) into the folder: plain names only, at most
    /// [`MAX_FILES`] files and [`MAX_FILES_BYTES`] in all. Checked before anything is written.
    pub fn write_files(&self, files: &Map<String, Value>) -> std::result::Result<(), String> {
        let files = checked_files(files)?;
        for (name, text) in files {
            let path = self.path.join(name);
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|e| format!("could not write {}: {e}", path.display()))?;
            std::io::Write::write_all(&mut f, text.as_bytes()).map_err(|e| format!("could not write {}: {e}", path.display()))?;
        }
        Ok(())
    }

    /// Deletes the folder and everything in it (as dropping it does).
    pub fn remove(self) {}
}

impl Drop for RunDir {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            log::warn!("could not remove the plug-in run folder {}: {e}", self.path.display());
            // the lock file stays with it: a later sweep removes both
            return;
        }
        let _ = std::fs::remove_file(&self.lock);
    }
}

/// `files` as `(name, text)`, or why they can't be written.
pub(crate) fn checked_files(files: &Map<String, Value>) -> std::result::Result<Vec<(&str, &str)>, String> {
    if files.len() > MAX_FILES {
        return Err(format!("at most {MAX_FILES} files"));
    }
    let mut total = 0usize;
    let mut out = Vec::with_capacity(files.len());
    for (name, v) in files {
        if !valid_file_name(name) {
            return Err(format!("{name:?} must be a plain file name (no folder, no `..`)"));
        }
        let text = v.as_str().ok_or_else(|| format!("file {name:?}: the contents must be text"))?;
        total = total.saturating_add(text.len());
        if total > MAX_FILES_BYTES {
            return Err(format!("the files are larger than {MAX_FILES_BYTES} bytes in all"));
        }
        out.push((name.as_str(), text));
    }
    Ok(out)
}

/// The arguments with `{tmpDir}` replaced by `dir`, or why they can't be passed.
pub(crate) fn expand_args(args: &Value, dir: &Path) -> std::result::Result<Vec<String>, String> {
    let list = match args {
        Value::Null => return Ok(Vec::new()),
        Value::Array(a) => a,
        _ => return Err("`args` must be a list of strings".into()),
    };
    if list.len() > MAX_ARGS {
        return Err(format!("at most {MAX_ARGS} arguments"));
    }
    let dir = dir.to_string_lossy();
    list.iter()
        .map(|a| {
            let a = a.as_str().ok_or("`args` must be a list of strings")?;
            if a.len() > MAX_ARG_BYTES || a.contains('\0') {
                return Err(format!("an argument is longer than {MAX_ARG_BYTES} bytes or holds a NUL"));
            }
            Ok(a.replace("{tmpDir}", &dir))
        })
        .collect()
}

/// Removes run folders under `root` that an earlier session left behind (its program still ran
/// when the app quit) once they are older than `age`, and lock files left without a folder. A
/// folder whose lock is held (a run some app instance still watches) is kept.
pub fn sweep_old_runs(root: &Path, age: Duration) -> usize {
    // only in a folder that is this user's (see `private_root`)
    if !root.exists() {
        return 0;
    }
    let Ok(root) = private_root(root) else { return 0 };
    let Ok(rd) = std::fs::read_dir(&root) else { return 0 };
    // `Some(file)`: the lock is ours until the file is dropped; `None`: no lock file
    let unlocked = |lock: &Path| -> Option<Option<std::fs::File>> {
        match std::fs::OpenOptions::new().write(true).open(lock) {
            Ok(f) => f.try_lock().is_ok().then_some(Some(f)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(None),
            Err(_) => None,
        }
    };
    let mut n = 0;
    for e in rd.filter_map(|e| e.ok()).take(10_000) {
        let path = e.path();
        let Ok(kind) = e.file_type() else { continue };
        // a new lock may not have its folder yet (`RunDir::create`), nor a new folder its lock
        let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|d| d >= age);
        if !old {
            continue;
        }
        if kind.is_dir() {
            let lock = lock_path(&path);
            let Some(held) = unlocked(&lock) else { continue };
            if std::fs::remove_dir_all(&path).is_ok() {
                n += 1;
                if held.is_some() {
                    let _ = std::fs::remove_file(&lock);
                }
            }
        } else if kind.is_file() && path.extension().is_some_and(|x| x == "lock") && !path.with_extension("").exists() {
            // a lock left without its folder (removed by hand, or the app stopped in between)
            if let Some(Some(_held)) = unlocked(&path) {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn decl() -> ProgramDecl {
        ProgramDecl {
            id: "tool".into(),
            name: "Tool".into(),
            mac_bundle_ids: vec!["com.example.tool3".into(), "com.example.tool2".into()],
            windows_exe: Some("Tool.exe".into()),
            linux_exe: None,
        }
    }

    #[test]
    fn a_program_needs_one_system_and_plain_file_names() {
        // a program needs one system and plain names
        {
            assert!(decl().validate().is_ok());
            let none = ProgramDecl { mac_bundle_ids: vec![], windows_exe: None, linux_exe: None, ..decl() };
            assert!(none.validate().is_err(), "an entry with no system is invalid");
            let only_linux = ProgramDecl { mac_bundle_ids: vec![], windows_exe: None, linux_exe: Some("tool".into()), ..decl() };
            assert!(only_linux.validate().is_ok(), "macBundleIds is optional");
            for exe in [
                "/usr/bin/tool",
                "..\\tool.exe",
                "../tool",
                "C:tool.exe",
                "",
                ".",
                "tool.",
                "tool ",
                "CON",
                "nul.exe",
                "com1.txt",
                "LPT9",
                "conin$",
                "CONOUT$.txt",
                "a|b",
                "a*",
                "a?",
                "<a>",
                "\"a\"",
            ] {
                let p = ProgramDecl { linux_exe: Some(exe.into()), ..decl() };
                assert!(p.validate().is_err(), "{exe:?} is a path, not a name");
            }
            let repeated = ProgramDecl { mac_bundle_ids: vec!["a.b".into(), "a.b".into()], ..decl() };
            assert!(repeated.validate().is_err());
            assert!(validate_all(&[decl(), decl()]).is_err(), "ids are unique");
        }
        // ordinary names are file names
        {
            for ok in ["tool", "PureRAW.exe", "COM10", "console.txt", "LPT0", ".hidden", "a b.txt", "NULL"] {
                assert!(valid_file_name(ok), "{ok:?}");
            }
        }
    }

    #[test]
    fn one_run_per_program_per_plugin() {
        let a = Reservation::take("p.one", "tool").unwrap();
        assert!(Reservation::take("p.one", "tool").is_err(), "a second run of the same program is refused");
        let other = Reservation::take("p.two", "tool").unwrap();
        drop(a);
        let again = Reservation::take("p.one", "tool").expect("once the run ended, a new one starts");
        drop((other, again));
    }

    #[test]
    fn files_and_args_are_checked_before_anything_is_written() {
        let root = std::env::temp_dir().join(format!("lcc-programs-test-{}", pid()));
        let dir = RunDir::create(&root).unwrap();
        for bad in [json!({"../x": "a"}), json!({"a/b": "a"}), json!({"a\\b": "a"}), json!({"..": "a"}), json!({"ok": 1})] {
            assert!(dir.write_files(bad.as_object().unwrap()).is_err(), "{bad}");
        }
        let big = json!({"a": "x".repeat(MAX_FILES_BYTES / 2 + 1), "b": "y".repeat(MAX_FILES_BYTES / 2 + 1)});
        assert!(dir.write_files(big.as_object().unwrap()).is_err(), "past the size limit");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "nothing was written");
        dir.write_files(json!({"batch": "one\ntwo"}).as_object().unwrap()).unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("batch")).unwrap(), "one\ntwo");
        let args = expand_args(&json!(["--batch", "{tmpDir}/batch"]), dir.path()).unwrap();
        assert_eq!(args[1], format!("{}/batch", dir.path().display()));
        assert!(expand_args(&json!("--x"), dir.path()).is_err());
        let p = dir.path().to_path_buf();
        dir.remove();
        assert!(!p.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn old_run_folders_go_unless_a_run_still_holds_them() {
        let root = std::env::temp_dir().join(format!("lcc-programs-sweep-{}", pid()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("1-0-other")).unwrap();
        std::fs::write(root.join("1-0-other.lock"), "").unwrap();
        std::fs::write(root.join("2-0-gone.lock"), "").unwrap();
        let live = RunDir::create(&root).unwrap();
        assert_eq!(sweep_old_runs(&root, Duration::from_secs(3600)), 0, "young folders stay");
        assert!(root.join("2-0-gone.lock").exists(), "a young lock stays without its folder: a run being made");
        assert_eq!(sweep_old_runs(&root, Duration::ZERO), 1, "an old one no run holds goes");
        assert!(!root.join("1-0-other.lock").exists(), "with its lock file");
        assert!(!root.join("2-0-gone.lock").exists(), "a lock file without a folder goes");
        assert!(live.path().exists() && lock_path(live.path()).exists(), "a run's folder stays while it is locked, whatever its age");
        let p = live.path().to_path_buf();
        drop(live);
        assert!(!p.exists() && !lock_path(&p).exists(), "a run that ends removes its folder and its lock");
        let _ = std::fs::remove_dir_all(&root);
    }

    struct Exits(u32, bool);
    impl Running for Exits {
        fn try_wait(&mut self) -> std::result::Result<Option<Exit>, String> {
            if self.0 == 0 {
                return Ok(Some(Exit { success: self.1, code: Some(if self.1 { 0 } else { 3 }) }));
            }
            self.0 -= 1;
            Ok(None)
        }
    }

    #[test]
    fn waiting_ends_on_exit_or_cancel() {
        let flag = AtomicBool::new(false);
        assert_eq!(wait_for_end(&mut Exits(2, true), &flag), (EndStatus::Exited, Some(0)));
        assert_eq!(wait_for_end(&mut Exits(0, false), &flag), (EndStatus::Exited, Some(3)), "the exit code is passed on");
        flag.store(true, Ordering::Relaxed);
        assert_eq!(wait_for_end(&mut Exits(u32::MAX, true), &flag), (EndStatus::Cancelled, None));
        assert!(check_quick_failure(&mut Exits(0, false), Duration::from_millis(50)).unwrap_err().contains("code 3"), "failing at once is an error");
        assert!(check_quick_failure(&mut Exits(0, true), Duration::from_millis(50)).is_ok());
        assert!(check_quick_failure(&mut Exits(u32::MAX, true), Duration::from_millis(50)).is_ok(), "still running is fine");
    }

    #[cfg(unix)]
    #[test]
    fn a_run_root_that_is_a_link_or_a_file_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let base = std::env::temp_dir().join(format!("lcc-programs-root-{}", pid()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("elsewhere")).unwrap();
        // someone else's place behind a link: refused, nothing made there
        std::os::unix::fs::symlink(base.join("elsewhere"), base.join("link")).unwrap();
        assert!(RunDir::create(&base.join("link")).unwrap_err().contains("link"));
        assert_eq!(std::fs::read_dir(base.join("elsewhere")).unwrap().count(), 0);
        std::fs::write(base.join("file"), "x").unwrap();
        assert!(RunDir::create(&base.join("file")).is_err());
        // a folder of this user's that others could open is made private first
        let open = base.join("open");
        std::fs::create_dir(&open).unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o777)).unwrap();
        let dir = RunDir::create(&open).unwrap();
        assert_eq!(std::fs::metadata(&open).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(std::fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777, 0o700);
        drop(dir);
        let _ = std::fs::remove_dir_all(&base);
    }
}
