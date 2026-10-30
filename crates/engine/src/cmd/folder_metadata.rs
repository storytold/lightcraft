//! Save Metadata to Files and Read Metadata from Files for a whole folder of the library
//! (`folder.saveMetadata`, `folder.readMetadata`): what `photo.saveMetadataToFile` and
//! `photo.readMetadataFromFile` do for the selection, for every library photo of one folder and
//! the folders inside it, with a row in the activity stack (kinds `saveMetadata` and
//! `readMetadata`, "n of N", the file in hand as detail, ✕ stops it). The Folders and Local
//! folder menus' Save/Read Metadata items will use it.
//!
//! - **Scope** is the Folders tree's: the library photos imported from the folder or any folder
//!   inside it (`library.removeFolder`'s rule, `Filter::library_folder`, the path as given). Local-only
//!   records seen while browsing are not library photos and are left alone, as are photos in
//!   Recently Deleted and virtual copies (their settings live only in the library; a read would
//!   put the master's file over the copy's own settings).
//! - **Work is done photo by photo** by a [`FolderMetadataJob`]: the session is needed for every
//!   file (sidecar naming, the repair queues), so the job is paced rather than moved to another
//!   thread, as the focus-point reader is. The commands run it in two ways. Plainly
//!   (`{path}`), one call plans it, does every photo and ends it: for scripts, the CLI and MCP,
//!   where nothing else runs meanwhile, so the row can't be stopped then. With `background: true`
//!   the call only plans it and returns; a frontend calls `folder.metadataPump` every frame, which
//!   does photos for a few milliseconds and ends the job after the last, so the window keeps
//!   drawing, the row shows its count and its ✕ (or `activity.cancel`) stops the job at the next
//!   pump (a stopped job nobody pumps is ended by the next folder command, a stopped read then
//!   committing what it read as its own undo step). One job runs at a time; opening another
//!   library drops it. The work never goes through the automatic sidecar writes: an explicit
//!   save writes its files before it reports.
//!   Which photos share a stem sidecar is worked out again whenever the catalog changed since the
//!   last photo (never during a plain call), so a file imported while a paced job waits is
//!   named as the photo commands would name it.
//! - **Cancel** is checked before each photo. A canceled save keeps the sidecars already
//!   written. A canceled read commits what it has read so far as one undo step. Either way the
//!   result has `cancelled: true`, and `done` of `total` says how far it got.
//! - **A file that can't be saved or read** (offline, missing, read-only, not XMP) lands in
//!   `failed` with its photo id and the reason, and the rest go on. So does a photo that was
//!   deleted, moved or edited while a paced job was between two pumps: a read never puts a
//!   sidecar over an edit made after the file was read, and a save never writes a photo that has
//!   left the folder.
//! - **A read** is one undo step ("Read Metadata from Files"), and never writes the sidecars it
//!   has read, auto-write or not (`Session::skip_auto_write`, as `photo.readMetadataFromFile`
//!   does, issue #934); [`FolderMetadataJob::finish`] must therefore run inside a command.

use std::sync::Arc;
use std::time::Duration;

use lightcraft_catalog::{Filter, Op, Photo, PhotoId, Sort, Source};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, bool_or, cmd, str_param};
use crate::activity::{Cancel, TaskGuard};
use crate::sidecar::{SidecarNaming, StemOwners};
use crate::{Result, Session};

/// What a job does with each photo's file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// Write the photo's XMP sidecar.
    Save,
    /// Take the photo's sidecar (or embedded XMP) into the catalog.
    Read,
}

impl Direction {
    fn command(self) -> &'static str {
        match self {
            Direction::Save => "folder.saveMetadata",
            Direction::Read => "folder.readMetadata",
        }
    }
}

/// A photo to do and the file it had when the job was planned (a photo removed, moved or relinked
/// meanwhile is reported in `failed`, not written under another file's name).
struct Item {
    id: PhotoId,
    path: String,
}

