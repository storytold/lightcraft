//! Recognition commands: `faces.index` (embed the faces in the library), `faces.suggest` (who might an unnamed
//! face be?) and `faces.setName` (name a face, which is what teaches the suggestions).
//!
//! Everything here is a suggestion for the user to confirm; nothing is ever named automatically. All of it is
//! catalog-only (no sidecar is written) and `faces.setName` is undoable.

use lightcraft_catalog::Op;
use serde_json::{Value, json};

use super::{CommandSpec, bad, cmd};
use crate::{Result, Session};

/// `faces.setName {id?, index, name}`: name (or, with `null` or an empty name, un-name) one face region. Naming
/// makes the face yours: a later "Detect Faces" run no longer replaces it.
fn set_name(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "faces.setName";
    let id = p.get("id").and_then(Value::as_u64).map(lightcraft_catalog::PhotoId).or_else(|| s.active()).ok_or_else(|| bad(C, "no photo"))?;
    let index = p.get("index").and_then(Value::as_u64).and_then(|i| usize::try_from(i).ok()).ok_or_else(|| bad(C, "missing or invalid `index`"))?;
    let name = match p.get("name") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_str().ok_or_else(|| bad(C, "`name` must be text or null"))?.trim().to_string()).filter(|n| !n.is_empty()),
    };
    if let Some(n) = &name
        && (n.chars().count() > 200 || n.chars().any(char::is_control))
    {
        return Err(bad(C, "a name is at most 200 characters, without control characters"));
    }
    let mut meta = s.catalog.photo(id).ok_or_else(|| bad(C, "no such photo"))?.meta.clone();
    let region = meta.regions.get_mut(index).ok_or_else(|| bad(C, "no such region"))?;
    region.name = name.clone();
    if name.is_some() && region.description.as_deref().is_some_and(|d| d.starts_with(super::face_detect::MARK)) {
        // a detected face the user has named is theirs now
        region.description = None;
    }
    s.commit(if name.is_some() { "Name Face" } else { "Clear Face Name" }, Op::SetMeta { id, meta: Box::new(meta) })?;
    s.skip_auto_write = true;
    Ok(json!({"id": id.0, "index": index, "name": name}))
}

#[cfg(feature = "recognition")]
mod imp {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use lightcraft_catalog::PhotoId;
    use lightcraft_develop::DevelopSettings;
    use lightcraft_faces::matching;
    use lightcraft_faces::runtime::Embedder;
    use lightcraft_geom::Rect;
    use lightcraft_meta::RegionKind;
    use serde_json::{Value, json};

    use super::super::bad;
    use super::super::face_models::{finish_downloads, installed_models, read_settings};
    use crate::faces_worker::{Done, EDGE, Prepared, Worker, process};
    use crate::{Result, Session};

    /// Defaults for suggestions, until a model's own threshold is known.
    const DEFAULT_THRESHOLD: f32 = 0.45;
    const DEFAULT_MARGIN: f32 = 0.06;
    /// How long a look at the models folder is trusted by the per-frame `faces.pump`.
    const RECHECK: Duration = Duration::from_secs(1);
    /// How often the background pump writes new embeddings to the cache file (appending is cheap; this keeps it to a few
    /// writes a minute, and a session that ends writes the rest).
    const SAVE_EVERY: Duration = Duration::from_secs(3);

