//! Build previews ahead of time: each photo's grid thumbnail and its loupe view (standard size,
//! or 1:1), rendered into the memory + disk preview cache so browsing and the loupe are instant.
//! Runs on a background thread (the app keeps working; progress / cancel by command, and a row in
//! the activity stack, issue #345), or inline with `wait` (CLI, MCP, tests).

#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod location;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd, str_param};
use crate::activity::{Cancel, TaskGuard};
use crate::media::{RenderJob, THUMB_SIZES};
use crate::{Result, Session};

/// The long edge of a standard-sized preview.
pub const STANDARD_EDGE: usize = 2048;

/// A preview build in progress.
#[derive(Debug, Default)]
pub struct PreviewBuild {
    pub total: usize,
    pub done: AtomicUsize,
    pub failed: AtomicUsize,
    /// Shared with the build's row in the activity stack (its ✕ sets it).
    pub cancel: Arc<AtomicBool>,
    pub finished: AtomicBool,
    /// Damaged smart previews rebuilt (Build Smart Previews only).
    pub repaired: AtomicUsize,
    /// Existing proxies kept because the original could not be read or the replacement written.
    pub kept: AtomicUsize,
    /// What is built ("" = previews; "smart previews" — Build Smart Previews runs the same way).
    pub what: &'static str,
    /// Why the whole build failed (e.g. the smart previews folder isn't there).
    pub error: std::sync::Mutex<Option<String>>,
}

impl PreviewBuild {
    pub fn json(&self) -> Value {
        json!({
            "total": self.total,
            "done": self.done.load(Ordering::Relaxed),
            "failed": self.failed.load(Ordering::Relaxed),
            "running": !self.finished.load(Ordering::Relaxed),
            "cancelled": self.cancel.load(Ordering::Relaxed),
            "what": if self.what.is_empty() { "previews" } else { self.what },
            "repaired": self.repaired.load(Ordering::Relaxed),
            "kept": self.kept.load(Ordering::Relaxed),
            "error": self.error(),
        })
    }

    pub fn error(&self) -> Option<String> {
        self.error.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }
}

/// The jobs that build `id`'s previews: the largest grid thumbnail and the view render at `edge`
/// (`None` = 1:1, the photo's full size), the larger first.
pub(crate) fn jobs_for(s: &mut Session, id: lightcraft_catalog::PhotoId, edge: Option<usize>) -> Vec<RenderJob> {
    let Some(p) = s.catalog.photo(id) else { return Vec::new() };
    let full = p.width.max(p.height) as usize;
    let e = edge.unwrap_or(full).min(full.max(1)).max(1);
    let thumb = THUMB_SIZES[THUMB_SIZES.len() - 1];
    let (view, grid) = (s.loupe_job(id, e, e, true), s.thumb_job(id, thumb));
    // the larger render goes first: the one after it renders from the source it loaded (see
    // `run_photo`), which must not be smaller than what it needs
    if e >= thumb { view.into_iter().chain(grid).collect() } else { grid.into_iter().chain(view).collect() }
}

/// The build's row in the activity stack (`what` as in [`PreviewBuild::what`]).
fn task_for(s: &Session, state: &PreviewBuild) -> TaskGuard {
    let (kind, label) = match state.what {
        "" => ("previews", "Building previews"),
        "smart previews" => ("smartPreviews", "Building smart previews"),
        _ => ("smartPreviews", "Discarding smart previews"),
    };
    let task = s.activity.start(kind, label, Cancel::Flag(state.cancel.clone()));
    task.progress(0, state.total as u64);
    task
}

/// Render one photo's jobs in order. The source the first job decodes serves the ones after it
/// (the grid thumbnail is made from the view's source instead of decoding the file again).
pub(crate) fn run_photo(jobs: Vec<RenderJob>) -> bool {
    let mut decoded: Option<crate::media::DecodedSource> = None;
    let mut ok = true;
    for mut j in jobs {
        if let Some(src) = &decoded
            && !matches!(j.source, crate::media::SourceRef::Loaded(_))
        {
            j.source = crate::media::SourceRef::Loaded(Box::new(src.clone()));
        }
        let r = j.run();
        ok &= r.rendered.is_ok();
        if decoded.is_none() {
            decoded = r.loaded;
        }
    }
    ok
}

/// How many photos render side by side: as many as an export batch for standard previews (each
/// render spreads over the cores already; the others fill them while one decodes), two for 1:1
/// previews (a full-size source each).
fn lanes(n: usize, full: bool) -> usize {
    let l = crate::export::export_parallelism(n);
    if full { l.min(2) } else { l }
}

fn run_all(jobs: Vec<Vec<RenderJob>>, state: &PreviewBuild, task: &TaskGuard, lanes: usize) {
    let total = jobs.len();
    let queue = std::sync::Mutex::new(jobs.into_iter());
    let work = || {
        loop {
            if state.cancel.load(Ordering::Relaxed) {
                break;
            }
            let next = queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner).next();
            let Some(photo_jobs) = next else { break };
            let ok = crate::guard::catch("building previews", || run_photo(photo_jobs)).unwrap_or(false);
            if !ok {
                state.failed.fetch_add(1, Ordering::Relaxed);
            }
            let done = state.done.fetch_add(1, Ordering::Relaxed).saturating_add(1);
            task.progress(done as u64, total as u64);
        }
    };
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::scope(|sc| {
        for k in 1..lanes.max(1) {
            // a lane that can't start leaves its photos to the others (and to this thread)
            if std::thread::Builder::new().name(format!("previews-{k}")).spawn_scoped(sc, work).is_err() {
                break;
            }
        }
        work();
    });
    #[cfg(target_arch = "wasm32")]
    {
        let _ = lanes;
        work();
    }
    state.finished.store(true, Ordering::Relaxed);
}