/// What the job needs from the whole library, worked out when it is planned and again at the next
/// photo after the catalog changed (per photo it would be a pass over the catalog each).
enum Naming {
    /// The owners of the stems the photos share with other files.
    Save(StemOwners),
    /// The sidecar naming of each item, in order.
    Read(Vec<SidecarNaming>),
}

/// A photo read: its catalog entry as it was then, and what came of the file. The op is dropped
/// at [`FolderMetadataJob::finish`] when the photo is no longer that entry.
struct Read {
    id: PhotoId,
    photo: Arc<Photo>,
    op: Op,
    from: String,
}

/// A folder's metadata save or read in progress: planned without touching the disk, advanced one
/// photo per [`FolderMetadataJob::step`], ended by [`FolderMetadataJob::finish`]. Dropping it
/// ends its row in the activity stack and abandons what a read had gathered.
pub struct FolderMetadataJob {
    direction: Direction,
    items: Vec<Item>,
    next: usize,
    task: TaskGuard,
    naming: Naming,
    /// The catalog revision `naming` was worked out at.
    revision: u64,
    cancelled: bool,
    written: Vec<String>,
    merged: Vec<String>,
    backups: Vec<Value>,
    reads: Vec<Read>,
    warnings: Vec<crate::import::ImportWarning>,
    failed: Vec<Value>,
}

impl FolderMetadataJob {
    /// The job for the library photos in `folder` and the folders inside it. No file is touched.
    pub fn plan(s: &Session, direction: Direction, folder: &str) -> Result<FolderMetadataJob> {
        let c = direction.command();
        // as given, like every folder command: a folder named with a space at its edge is not its neighbor
        if folder.trim().is_empty() || lightcraft_catalog::query::folder_key(folder).is_empty() {
            return Err(bad(c, "`path` must be a folder from library.folders"));
        }
        let filter = Filter { library_folder: Some(folder.to_string()), ..Default::default() };
        let ids = s.catalog.query(&filter, &Sort::default());
        if ids.is_empty() {
            return Err(bad(c, format!("{folder}: no photo in the library was imported from it")));
        }
        let items: Vec<Item> = ids
            .iter()
            .filter_map(|id| {
                let p = s.catalog.photo(*id)?;
                match &p.source {
                    Source::File { path } if p.copy_of.is_none() => Some(Item { id: *id, path: path.clone() }),
                    _ => None,
                }
            })
            .collect();
        if items.is_empty() {
            return Err(bad(c, format!("{folder}: its photos have no file of their own")));
        }
        let ids: Vec<PhotoId> = items.iter().map(|i| i.id).collect();
        let naming = match direction {
            Direction::Save => Naming::Save(StemOwners::among(&s.catalog, &ids)),
            Direction::Read => Naming::Read(s.sidecar_namings(&ids)),
        };
        let (kind, label) = match direction {
            Direction::Save => ("saveMetadata", "Saving metadata to files"),
            Direction::Read => ("readMetadata", "Reading metadata from files"),
        };
        let task = s.activity.start(kind, label, Cancel::Yes);
        task.progress(0, items.len() as u64);
        Ok(FolderMetadataJob {
            direction,
            items,
            next: 0,
            task,
            naming,
            revision: s.catalog.revision,
            cancelled: false,
            written: Vec::new(),
            merged: Vec::new(),
            backups: Vec::new(),
            reads: Vec::new(),
            warnings: Vec::new(),
            failed: Vec::new(),
        })
    }

    /// The photos the folder held when the job was planned.
    pub fn total(&self) -> usize {
        self.items.len()
    }

    /// The photos done so far (failed ones included).
    pub fn done(&self) -> usize {
        self.next
    }

    /// The row in the activity stack.
    pub fn task_id(&self) -> u64 {
        self.task.id()
    }

