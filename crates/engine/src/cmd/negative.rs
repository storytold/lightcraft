//! Film negative conversion commands (`docs/negative-conversion.md`): convert a scan, re-analyse its film base and
//! density range, sample the base from the film edge, choose the film type / tone / colour model, and reset. The
//! sliders are ordinary `negative.*` controls (`develop.set`). Every command acts on the active photo, or on `ids`.

use lightcraft_catalog::{Op, PhotoId};
use lightcraft_develop::{BaseSource, DevelopSettings, FilmAnalysis, FilmColorModel, FilmTone, FilmType, Negative, WbMode};
use lightcraft_geom::Rect;
use serde_json::{Value, json};

use super::{CommandSpec, bad, cmd, has_active, photo_ids};
use crate::{Result, Session, media::SourceLevel};

/// The photos a call acts on: `ids`, else the active photo.
fn targets(s: &Session, p: &Value, c: &str) -> Result<Vec<PhotoId>> {
    match photo_ids(p, c)? {
        Some(ids) if ids.is_empty() => Err(bad(c, "`ids` is empty")),
        Some(ids) => Ok(ids),
        None => s.active().map(|id| vec![id]).ok_or_else(|| bad(c, "no active photo")),
    }
}

/// `p[key]` read as the enum `T` by its JSON name; `None` when absent or null, an error naming the bad value.
fn enum_param<T: serde::de::DeserializeOwned + serde::Serialize>(p: &Value, key: &str, c: &str) -> Result<Option<T>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            let t: T = serde_json::from_value(v.clone()).map_err(|e| bad(c, format!("invalid `{key}`: {e}")))?;
            // (a name the enum doesn't know reads as its fallback: a typo is an error, not the fallback)
            if serde_json::to_value(&t).ok().as_ref() != Some(v) {
                return Err(bad(c, format!("invalid `{key}`: {v}")));
            }
            Ok(Some(t))
        }
    }
}

/// Measure `d`'s film on photo `id`'s source inside its crop (`sampled`: a base picked from the film edge).
fn analysis_of(s: &mut Session, id: PhotoId, d: &DevelopSettings, sampled: Option<([f64; 3], Rect)>, c: &str) -> Result<FilmAnalysis> {
    let src = s.source_now(id, SourceLevel::Preview).map_err(|e| bad(c, e))?;
    lightcraft_pipeline::negative::analyze(&src, &d.negative, d.negative_region(), sampled).map_err(|e| bad(c, e))
}

/// Commit `f` applied to each photo's settings as one undo step.
fn commit_each(
    s: &mut Session,
    ids: &[PhotoId],
    c: &str,
    label: &str,
    mut f: impl FnMut(&mut Session, PhotoId, &mut DevelopSettings) -> Result<()>,
) -> Result<Vec<(PhotoId, DevelopSettings)>> {
    let mut done = Vec::new();
    for id in ids {
        let Some(d) = s.develop_of(*id) else { return Err(bad(c, format!("no photo {}", id.0))) };
        let mut d = (*d).clone();
        f(s, *id, &mut d)?;
        done.push((*id, d));
    }
    let ops = done.iter().filter_map(|(id, d)| s.develop_op(*id, d.clone(), label)).collect();
    s.commit(label, Op::Batch { ops })?;
    Ok(done)
}

/// The command result for the photos `done`: each one's id and its conversion (film type, tone, colour model, lock
/// and stored analysis), as agents read it back.
fn report(done: &[(PhotoId, DevelopSettings)]) -> Value {
    let photos: Vec<Value> = done
        .iter()
        .map(|(id, d)| {
            let n = &d.negative;
            json!({"id": id.0, "film": n.film, "tone": n.tone, "colorModel": n.color_model, "locked": n.locked, "analysis": n.analysis})
        })
        .collect();
    json!({"photos": photos})
}

/// A converted photo's white balance starts at "as converted" (the relative scale's neutral).
fn reset_wb(d: &mut DevelopSettings) {
    d.wb.mode = WbMode::AsShot;
    d.wb.temp = 6500.0;
    d.wb.tint = 0.0;
}

