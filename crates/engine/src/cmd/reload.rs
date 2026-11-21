//! Reload from Disk (`photo.reload`), split so the app can read the files on a worker thread: a
//! raw re-decode takes a second or more per file, and a selection of them froze the window while
//! the command ran. [`plan`] lists the photos to re-read (no file-system calls), [`probe`] reads
//! their files without the session (cancellable, with progress), and [`apply`] commits what came
//! of it as one undo step. The command itself (agents, CLI, MCP) does all three at once.
//!
//! Reloads can overlap (the user picks Reload from Disk again while a first one still reads), so
//! each [`plan`] takes a ticket and [`apply`] drops a photo's result when a reload planned later
//! has already been applied to it: an older read never replaces a newer one.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use lightcraft_catalog::{Op, PhotoId, Source};
use serde_json::{Value, json};

use crate::media::{FileProbe, ProbeInfo};
use crate::{Result, Session};

/// A photo to re-read: its id and the file it had when it was listed (a photo relinked meanwhile
/// is skipped by [`apply`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub id: PhotoId,
    pub path: String,
    /// The [`plan`] it came from, in the order reloads were started (see [`Tickets`]).
    pub ticket: u64,
}

/// Which reload each photo last took its file information from, so a reload started earlier but
/// finishing later doesn't put back what its file was before (session memory, not saved).
#[derive(Debug, Default)]
pub struct Tickets {
    last: u64,
    applied: std::collections::BTreeMap<PhotoId, u64>,
}

/// The photos `p` targets (`ids`, else the selection) that have a file, under a new ticket. No
/// file-system calls. A virtual copy is listed as the photo it was copied from when the two share
/// the file: the file is re-read once, and [`apply`] brings that photo and all its copies up to
/// date, so none keeps what the file was before.
pub fn plan(s: &mut Session, p: &Value) -> Result<Vec<Item>> {
    let ids = s.targets(p)?;
    s.reload_tickets.last = s.reload_tickets.last.saturating_add(1);
    let ticket = s.reload_tickets.last;
    let mut seen = std::collections::HashSet::new();
    Ok(ids
        .into_iter()
        .filter_map(|id| {
            let ph = s.catalog.photo(id)?;
            let Source::File { path } = &ph.source else { return None };
            let master = ph.copy_of.filter(|m| s.catalog.photo(*m).is_some_and(|mp| matches!(&mp.source, Source::File { path: q } if q == path)));
            let id = master.unwrap_or(id);
            seen.insert(id).then(|| Item { id, path: path.clone(), ticket })
        })
        .collect())
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

/// Commit [`probe`]'s results as one undo step ("Reload Changed Files"): each photo and its virtual
/// copies get what their file is now. A file that couldn't be read changes nothing; a photo removed
/// or relinked since it was listed, or already given a newer reload's result, is skipped. Returns
/// the photos that changed.
pub fn apply(s: &mut Session, items: Vec<Item>, results: Vec<std::result::Result<ProbeInfo, String>>) -> Result<Vec<PhotoId>> {
    let mut ops = Vec::new();
    let mut reloaded = Vec::new();
    for (item, info) in items.into_iter().zip(results) {
        let Ok(info) = info else { continue };
        let current = s.catalog.photo(item.id).is_some_and(|ph| matches!(&ph.source, Source::File { path } if *path == item.path));
        let superseded = s.reload_tickets.applied.get(&item.id).is_some_and(|t| *t > item.ticket);
        if !current || superseded {
            continue;
        }
        s.reload_tickets.applied.insert(item.id, item.ticket);
        crate::cmd::convert::reload_ops(&s.catalog, item.id, &info, &mut ops, &mut reloaded);
    }
    for id in &reloaded {
        s.media.forget(*id);
    }
    if !ops.is_empty() {
        s.commit("Reload Changed Files", Op::Batch { ops })?;
    }
    Ok(reloaded)
}

/// `{reloaded: [ids]}`.
pub fn to_json(reloaded: &[PhotoId]) -> Value {
    json!({"reloaded": reloaded.iter().map(|i| i.0).collect::<Vec<_>>()})
}

#[cfg(test)]
mod tests {
    use lightcraft_catalog::{Op, Photo, PhotoId, Source};
    use serde_json::json;

    use super::plan;
    use crate::Session;

    /// A session holding `photos` as `(id, path, copy_of)`; `path: None` is a photo with no file.
    fn session(photos: &[(u64, Option<&str>, Option<u64>)]) -> Session {
        let mut s = Session::new();
        for (id, path, copy_of) in photos {
            let source = match path {
                Some(p) => Source::File { path: (*p).into() },
                None => Source::Demo { scene: 1 },
            };
            let mut ph = Photo::new(PhotoId(*id), source, "x.jpg", "JPEG", 60, 40, "2026-10-06T00:00:00");
            ph.copy_of = copy_of.map(PhotoId);
            s.catalog.apply(Op::AddPhoto { photo: Box::new(ph) }).unwrap();
        }
        s
    }

    fn planned(s: &mut Session, ids: &[u64]) -> Vec<(u64, String)> {
        plan(s, &json!({"ids": ids})).unwrap().into_iter().map(|i| (i.id.0, i.path)).collect()
    }

    /// A virtual copy that shares its master's file is re-read as the master, once, even when both
    /// (or only the copy) are asked for.
    #[test]
    fn a_copy_sharing_the_file_is_planned_as_its_master_once() {
        let mut s = session(&[(1, Some("/a.jpg"), None), (2, Some("/a.jpg"), Some(1)), (3, Some("/a.jpg"), Some(1))]);
        assert_eq!(planned(&mut s, &[1, 2, 3]), vec![(1, "/a.jpg".to_string())]);
        assert_eq!(planned(&mut s, &[2]), vec![(1, "/a.jpg".to_string())], "the copy alone reloads through its master");
        assert_eq!(planned(&mut s, &[3, 2]), vec![(1, "/a.jpg".to_string())]);
    }

    /// A copy linked to a file of its own is re-read as itself, beside its master.
    #[test]
    fn a_copy_with_its_own_file_is_planned_as_itself() {
        let mut s = session(&[(1, Some("/a.jpg"), None), (2, Some("/b.jpg"), Some(1))]);
        assert_eq!(planned(&mut s, &[1, 2]), vec![(1, "/a.jpg".to_string()), (2, "/b.jpg".to_string())]);
        assert_eq!(planned(&mut s, &[2]), vec![(2, "/b.jpg".to_string())]);
    }

    /// A copy whose master is gone from the library, or has no file, is re-read as itself.
    #[test]
    fn a_copy_without_a_master_on_file_is_planned_as_itself() {
        let mut s = session(&[(2, Some("/a.jpg"), Some(1)), (3, None, None), (4, Some("/c.jpg"), Some(3))]);
        assert_eq!(planned(&mut s, &[2]), vec![(2, "/a.jpg".to_string())], "master removed");
        assert_eq!(planned(&mut s, &[4]), vec![(4, "/c.jpg".to_string())], "master has no file");
        assert!(planned(&mut s, &[3, 99]).is_empty(), "no file, no photo: nothing to read");
    }
}
