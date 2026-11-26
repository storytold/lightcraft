//! Reload from Disk (`photo.reload`), split so the app can read the files on a worker thread: a
//! raw re-decode takes a second or more per file, and a selection of them froze the window while
//! the command ran. [`plan`] lists the photos to re-read (no file-system calls), [`probe`] reads
//! their files without the session (cancellable, with progress), and [`apply`] commits what came
//! of it as one undo step. The command itself (agents, CLI, MCP) does all three at once.
//!
//! Reloads can overlap (the user picks Reload from Disk again while a first one still reads), so
//! each [`plan`] takes a ticket and [`apply`] drops a photo's result when a reload planned later
//! has already been applied to it: an older read never replaces a newer one.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use lightcraft_catalog::{Op, PhotoId, Source};
use serde_json::{Value, json};

use crate::media::{FileProbe, ProbeInfo};
use crate::{Result, Session};

/// One family’s file to re-read, retaining every explicitly selected metadata target.
/// [`apply`] skips targets removed or relinked meanwhile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub id: PhotoId,
    pub path: String,
    /// Explicitly selected photos sharing this probe; each may gain missing metadata.
    pub metadata_targets: Vec<PhotoId>,
    /// Same-file family members present at planning time, including implicit fallback targets.
    pub(crate) source_targets: Vec<PhotoId>,
    family: PhotoId,
    /// The [`plan`] it came from, in the order reloads were started (see [`Tickets`]).
    pub ticket: u64,
}

impl Item {
    /// Accept a successful read for the still-current members, returning its representative.
    pub(crate) fn accept(&mut self, s: &mut Session) -> Option<PhotoId> {
        self.source_targets.retain(|id| {
            s.reload_tickets.valid(*id, self.ticket)
                && s.catalog.photo(*id).is_some_and(|p| {
                    !p.deleted && p.copy_of.unwrap_or(p.id) == self.family && matches!(&p.source, Source::File { path } if *path == self.path)
                })
        });
        let id = *self.source_targets.first()?;
        s.reload_tickets.accept(self.family, self.path.clone(), self.ticket).then_some(id)
    }
}

/// Which Reload or recheck each same-file family last took its file information from, so an older read
/// cannot replace a newer one. A relink must not carry freshness to another file (session memory).
#[derive(Debug, Default)]
pub struct Tickets {
    last: u64,
    applied: std::collections::BTreeMap<(PhotoId, String), u64>,
    invalidated: std::collections::BTreeMap<PhotoId, u64>,
}

impl Tickets {
    /// Planning order shared by Reload and automatic raw rechecks.
    pub(crate) fn next(&mut self) -> u64 {
        self.last = self.last.saturating_add(1);
        self.last
    }

    /// A catalog source change supersedes reads already planned for this photo.
    pub(crate) fn invalidate(&mut self, id: PhotoId) {
        self.invalidated.insert(id, self.last);
    }

    /// Keep changed file facts without cancelling an untouched sibling's pending read.
    pub(crate) fn valid(&self, id: PhotoId, ticket: u64) -> bool {
        self.invalidated.get(&id).is_none_or(|cutoff| *cutoff < ticket)
    }

    /// Only a newer completed read of this family's file supersedes its result.
    pub(crate) fn accept(&mut self, family: PhotoId, path: String, ticket: u64) -> bool {
        let key = (family, path);
        if self.applied.get(&key).is_some_and(|latest| *latest > ticket) {
            return false;
        }
        self.applied.insert(key, ticket);
        true
    }
}

/// The files `p` targets (`ids`, else the selection), grouped by family and path under a new
/// ticket. No file-system calls.
pub fn plan(s: &mut Session, p: &Value) -> Result<Vec<Item>> {
    let ids = s.targets(p)?;
    Ok(plan_ids(s, ids))
}

