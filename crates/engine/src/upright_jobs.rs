//! Upright is worked out on each photo, as Lightroom does: an automatic Upright mode (Auto, Level,
//! Vertical, Full) that reaches a photo without its own correction (Auto Sync, Sync Settings,
//! Paste, a preset) is a recipe, and that photo's own lines decide the correction. Copying the
//! active photo's correction onto the others tilted every photo by the first one's lean.
//!
//! After every committed edit (and every undo or redo), the photos whose settings name such a mode
//! but hold no correction are analysed. With [`UprightJobs::background`] (the desktop app) that
//! runs on a worker thread holding one activity row (`upright`, cancellable) for the batch; the
//! results are merged into each photo's current settings as they arrive ([`Session::upright_poll`],
//! every frame): no undo step of their own, and a photo that was edited meanwhile (another mode, a
//! lens change) is left to its newer analysis. Without it (CLI, MCP, tests) they are worked out at
//! once. Until its correction lands a photo renders with one worked out for the render
//! ([`lightcraft_pipeline::upright::resolve`]), so nothing waits on the analysis.
//!
//! A second click on the mode that is on (`geometry.upright {again: true}`) starts over: the
//! analyses still running are cancelled and every photo it reaches is analysed afresh.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};

use lightcraft_catalog::{Op, PhotoId};
use lightcraft_develop::{DevelopSettings, Upright};
use lightcraft_geom::Homography;

use crate::Session;
use crate::activity::TaskGuard;
use crate::media::SourceLevel;

/// Batches nested deeper than this are not looked into (ops are built by the app, but a journal
/// read back from disk is input).
const MAX_DEPTH: usize = 16;

/// Whether settings `d` name an automatic Upright mode but hold no correction for it.
pub(crate) fn needs_analysis(d: &DevelopSettings) -> bool {
    matches!(d.geometry.upright, Upright::Auto | Upright::Level | Upright::Vertical | Upright::Full)
        && d.geometry.upright_transform.is_none()
        && d.section_enabled("geometry")
}

/// What an analysis depends on besides the pixels: the lens corrections and the orientation.
fn basis(d: &DevelopSettings) -> u64 {
    DevelopSettings { optics: d.optics, orientation: d.orientation, ..Default::default() }.hash64() ^ u64::from(d.section_enabled("optics"))
}

/// Photos whose settings `op` sets, with those settings.
fn developed(op: &Op, out: &mut Vec<(PhotoId, Arc<DevelopSettings>)>, depth: usize) {
    match op {
        Op::SetDevelop { id, settings, .. } => out.push((*id, settings.clone())),
        Op::Batch { ops } if depth < MAX_DEPTH => ops.iter().for_each(|o| developed(o, out, depth + 1)),
        _ => {}
    }
}

/// Whether op `op` leaves some photo needing [`needs_analysis`] (cheap: no session).
pub(crate) fn needs_any(op: &Op) -> bool {
    let mut set = Vec::new();
    developed(op, &mut set, 0);
    set.iter().any(|(_, d)| needs_analysis(d))
}

/// One photo to analyse: its mode and what its analysis depends on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Job {
    mode: Upright,
    basis: u64,
}

/// What a worker found for a photo.
struct Found {
    id: PhotoId,
    job: Job,
    result: Result<Homography, String>,
}

/// The Upright analyses in flight ([`Session::upright_jobs`]).
#[derive(Default)]
pub struct UprightJobs {
    /// Analyse on a worker thread (the desktop app); else at once.
    pub background: bool,
    /// Photos queued or being analysed.
    pending: HashMap<PhotoId, Job>,
    /// One per worker started for this batch; a worker that ended (or died) is dropped.
    rx: Vec<Receiver<Found>>,
    /// The batch's activity row, while photos are pending.
    guard: Option<TaskGuard>,
    /// The batch's cancel flag (the row's ✕, `activity.cancel`, a second click).
    cancel: Option<Arc<AtomicBool>>,
    /// Photos the batch has finished.
    done: u64,
    /// Set by `geometry.upright {again}` while its edit is committed: Auto Sync carries the mode to
    /// every selected photo again, each to be analysed afresh.
    pub(crate) again: bool,
}

