//! Auto-tags, the session side: the vocabulary in use, embedding it (on the text thread, an activity row), scoring the
//! library against it (on a thread of its own, after each scan batch and when the vocabulary or the model changes),
//! the library's tags with the cut-off applied, accepting and dismissing suggestions, and the context that lets the
//! catalog's filters and smart albums ask about photo contents ([`lightcraft_catalog::RuleContext`]).
//!
//! The data side (files, the matrix product) is [`crate::content_tags`].

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::Instant;

use lightcraft_catalog::{Op, PhotoId};
use lightcraft_vision::Scoring;
use lightcraft_vision::vocab::{self, Vocabulary};
use serde_json::{Value, json};

use super::bad;
use crate::content_index::Hash;
use crate::content_models as cm;
use crate::content_tags::{
    Dismissed, FLOOR, Matrix, Scores, VOCAB_FILE, View, dismissed_path, read_vocab_cache, tag_rows, vocab_cache_path, write_vocab_cache,
};
use crate::{Result, Session};

/// The parent accepted tags go under unless the user chose another (Settings ▸ Search).
pub const DEFAULT_PARENT: &str = "Contents";

/// A vocabulary being embedded on the text thread.
struct VocabJob {
    id: u64,
    /// `<model id>@<sha256>` and vocabulary hash it is for.
    tag: String,
    vocab: u64,
    /// Per tag: (its key, first sentence, sentences).
    spans: Vec<(String, usize, usize)>,
    /// Embeddings of the sentences, as they come back.
    got: Vec<Option<Vec<f32>>>,
    received: usize,
    /// Tags whose embedding is done.
    done: Vec<bool>,
    cancel: Arc<AtomicBool>,
    task: crate::activity::TaskGuard,
}

/// Scores of photos worked out on the tagging thread.
struct ComputeDone {
    tag: String,
    vocab: u64,
    results: Vec<(u64, Hash, Scores)>,
    ms: f64,
}

/// What auto-tags keep between calls (`Session::content.tags`).
#[derive(Default)]
pub(crate) struct TagState {
    /// The vocabulary in use and its hash ([`Vocabulary::hash`]).
    vocab: Option<(Arc<Vocabulary>, u64)>,
    /// The vocabulary is the user's own (`vocabulary.txt`), not the built-in one.
    custom: bool,
    /// Embeddings of tags by key ([`vocab::Term::key`]), of the model `vectors_for`, and how it scores (`None`: not
    /// known yet).
    vectors: HashMap<String, Vec<f32>>,
    vectors_for: Option<String>,
    scoring: Option<Scoring>,
    cache_note: Option<String>,
    matrix: Option<Arc<Matrix>>,
    job: Option<VocabJob>,
    next_job: u64,
    /// Embedding the vocabulary was stopped by the user, or failed, for (model, vocabulary).
    held: Option<(String, u64)>,
    error: Option<(String, u64, String)>,
    /// Each indexed photo's best scores, made with the model and vocabulary `scores_for`.
    scores: HashMap<u64, (Hash, Scores)>,
    scores_for: Option<(String, u64)>,
    /// The index version the last scoring was started for.
    tagged_version: Option<u64>,
    compute: Option<Receiver<ComputeDone>>,
    /// Bumped whenever the scores change.
    generation: u64,
    pub compute_ms: Option<f64>,
    dismissed: Option<Dismissed>,
    view: Option<((u64, u64, u32, u64), Arc<View>)>,
    /// Bumped whenever a new view is made ([`Session::content_tags_generation`]).
    view_serial: u64,
    /// The Contents column: made from the view `.0` at catalog revision `.1`.
    counts: Option<((u64, u64), Arc<Vec<(String, usize)>>)>,
    /// The context handed to the catalog, what it was made from, and a number that changes with it.
    rules: Option<(String, Arc<ContentRules>)>,
    rule_scores: HashMap<String, (String, Arc<HashMap<u64, f32>>)>,
    rules_generation: u64,
    /// What rules being edited (not saved yet) ask, so the editor's count is right.
    pub(crate) extra: Vec<(bool, String)>,
}

impl TagState {
    /// Another library: its scores and dismissals go (the vocabulary and its embeddings stay: they are the user's).
    pub fn library_changed(&mut self) {
        self.scores.clear();
        self.scores_for = None;
        self.tagged_version = None;
        self.compute = None;
        self.generation = self.generation.wrapping_add(1);
        self.dismissed = None;
        self.view = None;
        self.rules = None;
        self.rule_scores.clear();
    }
}

fn content_dir(s: &Session) -> Option<PathBuf> {
    s.content_models_dir.clone()
}

fn library_dir(s: &Session) -> Option<PathBuf> {
    s.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.clone())
}

/// The vocabulary in use: the user's `vocabulary.txt` in the content models folder, else the built-in list.
pub(crate) fn vocabulary(s: &mut Session) -> Arc<Vocabulary> {
    if let Some((v, _)) = &s.content.tags.vocab {
        return v.clone();
    }
    let own = content_dir(s).and_then(|d| cm::read_capped(&d.join(VOCAB_FILE), vocab::MAX_BYTES));
    let (v, custom) = match own.map(|b| vocab::parse(&String::from_utf8_lossy(&b))) {
        Some(v) if !v.terms.is_empty() => (v, true),
        _ => (Vocabulary::default_en(), false),
    };
    let v = Arc::new(v);
    s.content.tags.vocab = Some((v.clone(), v.hash()));
    s.content.tags.custom = custom;
    v
}