    /// Does the next photo. Returns whether there is more to do: `false` once every photo is done
    /// or the row's ✕ was pressed (checked before each photo, so the one in hand always finishes).
    pub fn step(&mut self, s: &mut Session) -> bool {
        if self.cancelled {
            return false;
        }
        if self.task.is_cancelled() {
            self.cancelled = true;
            return false;
        }
        // a file imported or relinked since may now share a stem with these photos: name from the catalog as it is
        if self.revision != s.catalog.revision {
            let ids: Vec<PhotoId> = self.items.iter().map(|i| i.id).collect();
            self.naming = match &self.naming {
                Naming::Save(_) => Naming::Save(StemOwners::among(&s.catalog, &ids)),
                Naming::Read(_) => Naming::Read(s.sidecar_namings(&ids)),
            };
            self.revision = s.catalog.revision;
        }
        let Some(item) = self.items.get(self.next) else { return false };
        let id = item.id;
        if let Some(name) = std::path::Path::new(&item.path).file_name() {
            self.task.detail(&name.to_string_lossy());
        }
        let in_place = s.catalog.photo(id).is_some_and(|p| !p.deleted && matches!(&p.source, Source::File { path } if *path == item.path));
        if !in_place {
            self.failed.push(json!({"id": id.0, "error": "the photo was deleted, moved or relinked while the folder was being done"}));
        } else {
            match &self.naming {
                Naming::Save(owners) => match s.save_sidecar_with(id, owners) {
                    Ok(r) => {
                        let path = r.path.display().to_string();
                        if r.merged {
                            self.merged.push(path.clone());
                        }
                        if let Some(b) = r.backup {
                            self.backups.push(json!({"path": path, "backup": b.display().to_string()}));
                        }
                        self.written.push(path);
                    }
                    Err(e) => self.failed.push(json!({"id": id.0, "error": e.to_string()})),
                },
                Naming::Read(namings) => {
                    let naming = namings.get(self.next).copied().unwrap_or(s.xmp.naming);
                    let photo = s.catalog.photo(id).cloned();
                    match (s.read_sidecar_op_named(id, naming), photo) {
                        (Ok(Some((op, from, mut warnings))), Some(photo)) => {
                            self.reads.push(Read { id, photo, op, from: from.display().to_string() });
                            self.warnings.append(&mut warnings);
                        }
                        (Ok(_), None) | (Ok(None), _) => self.failed.push(json!({"id": id.0, "error": "no XMP sidecar or embedded XMP"})),
                        (Err(e), _) => self.failed.push(json!({"id": id.0, "error": e.to_string()})),
                    }
                }
            }
        }
        self.next = self.next.saturating_add(1);
        self.task.progress(self.next as u64, self.items.len() as u64);
        self.next < self.items.len()
    }

    /// Ends the job: a read commits what it read as one undo step, except for a photo that was
    /// edited, deleted or moved since its file was read (it goes to `failed`). The result is
    /// `{total, done, cancelled, written, merged, backups, failed}` for a save and
    /// `{total, done, cancelled, read, failed, warnings?}` for a read.
    pub fn finish(mut self, s: &mut Session) -> Result<Value> {
        let (total, done, cancelled) = (self.items.len(), self.next, self.cancelled);
        let mut result = match self.direction {
            Direction::Save => json!({"written": self.written, "merged": self.merged, "backups": self.backups, "failed": self.failed}),
            Direction::Read => {
                let mut ops = Vec::new();
                let mut read = Vec::new();
                for r in std::mem::take(&mut self.reads) {
                    if s.catalog.photo(r.id).is_some_and(|now| Arc::ptr_eq(now, &r.photo)) {
                        ops.push(r.op);
                        read.push(json!({"id": r.id.0, "from": r.from}));
                    } else {
                        self.failed.push(json!({"id": r.id.0, "error": "the photo changed while the folder was being read (an edit, or an update made in the background): read it again"}));
                    }
                }
                if !ops.is_empty() {
                    s.commit("Read Metadata from Files", Op::Batch { ops })?;
                    // the sidecars just read are not written back (see the module docs)
                    s.skip_auto_write = true;
                }
                let mut r = json!({"read": read, "failed": self.failed});
                if !self.warnings.is_empty() {
                    r["warnings"] = serde_json::to_value(&self.warnings).unwrap_or_default();
                }
                r
            }
        };
        result["total"] = json!(total);
        result["done"] = json!(done);
        result["cancelled"] = json!(cancelled);
        Ok(result)
    }
}

