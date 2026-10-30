//! A record of file moves in progress, written into the library before the first file moves and
//! removed once the library has saved where the files went. If LightCraft is stopped part-way (a
//! force-quit, a crash, a power cut), the record is still there the next time the library opens,
//! and [`Session::recover_moves`] brings the library up to date with the disk:
//!
//! * a file that reached its new place (and is gone from the old one): its photos point there,
//!   and its XMP sidecars follow it;
//! * a file still at its old place and not at the new one: nothing to do;
//! * a file at both places: the copy at the new place is ours (nothing was there when the record
//!   was written, and moves never replace a file). It is removed when it is the same file under a
//!   second name or holds the same bytes; the library keeps the original. Otherwise both stay and
//!   the person is told;
//! * a file at neither place, or gone from a folder that is gone too (its disk may just not be
//!   connected, and a copy at the new place may never have been checked): the record keeps it
//!   for the next time.
//!
//! The records live in `<library>/moves/`, one file per batch, so a background move and a move
//! the session makes itself (a rename, an Undo) never share one. Beside them, `held-sidecars`
//! lists the photos whose automatic sidecar write waits for a running move to bring their file
//! (edits made meanwhile): the next open makes those writes, and no others.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use lightcraft_catalog::store::{FsStore, Store};
use lightcraft_catalog::{Op, PhotoId, Source};
use serde::{Deserialize, Serialize};

use crate::rename::{MoveFs, folded_path};
use crate::{EngineError, Result, Session};

/// The folder of the library that holds the records.
const DIR: &str = "moves";
/// The list of sidecar writes held back for a running move, in [`DIR`] (not a record: records end
/// in `.json`).
const HELD: &str = "held-sidecars";
/// The records' format, so a later one can tell an older file apart.
const VERSION: u32 = 1;
/// Records larger than this aren't read (a damaged or planted file): 64 MiB is about half a
/// million moves.
const MAX_BYTES: u64 = 64 << 20;

#[derive(Serialize, Deserialize, Default)]
struct Record {
    version: u32,
    /// (from, to) for each file, in the order they move.
    moves: Vec<(String, String)>,
}

/// A record on disk: removed once the library has saved the moves it names
/// ([`Session::settle_moves`]).
#[derive(Debug)]
pub(crate) struct MoveRecord {
    path: PathBuf,
}

/// Write the record of `moves` into the library folder `library`, durably, before any of them
/// is made. Callable from a worker thread.
pub(crate) fn write(library: &Path, moves: &[(String, String)]) -> std::io::Result<MoveRecord> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = library.join(DIR);
    let fresh = !dir.is_dir();
    std::fs::create_dir_all(&dir)?;
    if fresh {
        // the new folder's entry in the library folder must survive a power cut too
        lightcraft_catalog::safe_file::sync_dir(library);
    }
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default();
    let name = format!("move-{}-{stamp}-{}.json", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
    let bytes = serde_json::to_vec(&Record { version: VERSION, moves: moves.to_vec() }).map_err(std::io::Error::other)?;
    FsStore::open(&dir)?.write_atomic(&name, &bytes)?;
    Ok(MoveRecord { path: dir.join(name) })
}

/// What the records left by an earlier run came to (`library.info` → `recoveredMoves`, and the
/// notice the CLI prints; the window shows only what a worker that died came to).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Recovered {
    /// Photos now pointing at the place their file had reached.
    pub relinked: usize,
    /// Copies at the new place removed (the original stayed where the library has it).
    pub copies_removed: usize,
    /// Files whose two places couldn't be decided on, records that can't be read, sidecars that
    /// couldn't follow.
    pub problems: Vec<MoveProblem>,
    /// Moves kept for the next time: the old place can't be read (a disk not connected?).
    pub waiting: usize,
}

/// Something about a move cut short that the person has to know.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum MoveProblem {
    /// The record (a path) can't be read; it stays as it is.
    UnreadableRecord { record: String, error: String },
    /// The file is at both places; the library keeps `from`.
    BothPlaces { from: String, to: String },
    /// The file reached `to`; one of its sidecars didn't follow.
    SidecarStayed { to: String, error: String },
    /// The library couldn't take the relinks in.
    NotFollowed { error: String },
    /// The relinks were made and couldn't be saved; the next open makes them again.
    NotSaved { error: String },
    /// Records of moves are there and the library is open without its lock (another program
    /// may be making them): nothing was touched.
    Unprotected,
}

