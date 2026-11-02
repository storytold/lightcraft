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

fn enum_param<T: serde::de::DeserializeOwned>(p: &Value, key: &str, c: &str) -> Result<Option<T>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => serde_json::from_value(v.clone()).map(Some).map_err(|e| bad(c, format!("invalid `{key}`: {e}"))),
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

fn report(done: &[(PhotoId, DevelopSettings)]) -> Value {
    let photos: Vec<Value> = done
        .iter()
        .map(|(id, d)| {
            let n = &d.negative;
            json!({"id": id.0, "film": n.film, "tone": n.tone, "colorModel": n.color_model, "analysis": n.analysis})
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
        if let Some(pr) = &preset {
            *d = lightcraft_develop::apply_partial(d, &pr.settings, 1.0);
        }
        if !d.negative.converts() || p.get("film").is_some() {
            d.negative.film = film;
        }
        d.set_section_enabled("negative", true);
        // keep a base sampled from the film edge; measure everything else again
        let sampled = d.negative.analysis.filter(|a| a.base_source == BaseSource::Sampled).and_then(|a| a.base_rect.map(|r| (a.base, r)));
        d.negative.analysis = Some(analysis_of(s, id, d, sampled, C)?);
        reset_wb(d);
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

fn analyze(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "negative.analyze";
    let ids = targets(s, p, C)?;
    let keep_sampled = p.get("keepSampled").and_then(Value::as_bool).unwrap_or(true);
    let done = commit_each(s, &ids, C, "Analyze Negative", |s, id, d| {
        if !d.negative.converts() {
            return Err(bad(C, format!("photo {} is not converted (negative.convert first)", id.0)));
        }
        let sampled =
            d.negative.analysis.filter(|a| keep_sampled && a.base_source == BaseSource::Sampled).and_then(|a| a.base_rect.map(|r| (a.base, r)));
        d.negative.analysis = Some(analysis_of(s, id, d, sampled, C)?);
        Ok(())
    })?;
    Ok(report(&done))
}

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

fn sample_base(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "negative.sampleBase";
    let ids = targets(s, p, C)?;
    let shown = rect_param(p, C)?;
    let done = commit_each(s, &ids, C, "Sample Film Base", |s, id, d| {
        // the rectangle is on the photo as shown (its rotation applied); the source is before that rotation
        let r = d.orientation.inverse().map_norm_rect(shown);
        let src = s.source_now(id, SourceLevel::Preview).map_err(|e| bad(C, e))?;
        let base = lightcraft_pipeline::negative::sample_mean(&src, r).ok_or_else(|| bad(C, "no usable pixels in `rect`"))?;
        if !d.negative.converts() {
            d.negative.film = FilmType::ColorNegative;
            reset_wb(d);
        }
        d.negative.analysis =
            Some(lightcraft_pipeline::negative::analyze(&src, &d.negative, d.negative_region(), Some((base, r))).map_err(|e| bad(C, e))?);
        Ok(())
    })?;
    Ok(report(&done))
}

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
    let done = commit_each(s, &ids, C, "Negative Conversion", |_, _, d| {
        if let Some(f) = film {
            if f != d.negative.film {
                // a range measured for another film type doesn't fit this one: measured again at render time
                // (negative.analyze stores it)
                d.negative.analysis = d.negative.analysis.filter(|a| a.base_source == BaseSource::Sampled && f == FilmType::ColorNegative);
                if !d.negative.converts() {
                    reset_wb(d);
                }
            }
            d.negative.film = f;
        }
        if let Some(t) = tone {
            d.negative.tone = t;
        }
        if let Some(m) = model {
            d.negative.color_model = m;
        }
        Ok(())
    })?;
    Ok(report(&done))
}

fn reset(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "negative.reset";
    let ids = targets(s, p, C)?;
    let done = commit_each(s, &ids, C, "Reset Negative Conversion", |_, _, d| {
        d.negative = Negative::default();
        d.set_section_enabled("negative", true);
        Ok(())
    })?;
    Ok(report(&done))
}

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
                "{rect: {x0, y0, x1, y1} (0..1 of the photo as shown, uncropped), ids?: [photo]} — set the film base from unexposed film (the rebate between frames) and re-measure the range",
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
                "{film?: off|colorNegative|bwNegative|slide, tone?: linear|soft|standard|hard, colorModel?: none|neutral|warm|cool, ids?: [photo]} — sliders are negative.* controls (develop.set)",
                has_active,
                set
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