/// `folder.saveMetadata` / `folder.readMetadata`: plan, then either hand the job to the session
/// (`background`) or do it all now.
fn run(s: &mut Session, p: &Value, direction: Direction) -> Result<Value> {
    let c = direction.command();
    let folder = str_param(p, "path").ok_or_else(|| bad(c, "missing `path`"))?;
    // a waiting job whose row was stopped has nobody left to end it when no frontend pumps: end it here
    if let Some(stopped) = s.folder_metadata.take_if(|j| j.task.is_cancelled()) {
        stopped.finish(s)?;
    }
    if s.folder_metadata.is_some() {
        return Err(bad(c, "a folder's metadata is already being saved or read: let it finish, or stop it in the activity stack"));
    }
    let mut job = FolderMetadataJob::plan(s, direction, folder)?;
    if bool_or(p, "background", false) {
        let total = job.total();
        s.folder_metadata = Some(job);
        return Ok(json!({"started": true, "total": total}));
    }
    while job.step(s) {}
    job.finish(s)
}

/// `folder.metadataPump`: does photos of the background job for about `budgetMs`, and ends it when
/// the last is done or its row was stopped.
fn pump(s: &mut Session, p: &Value) -> Result<Value> {
    let budget = Duration::from_millis(p.get("budgetMs").and_then(Value::as_u64).unwrap_or(10).clamp(0, 1000));
    let Some(mut job) = s.folder_metadata.take() else { return Ok(json!({"running": false})) };
    let began = web_time::Instant::now();
    // (the job is out of the session while it works, so a panic below drops it and its row)
    let more = loop {
        if !job.step(s) {
            break false;
        }
        if began.elapsed() >= budget {
            break true;
        }
    };
    if more {
        let status = json!({"running": true, "done": job.done(), "total": job.total()});
        s.folder_metadata = Some(job);
        return Ok(status);
    }
    Ok(json!({"running": false, "result": job.finish(s)?}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "folder.saveMetadata",
            "Save Metadata to Files",
            [],
            None,
            "{path, background?: false} — as photo.saveMetadataToFile for every library photo imported from the folder or a folder inside it (not Local-only browse records, Recently Deleted or virtual copies), with a row in the activity stack (`saveMetadata`); a photo whose file can't be written lands in `failed` and the rest go on. Plain, it does every photo before it returns → {total, done, cancelled, written: [path], merged: [path], backups: [{path, backup}], failed: [{id, error}]}. With `background: true` it only plans the work → {started, total}; each `folder.metadataPump` call (the caller repeats it; the app doesn't yet) then does it a few milliseconds at a time, and the row's ✕ or `activity.cancel` stops it (the sidecars already written stay)",
            always,
            |s, p| run(s, p, Direction::Save)
        ),
        cmd!(
            "folder.readMetadata",
            "Read Metadata from Files",
            [],
            None,
            "{path, background?: false} — as photo.readMetadataFromFile for every library photo imported from the folder or a folder inside it (not Local-only browse records, Recently Deleted or virtual copies), with a row in the activity stack (`readMetadata`): one undo step, never writes the sidecars it reads, even with auto-write on; a photo without a readable sidecar lands in `failed` and the rest go on, and so does one edited while a background read ran. Plain, it does every photo before it returns → {total, done, cancelled, read: [{id, from}], failed: [{id, error}], warnings?}. With `background: true` it only plans the work → {started, total}; `folder.metadataPump` then does it, and a stopped read commits what it had read",
            always,
            |s, p| run(s, p, Direction::Read)
        ),
        cmd!(
            query "folder.metadataPump",
            "Save or Read Folder Metadata in the Background",
            [],
            None,
            "{budgetMs?: 10} → {running: false} when no background job is waiting, {running: true, done, total} while one is, or {running: false, result} with the job's result (as the plain command's) when it ended — cheap enough to call every frame: does photos for about `budgetMs` (0–1000; at least one photo is done) and ends the job after the last, or at the next call after its row was stopped",
            always,
            pump
        ),
    ]
}