impl std::fmt::Display for MoveProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MoveProblem::UnreadableRecord { record, error } => {
                write!(f, "A record of a move ({record}) can't be read ({error}); it was left as it is.")
            }
            MoveProblem::BothPlaces { from, to } => write!(f, "{to} was being moved and is now at both places: the library keeps {from}."),
            MoveProblem::SidecarStayed { to, error } => write!(f, "A sidecar of {to} couldn't follow it ({error})."),
            MoveProblem::NotFollowed { error } => {
                write!(f, "The library couldn't follow the files that were being moved ({error}); Find Missing Photos relinks them.")
            }
            MoveProblem::NotSaved { error } => write!(f, "The files that were being moved are followed, but that couldn't be saved ({error})."),
            MoveProblem::Unprotected => write!(
                f,
                "This library is open without its lock, so moves that were cut short are left as they are until it opens with the lock; Find Missing Photos relinks files that moved."
            ),
        }
    }
}

impl Recovered {
    /// Nothing to tell.
    pub fn is_empty(&self) -> bool {
        self.relinked == 0 && self.copies_removed == 0 && self.problems.is_empty() && self.waiting == 0
    }

    /// The notice in English (the log, the CLI's stderr), or `None` when there is nothing to say.
    pub fn notice(&self) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let mut text = String::new();
        if self.relinked > 0 {
            let (s, were) = if self.relinked == 1 { ("", "was") } else { ("s", "were") };
            text.push_str(&format!(
                "{} photo{s} {were} being moved when LightCraft last closed: the library now points at where the files are. Edit → Undo moves them back.",
                self.relinked
            ));
        }
        if self.copies_removed > 0 {
            let what = if self.copies_removed == 1 {
                "1 copy of a file that was being moved was".to_string()
            } else {
                format!("{} copies of files that were being moved were", self.copies_removed)
            };
            text.push_str(&format!(" {what} removed from the new place: the original was still where the library has it."));
        }
        if self.waiting > 0 {
            let (s, were) = if self.waiting == 1 { ("", "was") } else { ("s", "were") };
            text.push_str(&format!(
                " {} file{s} that {were} being moved can't be reached (a disk not connected?): LightCraft looks again the next time this library opens.",
                self.waiting
            ));
        }
        for p in &self.problems {
            text.push(' ');
            text.push_str(&p.to_string());
        }
        Some(text.trim().to_string())
    }
}

/// Where one recorded move stands on disk.
enum Found {
    /// Reached `to`: point the photos there.
    Moved,
    /// Still at `from` only, or settled already: nothing to do.
    Stayed,
    /// The old place doesn't answer (a disk not connected?): keep the move for next time.
    Unknown,
    /// Reached `to` (the photos point there), and a sidecar couldn't follow it: keep the move
    /// for next time, which takes the sidecars along again.
    SidecarsLeft,
}

impl MoveRecord {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Session {
    /// Write the record of `moves` (made by this session, on this thread) before making them;
    /// `None` when the library has no folder of its own (nothing survives a restart there) or
    /// there's nothing to move.
    pub(crate) fn record_moves(&self, moves: &[(String, String)]) -> Result<Option<MoveRecord>> {
        let Some(dir) = self.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.clone()) else { return Ok(None) };
        if moves.is_empty() {
            return Ok(None);
        }
        write(&dir, moves).map(Some).map_err(|e| EngineError::Other(format!("can't note the move in the library ({e}): nothing was moved")))
    }

    /// The moves `record` names are in the catalog now: the record goes once that is saved
    /// (after the command, see [`Session::clear_settled_moves`]).
    pub(crate) fn settle_moves(&mut self, record: Option<MoveRecord>) {
        if let Some(r) = record {
            self.settled_moves.push(r.path);
        }
    }

    /// Remove the records of settled moves once everything the library holds is saved; while a
    /// save is failing they stay, and the next open finds the files where the record says.
    pub(crate) fn clear_settled_moves(&mut self) {
        if self.settled_moves.is_empty() || !self.pending_log.is_empty() {
            return;
        }
        for path in std::mem::take(&mut self.settled_moves) {
            if let Err(e) = std::fs::remove_file(&path)
                && e.kind() != std::io::ErrorKind::NotFound
            {
                log::warn!("move record {}: {e}", path.display());
            }
        }
    }