/// A photo no longer converted gets its source's own white balance back, As Shot: the relative values set on the
/// positive would read as the raw's Kelvin.
fn restore_wb(s: &Session, id: PhotoId, d: &mut DevelopSettings) {
    let info = s.source_info(id);
    d.wb = if info.raw { DevelopSettings::for_raw(info.as_shot_temp, info.as_shot_tint) } else { DevelopSettings::default() }.wb;
}

/// `negative.convert`: switch the target photos to `film` (default colour negative), measure and store each one's
/// analysis, and, unless `keepEdits`, reset the edits made on the negative ([`DevelopSettings::reset_negative_conflicts`])
/// on a photo not converted before. One undo step.
fn convert(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "negative.convert";
    let ids = targets(s, p, C)?;
    let film = enum_param::<FilmType>(p, "film", C)?.unwrap_or(FilmType::ColorNegative);
    if !matches!(film, FilmType::ColorNegative | FilmType::BwNegative | FilmType::Slide) {
        return Err(bad(C, "`film` must be colorNegative, bwNegative or slide"));
    }
    let preset = match p.get("preset").and_then(Value::as_str) {
        Some(pid) => Some(s.presets.iter().find(|x| x.id == pid).cloned().ok_or_else(|| bad(C, format!("no preset `{pid}`")))?),
        None => None,
    };
    // edits made looking at the negative are reset unless the call says not to (`keepEdits`); a photo already
    // converted keeps them all (they were made on the positive)
    let keep = p.get("keepEdits").and_then(Value::as_bool).unwrap_or(false);
    let mut reset: Vec<&'static str> = Vec::new();
    let done = commit_each(s, &ids, C, "Convert Negative", |s, id, d| {
        if !keep {
            for name in d.negative_conflicts() {
                if !reset.contains(&name) {
                    reset.push(name);
                }
            }
            if !d.negative.converts() {
                d.reset_negative_conflicts();
            }
        }
        // (before the preset: a film preset makes the photo read as converted, and its white balance would then be
        // kept as if it were set on the positive)
        let was_converted = d.negative.converts();
        let film_before = d.negative.film;
        if let Some(pr) = &preset {
            *d = lightcraft_develop::apply_partial(d, &pr.settings, 1.0);
        }
        // (a `film` of null is no film named, as `enum_param` reads it)
        if !was_converted || p.get("film").is_some_and(|v| !v.is_null()) {
            d.negative.film = film;
        }
        d.set_section_enabled("negative", true);
        // a roll's analysis was measured for the film type it had: another type is measured on its own
        if d.negative.film != film_before {
            d.negative.locked = false;
        }
        // a roll-synced frame keeps the roll's analysis; otherwise keep a base sampled from the film edge and measure
        // everything else again
        if !(d.negative.locked && d.negative.analysis.is_some_and(|a| a.fits(d.negative.film))) {
            d.negative.locked = false;
            let sampled = d.negative.analysis.filter(|a| a.base_source == BaseSource::Sampled).and_then(|a| a.base_rect.map(|r| (a.base, r)));
            d.negative.analysis = Some(analysis_of(s, id, d, sampled, C)?);
        }
        // (a converted photo's white balance was set on the positive: kept)
        if !was_converted {
            reset_wb(d);
        }
        Ok(())
    })?;
    let mut out = report(&done);
    if let Some(o) = out.as_object_mut() {
        o.insert("reset".into(), json!(reset));
    }
    Ok(out)
}

/// What `negative.convert` would reset on these photos (see `DevelopSettings::negative_conflicts`), so the UI can ask
/// first.
fn conflicts(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "negative.conflicts";
    let ids = targets(s, p, C)?;
    let mut reset: Vec<&'static str> = Vec::new();
    for id in ids {
        for name in s.develop_of(id).map(|d| d.negative_conflicts()).unwrap_or_default() {
            if !reset.contains(&name) {
                reset.push(name);
            }
        }
    }
    Ok(json!({"reset": reset}))
}

