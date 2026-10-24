//! Assisted culling: score photos for focus and clipping, group similar shots taken close
//! together (bursts) and mark the sharpest of each group; optionally reject blurry ones and pick
//! each group's best. Focus is measured on the subject where one is known (face and pet regions,
//! else the camera's focus point), else on the frame. Scores are kept on the photos (rules / smart
//! albums: `sharpness`, `bestOfGroup`).

use lightcraft_catalog::{Analysis, Flag, FocusOn, Op, PhotoId};
use lightcraft_geom::Rect;
use lightcraft_pipeline::SourceInfo;
use lightcraft_raster::Rgb32f;
use serde_json::{Value, json};

use super::{CommandSpec, always, cmd};
use crate::media::{DecodedSource, SourceLevel, SourceRef};
use crate::{Result, Session};

/// Seconds between shots of the same burst at most (default of `burstGap`).
const BURST_GAP: f64 = 10.0;
/// Signature similarity for "the same scene" (default of `similarity`).
const SAME: f64 = 0.93;
/// The longest `burstGap` accepted: a day.
const MAX_GAP: f64 = 86_400.0;

fn secs(iso: &str) -> Option<i64> {
    let d = lightcraft_meta::DateTime::parse_iso(iso)?;
    Some(d.unix_seconds())
}

/// How far a subject box is grown on every side before its focus is measured (a share of its
/// width and height): a face region is the face itself, and a little around it keeps hair, ears
/// and the edge of the chin in.
const SUBJECT_PAD: f64 = 0.15;
/// The shortest side (pixels, on the thumbnail) a subject box needs to be scored there; a smaller
/// one is measured on the larger preview source instead.
const SUBJECT_MIN_PX: usize = 48;
/// At most this many subject boxes are measured on a photo (the largest ones).
const MAX_BOXES: usize = 8;

/// Where the photo's subject is, as boxes on the upright (EXIF-oriented) frame — the frame the
/// sources are decoded in and regions and the focus point are stored in, so the numbers are used
/// as they are: face and pet regions (each grown by [`SUBJECT_PAD`]), else the camera's focus
/// point; none when the photo has neither.
fn subject_boxes(p: &lightcraft_catalog::Photo) -> (Vec<Rect>, FocusOn) {
    use lightcraft_meta::RegionKind;
    let grow = |r: &Rect| -> Option<Rect> {
        if ![r.x0, r.y0, r.x1, r.y1].iter().all(|v| v.is_finite()) {
            return None;
        }
        let (x0, x1) = (r.x0.min(r.x1).clamp(0.0, 1.0), r.x0.max(r.x1).clamp(0.0, 1.0));
        let (y0, y1) = (r.y0.min(r.y1).clamp(0.0, 1.0), r.y0.max(r.y1).clamp(0.0, 1.0));
        let (w, h) = (x1 - x0, y1 - y0);
        (w > 0.0 && h > 0.0).then(|| Rect {
            x0: (x0 - w * SUBJECT_PAD).max(0.0),
            y0: (y0 - h * SUBJECT_PAD).max(0.0),
            x1: (x1 + w * SUBJECT_PAD).min(1.0),
            y1: (y1 + h * SUBJECT_PAD).min(1.0),
        })
    };
    let area = |r: &Rect| (r.x1 - r.x0) * (r.y1 - r.y0);
    let mut faces: Vec<Rect> =
        p.meta.regions.iter().filter(|r| matches!(r.kind, RegionKind::Face | RegionKind::Pet)).filter_map(|r| grow(&r.rect)).collect();
    if !faces.is_empty() {
        faces.sort_by(|a, b| area(b).total_cmp(&area(a)));
        faces.truncate(MAX_BOXES);
        return (faces, FocusOn::Face);
    }
    match p.meta.focus.as_ref().and_then(|f| grow(&f.rect)) {
        Some(r) => (vec![r], FocusOn::FocusPoint),
        None => (Vec::new(), FocusOn::Frame),
    }
}