impl UprightJobs {
    /// Photos still waiting for their analysis.
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    /// Stop the batch: its workers stop at their next photo, their results are dropped, the row
    /// goes. Returns the photos that were still waiting.
    fn stop(&mut self) -> Vec<PhotoId> {
        if let Some(c) = self.cancel.take() {
            c.store(true, Ordering::Relaxed);
        }
        self.rx.clear();
        self.guard = None;
        self.done = 0;
        self.pending.drain().map(|(id, _)| id).collect()
    }
}

impl Session {
    /// Photo `id`'s own automatic Upright correction for `mode`, worked out now on its preview.
    pub fn upright_for(&mut self, id: PhotoId, mode: Upright) -> Result<Homography, String> {
        let src = self.source_now(id, SourceLevel::Preview)?;
        let info = self.source_info(id);
        let d = self.develop_of(id).unwrap_or_default();
        Ok(lightcraft_pipeline::upright::auto_transform(&src, &info, &d, mode))
    }

    /// After `op` was committed (or undone, or redone): analyse the photos it left with an
    /// automatic Upright mode and no correction.
    pub(crate) fn upright_after(&mut self, op: &Op) {
        let mut set = Vec::new();
        developed(op, &mut set, 0);
        self.upright_start(set.into_iter().map(|(id, _)| id).collect());
    }

    /// A second click on the mode that is on: stop what is running, and return the photos it was
    /// still to analyse (to be started over by [`Session::upright_start`]).
    pub(crate) fn upright_cancel(&mut self) -> Vec<PhotoId> {
        self.upright_jobs.stop()
    }

    /// Analyse those of `ids` whose current settings need it and that aren't already waiting for
    /// the same analysis.
    pub(crate) fn upright_start(&mut self, ids: Vec<PhotoId>) {
        let mut todo: Vec<(PhotoId, Job)> = Vec::new();
        for id in ids {
            let Some(d) = self.develop_of(id) else { continue };
            if !needs_analysis(&d) || todo.iter().any(|(x, _)| *x == id) {
                continue;
            }
            let job = Job { mode: d.geometry.upright, basis: basis(&d) };
            if self.upright_jobs.pending.get(&id) != Some(&job) {
                todo.push((id, job));
            }
        }
        if todo.is_empty() {
            return;
        }
        if self.upright_jobs.background {
            self.upright_queue(todo);
        } else {
            for (id, job) in todo {
                let result = self.upright_for(id, job.mode);
                self.upright_land(Found { id, job, result });
            }
        }
    }

    /// Give photo `f.id` what its analysis found, when its settings still ask for it.
    fn upright_land(&mut self, f: Found) {
        let t = match f.result {
            Ok(t) => t,
            Err(e) => {
                log::warn!("Upright on photo {}: {e}", f.id.0);
                return;
            }
        };
        let Some(cur) = self.develop_of(f.id) else { return };
        // edited meanwhile: another mode, Off, a correction of its own, or another lens
        // correction (a newer analysis was started for that one)
        if !needs_analysis(&cur) || cur.geometry.upright != f.job.mode || basis(&cur) != f.job.basis {
            return;
        }
        let mut d = (*cur).clone();
        d.geometry.upright_transform = Some(t);
        // Constrain Crop fits the crop to the new warp, as it does for an edit
        let d = self.fit_constrained_crop(f.id, d);
        let edited = self.catalog.photo(f.id).and_then(|p| p.edited.clone());
        if let Err(e) = self.apply_system(Op::SetDevelop { id: f.id, settings: Arc::new(d), label: "Upright".into(), edited }) {
            log::warn!("Upright on photo {}: {e}", f.id.0);
        }
    }