/// [`Vocabulary::hash`] of the vocabulary in use (worked out once).
pub(crate) fn vocab_hash(s: &mut Session) -> u64 {
    vocabulary(s);
    s.content.tags.vocab.as_ref().map_or(0, |v| v.1)
}

/// A cached scoring brought up to the model's manifest: a softmax (CLIP) model's offset is LightCraft's calibration,
/// not part of the weights, so the one in use replaces the one the cache was written with.
pub(crate) fn current_scoring(cached: Scoring, m: &lightcraft_vision::Manifest) -> Scoring {
    if m.score.kind == lightcraft_vision::manifest::ScoreKind::Softmax && cached.sigmoid {
        Scoring { bias: m.score.softmax_offset(), ..cached }
    } else {
        cached
    }
}

/// The cut-off of every tag without its own: the user's (Settings ▸ Search), else the model's. A model LightCraft
/// knows takes its value from the built-in manifest, so a calibration in a newer version reaches installed models.
pub(crate) fn threshold(s: &Session) -> f32 {
    let user = s.content.tag_threshold.filter(|t| t.is_finite() && *t > 0.0 && *t < 1.0);
    if let Some(t) = user {
        return t;
    }
    let Some(cur) = s.content.current.as_ref() else { return lightcraft_vision::manifest::builtin_siglip2_b16_256().tag_threshold };
    lightcraft_vision::manifest::builtin().into_iter().find(|m| m.id == cur.id).map_or(cur.manifest.tag_threshold, |m| m.tag_threshold)
}

/// The model's own cut-off (what [`threshold`] is without the user's).
fn model_threshold(s: &Session) -> f32 {
    let Some(cur) = s.content.current.as_ref() else { return lightcraft_vision::manifest::builtin_siglip2_b16_256().tag_threshold };
    lightcraft_vision::manifest::builtin().into_iter().find(|m| m.id == cur.id).map_or(cur.manifest.tag_threshold, |m| m.tag_threshold)
}

/// The keyword parent of accepted tags (empty: none, the tag's name alone).
pub(crate) fn parent(s: &Session) -> String {
    match s.content.tag_parent.clone() {
        Some(p) => vocab::clean_path(&p).unwrap_or_default(),
        None => DEFAULT_PARENT.to_string(),
    }
}

/// The keyword an accepted tag becomes: `<parent>|<its name>`.
pub(crate) fn keyword_for(parent: &str, label: &str) -> String {
    if parent.is_empty() { label.to_string() } else { format!("{parent}|{label}") }
}

/// The dismissals of the open library, read when first needed.
fn dismissed(s: &mut Session) -> &mut Dismissed {
    let path = library_dir(s).map(|d| dismissed_path(&d));
    let t = &mut s.content.tags;
    if t.dismissed.as_ref().is_none_or(|d| d.path != path) {
        t.dismissed = Some(Dismissed::open(path));
    }
    t.dismissed.get_or_insert_with(Dismissed::default)
}

/// One step of auto-tagging (called by the scan's pump): load the tags' embeddings of the model in use, embed the
/// tags missing from them, and score the photos indexed since the last step.
pub(crate) fn step(s: &mut Session) {
    let Some(cur) = s.content.current.clone() else {
        stop_job(s);
        return;
    };
    if !super::content_scan::ensure_index(s) {
        return;
    }
    let v = vocabulary(s);
    let vhash = vocab_hash(s);
    // the embeddings of this model's tags
    if s.content.tags.vectors_for.as_deref() != Some(cur.tag.as_str()) {
        stop_job(s);
        let t = &mut s.content.tags;
        t.vectors.clear();
        t.scoring = None;
        t.matrix = None;
        t.cache_note = None;
        if let Some(dir) = content_dir(s) {
            let c = read_vocab_cache(&vocab_cache_path(&dir, &cur.id), &cur.tag, cur.manifest.embedding.dim as usize);
            let t = &mut s.content.tags;
            if c.note.is_none() && !c.vectors.is_empty() {
                t.scoring = Some(current_scoring(c.scoring.unwrap_or(Scoring::COSINE), &cur.manifest));
            }
            t.vectors = c.vectors;
            t.cache_note = c.note;
        }
        s.content.tags.vectors_for = Some(cur.tag.clone());
    }
    poll_job(s, &v);
    let t = &mut s.content.tags;
    if t.job.as_ref().is_some_and(|j| j.vocab != vhash || j.tag != cur.tag) {
        // the vocabulary changed while it was embedded: what is done is kept, the rest is started again
        stop_job(s);
    }
    let t = &mut s.content.tags;
    if t.matrix.as_ref().is_none_or(|m| m.tag != cur.tag || m.vocab != vhash) {
        t.matrix = None;
        let missing = v.terms.iter().any(|term| !t.vectors.contains_key(&term.key()));
        if !missing && let Some(scoring) = t.scoring {
            let dim = cur.manifest.embedding.dim as usize;
            let mut data = Vec::with_capacity(v.terms.len() * dim);
            for term in &v.terms {
                match t.vectors.get(&term.key()) {
                    Some(x) if x.len() == dim => data.extend_from_slice(x),
                    _ => data.extend(std::iter::repeat_n(0.0, dim)),
                }
            }
            t.matrix = Some(Arc::new(Matrix { tag: cur.tag.clone(), vocab: vhash, dim, data, scoring }));
        } else {
            let index_has_photos = s.content.index.as_ref().is_some_and(|i| i.len() > 0);
            let stopped = t.held.as_ref().is_some_and(|h| *h == (cur.tag.clone(), vhash))
                || t.error.as_ref().is_some_and(|(tg, h, _)| *tg == cur.tag && *h == vhash);
            if index_has_photos && !stopped && t.job.is_none() {
                start_job(s, &v, &cur.tag, vhash);
            }
        }
    }
    if let Some(m) = s.content.tags.matrix.clone() {
        score_step(s, m);
    }
}

