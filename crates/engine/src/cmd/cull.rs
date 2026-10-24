//! Assisted culling: score photos for focus and clipping, group similar shots taken close
//! together (bursts) and mark the sharpest of each group; optionally reject blurry ones and pick
//! each group's best. Focus is measured on the subject where one is known (face and pet regions,
//! else the camera's focus point), else on the frame. Scores are kept on the photos (rules / smart
//! albums: `sharpness`, `bestOfGroup`).

use lightcraft_catalog::{Analysis, Flag, FocusOn, Op, PhotoId};
use lightcraft_geom::Rect;
use lightcraft_raster::Rgb32f;
use serde_json::{Value, json};

use super::{CommandSpec, always, cmd};
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

/// Focus on the photo's subject: the boxes' scores averaged by area, so the prominent faces of a
/// group count most, and whether they were measured on the preview. All boxes of a photo are
/// measured on one source: the preview when some box is too small on the thumbnail (loaded once,
/// and only when the photo has more pixels than the thumbnail), else the thumbnail; if no box can
/// be scored on the preview, all of them are tried on the thumbnail. A score's scale depends on
/// the source (a preview shows finer detail), so scores from different sources are not compared
/// (see `analyze`). `None` when nothing could be scored.
fn subject_focus(s: &mut Session, id: PhotoId, thumb: &Rgb32f, boxes: &[Rect]) -> Option<(f32, bool)> {
    use lightcraft_pipeline::cull;
    let full_edge = s.catalog.photo(id).map_or(0, |p| p.width.max(p.height) as usize);
    let small = |r: &Rect| cull::subject_px(thumb, r) < SUBJECT_MIN_PX;
    let preview = if boxes.iter().any(small) && full_edge > thumb.width.max(thumb.height) {
        s.source_now(id, crate::media::SourceLevel::Preview).ok()
    } else {
        None
    };
    let on = |img: &Rgb32f| -> Option<f32> {
        let (mut sum, mut weight) = (0.0f64, 0.0f64);
        for r in boxes {
            if let Some(v) = cull::sharpness_in(img, r).filter(|v| v.is_finite()) {
                let w = ((r.x1 - r.x0) * (r.y1 - r.y0)).max(f64::MIN_POSITIVE);
                sum += f64::from(v) * w;
                weight += w;
            }
        }
        (weight > 0.0).then(|| (sum / weight) as f32)
    };
    preview.as_deref().and_then(|pv| on(pv).map(|v| (v, true))).or_else(|| on(thumb).map(|v| (v, false)))
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
    // measure (thumbnail-level sources: fast, and enough for focus at 512 px; a small subject is
    // measured on the preview)
    let mut rows: Vec<Row> = Vec::new();
    let mut failed = Vec::new();
    for id in ids {
        match s.source_now(id, crate::media::SourceLevel::Thumb) {
            Ok(src) => {
                use lightcraft_pipeline::cull;
                let (t, boxes, kind) = match s.catalog.photo(id) {
                    Some(ph) => {
                        let (boxes, kind) = subject_boxes(ph);
                        (ph.captured.as_deref().and_then(secs), boxes, kind)
                    }
                    None => (None, Vec::new(), FocusOn::Frame),
                };
                let frame = cull::sharpness(&src);
                let measured = if boxes.is_empty() { None } else { subject_focus(s, id, &src, &boxes) };
                let (subject, on_preview) = (measured.map(|m| m.0), measured.is_some_and(|m| m.1));
                let focus_on = if subject.is_some() { kind } else { FocusOn::Frame };
                rows.push(Row {
                    id,
                    time: t,
                    sharp: subject.unwrap_or(frame),
                    frame,
                    subject,
                    on_preview,
                    focus_on,
                    clip: cull::clipped(&src),
                    sig: cull::signature(&src),
                });
            }
            Err(e) => failed.push(json!([id.0, e])),
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
    let sig = |s: &mut Session, id: PhotoId| -> Option<[f32; 64]> {
        let key = s.catalog.photo(id).map(|p| crate::media::content_key(p))?;
        if let Some(v) = s.signatures.get(&key) {
            return Some(*v);
        }
        let src = s.source_now(id, crate::media::SourceLevel::Thumb).ok()?;
        let v = lightcraft_pipeline::cull::signature(&src);
        s.signatures.insert(key, v);
        Some(v)
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