    /// Analyse `todo` on a worker thread, under the batch's one activity row.
    #[cfg(not(target_arch = "wasm32"))]
    fn upright_queue(&mut self, todo: Vec<(PhotoId, Job)>) {
        let mut jobs = Vec::with_capacity(todo.len());
        for (id, job) in todo {
            let Some(p) = self.catalog.photo(id).cloned() else { continue };
            let d = self.develop_of(id).unwrap_or_default();
            let src = self.media.source_ref(&p, SourceLevel::Preview);
            let info = self.source_info(id);
            jobs.push((id, job, src, info, d));
        }
        if jobs.is_empty() {
            return;
        }
        let cancel = self.upright_jobs.cancel.get_or_insert_with(|| Arc::new(AtomicBool::new(false))).clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let work = jobs.iter().map(|(id, job, ..)| (*id, *job)).collect::<Vec<_>>();
        let flag = cancel.clone();
        let spawned = std::thread::Builder::new().name("lc-upright".into()).spawn(move || {
            for (id, job, src, info, d) in jobs {
                if flag.load(Ordering::Relaxed) {
                    return;
                }
                // a panic in the analysis of one photo fails that photo, not the batch
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    src.load_source().map(|r| {
                        let info = r.info.clone().unwrap_or(info);
                        lightcraft_pipeline::upright::auto_transform(&r.image, &info, &d, job.mode)
                    })
                }))
                .unwrap_or_else(|_| Err("the analysis failed".into()));
                if tx.send(Found { id, job, result }).is_err() {
                    return;
                }
            }
        });
        if let Err(e) = spawned {
            // no thread to be had: work them out now rather than not at all
            log::warn!("Upright: no worker thread ({e}); analysing now");
            for (id, job) in work {
                let result = self.upright_for(id, job.mode);
                self.upright_land(Found { id, job, result });
            }
            return;
        }
        let uj = &mut self.upright_jobs;
        uj.rx.push(rx);
        for (id, job) in work {
            uj.pending.insert(id, job);
        }
        let activity = &self.activity;
        let guard = uj.guard.get_or_insert_with(|| activity.start("upright", "Upright", crate::activity::Cancel::Flag(cancel)));
        guard.progress(uj.done, uj.done.saturating_add(uj.pending.len() as u64));
    }

    /// The web build has no worker threads: work them out now.
    #[cfg(target_arch = "wasm32")]
    fn upright_queue(&mut self, todo: Vec<(PhotoId, Job)>) {
        for (id, job) in todo {
            let result = self.upright_for(id, job.mode);
            self.upright_land(Found { id, job, result });
        }
    }

    /// Apply the background analyses that finished (the desktop app calls this every frame) and
    /// end the batch when it is done or cancelled → whether a photo's settings changed.
    pub fn upright_poll(&mut self) -> bool {
        let uj = &mut self.upright_jobs;
        if uj.cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed)) {
            // the row's ✕: the photos left keep rendering with a correction worked out per render
            uj.stop();
            return false;
        }
        let mut found = Vec::new();
        uj.rx.retain(|rx| {
            loop {
                match rx.try_recv() {
                    Ok(f) => found.push(f),
                    Err(TryRecvError::Empty) => break true,
                    Err(TryRecvError::Disconnected) => break false,
                }
            }
        });
        let before = self.catalog.revision;
        for f in found {
            let uj = &mut self.upright_jobs;
            if uj.pending.get(&f.id) != Some(&f.job) {
                // superseded by a newer analysis of the same photo
                continue;
            }
            uj.pending.remove(&f.id);
            uj.done = uj.done.saturating_add(1);
            self.upright_land(f);
        }
        let uj = &mut self.upright_jobs;
        if uj.rx.is_empty() && !uj.pending.is_empty() {
            log::warn!("Upright: {} photos were left without their analysis", uj.pending.len());
            uj.pending.clear();
        }
        if uj.pending.is_empty() {
            uj.stop();
        } else if let Some(g) = &uj.guard {
            g.progress(uj.done, uj.done.saturating_add(uj.pending.len() as u64));
        }
        self.catalog.revision != before
    }
}