fn build(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "library.buildPreviews";
    let edge = match str_param(p, "size").unwrap_or("standard") {
        "standard" => Some(p.get("edge").and_then(Value::as_u64).map_or(STANDARD_EDGE, |e| e.clamp(256, 8192) as usize)),
        "full" | "1:1" => None,
        other => return Err(bad(C, format!("unknown size `{other}` (standard|full)"))),
    };
    if s.preview_build.as_ref().is_some_and(|b| !b.finished.load(Ordering::Relaxed)) {
        return Err(bad(C, "a preview build is already running"));
    }
    // explicit ids, else the selection, else everything in view
    let ids = if p.get("ids").is_some() || p.get("id").is_some() || !s.selection.ids.is_empty() { s.targets(p)? } else { s.visible_cloned() };
    let jobs: Vec<Vec<RenderJob>> = ids.iter().map(|id| jobs_for(s, *id, edge)).filter(|j| !j.is_empty()).collect();
    let state = Arc::new(PreviewBuild { total: jobs.len(), ..Default::default() });
    s.preview_build = Some(state.clone());
    let task = task_for(s, &state);
    let lanes = lanes(jobs.len(), edge.is_none());
    let wait = p.get("wait").and_then(Value::as_bool).unwrap_or(false) || cfg!(target_arch = "wasm32");
    if wait {
        run_all(jobs, &state, &task, lanes);
        return Ok(state.json());
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        // the row stays while the worker runs (it is dropped with the closure, however that ends)
        let st = state.clone();
        std::thread::Builder::new()
            .name("lc-build-previews".into())
            .spawn(move || run_all(jobs, &st, &task, lanes))
            .map_err(|e| bad(C, format!("could not start: {e}")))?;
    }
    Ok(state.json())
}

/// One photo's smart preview to build or discard: (photo, its file in the smart previews folder,
/// how to load the original at preview size, whether the photo can take a preview-fitted look:
/// vendor raws and DNGs (whose tone can come from their preview); only those proxies go stale when
/// the fit changes; an older encoding makes any proxy stale).
#[cfg(not(target_arch = "wasm32"))]
type SmartJob = (lightcraft_catalog::PhotoId, std::path::PathBuf, Option<crate::media::SourceRef>, bool);

/// DNG tone fits also go stale, although DNGs keep their own matrix and Kelvin white-balance scale.
#[cfg(not(target_arch = "wasm32"))]
fn preview_fitted_look(photo: &lightcraft_catalog::Photo) -> bool {
    photo.fitted_look() || (photo.develops_raw() && photo.format.eq_ignore_ascii_case("DNG"))
}

/// The smart previews being checked or written right now, by file name: a sharded proxy and its
/// pre-upgrade flat copy share it, and so do copies of one photo (`smart::file_name` is the content
/// hash). Build Smart Previews' lanes and the opening look refresh ([`refresh_stale_smart_previews`])
/// take a proxy's name here first, so no two write the same file at once.
#[cfg(not(target_arch = "wasm32"))]
struct ProxyLocks {
    busy: std::sync::Mutex<Vec<std::ffi::OsString>>,
    freed: std::sync::Condvar,
}

#[cfg(not(target_arch = "wasm32"))]
static PROXY_LOCKS: ProxyLocks = ProxyLocks { busy: std::sync::Mutex::new(Vec::new()), freed: std::sync::Condvar::new() };

/// One proxy's name in [`PROXY_LOCKS`], released when dropped.
#[cfg(not(target_arch = "wasm32"))]
struct ProxyLock(std::ffi::OsString);