    /// Keep the list of sidecar writes held back for a move ([`Session::held_sidecars`]) beside
    /// the records, so a run that ends before the move does still makes them at the next open.
    /// A listed write stays listed until it is made: while it is held, or after it failed (the
    /// file not reachable yet); the list goes once none is left.
    pub(crate) fn save_held_sidecars(&mut self) {
        let Some(dir) = self.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.join(DIR)) else { return };
        let mut ids: std::collections::BTreeSet<PhotoId> = read_held(&dir).into_iter().collect();
        let waiting = |id: &PhotoId| self.held_sidecars.contains(id) || self.unsaved_sidecars.contains(id);
        if self.library.as_ref().is_some_and(|l| l.is_unprotected()) {
            // without the lock, the entries an earlier run (or another program) left wait for an
            // open that has it: only what this session listed itself is taken back
            ids.retain(|id| !self.held_listed.contains(id) || waiting(id));
        } else {
            ids.retain(waiting);
        }
        ids.extend(self.held_sidecars.iter().copied());
        let listed: std::collections::BTreeSet<PhotoId> =
            self.held_listed.iter().chain(self.held_sidecars.iter()).copied().filter(|id| ids.contains(id)).collect();
        if ids.is_empty() {
            if remove_held(&dir) {
                self.held_listed = listed;
            }
            return;
        }
        let ids: Vec<PhotoId> = ids.into_iter().collect();
        let written = serde_json::to_vec(&ids)
            .map_err(std::io::Error::other)
            .and_then(|bytes| std::fs::create_dir_all(&dir).and_then(|()| FsStore::open(&dir)?.write_atomic(HELD, &bytes)));
        match written {
            // (only a list that is written says what this session has listed)
            Ok(()) => self.held_listed = listed,
            Err(e) => log::warn!("held sidecar writes {}: {e}", dir.join(HELD).display()),
        }
    }

    /// Bring the library up to date with moves left unfinished (see the module docs): those of an
    /// earlier run when the library opens, or of a move whose worker died. The relinks are one undo
    /// step; what was done is returned for a notice.
    pub fn recover_moves(&mut self) -> Recovered {
        self.recover_own_move(None)
    }

    /// [`Session::recover_moves`] (every record) when `own` is `None`. With `own`, the record this
    /// process's own worker wrote and whose worker died: that record only, with the library's lock
    /// or without it (no other program is making that move; the others were told when the library opened).
    pub(crate) fn recover_own_move(&mut self, own: Option<&Path>) -> Recovered {
        let locked = !self.library.as_ref().is_some_and(|l| l.is_unprotected());
        self.recover_records(&crate::rename::RealFs, locked, own)
    }

    /// `locked`: this program holds the library's lock. Without it another program may be making
    /// a move right now, and the records are its, so they are left exactly as they are.
    #[cfg(test)]
    pub(crate) fn recover_moves_with(&mut self, fs: &dyn MoveFs, locked: bool) -> Recovered {
        self.recover_records(fs, locked, None)
    }

    fn recover_records(&mut self, fs: &dyn MoveFs, locked: bool, own: Option<&Path>) -> Recovered {
        let mut out = Recovered::default();
        let Some(dir) = self.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.join(DIR)) else { return out };
        let Ok(entries) = std::fs::read_dir(&dir) else { return out };
        let mut files: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        files.sort();
        if !locked {
            let ours = |p: &PathBuf| own == Some(p.as_path());
            // (a dead worker's own move: the other records were already reported when the library opened)
            if own.is_none() && files.iter().any(|p| p.extension().is_some_and(|x| x == "json") && !self.settled_moves.contains(p) && !ours(p)) {
                out.problems.push(MoveProblem::Unprotected);
            }
            // only the record of this process's own dead worker is followed
            files.retain(ours);
            if files.is_empty() {
                return out;
            }
        } else if own.is_some() {
            // a dead worker's move is told by its own record: those of earlier runs were followed when the library opened
            files.retain(|p| own == Some(p.as_path()));
        }
        // the photos pointing at each file (a master and its virtual copies), looked up once a
        // record is there to read
        let mut at: Option<HashMap<PathBuf, Vec<(PhotoId, String)>>> = None;
        let mut partials = HashMap::new();
        let mut ops = Vec::new();
        // (record, the moves to keep in it): written back, or removed when none is left
        let mut done: Vec<(PathBuf, Vec<(String, String)>)> = Vec::new();
        for path in files {
            let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            if name.ends_with(".tmp") {
                // a record cut short while it was written: nothing had moved yet
                let _ = std::fs::remove_file(&path);
                continue;
            }
            // this session's own records whose moves wait to be saved (they go once they are)
            if !name.ends_with(".json") || self.settled_moves.contains(&path) {
                continue;
            }
            let record = match read_record(&path) {
                Ok(r) => r,
                Err(e) => {
                    out.problems.push(MoveProblem::UnreadableRecord { record: path.display().to_string(), error: e });
                    continue;
                }
            };
            let mut keep = Vec::new();
            let at = at.get_or_insert_with(|| {
                let mut at: HashMap<PathBuf, Vec<(PhotoId, String)>> = HashMap::new();
                for p in self.catalog.photos() {
                    if let Source::File { path } = &p.source {
                        at.entry(PathBuf::from(path)).or_default().push((p.id, p.file_name.clone()));
                    }
                }
                at
            });
            for (from, to) in record.moves {
                match recover_one(fs, locked, at, &mut partials, (&from, &to), &mut ops, &mut out) {
                    Found::Moved | Found::Stayed => {}
                    // the photos follow; the record keeps the move until its sidecars do too
                    Found::SidecarsLeft => keep.push((from, to)),
                    Found::Unknown => {
                        out.waiting += 1;
                        keep.push((from, to));
                    }
                }
            }
            done.push((path, keep));
        }
        let n = ops.len();
        if n > 0 {
            let s = if n == 1 { "" } else { "s" };
            if let Err(e) = self.commit(&format!("Finish Moving {n} Photo{s}"), Op::Batch { ops }) {
                out.problems.push(MoveProblem::NotFollowed { error: e.to_string() });
                return out;
            }
            out.relinked = n;
        }
        // edits made while the files moved had their sidecar writes held back, and those were
        // lost with the run: write them now, where the photos are (those that fail, a file not
        // reachable yet, stay listed for the next time)
        let held = if locked { read_held(&dir) } else { Vec::new() };
        if !held.is_empty() {
            if self.xmp.auto_write {
                self.auto_write_sidecar_ids(held);
                self.save_held_sidecars();
            } else {
                remove_held(&dir);
            }
        }
        if n > 0
            && let Err(e) = self.persist()
        {
            // the records stay: the next open finds the files where they are and does this again
            out.problems.push(MoveProblem::NotSaved { error: e.to_string() });
            return out;
        }
        for (path, keep) in done {
            let kept = if keep.is_empty() {
                std::fs::remove_file(&path).map_err(|e| e.to_string())
            } else {
                serde_json::to_vec(&Record { version: VERSION, moves: keep }).map_err(|e| e.to_string()).and_then(|bytes| {
                    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    FsStore::open(&dir).and_then(|mut st| st.write_atomic(&name, &bytes)).map_err(|e| e.to_string())
                })
            };
            if let Err(e) = kept {
                log::warn!("move record {}: {e}", path.display());
            }
        }
        out
    }
}

