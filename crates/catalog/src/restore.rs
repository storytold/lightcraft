//! Restoring a library's catalog from a backup ([`crate::backup`]).
//!
//! **The library becomes what the backup holds**: `catalog.snap`, its segments, `catalog.log`
//! and the settings files asked for. The backup itself is only read: its files are copied, never
//! shared, since a library goes on changing.
//!
//! **Nothing is restored from a backup that isn't what it was when it was made**, or that this
//! version can't open. The backup is read against its marker first ([`crate::backup::check`]),
//! and every file again as it is copied; a snapshot or a log record of a newer LightCraft is
//! refused there, with the library untouched.
//!
//! **What the library was is kept.** The files a restore replaces are moved to
//! `Replaced/<name>` in the library folder, which is a library folder in its own right: a
//! restore can be taken back by opening it, or by moving its files back ([`take_back`]).
//!
//! **A restore is all or nothing, also when it is cut short.** The backup is first copied into
//! `restore.partial` in the library folder, with the library untouched. Only then are files
//! moved, and before the first of them a note of what is to be done is written (`restore.json`).
//! A restore that stops after that is completed by [`finish`], which whoever opens the library
//! calls first: the library is never opened as some files of one catalog and some of another.
//! The snapshot is the first file moved out and the last moved in, so a folder with a
//! `catalog.snap` and no note is always a whole library. Where the library's files can't all be
//! moved out, or the copy of the backup is no longer whole, the ones that were are moved back,
//! and the library is as it was.
//!
//! **The note is a file like any other.** It may have been edited, or be what is left of
//! something else: it moves files of the library folder itself, by plain name, between that
//! folder, `Replaced` and `restore.partial`, which must be folders of the library's own (not
//! links); never another folder, a lock file or a backup's marker. A note that asks for anything
//! else is not followed, and nothing is moved.

use std::path::{Path, PathBuf};

use crate::backup::{self, BackupError, Watch};
use crate::journal::{LOG, SNAPSHOT};
use crate::segments::{self, SEGMENT_DIR};

/// The note of a restore under way, in the library folder.
pub const PENDING: &str = "restore.json";
/// The folder in the library folder that holds what restores replaced.
pub const REPLACED: &str = "Replaced";
/// The backup's files, copied into the library folder before any is moved into place.
const STAGED: &str = "restore.partial";

/// How to restore.
#[derive(Clone, Copy, Debug)]
pub struct RestoreOptions<'a> {
    /// Other files of the backup to restore besides the catalog's (presets, preferences): plain
    /// file names. A file of the backup that isn't named here stays out, and the library's own
    /// file of that name stays where it is.
    pub also: &'a [&'a str],
    /// The name of the folder in `Replaced` that takes what the restore replaces (the time).
    pub aside: &'a str,
}

/// A restore that was made.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RestoreReport {
    pub backup: PathBuf,
    pub library: PathBuf,
    /// Where what the library was went; `None` when there was nothing to replace.
    pub aside: Option<PathBuf>,
    /// The op the restored snapshot is of, and the photos it holds (as the backup reports them).
    pub seq: u64,
    pub photos: u64,
    /// Files restored, and their size in all.
    pub files: u64,
    pub bytes: u64,
    /// Files of the backup that were not asked for, and so not restored.
    pub skipped: Vec<String>,
    /// What the restore put into the library folder, and what it moved out of it to `aside`, by
    /// name there (`segments`: the folder).
    pub placed: Vec<String>,
    pub replaced: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum Phase {
    /// What the library holds is being moved to `Replaced/<aside>`.
    Aside,
    /// The backup's files are being moved in.
    Place,
}

/// The note: what a restore that was started still has to do. Names only, all of them of the
/// library folder itself.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Pending {
    phase: Phase,
    aside: String,
    /// What is moved out of the library folder, the snapshot first.
    old: Vec<String>,
    /// What is moved in, the snapshot last: from `restore.partial`, or (a restore that is
    /// taken back) from the folder of this name in `Replaced`.
    new: Vec<String>,
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    report: RestoreReport,
}

fn refused(why: impl Into<String>) -> BackupError {
    BackupError::Restore(why.into())
}

