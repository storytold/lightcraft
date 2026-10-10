//! Build previews ahead of time: each photo's grlet ids: Vec<u64> = ids.iter().map(|id| id.0).collect(); thumbnail and its loupe view (standard size,
//! or 1:1), rendered into the memory + disk preview cache so browsing and the loupe are instant.
//! Runs on a background thread (the app keeps working; progress / cancel by command, and a row in
//! the activity stack, issue #345), or inline with `wait` (CLI, MCP, tests).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd, str_param};
use crate::activity::{Cancel, TaskGuard};
use crate::media::{RenderJob, THUMB_SIZES};
use crate::{Result, Session};

/// The long edge of a standard-sized preview.
pub const STANDARD_EDGE: usize = 2048;

/// When 1:1 previews are discarded on their own (the catalog setting "Automatically discard 1:1
/// previews"): this long after they were built or last shown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DiscardAfter {
    Day,
    Week,
    #[default]
    Month,
    Never,
}

impl DiscardAfter {
    pub fn parse(s: &str) -> Option<DiscardAfter> {
        Some(match s {
            "day" => DiscardAfter::Day,
            "week" => DiscardAfter::Week,
            "month" => DiscardAfter::Month,
            "never" => DiscardAfter::Never,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            DiscardAfter::Day => "day",
            DiscardAfter::Week => "week",
            DiscardAfter::Month => "month",
            DiscardAfter::Never => "never",
        }
    }

    /// The age past which a 1:1 preview goes (`None`: never).
    pub fn max_age(self) -> Option<std::time::Duration> {
        let day = 24 * 3600;
        match self {
            DiscardAfter::Day => Some(std::time::Duration::from_secs(day)),
            DiscardAfter::Week => Some(std::time::Duration::from_secs(7 * day)),
            DiscardAfter::Month => Some(std::time::Duration::from_secs(30 * day)),
            DiscardAfter::Never => None,
        }
    }
}

/// Which previews an import builds once its photos are in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ImportPreviews {
    /// Thumbnails only, as the grid asks for them.
    #[default]
    Minimal,
    Standard,
    Full,
}

impl ImportPreviews {
    pub fn parse(s: &str) -> Option<ImportPreviews> {
        Some(match s {
            "minimal" => ImportPreviews::Minimal,
            "standard" => ImportPreviews::Standard,
            "full" | "1:1" => ImportPreviews::Full,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ImportPreviews::Minimal => "minimal",
            ImportPreviews::Standard => "standard",
            ImportPreviews::Full => "full",
        }
    }
}

/// The preview store's settings, per library (saved in its `prefs.json`).
// TODO(P1.5): move into the catalog's own settings once its storage rework lands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PreviewPrefs {
    /// Long edge of standard-sized previews (0 = [`STANDARD_EDGE`]).
    pub standard_edge: u32,
    pub discard_full: DiscardAfter,
    pub at_import: ImportPreviews,
}

impl PreviewPrefs {
    pub fn standard_edge(&self) -> usize {
        if self.standard_edge == 0 { STANDARD_EDGE } else { (self.standard_edge as usize).clamp(256, 8192) }
    }

    pub fn json(&self) -> Value {
        json!({
            "standardEdge": self.standard_edge(),
            "discardFull": self.discard_full.as_str(),
            "atImport": self.at_import.as_str(),
        })
    }
}

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
            "error": self.error(),
        })
    }

    pub fn error(&self) -> Option<String> {
        self.error.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }
}