/// Drop the vocabulary job (what it finished is kept).
fn stop_job(s: &mut Session) {
    if let Some(j) = s.content.tags.job.take() {
        j.cancel.store(true, Ordering::SeqCst);
    }
}

/// Start embedding the tags of `v` that have no embedding yet (or only load the model, to learn how it scores).
fn start_job(s: &mut Session, v: &Vocabulary, tag: &str, vhash: u64) {
    let t = &s.content.tags;
    let mut texts = Vec::new();
    let mut spans = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for term in &v.terms {
        let key = term.key();
        if t.vectors.contains_key(&key) || !seen.insert(key.clone()) {
            continue;
        }
        let sentences = term.sentences();
        spans.push((key, texts.len(), sentences.len()));
        texts.extend(sentences);
    }
    let id = t.next_job.wrapping_add(1);
    let cancel = Arc::new(AtomicBool::new(false));
    let total = texts.len();
    let worker = match super::content_scan::text_worker(s) {
        Ok(Some(w)) => w,
        Ok(None) => return,
        Err(e) => {
            s.content.tags.error = Some((tag.to_string(), vhash, e));
            return;
        }
    };
    if total > 0 && !worker.embed_vocab(id, texts, cancel.clone()) {
        s.content.tags.error = Some((tag.to_string(), vhash, "the search model stopped".into()));
        return;
    }
    let task = s.activity.start("content", "Preparing auto-tags", crate::activity::Cancel::Flag(cancel.clone()));
    // (quitting loses nothing: what is done is saved, the rest is done next time)
    task.set_resumable(true);
    task.progress(0, total as u64);
    let n = spans.len();
    s.content.tags.next_job = id;
    s.content.tags.job =
        Some(VocabJob { id, tag: tag.to_string(), vocab: vhash, spans, got: vec![None; total], received: 0, done: vec![false; n], cancel, task });
}

/// Take in the parts of the vocabulary job the text thread finished; when it is done, write the cache.
fn poll_job(s: &mut Session, v: &Vocabulary) {
    let Some(job_id) = s.content.tags.job.as_ref().map(|j| j.id) else { return };
    let (parts, scoring) = match s.content.text.as_mut() {
        Some(w) => {
            w.poll();
            // (the text model stays loaded while it works)
            w.used = Instant::now();
            let parts: Vec<_> = std::mem::take(&mut w.vocab_parts).into_iter().filter(|p| p.0 == job_id).collect();
            let error = w.error.clone();
            if let Some(e) = error {
                let j = s.content.tags.job.take();
                if let Some(j) = j {
                    s.content.tags.error = Some((j.tag, j.vocab, e));
                }
                return;
            }
            (parts, w.model.as_ref().map(|m| m.scoring().unwrap_or(Scoring::COSINE)))
        }
        None => {
            // the text thread went (the model changed): start again
            stop_job(s);
            return;
        }
    };
    let t = &mut s.content.tags;
    if let Some(sc) = scoring {
        t.scoring = Some(sc);
    }
    let Some(job) = t.job.as_mut() else { return };
    for (_, start, vectors) in parts {
        match vectors {
            Ok(vs) => {
                for (i, v) in vs.into_iter().enumerate() {
                    if let Some(slot) = job.got.get_mut(start + i)
                        && slot.is_none()
                    {
                        *slot = Some(v);
                        job.received += 1;
                    }
                }
            }
            Err(e) => {
                let (tag, h) = (job.tag.clone(), job.vocab);
                t.error = Some((tag, h, format!("the tags could not be prepared: {e}")));
                t.job = None;
                return;
            }
        }
    }
    // the tags whose sentences are all in: their mean, normalised
    for (k, (key, start, n)) in job.spans.iter().enumerate() {
        if job.done.get(k).copied().unwrap_or(true) {
            continue;
        }
        let Some(all) = job.got.get(*start..start + n) else { continue };
        if all.iter().any(Option::is_none) {
            continue;
        }
        let dim = all.iter().flatten().next().map_or(0, Vec::len);
        let mut mean = vec![0.0f32; dim];
        for x in all.iter().flatten() {
            if x.len() == dim {
                mean.iter_mut().zip(x).for_each(|(m, v)| *m += v);
            }
        }
        lightcraft_vision::normalize(&mut mean);
        t.vectors.insert(key.clone(), mean);
        if let Some(d) = job.done.get_mut(k) {
            *d = true;
        }
    }
    job.task.progress(job.received as u64, job.got.len() as u64);
    let finished = job.done.iter().all(|d| *d) && t.scoring.is_some();
    let cancelled = job.cancel.load(Ordering::SeqCst);
    if finished || cancelled {
        if cancelled && !finished {
            t.held = Some((job.tag.clone(), job.vocab));
        }
        t.job = None;
        save_cache(s, v);
    }
}

/// Write the embeddings of `v`'s tags to the model's cache.
fn save_cache(s: &mut Session, v: &Vocabulary) {
    let (Some(dir), Some(cur)) = (content_dir(s), s.content.current.clone()) else { return };
    let t = &s.content.tags;
    let entries: Vec<(String, &Vec<f32>)> = v.terms.iter().filter_map(|term| t.vectors.get(&term.key()).map(|x| (term.key(), x))).collect();
    let path = vocab_cache_path(&dir, &cur.id);
    if let Err(e) =
        write_vocab_cache(&path, &cur.tag, cur.manifest.embedding.dim as usize, t.scoring, entries.iter().map(|(k, x)| (k.as_str(), x.as_slice())))
    {
        log::warn!("auto-tags: could not write {}: {e}", path.display());
    }
}