/// `negative.analyze` (Re-analyze): measure each converted target again on its current crop and store the result;
/// a roll-synced (locked) photo keeps the roll's analysis while it still fits its film type.
fn analyze(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "negative.analyze";
    let ids = targets(s, p, C)?;
    let keep_sampled = p.get("keepSampled").and_then(Value::as_bool).unwrap_or(true);
    let done = commit_each(s, &ids, C, "Analyze Negative", |s, id, d| {
        if !d.negative.converts() {
            return Err(bad(C, format!("photo {} is not converted (negative.convert first)", id.0)));
        }
        // a roll-synced frame keeps the roll's numbers (negative.unlock first), unless they were measured for another
        // film type
        if d.negative.locked && d.negative.analysis.is_some_and(|a| a.fits(d.negative.film)) {
            return Ok(());
        }
        d.negative.locked = false;
        let sampled =
            d.negative.analysis.filter(|a| keep_sampled && a.base_source == BaseSource::Sampled).and_then(|a| a.base_rect.map(|r| (a.base, r)));
        d.negative.analysis = Some(analysis_of(s, id, d, sampled, C)?);
        Ok(())
    })?;
    Ok(report(&done))
}

/// `p.rect` as a normalised rectangle of the uncropped photo: finite, put in order and clipped to the photo; an error
/// when nothing of it is left.
fn rect_param(p: &Value, c: &str) -> Result<Rect> {
    let r = p.get("rect").ok_or_else(|| bad(c, "missing `rect` {x0, y0, x1, y1} (0..1 of the uncropped photo)"))?;
    let r: Rect = serde_json::from_value(r.clone()).map_err(|e| bad(c, format!("invalid `rect`: {e}")))?;
    if ![r.x0, r.y0, r.x1, r.y1].iter().all(|v| v.is_finite()) {
        return Err(bad(c, "`rect` must be finite numbers"));
    }
    let r = Rect::new(r.x0.min(r.x1), r.y0.min(r.y1), r.x0.max(r.x1), r.y0.max(r.y1)).intersect(&Rect::UNIT);
    if r.is_empty() {
        return Err(bad(c, "`rect` is empty or outside the photo"));
    }
    Ok(r)
}

/// `negative.sampleBase` (Film Base Selector): take the film base from `rect` on each converted target and measure the
/// range again with it, so the sampled base is stored and kept until the type changes to slide or the photo is reset.
fn sample_base(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "negative.sampleBase";
    let ids = targets(s, p, C)?;
    let shown = rect_param(p, C)?;
    let done = commit_each(s, &ids, C, "Sample Film Base", |s, id, d| {
        // the rectangle is on the photo as shown (its rotation applied); the source is before that rotation
        let r = d.orientation.inverse().map_norm_rect(shown);
        let src = s.source_now(id, SourceLevel::Preview).map_err(|e| bad(C, e))?;
        let base = lightcraft_pipeline::negative::sample_mean(&src, r).ok_or_else(|| bad(C, "no usable pixels in `rect`"))?;
        // (an unconverted photo is converted first, where its edits made on the negative are dealt with)
        if !d.negative.converts() {
            return Err(bad(C, format!("photo {} is not converted (negative.convert first)", id.0)));
        }
        // (slide film has no base to divide out: its conversion would ignore one)
        if d.negative.film == FilmType::Slide {
            return Err(bad(C, format!("photo {} is slide film, which has no film base", id.0)));
        }
        d.negative.analysis =
            Some(lightcraft_pipeline::negative::analyze(&src, &d.negative, d.negative_region(), Some((base, r))).map_err(|e| bad(C, e))?);
        // a base picked on this frame makes it its own, no longer the roll's
        d.negative.locked = false;
        Ok(())
    })?;
    Ok(report(&done))
}

