//! A record of file moves in progress, written into the library before the first file moves and
//! removed once the library has saved where the files went. If LightCraft is stopped part-way (a
//! force-quit, a crash, a power cut), the record is still there: it names every file the move
//! was to make, in the order they move. (Following a record the next time the library opens is a
//! later part; until then a record left by a stopped move stays where it is.)
//!
//! The records live in `<library>/moves/`, one file per batch, so a background move and a move
//! the session makes itself (a rename, an Undo) never share one.
//!
//! Nothing is deleted before the library has the move: a record goes only after the catalog that
//! points at the new places is saved ([`Session::clear_settled_moves`]); while a save is
//! failing, or the library couldn't follow, the record stays.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use lightcraft_catalog::store::{FsStore, Store};
use serde::{Deserialize, Serialize};

use crate::{EngineError, Result, Session};

/// The folder of the library that holds the records.
const DIR: &str = "moves";
/// The records' format, so a later one can tell an older file apart.
const VERSION: u32 = 1;

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
    /// save is failing they stay, so the files a record names are still told.
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
}