/// Score the photos indexed (or indexed again) since the last time, on a thread of their own; take in what it did.
fn score_step(s: &mut Session, m: Arc<Matrix>) {
    let key = (m.tag.clone(), m.vocab);
    let t = &mut s.content.tags;
    if t.scores_for.as_ref() != Some(&key) {
        // another model or vocabulary: everything is scored again (results under way are dropped)
        t.scores.clear();
        t.scores_for = Some(key.clone());
        t.tagged_version = None;
        t.compute = None;
        t.generation = t.generation.wrapping_add(1);
    }
    if let Some(rx) = &t.compute {
        match rx.try_recv() {
            Ok(done) => {
                if (done.tag.as_str(), done.vocab) == (key.0.as_str(), key.1) {
                    for (photo, hash, sc) in done.results {
                        t.scores.insert(photo, (hash, sc));
                    }
                    t.generation = t.generation.wrapping_add(1);
                    t.compute_ms = Some(done.ms);
                }
                t.compute = None;
            }
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => t.compute = None,
        }
    }
    let Some(index) = s.content.index.as_ref() else { return };
    let t = &mut s.content.tags;
    if t.tagged_version == Some(index.version()) {
        return;
    }
    t.tagged_version = Some(index.version());
    let mut todo = Vec::new();
    let mut live = std::collections::HashSet::with_capacity(index.len());
    for (photo, hash) in index.entries() {
        live.insert(photo);
        if t.scores.get(&photo).map(|e| e.0) != Some(hash) {
            todo.push(photo);
        }
    }
    let before = t.scores.len();
    t.scores.retain(|p, _| live.contains(p));
    if t.scores.len() != before {
        t.generation = t.generation.wrapping_add(1);
    }
    if todo.is_empty() {
        return;
    }
    let (photos, hashes, rows) = index.rows(&todo);
    let (tx, rx) = channel();
    let spawned = std::thread::Builder::new().name("lightcraft-content-tags".into()).spawn(move || {
        let started = Instant::now();
        let work = || tag_rows(&rows, &m, FLOOR);
        let scores = match crate::content_worker::pool(crate::content_worker::Pace::Normal) {
            Some(p) => p.install(work),
            None => work(),
        };
        let results = photos.into_iter().zip(hashes).zip(scores).map(|((p, h), sc)| (p, h, sc)).collect();
        let _ = tx.send(ComputeDone { tag: m.tag.clone(), vocab: m.vocab, results, ms: started.elapsed().as_secs_f64() * 1e3 });
    });
    match spawned {
        Ok(_) => s.content.tags.compute = Some(rx),
        Err(e) => log::warn!("auto-tags: could not start scoring: {e}"),
    }
}

/// Whether tags are being worked out (the vocabulary embedded, or photos scored).
fn busy(s: &Session) -> bool {
    s.content.tags.job.is_some() || s.content.tags.compute.is_some()
}

/// How auto-tags stand: `noModel`, `noIndex` (nothing indexed yet), `preparing`, `stopped` (the user cancelled
/// preparing them), `error`, or `ready`.
fn state(s: &mut Session) -> (&'static str, Option<String>) {
    let Some(cur) = s.content.current.clone() else { return ("noModel", None) };
    let vh = vocab_hash(s);
    let t = &s.content.tags;
    if let Some((tg, h, e)) = &t.error
        && *tg == cur.tag
        && *h == vh
    {
        return ("error", Some(e.clone()));
    }
    if t.held.as_ref().is_some_and(|h| *h == (cur.tag.clone(), vh)) && t.matrix.is_none() {
        return ("stopped", None);
    }
    if s.content.index.as_ref().is_none_or(|i| i.len() == 0) {
        return ("noIndex", None);
    }
    if busy(s) || t.matrix.is_none() || t.scores_for.as_ref().is_none_or(|k| k.1 != vh) {
        return ("preparing", None);
    }
    ("ready", None)
}

/// The library's tags with the cut-off applied (`None`: there are none yet).
pub(crate) fn view(s: &mut Session) -> Option<Arc<View>> {
    let cur = s.content.current.clone()?;
    let v = vocabulary(s);
    let vh = vocab_hash(s);
    if s.content.tags.scores_for.as_ref() != Some(&(cur.tag.clone(), vh)) {
        return None;
    }
    let thr = threshold(s);
    let dgen = dismissed(s).generation;
    let t = &s.content.tags;
    let key = (t.generation, dgen, thr.to_bits(), vh);
    if let Some((k, view)) = &t.view
        && *k == key
    {
        return Some(view.clone());
    }
    let d = t.dismissed.as_ref();
    let hashes: Vec<u64> = v.terms.iter().map(|term| vocab::path_hash(&term.path)).collect();
    let built = Arc::new(View::build(
        &t.scores,
        &v,
        thr,
        |j| v.terms.get(j).and_then(|term| term.threshold).unwrap_or(thr),
        |p, j| d.is_some_and(|d| hashes.get(j).is_some_and(|h| d.set.contains(&(p, *h)))),
    ));
    s.content.tags.view = Some((key, built.clone()));
    s.content.tags.view_serial = s.content.tags.view_serial.wrapping_add(1);
    Some(built)
}