#[cfg(test)]
thread_local! {
    static STOP_AT: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// Tests: make the `n`th change a restore makes to the library folder from now on fail, and
/// every one after it, as if the program had ended just before it (`None`: none).
#[cfg(test)]
pub(crate) fn stop_at(n: Option<usize>) {
    STOP_AT.with(|s| s.set(n));
}

/// Before every change to the library folder.
fn step() -> Result<(), BackupError> {
    #[cfg(test)]
    {
        let stop = STOP_AT.with(|s| match s.get() {
            Some(0) => true,
            Some(n) => {
                s.set(Some(n - 1));
                false
            }
            None => false,
        });
        if stop {
            return Err(BackupError::Io("stopped (test)".into()));
        }
    }
    Ok(())
}

fn there(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// A folder of its own (not a link to one elsewhere).
fn real_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
}

fn rename(from: &Path, to: &Path) -> Result<(), BackupError> {
    step()?;
    std::fs::rename(from, to).map_err(|e| refused(format!("can't move {} to {}: {e}", from.display(), to.display())))
}

/// Names of a library folder a restore never moves: its own, the lock's, a backup's marker
/// (whatever their case: some file systems don't tell).
fn reserved(name: &str) -> bool {
    let tmp = format!("{PENDING}.tmp");
    [PENDING, tmp.as_str(), STAGED, REPLACED, backup::MARKER, crate::lock::LOCK, crate::lock::OWNER].iter().any(|r| r.eq_ignore_ascii_case(name))
}

/// A name of the library folder a restore may move: a plain file name that isn't [`reserved`].
fn movable(name: &str) -> bool {
    backup::valid_name(name) && !reserved(name)
}

/// A name `options.also` may hold.
fn valid_extra(name: &str) -> bool {
    backup::valid_extra(name) && movable(name) && ![SNAPSHOT, LOG, SEGMENT_DIR].iter().any(|own| own.eq_ignore_ascii_case(name))
}

/// The top-level name of a file of a backup: `segments` for a segment, else the name.
fn top(name: &str) -> &str {
    if name.starts_with(segments::SEGMENT_PREFIX) { SEGMENT_DIR } else { name }
}

/// Whether `path` is the folder `folder` or in it, going by what of the two exists.
pub fn within(path: &Path, folder: &Path) -> bool {
    let Ok(folder) = std::fs::canonicalize(folder) else { return path.starts_with(folder) };
    // the nearest ancestor that exists, resolved, with the rest of the path after it
    let mut rest = Vec::new();
    let mut at = path;
    loop {
        if let Ok(real) = std::fs::canonicalize(at) {
            return rest.iter().rev().fold(real, |p: PathBuf, part| p.join(part)).starts_with(&folder);
        }
        match (at.parent(), at.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name);
                at = parent;
            }
            _ => return path.starts_with(&folder),
        }
    }
}

/// Write the note, whole or not at all.
fn note(library: &Path, pending: &Pending) -> Result<(), BackupError> {
    step()?;
    let bytes = serde_json::to_vec_pretty(pending).map_err(|e| BackupError::Io(e.to_string()))?;
    let (tmp, path) = (library.join(format!("{PENDING}.tmp")), library.join(PENDING));
    let written = (|| {
        use std::io::Write;
        // a new file of the library's own: never written through a link that is in its place
        if there(&tmp) {
            std::fs::remove_file(&tmp)?;
        }
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, &path)
    })();
    written.map_err(|e| BackupError::Io(format!("can't write {}: {e}", path.display())))?;
    backup::sync_dir(library);
    Ok(())
}

/// The folder a restore's files are moved in from.
fn source(library: &Path, pending: &Pending) -> PathBuf {
    match &pending.from {
        Some(from) => library.join(REPLACED).join(from),
        None => library.join(STAGED),
    }
}

/// Whether `pending` asks only for what a restore does: the error says what else.
fn valid(library: &Path, pending: &Pending) -> Result<(), String> {
    if let Some(bad) = [Some(&pending.aside), pending.from.as_ref()].into_iter().flatten().find(|n| !backup::valid_name(n)) {
        return Err(format!("{bad:?} is no folder name"));
    }
    // the folders files are moved between are the library's own
    let source = source(library, pending);
    for folder in [library.join(REPLACED), library.join(REPLACED).join(&pending.aside), source.clone()] {
        if there(&folder) && !real_dir(&folder) {
            return Err(format!("{} is not a folder", folder.display()));
        }
    }
    for name in pending.old.iter().chain(&pending.new) {
        if !movable(name) {
            return Err(format!("{name:?} is not a file of a library's catalog"));
        }
        // files, by whatever name; of folders, the segments folder alone
        for path in [library.join(name), source.join(name)] {
            if real_dir(&path) && name != SEGMENT_DIR {
                return Err(format!("{} is a folder, not a file of a library's catalog", path.display()));
            }
        }
    }
    Ok(())
}