/// The jobs that build `id`'s previews: the largest grid thumbnail, then the view render at
/// `edge` (`None` = 1:1, the photo's full size).
fn jobs_for(s: &mut Session, id: dac_catalog::PhotoId, edge: Option<usize>) -> Vec<RenderJob> {
    let Some(p) = s.catalog.photo(id).cloned() else { return Vec::new() };
    let full = p.width.max(p.height) as usize;
    let e = edge.unwrap_or(full).min(full.max(1)).max(1);
    let mut v = Vec::new();
    v.extend(s.thumb_job(id, THUMB_SIZES[THUMB_SIZES.len() - 1]));
    let mut view = s.loupe_job(id, e, e, true);
    // a 1:1 preview is kept under its own key, to be discarded on its own
    if edge.is_none()
        && let Some(j) = view.as_mut()
        && let Some((_, key)) = j.view_cache.as_mut()
    {
        *key = Session::full_view_key(&p);
    }
    v.extend(view);
    v
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

fn run_all(jobs: Vec<Vec<RenderJob>>, state: &PreviewBuild, task: &TaskGuard) {
    for photo_jobs in jobs {
        if state.cancel.load(Ordering::Relaxed) {
            break;
        }
        let ok = photo_jobs.into_iter().all(|j| j.run().rendered.is_ok());
        if !ok {
            state.failed.fetch_add(1, Ordering::Relaxed);
        }
        let done = state.done.fetch_add(1, Ordering::Relaxed).saturating_add(1);
        task.progress(done as u64, state.total as u64);
    }
    state.finished.store(true, Ordering::Relaxed);
}

fn build(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "library.buildPreviews";
    let edge = match str_param(p, "size").unwrap_or("standard") {
        "standard" => Some(p.get("edge").and_then(Value::as_u64).map_or(s.preview_prefs.standard_edge(), |e| e.clamp(256, 8192) as usize)),
        "full" | "1:1" => None,
        other => return Err(bad(C, format!("unknown size `{other}` (standard|full)"))),
    };
    if s.preview_build.as_ref().is_some_and(|b| !b.finished.load(Ordering::Relaxed)) {
        return Err(bad(C, "a preview build is already running"));
    }
    // 1:1 previews past their time go first (the build may need the room)
    auto_discard(s);
    // explicit ids, else the selection, else everything in view
    let ids = if p.get("ids").is_some() || p.get("id").is_some() || !s.selection.ids.is_empty() { s.targets(p) } else { s.visible_cloned() };
    let jobs: Vec<Vec<RenderJob>> = ids.iter().map(|id| jobs_for(s, *id, edge)).filter(|j| !j.is_empty()).collect();
    let state = Arc::new(PreviewBuild { total: jobs.len(), ..Default::default() });
    s.preview_build = Some(state.clone());
    let task = task_for(s, &state);
    let wait = p.get("wait").and_then(Value::as_bool).unwrap_or(false) || cfg!(target_arch = "wasm32");
    if wait {
        run_all(jobs, &state, &task);
        return Ok(state.json());
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        // the row stays while the worker runs (it is dropped with the closure, however that ends)
        let st = state.clone();
        std::thread::Builder::new()
            .name("lc-build-previews".into())
            .spawn(move || run_all(jobs, &st, &task))
            .map_err(|e| bad(C, format!("could not start: {e}")))?;
    }
    Ok(state.json())
}

/// What discarding 1:1 previews did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Discarded {
    /// 1:1 previews removed.
    pub removed: usize,
    /// 1:1 previews still kept (of the photos looked at).
    pub kept: usize,
}

/// Discard the 1:1 previews of `ids` (memory and disk): all of them, or with `older_than` only
/// those not built or shown for that long. A preview held only in memory (no library on disk) has
/// no age and is kept unless all go.
pub fn discard_full(s: &Session, ids: &[dac_catalog::PhotoId], older_than: Option<std::time::Duration>) -> Discarded {
    let cache = &s.media.rendered;
    let now = std::time::SystemTime::now();
    let mut d = Discarded::default();
    for id in ids {
        let Some(p) = s.catalog.photo(*id) else { continue };
        let key = Session::full_view_key(p);
        if !cache.contains(key) {
            continue;
        }
        let stale = match older_than {
            None => true,
            Some(age) => cache.disk().and_then(|disk| disk.modified(key)).and_then(|m| now.duration_since(m).ok()).is_some_and(|a| a >= age),
        };
        if stale && cache.remove(key) {
            d.removed += 1;
        } else {
            d.kept += 1;
        }
    }
    d
}