/// `negative.set`: change the conversion options (film type, tone, colour model) of the targets. Switching a photo on
/// resets its negative-only edits and measures it; a change of film type measures it again for the new type (a
/// sampled base is kept, except for slide film).
fn set(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "negative.set";
    let ids = targets(s, p, C)?;
    let film = enum_param::<FilmType>(p, "film", C)?;
    let tone = enum_param::<FilmTone>(p, "tone", C)?;
    let model = enum_param::<FilmColorModel>(p, "colorModel", C)?;
    if film.is_none() && tone.is_none() && model.is_none() {
        return Err(bad(C, "give `film`, `tone` or `colorModel`"));
    }
    if film == Some(FilmType::Unknown) {
        return Err(bad(C, "`film` must be off, colorNegative, bwNegative or slide"));
    }
    let keep = p.get("keepEdits").and_then(Value::as_bool).unwrap_or(false);
    let mut reset: Vec<&'static str> = Vec::new();
    let done = commit_each(s, &ids, C, "Negative Conversion", |s, id, d| {
        if let Some(f) = film
            && f != d.negative.film
        {
            let was_converted = d.negative.converts();
            if !was_converted && matches!(f, FilmType::ColorNegative | FilmType::BwNegative | FilmType::Slide) && !keep {
                // switched on here: the edits made on the negative go, as with negative.convert
                for name in d.negative_conflicts() {
                    if !reset.contains(&name) {
                        reset.push(name);
                    }
                }
                d.reset_negative_conflicts();
            }
            // a range measured for another film type doesn't fit this one: measured again now and stored, keeping a
            // base picked on the film edge where it still applies
            let sampled = d
                .negative
                .analysis
                // (a B&W negative divides by the base too; a slide has none)
                .filter(|a| a.base_source == BaseSource::Sampled && f != FilmType::Slide)
                .and_then(|a| a.base_rect.map(|r| (a.base, r)));
            d.negative.film = f;
            d.negative.locked = false;
            d.negative.analysis = None;
            if d.negative.converts() {
                d.set_section_enabled("negative", true);
                d.negative.analysis = Some(analysis_of(s, id, d, sampled, C)?);
                if !was_converted {
                    reset_wb(d);
                }
            } else if was_converted {
                restore_wb(s, id, d);
            }
        }
        if let Some(t) = tone {
            d.negative.tone = t;
        }
        if let Some(m) = model {
            d.negative.color_model = m;
        }
        Ok(())
    })?;
    let mut out = report(&done);
    if let Some(o) = out.as_object_mut() {
        o.insert("reset".into(), json!(reset));
    }
    Ok(out)
}

/// `negative.reset`: the targets' conversion back to its defaults (off, no options or stored analysis).
fn reset(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "negative.reset";
    let ids = targets(s, p, C)?;
    let done = commit_each(s, &ids, C, "Reset Negative Conversion", |s, id, d| {
        if d.negative.converts() {
            restore_wb(s, id, d);
        }
        d.negative = Negative::default();
        d.set_section_enabled("negative", true);
        Ok(())
    })?;
    Ok(report(&done))
}

/// `negative.unlock`: drop the roll lock on the targets, so Re-analyze measures each frame on its own again.
fn unlock(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "negative.unlock";
    let ids = targets(s, p, C)?;
    let done = commit_each(s, &ids, C, "Unlock Roll Analysis", |_, _, d| {
        d.negative.locked = false;
        Ok(())
    })?;
    Ok(report(&done))
}

/// What a roll sync needs from the session: the frames (the selection or `ids`, at least two), the film type, each
/// frame's settings and a way to read its source off the UI thread. Made by [`roll_plan`], measured by
/// [`roll_measure`] (on a worker, in the app), written by [`roll_apply`].
pub struct RollPlan {
    pub frames: Vec<RollFrame>,
    pub film: FilmType,
    /// Reset the edits made on the negative on frames that weren't converted yet (`keepEdits` false).
    pub reset_edits: bool,
}

/// One frame of a [`RollPlan`].
pub struct RollFrame {
    pub id: PhotoId,
    /// The frame's settings as measured: as planned, with the roll's film type (its crop, border and any sampled base
    /// are measured).
    pub settings: DevelopSettings,
    /// The frame's settings as they were when the sync was planned, untouched: what [`roll_apply`] checks the frame
    /// against, so an edit made while the roll was read is never overwritten.
    pub original: DevelopSettings,
    pub source: crate::media::SourceRef,
}

