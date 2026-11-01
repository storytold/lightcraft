//! Preview-only raws rechecked after an update (issue #662): a raw imported while its format or
//! variant couldn't be decoded keeps [`Photo::preview_only`] until something re-reads its file.
//! Each flag is stamped with the app version that set it ([`Photo::preview_checked`]); when a
//! library opens in another version, its stale preview-only photos are probed again and get
//! exactly what Reload from Disk would give them: a file that decodes now loses the flag, one that
//! still doesn't gets the new reason and a fresh stamp, so it isn't probed again.
//!
//! The work is split so the app can probe on a worker thread: [`stale`] lists what to probe (no
//! file-system calls), [`probe`] reads the files without the session, and [`apply`] applies the
//! results as LightCraft's own bookkeeping (journaled, not an undo step). Agents and tests use
//! `library.recheckPreviewOnly`, which does all three at once.

use lightcraft_catalog::{Catalog, Op, Photo, PhotoId};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd};
use crate::media::{FileProbe, ProbeInfo};
use crate::{Result, Session};

/// A preview-only photo to probe, and what the library knew about it when it was listed (so a
/// result for a photo that changed meanwhile is dropped).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stale {
    pub id: PhotoId,
    pub path: String,
    preview_only: Option<String>,
    preview_checked: String,
}

impl Stale {
    fn of(p: &Photo, path: &str) -> Stale {
        Stale { id: p.id, path: path.to_string(), preview_only: p.preview_only.clone(), preview_checked: p.preview_checked.clone() }
    }

    /// The photo is still as it was listed: same file, flag and stamp.
    fn current(&self, p: &Photo) -> bool {
        crate::cmd::missing::checked_path(p) == Some(self.path.as_str())
            && p.preview_only == self.preview_only
            && p.preview_checked == self.preview_checked
    }
}

/// The library's preview-only photos whose flag another app version set (or every preview-only
/// photo, with `all`): library photos with a file, not virtual copies (they follow the photo they
/// were copied from). No file-system calls.
pub fn stale(cat: &Catalog, all: bool) -> Vec<Stale> {
    cat.photos()
        .filter(|p| p.copy_of.is_none() && p.preview_only.is_some() && (all || p.preview_only_stale()))
        .filter_map(|p| crate::cmd::missing::checked_path(p).map(|path| Stale::of(p, path)))
        .collect()
}

/// The session's file probe, to [`probe`] with on a worker thread; `None` when the session has no
/// file access (a demo or browser session), where nothing can be rechecked.
pub fn file_probe(s: &Session) -> Option<FileProbe> {
    s.media.file_probe.clone()
}

/// Probe each listed photo's file as Reload would. Doesn't touch the session.
pub fn probe(probe: &FileProbe, stale: Vec<Stale>) -> Vec<(Stale, std::result::Result<ProbeInfo, String>)> {
    let paths: Vec<String> = stale.iter().map(|x| x.path.clone()).collect();
    stale.into_iter().zip(crate::import::probe_with(probe, &paths)).collect()
}

/// Probe each listed file, reporting completed files and stopping before starting more work after
/// `cancel` is set. Files not reached retain an error result, which [`apply`] leaves unchanged.
/// Cancellation is cooperative between files: a file probe already in progress may finish its
/// read before the worker observes cancellation.
pub fn probe_cancellable_with_progress(
    probe: &FileProbe,
    stale: Vec<Stale>,
    cancel: &std::sync::atomic::AtomicBool,
    progress: &dyn Fn(usize),
) -> Vec<(Stale, std::result::Result<ProbeInfo, String>)> {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let paths: Vec<String> = stale.iter().map(|item| item.path.clone()).collect();
    let done = AtomicUsize::new(0);
    #[cfg(not(target_arch = "wasm32"))]
    let probed = std::thread::scope(|scope| {
        let worker = scope.spawn(|| crate::import::probe_all(Some(probe), &paths, &done, cancel));
        while !worker.is_finished() {
            progress(done.load(Ordering::Relaxed));
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        worker.join().unwrap_or_else(|_| paths.iter().map(|path| Err(format!("{path}: could not be read"))).collect())
    });
    #[cfg(target_arch = "wasm32")]
    let probed = crate::import::probe_all(Some(probe), &paths, &done, cancel);
    progress(done.load(Ordering::Relaxed));
    stale.into_iter().zip(probed).collect()
}

/// What a recheck changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rechecked {
    /// Photos brought up to date (virtual copies included).
    pub updated: Vec<PhotoId>,
    /// Of those, the ones that develop from their raw data now.
    pub cleared: Vec<PhotoId>,
}

impl Rechecked {
    pub fn to_json(&self) -> Value {
        json!({"updated": self.updated.iter().map(|i| i.0).collect::<Vec<_>>(), "cleared": self.cleared.iter().map(|i| i.0).collect::<Vec<_>>()})
    }
}

/// Apply [`probe`]'s results: each photo gets the ops Reload would give it, all in one batch that is
/// journaled but isn't an undo step. A file that can't be read (missing, offline) changes nothing,
/// so it is tried again next time; a photo that changed since it was listed (relinked, reloaded,
/// removed) is skipped.
pub fn apply(s: &mut Session, results: Vec<(Stale, std::result::Result<ProbeInfo, String>)>) -> Result<Rechecked> {
    let mut ops = Vec::new();
    let mut out = Rechecked::default();
    for (item, info) in results {
        let Ok(info) = info else { continue };
        if !s.catalog.photo(item.id).is_some_and(|p| item.current(p)) {
            continue;
        }
        let first = out.updated.len();
        crate::cmd::convert::reload_ops(&s.catalog, item.id, &info, &mut ops, &mut out.updated);
        if info.preview_only.is_none() {
            out.cleared.extend_from_slice(&out.updated[first..]);
        }
    }
    if !ops.is_empty() {
        s.apply_system(Op::Batch { ops })?;
    }
    for id in &out.updated {
        s.media.forget(*id);
    }
    Ok(out)
}

