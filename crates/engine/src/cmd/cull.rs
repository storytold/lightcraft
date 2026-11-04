//! Assisted culling: score photos for focus and clipping, group similar shots taken close
//! together (bursts) and mark the sharpest of each group; optionally reject blurry ones and pick
//! each group's best. Scores are kept on the photos (rules / smart albums: `sharpness`,
//! `bestOfGroup`).

use lightcraft_catalog::{Analysis, Flag, Op, PhotoId};
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

/// Source measurements reused independently of selection-dependent burst groups and flags.
#[derive(Clone, Copy)]
pub(crate) struct Measurement {
    sharpness: f32,
    clipped: f32,
    signature: [f32; 64],
}

/// What a session keeps of a [`Measurement`]: the signature lives once, in `Session::signatures`.
#[derive(Clone, Copy)]
pub(crate) struct Scores {
    sharpness: f32,
    clipped: f32,
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
    /// Targets that can't be analyzed (in Recently Deleted), reported with the result.
    skipped: Vec<Value>,
    library: std::sync::Arc<()>,
    reject_below: Option<f32>,
    pick_best: bool,
    /// `burstGap` seconds and `similarity`, checked before anything is measured.
    gap: f64,
    same: f32,
}

/// Worker result. A cancelled run changes no scores or flags.
pub struct CullResult {
    library: std::sync::Arc<()>,
    reject_below: Option<f32>,
    pick_best: bool,
    gap: f64,
    same: f32,
    /// Each measured photo, with what a fresh decode read about its file (the as-shot white) and
    /// whether it came from a smart preview standing in for an offline original.
    rows: Vec<(PhotoId, MeasurementKey, Measurement, Option<lightcraft_pipeline::SourceInfo>, bool)>,
    failed: Vec<Value>,
    cancelled: bool,
}

/// Analysis advanced one photo at a time by hosts without worker threads.
pub struct CullRun {
    photos: std::vec::IntoIter<InputPhoto>,
    total: u64,
    result: Option<CullResult>,
}

impl CullJob {
    pub fn prepare(s: &mut Session, p: &Value) -> Result<Self> {
        let ids: Vec<PhotoId> = if p.get("ids").is_some() || s.selection.ids.len() > 1 { s.targets(p)? } else { s.visible_cloned() };
        // a photo listed twice is one photo, not a burst of two look-alikes (first occurrence kept)
        let mut seen = std::collections::HashSet::new();
        let ids: Vec<PhotoId> = ids.into_iter().filter(|id| seen.insert(*id)).collect();
        let fresh = p.get("reanalyze").and_then(Value::as_bool).unwrap_or(false);
        let (gap, same) = burst_params(p)?;
        let mut photos = Vec::with_capacity(ids.len());
        let mut skipped = Vec::new();
        for id in ids {
            match s.catalog.photo(id).map(|p| p.deleted) {
                None => {
                    skipped.push(json!([id.0, format!("no photo {}", id.0)]));
                    continue;
                }
                Some(true) => {
                    skipped.push(json!([id.0, "the photo is in Recently Deleted"]));
                    continue;
                }
                Some(false) => {}
            }
            // On the UI thread for the whole view: borrow the photo, don't clone it.
            let generation = s.cull_source_generation(id);
            let Some(photo) = s.catalog.photo(id) else { continue };
            let key = MeasurementKey { photo: id, content: crate::media::content_key(photo), source: photo.source.clone(), generation };
            let cached = if fresh {
                None
            } else {
                s.cull_measurements.get(&id).filter(|(stored, _)| *stored == key).and_then(|(_, m)| {
                    let signature = *s.signatures.get(&key.content)?;
                    Some(Measurement { sharpness: m.sharpness, clipped: m.clipped, signature })
                })
            };
            let source = cached.is_none().then(|| {
                if fresh {
                    s.media.source_ref_uncached(photo, crate::media::SourceLevel::Thumb)
                } else {
                    s.media.source_ref(photo, crate::media::SourceLevel::Thumb)
                }
            });
            photos.push(InputPhoto { id, key, name: photo.file_name.clone(), cached, source });
        }
        Ok(Self {
            photos,
            skipped,
            library: s.library_identity.clone(),
            reject_below: p.get("rejectBelow").and_then(Value::as_f64).map(|v| v as f32),
            pick_best: p.get("pickBest").and_then(Value::as_bool).unwrap_or(false),
            gap,
            same,
        })
    }

    pub fn len(&self) -> usize {
        self.photos.len()
    }

    pub fn is_empty(&self) -> bool {
        self.photos.is_empty()
    }

    /// Detach the iteration so browser hosts can yield between source reads.
    pub fn into_steps(self) -> CullRun {
        let total = self.photos.len();
        CullRun {
            photos: self.photos.into_iter(),
            total: total as u64,
            result: Some(CullResult {
                library: self.library,
                reject_below: self.reject_below,
                pick_best: self.pick_best,
                gap: self.gap,
                same: self.same,
                rows: Vec::with_capacity(total),
                failed: self.skipped,
                cancelled: false,
            }),
        }
    }

    /// Progress and cancellation are shared with the existing activity stack.
    pub fn run(self, progress: Option<&crate::activity::TaskHandle>) -> CullResult {
        let mut run = self.into_steps();
        loop {
            if let Some(result) = run.step(progress) {
                return result;
            }
        }
    }
}