/// Apply the library's "discard 1:1 previews after" setting to every photo.
pub fn auto_discard(s: &Session) -> Discarded {
    let Some(age) = s.preview_prefs.discard_full.max_age() else { return Discarded::default() };
    let ids: Vec<_> = s.catalog.photos().map(|p| p.id).collect();
    discard_full(s, &ids, Some(age))
}

fn discard(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "library.discardPreviews";
    match str_param(p, "size").unwrap_or("full") {
        "full" | "1:1" => {}
        other => return Err(bad(C, format!("unknown size `{other}` (only full: standard previews go with library.clearPreviews)"))),
    }
    let d = if p.get("auto").and_then(Value::as_bool) == Some(true) {
        auto_discard(s)
    } else {
        let ids = if p.get("ids").is_some() || p.get("id").is_some() || !s.selection.ids.is_empty() { s.targets(p) } else { s.visible_cloned() };
        discard_full(s, &ids, None)
    };
    Ok(json!({"removed": d.removed, "kept": d.kept}))
}

fn settings(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "library.previewSettings";
    let mut next = s.preview_prefs;
    if let Some(v) = p.get("standardEdge") {
        let e = v.as_u64().filter(|e| (256..=8192).contains(e)).ok_or_else(|| bad(C, "standardEdge must be 256–8192 px"))?;
        next.standard_edge = e as u32;
    }
    if let Some(v) = p.get("discardFull") {
        next.discard_full = v.as_str().and_then(DiscardAfter::parse).ok_or_else(|| bad(C, "discardFull must be day, week, month or never"))?;
    }
    if let Some(v) = p.get("atImport") {
        next.at_import = v.as_str().and_then(ImportPreviews::parse).ok_or_else(|| bad(C, "atImport must be minimal, standard or full"))?;
    }
    if next != s.preview_prefs {
        s.preview_prefs = next;
        s.save_prefs()?;
    }
    Ok(s.preview_prefs.json())
}

/// The previews an import asks for ([`PreviewPrefs::at_import`]) of the photos it brought in,
/// built in the background like Build Previews. Errors are logged: the import itself is done.
pub fn build_after_import(s: &mut Session, ids: &[dac_catalog::PhotoId]) {
    let size = match s.preview_prefs.at_import {
        ImportPreviews::Minimal => return,
        ImportPreviews::Standard => "standard",
        ImportPreviews::Full => "full",
    };
    if ids.is_empty() {
        return;
    }
    let ids: Vec<u64> = ids.iter().map(|id| id.0).collect();
    if let Err(e) = build(s, &json!({"size": size, "ids": ids})) {
        log::warn!("previews at import: {e}");
    }
}

/// One photo's smart preview to build or discard: (photo, its file in the smart previews folder,
/// how to load the original at preview size, whether the photo takes the per-file camera look —
/// `camera_preview::file_local_look`, [`dac_catalog::Photo::relative_wb`]: only those
/// proxies go stale when the fit changes).
#[cfg(not(target_arch = "wasm32"))]
type SmartJob = (dac_catalog::PhotoId, std::path::PathBuf, Option<crate::media::SourceRef>, bool);

/// Held while one proxy is checked and (re)written, so Build Smart Previews and the opening look
/// refresh ([`refresh_stale_smart_previews`]), on their own threads, never write the same file at once.
#[cfg(not(target_arch = "wasm32"))]
static SMART_WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

/// `source` if it reads the photo's original, else `None`: a cached source decoded from a smart
/// preview (it has no decoder facts), a proxy standing in for an offline original, and the proxy
/// fallback of a file source are not the original.
#[cfg(not(target_arch = "wasm32"))]
fn original_only(source: crate::media::SourceRef) -> Option<crate::media::SourceRef> {
    use crate::media::SourceRef;
    match source {
        SourceRef::File { path, max_edge, loader, denoise, .. } => Some(SourceRef::File { path, max_edge, loader, fallback: None, denoise }),
        SourceRef::Loaded(d) if d.info.is_some() => Some(SourceRef::Loaded(d)),
        _ => None,
    }
}

