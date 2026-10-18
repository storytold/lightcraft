//! Photo ▸ Auto Settings on several photos in the background (`develop.auto {background: true}`, #1752): each photo's
//! thumbnail-size source is read on a worker thread, with a row in the activity stack that can be cancelled. The
//! values are worked out on the session as the sources arrive ([`Session::pump_auto_settings`], every frame in the
//! app) and set as one undo step once all of them are in. A cancel, or a photo that can't be read, sets none, as the
//! inline command does.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use lightcraft_catalog::PhotoId;
use lightcraft_pipeline::auto::AutoTone;
use serde_json::{Value, json};

use crate::activity::{Cancel, TaskGuard};
use crate::cmd::bad;
use crate::media::{DecodedSource, SourceLevel};
use crate::{EngineError, Result, Session};

const C: &str = "develop.auto";

/// Sources taken in per pump at most, so a frame never works out a long run of them.
const MAX_DRAIN: usize = 8;

/// A background Auto Settings run.
pub struct AutoJob {
    identity: Arc<()>,
    cancel: Arc<AtomicBool>,
    rx: Receiver<(PhotoId, std::result::Result<DecodedSource, String>)>,
    /// Its row in the activity stack; goes with the job.
    guard: TaskGuard,
    total: usize,
    autos: Vec<(PhotoId, AutoTone)>,
}

/// How a background Auto Settings run ended.
#[derive(Debug)]
pub enum AutoOutcome {
    /// The values were set on this many photos, as one undo step.
    Applied(usize),
    /// Stopped (the row's ✕, `activity.cancel`, or another library opened): nothing was set.
    Cancelled,
    /// A photo could not be read: nothing was set.
    Failed(EngineError),
}

/// Start reading `ids`' sources on a worker.
pub(crate) fn start(s: &mut Session, ids: Vec<PhotoId>) -> Result<Value> {
    let mut sources = Vec::with_capacity(ids.len());
    for id in ids {
        let p = s.catalog.photo(id).ok_or_else(|| bad(C, "no such photo"))?.clone();
        sources.push((id, s.media.source_ref(&p, SourceLevel::Thumb)));
    }
    let total = sources.len();
    let cancel = Arc::new(AtomicBool::new(false));
    let (tx, rx) = channel();
    let stop = cancel.clone();
    std::thread::Builder::new()
        .name("lc-auto-settings".into())
        .spawn(move || {
            for (id, source) in sources {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                let r = crate::guard::catch("reading a photo for Auto Settings", || source.load_source()).and_then(|r| r);
                if tx.send((id, r)).is_err() {
                    return;
                }
            }
        })
        .map_err(|e| bad(C, format!("could not start: {e}")))?;
    let guard = s.activity.start("autoSettings", "Applying auto settings", Cancel::Flag(cancel.clone()));
    guard.progress(0, total as u64);
    let task_id = guard.id();
    s.auto_settings = Some(AutoJob { identity: Arc::clone(&s.library_identity), cancel, rx, guard, total, autos: Vec::with_capacity(total) });
    Ok(json!({"background": true, "total": total, "taskId": task_id}))
}

impl Session {
    /// Take in what a background Auto Settings run has read, and set the values once every photo is in. Cheap to
    /// call every frame (the app does). `Some` once the run has ended.
    pub fn pump_auto_settings(&mut self) -> Option<AutoOutcome> {
        let mut job = self.auto_settings.take()?;
        if job.cancel.load(Ordering::Relaxed) || !Arc::ptr_eq(&job.identity, &self.library_identity) {
            return Some(AutoOutcome::Cancelled);
        }
        let mut gone = false;
        for _ in 0..MAX_DRAIN {
            match job.rx.try_recv() {
                Ok((id, Ok(source))) => {
                    let image = self.keep_source(id, SourceLevel::Thumb, source);
                    job.autos.push((id, crate::cmd::develop::auto_values(self, id, &image)));
                }
                Ok((_, Err(e))) => return Some(AutoOutcome::Failed(bad(C, e))),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    gone = true;
                    break;
                }
            }
        }
        job.guard.progress(job.autos.len() as u64, job.total as u64);
        if job.autos.len() == job.total {
            let n = job.autos.len();
            return Some(match crate::cmd::develop::commit_autos(self, &job.autos) {
                Ok(()) => AutoOutcome::Applied(n),
                Err(e) => AutoOutcome::Failed(e),
            });
        }
        if gone {
            return Some(AutoOutcome::Failed(bad(C, "the photos could not all be read")));
        }
        self.auto_settings = Some(job);
        None
    }
}