/// Whether `photo` already has the keyword tag `label` becomes (or any keyword of that name).
fn has_keyword(s: &Session, photo: PhotoId, label: &str) -> bool {
    let label = label.to_lowercase();
    s.catalog.photo(photo).is_some_and(|p| p.meta.keywords.iter().any(|k| vocab::label_of(k).trim().to_lowercase() == label))
}

/// A photo's tags as shown: (path, name, score, accepted already).
pub(crate) fn tags_of(s: &mut Session, photo: PhotoId) -> Vec<(String, String, f32, bool)> {
    let Some(view) = view(s) else { return Vec::new() };
    let v = vocabulary(s);
    let shown = view.photo.get(&photo.0).cloned().unwrap_or_default();
    shown
        .into_iter()
        .filter_map(|(j, score)| {
            let t = v.terms.get(j as usize)?;
            Some((t.path.clone(), t.label().to_string(), score, has_keyword(s, photo, t.label())))
        })
        .collect()
}

/// A photo's best scores at or above `min` whatever the cut-off and dismissals (at most [`crate::content_tags::TOP_K`]):
/// what `content.tags {min}` shows to tune a cut-off.
fn scores_of(s: &mut Session, photo: PhotoId, min: f32) -> Vec<(String, String, f32, bool)> {
    let v = vocabulary(s);
    let vh = vocab_hash(s);
    let Some(cur) = s.content.current.clone() else { return Vec::new() };
    if s.content.tags.scores_for.as_ref() != Some(&(cur.tag, vh)) {
        return Vec::new();
    }
    let found = s.content.tags.scores.get(&photo.0).map(|e| e.1.clone()).unwrap_or_default();
    found
        .into_iter()
        .filter(|(_, sc)| *sc >= min)
        .filter_map(|(j, score)| {
            let t = v.terms.get(j as usize)?;
            Some((t.path.clone(), t.label().to_string(), score, has_keyword(s, photo, t.label())))
        })
        .collect()
}

/// A suggestion for some photos: the tag, its name, the photos it is suggested for, and its best score.
#[derive(Clone, Debug, PartialEq)]
pub struct Suggestion {
    pub tag: String,
    pub label: String,
    pub photos: Vec<PhotoId>,
    pub score: f32,
}

impl Session {
    /// Tags suggested for `photos` (the Keywording panel's "Suggested from contents"): those shown for them that are
    /// neither keywords yet nor dismissed, most photos first, then best score. Empty without a content index.
    pub fn content_suggestions(&mut self, photos: &[PhotoId]) -> Vec<Suggestion> {
        let mut by: Vec<Suggestion> = Vec::new();
        for p in photos.iter().take(10_000) {
            for (tag, label, score, accepted) in tags_of(self, *p) {
                if accepted {
                    continue;
                }
                match by.iter_mut().find(|x| x.tag == tag) {
                    Some(x) => {
                        x.photos.push(*p);
                        x.score = x.score.max(score);
                    }
                    None => by.push(Suggestion { tag, label, photos: vec![*p], score }),
                }
            }
        }
        by.sort_by(|a, b| b.photos.len().cmp(&a.photos.len()).then(b.score.total_cmp(&a.score)).then(a.tag.cmp(&b.tag)));
        by
    }

    /// The tags of the library with how many photos each covers (levels above included), in path order: the
    /// Library Filter's Contents column. Empty without a content index. Worked out again only when the tags or the
    /// catalog change.
    pub fn content_tag_counts(&mut self) -> Arc<Vec<(String, usize)>> {
        let Some(view) = view(self) else { return Arc::default() };
        let key = (self.content.tags.view_serial, self.catalog.revision);
        if let Some((k, c)) = &self.content.tags.counts
            && *k == key
        {
            return c.clone();
        }
        let cat = &self.catalog;
        let mut out: Vec<(String, usize)> = view
            .under
            .values()
            .map(|(path, photos)| {
                (path.clone(), photos.iter().filter(|p| cat.photo(PhotoId(**p)).is_some_and(|ph| ph.in_library() && !ph.deleted)).count())
            })
            .filter(|(_, n)| *n > 0)
            .collect();
        // (a level before the tags under it: `|` sorts before every letter)
        out.sort_by_key(|a| a.0.to_lowercase().replace('|', "\u{1}"));
        let out = Arc::new(out);
        self.content.tags.counts = Some((key, out.clone()));
        out
    }

    /// A number that changes whenever the library's tags as shown do (scores, cut-off, dismissals, vocabulary): the
    /// frontends' caches of suggestions key on it.
    pub fn content_tags_generation(&mut self) -> u64 {
        view(self);
        self.content.tags.view_serial
    }

    /// A number that changes whenever what the catalog's content rules see changes (tags, cut-off, search texts): the
    /// view and the album counts work themselves out again when it does.
    pub fn content_rules_generation(&self) -> u64 {
        self.content.tags.rules_generation
    }
}

// ---------------------------------------------------------------- the catalog's view of photo contents

/// The catalog's answers about photo contents ([`lightcraft_catalog::RuleContext`]): the library's tags and the
/// scores of the texts Contents rules ask for.
pub(crate) struct ContentRules {
    view: Option<Arc<View>>,
    texts: HashMap<String, Arc<HashMap<u64, f32>>>,
    min: f32,
}

impl lightcraft_catalog::RuleContext for ContentRules {
    fn has_tag(&self, photo: PhotoId, tag: &str) -> Option<bool> {
        self.view.as_ref().map(|v| v.has(photo.0, tag))
    }

    fn content_score(&self, photo: PhotoId, text: &str) -> Option<f32> {
        self.texts.get(&text.trim().to_lowercase())?.get(&photo.0).copied()
    }