/// Whether everything that is to be moved in is where it is moved in from.
fn whole(library: &Path, pending: &Pending) -> bool {
    let source = source(library, pending);
    pending.new.iter().all(|name| there(&source.join(name)))
}

/// The note in `library`, checked: `None` when there is none.
fn pending(library: &Path) -> Result<Option<Pending>, BackupError> {
    let path = library.join(PENDING);
    if !there(&path) {
        return Ok(None);
    }
    let unusable = |why: String| {
        refused(format!(
            "a restore of this library was cut short, and its note ({}) can't be followed: {why}. Nothing was moved now. What the library held is in \
             {REPLACED} or still in place, and the backup's files are in {STAGED}; with the note removed, the library opens as whatever is in place",
            path.display()
        ))
    };
    let bytes = std::fs::read(&path).map_err(|e| unusable(e.to_string()))?;
    let pending: Pending = serde_json::from_slice(&bytes).map_err(|e| unusable(e.to_string()))?;
    valid(library, &pending).map_err(unusable)?;
    Ok(Some(pending))
}

/// Move `from` into the folder `aside` under `name`, or under a name of its own when that is
/// taken (nothing set aside is ever written over).
fn set_aside(from: &Path, aside: &Path, name: &str) -> Result<(), BackupError> {
    std::fs::create_dir_all(aside).map_err(|e| backup::io("can't make the folder", aside, e))?;
    let to = std::iter::once(name.to_string())
        .chain((2..1000u32).map(|n| format!("{name} ({n})")))
        .map(|n| aside.join(n))
        .find(|p| !there(p))
        .ok_or_else(|| refused(format!("{} holds too many files named {name}", aside.display())))?;
    rename(from, &to)
}

/// Remove the note: the restore is over, one way or the other.
fn forget(library: &Path) -> Result<(), BackupError> {
    step()?;
    let path = library.join(PENDING);
    std::fs::remove_file(&path).map_err(|e| backup::io("can't remove", &path, e))?;
    backup::sync_dir(library);
    Ok(())
}

/// Undo a restore that has only moved files out so far: they go back (the snapshot last), and
/// the note is removed. The library is as it was.
fn unroll(library: &Path, pending: &Pending) -> Result<(), BackupError> {
    let aside = library.join(REPLACED).join(&pending.aside);
    for name in pending.old.iter().rev() {
        let (from, to) = (aside.join(name), library.join(name));
        if !there(&from) {
            continue;
        }
        // (not the library's: it turned up since)
        if there(&to) {
            set_aside(&to, &aside, name)?;
        }
        rename(&from, &to)?;
    }
    backup::sync_dir(library);
    // (only if nothing is in them)
    let _ = std::fs::remove_dir(&aside);
    let _ = std::fs::remove_dir(library.join(REPLACED));
    // the copy of the backup is of no use any more (a folder of `Replaced` that was to be taken
    // back stays as it is)
    let staged = library.join(STAGED);
    step()?;
    if pending.from.is_none()
        && real_dir(&staged)
        && let Err(e) = std::fs::remove_dir_all(&staged)
    {
        log::warn!("restore: can't remove {}: {e}", staged.display());
    }
    forget(library)
}

