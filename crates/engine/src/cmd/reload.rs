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
/// file-system calls.
pub fn plan(s: &mut Session, p: &Value) -> Result<Vec<Item>> {
    let ids = s.targets(p)?;
    s.reload_tickets.last = s.reload_tickets.last.saturating_add(1);
    let ticket = s.reload_tickets.last;
    Ok(ids
        .into_iter()
        .filter_map(|id| match s.catalog.photo(id).map(|ph| &ph.source) {
            Some(Source::File { path }) => Some(Item { id, path: path.clone(), ticket }),
            _ => None,
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
    use super::*;
    use lightcraft_catalog::Photo;

    #[test]
    fn file_changed_twice_quickly_superseded_older_reload_is_dropped() {
        let mut s = Session::new();
        let mut ph = Photo::new(PhotoId(1), Source::File { path: "/p/a.jpg".into() }, "a.jpg", "JPEG", 100, 100, "2026-01-01T10:00:00");
        ph.captured = Some("2026-01-01T10:00:00".into());
        ph.captured_manual = false;
        s.catalog.apply(Op::AddPhoto { photo: Box::new(ph) }).unwrap();

        // Two reloads are planned in rapid succession (e.g. file modified twice quickly)
        let items1 = plan(&mut s, &json!({"ids": [1]})).unwrap();
        let items2 = plan(&mut s, &json!({"ids": [1]})).unwrap();
        assert_eq!(items1[0].ticket, 1);
        assert_eq!(items2[0].ticket, 2);

        let info1 = ProbeInfo {
            width: 200,
            height: 200,
            file_size: 2000,
            format: "JPEG".into(),
            captured: Some("2026-01-01T11:00:00".into()),
            ..Default::default()
        };
        let info2 = ProbeInfo {
            width: 300,
            height: 300,
            file_size: 3000,
            format: "JPEG".into(),
            captured: Some("2026-01-01T12:00:00".into()),
            ..Default::default()
        };

        // Worker probe for ticket 2 completes and applies first:
        let reloaded2 = apply(&mut s, items2, vec![Ok(info2)]).unwrap();
        assert_eq!(reloaded2, vec![PhotoId(1)]);
        let p = s.catalog.photo(PhotoId(1)).unwrap();
        assert_eq!(p.width, 300);
        assert_eq!(p.captured.as_deref(), Some("2026-01-01T12:00:00"));

        // Worker probe for older ticket 1 completes later and attempts to apply:
        let reloaded1 = apply(&mut s, items1, vec![Ok(info1)]).unwrap();
        assert!(reloaded1.is_empty(), "older superseded reload must be dropped");

        // The photo retains the latest reload (ticket 2)
        let p = s.catalog.photo(PhotoId(1)).unwrap();
        assert_eq!(p.width, 300);
        assert_eq!(p.captured.as_deref(), Some("2026-01-01T12:00:00"));
    }

    #[test]
    fn file_changed_twice_quickly_sequential_reloads_apply_latest_state() {
        let mut s = Session::new();
        let mut ph = Photo::new(PhotoId(1), Source::File { path: "/p/a.jpg".into() }, "a.jpg", "JPEG", 100, 100, "2026-01-01T10:00:00");
        ph.captured = Some("2026-01-01T10:00:00".into());
        ph.captured_manual = false;
        s.catalog.apply(Op::AddPhoto { photo: Box::new(ph) }).unwrap();

        // Change 1
        let items1 = plan(&mut s, &json!({"ids": [1]})).unwrap();
        let info1 = ProbeInfo {
            width: 200,
            height: 200,
            file_size: 2000,
            format: "JPEG".into(),
            captured: Some("2026-01-01T11:00:00".into()),
            ..Default::default()
        };
        let reloaded1 = apply(&mut s, items1, vec![Ok(info1)]).unwrap();
        assert_eq!(reloaded1, vec![PhotoId(1)]);
        assert_eq!(s.catalog.photo(PhotoId(1)).unwrap().captured.as_deref(), Some("2026-01-01T11:00:00"));

        // Change 2 immediately after
        let items2 = plan(&mut s, &json!({"ids": [1]})).unwrap();
        let info2 = ProbeInfo {
            width: 300,
            height: 300,
            file_size: 3000,
            format: "JPEG".into(),
            captured: Some("2026-01-01T12:00:00".into()),
            ..Default::default()
        };
        let reloaded2 = apply(&mut s, items2, vec![Ok(info2)]).unwrap();
        assert_eq!(reloaded2, vec![PhotoId(1)]);
        assert_eq!(s.catalog.photo(PhotoId(1)).unwrap().captured.as_deref(), Some("2026-01-01T12:00:00"));
    }

    #[test]
    fn reload_with_manual_capture_time_is_kept_and_without_is_updated() {
        let mut s = Session::new();
        // Photo 1: manual capture time edited by user
        let mut p1 = Photo::new(PhotoId(1), Source::File { path: "/p/1.jpg".into() }, "1.jpg", "JPEG", 100, 100, "2026-01-01T10:00:00");
        p1.captured = Some("2024-05-01T12:00:00".into());
        p1.captured_manual = true;
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p1) }).unwrap();

        // Photo 2: file-derived capture time
        let mut p2 = Photo::new(PhotoId(2), Source::File { path: "/p/2.jpg".into() }, "2.jpg", "JPEG", 100, 100, "2026-01-01T10:00:00");
        p2.captured = Some("2026-01-01T10:00:00".into());
        p2.captured_manual = false;
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p2) }).unwrap();

        let items = plan(&mut s, &json!({"ids": [1, 2]})).unwrap();
        let new_probe = ProbeInfo {
            width: 150,
            height: 150,
            file_size: 1500,
            format: "JPEG".into(),
            captured: Some("2026-10-20T17:05:32".into()),
            ..Default::default()
        };

        apply(&mut s, items, vec![Ok(new_probe.clone()), Ok(new_probe)]).unwrap();

        // Photo 1 kept its manual capture time:
        assert_eq!(s.catalog.photo(PhotoId(1)).unwrap().captured.as_deref(), Some("2024-05-01T12:00:00"));
        assert!(s.catalog.photo(PhotoId(1)).unwrap().captured_manual);

        // Photo 2 updated to file's probe capture time:
        assert_eq!(s.catalog.photo(PhotoId(2)).unwrap().captured.as_deref(), Some("2026-10-20T17:05:32"));
        assert!(!s.catalog.photo(PhotoId(2)).unwrap().captured_manual);
    }
}