/// Group explicit targets and index their same-file siblings once, also for automatic rechecks.
pub(crate) fn plan_ids(s: &mut Session, ids: Vec<PhotoId>) -> Vec<Item> {
    let mut groups = std::collections::HashMap::<(PhotoId, &str), usize>::new();
    let mut items: Vec<Item> = Vec::new();
    let mut seen = HashSet::new();
    let ticket = s.reload_tickets.next();
    for id in ids.into_iter().filter(|id| seen.insert(*id)) {
        let Some(ph) = s.catalog.photo(id) else { continue };
        if ph.deleted {
            continue;
        }
        let Source::File { path } = &ph.source else { continue };
        let family = ph.copy_of.unwrap_or(ph.id);
        let key = (family, path.as_str());
        // Sharing a probe must not drop any explicitly selected metadata target.
        if let Some(item) = groups.get(&key).and_then(|index| items.get_mut(*index)) {
            item.metadata_targets.push(id);
        } else {
            groups.insert(key, items.len());
            items.push(Item { id, path: path.clone(), metadata_targets: vec![id], source_targets: vec![id], family, ticket });
        }
    }
    if groups.is_empty() {
        return items;
    }
    for ph in s.catalog.photos().filter(|p| !p.deleted) {
        if let Source::File { path } = &ph.source
            && let Some(index) = groups.get(&(ph.copy_of.unwrap_or(ph.id), path.as_str()))
            && let Some(item) = items.get_mut(*index)
            && ph.id != item.id
        {
            item.source_targets.push(ph.id);
        }
    }
    items
}

/// The session's file probe, to [`probe`] with on a worker thread (`None` in a session without file
/// access, where a reload reads nothing but the file name, as the command does).
pub fn file_probe(s: &Session) -> Option<FileProbe> {
    s.media.file_probe.clone()
}

/// Read each listed photo's file as Reload does, several at once. Doesn't touch the session.
/// `progress(done)` is called while it runs (from this thread); once `cancel` is set no further
/// file is started, and the files not reached come back as errors (left unchanged by [`apply`]).
pub fn probe(
    probe: Option<&FileProbe>,
    items: &[Item],
    cancel: &AtomicBool,
    progress: &dyn Fn(usize),
) -> Vec<std::result::Result<ProbeInfo, String>> {
    let paths: Vec<String> = items.iter().map(|i| i.path.clone()).collect();
    let done = AtomicUsize::new(0);
    #[cfg(not(target_arch = "wasm32"))]
    let r = std::thread::scope(|sc| {
        let h = sc.spawn(|| crate::import::probe_all(probe, &paths, &done, cancel));
        while !h.is_finished() {
            progress(done.load(Ordering::Relaxed));
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        // a probe that panicked (already logged): nothing is known about any file
        h.join().unwrap_or_else(|_| paths.iter().map(|p| Err(format!("{p}: could not be read"))).collect())
    });
    #[cfg(target_arch = "wasm32")]
    let r = crate::import::probe_all(probe, &paths, &done, cancel);
    progress(done.load(Ordering::Relaxed));
    r
}

/// What Reload checked successfully and what it changed.
#[derive(Debug, Default)]
pub struct Reloaded {
    pub reloaded: Vec<PhotoId>,
    /// Current targets whose probes succeeded, including photos whose facts were unchanged.
    pub checked: Vec<PhotoId>,
}

impl Reloaded {
    /// `{reloaded: [ids]}`; accepted probes are internal notification bookkeeping.
    pub fn to_json(&self) -> Value {
        json!({"reloaded": self.reloaded.iter().map(|i| i.0).collect::<Vec<_>>()})
    }
}

/// Commit [`probe`]'s results as one undo step ("Reload Changed Files"): each selected photo and
/// its same-file family get what their file is now. A file that couldn't be read changes nothing; a photo removed
/// or relinked since it was listed, or already given a newer reload's result, is skipped. Returns
/// both the accepted probes and the photos that changed.
pub fn apply(s: &mut Session, items: Vec<Item>, results: Vec<std::result::Result<ProbeInfo, String>>) -> Result<Reloaded> {
    let mut ops = Vec::new();
    let mut out = Reloaded::default();
    for (mut item, info) in items.into_iter().zip(results) {
        let Ok(info) = info else { continue };
        let Some(id) = item.accept(s) else { continue };
        let selected: HashSet<_> = item.metadata_targets.into_iter().collect();
        crate::cmd::convert::reload_ops(&s.catalog, id, &info, &mut ops, &mut out.reloaded, item.source_targets.iter().copied(), |id| {
            selected.contains(&id)
        });
        out.checked.extend(item.source_targets);
    }
    if !ops.is_empty() {
        s.commit_with_read_invalidation("Reload Changed Files", Op::Batch { ops }, false)?;
    }
    Ok(out)
}