fn recheck(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "library.recheckPreviewOnly";
    let all = p.get("all").and_then(Value::as_bool).unwrap_or(false);
    // without file access every probe "succeeds" with nothing known, which would clear every flag
    let fp = file_probe(s).ok_or_else(|| bad(C, "this session has no file access"))?;
    let listed = stale(&s.catalog, all);
    let n = listed.len();
    let mut r = apply(s, probe(&fp, listed))?.to_json();
    r["checked"] = json!(n);
    Ok(r)
}

pub fn specs() -> Vec<CommandSpec> {
    vec![cmd!(
        "library.recheckPreviewOnly",
        "Recheck Preview-Only Raws",
        [],
        None,
        "{all?: bool} — probe again the raws shown from their embedded preview whose flag another LightCraft version set (the app does this in the background when a library opens; `all` rechecks every preview-only raw, e.g. when testing a decoder build of the same version), and bring each up to date as Reload from Disk would; not an undo step, develop settings and history are kept; files that can't be read are left for next time → {checked, updated: [ids], cleared: [ids] (develop from their raw data now)}",
        always,
        recheck
    )]
}

#[cfg(test)]
mod cancellation_tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Condvar, Mutex};

    use super::{Stale, probe_cancellable_with_progress};
    use crate::media::{FileProbe, ProbeInfo};
    use lightcraft_catalog::PhotoId;

    #[test]
    fn pre_cancelled_recheck_does_not_read_files_or_advance_progress() {
        let calls = Arc::new(AtomicUsize::new(0));
        let probe_calls = calls.clone();
        let probe: FileProbe = Arc::new(move |_| {
            probe_calls.fetch_add(1, Ordering::Relaxed);
            Ok(ProbeInfo::default())
        });
        let stale = vec![Stale { id: PhotoId(1), path: "photo.raw".into(), preview_only: Some("preview".into()), preview_checked: "0.5.0".into() }];
        let cancel = AtomicBool::new(true);
        let progress = Mutex::new(Vec::new());

        let results = probe_cancellable_with_progress(&probe, stale, &cancel, &|done| {
            progress.lock().unwrap_or_else(|e| e.into_inner()).push(done);
        });

        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert_eq!(results.len(), 1);
        assert!(results[0].1.is_err(), "a pre-cancelled file has no probe result");
        assert_eq!(progress.lock().unwrap_or_else(|e| e.into_inner()).last(), Some(&0));
    }

    #[test]
    fn cancelled_recheck_probe_does_not_start_remaining_files_and_reports_progress() {
        const FILES: usize = 32;
        let calls = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let probe_calls = calls.clone();
        let probe_gate = gate.clone();
        let probe: FileProbe = Arc::new(move |_| {
            probe_calls.fetch_add(1, Ordering::Relaxed);
            let mut released = probe_gate.0.lock().unwrap_or_else(|e| e.into_inner());
            probe_gate.1.notify_all();
            while !*released {
                released = probe_gate.1.wait(released).unwrap_or_else(|e| e.into_inner());
            }
            Ok(ProbeInfo::default())
        });
        let stale: Vec<_> = (0..FILES)
            .map(|i| Stale {
                id: PhotoId(i as u64),
                path: format!("photo-{i}.raw"),
                preview_only: Some("preview".into()),
                preview_checked: "0.5.0".into(),
            })
            .collect();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let progress = Arc::new(Mutex::new(Vec::new()));
        let worker_progress = progress.clone();

        let worker = std::thread::spawn(move || {
            probe_cancellable_with_progress(&probe, stale, &worker_cancel, &|done| {
                worker_progress.lock().unwrap_or_else(|e| e.into_inner()).push(done);
            })
        });
        let (entered, _) = gate
            .1
            .wait_timeout_while(gate.0.lock().unwrap_or_else(|e| e.into_inner()), std::time::Duration::from_secs(5), |_| {
                calls.load(Ordering::Relaxed) == 0
            })
            .unwrap_or_else(|e| e.into_inner());
        let started = calls.load(Ordering::Relaxed);
        cancel.store(true, Ordering::Relaxed);
        drop(entered);
        *gate.0.lock().unwrap_or_else(|e| e.into_inner()) = true;
        gate.1.notify_all();

        let results = worker.join().unwrap_or_else(|_| panic!("the probe worker should return results"));
        let completed = results.iter().filter(|(_, result)| result.is_ok()).count();
        assert!(started > 0, "the test waited for at least one active file");
        assert_eq!(calls.load(Ordering::Relaxed), completed, "all started probes complete");
        assert!(completed < FILES, "cancel leaves later files unstarted: {completed}");
        assert_eq!(results.len(), FILES);
        assert!(results.iter().any(|(_, result)| result.as_ref().is_err_and(|e| e == "not probed")));
        assert_eq!(progress.lock().unwrap_or_else(|e| e.into_inner()).last(), Some(&completed));
    }
}
