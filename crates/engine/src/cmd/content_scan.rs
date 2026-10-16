//! Indexing and searching photo contents, the session side: which model is in use, the index of it for the open
//! library ([`crate::content_index`]), the background scan ([`crate::content_worker`]) and the searches over it.
//!
//! `content.pump` drives the scan: cheap enough to call every frame (the desktop app does, a few times a second), and
//! called by the other `content.*` commands too, so headless sessions (CLI, MCP) index as they are used;
//! `content.pump {wait: true}` runs the scan to its end. It takes in what the worker finished, writes the index every
//! few seconds, makes the list of photos to do when the catalog changed, hands the worker the next batches and keeps the
//! row in the activity stack.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use lightcraft_catalog::{MediaKind, Photo, PhotoId, Source};
use lightcraft_develop::DevelopSettings;
use lightcraft_preview::Hasher128;
use serde_json::{Value, json};

use super::bad;
use crate::content_index::{Hash, Index, file_path};
use crate::content_models::{self as cm, Found, read_settings};
use crate::content_query::{ContentAsk, ContentHint, ContentState, ContentView};
use crate::content_worker::{Batch, Counts, Current, EDGE, Item, Outcome, Pace, TextVector, TextWorker, Worker};
use crate::{Result, Session};

/// How long a look at the settings is trusted by the per-frame pump.
const RECHECK: Duration = Duration::from_secs(1);
/// How often new embeddings are written to the index file (appending is cheap; the rest is written on stop and quit).
const SAVE_EVERY: Duration = Duration::from_secs(3);
/// After the catalog changes, the list of photos to do is made again at most this often.
const REQUEUE_EVERY: Duration = Duration::from_secs(2);
/// Photos whose file was missing are tried again after this.
const OFFLINE_RETRY: Duration = Duration::from_secs(60);
/// The text tower is unloaded after this long without a search.
const TEXT_IDLE: Duration = Duration::from_secs(300);
/// Batches waiting behind the one running, so the worker never waits for the next frame.
const PREFETCH: usize = 1;
/// How long a command that answers at once waits for the search model (loading it takes about half a second).
const TEXT_WAIT: Duration = Duration::from_secs(120);
/// Photos shown as "best matches" when none reaches the cut-off (at the default step; More doubles it).
const BEST_MATCHES: usize = 200;
/// Photos shown by Find Similar Contents (at the default step; More doubles it).
const SIMILAR_MATCHES: usize = 100;
/// Most results a search or a find-similar returns.
pub(crate) const MAX_RESULTS: usize = 10_000;

/// The photo's content hash as the index keeps it: a digest of its content key (its file's hash, or its source).
pub(crate) fn photo_hash(p: &Photo) -> Hash {
    Hasher128::new().str(&crate::media::content_key(p)).finish().0.to_le_bytes()
}

/// Whether a photo is indexed at all: in the library and not a video.
fn eligible(p: &Photo) -> bool {
    p.in_library() && p.kind != MediaKind::Video
}

fn library_dir(s: &Session) -> Option<PathBuf> {
    s.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.clone())
}

/// The selected model when it is installed, with its tag.
fn installed_current(dir: &std::path::Path, id: &str) -> Option<Current> {
    let known = cm::known(&cm::read_catalog(dir));
    let (disk, _) = cm::on_disk(dir, &known);
    let d = disk.into_iter().find(|d| d.manifest.id == id && d.state == Found::Installed)?;
    let m = d.manifest;
    let sha = m.file(lightcraft_vision::siglip::WEIGHTS_FILE).or(m.files.first()).map(|f| f.sha256.clone()).unwrap_or_default();
    Some(Current { id: m.id.clone(), tag: format!("{}@{sha}", m.id), home: dir.join(&m.id), manifest: m })
}