/// Focus on the photo's subject in `img`: the boxes' scores averaged by area, so the prominent
/// faces of a group count most; `None` when no box could be scored.
fn subject_on(img: &Rgb32f, boxes: &[Rect]) -> Option<f32> {
    let (mut sum, mut weight) = (0.0f64, 0.0f64);
    for r in boxes {
        if let Some(v) = lightcraft_pipeline::cull::sharpness_in(img, r).filter(|v| v.is_finite()) {
            let w = ((r.x1 - r.x0) * (r.y1 - r.y0)).max(f64::MIN_POSITIVE);
            sum += f64::from(v) * w;
            weight += w;
        }
    }
    (weight > 0.0).then(|| (sum / weight) as f32)
}

/// Whether the subject is measured on the preview instead of the thumbnail (`thumb`): some box is
/// too small on the thumbnail and the photo (`full_edge` pixels long) has more pixels than it. All
/// boxes of a photo are measured on one source; if none can be scored on the preview, they are
/// measured on the thumbnail. A score's scale depends on the source (a preview shows finer
/// detail), so scores from different sources are not compared (see `analyze`). The preview is
/// loaded for such a photo only, and not kept: a culling run would push the loupe's out of memory
/// and leave the last few measured in its place.
fn wants_preview(thumb: &Rgb32f, boxes: &[Rect], full_edge: usize) -> bool {
    boxes.iter().any(|r| lightcraft_pipeline::cull::subject_px(thumb, r) < SUBJECT_MIN_PX) && full_edge > thumb.width.max(thumb.height)
}

/// Photos `ids` measured side by side on rayon's pool, a batch at a time, results in the order of
/// `ids`. `job` reads what one needs from the session (serially); `measure` decodes and measures
/// it off the session and hands back the source to keep at `level`, else what the decode of one
/// not kept read; the session keeps or learns it as [`Session::source_now`] does before the next
/// batch starts. A batch is at most a photo per pool thread, and no more than the memory gate's
/// limit counts for decoding them at `level` ([`crate::files::gate_weight`]): the decodes count
/// there without waiting (interactive work), so the batch is what keeps them within it.
fn measure_all<J: Send, R: Send>(
    s: &mut Session,
    ids: &[PhotoId],
    level: SourceLevel,
    mut job: impl FnMut(&mut Session, PhotoId) -> J,
    measure: impl Fn(PhotoId, J) -> (R, Option<DecodedSource>, Option<SourceInfo>) + Sync,
) -> Vec<R> {
    use rayon::prelude::*;
    let (limit, threads) = (crate::memory::work_gate().usage().1, rayon::current_num_threads().max(1));
    let weight = |s: &Session, id: PhotoId| {
        s.catalog.photo(id).map_or(0, |p| crate::files::gate_weight(usize::try_from(p.file_size).unwrap_or(usize::MAX), level.max_edge()))
    };
    let mut out = Vec::with_capacity(ids.len());
    let mut rest = ids;
    while !rest.is_empty() {
        let (mut n, mut held) = (0, 0usize);
        while n < rest.len().min(threads) {
            let w = weight(s, rest[n]);
            if n > 0 && held.saturating_add(w) > limit {
                break;
            }
            (n, held) = (n + 1, held.saturating_add(w));
        }
        let (batch, tail) = rest.split_at(n);
        rest = tail;
        let jobs: Vec<(PhotoId, J)> = batch.iter().map(|&id| (id, job(s, id))).collect();
        // No deadlock at the memory gate: a decode waits there only as background work
        // (`memory::in_background`, a thread-local the app's render worker threads set, never
        // rayon's), so these decodes, and the parallel loops nested in them, count without waiting.
        let done: Vec<_> = jobs.into_par_iter().map(|(id, j)| (id, measure(id, j))).collect();
        for (id, (r, kept, info)) in done {
            match kept {
                Some(src) => s.keep_source(id, level, src),
                None => s.learn_source(id, info.as_ref()),
            }
            out.push(r);
        }
    }
    out
}

/// What one photo measured to.
struct Row {
    id: PhotoId,
    time: Option<i64>,
    /// The score culling goes by (the subject's when there is one).
    sharp: f32,
    /// Focus of the whole frame.
    frame: f32,
    subject: Option<f32>,
    /// The subject was measured on the preview (else on the thumbnail).
    on_preview: bool,
    focus_on: FocusOn,
    clip: f32,
    sig: [f32; 64],
}