/// One recorded move: where the file is, and what the library does about it. `at`: the photos
/// pointing at each file, kept up to date as moves are followed; `partials`: see
/// [`remove_partials`].
fn recover_one(
    fs: &dyn MoveFs,
    locked: bool,
    at: &mut HashMap<PathBuf, Vec<(PhotoId, String)>>,
    partials: &mut HashMap<PathBuf, Vec<PathBuf>>,
    (from, to): (&str, &str),
    ops: &mut Vec<Op>,
    out: &mut Recovered,
) -> Found {
    let (f, t) = (Path::new(from), Path::new(to));
    // the same file spelled another way (a change of letter case only): nothing to remove, ever
    let respelling = folded_path(from) == folded_path(to) && fs.same_file(f, t);
    if at.get(f).is_none_or(Vec::is_empty) {
        // the library already follows the move (it was saved), or the photo is gone. A sidecar
        // that couldn't follow its file last time is taken along now
        if !respelling && at.get(t).is_some_and(|v| !v.is_empty()) && !fs.exists(f) && fs.is_file(t) {
            return follow_sidecars(fs, from, to, out);
        }
        return Found::Stayed;
    }
    let moved = if respelling {
        fs.listed(t) && !fs.listed(f)
    } else {
        match (fs.exists(f), fs.is_file(t)) {
            // gone from a folder that is still there: it moved. With the folder gone too (a link
            // to it is followed), its disk may just not be connected, and the copy at `to` was
            // maybe never checked
            (false, true) if f.parent().is_some_and(Path::is_dir) => true,
            (false, true) => return Found::Unknown,
            (true, false) => false,
            (true, true) => {
                // the copy at `to` is ours: drop it when that loses nothing. One file is dropped
                // from `to` only when both names are really there (a move cut between link and
                // unlink), never when they are one name spelled two ways
                let same = if fs.same_file(f, t) { fs.listed(f) && fs.listed(t) } else { crate::rename::same_contents(f, t).unwrap_or(false) };
                // (without the lock, the copy may be another program's move in progress: left alone;
                // a photo of the library at `to` is never a copy of ours)
                if locked && same && at.get(t).is_none_or(Vec::is_empty) && fs.remove_file(t).is_ok() {
                    out.copies_removed += 1;
                } else {
                    out.problems.push(MoveProblem::BothPlaces { from: from.to_string(), to: to.to_string() });
                }
                false
            }
            (false, false) => return Found::Unknown,
        }
    };
    remove_partials(t, partials, locked);
    if !moved {
        return Found::Stayed;
    }
    let sidecars = if respelling { Found::Moved } else { follow_sidecars(fs, from, to, out) };
    let (old_name, new_name) = (file_name(f), file_name(t));
    let photos = at.remove(f).unwrap_or_default();
    for (id, file_name) in &photos {
        // a rename changes the name the library shows; a move to another folder keeps it
        let file_name = if *file_name == old_name { new_name.clone() } else { file_name.clone() };
        ops.push(Op::SetFile { id: *id, file_name, source: Source::File { path: to.to_string() } });
    }
    at.entry(t.to_path_buf()).or_default().extend(photos.into_iter().map(|(id, _)| (id, new_name.clone())));
    sidecars
}

