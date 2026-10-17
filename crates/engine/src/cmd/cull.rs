//! Assisted culling: score photos for focus and clipping, group similar shots taken close
//! together (bursts) and mark the sharpest of each group; optionally reject blurry ones and pick
//! each group's best. Scores are kept on the photos (rules / smart albums: `sharpness`,
//! `bestOfGroup`).

use lightcraft_catalog::{Analysis, Flag, Op, PhotoId};
use serde_json::{Value, json};

use super::{CommandSpec, always, cmd};
use crate::{Result, Session};

/// Seconds between shots of the same burst at most.
const BURST_GAP: i64 = 10;
/// Signature similarity for "the same scene".
const SAME: f32 = 0.93;

fn secs(iso: &str) -> Option<i64> {
    let d = lightcraft_meta::DateTime::parse_iso(iso)?;
    Some(((d.year as i64 * 372 + d.month as i64 * 31 + d.day as i64) * 24 + d.hour as i64) * 3600 + d.minute as i64 * 60 + d.second as i64)
}

/// Source measurements reused independently of selection-dependent burst groups and flags.
#[derive(Clone, Copy)]
pub(crate) struct Measurement {
    sharpness: f32,
    clipped: f32,
    signature: [f32; 64],
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct MeasurementKey {
    photo: PhotoId,
    content: String,
    source: lightcraft_catalog::Source,
    generation: u64,
}

struct InputPhoto {
    id: PhotoId,
    key: MeasurementKey,
    name: String,
    cached: Option<Measurement>,
    source: Option<crate::media::SourceRef>,
}

/// File decoding and pixel analysis detached from the session; applying a result stays on its owner.
pub struct CullJob {
    photos: Vec<InputPhoto>,
    library: std::sync::Arc<()>,
    reject_below: Option<f32>,
    pick_best: bool,
}

/// Worker result. A cancelled run changes no scores or flags.
pub struct CullResult {
    library: std::sync::Arc<()>,
    reject_below: Option<f32>,
    pick_best: bool,
    rows: Vec<(PhotoId, MeasurementKey, Measurement)>,
    failed: Vec<Value>,
    cancelled: bool,
}

impl CullJob {
    pub fn prepare(s: &mut Session, p: &Value) -> Result<Self> {
        let ids = if p.get("ids").is_some() || s.selection.ids.len() > 1 { s.targets(p)? } else { s.visible_cloned() };
        let fresh = p.get("reanalyze").and_then(Value::as_bool).unwrap_or(false);
        let photos = ids
            .into_iter()
            .filter_map(|id| {
                let photo = s.catalog.photo(id)?.clone();
                if photo.deleted {
                    return None;
                }
                let key = MeasurementKey {
                    photo: id,
                    content: crate::media::content_key(&photo),
                    source: photo.source.clone(),
                    generation: s.cull_source_generation(id),
                };
                let cached = (!fresh).then(|| s.cull_measurements.get(&id).filter(|(stored, _)| *stored == key).map(|(_, m)| *m)).flatten();
                let source = cached.is_none().then(|| {
                    if fresh {
                        s.media.source_ref_uncached(&photo, crate::media::SourceLevel::Thumb)
                    } else {
                        s.media.source_ref(&photo, crate::media::SourceLevel::Thumb)
                    }
                });
                Some(InputPhoto { id, key, name: photo.file_name.clone(), cached, source })
            })
            .collect();
        Ok(Self {
            photos,
            library: s.library_identity.clone(),
            reject_below: p.get("rejectBelow").and_then(Value::as_f64).map(|v| v as f32),
            pick_best: p.get("pickBest").and_then(Value::as_bool).unwrap_or(false),
        })
    }

    pub fn len(&self) -> usize {
        self.photos.len()
    }

    pub fn is_empty(&self) -> bool {
        self.photos.is_empty()
    }

