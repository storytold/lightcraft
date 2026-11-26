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
    /// Same-file family members, including implicit fallback targets.
    pub(crate) source_targets: Vec<PhotoId>,
    family: PhotoId,
    /// The [`plan`] it came from, in the order reloads were started (see [`Tickets`]).
    pub ticket: u64,
}

impl Item {
    /// Find a still-current representative without recording the read before its batch commits.
    pub(crate) fn current_representative(&mut self, s: &Session) -> Option<PhotoId> {
        self.source_targets.retain(|id| {
            s.reload_tickets.valid(*id, self.ticket)
                && s.catalog.photo(*id).is_some_and(|p| {
                    !p.deleted && p.copy_of.unwrap_or(p.id) == self.family && matches!(&p.source, Source::File { path } if *path == self.path)
                })
        });
        let id = *self.source_targets.first()?;
        s.reload_tickets.applied.get(&(self.family, self.path.clone())).is_none_or(|latest| *latest <= self.ticket).then_some(id)
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

    /// A new virtual copy shares the source's facts and their freshness.
    pub(crate) fn inherit_copy(&mut self, copy: PhotoId, source: PhotoId) {
        if let Some(cutoff) = self.invalidated.get(&source).copied() {
            self.invalidated.insert(copy, cutoff);
        } else {
            self.invalidated.remove(&copy);
        }
    }

    /// Keep changed file facts without cancelling an untouched sibling's pending read.
    pub(crate) fn valid(&self, id: PhotoId, ticket: u64) -> bool {
        self.invalidated.get(&id).is_none_or(|cutoff| *cutoff < ticket)
    }
}

/// Supersede earlier reads after a successful batch, including a successful read with no
/// changes. A partial rollback can leave new facts behind, so preserve their ordering too.
pub(crate) fn finish_reads(tickets: &mut Tickets, accepted: Vec<Item>, result: Result<()>) -> Result<()> {
    if result.is_ok() || matches!(result, Err(crate::EngineError::Catalog(lightcraft_catalog::CatalogError::PartlyApplied { .. }))) {
        for item in accepted {
            let latest = tickets.applied.entry((item.family, item.path)).or_default();
            *latest = (*latest).max(item.ticket);
        }
    }
    result
}

/// The files `p` targets (`ids`, else the selection), grouped by family and path under a new
/// ticket. No file-system calls.
pub fn plan(s: &mut Session, p: &Value) -> Result<Vec<Item>> {
    let ids = s.targets(p)?;
    Ok(plan_ids(s, ids))
}

/// Group explicit targets for Reload and automatic rechecks.
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
    items
}

/// Index same-file siblings in one pass at completion, including copies created during the probe.
pub(crate) fn include_same_file_copies<'a>(catalog: &lightcraft_catalog::Catalog, items: impl Iterator<Item = &'a mut Item>) {
    let mut groups: std::collections::HashMap<_, _> =
        items.map(|item| ((item.family, item.path.as_str()), (item.id, &mut item.source_targets))).collect();
    if groups.is_empty() {
        return;
    }
    for ph in catalog.photos().filter(|p| !p.deleted) {
        if let Source::File { path } = &ph.source
            && let Some((id, targets)) = groups.get_mut(&(ph.copy_of.unwrap_or(ph.id), path.as_str()))
            && ph.id != *id
        {
            targets.push(ph.id);
        }
    }
}

/// The session's file probe, to [`probe`] with on a worker thread (`None` without file access).
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
    let Some(probe) = probe else {
        progress(items.len());
        return items.iter().map(|item| Err(format!("{}: this session has no file access", item.path))).collect();
    };
    let paths: Vec<String> = items.iter().map(|i| i.path.clone()).collect();
    let done = AtomicUsize::new(0);
    #[cfg(not(target_arch = "wasm32"))]
    let r = std::thread::scope(|sc| {
        let h = sc.spawn(|| crate::import::probe_all(Some(probe), &paths, &done, cancel));
        while !h.is_finished() {
            progress(done.load(Ordering::Relaxed));
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        // a probe that panicked (already logged): nothing is known about any file
        h.join().unwrap_or_else(|_| paths.iter().map(|p| Err(format!("{p}: could not be read"))).collect())
    });
    #[cfg(target_arch = "wasm32")]
    let r = crate::import::probe_all(Some(probe), &paths, &done, cancel);
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
    let mut results: Vec<_> = items.into_iter().zip(results).filter_map(|(item, info)| info.ok().map(|info| (item, info))).collect();
    include_same_file_copies(&s.catalog, results.iter_mut().map(|(item, _)| item));
    let mut accepted = Vec::new();
    for (mut item, info) in results {
        let Some(id) = item.current_representative(s) else { continue };
        let selected: HashSet<_> = std::mem::take(&mut item.metadata_targets).into_iter().collect();
        crate::cmd::convert::reload_ops(&s.catalog, id, &info, &mut ops, &mut out.reloaded, item.source_targets.iter().copied(), |id| {
            selected.contains(&id)
        });
        out.checked.extend(item.source_targets.iter().copied());
        accepted.push(item);
    }
    let result = if ops.is_empty() { Ok(()) } else { s.commit_with_read_invalidation("Reload Changed Files", Op::Batch { ops }, false) };
    finish_reads(&mut s.reload_tickets, accepted, result)?;
    Ok(out)
}