/// Look at the settings again (at most once a second unless `fresh`): the model in use, whether indexing is on, the
/// pace. A different model stops the scan and drops the old model's index (it stays in its file).
pub(crate) fn refresh(s: &mut Session, fresh: bool) {
    if !fresh && s.content.checked.is_some_and(|t| t.elapsed() < RECHECK) {
        return;
    }
    s.content.checked = Some(Instant::now());
    let (current, enabled, pace) = match s.content_models_dir.clone() {
        Some(dir) => {
            let st = read_settings(&dir);
            let current = st.model.as_deref().and_then(|id| installed_current(&dir, id));
            let pace = st.pace.as_deref().and_then(Pace::parse).unwrap_or_default();
            s.content.tag_parent = st.tag_parent.clone();
            s.content.tag_threshold = st.tag_threshold;
            (current, st.enabled, pace)
        }
        None => (None, false, Pace::default()),
    };
    s.content.pace = pace;
    s.content.enabled = enabled && current.is_some();
    let old = s.content.current.as_ref().map(|c| c.tag.clone());
    let new = current.as_ref().map(|c| c.tag.clone());
    if old != new {
        let st = &mut s.content;
        st.stop();
        st.index = None;
        st.failed.clear();
        st.offline.clear();
        st.force.clear();
        st.held = false;
        st.cancel.store(false, Ordering::SeqCst);
        st.counts = Counts::default();
        st.counts_stamp = None;
        st.ms_per_photo = None;
        st.device = None;
        st.gpu_fallback = None;
        if st.text.as_ref().is_some_and(|t| Some(&t.tag) != new.as_ref()) {
            st.text = None;
        }
    }
    s.content.current = current;
}

/// The index of the model in use for the open library, opened (read from its file) when it is not yet.
pub(crate) fn ensure_index(s: &mut Session) -> bool {
    let Some(cur) = s.content.current.clone() else {
        s.content.index = None;
        return false;
    };
    let path = library_dir(s).map(|d| file_path(&d, &cur.id));
    if s.content.index.as_ref().is_some_and(|i| i.tag() == cur.tag && i.path() == path.as_deref()) {
        return true;
    }
    // (the old one is written when dropped)
    s.content.index = None;
    s.content.index = Some(Index::open(&cur.tag, cur.manifest.embedding.dim as usize, path));
    s.content.queue_stamp = None;
    s.content.queue.clear();
    true
}

/// Go through the library: the photos to do (newest last, as the queue is taken from the end) and the counts. Rows of
/// photos no longer in the library are dropped from the index.
fn survey(s: &mut Session, make_queue: bool) -> Vec<PhotoId> {
    let st = &s.content;
    let Some(index) = st.index.as_ref() else { return Vec::new() };
    let (mut queue, mut counts, mut live) = (Vec::new(), Counts::default(), HashSet::new());
    for p in s.catalog.photos().filter(|p| eligible(p)) {
        live.insert(p.id.0);
        counts.total += 1;
        let h = photo_hash(p);
        match index.hash(p.id.0) {
            Some(x) if x == h => {
                counts.indexed += 1;
                if !st.force.contains(&p.id.0) {
                    continue;
                }
            }
            Some(_) => counts.stale += 1,
            None => {}
        }
        if !make_queue || st.in_flight.contains(&p.id) || st.offline.contains(&p.id.0) || st.failed.get(&p.id.0).is_some_and(|(fh, _)| *fh == h) {
            continue;
        }
        queue.push(p.id);
    }
    let gone: Vec<u64> = index.entries().map(|(id, _)| id).filter(|id| !live.contains(id)).collect();
    let st = &mut s.content;
    if let Some(index) = st.index.as_mut() {
        for id in gone {
            index.remove(id);
        }
    }
    st.failed.retain(|id, _| live.contains(id));
    st.force.retain(|id| live.contains(id));
    st.counts = counts;
    st.counts_stamp = st.index.as_ref().map(|i| (s.catalog.revision, i.len()));
    // the newest photos (the highest ids) first
    queue.sort_by_key(|id| id.0);
    queue
}

/// One photo, ready for the worker; `None` when it is not to be indexed now (gone, a video, its file offline).
fn prepare(s: &mut Session, id: PhotoId) -> Option<Item> {
    let p = s.catalog.photo(id).filter(|p| eligible(p))?.clone();
    let path = match &p.source {
        Source::File { path } => Some(path.clone()),
        _ => None,
    };
    if let Some(path) = &path
        && s.media.availability.is_offline(path)
    {
        s.content.offline.insert(id.0);
        s.content.offline_at.get_or_insert_with(Instant::now);
        return None;
    }
    let preview = s.scan_preview_of(&p);
    // the unedited look, upright and uncropped; not written to the thumbnail cache (it is only read once)
    let job = s.variant_job(id, &DevelopSettings::default(), EDGE).map(|mut j| {
        j.cache = None;
        crate::content_worker::render(j)
    });
    Some(Item { id, hash: photo_hash(&p), preview, job, path })
}

