//! Moving photos to another folder on disk (`photo.moveToFolder`) as a job in two halves, so the
//! app can run the slow part on a worker thread: copying a large raw or video to a NAS takes as
//! long as the network needs, and no frame should wait for it.
//!
//! * [`start`] (on the session): the checks that need only the library (Local photos, files the
//!   library doesn't have, names clashing within the batch), the plan, and the activity row.
//! * [`MoveWork::run`] (anywhere): the checks that read the disk (the folder, each file, names
//!   already taken there), then the files, one at a time, reporting progress to the activity row.
//! * [`MoveFinish::finish`] (on the session): the library follows the files that moved, as one
//!   undo step.
//!
//! While a move runs, the session refuses anything else that moves files or opens another library
//! ([`Session::refuse_while_moving`]), and doesn't write the XMP sidecars of the photos on their way
//! (they're written once the move ends, at the new place).

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Weak};

use lightcraft_catalog::{Op, PhotoId, Source};
use serde_json::{Value, json};

use crate::activity::{Cancel, TaskGuard, TaskHandle};
use crate::cmd::bad;
use crate::rename::{BatchMoveError, MoveFs, RealFs, RenamePlan, folded_path};
use crate::{EngineError, Result, Session};

const C: &str = "photo.moveToFolder";

/// The files of a move in progress; the session holds it weakly ([`Session::moving`]), so the move
/// ends for the session when its [`MoveFinish`] goes, however it goes.
#[derive(Debug, Default)]
pub struct MoveTicket {
    /// The old paths, folded (see [`folded_path`]).
    paths: HashSet<String>,
}

impl MoveTicket {
    fn holds(&self, path: &str) -> bool {
        self.paths.contains(&folded_path(path))
    }
}

/// The half of a move that reads and writes the disk ([`MoveWork::run`]); `Send`, for a worker.
pub struct MoveWork {
    folder: String,
    /// (from, to), one per file (a master and its virtual copies move once).
    moves: Vec<(String, String)>,
    task: TaskHandle,
}

/// What [`MoveWork::run`] did, for [`MoveFinish::finish`].
pub struct MoveOutcome(Ran);

enum Ran {
    /// Nothing moved, and why (a name taken, a file offline…).
    Refused(String),
    /// Every file moved.
    Moved,
    /// The worker died part-way and left no outcome: the disk says what moved.
    Died,
    /// A file couldn't move: the earlier ones were moved back (what couldn't be is in `error`).
    Failed { error: BatchMoveError },
}

/// The half of a move that brings the library along ([`MoveFinish::finish`]); it holds the
/// activity row and the session's [`MoveTicket`].
pub struct MoveFinish {
    folder: String,
    plans: Vec<RenamePlan>,
    identity: Arc<()>,
    ticket: Arc<MoveTicket>,
    guard: TaskGuard,
}

impl std::fmt::Debug for MoveFinish {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MoveFinish").field("folder", &self.folder).field("files", &self.plans.len()).finish()
    }
}