    /// The chosen recognition model, loaded, with the index reset to it when it changed. `fresh` looks at the models
    /// folder again; otherwise a look made in the last second is trusted (the UI asks every frame).
    fn current_with(s: &mut Session, cmd: &str, fresh: bool) -> Result<Arc<Embedder>> {
        if !fresh
            && let Some((_, e)) = &s.faces.embedder
            && s.faces.checked.is_some_and(|t| t.elapsed() < RECHECK)
        {
            return Ok(e.clone());
        }
        let dir = s.face_models_dir.clone().ok_or_else(|| bad(cmd, "this build has nowhere to keep face models"))?;
        let id = read_settings(&dir).embedder.ok_or_else(|| bad(cmd, "no recognition model is chosen: add one in Settings > Faces and press Use"))?;
        let installed = installed_models(&dir)
            .into_iter()
            .find(|i| i.manifest.id == id)
            .ok_or_else(|| bad(cmd, "the chosen recognition model is not installed"))?;
        let tag = format!("{id}@{}", installed.manifest.sha256.as_deref().unwrap_or("unknown"));
        s.faces.checked = Some(Instant::now());
        if let Some((have, e)) = &s.faces.embedder
            && *have == tag
        {
            return Ok(e.clone());
        }
        let embedder = Arc::new(Embedder::load(&dir.join(&id).join("model.onnx"), &installed.manifest).map_err(|e| bad(cmd, e.to_string()))?);
        let dim = match installed.manifest.output {
            lightcraft_faces::OutputSpec::Embedding { dim } => dim as usize,
            _ => return Err(bad(cmd, "the chosen model is not a recognition model")),
        };
        let cache = s.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.join(format!("face-embeddings-{id}.bin")));
        s.faces.index.reset(&tag, dim, cache);
        s.faces.queue.clear();
        s.faces.queue_stamp = None;
        s.faces.in_flight.clear();
        s.faces.embedder = Some((tag, embedder.clone()));
        Ok(embedder)
    }

    fn current(s: &mut Session, cmd: &str) -> Result<Arc<Embedder>> {
        current_with(s, cmd, true)
    }

    /// The photo's faces that still need embedding, with the render job for its picture. `None` when there are none.
    fn prepare(s: &mut Session, id: PhotoId) -> Option<Prepared> {
        let tag = s.faces.index.tag.clone();
        let photo = s.catalog.photo(id)?.clone();
        let todo: Vec<Rect> =
            photo.meta.regions.iter().filter(|r| r.kind == RegionKind::Face && s.faces.index.needs(id.0, &r.rect)).map(|r| r.rect).collect();
        if todo.is_empty() {
            return None;
        }
        let job = s.preview_job(id, EDGE, EDGE, false, &DevelopSettings::default())?;
        Some(Prepared { tag, id, job, todo })
    }

    /// Take a finished photo into the index (ignored when it was made for another model). Returns (embedded, failed).
    fn ingest(s: &mut Session, d: Done) -> (usize, usize) {
        s.faces.in_flight.remove(&d.id);
        if d.tag != s.faces.index.tag {
            return (0, 0);
        }
        let (mut embedded, mut failed) = (0, 0);
        for (rect, v) in d.results {
            match v {
                Some(v) if v.len() == s.faces.index.dim => {
                    s.faces.index.insert(d.id.0, &rect, v);
                    embedded += 1;
                }
                _ => {
                    s.faces.index.skip(d.id.0, &rect);
                    failed += 1;
                }
            }
        }
        (embedded, failed)
    }

    /// Take in whatever the worker has finished.
    fn collect(s: &mut Session) -> (usize, usize) {
        let finished = s.faces.worker.as_ref().map(Worker::finished).unwrap_or_default();
        finished.into_iter().fold((0, 0), |(e, f), d| {
            let (de, df) = ingest(s, d);
            (e + de, f + df)
        })
    }

    /// Embed every face region of `id` the index does not have yet, here and now. Returns (embedded, failed).
    fn embed_photo(s: &mut Session, id: PhotoId, e: &Embedder) -> Result<(usize, usize)> {
        let Some(job) = prepare(s, id) else { return Ok((0, 0)) };
        Ok(ingest(s, process(job, e)))
    }

    /// Photos with a face that needs embedding: those the caller asked for first, then those with named faces
    /// (they are the gallery everything else is compared with), then the rest.
    fn pending(s: &Session, first: &[PhotoId]) -> Vec<PhotoId> {
        let mut out: Vec<(u8, PhotoId)> = Vec::new();
        for p in s.catalog.photos().filter(|p| p.in_library()) {
            let faces: Vec<_> = p.meta.regions.iter().filter(|r| r.kind == RegionKind::Face).collect();
            if faces.iter().any(|r| s.faces.index.needs(p.id.0, &r.rect)) {
                let rank = if first.contains(&p.id) {
                    0
                } else if faces.iter().any(|r| r.name.is_some()) {
                    1
                } else {
                    2
                };
                out.push((rank, p.id));
            }
        }
        out.sort_by_key(|(rank, id)| (*rank, id.0));
        out.into_iter().map(|(_, id)| id).collect()
    }

    pub fn index(s: &mut Session, p: &Value) -> Result<Value> {
        const C: &str = "faces.index";
        let embedder = current(s, C)?;
        collect(s);
        let budget = Duration::from_millis(p.get("budgetMs").and_then(Value::as_u64).unwrap_or(200).min(600_000));
        let first: Vec<PhotoId> =
            p.get("ids").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_u64).map(PhotoId).collect()).unwrap_or_default();
        let todo = pending(s, &first);
        let started = Instant::now();
        let (mut embedded, mut failed, mut done) = (0, 0, 0);
        for id in &todo {
            // at least one photo per call, so a slow model still makes progress; none when only the status is asked for
            if budget.is_zero() || (done > 0 && started.elapsed() >= budget) {
                break;
            }
            let (e, f) = embed_photo(s, *id, &embedder)?;
            (embedded, failed, done) = (embedded + e, failed + f, done + 1);
        }
        if let Err(e) = s.faces.index.save() {
            log::warn!("could not save the face embeddings: {e}");
        }
        Ok(json!({
            "model": s.faces.index.tag,
            "embedded": embedded,
            "failed": failed,
            "photosDone": done,
            "pendingPhotos": todo.len().saturating_sub(done),
            "indexedFaces": s.faces.index.len(),
            "ms": started.elapsed().as_secs_f64() * 1000.0,
        }))
    }

    fn str_scope(p: &Value) -> Option<&str> {
        p.get("scope").and_then(Value::as_str)
    }

    /// Seconds since 1970 of an ISO capture time (`2024-09-20T10:48:45…`, any zone suffix ignored): only for telling
    /// shots taken close together, so the zone does not matter.
    fn capture_secs(iso: &str) -> Option<i64> {
        let n = |a: usize, b: usize| iso.get(a..b)?.parse::<i64>().ok();
        let (y, m, d, hh, mm, ss) = (n(0, 4)?, n(5, 7)?, n(8, 10)?, n(11, 13)?, n(14, 16)?, n(17, 19)?);
        if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
            return None;
        }
        // days from civil (proleptic Gregorian)
        let y = if m <= 2 { y - 1 } else { y };
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        Some((era * 146_097 + doe - 719_468) * 86_400 + hh * 3600 + mm * 60 + ss)
    }

    /// `faces.evaluate`: leave-one-out over the library's named faces, to see how well recognition works on *your*
    /// photos and which threshold to trust. Each named face in turn is treated as unknown and ranked against the
    /// other named faces, ignoring those from shots taken within `burstSecs` of it (near-duplicates would make
    /// everything look easy). Reports counts only.
    pub fn evaluate(s: &mut Session, p: &Value) -> Result<Value> {
        const C: &str = "faces.evaluate";
        let embedder = current(s, C)?;
        collect(s);
        let burst = p.get("burstSecs").and_then(Value::as_i64).unwrap_or(5).clamp(0, 86_400 * 7);
        let margin = p.get("margin").and_then(Value::as_f64).filter(|v| v.is_finite()).map_or(DEFAULT_MARGIN, |v| v as f32);
        let thresholds: Vec<f32> = p
            .get("thresholds")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_f64).filter(|v| v.is_finite()).map(|v| v as f32).take(64).collect())
            .filter(|v: &Vec<f32>| !v.is_empty())
            .unwrap_or_else(|| (4..=16).map(|i| i as f32 * 0.05).collect());
        let budget = Duration::from_millis(p.get("budgetMs").and_then(Value::as_u64).unwrap_or(0).min(3_600_000));
        let started = Instant::now();
        // `scope: "visible"` looks only at the photos the current filter shows (say one year, or one album)
        let scope: Option<std::collections::HashSet<PhotoId>> = (str_scope(p) == Some("visible")).then(|| s.visible_cloned().into_iter().collect());
        let in_scope = |id: PhotoId| scope.as_ref().is_none_or(|v| v.contains(&id));
        let todo: Vec<PhotoId> = pending(s, &[]).into_iter().filter(|id| in_scope(*id)).collect();
        let mut done = 0;
        for id in &todo {
            if started.elapsed() >= budget {
                break;
            }
            embed_photo(s, *id, &embedder)?;
            done += 1;
        }
        // every named face with its embedding, the photo it is in, and when that was taken
        struct Named {
            name: String,
            emb: Arc<[f32]>,
            photo: PhotoId,
            when: Option<i64>,
        }
        let named: Vec<Named> = s
            .catalog
            .photos()
            .filter(|ph| ph.in_library() && in_scope(ph.id))
            .flat_map(|ph| {
                let when = ph.captured.as_deref().and_then(capture_secs);
                ph.meta
                    .regions
                    .iter()
                    .filter(|r| r.kind == RegionKind::Face)
                    .filter_map(|r| Some(Named { name: r.name.clone()?, emb: s.faces.index.get(ph.id.0, &r.rect)?.clone(), photo: ph.id, when }))
                    .collect::<Vec<_>>()
            })
            .collect();
        let near = |a: &Named, b: &Named| a.photo == b.photo || matches!((a.when, b.when), (Some(x), Some(y)) if (x - y).abs() <= burst);
        let mut sweep: Vec<(f32, usize, usize)> = thresholds.iter().map(|t| (*t, 0, 0)).collect();
        let (mut queries, mut top1, mut unmatchable) = (0usize, 0usize, 0usize);
        for q in &named {
            let others: Vec<&Named> = named.iter().filter(|g| !near(q, g)).collect();
            if !others.iter().any(|g| g.name.eq_ignore_ascii_case(&q.name)) {
                unmatchable += 1;
                continue;
            }
            let refs: Vec<(&str, &[f32])> = others.iter().map(|g| (g.name.as_str(), &g.emb[..])).collect();
            let ranked = matching::rank(&q.emb, &refs);
            queries += 1;
            if ranked.first().is_some_and(|r| r.name.eq_ignore_ascii_case(&q.name)) {
                top1 += 1;
            }
            for (t, suggested, correct) in sweep.iter_mut() {
                if let Some(sg) = matching::suggest(&ranked, *t, margin) {
                    *suggested += 1;
                    if sg.name.eq_ignore_ascii_case(&q.name) {
                        *correct += 1;
                    }
                }
            }
        }
        let pct = |n: usize, d: usize| if d == 0 { Value::Null } else { json!((n as f64 / d as f64 * 1000.0).round() / 1000.0) };
        Ok(json!({
            "model": s.faces.index.tag,
            "namedFaces": named.len(),
            "queries": queries,
            "unmatchable": unmatchable,
            "pendingPhotos": todo.len().saturating_sub(done),
            "top1": pct(top1, queries),
            "margin": margin,
            "burstSecs": burst,
            "sweep": sweep.iter().map(|(t, sg, ok)| json!({"threshold": t, "suggested": sg, "correct": ok, "precision": pct(*ok, *sg), "recall": pct(*ok, queries)})).collect::<Vec<_>>(),
        }))
    }

    /// `faces.pump`: cheap to call every frame. Takes in what the background worker has finished and, when it is idle,
    /// hands it the next photo with faces to embed. Does nothing (and says so) until recognition is switched on in
    /// Settings with a model chosen.
    pub fn pump(s: &mut Session, _: &Value) -> Result<Value> {
        const C: &str = "faces.pump";
        // a model that has finished downloading is installed and switched on here, whether or not recognition was on
        finish_downloads(s);
        let enabled = s.face_models_dir.clone().is_some_and(|d| enabled_cached(s, &d));
        if !enabled {
            return Ok(json!({"active": false}));
        }
        if s.faces.retry_at.is_some_and(|t| Instant::now() < t) {
            return Ok(json!({"active": false}));
        }
        let Ok(embedder) = current_with(s, C, false) else {
            // the chosen model is gone or broken: look again in a few seconds, not every frame
            s.faces.retry_at = Some(Instant::now() + Duration::from_secs(3));
            return Ok(json!({"active": false}));
        };
        s.faces.retry_at = None;
        let (embedded, _) = collect(s);
        if s.faces.index.has_unsaved() && s.faces.saved_at.is_none_or(|t| t.elapsed() >= SAVE_EVERY) {
            if let Err(e) = s.faces.index.save() {
                log::warn!("could not save the face embeddings: {e}");
            }
            s.faces.saved_at = Some(Instant::now());
        }
        if s.faces.in_flight.is_empty() {
            if s.faces.queue.is_empty() && s.faces.queue_stamp != Some(s.catalog.revision) {
                s.faces.queue = pending(s, &[]);
                s.faces.queue_stamp = Some(s.catalog.revision);
            }
            while !s.faces.queue.is_empty() {
                let id = s.faces.queue.remove(0);
                let Some(job) = prepare(s, id) else { continue };
                if s.faces.worker.is_none() {
                    s.faces.worker = Worker::start();
                }
                if s.faces.worker.as_ref().is_some_and(|w| w.submit(embedder.clone(), job)) {
                    s.faces.in_flight.insert(id);
                }
                break;
            }
        }
        Ok(json!({
            "active": true,
            "inFlight": s.faces.in_flight.len(),
            "pendingPhotos": s.faces.queue.len() + s.faces.in_flight.len(),
            "indexedFaces": s.faces.index.len(),
            "embedded": embedded,
        }))
    }

    /// Whether face recognition is switched on, looked up at most once a second.
    fn enabled_cached(s: &mut Session, dir: &std::path::Path) -> bool {
        if let Some((t, on)) = s.faces.enabled_seen
            && t.elapsed() < RECHECK
        {
            return on;
        }
        let on = read_settings(dir).enabled;
        s.faces.enabled_seen = Some((Instant::now(), on));
        on
    }

    pub fn suggest(s: &mut Session, p: &Value) -> Result<Value> {
        const C: &str = "faces.suggest";
        let embedder = current(s, C)?;
        collect(s);
        let targets = s.targets(p);
        let model_threshold = embedder.manifest().thresholds.match_cosine;
        let num = |k: &str| p.get(k).and_then(Value::as_f64).filter(|v| v.is_finite()).map(|v| v as f32);
        let threshold = num("threshold").or(model_threshold).unwrap_or(DEFAULT_THRESHOLD);
        let margin = num("margin").unwrap_or(DEFAULT_MARGIN);
        // the photos asked about are embedded first, then the named faces they are compared with, within the budget
        let budget = Duration::from_millis(p.get("budgetMs").and_then(Value::as_u64).unwrap_or(1500).min(600_000));
        let started = Instant::now();
        let todo = pending(s, &targets);
        let mut done = 0;
        for id in &todo {
            // a budget of zero only looks at what is already embedded
            if budget.is_zero() || (done > 0 && started.elapsed() >= budget) {
                break;
            }
            embed_photo(s, *id, &embedder)?;
            done += 1;
        }
        let pending_photos: Vec<u64> = todo.iter().skip(done).map(|id| id.0).collect();
        let gallery: Vec<(String, Arc<[f32]>)> = s
            .catalog
            .photos()
            .filter(|ph| ph.in_library())
            .flat_map(|ph| {
                ph.meta
                    .regions
                    .iter()
                    .filter(|r| r.kind == RegionKind::Face)
                    .filter_map(|r| Some((r.name.clone()?, s.faces.index.get(ph.id.0, &r.rect)?.clone())))
                    .collect::<Vec<_>>()
            })
            .collect();
        let refs: Vec<(&str, &[f32])> = gallery.iter().map(|(n, e)| (n.as_str(), &e[..])).collect();
        let mut photos = Vec::new();
        for id in &targets {
            let Some(photo) = s.catalog.photo(*id) else { continue };
            let mut faces = Vec::new();
            for (index, r) in photo.meta.regions.iter().enumerate().filter(|(_, r)| r.kind == RegionKind::Face && r.name.is_none()) {
                let Some(q) = s.faces.index.get(id.0, &r.rect) else {
                    faces.push(json!({"index": index, "pending": true}));
                    continue;
                };
                let ranked = matching::rank(q, &refs);
                let suggestion = matching::suggest(&ranked, threshold, margin);
                faces.push(json!({
                    "index": index,
                    "suggestion": suggestion.as_ref().map(|x| json!({"name": x.name, "score": x.score, "runnerUp": x.runner_up.as_ref().map(|(n, sc)| json!({"name": n, "score": sc}))})),
                    "candidates": ranked.iter().take(3).map(|c| json!({"name": c.name, "score": c.score, "faces": c.faces})).collect::<Vec<_>>(),
                }));
            }
            photos.push(json!({"id": id.0, "faces": faces}));
        }
        Ok(
            json!({"model": s.faces.index.tag, "threshold": threshold, "margin": margin, "galleryFaces": gallery.len(), "pendingPhotos": pending_photos, "photos": photos}),
        )
    }
}