/// Do what the note says is left to do, and remove the note. `None`: the restore could not go
/// on and nothing had been moved in yet, so what was moved out is back, and the library is as
/// it was.
fn roll(library: &Path, mut pending: Pending) -> Result<Option<RestoreReport>, BackupError> {
    let aside = library.join(REPLACED).join(&pending.aside);
    let from = source(library, &pending);
    if pending.phase == Phase::Aside {
        // nothing is moved out for a copy of the backup that is no longer whole
        if !whole(library, &pending) {
            log::warn!("restore: {} no longer holds the backup's files; the library stays as it was", from.display());
            unroll(library, &pending)?;
            return Ok(None);
        }
        for name in &pending.old {
            let at = library.join(name);
            if there(&at)
                && let Err(e) = set_aside(&at, &aside, name)
            {
                // what can't all be moved out is moved back (when that fails too, the note stays,
                // and the restore is completed later)
                return match unroll(library, &pending) {
                    Ok(()) => {
                        let (BackupError::Restore(why) | BackupError::Io(why)) = &e else { return Err(e) };
                        Err(refused(format!("{why}; the library is as it was")))
                    }
                    Err(_) => Err(e),
                };
            }
        }
        backup::sync_dir(&aside);
        backup::sync_dir(library);
        pending.phase = Phase::Place;
        note(library, &pending)?;
    }
    for name in &pending.new {
        let (at, to) = (from.join(name), library.join(name));
        if !there(&at) {
            if there(&to) {
                // moved in already
                continue;
            }
            return Err(refused(format!(
                "{name} of the backup is neither in {} nor in the library. What the library held is in {}; with {PENDING} removed, the library opens as \
                 whatever is in place",
                from.display(),
                aside.display()
            )));
        }
        // (nothing of the library is there any more: this turned up since, made by a program
        // that didn't hold the library's lock)
        if there(&to) {
            set_aside(&to, &aside, name)?;
        }
        rename(&at, &to)?;
    }
    backup::sync_dir(library);
    step()?;
    // the copy is used up; a folder of `Replaced` that was taken back goes only if it is empty
    let removed = if pending.from.is_some() { std::fs::remove_dir(&from) } else { std::fs::remove_dir_all(&from) };
    if let Err(e) = removed
        && there(&from)
    {
        log::warn!("restore: can't remove {}: {e}", from.display());
    }
    forget(library)?;
    let mut report = pending.report;
    report.library = library.to_path_buf();
    report.aside = real_dir(&aside).then_some(aside);
    Ok(Some(report))
}

/// Complete a restore of `library` that was cut short after it had begun to move files. `None`:
/// there was none, or it could not go on and was undone; either way the library is whole.
/// Whoever opens a library calls this first, holding the library's lock.
pub fn finish(library: &Path) -> Result<Option<RestoreReport>, BackupError> {
    match pending(library)? {
        Some(pending) => roll(library, pending),
        None => Ok(None),
    }
}

/// Remove what a restore that never got to move a file left in `library`: the copy of a backup
/// (`restore.partial`), when no restore is under way (no note) and the folder holds nothing but
/// what such a copy holds. Whoever opens a library calls this after [`finish`], with its lock.
pub fn clear_leftover(library: &Path) {
    let staged = library.join(STAGED);
    if there(&library.join(PENDING)) || !real_dir(&staged) {
        return;
    }
    let of_a_copy = |n: &String| n == SNAPSHOT || n == LOG || segments::valid_name(n) || (movable(n) && n.ends_with(".json"));
    if backup::held(&staged).is_ok_and(|names| names.iter().all(of_a_copy))
        && let Err(e) = std::fs::remove_dir_all(&staged)
    {
        log::warn!("restore: can't remove {}: {e}", staged.display());
    }
}

/// Start what `note_of` describes: write it down, then do it. Refused with nothing changed when
/// it asks for more than a restore does. `begun` is called once the note is written: from there
/// on the restore is completed, now or by [`finish`].
fn begin(library: &Path, mut note_of: Pending, begun: impl FnOnce()) -> Result<RestoreReport, BackupError> {
    valid(library, &note_of).map_err(refused)?;
    if !whole(library, &note_of) {
        return Err(refused(format!("{} no longer holds the files to move in; nothing was changed", source(library, &note_of).display())));
    }
    let replaced = library.join(REPLACED);
    let aside = replaced.join(&note_of.aside);
    if there(&aside) {
        return Err(BackupError::Exists(note_of.aside));
    }
    note_of.report.replaced = note_of.old.iter().filter(|name| there(&library.join(name))).cloned().collect();
    // the folder for what is replaced, before anything is promised: a restore that can't make it
    // has not begun
    let had = there(&replaced);
    let made = !note_of.report.replaced.is_empty();
    if made {
        std::fs::create_dir_all(&aside).map_err(|e| backup::io("can't make the folder", &aside, e))?;
        backup::sync_dir(&replaced);
        backup::sync_dir(library);
    }
    if let Err(e) = note(library, &note_of) {
        if made {
            let _ = std::fs::remove_dir(&aside);
            if !had {
                let _ = std::fs::remove_dir(&replaced);
            }
        }
        return Err(e);
    }
    begun();
    roll(library, note_of)?.ok_or_else(|| refused("the files to move in vanished; the library is as it was"))
}