#[cfg(not(target_arch = "wasm32"))]
impl ProxyLocks {
    /// Wait until no one else holds `path`'s proxy, then hold it (a handful of names at most: one per lane).
    fn lock(&'static self, path: &std::path::Path) -> ProxyLock {
        let name = path.file_name().map_or_else(|| path.as_os_str().to_os_string(), std::ffi::OsStr::to_os_string);
        let mut busy = self.busy.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        while busy.contains(&name) {
            busy = self.freed.wait(busy).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        busy.push(name.clone());
        ProxyLock(name)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for ProxyLock {
    fn drop(&mut self) {
        let mut busy = PROXY_LOCKS.busy.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        busy.retain(|n| *n != self.0);
        PROXY_LOCKS.freed.notify_all();
    }
}

/// What Build / Discard Smart Previews did.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Default)]
struct SmartCounts {
    /// Proxies there now (complete ones kept, missing or damaged ones written).
    built: usize,
    /// Of those, damaged ones (cut short by a crash or a full drive) that were rebuilt.
    repaired: usize,
    /// Of those, proxies written by an older camera-look fit that were rebuilt from their online originals.
    refreshed: usize,
    /// Of those kept as they were: out of date, but the original can't be read now (offline), so
    /// the stored look stays and the next run with the original online rebuilds them.
    stale_kept: usize,
    removed: usize,
    /// `[[id, why]]`
    failed: Vec<Value>,
}

#[cfg(not(target_arch = "wasm32"))]
impl SmartCounts {
    /// Fold one lane's counts into the run's.
    fn add(&mut self, o: SmartCounts) {
        self.built += o.built;
        self.repaired += o.repaired;
        self.refreshed += o.refreshed;
        self.stale_kept += o.stale_kept;
        self.removed += o.removed;
        self.failed.extend(o.failed);
    }
}

/// `source` if it reads the photo's original, else `None`. A source decoded from a smart preview
/// (it now carries the proxy's stored facts), a proxy standing in for an offline original, and the
/// proxy fallback of a file source are not the original: refreshing a stale proxy from one of them
/// would stamp its older look as current.
#[cfg(not(target_arch = "wasm32"))]
fn original_only(source: crate::media::SourceRef) -> Option<crate::media::SourceRef> {
    use crate::media::SourceRef;
    match source {
        SourceRef::File { path, max_edge, loader, denoise, .. } => {
            Some(SourceRef::File { path, max_edge, loader, fallback: None, denoise, reduced: None })
        }
        SourceRef::Loaded(d) if d.info.is_some() && !d.from_proxy && !d.reduced => Some(SourceRef::Loaded(d)),
        _ => None,
    }
}

/// The file-system half of Build / Discard Smart Previews (runs on a worker thread in the app).
/// `custom`: the folder was chosen (on another drive, perhaps): it must be there, it is never
/// recreated. With `state`, progress and cancel. `lanes` proxies are built side by side (the
/// calling thread is one lane).
#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::too_many_arguments)]
fn smart_run(
    dir: &std::path::Path,
    custom: bool,
    discard: bool,
    jobs: Vec<SmartJob>,
    state: Option<(&PreviewBuild, &TaskGuard)>,
    format: Option<crate::smart::Format>,
    cache: &crate::media::Invalidation,
    lanes: usize,
) -> std::result::Result<SmartCounts, String> {
    if !discard {
        // a chosen folder on another drive must be there: never recreate it on this one
        if custom {
            crate::smart::check_writable(dir)?;
        } else {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
    }
    // Cleared once, when the run ends; recorded on disk before the first proxy is written or
    // removed, so a quit or crash before that clear makes the next opening use new thumbnail keys.
    let pending = std::sync::Mutex::new(cache.pending());
    // set when the thumbnail cache change can't be recorded: every lane stops, and the run fails
    let stop = AtomicBool::new(false);
    let fatal: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
    let lanes = lanes.clamp(1, jobs.len().max(1));
    // numbered, so failures are reported in job order whichever lane met them
    let queue = std::sync::Mutex::new(jobs.into_iter().enumerate());
    let lane = || {
        let mut n = SmartCounts::default();
        let mut failed: Vec<(usize, Value)> = Vec::new();
        loop {
            if stop.load(Ordering::Relaxed) || state.is_some_and(|(st, _)| st.cancel.load(Ordering::Relaxed)) {
                break;
            }
            let next = queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner).next();
            let Some((index, job)) = next else { break };
            let id = job.0;
            let r = crate::guard::catch("building smart previews", || smart_one(dir, discard, job, state, format, &pending, &mut n));
            failed.extend(std::mem::take(&mut n.failed).into_iter().map(|f| (index, f)));
            match r {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    stop.store(true, Ordering::Relaxed);
                    fatal.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get_or_insert(e);
                    break;
                }
                Err(e) => {
                    // a panic inside one photo (a hostile file) fails that photo alone
                    failed.push((index, json!([id.0, e])));
                    if let Some((st, _)) = state {
                        st.failed.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            if let Some((st, task)) = state {
                let done = st.done.fetch_add(1, Ordering::Relaxed).saturating_add(1);
                task.progress(done as u64, st.total as u64);
            }
        }
        (n, failed)
    };
    let mut n = SmartCounts::default();
    let mut failed: Vec<(usize, Value)> = Vec::new();
    std::thread::scope(|sc| {
        // a lane that can't start leaves its photos to the others (and to this thread)
        let others: Vec<_> =
            (1..lanes).map_while(|k| std::thread::Builder::new().name(format!("smart-previews-{k}")).spawn_scoped(sc, lane).ok()).collect();
        let mut add = |(c, f): (SmartCounts, Vec<(usize, Value)>)| {
            n.add(c);
            failed.extend(f);
        };
        add(lane());
        for h in others {
            match h.join() {
                Ok(r) => add(r),
                Err(_) => log::warn!("smart previews: a build lane stopped unexpectedly"),
            }
        }
    });
    // failures in job order (the order of the selection), as a serial build reports them
    failed.sort_by_key(|(i, _)| *i);
    n.failed = failed.into_iter().map(|(_, f)| f).collect();
    // clears the thumbnail cache if a proxy changed, then removes the marker
    drop(pending);
    match fatal.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner) {
        Some(e) => Err(e),
        None => Ok(n),
    }
}

/// One proxy of [`smart_run`]: check it and build, rebuild, keep or remove it, adding to `n`. `Err`
/// stops the whole run: the thumbnail cache change could not be recorded before a write.
#[cfg(not(target_arch = "wasm32"))]
fn smart_one(
    dir: &std::path::Path,
    discard: bool,
    (id, path, source, look): SmartJob,
    state: Option<(&PreviewBuild, &TaskGuard)>,
    format: Option<crate::smart::Format>,
    pending: &std::sync::Mutex<crate::media::Pending>,
    n: &mut SmartCounts,
) -> std::result::Result<(), String> {
    let pending = || pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let _one = PROXY_LOCKS.lock(&path);
    // Read either the new shard or a pre-upgrade flat file, without making new work
    // depend on having the original online.
    let path = crate::smart::existing_file_path(dir, path);
    if discard {
        if path.symlink_metadata().is_ok() {
            // (never refused: on a full drive the discard frees the space)
            pending().before_change(crate::media::Change::Remove)?;
            if std::fs::remove_file(&path).is_ok() {
                n.removed += 1;
                pending().changed(crate::media::Change::Remove);
            }
        }
    } else {
        // a complete, current proxy is kept; a missing or damaged one (cut short by a crash or a
        // full drive) is (re)built, and so is one written by an older encoding or camera-look
        // fit (see `smart::ENCODE_VERSION` and `camera_preview::LOOK_VERSION`) when its
        // original can be read
        let damaged = path.exists();
        let previous = crate::smart::header(&path);
        let storage = format.or_else(|| previous.as_ref().map(|(f, _)| *f)).unwrap_or_default();
        let usable = crate::smart::is_usable(&path);
        let stale = usable && (crate::smart::is_stale_for(&path, look) || previous.as_ref().is_some_and(|(f, _)| *f != storage));
        if usable && !stale {
            n.built += 1;
        } else {
            let change = if stale { crate::media::Change::Replace } else { crate::media::Change::Write };
            // a stale proxy is rebuilt from the original only: never from itself (the file
            // fallback or a source decoded from a proxy would stamp the old look as current)
            let source = if stale { source.and_then(original_only) } else { source };
            // atomic: a failed write leaves no partial proxy that would pass for a built one;
            // and synced (issue #134): built so the photo can be edited while its original
            // is offline, when it is the only copy — the sync is small next to the decode
            // A decoder or encoder that panics on a hostile file fails this proxy through the
            // error paths below (a stale one is kept and counted), not as a lane-level failure.
            let loaded = source
                .ok_or_else(|| "nothing to build from".to_string())
                .and_then(|s| crate::guard::catch("decoding a smart preview's original", || s.load_source()).and_then(|r| r));
            let unavailable = loaded.is_err();
            let encoded = loaded
                .and_then(|src| {
                    // the decoder facts ride with the pixels, so an offline edit renders as online
                    let info = src.info_or(Default::default());
                    crate::guard::catch("encoding a smart preview", || crate::smart::encode_format(&src.image, Some(&info), storage)).and_then(|r| r)
                })
                .and_then(|bytes| match &previous {
                    Some((_, head)) => crate::smart::preserve_header(&bytes, head),
                    None => Ok(bytes),
                });
            // recorded only once there is something to write: a run of offline originals changes nothing
            if encoded.is_ok() {
                pending().before_change(change)?;
            }
            let r = encoded.and_then(|b| {
                let parent = path.parent().ok_or_else(|| format!("{}: no parent folder", path.display()))?;
                // Only create the leaf shard: do not recreate a missing external drive.
                // Another lane may create the same new shard first: that is fine, a missing
                // parent of the shard still fails.
                if !parent.is_dir() {
                    match std::fs::create_dir(parent) {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && parent.is_dir() => {}
                        Err(e) => return Err(format!("{}: {e}", parent.display())),
                    }
                }
                lightcraft_catalog::safe_file::write_atomic(&path, &b).map_err(|e| format!("{}: {e}", path.display()))
            });
            match r {
                Ok(()) => {
                    pending().changed(change);
                    n.built += 1;
                    if stale {
                        n.refreshed += 1;
                    } else if damaged {
                        n.repaired += 1;
                        if let Some((st, _)) = state {
                            st.repaired.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                Err(e) if stale => {
                    // offline (or unreadable) original: the proxy is all there is, keep its look
                    log::info!("smart preview {} keeps its older look: {e}", path.display());
                    n.built += 1;
                    n.stale_kept += 1;
                    if !unavailable {
                        n.failed.push(json!([id.0, e]));
                        if let Some((st, _)) = state {
                            st.failed.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    if let Some((st, _)) = state {
                        st.kept.fetch_add(1, Ordering::Relaxed);
                    }
                }
                Err(e) => {
                    n.failed.push(json!([id.0, e]));
                    if let Some((st, _)) = state {
                        st.failed.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }
    }
    Ok(())
}

/// How many proxies Build Smart Previews builds side by side for `n` photos: as many as an export
/// batch. Tests run several lanes on every machine, so #1400's end-to-end tests cover them on small
/// CI runners too.
#[cfg(not(target_arch = "wasm32"))]
fn smart_lanes(n: usize) -> usize {
    if cfg!(test) { n.clamp(1, 4) } else { crate::export::export_parallelism(n) }
}

/// Smart previews: build (from the original, at preview size) or discard the proxies that keep
/// photos editable while their originals are offline. With `background` (the app) the files are
/// read and written on a worker thread; progress like Build Previews (`library.previewProgress`).
#[cfg(not(target_arch = "wasm32"))]
fn smart(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "library.smartPreviews";
    if s.activity.list().iter().any(|t| t.kind == "smartMigration") {
        return Err(bad(C, "the smart preview folder is changing; wait for it to finish"));
    }
    let dir = s.media.smart_dir.clone().ok_or_else(|| bad(C, "smart previews need a library on disk"))?;
    let discard = p.get("discard").and_then(Value::as_bool).unwrap_or(false);
    let background = p.get("background").and_then(Value::as_bool).unwrap_or(false);
    let format = match p.get("format") {
        Some(v) => v.as_str().and_then(crate::smart::Format::parse).ok_or_else(|| bad(C, "format must be compact or lossless"))?,
        None => s.smart_previews_format,
    };
    if s.preview_build.as_ref().is_some_and(|b| !b.finished.load(Ordering::Relaxed)) {
        return Err(bad(C, "a preview build is already running"));
    }
    let ids = if p.get("ids").is_some() || !s.selection.ids.is_empty() { s.targets(p)? } else { s.visible_cloned() };
    let mut jobs: Vec<SmartJob> = Vec::new();
    for id in ids {
        let Some(ph) = s.catalog.photo(id).cloned() else { continue };
        if !matches!(ph.source, lightcraft_catalog::Source::File { .. }) {
            continue;
        }
        let path = crate::smart::sharded_file_path(&dir, &ph);
        let source = (!discard).then(|| s.media.origin_ref(&ph.source, crate::media::SourceLevel::Preview.max_edge()));
        jobs.push((id, path, source, preview_fitted_look(&ph)));
    }
    let custom = s.smart_previews_dir.is_some();
    let cache = s.media.invalidation();
    let lanes = smart_lanes(jobs.len());
    if background {
        let what = if discard { "discarding smart previews" } else { "smart previews" };
        let state = Arc::new(PreviewBuild { total: jobs.len(), what, ..Default::default() });
        s.preview_build = Some(state.clone());
        let task = task_for(s, &state);
        let st = state.clone();
        std::thread::Builder::new()
            .name("lc-smart-previews".into())
            .spawn(move || {
                if let Err(e) = smart_run(&dir, custom, discard, jobs, Some((&st, &task)), Some(format), &cache, lanes) {
                    log::warn!("{C}: {e}");
                    *st.error.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(e);
                }
                st.finished.store(true, Ordering::Relaxed);
            })
            .map_err(|e| bad(C, format!("could not start: {e}")))?;
        return Ok(state.json());
    }
    let state = PreviewBuild { total: jobs.len(), what: "smart previews", ..Default::default() };
    let task = task_for(s, &state);
    let r = smart_run(&dir, custom, discard, jobs, Some((&state, &task)), Some(format), &cache, lanes).map_err(|e| bad(C, e))?;
    Ok(
        json!({"built": r.built, "repaired": r.repaired, "refreshed": r.refreshed, "staleKept": r.stale_kept, "removed": r.removed, "failed": r.failed}),
    )
}

/// Bring smart previews up to date, once per fit-and-encoding version: when the smart previews
/// folder has not been checked at this build's versions (its marker, [`crate::smart::look_checked`]),
/// a worker thread (the UI never waits on the drives) looks at the proxies of the photos on disk
/// and rebuilds the stale ones from their originals — one whose stored pixels are an older encoding
/// (every photo), or, for a raw that takes the per-file camera look or a DNG, one written by an older fit.
/// One whose original can't be read (offline) is kept as it is, still marked stale in its header:
/// Build Smart Previews rebuilds it once the original is back. Only existing proxies are touched:
/// none is built, and a missing folder is never created. Returns the thread, if started.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn refresh_stale_smart_previews(s: &mut Session) -> Option<std::thread::JoinHandle<()>> {
    if s.activity.list().iter().any(|t| t.kind == "smartMigration") {
        return None;
    }
    let dir = s.media.smart_dir.clone()?;
    if !dir.is_dir() || crate::smart::look_checked(&dir) {
        return None;
    }
    let max_edge = crate::media::SourceLevel::Preview.max_edge();
    let photos: Vec<_> = s.catalog.photos().filter(|p| matches!(p.source, lightcraft_catalog::Source::File { .. })).collect();
    let jobs: Vec<SmartJob> = photos
        .iter()
        .map(|p| (p.id, crate::smart::sharded_file_path(&dir, p), Some(s.media.origin_ref(&p.source, max_edge)), preview_fitted_look(p)))
        .collect();
    if jobs.is_empty() {
        return None;
    }
    let cache = s.media.invalidation();
    let mut state = PreviewBuild { total: jobs.len(), what: "smart previews", ..Default::default() };
    let task = s.activity.start("smartRefresh", "Refreshing smart previews", Cancel::Flag(state.cancel.clone()));
    std::thread::Builder::new()
        .name("lc-smart-look".into())
        .spawn(move || refresh_stale(&dir, jobs, &cache, &mut state, &task))
        .map_err(|e| log::warn!("smart previews: could not start the look refresh: {e}"))
        .ok()
}

/// Tests: wait for the opening refresh ([`refresh_stale_smart_previews`]) to finish. Opening a
/// library whose smart previews folder was not checked at this build's versions starts it (it looks
/// at every photo's proxy, since an older encoding reaches them all), and a folder change refuses
/// to run beside it.
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) fn wait_for_opening_refresh(s: &Session) {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while s.activity.list().iter().any(|t| t.kind == "smartRefresh") {
        assert!(std::time::Instant::now() < until, "the opening smart preview refresh did not finish");
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

/// The body of [`refresh_stale_smart_previews`]: rebuild the stale proxies among `jobs`.
#[cfg(not(target_arch = "wasm32"))]
fn refresh_stale(dir: &std::path::Path, jobs: Vec<SmartJob>, cache: &crate::media::Invalidation, state: &mut PreviewBuild, task: &TaskGuard) {
    let stale: Vec<SmartJob> = jobs
        .into_iter()
        .map(|(id, path, source, look)| (id, crate::smart::existing_file_path(dir, path), source, look))
        .filter(|(_, path, _, look)| crate::smart::is_stale_for(path, *look))
        .collect();
    if state.cancel.load(Ordering::Relaxed) {
        return;
    }
    state.total = stale.len();
    task.progress(0, stale.len() as u64);
    if !stale.is_empty() {
        // `custom`: the folder is there already, and must not be recreated if it went away since;
        // one lane: it runs while the library opens, next to the first renders
        match smart_run(dir, true, false, stale, Some((state, task)), None, cache, 1) {
            Ok(n) => log::info!("smart previews: {} refreshed to the current camera look, {} kept (original offline)", n.refreshed, n.stale_kept),
            Err(e) => {
                // not marked: the next opening tries again
                log::warn!("smart previews: look refresh: {e}");
                return;
            }
        }
    }
    if state.cancel.load(Ordering::Relaxed) {
        return;
    }
    if let Err(e) = crate::smart::mark_look_checked(dir) {
        log::warn!("smart previews: {e}");
    }
}

/// The smart previews folder: where it is, whether it is custom, what it holds, whether it can be
/// used now (a chosen folder on an unplugged drive is `available: false`).
#[cfg(not(target_arch = "wasm32"))]
fn location_json(s: &Session) -> Value {
    let dir = s.media.smart_dir.clone();
    let default = s.library.as_ref().filter(|l| l.on_disk).map(|l| crate::smart::dir(&l.dir));
    let (count, bytes) = dir.as_deref().map_or((0, 0), crate::smart::stats);
    json!({
        "path": dir.as_ref().map(|d| d.to_string_lossy()),
        "default": default.map(|d| d.to_string_lossy().to_string()),
        "custom": s.smart_previews_dir.is_some(),
        "available": dir.as_ref().is_some_and(|d| d.is_dir()) || s.smart_previews_dir.is_none(),
        "count": count,
        "bytes": bytes,
    })
}

/// Show or change where smart previews are kept (per library).
#[cfg(not(target_arch = "wasm32"))]
fn smart_location(s: &mut Session, p: &Value) -> Result<Value> {
    location::start(s, p)
}

/// Choose the default for future builds. Changing it never discards an offline proxy.
#[cfg(not(target_arch = "wasm32"))]
fn smart_format(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "library.smartPreviewsFormat";
    if s.media.smart_dir.is_none() {
        return Err(bad(C, "smart previews need a library on disk"));
    }
    if let Some(value) = p.get("format") {
        let format = value.as_str().and_then(crate::smart::Format::parse).ok_or_else(|| bad(C, "format must be compact or lossless"))?;
        let previous = s.smart_previews_format;
        s.smart_previews_format = format;
        if let Err(e) = s.save_prefs() {
            s.smart_previews_format = previous;
            return Err(e);
        }
    }
    Ok(json!({"format": s.smart_previews_format}))
}

/// Whether photo `id` has a smart preview.
pub fn has_smart_preview(s: &Session, id: lightcraft_catalog::PhotoId) -> bool {
    match (&s.media.smart_dir, s.catalog.photo(id)) {
        (Some(dir), Some(p)) => crate::smart::is_usable(&crate::smart::file_path(dir, p)),
        _ => false,
    }
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        #[cfg(not(target_arch = "wasm32"))]
        cmd!(
            "library.smartPreviewsFormat",
            "Smart Preview Format",
            [],
            None,
            "{format?: compact|lossless}. Read or set this library's default. Existing previews keep working; Build Smart Previews converts them from available originals.",
            always,
            smart_format
        ),
        #[cfg(not(target_arch = "wasm32"))]
        cmd!(
            "library.smartPreviews",
            "Build Smart Previews",
            [],
            None,
            "{ids?, discard?: bool, background?: bool, format?: compact|lossless}. Build or discard smart previews for selected photos, or all in view. Uses the library format unless overridden. Converts existing previews only from available originals. Returns {built, repaired, refreshed, staleKept, removed, failed}. Background builds return progress, including kept previews; use library.previewProgress and library.cancelPreviews.",
            always,
            smart
        ),
        #[cfg(not(target_arch = "wasm32"))]
        cmd!(
            "library.smartPreviewsLocation",
            "Smart Previews Location",
            [],
            None,
            "{path?: absolute folder, reset?: bool, existing?: move|leave|discard, background?: bool}. Without path/reset, returns {path, default, custom, available, count, bytes, running}. Set path to change folders, or reset to use the library folder. Existing previews need an explicit move, leave or discard choice. Background mode returns immediately; activity.list and activity.cancel track copying. The old folder stays in use on cancellation or copy failure. After all copies verify and the location is saved, source cleanup completes without cancellation. Poll this command for the outcome in migration. Synchronous mode returns {handled, failed}.",
            always,
            smart_location
        ),
        cmd!(query "photo.smartPreview", "Smart Preview Status", [], None, "{id?} → {smartPreview: bool, originalOnline: bool}", always, |s, p| {
            let id = s.targets(p)?.first().copied().ok_or_else(|| bad("photo.smartPreview", "no photo"))?;
            let online = match s.catalog.photo(id).map(|p| p.source.clone()) {
                Some(lightcraft_catalog::Source::File { path }) => std::path::Path::new(&path).exists(),
                _ => true,
            };
            Ok(json!({"smartPreview": has_smart_preview(s, id), "originalOnline": online}))
        }),
        cmd!(
            "library.buildPreviews",
            "Build Previews",
            [],
            None,
            "{size?: standard (2048 px, or `edge`) | full (1:1), ids?, wait?: bool} — render the grid thumbnail and loupe view of the selected photos (else all in view) into the preview cache, in the background unless `wait` → {total, done, failed, running}",
            always,
            build
        ),
        cmd!(query "library.previewProgress", "Preview Build Progress", [], None, "{} → {total, done, failed, repaired, running, cancelled, what, error} | null", always, |s, _| {
            Ok(s.preview_build.as_ref().map_or(Value::Null, |b| b.json()))
        }),
        cmd!("library.cancelPreviews", "Cancel Preview Build", [], None, "{}", always, |s, _| {
            if let Some(b) = &s.preview_build {
                b.cancel.store(true, Ordering::Relaxed);
            }
            Ok(s.preview_build.as_ref().map_or(Value::Null, |b| b.json()))
        }),
    ]
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{jobs_for, run_photo};
    use crate::Session;
    use crate::media::SourceRef;

    // Given a photo's preview jobs, the grid thumbnail is made from the source the view decoded, not
    // by reading the file again (here: a thumbnail job pointing at a file that is not there)
    #[test]
    fn preview_build_makes_the_thumbnail_from_the_views_source() {
        let mut s = Session::with_demo();
        let id = s.active().unwrap();
        let mut jobs = jobs_for(&mut s, id, Some(512));
        assert_eq!(jobs.len(), 2, "view and thumbnail");
        assert!(jobs[0].view_cache.is_some(), "the view renders first");
        jobs[1].source =
            SourceRef::File { path: "/nonexistent/photo.arw".into(), max_edge: 512, loader: None, fallback: None, denoise: None, reduced: None };
        assert!(run_photo(jobs), "the thumbnail rendered from the view's decoded source");
    }

    // Given many photos, a standard preview build renders them side by side and counts every one
    #[test]
    fn preview_build_of_many_photos_counts_each_once() {
        let mut s = Session::with_demo();
        let ids: Vec<u64> = s.catalog.photos().map(|p| p.id.0).collect();
        assert!(ids.len() > 4);
        let r = s.execute("library.buildPreviews", &json!({"size": "standard", "edge": 512, "ids": ids, "wait": true})).unwrap();
        assert_eq!(r["done"], json!(ids.len()), "{r}");
        assert_eq!(r["total"], json!(ids.len()));
        assert_eq!(r["failed"], json!(0));
    }

    #[cfg(not(target_arch = "wasm32"))]
    mod smart_lanes {
        use std::path::{Path, PathBuf};
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::{Duration, Instant};

        use lightcraft_catalog::PhotoId;
        use lightcraft_raster::Rgb32f;

        use super::super::{PreviewBuild, SmartJob, smart_run, task_for};
        use crate::Session;
        use crate::media::{FileLoader, SourceRef};

        fn temp_dir(tag: &str) -> PathBuf {
            let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let d = std::env::temp_dir().join(format!("lc-smartlanes-{tag}-{}-{nanos}", std::process::id()));
            std::fs::create_dir_all(&d).unwrap();
            d
        }

        /// A job whose "original" is a tiny synthetic image; `load` runs first and can wait, fail or panic.
        fn job(dir: &Path, name: &str, id: u64, load: impl Fn() -> Result<(), String> + Send + Sync + 'static) -> SmartJob {
            let loader: FileLoader = Arc::new(move |_: &str, _: usize| {
                load()?;
                Ok((Rgb32f { width: 8, height: 6, data: vec![[0.25, 0.5, 0.75]; 48] }, Default::default()))
            });
            let source = SourceRef::File {
                path: format!("/virtual/{name}.raw"),
                max_edge: 2560,
                loader: Some(loader),
                fallback: None,
                denoise: None,
                reduced: None,
            };
            (PhotoId(id), dir.join(&name[..2]).join(format!("{name}.lcsp")), Some(source), false)
        }

        // Given two photos and two lanes, both decodes are in flight at once: each loader waits (up to
        // 5 s) for the other to start, so a serial build times the first one out.
        #[test]
        fn two_lanes_build_two_photos_at_once() {
            let s = Session::new();
            let dir = temp_dir("both");
            let started = Arc::new(AtomicUsize::new(0));
            let rendezvous = |started: Arc<AtomicUsize>| {
                move || {
                    started.fetch_add(1, Ordering::SeqCst);
                    let t = Instant::now();
                    while started.load(Ordering::SeqCst) < 2 {
                        if t.elapsed() > Duration::from_secs(5) {
                            return Err("the other photo never started".to_string());
                        }
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Ok(())
                }
            };
            let jobs = vec![job(&dir, "aa01", 1, rendezvous(started.clone())), job(&dir, "bb02", 2, rendezvous(started.clone()))];
            let state = PreviewBuild { total: 2, what: "smart previews", ..Default::default() };
            let task = task_for(&s, &state);
            let n = smart_run(&dir, false, false, jobs, Some((&state, &task)), None, &s.media.invalidation(), 2).unwrap();
            assert_eq!((n.built, n.failed.len()), (2, 0), "{:?}", n.failed);
            assert_eq!(state.done.load(Ordering::Relaxed), 2);
            let _ = std::fs::remove_dir_all(&dir);
        }

        // Given two jobs for one proxy (copies of a photo share a content hash), the proxy is decoded and
        // written once, and both jobs count as built.
        #[test]
        fn copies_share_one_proxy_and_one_decode() {
            let s = Session::new();
            let dir = temp_dir("copies");
            let decodes = Arc::new(AtomicUsize::new(0));
            let count = |d: Arc<AtomicUsize>| {
                move || {
                    d.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(50));
                    Ok(())
                }
            };
            let jobs = vec![job(&dir, "cc03", 1, count(decodes.clone())), job(&dir, "cc03", 2, count(decodes.clone()))];
            let n = smart_run(&dir, false, false, jobs, None, None, &s.media.invalidation(), 2).unwrap();
            assert_eq!((n.built, n.failed.len()), (2, 0), "{:?}", n.failed);
            assert_eq!(decodes.load(Ordering::SeqCst), 1, "the second job found the proxy built");
            assert!(crate::smart::is_usable(&dir.join("cc").join("cc03.lcsp")));
            let _ = std::fs::remove_dir_all(&dir);
        }

        // Given a photo whose decode panics, that photo fails alone and the build finishes the rest.
        #[test]
        fn a_panicking_photo_fails_alone() {
            let s = Session::new();
            let dir = temp_dir("panic");
            let mut jobs: Vec<SmartJob> = (0..5u64).map(|i| job(&dir, &format!("d{i}0{i}"), i, || Ok(()))).collect();
            jobs.insert(2, job(&dir, "ee99", 99, || panic!("hostile raw")));
            let state = PreviewBuild { total: jobs.len(), what: "smart previews", ..Default::default() };
            let task = task_for(&s, &state);
            let n = smart_run(&dir, false, false, jobs, Some((&state, &task)), None, &s.media.invalidation(), 3).unwrap();
            assert_eq!(n.built, 5);
            assert_eq!(n.failed.len(), 1);
            assert_eq!(n.failed[0][0], 99);
            assert_eq!(state.done.load(Ordering::Relaxed), 6, "progress reaches the total");
            assert_eq!(state.failed.load(Ordering::Relaxed), 1);
            let _ = std::fs::remove_dir_all(&dir);
        }

        // Given a usable proxy due for conversion whose original's decoder panics, the proxy is kept
        // and counted like one whose original is offline, not reported as a failed photo.
        #[test]
        fn a_stale_proxy_whose_decoder_panics_is_kept() {
            let s = Session::new();
            let dir = temp_dir("stalepanic");
            let n = smart_run(&dir, false, false, vec![job(&dir, "fa01", 1, || Ok(()))], None, None, &s.media.invalidation(), 1).unwrap();
            assert_eq!(n.built, 1);
            let proxy = dir.join("fa").join("fa01.lcsp");
            let before = std::fs::read(&proxy).unwrap();
            let state = PreviewBuild { total: 1, what: "smart previews", ..Default::default() };
            let task = task_for(&s, &state);
            let jobs = vec![job(&dir, "fa01", 1, || panic!("hostile raw"))];
            let n =
                smart_run(&dir, false, false, jobs, Some((&state, &task)), Some(crate::smart::Format::Lossless), &s.media.invalidation(), 2).unwrap();
            assert_eq!((n.built, n.stale_kept, n.failed.len()), (1, 1, 0), "{:?}", n.failed);
            assert_eq!(state.kept.load(Ordering::Relaxed), 1);
            assert_eq!(std::fs::read(&proxy).unwrap(), before, "the proxy is left as it was");
            let _ = std::fs::remove_dir_all(&dir);
        }

        // Given several lanes writing the first proxies into one new shard folder at once, none of
        // them fails because another lane created the folder first.
        #[test]
        fn lanes_share_a_new_shard_folder() {
            let s = Session::new();
            for round in 0..20 {
                let dir = temp_dir(&format!("shard{round}"));
                let started = Arc::new(AtomicUsize::new(0));
                let jobs: Vec<SmartJob> = (1..=8u64)
                    .map(|i| {
                        let started = started.clone();
                        job(&dir, &format!("ab{i:02}"), i, move || {
                            // the first four line up, so their writes meet the missing shard together
                            started.fetch_add(1, Ordering::SeqCst);
                            let t = Instant::now();
                            while started.load(Ordering::SeqCst) < 4 && t.elapsed() < Duration::from_secs(5) {
                                std::hint::spin_loop();
                            }
                            Ok(())
                        })
                    })
                    .collect();
                let n = smart_run(&dir, false, false, jobs, None, None, &s.media.invalidation(), 4).unwrap();
                assert!(n.failed.is_empty(), "round {round}: {:?}", n.failed);
                assert_eq!(n.built, 8);
                let _ = std::fs::remove_dir_all(&dir);
            }
        }

        // Given failures met by different lanes out of order, they are reported in job order (the
        // selection's order, not photo id order), as the serial build reported them.
        #[test]
        fn failures_are_reported_in_job_order() {
            let s = Session::new();
            let dir = temp_dir("order");
            let ids = [9u64, 7, 5, 3, 1];
            let jobs: Vec<SmartJob> = ids
                .iter()
                .enumerate()
                .map(|(k, &id)| {
                    job(&dir, &format!("f{id}0{id}"), id, move || {
                        // earlier jobs finish later
                        std::thread::sleep(Duration::from_millis(10 * (ids.len() - k) as u64));
                        Err("unreadable".to_string())
                    })
                })
                .collect();
            let n = smart_run(&dir, false, false, jobs, None, None, &s.media.invalidation(), 3).unwrap();
            let got: Vec<u64> = n.failed.iter().filter_map(|f| f[0].as_u64()).collect();
            assert_eq!(got, ids);
            let _ = std::fs::remove_dir_all(&dir);
        }

        // Given a cancel during a build with several lanes, every lane stops and each proxy on disk is complete.
        #[test]
        fn cancel_stops_every_lane_and_leaves_no_partial_proxy() {
            let s = Session::new();
            let dir = temp_dir("cancel");
            let state = PreviewBuild { total: 40, what: "smart previews", ..Default::default() };
            let cancel = state.cancel.clone();
            let jobs: Vec<SmartJob> = (0..40u64)
                .map(|i| {
                    let cancel = cancel.clone();
                    job(&dir, &format!("{i:02x}{i:02x}"), i, move || {
                        if i == 3 {
                            cancel.store(true, Ordering::Relaxed);
                        }
                        std::thread::sleep(Duration::from_millis(5));
                        Ok(())
                    })
                })
                .collect();
            let task = task_for(&s, &state);
            let n = smart_run(&dir, false, false, jobs, Some((&state, &task)), None, &s.media.invalidation(), 4).unwrap();
            // job 3 cancels: the four first jobs, plus at most one more per lane already in flight
            assert!(n.built <= 4 + 4, "stopped early: {}", n.built);
            assert_eq!(state.done.load(Ordering::Relaxed), n.built, "no photo counted twice or lost across lanes");
            for e in walk(&dir) {
                assert!(e.extension().is_some_and(|x| x == "lcsp"), "{} is left behind", e.display());
                assert!(crate::smart::is_usable(&e), "{} is complete", e.display());
            }
            let _ = std::fs::remove_dir_all(&dir);
        }

        fn walk(dir: &Path) -> Vec<PathBuf> {
            let mut out = Vec::new();
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() { out.extend(walk(&p)) } else { out.push(p) }
            }
            out
        }
    }
}