fn analyze(s: &mut Session, p: &Value) -> Result<Value> {
    let ids: Vec<PhotoId> = if p.get("ids").is_some() || s.selection.ids.len() > 1 { s.targets(p)? } else { s.visible_cloned() };
    // a photo listed twice is one photo, not a burst of two look-alikes (first occurrence kept)
    let mut seen = std::collections::HashSet::new();
    let ids: Vec<PhotoId> = ids.into_iter().filter(|id| seen.insert(*id)).collect();
    let reject_below = p.get("rejectBelow").and_then(Value::as_f64).map(|v| v as f32);
    let pick_best = p.get("pickBest").and_then(Value::as_bool).unwrap_or(false);
    let (gap, same) = burst_params(p)?;
    // measure on all cores: thumbnail-level sources (fast, and enough for focus at 512 px), then a
    // subject too small there on the preview (see `wants_preview`)
    struct Job {
        thumb: std::result::Result<SourceRef, String>,
        time: Option<i64>,
        boxes: Vec<Rect>,
        kind: FocusOn,
        full_edge: usize,
    }
    let job = |s: &mut Session, id: PhotoId| {
        let thumb = s.source_load(id, SourceLevel::Thumb);
        let (time, (boxes, kind), full_edge) = match s.catalog.photo(id) {
            Some(ph) => (ph.captured.as_deref().and_then(secs), subject_boxes(ph), ph.width.max(ph.height) as usize),
            None => (None, (Vec::new(), FocusOn::Frame), 0),
        };
        Job { thumb, time, boxes, kind, full_edge }
    };
    let measured = measure_all(s, &ids, SourceLevel::Thumb, job, |id, j: Job| {
        use lightcraft_pipeline::cull;
        let src = match j.thumb.and_then(|r| r.load_source()) {
            Ok(src) => src,
            Err(e) => return (Err(json!([id.0, e])), None, None),
        };
        let (img, frame) = (&src.image, cull::sharpness(&src.image));
        let row = Row {
            id,
            time: j.time,
            sharp: frame,
            frame,
            subject: subject_on(img, &j.boxes),
            on_preview: false,
            focus_on: j.kind,
            clip: cull::clipped(img),
            sig: cull::signature(img),
        };
        let preview = wants_preview(img, &j.boxes, j.full_edge).then_some(j.boxes);
        (Ok((row, preview)), Some(src), None)
    });
    let (mut rows, mut failed, mut small) = (Vec::new(), Vec::new(), Vec::new());
    for m in measured {
        match m {
            Ok((r, Some(boxes))) => {
                small.push((rows.len(), r.id, boxes));
                rows.push(r);
            }
            Ok((r, None)) => rows.push(r),
            Err(e) => failed.push(e),
        }
    }
    let on: Vec<PhotoId> = small.iter().map(|m| m.1).collect();
    let mut boxes = small.iter().map(|m| m.2.clone());
    let job = |s: &mut Session, id: PhotoId| (s.source_load(id, SourceLevel::Preview).ok(), boxes.next().unwrap_or_default());
    let scores =
        measure_all(s, &on, SourceLevel::Preview, job, |_, (r, bx): (Option<SourceRef>, Vec<Rect>)| match r.and_then(|r| r.load_source().ok()) {
            Some(pv) => (subject_on(&pv.image, &bx), None, pv.info),
            None => (None, None, None),
        });
    for ((i, ..), v) in small.iter().zip(scores) {
        if let (Some(v), Some(r)) = (v, rows.get_mut(*i)) {
            (r.subject, r.on_preview) = (Some(v), true);
        }
    }
    for r in &mut rows {
        r.sharp = r.subject.unwrap_or(r.frame);
        if r.subject.is_none() {
            r.focus_on = FocusOn::Frame;
        }
    }
    // bursts: consecutive (by capture time) look-alike shots
    rows.sort_by_key(|r| (r.time.unwrap_or(i64::MAX), r.id));
    let mut group_of: Vec<Option<u32>> = vec![None; rows.len()];
    let mut next = 1u32;
    for i in 1..rows.len() {
        let (a, b) = (&rows[i - 1], &rows[i]);
        let close = matches!((a.time, b.time), (Some(x), Some(y)) if (y.saturating_sub(x)).unsigned_abs() as f64 <= gap);
        if close && lightcraft_pipeline::cull::similarity(&a.sig, &b.sig) >= same {
            let g = *group_of[i - 1].get_or_insert_with(|| {
                next += 1;
                next - 1
            });
            group_of[i] = Some(g);
        }
    }
    // a group's best is its sharpest on the subject when every shot has one measured on the same
    // source (the same thing at the same scale is compared); when only some do (a face the
    // detector missed, a turned head) or a subject straddles the thumbnail / preview limit within
    // the burst, the whole frames are compared instead, so unlike scores are never set against
    // each other
    let mut on_subject: std::collections::HashMap<u32, (bool, Option<bool>)> = Default::default();
    for (i, g) in group_of.iter().enumerate() {
        if let (Some(g), Some(r)) = (g, rows.get(i)) {
            let (all, level) = on_subject.entry(*g).or_insert((true, None));
            *all &= r.subject.is_some() && level.is_none_or(|l| l == r.on_preview);
            *level = Some(r.on_preview);
        }
    }
    let mut best: std::collections::HashMap<u32, (usize, f32)> = Default::default();
    for (i, g) in group_of.iter().enumerate() {
        if let Some(g) = g {
            let score = if on_subject.get(g).is_some_and(|o| o.0) { rows[i].sharp } else { rows[i].frame };
            let e = best.entry(*g).or_insert((i, -1.0));
            if score > e.1 {
                *e = (i, score);
            }
        }
    }
    let mut ops = Vec::new();
    let (mut rejected, mut picked) = (0, 0);
    let mut out = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        let g = group_of[i];
        let is_best = g.is_some_and(|g| best.get(&g).is_some_and(|b| b.0 == i));
        let analysis = Analysis {
            sharpness: r.sharp,
            clipped: r.clip,
            group: g,
            best: is_best,
            subject: r.subject,
            focus_on: r.focus_on,
            frame: r.subject.map(|_| r.frame),
        };
        ops.push(Op::SetAnalysis { id: r.id, analysis: Some(analysis) });
        let flag = s.catalog.photo(r.id).map(|p| p.flag).unwrap_or_default();
        if reject_below.is_some_and(|t| r.sharp < t) && flag != Flag::Reject {
            ops.push(Op::SetFlag { id: r.id, flag: Flag::Reject });
            rejected += 1;
        } else if pick_best && is_best && flag == Flag::None {
            ops.push(Op::SetFlag { id: r.id, flag: Flag::Pick });
            picked += 1;
        }
        let one = |v: f32| (v * 10.0).round() / 10.0;
        out.push(json!({
            "id": r.id.0,
            "sharpness": one(r.sharp),
            "subject": r.subject.map(one),
            "focusOn": r.focus_on.id(),
            "measuredOn": r.subject.map(|_| if r.on_preview { "preview" } else { "thumbnail" }),
            "frame": one(r.frame),
            "clipped": r.clip,
            "group": g,
            "best": is_best,
        }));
    }
    if !ops.is_empty() {
        s.commit("Assisted Culling", Op::Batch { ops })?;
    }
    Ok(json!({"photos": out, "groups": best.len(), "rejected": rejected, "picked": picked, "failed": failed}))
}