/// The file-system half of Build / Discard Smart Previews (runs on a worker thread in the app).
/// `custom`: the folder was chosen (on another drive, perhaps): it must be there, it is never
/// recreated. With `state`, progress and cancel.
#[cfg(not(target_arch = "wasm32"))]
fn smart_run(
    dir: &std::path::Path,
    custom: bool,
    discard: bool,
    jobs: Vec<SmartJob>,
    state: Option<(&PreviewBuild, &TaskGuard)>,
) -> std::result::Result<SmartCounts, String> {
    let mut n = SmartCounts::default();
    if !discard {
        // a chosen folder on another drive must be there: never recreate it on this one
        if custom {
            crate::smart::check_writable(dir)?;
        } else {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
    }
    for (id, path, source, look) in jobs {
        if state.is_some_and(|(st, _)| st.cancel.load(Ordering::Relaxed)) {
            break;
        }
        let _one = SMART_WRITE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if discard {
            if std::fs::remove_file(&path).is_ok() {
                n.removed += 1;
            }
        } else {
            // a complete, current proxy is kept; a missing or damaged one (cut short by a crash or a
            // full drive) is (re)built, and so is one written by an older camera-look fit (see
            // `camera_preview::LOOK_VERSION`) when its original can be read
            let damaged = path.exists();
            let stale = look && damaged && crate::smart::is_stale(&path);
            if damaged && !stale && crate::smart::is_valid(&path) {
                n.built += 1;
            } else {
                // a stale proxy is rebuilt from the original only: never from itself (the file
                // fallback or a source decoded from a proxy would stamp the old look as current)
                let source = if stale { source.and_then(original_only) } else { source };
                // atomic: a failed write leaves no partial proxy that would pass for a built one;
                // and synced (issue #134): built so the photo can be edited while its original
                // is offline, when it is the only copy — the sync is small next to the decode
                let r = source
                    .ok_or_else(|| "nothing to build from".to_string())
                    .and_then(|s| s.load_source())
                    .and_then(|src| crate::smart::encode(&src.image, src.info_or(Default::default()).camera_tone.as_ref()))
                    .and_then(|b| dac_catalog::safe_file::write_atomic(&path, &b).map_err(|e| format!("{}: {e}", path.display())));
                match r {
                    Ok(()) => {
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
        if let Some((st, task)) = state {
            let done = st.done.fetch_add(1, Ordering::Relaxed).saturating_add(1);
            task.progress(done as u64, st.total as u64);
        }
    }
    Ok(n)
}

/// Smart previews: build (from the original, at preview size) or discard the proxies that keep
/// photos editable while their originals are offline. With `background` (the app) the files are
/// read and written on a worker thread; progress like Build Previews (`library.previewProgress`).
#[cfg(not(target_arch = "wasm32"))]
fn smart(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "library.smartPreviews";
    let dir = s.media.smart_dir.clone().ok_or_else(|| bad(C, "smart previews need a library on disk"))?;
    let discard = p.get("discard").and_then(Value::as_bool).unwrap_or(false);
    let background = p.get("background").and_then(Value::as_bool).unwrap_or(false);
    if background && s.preview_build.as_ref().is_some_and(|b| !b.finished.load(Ordering::Relaxed)) {
        return Err(bad(C, "a preview build is already running"));
    }
    let ids = if p.get("ids").is_some() || !s.selection.ids.is_empty() { s.targets(p) } else { s.visible_cloned() };
    let mut jobs: Vec<SmartJob> = Vec::new();
    for id in ids {
        let Some(ph) = s.catalog.photo(id).cloned() else { continue };
        if !matches!(ph.source, dac_catalog::Source::File { .. }) {
            continue;
        }
        let path = dir.join(crate::smart::file_name(&ph));
        let source = (!discard).then(|| s.media.source_ref(&ph, crate::media::SourceLevel::Preview));
        jobs.push((id, path, source, ph.relative_wb()));
    }
    let custom = s.smart_previews_dir.is_some();
    if background {
        let what = if discard { "discarding smart previews" } else { "smart previews" };
        let state = Arc::new(PreviewBuild { total: jobs.len(), what, ..Default::default() });
        s.preview_build = Some(state.clone());
        let task = task_for(s, &state);
        let st = state.clone();
        std::thread::Builder::new()
            .name("lc-smart-previews".into())
            .spawn(move || {
                if let Err(e) = smart_run(&dir, custom, discard, jobs, Some((&st, &task))) {
                    log::warn!("{C}: {e}");
                    *st.error.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(e);
                }
                st.finished.store(true, Ordering::Relaxed);
            })
            .map_err(|e| bad(C, format!("could not start: {e}")))?;
        return Ok(state.json());
    }
    let r = smart_run(&dir, custom, discard, jobs, None).map_err(|e| bad(C, e))?;
    Ok(
        json!({"built": r.built, "repaired": r.repaired, "refreshed": r.refreshed, "staleKept": r.stale_kept, "removed": r.removed, "failed": r.failed}),
    )
}

/// Bring smart previews written by an older camera-look fit up to date, once per fit version: when
/// the smart previews folder has not been checked at this `LOOK_VERSION` (its marker,
/// [`crate::smart::look_checked`]), a worker thread (the UI never waits on the drives) looks at the
/// proxies of the raws that take the per-file camera look, the only ones the fit reaches (JPEG,
/// PNG… proxies are never rebuilt), and rebuilds the stale ones from their originals. One whose
/// original can't be read (offline) is kept as it is, still marked stale in its header: Build
/// Smart Previews rebuilds it once the original is back. Only existing proxies are touched: none
/// is built, and a missing folder is never created. Returns the thread, if started.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn refresh_stale_smart_previews(s: &mut Session) -> Option<std::thread::JoinHandle<()>> {
    let dir = s.media.smart_dir.clone()?;
    if !dir.is_dir() || crate::smart::look_checked(&dir) {
        return None;
    }
    let max_edge = crate::media::SourceLevel::Preview.max_edge();
    let photos: Vec<_> = s.catalog.photos().filter(|p| matches!(p.source, dac_catalog::Source::File { .. }) && p.relative_wb()).collect();
    let jobs: Vec<SmartJob> =
        photos.iter().map(|p| (p.id, dir.join(crate::smart::file_name(p)), Some(s.media.origin_ref(&p.source, max_edge)), true)).collect();
    std::thread::Builder::new()
        .name("lc-smart-look".into())
        .spawn(move || refresh_stale(&dir, jobs))
        .map_err(|e| log::warn!("smart previews: could not start the look refresh: {e}"))
        .ok()
}

/// The body of [`refresh_stale_smart_previews`]: rebuild the stale proxies among `jobs`.
#[cfg(not(target_arch = "wasm32"))]
fn refresh_stale(dir: &std::path::Path, jobs: Vec<SmartJob>) {
    let stale: Vec<SmartJob> = jobs.into_iter().filter(|(_, path, _, _)| crate::smart::is_stale(path)).collect();
    if !stale.is_empty() {
        // `custom`: the folder is there already, and must not be recreated if it went away since
        match smart_run(dir, true, false, stale, None) {
            Ok(n) => log::info!("smart previews: {} refreshed to the current camera look, {} kept (original offline)", n.refreshed, n.stale_kept),
            Err(e) => {
                // not marked: the next opening tries again
                log::warn!("smart previews: look refresh: {e}");
                return;
            }
        }
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
    const C: &str = "library.smartPreviewsLocation";
    let current = s.media.smart_dir.clone().ok_or_else(|| bad(C, "smart previews need a library on disk"))?;
    let reset = p.get("reset").and_then(Value::as_bool).unwrap_or(false);
    let chosen = str_param(p, "path").filter(|x| !x.trim().is_empty());
    if !reset && chosen.is_none() {
        return Ok(location_json(s));
    }
    let lib_dir = s.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.clone()).ok_or_else(|| bad(C, "smart previews need a library on disk"))?;
    let default = crate::smart::dir(&lib_dir);
    let (new_dir, custom) = match chosen {
        Some(x) if !reset => {
            let d = std::path::PathBuf::from(x.trim());
            if !d.is_absolute() {
                return Err(bad(C, "path must be an absolute folder path"));
            }
            (d.clone(), (d != default).then_some(d))
        }
        _ => (default, None),
    };
    if new_dir == current {
        return Ok(location_json(s));
    }
    // the new place must work before anything moves; there is no fallback to another drive
    crate::smart::check_writable(&new_dir).map_err(|e| bad(C, e))?;
    let (held, _) = crate::smart::stats(&current);
    let existing = match str_param(p, "existing") {
        Some(x) => Some(crate::smart::Existing::parse(x).ok_or_else(|| bad(C, "existing must be move, leave or discard"))?),
        None if held == 0 => Some(crate::smart::Existing::Leave),
        None => None,
    };
    let Some(existing) = existing else {
        return Err(bad(C, format!("{held} smart preview(s) are in {}: pass existing = move, leave or discard", current.display())));
    };
    let (moved, failed) = crate::smart::migrate(&current, &new_dir, existing);
    s.smart_previews_dir = custom;
    s.media.smart_dir = Some(new_dir);
    s.save_prefs()?;
    let mut r = location_json(s);
    r["handled"] = json!(moved);
    r["failed"] = json!(failed);
    Ok(r)
}

/// Whether photo `id` has a smart preview.
pub fn has_smart_preview(s: &Session, id: dac_catalog::PhotoId) -> bool {
    match (&s.media.smart_dir, s.catalog.photo(id)) {
        (Some(dir), Some(p)) => crate::smart::is_valid(&dir.join(crate::smart::file_name(p))),
        _ => false,
    }
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        #[cfg(not(target_arch = "wasm32"))]
        cmd!(
            "library.smartPreviews",
            "Build Smart Previews",
            [],
            None,
            "{ids?, discard?: bool, background?: bool} — build (or discard) the smart previews of the selected photos (else all in view): compact proxies in the library that keep photos editable and exportable (at proxy size) while their originals are offline; a damaged proxy (cut short) is rebuilt → {built, repaired, removed, failed}; with background (the app's menu) the files are read and written on a worker thread → {total, done, failed, repaired, running, what} like library.buildPreviews (progress: library.previewProgress, stop: library.cancelPreviews)",
            always,
            smart
        ),
        #[cfg(not(target_arch = "wasm32"))]
        cmd!(
            "library.smartPreviewsLocation",
            "Smart Previews Location",
            [],
            None,
            "{path?: absolute folder, reset?: bool, existing?: move|leave|discard} — without path/reset: where this library keeps its smart previews → {path, default, custom, available, count, bytes}. With path (or reset = back to `Smart Previews` in the library): use that folder instead (must exist or have an existing parent, and accept writes; never falls back to another drive). Smart previews already in the old folder need `existing`: move them, leave them (build again), or discard them → also {handled, failed}",
            always,
            smart_location
        ),
        cmd!(query "photo.smartPreview", "Smart Preview Status", [], None, "{id?} → {smartPreview: bool, originalOnline: bool}", always, |s, p| {
            let id = s.targets(p).first().copied().ok_or_else(|| bad("photo.smartPreview", "no photo"))?;
            let online = match s.catalog.photo(id).map(|p| p.source.clone()) {
                Some(dac_catalog::Source::File { path }) => std::path::Path::new(&path).exists(),
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
        cmd!(
            "library.discardPreviews",
            "Discard 1:1 Previews",
            [],
            None,
            "{size?: full, ids?, auto?: bool} — discard the 1:1 previews of the selected photos (else all in view), or with auto those past the library's discardFull age (library.previewSettings); standard previews stay → {removed, kept}",
            always,
            discard
        ),
        cmd!(
            "library.previewSettings",
            "Preview Settings",
            [],
            None,
            "{standardEdge?: 256–8192 px, discardFull?: day|week|month|never, atImport?: minimal|standard|full} — the library's preview store: the size of standard previews, when 1:1 previews are discarded on their own, and which previews an import builds (saved with the library) → {standardEdge, discardFull, atImport}",
            always,
            settings
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