    fn content_min(&self) -> f32 {
        self.min
    }
}

/// Bring the catalog's view of photo contents up to date with what the filter and the smart albums ask, and install
/// it for this thread. Returns [`Session::content_rules_generation`].
pub(crate) fn refresh_rules(s: &mut Session) -> u64 {
    // what is asked: the view's filter and every smart album's (album counts, albums testing albums)
    let mut asked: Vec<(bool, String)> = s.filter.contents_asked();
    asked.extend(s.content.tags.extra.iter().cloned());
    for a in s.catalog.albums() {
        if let Some(f) = a.smart.as_deref() {
            asked.extend(f.contents_asked());
        }
    }
    if asked.is_empty() {
        if s.content.tags.rules.is_some() {
            s.content.tags.rules = None;
            s.content.tags.rules_generation = s.content.tags.rules_generation.wrapping_add(1);
        }
        lightcraft_catalog::rules::set_context(None);
        return s.content.tags.rules_generation;
    }
    super::content_scan::refresh(s, false);
    let has_index = super::content_scan::ensure_index(s);
    let view = if asked.iter().any(|(text, _)| !text) && has_index { view(s) } else { None };
    let mut texts = HashMap::new();
    let mut key = format!("{:?}|{}", view.as_ref().map(Arc::as_ptr), threshold(s));
    if has_index {
        let version = s.content.index.as_ref().map(|i| format!("{}@{}", i.tag(), i.version())).unwrap_or_default();
        for (_, text) in asked.iter().filter(|(t, _)| *t) {
            let k = text.trim().to_lowercase();
            if texts.contains_key(&k) {
                continue;
            }
            if let Some((v, scores)) = s.content.tags.rule_scores.get(&k)
                && *v == version
            {
                texts.insert(k.clone(), scores.clone());
                key.push_str(&format!("|{k}:{}", Arc::as_ptr(scores) as usize));
                continue;
            }
            let Ok(Some(w)) = super::content_scan::text_worker(s) else { continue };
            let crate::content_worker::TextVector::Ready(q) = w.vector(&k) else { continue };
            let Some(model) = w.model.clone() else { continue };
            let scores: HashMap<u64, f32> = s
                .content
                .index
                .as_ref()
                .map(|i| i.cosines(&q))
                .unwrap_or_default()
                .into_iter()
                .map(|(p, c)| (p, model.score_cosine(c)))
                .filter(|(_, sc)| sc.is_finite())
                .collect();
            let scores = Arc::new(scores);
            s.content.tags.rule_scores.insert(k.clone(), (version.clone(), scores.clone()));
            key.push_str(&format!("|{k}:{}", Arc::as_ptr(&scores) as usize));
            texts.insert(k, scores);
        }
    }
    let min = s.content.current.as_ref().map_or(0.02, |c| c.manifest.match_threshold);
    let t = &mut s.content.tags;
    let ctx = match &t.rules {
        Some((k, ctx)) if *k == key => ctx.clone(),
        _ => {
            let ctx = Arc::new(ContentRules { view, texts, min });
            t.rules = Some((key, ctx.clone()));
            t.rules_generation = t.rules_generation.wrapping_add(1);
            ctx
        }
    };
    lightcraft_catalog::rules::set_context(Some(ctx));
    t.rules_generation
}

impl Session {
    /// Also answer these contents questions (`shows`: a text, else a tag) from now on: the smart album editor's rules,
    /// before they are saved. Then [`Session::content_rules_refresh`].
    pub fn content_rules_ask(&mut self, asked: Vec<(bool, String)>) {
        self.content.tags.extra = asked.into_iter().take(64).collect();
    }

    /// What [`Self::content_rules_ask`] was last given.
    pub fn content_rules_asked(&self) -> &[(bool, String)] {
        &self.content.tags.extra
    }

    /// Whether contents rules and the Contents filter can match anything here: a content model is in use and photos
    /// are indexed (Settings ▸ Search). The smart album editor says so when they can't.
    pub fn content_rules_usable(&mut self) -> bool {
        super::content_scan::refresh(self, false);
        super::content_scan::ensure_index(self) && self.content.index.as_ref().is_some_and(|i| i.len() > 0)
    }
}

// ---------------------------------------------------------------- commands

fn round(x: f32) -> f64 {
    (f64::from(x) * 1e4).round() / 1e4
}

/// The photos a command acts on: `ids`, `id`, else the selection; only photos in the library. An `ids` or `id` that
/// isn't photo ids is an error ([`Session::targets`]).
fn photos(s: &Session, p: &Value) -> Result<Vec<PhotoId>> {
    let mut out = s.targets(p)?;
    out.retain(|id| s.catalog.photo(*id).is_some_and(|ph| ph.in_library() && !ph.deleted));
    out.dedup();
    Ok(out)
}

/// Bring tags up to date for a command that answers at once.
fn settle(s: &mut Session) {
    super::content_scan::refresh(s, true);
    step(s);
}