    /// Progress and cancellation are shared with the existing activity stack.
    pub fn run(self, progress: Option<&crate::activity::TaskHandle>) -> CullResult {
        let total = self.photos.len() as u64;
        let mut result = CullResult {
            library: self.library,
            reject_below: self.reject_below,
            pick_best: self.pick_best,
            rows: Vec::with_capacity(self.photos.len()),
            failed: Vec::new(),
            cancelled: false,
        };
        for (i, photo) in self.photos.into_iter().enumerate() {
            if progress.is_some_and(|p| p.is_cancelled()) {
                result.cancelled = true;
                break;
            }
            if let Some(p) = progress {
                p.detail(&photo.name);
            }
            let measured = match photo.cached {
                Some(cached) => Ok(cached),
                None => photo.source.as_ref().ok_or_else(|| "missing analysis source".to_string()).and_then(|s| s.load()).map(|source| {
                    use lightcraft_pipeline::cull;
                    Measurement { sharpness: cull::sharpness(&source), clipped: cull::clipped(&source), signature: cull::signature(&source) }
                }),
            };
            match measured {
                Ok(measurement) => result.rows.push((photo.id, photo.key, measurement)),
                Err(e) => result.failed.push(json!([photo.id.0, e])),
            }
            if let Some(p) = progress {
                p.progress((i + 1) as u64, total);
            }
        }
        result.cancelled |= progress.is_some_and(|p| p.is_cancelled());
        result
    }
}

impl CullResult {
    pub fn apply(self, s: &mut Session) -> Result<Value> {
        if self.cancelled {
            return Ok(json!({"cancelled": true, "photos": [], "groups": 0, "rejected": 0, "picked": 0, "failed": []}));
        }
        if !std::sync::Arc::ptr_eq(&self.library, &s.library_identity) {
            return Err(super::bad("photo.analyze", "another library was opened; analysis was discarded"));
        }
        let mut rows = Vec::with_capacity(self.rows.len());
        let mut failed = self.failed;
        for (id, key, measurement) in self.rows {
            let Some(photo) = s.catalog.photo(id).filter(|p| {
                !p.deleted
                    && p.source == key.source
                    && crate::media::content_key(p) == key.content
                    && s.cull_source_generations.get(&id) == Some(&key.generation)
            }) else {
                failed.push(json!([id.0, "photo changed or was removed during analysis"]));
                continue;
            };
            let time = photo.captured.as_deref().and_then(secs);
            s.signatures.insert(key.content.clone(), measurement.signature);
            s.cull_measurements.insert(id, (key, measurement));
            rows.push((id, time, measurement.sharpness, measurement.clipped, measurement.signature));
        }
        finish(s, rows, failed, self.reject_below, self.pick_best)
    }
}

fn analyze(s: &mut Session, p: &Value) -> Result<Value> {
    CullJob::prepare(s, p)?.run(None).apply(s)
}

fn finish(
    s: &mut Session,
    mut rows: Vec<(PhotoId, Option<i64>, f32, f32, [f32; 64])>,
    failed: Vec<Value>,
    reject_below: Option<f32>,
    pick_best: bool,
) -> Result<Value> {
    // bursts: consecutive (by capture time) look-alike shots
    rows.sort_by_key(|r| (r.1.unwrap_or(i64::MAX), r.0));
    let mut group_of: Vec<Option<u32>> = vec![None; rows.len()];
    let mut next = 1u32;
    for i in 1..rows.len() {
        let (a, b) = (&rows[i - 1], &rows[i]);
        let close = matches!((a.1, b.1), (Some(x), Some(y)) if (y - x).abs() <= BURST_GAP);
        if close && lightcraft_pipeline::cull::similarity(&a.4, &b.4) >= SAME {
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
    for (i, (id, _, sharp, clip, _)) in rows.iter().enumerate() {
        let g = group_of[i];
        let is_best = g.is_some_and(|g| best.get(&g).is_some_and(|b| b.0 == i));
        ops.push(Op::SetAnalysis { id: *id, analysis: Some(Analysis { sharpness: *sharp, clipped: *clip, group: g, best: is_best }) });
        let flag = s.catalog.photo(*id).map(|p| p.flag).unwrap_or_default();
        if reject_below.is_some_and(|t| *sharp < t) && flag != Flag::Reject {
            ops.push(Op::SetFlag { id: *id, flag: Flag::Reject });
            rejected += 1;
        } else if pick_best && is_best && flag == Flag::None {
            ops.push(Op::SetFlag { id: *id, flag: Flag::Pick });
            picked += 1;
        }
        out.push(json!({"id": id.0, "sharpness": (sharp * 10.0).round() / 10.0, "clipped": clip, "group": g, "best": is_best}));
    }
    if !ops.is_empty() {
        s.commit("Assisted Culling", Op::Batch { ops })?;
    }
    Ok(json!({"photos": out, "groups": best.len(), "rejected": rejected, "picked": picked, "failed": failed}))
}

/// Find Similar: photos that look like `id` (signatures from thumbnail-level sources, cached by
/// content), most similar first; the view is filtered to them.
fn find_similar(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "library.findSimilar";
    let id = p.get("id").and_then(Value::as_u64).map(PhotoId).or(s.active()).ok_or_else(|| super::bad(C, "no photo"))?;
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
                hits.push((q, sim));
            }
        }
    }
    hits.sort_by(|a, b| b.1.total_cmp(&a.1));
    if p.get("filter").and_then(Value::as_bool).unwrap_or(true) {
        s.filter.only = hits.iter().map(|h| h.0).collect();
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
            "{ids?, rejectBelow?: sharpness 0..100, pickBest?: bool, reanalyze?: bool (false)} — reuse unchanged focus/clipping measurements, group look-alike shots taken within 10 s and mark the sharpest of each; reanalyze forces fresh measurements; optionally reject blurry photos and pick each group's best; one undo step → {photos: [{id, sharpness, clipped, group, best}], groups, rejected, picked}",
            always,
            analyze
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn another_library_or_changed_photo_never_receives_a_stale_analysis() {
        let mut s = Session::with_demo();
        let id = s.active().unwrap();
        let result = CullJob::prepare(&mut s, &json!({"ids": [id.0]})).unwrap().run(None);
        s.library_identity = std::sync::Arc::new(());
        assert!(result.apply(&mut s).is_err());
        assert!(s.catalog.photo(id).unwrap().analysis.is_none());
        let result = CullJob::prepare(&mut s, &json!({"ids": [id.0]})).unwrap().run(None);
        s.commit(
            "change source",
            Op::SetContent {
                id,
                width: 600,
                height: 400,
                file_size: 0,
                content_hash: Some("changed-source".into()),
                preview_only: None,
                preview_checked: String::new(),
            },
        )
        .unwrap();
        let report = result.apply(&mut s).unwrap();
        assert_eq!(report["photos"], json!([]));
        assert_eq!(report["failed"][0][0], id.0);
        assert!(s.catalog.photo(id).unwrap().analysis.is_none());
    }

    #[test]
    fn removed_photo_is_skipped_and_changed_content_is_measured_again() {
        let mut s = Session::with_demo();
        let id = s.active().unwrap();
        let params = json!({"ids": [id.0]});
        analyze(&mut s, &params).unwrap();
        s.media.forget(id);
        s.commit(
            "new source",
            Op::Relink {
                id,
                file_name: "new.png".into(),
                source: lightcraft_catalog::Source::File { path: "new-source.png".into() },
                format: Some("PNG".into()),
            },
        )
        .unwrap();
        // A failed replacement probe can retain the imported content hash after relinking.
        // The source path itself must invalidate both cached measurements and in-flight results.
        s.media.file_loader = Some(std::sync::Arc::new(|_, _| Err("new source unreadable".into())));
        let report = analyze(&mut s, &params).unwrap();
        assert_eq!(report["photos"], json!([]), "a previous content key cannot hide a changed source");
        assert_eq!(report["failed"][0][1], "new source unreadable");
        let result = CullJob::prepare(&mut s, &params).unwrap().run(None);
        s.commit("delete", Op::SetDeleted { id, deleted: true }).unwrap();
        let before = s.undo.len();
        let report = result.apply(&mut s).unwrap();
        assert_eq!(report["photos"], json!([]));
        assert_eq!(s.undo.len(), before, "missing/deleted photos get no catalog operations");
    }

    #[test]
    fn removed_then_restored_photo_never_receives_old_worker_measurements() {
        let mut s = Session::with_demo();
        let id = s.active().unwrap();
        let params = json!({"ids": [id.0], "rejectBelow": 101});
        let result = CullJob::prepare(&mut s, &params).unwrap().run(None);
        s.commit("Remove", Op::RemovePhoto { id }).unwrap();
        s.execute("edit.undo", &json!({})).unwrap();
        let stale = result.apply(&mut s).unwrap();
        assert_eq!(stale["photos"], json!([]));
        assert_eq!(stale["failed"][0][0], id.0);
        assert!(s.catalog.photo(id).unwrap().analysis.is_none());
        assert_eq!(s.catalog.photo(id).unwrap().flag, Flag::None);
        let fresh = analyze(&mut s, &params).unwrap();
        assert_eq!(fresh["failed"], json!([]));
        assert_eq!(s.catalog.photo(id).unwrap().flag, Flag::Reject);
    }

    #[test]
    fn delete_then_undo_discards_measurements_and_in_flight_results() {
        let mut s = Session::with_demo();
        let id = s.active().unwrap();
        let params = json!({"ids": [id.0]});
        analyze(&mut s, &params).unwrap();
        let result = CullJob::prepare(&mut s, &params).unwrap().run(None);
        s.commit("Delete", Op::SetDeleted { id, deleted: true }).unwrap();
        s.execute("edit.undo", &json!({})).unwrap();
        assert!(!s.cull_measurements.contains_key(&id));
        let before = s.undo.len();
        let report = result.apply(&mut s).unwrap();
        assert_eq!(report["photos"], json!([]));
        assert_eq!(report["failed"][0][0], id.0);
        assert_eq!(s.undo.len(), before);
    }

    #[test]
    fn system_reload_invalidates_measurements_and_in_flight_results() {
        let mut s = Session::with_demo();
        let id = s.active().unwrap();
        let params = json!({"ids": [id.0]});
        analyze(&mut s, &params).unwrap();
        let result = CullJob::prepare(&mut s, &params).unwrap().run(None);
        let p = s.catalog.photo(id).unwrap();
        let op = Op::SetContent {
            id,
            width: p.width,
            height: p.height,
            file_size: p.file_size,
            content_hash: p.content_hash.clone(),
            preview_only: p.preview_only.clone(),
            preview_checked: p.preview_checked.clone(),
        };
        let before = s.undo.len();
        s.apply_system(op).unwrap();
        assert!(!s.cull_measurements.contains_key(&id));
        let report = result.apply(&mut s).unwrap();
        assert_eq!(report["photos"], json!([]));
        assert_eq!(report["failed"][0][0], id.0);
        assert_eq!(s.undo.len(), before, "system reload and stale analysis are not undo steps");
    }

    #[test]
    fn browse_forgetting_local_photo_clears_culling_generation() {
        let mut s = Session::new();
        let id = PhotoId(1);
        let dir = std::env::temp_dir().join(format!("lc-cull-gone-{}", std::process::id()));
        let path = dir.join("gone.png").to_string_lossy().into_owned();
        let mut photo =
            lightcraft_catalog::Photo::new(id, lightcraft_catalog::Source::File { path }, "gone.png", "PNG", 48, 32, "2026-01-01T00:00:00");
        photo.local = true;
        s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        s.media.file_loader = Some(std::sync::Arc::new(|_, _| Ok((lightcraft_raster::Rgb32f::new(48, 32), Default::default()))));
        let result = CullJob::prepare(&mut s, &json!({"ids": [id.0]})).unwrap().run(None);
        assert!(s.cull_source_generations.contains_key(&id));
        assert_eq!(s.forget_gone_from(&dir.to_string_lossy(), false, &[]), 1);
        assert!(!s.cull_source_generations.contains_key(&id));
        let report = result.apply(&mut s).unwrap();
        assert_eq!(report["photos"], json!([]));
        assert_eq!(report["failed"][0][0], id.0);
    }
}