/// Plan `negative.syncRoll` for `p` (`ids?`, `film?`, `keepEdits?`).
pub fn roll_plan(s: &mut Session, p: &Value) -> Result<RollPlan> {
    const C: &str = "negative.syncRoll";
    let mut seen = std::collections::HashSet::new();
    let ids: Vec<PhotoId> = match photo_ids(p, C)? {
        Some(ids) => ids,
        None => s.selected(),
    }
    .into_iter()
    .filter(|id| seen.insert(*id))
    .collect();
    if ids.len() < 2 {
        return Err(bad(C, "select at least two frames of the roll"));
    }
    let asked = match enum_param::<FilmType>(p, "film", C)? {
        Some(f @ (FilmType::ColorNegative | FilmType::BwNegative | FilmType::Slide)) => Some(f),
        None => None,
        Some(_) => return Err(bad(C, "`film` must be colorNegative, bwNegative or slide")),
    };
    // the film type: asked for, else the active frame's when it is one of the roll's (or the first converted frame's),
    // else colour negative
    let from_frames = s
        .active()
        .filter(|a| ids.contains(a))
        .into_iter()
        .chain(ids.iter().copied())
        .filter_map(|id| s.develop_of(id))
        .map(|d| d.negative.film)
        .find(|f| matches!(f, FilmType::ColorNegative | FilmType::BwNegative | FilmType::Slide));
    let film = asked.or(from_frames).unwrap_or(FilmType::ColorNegative);
    let mut frames = Vec::with_capacity(ids.len());
    for id in ids {
        let photo = s.catalog.photo(id).cloned().ok_or_else(|| bad(C, format!("no photo {}", id.0)))?;
        let original = (*s.develop_of(id).unwrap_or_default()).clone();
        let mut settings = original.clone();
        settings.negative.film = film;
        let source = s.media.source_ref(&photo, SourceLevel::Preview);
        frames.push(RollFrame { id, settings, original, source });
    }
    Ok(RollPlan { frames, film, reset_edits: !p.get("keepEdits").and_then(Value::as_bool).unwrap_or(false) })
}

/// Read every frame of `plan` and measure the roll (off the UI thread in the app), calling `progress(done)` after
/// each frame; stops with an error when `cancel` is set. A frame that can't be read or measured is left out (its id
/// and why come back in the list); an error when fewer than two frames could be measured.
pub fn roll_measure(plan: &RollPlan, cancel: &std::sync::atomic::AtomicBool, progress: &dyn Fn(usize)) -> std::result::Result<RollMeasured, String> {
    use lightcraft_pipeline::negative::{analyze_roll, measure_frame, thin_for_roll};
    let mut measured = Vec::with_capacity(plan.frames.len());
    let mut skipped = Vec::new();
    for (i, f) in plan.frames.iter().enumerate() {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err("stopped".into());
        }
        // a base sampled on a frame counts for the roll
        let sampled = f.settings.negative.analysis.filter(|a| a.base_source == BaseSource::Sampled).and_then(|a| a.base_rect.map(|r| (a.base, r)));
        let m = f.source.load_source().and_then(|src| measure_frame(&src.image, &f.settings.negative, f.settings.negative_region(), sampled));
        match m {
            Ok(mut m) => {
                thin_for_roll(&mut m, plan.frames.len());
                measured.push((f.id, m));
            }
            Err(e) => skipped.push((f.id, e)),
        }
        progress(i + 1);
    }
    if measured.len() < 2 {
        return Err(format!("only {} of {} frames could be measured", measured.len(), plan.frames.len()));
    }
    let (ids, frames): (Vec<PhotoId>, Vec<_>) = measured.into_iter().unzip();
    let analysis = analyze_roll(plan.film, &frames)?;
    let mixed = mixed_scans(&frames);
    Ok(RollMeasured { analysis, skipped, mixed, frames: ids.into_iter().zip(frames).collect() })
}