/// `content.tags {id?|ids?, wait?}`.
pub(crate) fn tags(s: &mut Session, p: &Value) -> Result<Value> {
    settle(s);
    if p.get("wait").and_then(Value::as_bool) == Some(true) {
        wait(s, std::time::Duration::from_secs(600));
    }
    let min = match p.get("min") {
        None | Some(Value::Null) => None,
        Some(m) => {
            Some(m.as_f64().filter(|m| m.is_finite() && *m >= 0.0 && *m < 1.0).ok_or_else(|| bad("content.tags", "`min` is a number from 0 to 1"))?
                as f32)
        }
    };
    let (state, error) = state(s);
    let list: Vec<Value> = photos(s, p)?
        .into_iter()
        .take(10_000)
        .map(|id| {
            let found = match min {
                None => tags_of(s, id),
                Some(m) => scores_of(s, id, m),
            };
            let tags: Vec<Value> = found
                .into_iter()
                .map(|(tag, label, score, accepted)| json!({"tag": tag, "label": label, "score": round(score), "accepted": accepted}))
                .collect();
            json!({"id": id.0, "tags": tags})
        })
        .collect();
    Ok(json!({"state": state, "error": error, "threshold": threshold(s), "photos": list}))
}

/// Wait (at most `timeout`) until the tags are worked out.
fn wait(s: &mut Session, timeout: std::time::Duration) {
    let started = Instant::now();
    while started.elapsed() < timeout {
        step(s);
        if state(s).0 != "preparing" {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// The tag names a command was given, resolved in the vocabulary (a path, a name or a phrase).
fn resolve(s: &mut Session, c: &str, p: &Value) -> Result<Vec<(String, String)>> {
    let names: Vec<String> = match p.get("tags") {
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        Some(Value::String(t)) => vec![t.clone()],
        _ => return Err(bad(c, "missing `tags` (a list of tags)")),
    };
    if names.is_empty() {
        return Err(bad(c, "`tags` is empty"));
    }
    let v = vocabulary(s);
    let mut out: Vec<(String, String)> = Vec::new();
    let mut unknown = Vec::new();
    for n in names.iter().take(vocab::MAX_TERMS) {
        match v.find(n).and_then(|j| v.terms.get(j)) {
            Some(t) => {
                if !out.iter().any(|(p, _)| *p == t.path) {
                    out.push((t.path.clone(), t.label().to_string()));
                }
            }
            None => unknown.push(n.clone()),
        }
    }
    if !unknown.is_empty() {
        return Err(bad(c, format!("not in the vocabulary: {}", unknown.join(", "))));
    }
    Ok(out)
}

/// `content.acceptTags {ids?, tags, all?}`: the tags become keywords (under the parent) of the photos they are
/// suggested for (`all`: of every photo given), as one undo step.
pub(crate) fn accept(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "content.acceptTags";
    settle(s);
    let tags = resolve(s, C, p)?;
    let ids = photos(s, p)?;
    if ids.is_empty() {
        return Err(bad(C, "no photos: give `ids` of photos in the library, or select some"));
    }
    let all = p.get("all").and_then(Value::as_bool) == Some(true);
    let parent = parent(s);
    let mut ops = Vec::new();
    let mut added: Vec<String> = Vec::new();
    for id in ids {
        let shown: Vec<String> = if all { Vec::new() } else { tags_of(s, id).into_iter().map(|t| t.0).collect() };
        let Some(ph) = s.catalog.photo(id) else { continue };
        let mut m = ph.meta.clone();
        let before = m.keywords.len();
        for (path, label) in &tags {
            if !all && !shown.iter().any(|x| x == path) {
                continue;
            }
            let k = lightcraft_catalog::keywords::clean(&keyword_for(&parent, label));
            if !k.is_empty() && !m.keywords.iter().any(|x| lightcraft_catalog::keywords::same(x, &k)) {
                m.keywords.push(k.clone());
                if !added.contains(&k) {
                    added.push(k);
                }
            }
        }
        if m.keywords.len() != before {
            ops.push(Op::SetMeta { id, meta: Box::new(m) });
        }
    }
    let n = ops.len();
    if n > 0 {
        s.commit("Accept Content Tags", Op::Batch { ops })?;
        crate::cmd::keywords::note_recent(s, &added);
    }
    Ok(json!({"changed": n, "keywords": added}))
}

/// `content.dismissTags {ids?, tags}`: never suggest these tags for these photos again.
pub(crate) fn dismiss(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "content.dismissTags";
    settle(s);
    let tags = resolve(s, C, p)?;
    let ids = photos(s, p)?;
    if ids.is_empty() {
        return Err(bad(C, "no photos: give `ids` of photos in the library, or select some"));
    }
    let items: Vec<(u64, u64)> = ids.iter().flat_map(|id| tags.iter().map(move |(path, _)| (id.0, vocab::path_hash(path)))).collect();
    let n = dismissed(s).add(items.into_iter()).map_err(|e| bad(C, e))?;
    Ok(json!({"dismissed": n}))
}

/// `content.vocabulary {text?}`.
pub(crate) fn vocabulary_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    settle(s);
    let v = vocabulary(s);
    let (state, error) = state(s);
    let t = &s.content.tags;
    let embedded = v.terms.iter().filter(|term| t.vectors.contains_key(&term.key())).count();
    let terms: Vec<Value> =
        v.terms.iter().map(|t| json!({"tag": t.path, "label": t.label(), "prompts": t.prompts, "threshold": t.threshold})).collect();
    let mut out = json!({
        "custom": t.custom,
        "count": v.terms.len(),
        "hash": format!("{:016x}", v.hash()),
        "notes": v.notes,
        "state": state,
        "error": error,
        "embedded": embedded,
        "cacheNote": t.cache_note,
        "file": content_dir(s).map(|d| d.join(VOCAB_FILE).display().to_string()),
        "templates": vocab::TEMPLATES,
        "terms": terms,
    });
    if p.get("text").and_then(Value::as_bool) == Some(true) {
        out["text"] = json!(v.to_text());
    }
    Ok(out)
}

/// `content.setVocabulary {text? | terms? | path? | reset?}`.
pub(crate) fn set_vocabulary(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "content.setVocabulary";
    let dir = content_dir(s).ok_or_else(|| bad(C, "this build has nowhere to keep a vocabulary"))?;
    let file = dir.join(VOCAB_FILE);
    let mut given_notes = Vec::new();
    if p.get("reset").and_then(Value::as_bool) == Some(true) {
        match std::fs::remove_file(&file) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(bad(C, format!("could not remove {}: {e}", file.display()))),
        }
    } else {
        let text = if let Some(t) = p.get("text").and_then(Value::as_str) {
            t.to_string()
        } else if let Some(a) = p.get("terms").and_then(Value::as_array) {
            a.iter().filter_map(Value::as_str).take(vocab::MAX_TERMS + 1).collect::<Vec<_>>().join("\n")
        } else if let Some(path) = p.get("path").and_then(Value::as_str) {
            let b = cm::read_capped(std::path::Path::new(path), vocab::MAX_BYTES)
                .ok_or_else(|| bad(C, format!("could not read {path} (at most {} KB)", vocab::MAX_BYTES / 1024)))?;
            String::from_utf8_lossy(&b).into_owned()
        } else {
            return Err(bad(C, "give `text`, `terms`, `path` or `reset: true`"));
        };
        if text.len() > vocab::MAX_BYTES {
            return Err(bad(C, format!("a vocabulary is at most {} KB", vocab::MAX_BYTES / 1024)));
        }
        let v = vocab::parse(&text);
        given_notes = v.notes.clone();
        if v.terms.is_empty() {
            return Err(bad(C, "the vocabulary has no tags (one per line: `animal|bird|owl` or `animal|dog = dog, puppy`)"));
        }
        std::fs::create_dir_all(&dir).map_err(|e| bad(C, format!("could not create {}: {e}", dir.display())))?;
        let part = file.with_extension("txt.part");
        std::fs::write(&part, v.to_text())
            .and_then(|()| std::fs::rename(&part, &file))
            .map_err(|e| bad(C, format!("could not save the vocabulary: {e}")))?;
    }
    let t = &mut s.content.tags;
    t.vocab = None;
    t.held = None;
    t.error = None;
    let v = vocabulary(s);
    step(s);
    Ok(json!({"count": v.terms.len(), "custom": s.content.tags.custom, "notes": given_notes, "hash": format!("{:016x}", v.hash())}))
}

/// `content.tagCounts {}`.
pub(crate) fn counts(s: &mut Session, _: &Value) -> Result<Value> {
    settle(s);
    let (state, error) = state(s);
    let v = vocabulary(s);
    let tags: Vec<Value> = s
        .content_tag_counts()
        .iter()
        .map(|(path, n)| {
            let term = v.find(path).and_then(|j| v.terms.get(j)).filter(|t| t.path.eq_ignore_ascii_case(path));
            json!({"tag": path, "label": vocab::label_of(path), "count": n, "depth": path.matches('|').count(), "inVocabulary": term.is_some()})
        })
        .collect();
    Ok(json!({"state": state, "error": error, "threshold": threshold(s), "tags": tags}))
}

/// `content.tagOptions {parent?, threshold?}`: the keyword parent of accepted tags and the tags' cut-off.
pub(crate) fn options(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "content.tagOptions";
    if let Some(dir) = content_dir(s) {
        let mut st = cm::read_settings(&dir);
        let mut changed = false;
        match p.get("parent") {
            None => {}
            Some(Value::Null) => {
                st.tag_parent = None;
                changed = true;
            }
            Some(Value::String(t)) => {
                if t.chars().count() > vocab::MAX_PATH_CHARS {
                    return Err(bad(C, format!("`parent` is at most {} characters", vocab::MAX_PATH_CHARS)));
                }
                st.tag_parent = Some(vocab::clean_path(t).unwrap_or_default());
                changed = true;
            }
            Some(_) => return Err(bad(C, "`parent` is a keyword (\"\" for none) or null for the default")),
        }
        match p.get("threshold") {
            None => {}
            Some(Value::Null) => {
                st.tag_threshold = None;
                changed = true;
            }
            Some(v) => {
                let t =
                    v.as_f64().filter(|t| t.is_finite() && *t > 0.0 && *t < 1.0).ok_or_else(|| bad(C, "`threshold` is a number between 0 and 1"))?;
                st.tag_threshold = Some(t as f32);
                changed = true;
            }
        }
        if changed {
            std::fs::create_dir_all(&dir).map_err(|e| bad(C, format!("could not create {}: {e}", dir.display())))?;
            cm::write_settings(&dir, &st).map_err(|e| bad(C, e))?;
            super::content_scan::refresh(s, true);
        }
    } else if p.get("parent").is_some() || p.get("threshold").is_some() {
        return Err(bad(C, "this build has nowhere to keep the setting"));
    }
    let user = content_dir(s).map(|d| cm::read_settings(&d)).and_then(|st| st.tag_threshold);
    Ok(json!({"parent": parent(s), "threshold": threshold(s), "modelThreshold": model_threshold(s), "custom": user.is_some()}))
}

/// What `content.status` says about auto-tags.
pub(crate) fn report(s: &mut Session) -> Value {
    let (state, error) = state(s);
    let v = vocabulary(s);
    let t = &s.content.tags;
    json!({
        "state": state,
        "error": error,
        "vocabulary": v.terms.len(),
        "customVocabulary": t.custom,
        "scored": t.scores.len(),
        "computeMs": t.compute_ms.map(f64::round),
        "dismissedNote": t.dismissed.as_ref().and_then(|d| d.note.clone()),
    })
}