/// Take in what the worker finished. Returns the photos embedded.
fn collect(s: &mut Session) -> usize {
    let Some(w) = s.content.worker.as_mut() else { return 0 };
    let finished = w.finished();
    let tag = s.content.current.as_ref().map(|c| c.tag.clone()).unwrap_or_default();
    let mut embedded = 0;
    for done in finished {
        if let Some(e) = done.model_error {
            log::warn!("content index: the model could not be loaded: {e}");
            s.content.error = Some((tag.clone(), format!("the content model could not be loaded: {e}")));
        }
        if let Some(d) = done.device {
            s.content.device = Some(d);
        }
        if let Some(why) = done.gpu_fallback
            && s.content.gpu_fallback.is_none()
        {
            log::warn!("content index: the GPU stopped running the model ({why}); the CPU indexes from now on");
            s.content.gpu_fallback = Some(why);
            // the batch was embedded on the CPU: the GPU's speed is no guide to what is left
            s.content.ms_per_photo = None;
        }
        if done.embedded > 0 {
            let per = done.ms / done.results.len().max(1) as f64;
            s.content.ms_per_photo = Some(s.content.ms_per_photo.map_or(per, |old| old * 0.7 + per * 0.3));
        }
        for (id, hash, outcome) in done.results {
            s.content.in_flight.remove(&id);
            // deleted, or its original replaced, while it was being embedded: the next survey sees to it
            let now = s.catalog.photo(id).filter(|p| eligible(p)).map(|p| photo_hash(p));
            if now != Some(hash) {
                continue;
            }
            match outcome {
                Outcome::Embedded(v) => {
                    let st = &mut s.content;
                    let Some(index) = st.index.as_mut() else { continue };
                    let before = index.hash(id.0);
                    match index.insert(id.0, hash, &v) {
                        Ok(()) => {
                            embedded += 1;
                            st.embedded += 1;
                            st.force.remove(&id.0);
                            st.failed.remove(&id.0);
                            match before {
                                None => st.counts.indexed += 1,
                                Some(b) if b != hash => {
                                    st.counts.indexed += 1;
                                    st.counts.stale = st.counts.stale.saturating_sub(1);
                                }
                                Some(_) => {}
                            }
                        }
                        Err(e) => {
                            // full (or a vector of the wrong size): stop, and say why
                            st.error = Some((tag.clone(), e));
                        }
                    }
                }
                Outcome::Offline => {
                    s.content.offline.insert(id.0);
                    s.content.offline_at.get_or_insert_with(Instant::now);
                }
                Outcome::Failed(why) => {
                    log::info!("content index: photo {} could not be indexed: {why}", id.0);
                    s.content.failed.insert(id.0, (hash, why));
                }
                Outcome::Skipped => {}
            }
        }
    }
    embedded
}

/// Keep the row in the activity stack while photos are left: indexed of all.
fn track(s: &mut Session) {
    let st = &mut s.content;
    let pending = st.queue.len() + st.in_flight.len();
    // a cancelled scan keeps its row until the batch it was on is in
    if pending == 0 || (st.held && st.in_flight.is_empty()) {
        st.task = None;
        return;
    }
    let cancel = st.cancel.clone();
    let task = st.task.get_or_insert_with(|| {
        let t = s.activity.start("content", "Indexing photo contents", crate::activity::Cancel::Flag(cancel));
        // quitting loses nothing (what is done is saved; the rest is done next time): no question on quit
        t.set_resumable(true);
        t
    });
    task.progress(st.counts.indexed.min(st.counts.total) as u64, st.counts.total as u64);
}

