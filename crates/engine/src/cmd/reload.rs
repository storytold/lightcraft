//! Reload from Disk (`photo.reload`), split so the app can read the files on a worker thread: a
//! raw re-decode takes a second or more per file, and a selection of them froze the window while
//! the command ran. [`plan`] lists the photos to re-read (no file-system calls), [`probe`] reads
//! their files without the session (cancellable, with progress), and [`apply`] commits what came
//! of it as one undo step. The command itself (agents, CLI, MCP) does all three at once.

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
}

/// The photos `p` targets (`ids`, else the selection) that have a file. No file-system calls.
pub fn plan(s: &Session, p: &Value) -> Result<Vec<Item>> {
    Ok(s.targets(p)?
        .into_iter()
        .filter_map(|id| match s.catalog.photo(id).map(|ph| &ph.source) {
            Some(Source::File { path }) => Some(Item { id, path: path.clone() }),
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
/// or relinked since it was listed is skipped. Returns the photos that changed.
pub fn apply(s: &mut Session, items: Vec<Item>, results: Vec<std::result::Result<ProbeInfo, String>>) -> Result<Vec<PhotoId>> {
    let mut ops = Vec::new();
    let mut reloaded = Vec::new();
    for (item, info) in items.into_iter().zip(results) {
        let Ok(info) = info else { continue };
        let current = s.catalog.photo(item.id).is_some_and(|ph| matches!(&ph.source, Source::File { path } if *path == item.path));
        if !current {
            continue;
        }
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