/// Plan the move `p` asks for (`{folder, ids?}`; the selection by default). `None` when every
/// photo is in that folder already.
pub fn start(s: &mut Session, p: &Value) -> Result<Option<(MoveWork, MoveFinish)>> {
    let folder = crate::cmd::browse::folder_param(p, "folder").ok_or_else(|| bad(C, "missing folder"))?;
    let dir = Path::new(folder);
    s.refuse_while_moving()?;
    // refused before anything touches the disk, as it always was (one look at one folder; the
    // worker looks again before the first file moves)
    if !dir.is_dir() {
        return Err(bad(C, format!("{folder} is not a folder (create it first with folder.create)")));
    }
    let ids = s.targets(p).map_err(|e| match e {
        EngineError::BadParams { cmd, msg } if cmd.is_empty() => bad(C, msg),
        e => e,
    })?;
    if ids.is_empty() {
        return Err(bad(C, "no photos to move: select some or give ids"));
    }
    let mut plans: Vec<RenamePlan> = Vec::new();
    let mut taken: HashSet<String> = HashSet::new();
    let mut clashes: Vec<String> = Vec::new();
    for id in ids {
        let ph = s.catalog.photo(id).ok_or_else(|| bad(C, format!("no photo {}", id.0)))?;
        if ph.local {
            return Err(bad(C, format!("{} is not in the library (Local): add it first", ph.file_name)));
        }
        let Source::File { path } = &ph.source else {
            return Err(bad(C, format!("{} has no file on disk", ph.file_name)));
        };
        if plans.iter().any(|pl| pl.from_path.as_deref() == Some(path.as_str())) {
            continue; // a virtual copy of a photo already moving
        }
        let src = Path::new(path);
        let Some(file) = src.file_name() else { return Err(bad(C, format!("{path}: no file name"))) };
        if src.parent() == Some(dir) {
            continue; // already there
        }
        let to = dir.join(file).to_string_lossy().to_string();
        if !taken.insert(to.to_lowercase()) {
            clashes.push(file.to_string_lossy().to_string());
            continue;
        }
        let name = ph.file_name.clone();
        plans.push(RenamePlan { id: id.0, from: name.clone(), to: name, from_path: Some(path.clone()), to_path: Some(to) });
    }
    if !clashes.is_empty() {
        return Err(bad(C, clash_message(&clashes, folder)));
    }
    if plans.is_empty() {
        return Ok(None);
    }
    // a sidecar repair left by an earlier step goes first, as before any file move
    s.repair_sidecars_with(&RealFs)?;
    let moves: Vec<(String, String)> = plans.iter().filter_map(|pl| Some((pl.from_path.clone()?, pl.to_path.clone()?))).collect();
    let ticket = Arc::new(MoveTicket { paths: moves.iter().map(|(from, _)| folded_path(from)).collect() });
    s.moving = Arc::downgrade(&ticket);
    let guard = s.activity.start("move", "Moving photos", Cancel::No);
    guard.progress(0, moves.len() as u64);
    let work = MoveWork { folder: folder.to_string(), moves, task: guard.handle() };
    let finish = MoveFinish { folder: folder.to_string(), plans, identity: Arc::clone(&s.library_identity), ticket, guard };
    Ok(Some((work, finish)))
}

fn clash_message(names: &[String], folder: &str) -> String {
    let s = if names.len() == 1 { "" } else { "s" };
    format!("{} already in {folder}: nothing was moved. Rename the photo{s} first or choose another folder", names.join(", "))
}

impl MoveWork {
    /// Do the move on the disk (blocking: on a worker thread in the app).
    pub fn run(self) -> MoveOutcome {
        self.run_with(&RealFs)
    }

    pub(crate) fn run_with(self, fs: &dyn MoveFs) -> MoveOutcome {
        let MoveWork { folder, moves, task } = self;
        if !Path::new(&folder).is_dir() {
            return MoveOutcome(Ran::Refused(format!("{folder} is not a folder (create it first with folder.create)")));
        }
        let mut clashes = Vec::new();
        for (from, to) in &moves {
            if !fs.is_file(Path::new(from)) {
                return MoveOutcome(Ran::Refused(format!("{from} is missing or offline: find it first (Find Missing Photos)")));
            }
            if fs.exists(Path::new(to)) {
                clashes.push(name_of(to));
            }
        }
        if !clashes.is_empty() {
            return MoveOutcome(Ran::Refused(clash_message(&clashes, &folder)));
        }
        let total = moves.len() as u64;
        let ran = crate::rename::move_all_with(fs, &moves, &mut |k, (from, _)| {
            task.progress(k as u64, total);
            task.detail(&name_of(from));
        });
        match ran {
            Ok(()) => {
                task.progress(total, total);
                MoveOutcome(Ran::Moved)
            }
            Err(error) => MoveOutcome(Ran::Failed { error }),
        }
    }
}

fn name_of(path: &str) -> String {
    Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string())
}

impl MoveFinish {
    /// How many files the move is for.
    pub fn files(&self) -> usize {
        self.plans.len()
    }

    /// The activity row's id (`activity.list`).
    pub fn task_id(&self) -> u64 {
        self.guard.id()
    }