/// One step of the scan (see the module). `pause`: the user is busy (dragging, typing): no new batch is started.
pub(crate) fn step(s: &mut Session, pause: bool) {
    super::content::settle(s);
    refresh(s, false);
    let st = &mut s.content;
    if let Some(t) = st.text.as_mut() {
        t.poll();
    }
    // (not while a search by contents is in view: it may be asked again any moment)
    if st.text.as_ref().is_some_and(|t| t.used.elapsed() >= TEXT_IDLE) && s.content_query.view.is_none() {
        s.content.text = None;
    }
    // auto-tags follow the index (also when indexing is off: the index is still there)
    super::content_tags::step(s);
    let st = &mut s.content;
    let tag = st.current.as_ref().map(|c| c.tag.clone());
    let failed = st.error.as_ref().is_some_and(|(t, _)| Some(t) == tag.as_ref());
    if !st.enabled || failed {
        if st.worker.is_some() || st.task.is_some() {
            st.stop();
        }
        return;
    }
    ensure_index(s);
    collect(s);
    let st = &mut s.content;
    if st.error.as_ref().is_some_and(|(t, _)| Some(t) == tag.as_ref()) {
        st.stop();
        return;
    }
    if st.cancel.load(Ordering::SeqCst) && !st.held {
        // cancelled: what is running finishes and is kept; nothing new starts until indexing is started again
        st.held = true;
        st.queue.clear();
    }
    if st.index.as_ref().is_some_and(Index::has_unsaved) && st.saved_at.is_none_or(|t| t.elapsed() >= SAVE_EVERY) {
        st.save();
    }
    if st.held {
        if st.in_flight.is_empty() {
            st.worker = None;
        }
        track(s);
        return;
    }
    if st.offline_at.is_some_and(|t| t.elapsed() >= OFFLINE_RETRY) {
        st.offline.clear();
        st.offline_at = None;
        st.queue_stamp = None;
    }
    let revision = s.catalog.revision;
    let st = &s.content;
    let changed = st.queue_stamp != Some(revision);
    let due = st.queue.is_empty() && st.in_flight.is_empty() || st.queue_at.is_none_or(|t| t.elapsed() >= REQUEUE_EVERY);
    if changed && due {
        let queue = survey(s, true);
        let st = &mut s.content;
        st.queue = queue;
        st.queue_stamp = Some(revision);
        st.queue_at = Some(Instant::now());
    }
    if !pause {
        submit(s);
    }
    track(s);
}

/// Hand the worker batches until it has one running and [`PREFETCH`] waiting.
fn submit(s: &mut Session) {
    let pace = s.content.pace;
    loop {
        let st = &s.content;
        if st.queue.is_empty() || st.worker.as_ref().is_some_and(|w| w.busy > PREFETCH) {
            return;
        }
        if st.worker.is_none() {
            let Some(cur) = st.current.clone() else { return };
            match Worker::start(cur.home.clone(), cur.manifest.clone(), st.loader(), st.cancel.clone()) {
                Ok(w) => s.content.worker = Some(w),
                Err(e) => {
                    s.content.error = Some((cur.tag, e));
                    return;
                }
            }
        }
        let mut items = Vec::new();
        while items.len() < pace.batch() {
            let Some(id) = s.content.queue.pop() else { break };
            if let Some(item) = prepare(s, id) {
                items.push(item);
            }
        }
        if items.is_empty() {
            return;
        }
        let ids: Vec<PhotoId> = items.iter().map(|i| i.id).collect();
        let Some(w) = s.content.worker.as_mut() else { return };
        if !w.submit(Batch { items, pace }) {
            // the thread is gone: start another next time
            s.content.worker = None;
            s.content.queue_stamp = None;
            return;
        }
        s.content.in_flight.extend(ids);
    }
}

/// Photos left to do.
pub(crate) fn pending(s: &Session) -> usize {
    s.content.queue.len() + s.content.in_flight.len()
}

/// Whether the scan is working (or about to).
pub(crate) fn indexing(s: &Session) -> bool {
    s.content.enabled && !s.content.held && pending(s) > 0
}

/// Make the counts current without starting anything (indexing may be off).
fn recount(s: &mut Session) {
    refresh(s, false);
    if !ensure_index(s) || s.content.worker.is_some() {
        return;
    }
    let stamp = s.content.index.as_ref().map(|i| (s.catalog.revision, i.len()));
    if stamp != s.content.counts_stamp {
        survey(s, false);
    }
}