impl CullRun {
    /// Analyze at most one photo. A completed/cancelled result is returned exactly once.
    /// Until completion no measurements or flags have been applied to the catalog.
    pub fn step(&mut self, progress: Option<&crate::activity::TaskHandle>) -> Option<CullResult> {
        let result = self.result.as_mut()?;
        if progress.is_some_and(|p| p.is_cancelled()) {
            result.cancelled = true;
            return self.result.take();
        }
        let Some(photo) = self.photos.next() else { return self.result.take() };
        if let Some(p) = progress {
            p.detail(&photo.name);
        }
        let measured = match photo.cached {
            Some(cached) => Ok((cached, None, false)),
            // a malformed file that panics the decoder fails this photo, not the whole run
            None => crate::guard::catch(&format!("analyzing {}", photo.name), || -> std::result::Result<_, String> {
                let source = photo.source.as_ref().ok_or_else(|| "missing analysis source".to_string())?.load_source()?;
                use lightcraft_pipeline::cull;
                let image = &source.image;
                let m = Measurement { sharpness: cull::sharpness(image), clipped: cull::clipped(image), signature: cull::signature(image) };
                Ok((m, source.info, source.from_proxy))
            })
            .and_then(|r| r),
        };
        match measured {
            Ok((measurement, info, from_proxy)) => result.rows.push((photo.id, photo.key, measurement, info, from_proxy)),
            Err(e) => result.failed.push(json!([photo.id.0, e])),
        }
        if let Some(p) = progress {
            p.progress(self.total - self.photos.len() as u64, self.total);
        }
        // Yield even after the last photo: a cancel accepted in this gap still discards it.
        None
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
        for (id, key, measurement, info, from_proxy) in self.rows {
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
            // as a synchronous decode would: a raw's file teaches the catalog its as-shot white
            if let Some(info) = &info {
                s.learn_as_shot_wb(id, info);
            }
            // a smart preview's numbers serve this run only: once the original is back it is measured
            if !from_proxy {
                s.signatures.insert(key.content.clone(), measurement.signature);
                s.cull_measurements.insert(id, (key, Scores { sharpness: measurement.sharpness, clipped: measurement.clipped }));
            }
            rows.push((id, time, measurement.sharpness, measurement.clipped, measurement.signature));
        }
        finish(s, rows, failed, self.reject_below, self.pick_best, (self.gap, self.same))
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
    (gap, same): (f64, f32),
) -> Result<Value> {
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
            "{ids?, rejectBelow?: sharpness 0..100, pickBest?: bool, burstGap?: seconds 0..86400 (10), similarity?: 0..1 (0.93), reanalyze?: bool (false)} — reuse unchanged focus/clipping measurements of the selected photos (else all in view), group look-alike shots (signature similarity at least `similarity`) taken at most `burstGap` seconds apart and mark the sharpest of each; reanalyze forces fresh measurements; optionally reject blurry photos and pick each group's best; one undo step → {photos: [{id, sharpness, clipped, group, best}], groups, rejected, picked}",
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
        s.source_now(id, crate::media::SourceLevel::Thumb).unwrap();
        s.commit("Delete", Op::SetDeleted { id, deleted: true }).unwrap();
        s.execute("edit.undo", &json!({})).unwrap();
        assert!(!s.cull_measurements.contains_key(&id));
        assert!(s.media.has_source(id, crate::media::SourceLevel::Thumb), "delete and undo leave the file's decoded sources");
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
        s.source_now(id, crate::media::SourceLevel::Thumb).unwrap();
        s.apply_system(op).unwrap();
        assert!(!s.cull_measurements.contains_key(&id));
        assert!(s.media.has_source(id, crate::media::SourceLevel::Thumb), "bookkeeping keeps decoded sources (and faces)");
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

    #[test]
    fn deleted_targets_are_reported_not_dropped() {
        let mut s = Session::with_demo();
        let id = s.active().unwrap();
        s.commit("Delete", Op::SetDeleted { id, deleted: true }).unwrap();
        let report = analyze(&mut s, &json!({"ids": [id.0]})).unwrap();
        assert_eq!(report["photos"], json!([]));
        assert_eq!(report["failed"][0][0], id.0);
        assert!(report["failed"][0][1].as_str().unwrap().contains("Recently Deleted"));
    }

    #[test]
    fn a_decoder_panic_fails_its_photo_and_the_run_goes_on() {
        let mut s = Session::new();
        for (n, name) in [(1, "boom.png"), (2, "fine.png")] {
            let photo = lightcraft_catalog::Photo::new(
                PhotoId(n),
                lightcraft_catalog::Source::File { path: format!("/nowhere/{name}") },
                name,
                "PNG",
                48,
                32,
                "2026-01-01T00:00:00",
            );
            s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        }
        s.media.file_loader = Some(std::sync::Arc::new(|path, _| {
            assert!(!path.ends_with("boom.png"), "a malformed file");
            Ok((lightcraft_raster::Rgb32f::new(48, 32), Default::default()))
        }));
        let report = CullJob::prepare(&mut s, &json!({"ids": [1, 2]})).unwrap().run(None).apply(&mut s).unwrap();
        assert_eq!(report["photos"].as_array().unwrap().len(), 1);
        assert_eq!(report["failed"][0][0], 1);
        assert!(report["failed"][0][1].as_str().unwrap().contains("failed unexpectedly"));
    }
}