    /// The worker died part-way (a panic, logged; this path runs only then) and left no outcome: the move is over for the
    /// session. The library follows the files that reached the new place (and are gone from the
    /// old one); the sidecars held back meanwhile are written where their photos now are.
    pub fn abandon(self, s: &mut Session) -> Result<Value> {
        self.finish(s, MoveOutcome(Ran::Died))
    }

    /// Point the photos whose files moved at their new place, as one undo step (labelled as
    /// before: "Move 3 Photos to “Porto”", counting virtual copies). `{moved, folder}` (in files);
    /// an error when nothing moved (`Refused`, `Failed`: the files went back, or the library
    /// follows those that couldn't). The sidecars held back meanwhile are written after.
    pub fn finish(self, s: &mut Session, out: MoveOutcome) -> Result<Value> {
        let MoveFinish { folder, plans, identity, ticket, guard } = self;
        // the move is over for the session: other file work, and these photos' sidecars, may go on
        drop(ticket);
        drop(guard);
        let held: Vec<PhotoId> = std::mem::take(&mut s.held_sidecars).into_iter().collect();
        if !Arc::ptr_eq(&identity, &s.library_identity) {
            // (opening a library is refused while a move runs)
            return Err(EngineError::Other(
                "the library changed while photos were being moved: the files are in their new folder, but the library still points at the old places (File ▸ Find Missing Photos… relinks them)".into(),
            ));
        }
        let r = match out.0 {
            Ran::Refused(why) => Err(bad(C, why)),
            Ran::Failed { error } => {
                let op = Op::Batch { ops: s.set_file_ops(&plans) };
                Err(EngineError::Other(s.follow_stuck(&op, error, true).0))
            }
            Ran::Moved => s.follow_moved(&folder, &plans),
            Ran::Died => {
                let arrived: Vec<RenamePlan> = plans
                    .into_iter()
                    .filter(|pl| match (&pl.from_path, &pl.to_path) {
                        (Some(from), Some(to)) => !Path::new(from).is_file() && Path::new(to).is_file(),
                        _ => false,
                    })
                    .collect();
                s.follow_moved(&folder, &arrived)
            }
        };
        // (a sidecar write the person switched off meanwhile isn't made now)
        if s.xmp.auto_write {
            s.auto_write_sidecar_ids(held);
        }
        r
    }
}

fn label(n: usize, folder: &str) -> String {
    let name = Path::new(folder).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_else(|| folder.to_string());
    format!("Move {n} Photo{} to “{name}”", if n == 1 { "" } else { "s" })
}

impl Session {
    /// The library follows the files `done` moved: one undo step. When it can't (a failed
    /// commit), the files go back.
    fn follow_moved(&mut self, folder: &str, done: &[RenamePlan]) -> Result<Value> {
        let ops = self.set_file_ops(done);
        if ops.is_empty() {
            return Ok(json!({"moved": 0, "folder": folder}));
        }
        match self.commit(&label(ops.len(), folder), Op::Batch { ops }) {
            Ok(()) => Ok(json!({"moved": done.len(), "folder": folder})),
            Err(err) => {
                // the library can't follow: the files go back
                let back: Vec<(String, String)> = done.iter().rev().filter_map(|pl| Some((pl.to_path.clone()?, pl.from_path.clone()?))).collect();
                match crate::rename::move_all(&RealFs, &back) {
                    Ok(()) => Err(err),
                    Err(e) => Err(EngineError::Other(format!("{err}; the moved files could not all be moved back: {}", e.message))),
                }
            }
        }
    }

    /// The move running in the background, if any.
    pub fn move_running(&self) -> bool {
        self.moving.strong_count() > 0
    }

    /// Anything else that moves files, or opens another library, waits for a running move: the
    /// two would work on the same files, and the move's photos belong to this library.
    pub(crate) fn refuse_while_moving(&self) -> Result<()> {
        if self.move_running() {
            return Err(EngineError::Other("photos are being moved to another folder: wait for the move to finish".into()));
        }
        Ok(())
    }

    /// The file `path` is on its way to another folder (its sidecar waits until it arrives).
    pub(crate) fn is_moving(&self, path: &str) -> bool {
        Weak::upgrade(&self.moving).is_some_and(|t| t.holds(path))
    }
}