/// What `content.status` and the pump report about the index.
pub(crate) fn report(s: &mut Session) -> Value {
    recount(s);
    let tags = super::content_tags::report(s);
    let st = &s.content;
    let pending = pending(s);
    let eta = st.ms_per_photo.filter(|_| indexing(s)).map(|ms| (ms * pending as f64 / 1000.0).round());
    let index = st.index.as_ref().map(|i| {
        json!({
            "file": i.path().map(|p| p.display().to_string()),
            "bytes": i.path().and_then(|p| std::fs::metadata(p).ok()).map(|m| m.len()),
            "photos": i.len(),
            "memoryBytes": i.matrix_bytes(),
            "records": i.file_records().0,
            "deadRecords": i.file_records().1,
            "note": i.note,
            "saveError": i.save_error,
        })
    });
    let has_index = st.index.is_some();
    json!({
        "indexed": has_index.then_some(st.counts.indexed),
        "total": has_index.then_some(st.counts.total),
        "stale": has_index.then_some(st.counts.stale),
        "failed": st.failed.len(),
        "offline": st.offline.len(),
        "pending": pending,
        "indexing": indexing(s),
        "stopped": st.held,
        "pace": st.pace.name(),
        "msPerPhoto": st.ms_per_photo.map(|v| v.round()),
        "device": st.device,
        "textDevice": st.text.as_ref().and_then(|t| t.model.as_ref()).map(|m| m.runs_on()),
        "gpuFallback": st.gpu_fallback.clone().or_else(|| st.text.as_ref().and_then(|t| t.model.as_ref()).and_then(|m| m.gpu_fallback())),
        "etaSeconds": eta,
        "embeddedThisSession": st.embedded,
        "error": st.error.as_ref().filter(|(t, _)| st.current.as_ref().is_some_and(|c| &c.tag == t)).map(|(_, e)| e.clone()),
        "index": index,
        "tags": tags,
    })
}