/// Take the sidecars of the file that went from `from` to `to` along: [`Found::Moved`], or
/// [`Found::SidecarsLeft`] (named in the notice) when one couldn't follow.
fn follow_sidecars(fs: &dyn MoveFs, from: &str, to: &str, out: &mut Recovered) -> Found {
    match crate::rename::finish_sidecars(fs, from, to) {
        Ok(_) => Found::Moved,
        Err(e) => {
            out.problems.push(MoveProblem::SidecarStayed { to: to.to_string(), error: e });
            Found::SidecarsLeft
        }
    }
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
}

/// The sidecar writes a run held back for its move and couldn't make (none when the list is
/// missing, too large or unreadable: logged).
fn read_held(dir: &Path) -> Vec<PhotoId> {
    let path = dir.join(HELD);
    let read = std::fs::metadata(&path)
        .and_then(|m| if m.len() > MAX_BYTES { Err(std::io::Error::other(format!("{} bytes", m.len()))) } else { std::fs::read(&path) });
    match read.map_err(|e| e.to_string()).and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string())) {
        Ok(ids) => ids,
        Err(e) => {
            if path.exists() {
                log::warn!("held sidecar writes {}: {e}", path.display());
            }
            Vec::new()
        }
    }
}

/// Whether the list is gone now.
fn remove_held(dir: &Path) -> bool {
    let path = dir.join(HELD);
    match std::fs::remove_file(&path) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(e) => {
            log::warn!("held sidecar writes {}: {e}", path.display());
            false
        }
    }
}

fn read_record(path: &Path) -> std::result::Result<Record, String> {
    let len = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if len > MAX_BYTES {
        return Err(format!("{len} bytes"));
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let r: Record = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if r.version > VERSION {
        return Err(format!("written by a newer LightCraft (format {})", r.version));
    }
    Ok(r)
}

/// Hidden partial copies a cut-off copy left beside `to` (`.<name>.lc-partial-<pid>-<k>`, see
/// `RealFs::copy_no_replace`): never a photo, safe to remove (without the lock, only this process's own). `partials`: each folder's partial
/// copies, listed once however many files of a move went there (a folder of thousands is read
/// once, not once per file).
fn remove_partials(to: &Path, partials: &mut HashMap<PathBuf, Vec<PathBuf>>, locked: bool) {
    let (Some(dir), Some(name)) = (to.parent(), to.file_name()) else { return };
    // (without the lock, only this process's own: another program's may be being written)
    let prefix = if locked {
        format!(".{}.lc-partial-", name.to_string_lossy())
    } else {
        format!(".{}.lc-partial-{}-", name.to_string_lossy(), std::process::id())
    };
    let listed = partials.entry(dir.to_path_buf()).or_insert_with(|| {
        std::fs::read_dir(dir)
            .map(|entries| entries.flatten().filter(|e| e.file_name().to_string_lossy().contains(".lc-partial-")).map(|e| e.path()).collect())
            .unwrap_or_default()
    });
    listed.retain(|p| {
        if p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(&prefix)) {
            let _ = std::fs::remove_file(p);
            false
        } else {
            true
        }
    });
}