/// A backup copied into its library folder, checked, ready to be moved into place
/// ([`Staged::replace`]). Dropped without that, it leaves nothing.
pub struct Staged {
    library: PathBuf,
    aside: String,
    new: Vec<String>,
    old: Vec<String>,
    report: RestoreReport,
    placed: bool,
}

impl Drop for Staged {
    fn drop(&mut self) {
        let staged = self.library.join(STAGED);
        if !self.placed
            && let Err(e) = std::fs::remove_dir_all(&staged)
            && there(&staged)
        {
            log::warn!("restore: can't remove {}: {e}", staged.display());
        }
    }
}

/// Copy the backup in `backup` into the library folder `library` (made if it isn't there),
/// checking it against its marker. The library's own files are not touched: it can stay open.
/// One at a time for a library (its lock is the caller's).
pub fn stage(backup: &Path, library: &Path, options: &RestoreOptions<'_>, watch: &dyn Watch) -> Result<Staged, BackupError> {
    if !backup::valid_name(options.aside) {
        return Err(BackupError::Name(options.aside.to_string()));
    }
    if let Some(bad) = options.also.iter().find(|n| !valid_extra(n)) {
        return Err(BackupError::Name(bad.to_string()));
    }
    let report = backup::check(backup)?;
    if !report.contents.contains_key(SNAPSHOT) {
        return Err(BackupError::Changed(format!("{} holds no {SNAPSHOT}", backup.display())));
    }
    // a catalog this version opens (what a newer LightCraft wrote is found out here, not once it
    // is the library)
    let snapshot = backup::read(&backup.join(SNAPSHOT))?.unwrap_or_default();
    crate::journal::readable(&snapshot).map_err(|e| refused(format!("this version can't open the backup: {e}")))?;
    if report.contents.contains_key(LOG) {
        let log = backup::read(&backup.join(LOG))?.unwrap_or_default();
        crate::journal::log_readable(&log).map_err(|e| refused(format!("this version can't open the backup: {e}")))?;
    }
    // (a backup is only read: nothing is made in its folder)
    if within(library, backup) {
        return Err(refused(format!("{} is the backup's own folder, or in it; restore into a library folder", library.display())));
    }
    if backup::is_backup(library) {
        return Err(refused(format!("{} is a backup itself; restore into a library folder", library.display())));
    }
    if there(&library.join(PENDING)) {
        return Err(match pending(library) {
            Err(e) => e,
            Ok(_) => refused("a restore of this library was cut short; open the library to finish it first"),
        });
    }
    let replaced = library.join(REPLACED);
    if there(&replaced) && !real_dir(&replaced) {
        return Err(refused(format!("{} is not a folder, and what the restore replaces goes there", replaced.display())));
    }
    if there(&replaced.join(options.aside)) {
        return Err(BackupError::Exists(options.aside.to_string()));
    }
    let wanted = |name: &str| name == SNAPSHOT || name == LOG || segments::valid_name(name) || options.also.contains(&name);
    // (stopped or given up before it began: it must not clear away a copy that is not its own)
    if watch.cancelled() {
        return Err(BackupError::Cancelled);
    }
    std::fs::create_dir_all(library).map_err(|e| backup::io("can't make the folder", library, e))?;
    // what a restore that never got to move a file left: a folder of that name that holds
    // anything else is somebody's, and stays
    let staged = library.join(STAGED);
    if there(&staged) {
        let ours = real_dir(&staged) && backup::held(&staged).is_ok_and(|names| names.iter().all(|n| wanted(n)));
        if !ours {
            return Err(refused(format!("{} is in the way, and is not what a restore leaves", staged.display())));
        }
        std::fs::remove_dir_all(&staged).map_err(|e| backup::io("can't remove", &staged, e))?;
    }
    std::fs::create_dir(&staged).map_err(|e| backup::io("can't make the folder", &staged, e))?;
    let mut made = Staged {
        library: library.to_path_buf(),
        aside: options.aside.to_string(),
        new: Vec::new(),
        old: Vec::new(),
        report: RestoreReport {
            backup: backup.to_path_buf(),
            library: library.to_path_buf(),
            seq: report.seq,
            photos: report.photos,
            ..Default::default()
        },
        placed: false,
    };
    let total = report.contents.iter().filter(|(n, _)| wanted(n)).fold(0u64, |sum, (_, h)| sum.saturating_add(h.bytes));
    let mut done = 0u64;
    for (name, held) in &report.contents {
        if !wanted(name) {
            made.report.skipped.push(name.clone());
            continue;
        }
        if watch.cancelled() {
            return Err(BackupError::Cancelled);
        }
        watch.progress(name, done, total);
        // read again as it is copied: what goes into the library is what the marker lists
        let bytes = backup::read(&backup.join(name))?.filter(|b| b.len() as u64 == held.bytes && crc32fast::hash(b) == held.crc);
        let Some(bytes) = bytes else {
            return Err(BackupError::Changed(format!("{name} is not as it was when the backup was made")));
        };
        let to = staged.join(name);
        backup::folder_for(&to)?;
        backup::write(&to, &bytes)?;
        done = done.saturating_add(held.bytes);
        made.report.files += 1;
        made.report.bytes = made.report.bytes.saturating_add(held.bytes);
        let top = top(name).to_string();
        if !made.new.contains(&top) {
            made.new.push(top);
        }
    }
    // the snapshot is the last file moved in, and the first moved out
    made.new.sort_by_key(|n| n == SNAPSHOT);
    made.report.placed = made.new.clone();
    made.old = [SNAPSHOT, LOG, SEGMENT_DIR].iter().chain(options.also).map(|n| n.to_string()).collect();
    backup::sync_dir(&staged.join(SEGMENT_DIR));
    backup::sync_dir(&staged);
    Ok(made)
}