#[cfg(not(feature = "recognition"))]
mod imp {
    use serde_json::Value;

    use super::super::bad;
    use crate::{Result, Session};

    pub fn index(_: &mut Session, _: &Value) -> Result<Value> {
        Err(bad("faces.index", "this build cannot run face recognition"))
    }

    pub fn suggest(_: &mut Session, _: &Value) -> Result<Value> {
        Err(bad("faces.suggest", "this build cannot run face recognition"))
    }

    pub fn evaluate(_: &mut Session, _: &Value) -> Result<Value> {
        Err(bad("faces.evaluate", "this build cannot run face recognition"))
    }

    pub fn pump(_: &mut Session, _: &Value) -> Result<Value> {
        Ok(serde_json::json!({"active": false}))
    }
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "faces.index",
            "Index Faces",
            [],
            None,
            "{budgetMs?: 200, ids?: [photo ids to do first]} → {embedded, photosDone, pendingPhotos, indexedFaces, ms} — embed the faces in the library with the chosen recognition model, as many photos as fit in the time (at least one); call again until `pendingPhotos` is 0. `budgetMs: 0` only reports",
            super::always,
            imp::index
        ),
        cmd!(
            "faces.suggest",
            "Suggest Names for Faces",
            [],
            None,
            "{ids?, threshold?, margin?, budgetMs?} → {photos: [{id, faces: [{index, suggestion: {name, score, runnerUp} | null, candidates: [{name, score}]}]}]} — who the unnamed faces in these photos might be, from the faces already named in the library. Only suggestions: nothing is named",
            super::always,
            imp::suggest
        ),
        cmd!(
            query "faces.pump",
            "Index Faces in the Background",
            [],
            None,
            "{} → {active, inFlight, pendingPhotos, indexedFaces} — cheap to call every frame: takes in what the background worker has finished and gives it the next photo; inactive until face recognition is on in Settings with a model chosen",
            super::always,
            imp::pump
        ),
        cmd!(
            "faces.evaluate",
            "Evaluate Face Recognition",
            [],
            None,
            "{budgetMs?: 0, burstSecs?: 5, margin?, thresholds?: [..], scope?: \"visible\"} → {queries, top1, sweep: [{threshold, suggested, correct, precision, recall}]} — leave-one-out over your named faces: each is ranked against the others (ignoring shots within burstSecs of it) to show how well the chosen model recognises people in *your* photos and which threshold to trust. Faces not yet embedded are done within budgetMs (`faces.index` does them all); counts only",
            super::always,
            imp::evaluate
        ),
        cmd!(
            "faces.setName",
            "Name Face",
            [],
            None,
            "{id?, index, name} — name a face region (null or empty clears it); undoable; the sidecar is not touched",
            super::always,
            set_name
        ),
    ]
}