/// `content.pump {pause?, wait?, timeoutMs?}`.
pub(crate) fn pump(s: &mut Session, p: &Value) -> Result<Value> {
    let pause = p.get("pause").and_then(Value::as_bool) == Some(true);
    if p.get("wait").and_then(Value::as_bool) == Some(true) {
        let timeout = Duration::from_millis(p.get("timeoutMs").and_then(Value::as_u64).unwrap_or(600_000).min(86_400_000));
        let started = Instant::now();
        loop {
            step(s, false);
            if !indexing(s) && s.content.in_flight.is_empty() {
                break;
            }
            if started.elapsed() >= timeout {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        // what was done is written before answering
        s.content.save();
    } else {
        step(s, pause);
    }
    let st = &s.content;
    Ok(json!({
        "active": st.enabled && st.error.is_none(),
        "indexing": indexing(s),
        "pending": pending(s),
        "inFlight": st.in_flight.len(),
        "indexed": st.counts.indexed,
        "total": st.counts.total,
        "stopped": st.held,
    }))
}

/// Start again after a cancel (or an error): `content.enable {on: true}` and `content.reindex` do this.
pub(crate) fn resume(s: &mut Session) {
    let st = &mut s.content;
    st.held = false;
    st.cancel.store(false, Ordering::SeqCst);
    st.error = None;
    st.queue_stamp = None;
    st.checked = None;
}

/// `content.reindex {ids?}`: embed these photos (or every photo) again, and try the ones that failed again.
pub(crate) fn reindex(s: &mut Session, ids: Option<Vec<u64>>) -> usize {
    refresh(s, true);
    ensure_index(s);
    let all: Vec<u64> = s.catalog.photos().filter(|p| eligible(p)).map(|p| p.id.0).collect();
    let chosen: Vec<u64> = match ids {
        Some(ids) => ids.into_iter().filter(|id| all.contains(id)).collect(),
        None => all,
    };
    let st = &mut s.content;
    for id in &chosen {
        st.failed.remove(id);
        st.offline.remove(id);
        st.force.insert(*id);
    }
    resume(s);
    chosen.len()
}

/// `content.clearIndex`: forget the model's index and delete its file; indexing (if on) starts over.
pub(crate) fn clear(s: &mut Session) -> Result<Value> {
    const C: &str = "content.clearIndex";
    refresh(s, true);
    if !ensure_index(s) {
        return Err(bad(C, "no content model is in use"));
    }
    s.content.stop();
    let file = s.content.index.as_ref().and_then(|i| i.path().map(|p| p.display().to_string()));
    if let Some(i) = s.content.index.as_mut() {
        i.clear().map_err(|e| bad(C, format!("could not delete the index file: {e}")))?;
    }
    s.content.failed.clear();
    s.content.offline.clear();
    s.content.force.clear();
    s.content.counts = Counts::default();
    s.content.counts_stamp = None;
    resume(s);
    Ok(json!({"cleared": true, "file": file}))
}

/// The text thread of the model in use, started (the model loads on it) when there is none yet. `None`: no model is in
/// use, or the thread could not be started (`Err`).
pub(crate) fn text_worker(s: &mut Session) -> std::result::Result<Option<&mut TextWorker>, String> {
    let Some(cur) = s.content.current.clone() else { return Ok(None) };
    if s.content.text.as_ref().is_none_or(|t| t.tag != cur.tag) {
        let loader = s.content.loader();
        s.content.text = Some(TextWorker::start(cur.tag, cur.home, cur.manifest, loader)?);
    }
    Ok(s.content.text.as_mut())
}

/// Start loading the search model of the model in use (the Contents search mode was chosen): `content.prepare`.
pub(crate) fn prepare_search(s: &mut Session) -> Value {
    refresh(s, false);
    let state = match text_worker(s) {
        Ok(Some(w)) => {
            w.poll();
            w.used = Instant::now();
            match (&w.model, &w.error) {
                (Some(_), _) => json!({"state": "ready"}),
                (None, Some(e)) => json!({"state": "error", "error": e}),
                (None, None) => json!({"state": "loading"}),
            }
        }
        Ok(None) => json!({"state": "noModel"}),
        Err(e) => json!({"state": "error", "error": e}),
    };
    let mut out = coverage(s);
    if let (Some(o), Some(st)) = (out.as_object_mut(), state.as_object()) {
        o.extend(st.clone());
    }
    out
}

/// The embedding of `text` with the model in use, waiting for it.
fn text_vector(s: &mut Session, c: &str, text: &str) -> Result<(Vec<f32>, Arc<dyn lightcraft_vision::ContentModel>)> {
    let w = text_worker(s).map_err(|e| bad(c, e))?.ok_or_else(|| bad(c, "no content model is in use: choose one in Settings ▸ Search"))?;
    match w.wait(text, TEXT_WAIT) {
        TextVector::Ready(v) => {
            let model = w.model.clone().ok_or_else(|| bad(c, "the search model is not loaded"))?;
            Ok((v, model))
        }
        TextVector::Failed(e) => Err(bad(c, e)),
        TextVector::Pending => Err(bad(c, "the search model took too long")),
    }
}

/// How much of the library the answers come from.
fn coverage(s: &mut Session) -> Value {
    let pending = pending(s);
    let st = &s.content;
    json!({
        "model": st.current.as_ref().map(|c| c.id.clone()),
        "indexed": st.counts.indexed,
        "total": st.counts.total,
        "complete": st.counts.total > 0 && st.counts.indexed >= st.counts.total && pending == 0,
        "indexing": indexing(s),
    })
}

fn ranked(s: &Session, scored: Vec<(u64, f32)>, min: f32, limit: usize, skip: Option<u64>) -> Vec<Value> {
    let mut scored: Vec<(u64, f32)> = scored
        .into_iter()
        .filter(|(id, score)| *score >= min && Some(*id) != skip && s.catalog.photo(PhotoId(*id)).is_some_and(|p| eligible(p)))
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    scored.truncate(limit);
    scored.into_iter().map(|(id, score)| json!({"id": id, "score": (f64::from(score) * 1e4).round() / 1e4})).collect()
}

/// `content.search {text, limit?, min?}`.
pub(crate) fn search(s: &mut Session, text: &str, limit: usize, min: f32) -> Result<Value> {
    const C: &str = "content.search";
    let started = Instant::now();
    step(s, false);
    refresh(s, true);
    if !ensure_index(s) {
        return Err(bad(C, "no content model is in use: choose one in Settings ▸ Search"));
    }
    recount(s);
    let key: String = text.trim().chars().take(crate::content_query::MAX_QUERY_CHARS).collect();
    let (query, model) = text_vector(s, C, &key)?;
    let cosines = s.content.index.as_ref().map(|i| i.cosines(&query)).unwrap_or_default();
    let scored: Vec<(u64, f32)> = cosines.into_iter().map(|(id, c)| (id, model.score_cosine(c))).collect();
    let results = ranked(s, scored, min, limit, None);
    let mut out = coverage(s);
    out["text"] = json!(key);
    out["results"] = json!(results);
    out["ms"] = json!((started.elapsed().as_secs_f64() * 1e3).round());
    Ok(out)
}

/// `content.similar {id, limit?}`: photos whose embedding is closest to this photo's (cosine similarity).
pub(crate) fn similar(s: &mut Session, id: u64, limit: usize, min: f32) -> Result<Value> {
    const C: &str = "content.similar";
    step(s, false);
    refresh(s, true);
    if !ensure_index(s) {
        return Err(bad(C, "no content model is in use: choose one in Settings ▸ Search"));
    }
    if s.catalog.photo(PhotoId(id)).is_none() {
        return Err(bad(C, "no such photo"));
    }
    let Some(v) = s.content.index.as_ref().and_then(|i| i.vector(id)) else {
        return Err(bad(C, "that photo is not indexed yet: switch on Index photo contents in Settings ▸ Search, or wait for it"));
    };
    recount(s);
    let scored = s.content.index.as_ref().map(|i| i.cosines(&v)).unwrap_or_default();
    let results = ranked(s, scored, min, limit, Some(id));
    let mut out = coverage(s);
    out["id"] = json!(id);
    out["results"] = json!(results);
    Ok(out)
}

/// What the search bar says about searching by contents, given whether a model is in use (see `Session::content_hint`).
fn hint_for(s: &Session, has_model: bool) -> Option<ContentHint> {
    let (indexed, total, enabled) = (s.content.counts.indexed, s.content.counts.total, s.content.enabled);
    if !has_model || (!enabled && indexed < total) || (total == 0 && !enabled) {
        Some(ContentHint::TurnOnIndexing)
    } else if indexed < total {
        Some(ContentHint::Partial { percent: (indexed.saturating_mul(100) / total.max(1)).min(99) as u32 })
    } else {
        None
    }
}

/// `Session::content_hint`.
pub(crate) fn hint(s: &mut Session) -> Option<ContentHint> {
    refresh(s, false);
    let has_model = ensure_index(s);
    if has_model {
        recount(s);
    }
    hint_for(s, has_model)
}

/// The cut-off of a text search `step`s from the model's own (More: each step halves it; Fewer: doubles it).
fn cutoff(base: f32, step: i32) -> f32 {
    (base * 2f32.powi(-step.clamp(crate::content_query::MIN_STEP, crate::content_query::MAX_STEP))).clamp(1e-6, 1.0)
}

/// How many photos a list of best matches holds at `step`.
fn best_count(base: usize, step: i32) -> usize {
    let step = step.clamp(crate::content_query::MIN_STEP, crate::content_query::MAX_STEP);
    let n = if step >= 0 { base.saturating_mul(1 << step) } else { base >> (-step) };
    n.clamp(10, MAX_RESULTS)
}

/// Work out the ranking of the library view's search by contents (`Session::content_query`), when what it depends on
/// changed: the model in use, the question, More / Fewer, the index. While the text is being read the last ranking of
/// the same model stays (typing does not blank the grid); a problem shows nothing (never a stale ranking).
pub(crate) fn rank(s: &mut Session, ask: ContentAsk) {
    refresh(s, false);
    let label = s.content_label(&ask);
    let step = s.content_query.step;
    let has_model = ensure_index(s);
    if has_model {
        recount(s);
    }
    let (indexed, total) = (s.content.counts.indexed, s.content.counts.total);
    let hint = hint_for(s, has_model);
    let mut view = ContentView {
        ask: ask.clone(),
        label,
        state: ContentState::Ready,
        matches: 0,
        best_matches: false,
        step,
        can_more: false,
        can_fewer: false,
        min: None,
        indexed,
        total,
        hint,
    };
    let problem = |s: &mut Session, mut view: ContentView, why: String| {
        let key = format!("problem|{why}|{:?}", view.ask);
        if s.content_query.key.as_deref() != Some(key.as_str()) {
            s.content_query.set_ranked(Some(lightcraft_catalog::Ranked::default()), Some(key));
        }
        view.state = ContentState::Problem(why);
        s.content_query.view = Some(view);
    };
    let Some(cur) = s.content.current.clone().filter(|_| has_model) else {
        return problem(s, view, "No content model is in use: turn on Index photo contents in Settings → Search".into());
    };
    let (index_len, index_version, index_path) =
        s.content.index.as_ref().map(|i| (i.len(), i.version(), i.path().map(|p| p.display().to_string()))).unwrap_or_default();
    // what the ranking depends on, but the query's vector
    let key = format!("{}|{ask:?}|{step}|{index_len}|{index_version}|{index_path:?}", cur.tag);
    let same_model = s.content_query.key.as_deref().is_some_and(|k| k.starts_with(&format!("{}|", cur.tag)));
    // the vector to rank by, and how to score a cosine
    let (query, model): (Vec<f32>, Option<Arc<dyn lightcraft_vision::ContentModel>>) = match &ask {
        ContentAsk::Text(text) => {
            if s.content_query.key.as_deref() == Some(key.as_str()) {
                // nothing changed: keep it (and its view's counts)
                if let Some(v) = s.content_query.view.as_mut() {
                    v.indexed = indexed;
                    v.total = total;
                    v.hint = view.hint.clone();
                }
                return;
            }
            let w = match text_worker(s) {
                Ok(Some(w)) => w,
                Ok(None) => return problem(s, view, "No content model is in use".into()),
                Err(e) => return problem(s, view, e),
            };
            match w.vector(text) {
                TextVector::Ready(v) => (v, w.model.clone()),
                TextVector::Failed(e) => return problem(s, view, e),
                TextVector::Pending => {
                    view.state = ContentState::Preparing;
                    if !same_model {
                        // another model's ranking (or a problem's) is not shown while this one is prepared
                        s.content_query.set_ranked(Some(lightcraft_catalog::Ranked::default()), Some(format!("{}|preparing", cur.tag)));
                    }
                    view.matches = s.content_query.ranked.as_ref().map_or(0, |r| r.ids.len());
                    s.content_query.view = Some(view);
                    return;
                }
            }
        }
        ContentAsk::Similar(id) => {
            if s.catalog.photo(*id).is_none_or(|p| p.deleted) {
                return problem(s, view, "That photo is no longer in the library".into());
            }
            if s.content_query.key.as_deref() == Some(key.as_str()) {
                if let Some(v) = s.content_query.view.as_mut() {
                    v.indexed = indexed;
                    v.total = total;
                    v.hint = view.hint.clone();
                }
                return;
            }
            match s.content.index.as_ref().and_then(|i| i.vector(id.0)) {
                Some(v) => (v, None),
                None => return problem(s, view, "That photo is not indexed yet".into()),
            }
        }
    };
    let cosines = s.content.index.as_ref().map(|i| i.cosines(&query)).unwrap_or_default();
    let mut scored: Vec<(PhotoId, f32)> = cosines
        .into_iter()
        .filter(|(id, _)| s.catalog.photo(PhotoId(*id)).is_some_and(|p| eligible(p)))
        .map(|(id, c)| (PhotoId(id), model.as_ref().map_or(c, |m| m.score_cosine(c))))
        .filter(|(_, score)| score.is_finite())
        .collect();
    if let ContentAsk::Similar(id) = &ask {
        // the photo itself first, ahead of any exact twin
        let top = scored.iter().map(|(_, sc)| *sc).fold(1.0f32, f32::max);
        if let Some(own) = scored.iter_mut().find(|(p, _)| p == id) {
            own.1 = top + 1e-3;
        }
    }
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let (kept, min, best) = match &ask {
        ContentAsk::Text(_) => {
            let base = cur.manifest.match_threshold;
            let passing = |step: i32| scored.iter().take_while(|(_, sc)| *sc >= cutoff(base, step)).count();
            let (lo, hi) = (crate::content_query::MIN_STEP, crate::content_query::MAX_STEP);
            if passing(0) > 0 {
                // photos reach the model's cut-off: More and Fewer move it (Fewer never down to nothing)
                let n = passing(step);
                view.can_more = step < hi && n < scored.len();
                view.can_fewer = step > lo && passing(step - 1) > 0 && passing(step - 1) < n;
                (n.min(MAX_RESULTS), cutoff(base, step), false)
            } else {
                // none does: the best matches there are, More and Fewer change how many
                let n = best_count(BEST_MATCHES, step).max(passing(step)).min(scored.len());
                view.can_more = step < hi && n < scored.len();
                view.can_fewer = step > lo && n > 10;
                (n, f32::NEG_INFINITY, true)
            }
        }
        ContentAsk::Similar(_) => {
            let n = best_count(SIMILAR_MATCHES, step).min(scored.len());
            view.can_more = step < crate::content_query::MAX_STEP && n < scored.len();
            view.can_fewer = step > crate::content_query::MIN_STEP && n > 10;
            (n, f32::NEG_INFINITY, false)
        }
    };
    scored.truncate(kept);
    view.matches = scored.len();
    view.best_matches = best;
    view.min = min.is_finite().then_some(min);
    s.content_query.set_ranked(Some(lightcraft_catalog::Ranked { ids: scored, min }), Some(key));
    s.content_query.view = Some(view);
}