/// The warning a roll sync gives when its frames' film bases disagree (`base_spread` past [`MIXED_SCANS`]).
fn mixed_scans(frames: &[lightcraft_pipeline::negative::FrameMeasure]) -> Option<String> {
    let spread = lightcraft_pipeline::negative::base_spread(frames);
    (spread > MIXED_SCANS).then(|| {
        format!("the film bases found on these frames differ by {:.0} %: they look scanned differently (light, exposure or scanner), so one analysis may not fit them all", (spread - 1.0) * 100.0)
    })
}

/// Largest ratio between film bases found on a roll's frames before a sync warns that they look scanned differently.
const MIXED_SCANS: f32 = 1.15;

/// What [`roll_measure`] found.
pub struct RollMeasured {
    pub analysis: FilmAnalysis,
    /// Frames that couldn't be read or measured, and why (left out of the roll).
    pub skipped: Vec<(PhotoId, String)>,
    /// Set when the frames' film bases disagree (see `lightcraft_pipeline::negative::base_spread`).
    pub mixed: Option<String>,
    /// Each measured frame's samples (thinned), so the roll can be analysed again without frames left out at apply.
    pub frames: Vec<(PhotoId, lightcraft_pipeline::negative::FrameMeasure)>,
}

/// Write the roll's analysis `roll` to every measured frame of `plan` (each keeps its own region), locked, as one undo
/// step; frames not yet converted are converted (their edits made on the negative reset unless the plan keeps them).
pub fn roll_apply(s: &mut Session, plan: RollPlan, measured: RollMeasured) -> Result<Value> {
    const C: &str = "negative.syncRoll";
    let RollMeasured { analysis: mut roll, mut skipped, mut mixed, frames } = measured;
    // a frame edited in any way while the roll was read (its crop or film type, an unlock, or an edit the sync would
    // reset) keeps its newer edit: it is left out, and neither written nor counted in the roll's numbers
    let before = skipped.len();
    for f in &plan.frames {
        let now = s.develop_of(f.id);
        if !skipped.iter().any(|(k, _)| *k == f.id) && now.is_none_or(|d| *d != f.original) {
            skipped.push((f.id, "changed while the roll was read".into()));
        }
    }
    if skipped.len() > before {
        let kept: Vec<_> = frames.into_iter().filter(|(id, _)| !skipped.iter().any(|(k, _)| k == id)).map(|(_, m)| m).collect();
        if kept.len() >= 2 {
            roll = lightcraft_pipeline::negative::analyze_roll(plan.film, &kept).map_err(|e| bad(C, e))?;
            // (and the warning about mixed scans, from the frames written only)
            mixed = mixed_scans(&kept);
        }
    }
    let ids: Vec<PhotoId> = plan.frames.iter().map(|f| f.id).filter(|id| !skipped.iter().any(|(k, _)| k == id)).collect();
    if ids.len() < 2 {
        return Err(bad(C, "fewer than two frames are left to sync (the others changed while the roll was read, or couldn't be read)"));
    }
    let mut reset: Vec<&'static str> = Vec::new();
    let done = commit_each(s, &ids, C, "Sync Roll", |_, _, d| {
        if !d.negative.converts() {
            if plan.reset_edits {
                for name in d.negative_conflicts() {
                    if !reset.contains(&name) {
                        reset.push(name);
                    }
                }
                d.reset_negative_conflicts();
            }
            reset_wb(d);
        }
        d.negative.film = plan.film;
        d.set_section_enabled("negative", true);
        d.negative.analysis = Some(FilmAnalysis { region: Some(d.negative_region()), ..roll });
        d.negative.locked = true;
        Ok(())
    })?;
    let mut out = report(&done);
    if let Some(o) = out.as_object_mut() {
        o.insert("reset".into(), json!(reset));
        o.insert("skipped".into(), json!(skipped.iter().map(|(id, why)| json!({"id": id.0, "error": why})).collect::<Vec<_>>()));
        if let Some(w) = mixed {
            o.insert("warning".into(), json!(w));
        }
    }
    Ok(out)
}