impl Staged {
    /// What the restore will report.
    pub fn report(&self) -> &RestoreReport {
        &self.report
    }

    /// Move what the library holds to `Replaced/<name>` and the backup's files into its place.
    /// Nothing may have the library's files open for writing: the caller holds its lock, and has
    /// closed its journal. Stopped part-way, [`finish`] completes it.
    pub fn replace(mut self) -> Result<RestoreReport, BackupError> {
        let library = self.library.clone();
        let note_of = Pending {
            phase: Phase::Aside,
            aside: self.aside.clone(),
            old: std::mem::take(&mut self.old),
            new: std::mem::take(&mut self.new),
            from: None,
            report: self.report.clone(),
        };
        begin(&library, note_of, || self.placed = true)
    }
}

/// Take back the restore `made` reports: what it put into the library folder goes to
/// `Replaced/<aside>`, and what it had moved out comes back from its folder there (which is
/// removed once empty; anything else in it stays). For a restored library that then can't be
/// opened; nothing may have changed in the library folder since. Cut short, [`finish`] completes
/// it.
pub fn take_back(made: &RestoreReport, aside: &str) -> Result<RestoreReport, BackupError> {
    if !backup::valid_name(aside) {
        return Err(BackupError::Name(aside.to_string()));
    }
    let library = made.library.as_path();
    if there(&library.join(PENDING)) {
        return Err(refused("a restore of this library was cut short; open the library to finish it first"));
    }
    // what was moved out: the files of its folder that the restore put there, the snapshot last
    let from = match &made.aside {
        Some(dir) if dir.parent() == Some(library.join(REPLACED).as_path()) => dir.file_name().and_then(|n| n.to_str()).map(str::to_string),
        Some(dir) => return Err(refused(format!("{} is not a folder of {REPLACED} in the library", dir.display()))),
        None => None,
    };
    let mut new: Vec<String> = match (&made.aside, &from) {
        (Some(dir), Some(_)) => made.replaced.iter().filter(|name| there(&dir.join(name))).cloned().collect(),
        _ => Vec::new(),
    };
    new.sort_by_key(|n| n == SNAPSHOT);
    let mut old = made.placed.clone();
    old.sort_by_key(|n| n != SNAPSHOT);
    let report = RestoreReport { backup: made.backup.clone(), library: library.to_path_buf(), placed: new.clone(), ..Default::default() };
    begin(library, Pending { phase: Phase::Aside, aside: aside.to_string(), old, new, from, report }, || {})
}

/// Restore the library in `library` from the backup in `backup`: [`finish`] a restore that was
/// cut short, [`stage`] the backup, and move it into place. For a library nothing has open.
pub fn restore(backup: &Path, library: &Path, options: &RestoreOptions<'_>, watch: &dyn Watch) -> Result<RestoreReport, BackupError> {
    if there(&library.join(PENDING)) {
        finish(library)?;
    }
    stage(backup, library, options, watch)?.replace()
}
