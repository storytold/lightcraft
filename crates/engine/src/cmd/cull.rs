//! Assisted culling: score photos for focus and clipping, group similar shots taken close
//! together (bursts) and mark the sharpest of each group; optionally reject blurry ones and pick
//! each group's best. Also writes classical aesthetic, eyes-open (when faces are known) and a
//! combined cull score. Scores are kept on the photos (rules / smart albums: `sharpness`,
//! `bestOfGroup`, `clipped`, `eyesOpen`, `aesthetic`, `cullScore`).

use lightcraft_catalog::{Analysis, Flag, Op, Photo, PhotoId};
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

/// Eyes-open from YuNet landmarks when the detector is installed; otherwise a weak guess from a
/// stored face box. `None` when neither is available (the field is skipped, not stored as 0).
fn eyes_open_for(src: &Rgb32f, detector: Option<&lightcraft_faces::yunet::Detector>, photo: Option<&Photo>) -> Option<f32> {
    if let Some(det) = detector {
        let rgb: Vec<u8> = src
            .data
            .iter()
            .flat_map(|c| {
                [0, 1, 2]
                    .map(|i| (lightcraft_color::transfer::linear_to_srgb(c.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0)) * 255.0).round() as u8)
            })
            .collect();
        if let Ok(faces) = det.detect(&rgb, src.width, src.height, &lightcraft_faces::yunet::Options::default())
            && let Some(first) = faces.first()
        {
            return lightcraft_pipeline::cull::eyes_open(src, &first.landmarks);
        }
    }
    let r = photo.and_then(|p| p.meta.regions.iter().find(|r| r.kind == lightcraft_meta::RegionKind::Face))?;
    let (x0, y0, x1, y1) = (r.rect.x0 as f32, r.rect.y0 as f32, r.rect.x1 as f32, r.rect.y1 as f32);
    let w = (x1 - x0).abs();
    let h = (y1 - y0).abs();
    if !w.is_finite() || !h.is_finite() || w < 0.02 || h < 0.02 {
        return None;
    }
    lightcraft_pipeline::cull::eyes_open(src, &[(x0 + 0.3 * w, y0 + 0.32 * h), (x0 + 0.7 * w, y0 + 0.32 * h)])
}

fn analyze(s: &mut Session, p: &Value) -> Result<Value> {
    let ids: Vec<PhotoId> = if p.get("ids").is_some() || s.selection.ids.len() > 1 { s.targets(p)? } else { s.visible_cloned() };
    // a photo listed twice is one photo, not a burst of two look-alikes (first occurrence kept)
    let mut seen = std::collections::HashSet::new();
    let ids: Vec<PhotoId> = ids.into_iter().filter(|id| seen.insert(*id)).collect();
    let reject_below = p.get("rejectBelow").and_then(Value::as_f64).map(|v| v as f32);
    let pick_best = p.get("pickBest").and_then(Value::as_bool).unwrap_or(false);
    let (gap, same) = burst_params(p)?;
    // measure (thumbnail-level sources: fast, and enough for focus at 512 px)
    let detector = super::face_detect::detector(s).ok();
    let mut rows: Vec<(PhotoId, Option<i64>, f32, f32, [f32; 64], Option<f32>, f32)> = Vec::new();
    let mut failed = Vec::new();
    for id in ids {
        match s.source_now(id, crate::media::SourceLevel::Thumb) {
            Ok(src) => {
                let photo = s.catalog.photo(id);
                let t = photo.and_then(|p| p.captured.as_deref().and_then(secs));
                use lightcraft_pipeline::cull;
                let eyes = eyes_open_for(&src, detector.as_deref(), photo.map(|p| p.as_ref()));
                rows.push((id, t, cull::sharpness(&src), cull::clipped(&src), cull::signature(&src), eyes, cull::aesthetic(&src)));
            }
            Err(e) => failed.push(json!([id.0, e])),
        }
    }
    // bursts: consecutive (by capture time) look-alike shots
    rows.sort_by_key(|r| (r.1.unwrap_or(i64::MAX), r.0));
    let mut group_of: Vec<Option<u32>> = vec![None; rows.len()];
    let mut next = 1u32;
    for i in 1..rows.len() {
        let (a, b) = (&rows[i - 1], &rows[i]);
        let close = matches!((a.1, b.1), (Some(x), Some(y)) if (y.saturating_sub(x)).unsigned_abs() as f64 <= gap);
        if close && lightcraft_pipeline::cull::similarity(&a.4, &b.4) >= same {
            let g = *group_of[i - 1].get_or_insert_with(|| {
                next += 1;
                next - 1
            });
            group_of[i] = Some(g);
        }
    }
    let mut best: std::collections::HashMap<u32, (usize, f32)> = Default::default();
    for (i, g) in group_of.iter().enumerate() {
        if let Some(g) = g {
            let e = best.entry(*g).or_insert((i, -1.0));
            if rows[i].2 > e.1 {
                *e = (i, rows[i].2);
            }
        }
    }
    let mut ops = Vec::new();
    let (mut rejected, mut picked) = (0, 0);
    let mut out = Vec::new();
    for (i, (id, _, sharp, clip, _, eyes, aesthetic)) in rows.iter().enumerate() {
        let g = group_of[i];
        let is_best = g.is_some_and(|g| best.get(&g).is_some_and(|b| b.0 == i));
        let score = lightcraft_pipeline::cull::cull_score(*sharp, *clip, is_best, g.is_some(), *eyes, *aesthetic);
        ops.push(Op::SetAnalysis {
            id: *id,
            analysis: Some(Analysis {
                sharpness: *sharp,
                clipped: *clip,
                group: g,
                best: is_best,
                eyes_open: *eyes,
                aesthetic: Some(*aesthetic),
                cull_score: Some(score),
            }),
        });
        let flag = s.catalog.photo(*id).map(|p| p.flag).unwrap_or_default();
        if reject_below.is_some_and(|t| *sharp < t) && flag != Flag::Reject {
            ops.push(Op::SetFlag { id: *id, flag: Flag::Reject });
            rejected += 1;
        } else if pick_best && is_best && flag == Flag::None {
            ops.push(Op::SetFlag { id: *id, flag: Flag::Pick });
            picked += 1;
        }
        out.push(json!({
            "id": id.0,
            "sharpness": (sharp * 10.0).round() / 10.0,
            "clipped": clip,
            "group": g,
            "best": is_best,
            "eyesOpen": eyes,
            "aesthetic": (aesthetic * 10.0).round() / 10.0,
            "cullScore": (score * 1000.0).round() / 1000.0,
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
            "{ids?, rejectBelow?: sharpness 0..100, pickBest?: bool, burstGap?: seconds 0..86400 (10), similarity?: 0..1 (0.93)} — score the selected photos (else all in view) for focus (0..100), clipping, classical aesthetic and eyes-open when faces are known; group look-alike shots (signature similarity at least `similarity`) taken at most `burstGap` seconds apart and mark the sharpest of each; optionally reject blurry photos and pick each group's best; one undo step → {photos: [{id, sharpness, clipped, group, best, eyesOpen?, aesthetic, cullScore}], groups, rejected, picked}",
            always,
            analyze
        ),
    ]
}