/// `burstGap` (seconds, 0..=a day) and `similarity` (0..=1) of `photo.analyze`, refused before
/// anything is measured when they are not numbers in range.
fn burst_params(p: &Value) -> Result<(f64, f32)> {
    const C: &str = "photo.analyze";
    let num = |key: &str, d: f64, max: f64| -> Result<f64> {
        match p.get(key) {
            None | Some(Value::Null) => Ok(d),
            Some(v) => match v.as_f64() {
                Some(x) if x.is_finite() && (0.0..=max).contains(&x) => Ok(x),
                _ => Err(super::bad(C, format!("`{key}` must be a number from 0 to {max}"))),
            },
        }
    };
    Ok((num("burstGap", BURST_GAP, MAX_GAP)?, num("similarity", SAME, 1.0)? as f32))
}

/// Find Similar: photos that look like `id` (signatures from thumbnail-level sources, cached by
/// content), most similar first; the view is filtered to them.
fn find_similar(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "library.findSimilar";
    let id = super::photo_id(p, C)?.or(s.active()).ok_or_else(|| super::bad(C, "no photo"))?;
    let min = p.get("similarity").and_then(Value::as_f64).unwrap_or(0.8) as f32;
    let all: Vec<PhotoId> = s.catalog.photos().filter(|q| q.in_library()).map(|q| q.id).collect();
    // signatures not cached yet, measured side by side: once for each content, from the next photo
    // with that content when one can't be read (an offline copy)
    let mut todo: Vec<(PhotoId, String)> = std::iter::once(id)
        .chain(all.iter().copied().filter(|q| *q != id))
        .filter_map(|q| Some((q, crate::media::content_key(s.catalog.photo(q)?))))
        .filter(|(_, key)| !s.signatures.contains_key(key))
        .collect();
    while !todo.is_empty() {
        let mut seen = std::collections::HashSet::new();
        let (now, later): (Vec<_>, Vec<_>) = todo.into_iter().partition(|(_, key)| seen.insert(key.clone()));
        let ids: Vec<PhotoId> = now.iter().map(|m| m.0).collect();
        let found = measure_all(
            s,
            &ids,
            SourceLevel::Thumb,
            |s, q| s.source_load(q, SourceLevel::Thumb),
            |_, r| match r.and_then(|r| r.load_source()) {
                Ok(src) => (Some(lightcraft_pipeline::cull::signature(&src.image)), Some(src), None),
                Err(_) => (None, None, None),
            },
        );
        for ((_, key), v) in now.into_iter().zip(found) {
            if let Some(v) = v {
                s.signatures.insert(key, v);
            }
        }
        todo = later.into_iter().filter(|(_, key)| !s.signatures.contains_key(key)).collect();
    }
    let sig = |s: &Session, id: PhotoId| -> Option<[f32; 64]> {
        let key = s.catalog.photo(id).map(|p| crate::media::content_key(p))?;
        s.signatures.get(&key).copied()
    };
    let me = sig(s, id).ok_or_else(|| super::bad(C, "can't read the photo"))?;
    let mut hits: Vec<(PhotoId, f32)> = Vec::new();
    for q in all {
        if let Some(v) = sig(s, q) {
            let sim = lightcraft_pipeline::cull::similarity(&me, &v);
            if sim >= min {
                hits.push((q, sim.clamp(0.0, 1.0)));
            }
        }
    }
    hits.sort_by(|a, b| b.1.total_cmp(&a.1));
    if p.get("filter").and_then(Value::as_bool).unwrap_or(true) {
        s.shelved_filter = None;
        s.filter.only = hits.iter().map(|h| h.0).collect();
        // an empty `only` means "no constraint": no hits is its own state that shows no photos
        s.filter.only_none = s.filter.only.is_empty();
    }
    Ok(json!({"photos": hits.iter().map(|(i, v)| json!({"id": i.0, "similarity": (v * 1000.0).round() / 1000.0})).collect::<Vec<_>>()}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "library.findSimilar",
            "Find Similar Photos",
            ["Photo"],
            None,
            "{id?, similarity?: 0..1 (0.8), filter?: bool (true)} — photos that look like the active one (composition and tones), most similar first; filters the view to them (Clear Filters to go back) → {photos: [{id, similarity}]}",
            always,
            find_similar
        ),
        cmd!(
            "photo.analyze",
            "Assisted Culling",
            [],
            None,
            "{ids?, rejectBelow?: sharpness 0..100, pickBest?: bool, burstGap?: seconds 0..86400 (10), similarity?: 0..1 (0.93)} — score the selected photos (else all in view) for focus (0..100) and clipping, group look-alike shots (signature similarity at least `similarity`) taken at most `burstGap` seconds apart and mark the sharpest of each; optionally reject blurry photos and pick each group's best; one undo step. Focus is measured on the subject where one is known — the photo's face and pet regions (run `faces.detect` first), else the camera's focus point — and on the whole frame (its sharpest areas) otherwise; `sharpness` is the score used, so `rejectBelow`, the best of a group and the `sharpness` / `bestOfGroup` rules go by the subject (a group whose shots don't all have a subject score measured on the same source — thumbnail, or the preview for a small subject — is compared on the frames) → {photos: [{id, sharpness, subject (null when not measured), focusOn: \"face\"|\"focusPoint\"|\"frame\", measuredOn: \"thumbnail\"|\"preview\"|null, frame, clipped, group, best}], groups, rejected, picked}",
            always,
            analyze
        ),
    ]
}