/// `negative.syncRoll` in place (the app reads the frames in the background unless `wait`).
fn sync_roll(s: &mut Session, p: &Value) -> Result<Value> {
    let plan = roll_plan(s, p)?;
    let never = std::sync::atomic::AtomicBool::new(false);
    let measured = roll_measure(&plan, &never, &|_| ()).map_err(|e| bad("negative.syncRoll", e))?;
    roll_apply(s, plan, measured)
}

/// The `negative.*` commands, as the command palette, CLI, control channel and MCP list them.
pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            explicit_targets: true,
            ..cmd!(
                "negative.convert",
                "Convert Negative",
                // (the app's Photo ▸ Convert Negative… is `dialog.convertNegative`: it asks before resetting edits)
                [],
                None,
                "{film?: colorNegative|bwNegative|slide (default colorNegative), preset?: presetId, keepEdits?: bool, ids?: [photo]} — turns a film scan into a positive: finds the film base (on the film edge when the scan shows it), measures the density range and stores both, so preview and export match; white balance becomes relative (as converted = neutral). Edits made on the negative (Light, Tone Curve, White Balance, Color, mixers, Color Grading, Effects, Calibration, Profile, Masks) are reset unless keepEdits (see negative.conflicts); the reply lists them in `reset`",
                has_active,
                convert
            )
        },
        CommandSpec {
            explicit_targets: true,
            ..cmd!(
                query "negative.conflicts",
                "Edits Convert Would Reset",
                [],
                None,
                "{ids?: [photo]} — the panels whose edits negative.convert would reset (made on the negative); empty for photos already converted",
                has_active,
                conflicts
            )
        },
        CommandSpec {
            explicit_targets: true,
            ..cmd!(
                "negative.analyze",
                "Re-analyze Negative",
                ["Photo"],
                None,
                "{keepSampled?: bool (default true), ids?: [photo]} — measure the film base and density range again (after a crop, or a change of Analysis Border)",
                has_active,
                analyze
            )
        },
        CommandSpec {
            explicit_targets: true,
            ..cmd!(
                "negative.sampleBase",
                "Sample Film Base",
                [],
                None,
                "{rect: {x0, y0, x1, y1} (0..1 of the photo as shown, uncropped), ids?: [photo]} — set the film base of a converted photo from unexposed film (the rebate between frames) and re-measure the range",
                has_active,
                sample_base
            )
        },
        CommandSpec {
            explicit_targets: true,
            ..cmd!(
                "negative.set",
                "Negative Conversion Options",
                [],
                None,
                "{film?: off|colorNegative|bwNegative|slide, tone?: linear|soft|standard|hard, colorModel?: none|neutral|warm|cool, keepEdits?: bool, ids?: [photo]} — a new film type is measured again and stored; switching an unconverted photo on resets its edits made on the negative unless keepEdits (listed in `reset`); sliders are negative.* controls (develop.set)",
                has_active,
                set
            )
        },
        CommandSpec {
            explicit_targets: true,
            ..cmd!(
                "negative.syncRoll",
                "Sync Roll",
                [],
                None,
                "{ids?: [photo] (default: the selection, at least two), film?: colorNegative|bwNegative|slide, keepEdits?: bool, wait?: bool} — one analysis for the whole roll: the film base (sampled, found on the film edges, or the clearest estimate) and the density range measured on every frame together, written to every frame and locked (Re-analyze leaves it until negative.unlock); frames not yet converted are converted (edits made on the negative reset unless keepEdits). The app reads the frames in the background, with a row in the activity stack, unless wait; the reply lists `reset` and `skipped` frames",
                has_active,
                sync_roll
            )
        },
        CommandSpec {
            explicit_targets: true,
            ..cmd!(
                "negative.unlock",
                "Unlock Roll Analysis",
                [],
                None,
                "{ids?: [photo]} — a roll-synced frame becomes its own again: Re-analyze then measures it alone (its numbers are kept until then)",
                has_active,
                unlock
            )
        },
        CommandSpec {
            explicit_targets: true,
            ..cmd!(
                "negative.reset",
                "Reset Negative Conversion",
                ["Photo"],
                None,
                "{ids?: [photo]} — turn the conversion off and forget its analysis",
                has_active,
                reset
            )
        },
    ]
}
