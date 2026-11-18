//! Masking and Remove (spot) commands on the active photo.

use lcc_develop::{
    BRUSH_MODEL_LEGACY, BRUSH_MODEL_LRC, BrushStroke, LocalAdjustments, MAX_POINT_COLORS, MAX_RED_EYES, MIXER_BANDS, Mask, MaskComponent, MaskOp,
    MaskShape, Mixer, PointColor, RedEye, Spot, SpotMode,
};
use lcc_geom::Point;
use serde_json::{Value, json};

use super::{CommandSpec, bad, bool_or, cmd, f64_or, has_active, ok, point, str_param};
use crate::segment::{Click, MAX_CLICKS};
use crate::{Result, Session};

/// A colour-range mask compares every pixel against every sample, so render and export cost is
/// pixels x samples. The interactive sampler stops at 5; automation (`mask.add`/`mask.update`)
/// passed its `samples` array straight through, so a few hundred samples on a large render hung.
const MAX_COLOR_RANGE_SAMPLES: usize = 64;

/// Caps a colour-range shape's sample list at [`MAX_COLOR_RANGE_SAMPLES`] (render cost is pixels x samples).
fn cap_color_range_samples(shape: &mut MaskShape) {
    if let MaskShape::ColorRange { samples, .. } = shape {
        samples.truncate(MAX_COLOR_RANGE_SAMPLES);
    }
}

fn shape_from(kind: &str, p: &Value, c: &str) -> Result<MaskShape> {
    Ok(match kind {
        "brush" => MaskShape::Brush { strokes: vec![] },
        "linear" => {
            MaskShape::Linear { start: point(p, "start").unwrap_or(Point::new(0.5, 0.25)), end: point(p, "end").unwrap_or(Point::new(0.5, 0.6)) }
        }
        "radial" => MaskShape::Radial {
            center: point(p, "center").unwrap_or(Point::new(0.5, 0.5)),
            rx: f64_or(p, "rx", 0.22),
            ry: f64_or(p, "ry", 0.16),
            angle: f64_or(p, "angle", 0.0),
            feather: f64_or(p, "feather", 50.0),
            invert: bool_or(p, "invert", false),
        },
        "sky" => MaskShape::Sky,
        "subject" => MaskShape::Subject,
        "background" => MaskShape::Background,
        "luminanceRange" => MaskShape::LuminanceRange {
            lo: f64_or(p, "lo", 0.6),
            hi: f64_or(p, "hi", 1.0),
            lo_feather: f64_or(p, "loFeather", 0.1),
            hi_feather: f64_or(p, "hiFeather", 0.1),
        },
        "depthRange" => {
            let lo = f64_or(p, "lo", 0.0).clamp(0.0, 1.0);
            let hi = f64_or(p, "hi", 0.35).clamp(0.0, 1.0);
            MaskShape::DepthRange { lo: lo.min(hi), hi: lo.max(hi), feather: f64_or(p, "feather", 0.1).clamp(0.0, 1.0) }
        }
        "colorRange" => {
            let samples: Vec<[f64; 3]> = p
                .get("samples")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|s| Some([s.get(0)?.as_f64()?, s.get(1)?.as_f64()?, s.get(2)?.as_f64()?]))
                        .take(MAX_COLOR_RANGE_SAMPLES)
                        .collect()
                })
                .unwrap_or_default();
            MaskShape::ColorRange { samples, refine: f64_or(p, "refine", 50.0), areas: Vec::new() }
        }
        "object" => MaskShape::Object { hint: points(p, "points"), exclude: points(p, "exclude"), seg: seg_param(p), detail: vec![], edge: 0.0 },
        "prompt" => {
            MaskShape::Prompt { text: str_param(p, "text").unwrap_or_default().trim().to_string(), seg: seg_param(p), detail: vec![], edge: 0.0 }
        }
        "people" => {
            let parts: Vec<String> = p
                .get("parts")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).map(|s| s.trim().to_string()).take(lcc_develop::MAX_PEOPLE_PARTS).collect())
                .unwrap_or_default();
            if let Some(odd) = parts.iter().find(|n| !PEOPLE_PARTS.iter().any(|k| k.eq_ignore_ascii_case(n))) {
                return Err(bad(c, format!("unknown People part `{odd}` ({})", PEOPLE_PARTS.join("|"))));
            }
            let person = p.get("person").and_then(Value::as_u64).map_or(0, |n| u32::try_from(n).unwrap_or(u32::MAX));
            let who = match p.get("who") {
                None | Some(Value::Null) => None,
                Some(v) => Some(serde_json::from_value(v.clone()).map_err(|e| bad(c, format!("who: {e}")))?),
            };
            MaskShape::People { person, parts, who, seg: seg_param(p) }
        }
        "landscape" => {
            let raw = str_param(p, "class").map(str::trim).unwrap_or_default();
            let class =
                landscape_class(raw).ok_or_else(|| bad(c, format!("landscape: `class` is one of {} (not `{raw}`)", LANDSCAPE_CLASSES.join(", "))))?;
            MaskShape::Landscape { class: class.to_string() }
        }
        "concept" => {
            let name = str_param(p, "name").map(str::trim).unwrap_or_default();
            if name.is_empty() || name.chars().count() > 200 {
                return Err(bad(c, "concept: `name` is a remembered object's name (concepts.list)"));
            }
            MaskShape::Concept { name: name.to_string(), seg: seg_param(p) }
        }
        other => {
            return Err(bad(
                c,
                format!(
                    "unknown mask kind `{other}` (brush|linear|radial|sky|subject|background|luminanceRange|colorRange|depthRange|object|prompt|people|landscape|concept)"
                ),
            ));
        }
    })
}

/// The Landscape classes `mask.add {kind: "landscape"}` takes ("Select Landscape"): the first three the photo analysis
/// finds, the rest need the EoMT scene model.
pub const LANDSCAPE_CLASSES: &[&str] = &["sky", "water", "vegetation", "mountains", "architecture", "natural ground", "artificial ground"];

/// The canonical Landscape class a name means (`Mountain`, `building`, `sand`, `road`…), `None` for anything else.
fn landscape_class(raw: &str) -> Option<&'static str> {
    let c = raw.to_ascii_lowercase();
    if c.is_empty() || c.len() > 40 {
        return None;
    }
    if c.contains("sky") {
        Some("sky")
    } else if c.contains("water") || c.contains("sea") || c.contains("river") || c.contains("lake") {
        Some("water")
    } else if let Some(t) = lcc_pipeline::auto_mask::terrain_class(&c) {
        Some(t)
    } else if c.contains("vegetation") || c.contains("plant") || c.contains("tree") || c.contains("grass") {
        Some("vegetation")
    } else {
        None
    }
}

/// The People parts `mask.add {kind: "people"}` takes (Lightroom's names; none: Entire Person).
const PEOPLE_PARTS: &[&str] =
    &["Entire Person", "Face Skin", "Body Skin", "Eyebrows", "Eye Sclera", "Iris and Pupil", "Eyes", "Lips", "Teeth", "Hair", "Clothes"];

/// `[[x, y], …]` normalized points under `key`.
fn points(p: &Value, key: &str) -> Vec<Point> {
    p.get(key)
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|q| Some(Point::new(q.get(0)?.as_f64()?, q.get(1)?.as_f64()?))).collect())
        .unwrap_or_default()
}

/// A segmentation passed in (`seg`: a stored or replayed AI mask; the model isn't needed).
fn seg_param(p: &Value) -> Option<lcc_develop::SegMask> {
    p.get("seg").and_then(|v| serde_json::from_value(v.clone()).ok())
}

/// What a new AI shape still needs once it is added.
enum Later {
    Nothing,
    /// Background mode: these Object clicks are segmented on the worker.
    Clicks(Vec<Point>, Vec<Point>),
    /// Background mode: the Describe selection is computed first; the mask is added when found.
    Text(String),
    /// Background mode: the People or remembered-object selection is computed on the AI worker
    /// after the command (Recompute AI Masks).
    Recompute,
}

/// Compute the segmentation of a new AI shape (Object with clicks, Prompt) — or, in background
/// mode, say what to queue. An Object without clicks stays empty until the photo is clicked
/// (the photo is analyzed meanwhile); a Prompt that matches nothing is an error. Shapes given
/// with their `seg` need no model.
fn resolve_ai(s: &mut Session, shape: &mut MaskShape, c: &str) -> Result<Later> {
    let id = s.active().ok_or_else(|| bad(c, "no active photo"))?;
    let background = s.segmenter.background;
    match shape {
        MaskShape::Object { seg: Some(_), .. } | MaskShape::Prompt { seg: Some(_), .. } => {}
        MaskShape::Object { hint, exclude, .. } if !hint.is_empty() && background => {
            // refused here, not when the worker later fails at the model's limit after "pending" (upstream #3717)
            if hint.len() + exclude.len() > MAX_CLICKS {
                return Err(ai_err(format!("an Object selection takes at most {MAX_CLICKS} clicks")));
            }
            s.segmenter.require_object_model()?;
            return Ok(Later::Clicks(std::mem::take(hint), std::mem::take(exclude)));
        }
        MaskShape::Object { hint, exclude, seg, .. } if !hint.is_empty() => {
            *seg = Some(segment_object(s, |s| s.segment_clicks(id, &clicks_of(hint, exclude)))?);
        }
        MaskShape::Object { .. } => {
            // the clicks come later: refuse an Object mask that could never be computed (no
            // model) rather than leave an empty one; in the app, analyze the photo meanwhile
            s.segmenter.require_object_model()?;
            if background {
                segment_object(s, |s| s.segment_prepare(id))?;
            }
        }
        MaskShape::Prompt { text, .. } if background => {
            s.segmenter.describe_model()?;
            if text.trim().is_empty() {
                return Err(ai_err("describe what to select"));
            }
            return Ok(Later::Text(text.clone()));
        }
        MaskShape::Prompt { text, seg, .. } => {
            let found = segment(s, |s| s.segment_text(id, text))?;
            *seg = Some(found.ok_or_else(|| ai_err(format!("nothing matching “{text}” was found in this photo")))?);
        }
        // a depth range needs the depth model (its grid is computed when the photo renders)
        MaskShape::DepthRange { .. } => {
            s.segmenter.require_depth_model()?;
        }
        // the people models when installed, else the heuristic (no model needed)
        MaskShape::People { seg: None, .. } if background => return Ok(Later::Recompute),
        MaskShape::Concept { name, seg: None } if background => {
            s.segmenter.require_sam2()?;
            if !s.concept_store().get(name).is_some_and(|c| !c.examples.is_empty()) {
                return Err(ai_err(format!("no remembered object called “{name}” with examples")));
            }
            return Ok(Later::Recompute);
        }
        MaskShape::Concept { name, seg: seg @ None } => {
            s.segmenter.require_sam2()?;
            *seg = Some(s.segment_concept(id, name).map_err(ai_err)?.ok_or_else(|| ai_err(format!("“{name}” wasn't found on this photo")))?);
        }
        // (a named person is resolved on the photo first: Recompute AI Masks)
        MaskShape::People { person, parts, seg: seg @ None, who: None } => {
            *seg = s.segment_people(id, *person, parts).map_err(ai_err)?;
        }
        _ => {}
    }
    Ok(Later::Nothing)
}

/// Queue what a new AI component (component `comp` of mask `mask`) still needs.
fn queue_later(s: &mut Session, later: Later, mask: u32, comp: usize, mut out: Value) -> Result<Value> {
    if let Later::Recompute = later {
        if let Some(id) = s.active() {
            s.want_missing_masks(id);
        }
        return Ok(out);
    }
    if let Later::Clicks(hint, exclude) = later {
        let id = s.active().ok_or_else(|| ai_err("no active photo"))?;
        segment_object(s, |s| s.segment_clicks_later(id, mask, comp, hint, exclude))?;
        out["pending"] = json!(true);
    }
    Ok(out)
}

/// AI mask failures are shown as they are (not as "invalid parameters").
fn ai_err(msg: impl Into<String>) -> crate::EngineError {
    crate::EngineError::Other(msg.into())
}

/// A Describe call: the models are checked first ([`crate::segment::Segmenter::describe_model`]: SAM 3, else the
/// OWLv2 finder and SAM 2.1), so a missing one is [`crate::EngineError::ModelMissing`] (the segmenter's own errors
/// are text).
fn segment<T>(s: &mut Session, f: impl FnOnce(&mut Session) -> std::result::Result<T, String>) -> Result<T> {
    s.segmenter.describe_model()?;
    f(s).map_err(ai_err)
}

/// [`segment`] for Object clicks, which use the model the user chose (`ai.models.setObjectModel`).
fn segment_object<T>(s: &mut Session, f: impl FnOnce(&mut Session) -> std::result::Result<T, String>) -> Result<T> {
    s.segmenter.require_object_model()?;
    f(s).map_err(ai_err)
}

pub(crate) fn clicks_of(include: &[Point], exclude: &[Point]) -> Vec<Click> {
    include.iter().map(|p| Click { at: *p, include: true }).chain(exclude.iter().map(|p| Click { at: *p, include: false })).collect()
}

/// A mask selects nothing when it has no components, or every component is a brush without a stroke.
pub(crate) fn mask_is_empty(m: &Mask) -> bool {
    m.components.iter().all(|c| matches!(&c.shape, MaskShape::Brush { strokes, .. } if strokes.is_empty()))
}

fn masks_edit(s: &mut Session, c: &str, label: &str, f: impl FnOnce(&mut Vec<Mask>, &mut Option<u32>) -> Result<()>) -> Result<Value> {
    let id = s.active().ok_or_else(|| bad(c, "no active photo"))?;
    let mut d = (*s.develop_of(id).unwrap_or_default()).clone();
    let before = d.masks.clone();
    let mut active = s.active_mask;
    f(&mut d.masks, &mut active)?;
    // automatic names follow what the masks select (until the photographer names one)
    s.refresh_mask_names(id, &before, &mut d.masks);
    s.set_develop(id, d, label)?;
    s.active_mask = active;
    Ok(json!({"activeMask": active}))
}

/// Most pieces a tapered ⇧-click segment is cut into.
const MAX_TAPER_STEPS: usize = 64;

/// The strokes a ⇧-click adds: `to` drawn on from point `from`, the last point of stroke `last` (a straight segment
/// to `to`'s first point, then the rest of `to`). When the brush's size, feather or flow changed since `last`, the
/// segment is cut into pieces whose values step from `last`'s to `to`'s, so the line tapers; strokes combine by their
/// maximum, so the pieces join without building up.
fn connected_strokes(from: Point, last: &BrushStroke, to: BrushStroke) -> Vec<BrushStroke> {
    let Some(&first) = to.points.first() else { return vec![to] };
    let same = |a: f64, b: f64| (a - b).abs() <= 1e-9;
    // pen pressure along the line: the last dab's at its start, the click's at its end (none when neither has any)
    let pressured = !last.pressure.is_empty() || !to.pressure.is_empty();
    let (p_from, p_to) = (last.pressure_at(0), to.pressure_at(0));
    if same(last.size, to.size) && same(last.feather, to.feather) && same(last.flow, to.flow) {
        let mut points = Vec::with_capacity(to.points.len() + 1);
        points.push(from);
        points.extend(to.points.iter().copied());
        let pressure = if pressured { std::iter::once(p_from).chain((0..to.points.len()).map(|i| to.pressure_at(i))).collect() } else { Vec::new() };
        return vec![BrushStroke { points, pressure, ..to }];
    }
    // about 5% of size, or 2 points of feather or flow, per piece
    let ratio = if last.size > 0.0 && to.size > 0.0 { (to.size / last.size).ln().abs() / 1.05f64.ln() } else { 0.0 };
    let needed = [ratio, (to.feather - last.feather).abs() / 2.0, (to.flow - last.flow).abs() / 2.0]
        .into_iter()
        .filter(|v| v.is_finite())
        .fold(1.0f64, f64::max);
    let steps = (needed.ceil() as usize).clamp(1, MAX_TAPER_STEPS);
    let lerp = |a: f64, b: f64, t: f64| a + (b - a) * t;
    let at = |t: f64| Point::new(lerp(from.x, first.x, t), lerp(from.y, first.y, t));
    let mut out: Vec<BrushStroke> = (0..steps)
        .map(|i| {
            let (t0, t1) = (i as f64 / steps as f64, (i + 1) as f64 / steps as f64);
            let p = |t: f64| (f64::from(p_from) + (f64::from(p_to) - f64::from(p_from)) * t) as f32;
            BrushStroke {
                points: vec![at(t0), at(t1)],
                size: lerp(last.size, to.size, t1),
                feather: lerp(last.feather, to.feather, t1),
                flow: lerp(last.flow, to.flow, t1),
                pressure: if pressured { vec![p(t0), p(t1)] } else { Vec::new() },
                ..to.clone()
            }
        })
        .collect();
    if to.points.len() > 1 {
        out.push(to);
    }
    out
}

/// `p.id` as a mask id: absent / null is `None`; anything but an integer that fits a mask id is an error (a
/// `u64` cast with `as u32` made 2^32 + k alias mask k, and a non-number silently meant the active mask).
pub(crate) fn mask_id_param(p: &Value, c: &str) -> Result<Option<u32>> {
    match p.get("id") {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .map(Some)
            .ok_or_else(|| bad(c, format!("`id` must be a mask id (a non-negative integer), got {v}"))),
    }
}

fn mask_id(p: &Value, active: Option<u32>, c: &str) -> Result<u32> {
    mask_id_param(p, c)?.or(active).ok_or_else(|| bad(c, "no mask selected (give `id`)"))
}

/// The slider of a `mask.mixer` key (`<band>.<hue|sat|lum>`, as the photo's `mixer.<band>.<…>` controls).
fn mixer_field<'a>(m: &'a mut Mixer, key: &str) -> Option<&'a mut f64> {
    let (band, attr) = key.split_once('.')?;
    let b = match band {
        "red" => &mut m.red,
        "orange" => &mut m.orange,
        "yellow" => &mut m.yellow,
        "green" => &mut m.green,
        "aqua" => &mut m.aqua,
        "blue" => &mut m.blue,
        "purple" => &mut m.purple,
        "magenta" => &mut m.magenta,
        _ => return None,
    };
    Some(match attr {
        "hue" => &mut b.hue,
        "sat" => &mut b.sat,
        "lum" => &mut b.lum,
        _ => return None,
    })
}

/// A mask's Color Mixer as the commands report it: `{"<band>.<hue|sat|lum>": value}`.
pub fn mixer_json(m: &Mixer) -> Value {
    let mut o = serde_json::Map::new();
    for (band, b) in MIXER_BANDS.iter().zip(m.bands()) {
        for (attr, v) in [("hue", b.hue), ("sat", b.sat), ("lum", b.lum)] {
            o.insert(format!("{band}.{attr}"), json!(v));
        }
    }
    Value::Object(o)
}

/// `mask.mixer`'s `values`, checked before anything changes: each key a band's slider, each value
/// a finite number (clamped to −100..100).
fn mixer_values(p: &Value, c: &str) -> Result<Vec<(String, f64)>> {
    let Some(v) = p.get("values") else { return Ok(Vec::new()) };
    let o = v.as_object().ok_or_else(|| bad(c, "`values` must be {\"<band>.<hue|sat|lum>\": number, …}"))?;
    let mut probe = Mixer::default();
    o.iter()
        .take(64)
        .map(|(k, v)| {
            if mixer_field(&mut probe, k).is_none() {
                return Err(bad(c, format!("unknown slider `{k}` (<band>.<hue|sat|lum>, band one of {})", MIXER_BANDS.join(", "))));
            }
            let v = v.as_f64().filter(|v| v.is_finite()).ok_or_else(|| bad(c, format!("`{k}` must be a number")))?;
            Ok((k.clone(), v.clamp(-100.0, 100.0)))
        })
        .collect()
}

fn find(masks: &mut [Mask], id: u32, c: &str) -> Result<usize> {
    masks.iter().position(|m| m.id == id).ok_or_else(|| bad(c, format!("no mask {id}")))
}

/// Why a spot that shows a generated fill is not moved or resized.
pub const GENERATED_STAYS: &str = "a generated fill stays where it was made: paint a new area to fill somewhere else";
/// A colour range keeps at most this many colours.
const MAX_COLOR_SAMPLES: usize = lcc_develop::MAX_COLOR_SAMPLES;
/// The place of a colour sample picked before the pins existed.
const UNKNOWN_AREA: [f64; 4] = lcc_develop::UNKNOWN_COLOR_AREA;

/// The picked place: `rect: [x0, y0, x1, y1]` (an area dragged, in either direction) or `x, y` (a
/// click: a point), normalized; checked to lie on the photo.
fn area_param(p: &Value, c: &str) -> Result<[f64; 4]> {
    let area = match p.get("rect").and_then(Value::as_array) {
        Some(r) => {
            let v: Vec<f64> = r.iter().filter_map(Value::as_f64).collect();
            let [x0, y0, x1, y1] = v[..] else { return Err(bad(c, "rect is [x0, y0, x1, y1]")) };
            [x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)]
        }
        None => {
            let (x, y) = (super::f64_req(p, "x", c)?, super::f64_req(p, "y", c)?);
            [x, y, x, y]
        }
    };
    let on_photo = |v: f64| (-0.01..=1.01).contains(&v);
    if !area.iter().all(|v| on_photo(*v)) {
        return Err(bad(c, "the point is outside the photo"));
    }
    Ok(area.map(|v| v.clamp(0.0, 1.0)))
}

/// The selected mask's colour range component (`comp`, else its first) and where its samples were
/// picked.
fn color_range_of(s: &Session, mid: u32, comp: Option<usize>, c: &str) -> Result<(usize, Vec<[f64; 4]>)> {
    let id = s.active().ok_or_else(|| bad(c, "no active photo"))?;
    let d = s.develop_of(id).unwrap_or_default();
    let m = d.masks.iter().find(|m| m.id == mid).ok_or_else(|| bad(c, format!("no mask {mid}")))?;
    let k = comp
        .or_else(|| m.components.iter().position(|x| matches!(x.shape, MaskShape::ColorRange { .. })))
        .ok_or_else(|| bad(c, "the mask has no colour range"))?;
    match m.components.get(k).map(|x| &x.shape) {
        Some(MaskShape::ColorRange { samples, areas, .. }) => {
            let mut areas = areas.clone();
            areas.resize(samples.len(), UNKNOWN_AREA);
            Ok((k, areas))
        }
        _ => Err(bad(c, "not a colour range component")),
    }
}

/// The active photo's pixels in `area` as the range pickers see them
/// ([`lcc_pipeline::range_area_pixels`], on a small render).
fn area_pixels(s: &mut Session, area: [f64; 4], c: &str) -> Result<Vec<([f64; 3], f64)>> {
    let id = s.active().ok_or_else(|| bad(c, "no active photo"))?;
    let src = s.source_now(id, crate::media::SourceLevel::Thumb).map_err(|e| bad(c, e))?;
    let info = s.source_info(id);
    let d = s.develop_of(id).unwrap_or_default();
    let req = lcc_pipeline::RenderRequest::fit(384, 384);
    lcc_pipeline::range_area_pixels(&src, &info, &d, &req, area).ok_or_else(|| bad(c, "the point is outside the photo"))
}

/// The colours to sample in `area`: a click, the mean round it; an area, up to `room` (at most 3)
/// of its main colours.
fn sample_area_colours(s: &mut Session, area: [f64; 4], room: usize, c: &str) -> Result<Vec<[f64; 3]>> {
    let px: Vec<[f64; 3]> = area_pixels(s, area, c)?.into_iter().map(|(lab, _)| lab).collect();
    let point = area[0] == area[2] && area[1] == area[3];
    let k = if point { 1 } else { room.clamp(1, 3) };
    let out = colour_clusters(&px, k);
    if out.is_empty() { Err(bad(c, "no colour there")) } else { Ok(out) }
}

/// Up to `k` main colours of `px` (OkLab): k-means from the mean and then the colours farthest from
/// those chosen; a cluster of under 10 % of the pixels is dropped (a few stray pixels at the edge
/// of a dragged area don't become a sample of their own). Deterministic.
pub(crate) fn colour_clusters(px: &[[f64; 3]], k: usize) -> Vec<[f64; 3]> {
    let dist = |a: &[f64; 3], b: &[f64; 3]| (0..3).map(|i| (a[i] - b[i]).powi(2)).sum::<f64>();
    let mean = |pts: &mut dyn Iterator<Item = &[f64; 3]>| {
        let (mut acc, mut n) = ([0.0f64; 3], 0usize);
        for p in pts {
            for i in 0..3 {
                acc[i] += p[i];
            }
            n += 1;
        }
        (n > 0).then(|| acc.map(|v| v / n as f64))
    };
    let Some(m) = mean(&mut px.iter()) else { return Vec::new() };
    let mut centres = vec![m];
    while centres.len() < k.max(1) {
        let far = px.iter().max_by(|a, b| {
            let da = centres.iter().map(|c| dist(a, c)).fold(f64::MAX, f64::min);
            let db = centres.iter().map(|c| dist(b, c)).fold(f64::MAX, f64::min);
            da.total_cmp(&db)
        });
        match far {
            Some(f) if centres.iter().all(|c| dist(f, c) > 1e-6) => centres.push(*f),
            _ => break,
        }
    }
    let labels_of = |centres: &[[f64; 3]]| -> Vec<usize> {
        px.iter().map(|p| centres.iter().enumerate().min_by(|a, b| dist(p, a.1).total_cmp(&dist(p, b.1))).map_or(0, |(i, _)| i)).collect()
    };
    for _ in 0..10 {
        let labels = labels_of(&centres);
        for (i, c) in centres.iter_mut().enumerate() {
            if let Some(m) = mean(&mut px.iter().zip(&labels).filter(|(_, l)| **l == i).map(|(p, _)| p)) {
                *c = m;
            }
        }
    }
    let labels = labels_of(&centres);
    let members: Vec<usize> = (0..centres.len()).map(|i| labels.iter().filter(|l| **l == i).count()).collect();
    let min = px.len() / 10;
    let mut out: Vec<([f64; 3], usize)> = centres.into_iter().zip(members).filter(|(_, n)| *n > 0 && *n >= min).collect();
    // the biggest first
    out.sort_by_key(|c| std::cmp::Reverse(c.1));
    out.into_iter().map(|(c, _)| c).collect()
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "mask.add",
            "Create New Mask",
            [],
            None,
            "{kind: brush|linear|radial|sky|subject|background|landscape|luminanceRange|colorRange|object|prompt, ...shape params (landscape: class sky|water|vegetation|mountains|architecture|natural ground|artificial ground, loose names like building, sand or road work; the last four need the EoMT scene model, else ModelMissing) (start/end, center/rx/ry/angle/feather, lo/hi…; object: points/exclude [[x,y],…]; prompt: text; concept: name (a remembered object, found on each photo); people: person? (0 everyone, n the n-th from the left), parts? [Entire Person|Face Skin|Body Skin|Eyebrows|Eye Sclera|Iris and Pupil|Eyes|Lips|Teeth|Hair|Clothes], who? {name}|{role}; object/prompt/people: seg? a stored segmentation, no model needed), name?}: AI kinds need the SAM 3 model; in the app a prompt returns {pending} and the mask appears when found",
            has_active,
            |s, p| {
                let kind = str_param(p, "kind").unwrap_or("radial").to_string();
                let mut shape = shape_from(&kind, p, "mask.add")?;
                // Landscape beyond sky, water and vegetation is the scene model's alone (the photo analysis can't tell it apart)
                if let MaskShape::Landscape { class } = &shape
                    && !matches!(class.as_str(), "sky" | "water" | "vegetation")
                {
                    s.segmenter.require_scene_model()?;
                }
                let later = resolve_ai(s, &mut shape, "mask.add")?;
                let name = str_param(p, "name").map(str::to_string);
                if let Later::Text(text) = &later {
                    let id = s.active().ok_or_else(|| bad("mask.add", "no active photo"))?;
                    segment(s, |s| s.segment_text_later(id, None, "add", name, text))?;
                    return Ok(json!({"pending": true}));
                }
                let next = s.active().and_then(|id| s.develop_of(id)).map(|d| d.next_mask_id()).unwrap_or(1);
                // the next-mask settings (what was dialed in with no mask selected)
                let adjust = super::mask_effects::new_mask_adjust(s, &kind);
                let out = masks_edit(s, "mask.add", "Add Mask", |masks, active| {
                    masks.push(Mask {
                        id: next,
                        name: name.unwrap_or_else(|| format!("Mask {next}")),
                        components: vec![MaskComponent { anchor: None, name: None, op: MaskOp::Add, invert: false, shape }],
                        adjust,
                        ..Default::default()
                    });
                    *active = Some(next);
                    Ok(())
                })?;
                queue_later(s, later, next, 0, out)
            }
        ),
        cmd!(
            "mask.addComponent",
            "Add/Subtract/Intersect Mask",
            [],
            None,
            "{id?: maskId, op: add|subtract|intersect, kind, ...shape params}",
            has_active,
            |s, p| {
                let kind = str_param(p, "kind").unwrap_or("brush").to_string();
                let op: MaskOp =
                    serde_json::from_value(p.get("op").cloned().unwrap_or(json!("add"))).map_err(|e| bad("mask.addComponent", e.to_string()))?;
                let active = s.active_mask;
                let mid = mask_id(p, active, "mask.addComponent")?;
                let id = s.active().ok_or_else(|| bad("mask.addComponent", "no active photo"))?;
                if !s.develop_of(id).is_some_and(|d| d.masks.iter().any(|m| m.id == mid)) {
                    return Err(bad("mask.addComponent", format!("no mask {mid}")));
                }
                let mut shape = shape_from(&kind, p, "mask.addComponent")?;
                let later = resolve_ai(s, &mut shape, "mask.addComponent")?;
                if let Later::Text(text) = &later {
                    let op = p.get("op").and_then(Value::as_str).unwrap_or("add");
                    segment(s, |s| s.segment_text_later(id, Some(mid), op, None, text))?;
                    return Ok(json!({"pending": true}));
                }
                let mut comp = 0;
                let out = masks_edit(s, "mask.addComponent", "Edit Mask", |masks, _| {
                    let i = find(masks, mid, "mask.addComponent")?;
                    masks[i].components.push(MaskComponent { anchor: None, name: None, op, invert: bool_or(p, "invert", false), shape });
                    comp = masks[i].components.len() - 1;
                    Ok(())
                })?;
                queue_later(s, later, mid, comp, out)
            }
        ),
        cmd!(
            "mask.brushStroke",
            "Brush Stroke",
            [],
            None,
            "{id?: maskId, component?: index, points: [[x,y],…] normalized, size?: the solid core's radius, fraction of long edge (0.03), feather?: 0..100, the soft halo outside the core, flow?, density?, erase?: bool, model?: 1 Lightroom's brush (default) | 0 the brush of strokes saved before it (size = the whole dab, feather inward), autoMask?: bool, autoRange?: -100 (tight) … 0 (default, Lightroom's limits) … 100 (wide), how different a pixel may be from the one under the dab and still take Auto Mask's paint (the limits scale by ½ … 2), kept with the stroke, pressure?: [p,…] pen pressure 0..1 per point (scales each dab's size and flow; omitted: full), runs?: [{points, erase?, size?, feather?, flow?, density?, pressure?}, …] (one gesture in parts painted with different modes, e.g. ⌥ / Alt pressed and let go during it: each run is a stroke, the values left out taken from the top level; instead of `points`), continue?: bool (the stroke being painted, sent again as it grows: it replaces the component's last `replace` strokes, so it shows live), replace?: n (with `continue`: how many strokes the gesture sent last time, 1), connect?: bool (start from the last point of the mask's last stroke (⇧-click), size / feather / flow tapering from that stroke's to these)} → {activeMask, connected}",
            has_active,
            |s, p| {
                // one stroke, or one per run of a gesture painted in parts (⌥ / Alt pressed and let go during it)
                let mut new: Vec<BrushStroke> = match p.get("runs") {
                    Some(Value::Array(runs)) if !runs.is_empty() => runs.iter().map(|r| brush_stroke_of(r, p)).collect::<Result<_>>()?,
                    Some(Value::Array(_)) => return Err(bad("mask.brushStroke", "`runs` is empty")),
                    Some(_) => return Err(bad("mask.brushStroke", "`runs` must be a list of strokes")),
                    None => vec![brush_stroke_of(p, p)?],
                };
                // the stroke being painted, sent again as it grows: it replaces what it sent last time
                let again = bool_or(p, "continue", false);
                let replace = p.get("replace").and_then(Value::as_u64).map_or(1, |n| usize::try_from(n).unwrap_or(usize::MAX)).max(1);
                // a ⇧-click line goes on from one click, not a gesture in parts or a live update
                let connect = bool_or(p, "connect", false) && new.len() == 1 && !again;
                let mut connected = false;
                let next = s.active().and_then(|id| s.develop_of(id)).map(|d| d.next_mask_id()).unwrap_or(1);
                let want = mask_id_param(p, "mask.brushStroke")?.or(s.active_mask);
                // (the next-mask settings are used up only when a new mask is made)
                let exists = want.is_some_and(|m| s.active().and_then(|id| s.develop_of(id)).is_some_and(|d| d.masks.iter().any(|x| x.id == m)));
                let adjust = if exists { LocalAdjustments::default() } else { super::mask_effects::new_mask_adjust(s, "brush") };
                // the Brush component armed on the photo: strokes and erasing go into THAT one (its
                // own Add or Subtract); a component that isn't a Brush, or isn't there, is refused
                let target_comp = match p.get("component").filter(|v| !v.is_null()) {
                    None => None,
                    Some(v) => Some(
                        v.as_u64()
                            .and_then(|k| usize::try_from(k).ok())
                            .ok_or_else(|| bad("mask.brushStroke", "`component` must be a component index"))?,
                    ),
                };
                let exists = want.is_some_and(|m| s.active().and_then(|id| s.develop_of(id)).is_some_and(|d| d.masks.iter().any(|x| x.id == m)));
                if target_comp.is_some() && !exists {
                    return Err(bad("mask.brushStroke", "`component` needs an existing mask"));
                }
                let mut out = masks_edit(s, "mask.brushStroke", "Brush", |masks, active| {
                    let i = match want.and_then(|m| masks.iter().position(|x| x.id == m)) {
                        Some(i) => i,
                        None => {
                            masks.push(Mask { id: next, name: format!("Mask {next}"), adjust, ..Default::default() });
                            *active = Some(next);
                            masks.len() - 1
                        }
                    };
                    let m = &mut masks[i];
                    let brush_idx = match target_comp {
                        Some(k) => match m.components.get(k) {
                            Some(c) if matches!(c.shape, MaskShape::Brush { .. }) => Some(k),
                            Some(_) => return Err(bad("mask.brushStroke", format!("component {k} is not a Brush"))),
                            None => return Err(bad("mask.brushStroke", format!("no component {k}"))),
                        },
                        None => m.components.iter().rposition(|c| matches!(c.shape, MaskShape::Brush { .. })),
                    };
                    let brush_comp = brush_idx.and_then(|k| m.components.get_mut(k));
                    if let Some(MaskComponent { shape: MaskShape::Brush { strokes }, .. }) = brush_comp {
                        // the previous stroke's end and its brush values (not its points)
                        let anchor = strokes.last().filter(|_| connect).and_then(|l| {
                            Some((
                                *l.points.last()?,
                                BrushStroke {
                                    points: Vec::new(),
                                    size: l.size,
                                    feather: l.feather,
                                    flow: l.flow,
                                    // the pressure the last dab was painted with, when the stroke recorded any
                                    pressure: if l.pressure.is_empty() { Vec::new() } else { vec![l.pressure_at(l.points.len().saturating_sub(1))] },
                                    ..BrushStroke::default()
                                },
                            ))
                        });
                        match anchor {
                            _ if again && !strokes.is_empty() => {
                                strokes.truncate(strokes.len().saturating_sub(replace));
                                strokes.append(&mut new);
                            }
                            Some((from, last)) if new.len() == 1 => {
                                let stroke = new.remove(0);
                                strokes.extend(connected_strokes(from, &last, stroke));
                                connected = true;
                            }
                            _ => strokes.append(&mut new),
                        }
                    } else {
                        m.components.push(MaskComponent {
                            anchor: None,
                            name: None,
                            op: MaskOp::Add,
                            invert: false,
                            shape: MaskShape::Brush { strokes: std::mem::take(&mut new) },
                        });
                    }
                    Ok(())
                })?;
                if let Some(o) = out.as_object_mut() {
                    o.insert("connected".into(), json!(connected));
                }
                Ok(out)
            }
        ),
        cmd!("mask.update", "Update Mask Shape", [], None, "{id?, component?: index (0), shape: MaskShape JSON}", has_active, |s, p| {
            let mut shape: MaskShape =
                serde_json::from_value(p.get("shape").cloned().unwrap_or_default()).map_err(|e| bad("mask.update", e.to_string()))?;
            cap_color_range_samples(&mut shape);
            let comp = super::index_opt(p, "component", "mask.update")?.unwrap_or(0);
            let mid = mask_id(p, s.active_mask, "mask.update")?;
            masks_edit(s, "mask.update", "Edit Mask", |masks, _| {
                let i = find(masks, mid, "mask.update")?;
                let c = masks[i].components.get_mut(comp).ok_or_else(|| bad("mask.update", "no such component"))?;
                // a gradient placed by hand stays where it was put: no longer placed through its
                // anchor's box (recomputing would move it), and its linked copies get exactly this place
                if c.shape != shape {
                    c.anchor = None;
                }
                c.shape = shape;
                Ok(())
            })
        }),
        cmd!(
            "mask.sampleColor",
            "Sample Mask Color",
            [],
            None,
            "{x, y: normalized image point | rect: [x0, y0, x1, y1] (an area dragged: up to 3 of its colours), add?: bool (⇧: add to the samples, max 5), id?, component?}: the colour range of the selected mask (its first colour-range component) samples the colour there; each picked place gets a pin (mask.colorPin) → {samples, pins}",
            has_active,
            |s, p| {
                let c = "mask.sampleColor";
                let area = area_param(p, c)?;
                let add = p.get("add").and_then(Value::as_bool).unwrap_or(false);
                let mid = mask_id(p, s.active_mask, c)?;
                let comp = super::index_opt(p, "component", c)?;
                let (k, current) = color_range_of(s, mid, comp, c)?;
                let room = if add { MAX_COLOR_SAMPLES.saturating_sub(current.len()) } else { MAX_COLOR_SAMPLES };
                if room == 0 {
                    // full (five colours): a further ⇧-click changes nothing, as before
                    let id = s.active().ok_or_else(|| bad(c, "no active photo"))?;
                    let d = s.develop_of(id).unwrap_or_default();
                    let samples = match d.masks.iter().find(|m| m.id == mid).and_then(|m| m.components.get(k)).map(|x| &x.shape) {
                        Some(MaskShape::ColorRange { samples, .. }) => samples.clone(),
                        _ => Vec::new(),
                    };
                    return Ok(json!({"samples": samples, "pins": lcc_develop::color_range_pins(&current).len()}));
                }
                let colours = sample_area_colours(s, area, room, c)?;
                let mut out = json!({});
                masks_edit(s, c, "Color Range", |masks, _| {
                    let i = find(masks, mid, c)?;
                    let Some(MaskShape::ColorRange { samples, areas, .. }) = masks[i].components.get_mut(k).map(|x| &mut x.shape) else {
                        return Err(bad(c, "not a colour range component"));
                    };
                    if !add {
                        samples.clear();
                        areas.clear();
                    }
                    // samples from before the pins have no place: an unknown one keeps them in step
                    areas.resize(samples.len(), UNKNOWN_AREA);
                    for lab in colours.iter().take(room) {
                        samples.push(*lab);
                        areas.push(area);
                    }
                    out = json!({"samples": samples.clone(), "pins": lcc_develop::color_range_pins(areas).len()});
                    Ok(())
                })?;
                Ok(out)
            }
        ),
        cmd!(
            "mask.colorPin",
            "Edit Color Range Sample",
            [],
            None,
            "{index: pin (0-based, in the order picked), remove?: bool (⌥-click), x?, y?: normalized point to move it to (the colours there are sampled again), id?, component?}: one picked place of the selected mask's colour range → {samples, pins}",
            has_active,
            |s, p| {
                let c = "mask.colorPin";
                let index = super::index_opt(p, "index", c)?.ok_or_else(|| bad(c, "index is required"))?;
                let remove = p.get("remove").and_then(Value::as_bool).unwrap_or(false);
                let mid = mask_id(p, s.active_mask, c)?;
                let comp = super::index_opt(p, "component", c)?;
                let (k, current) = color_range_of(s, mid, comp, c)?;
                let pins = lcc_develop::color_range_pins(&current);
                let (area, range) =
                    pins.get(index).cloned().ok_or_else(|| bad(c, format!("no pin {index} (the colour range has {})", pins.len())))?;
                let moved = if remove {
                    None
                } else {
                    let (x, y) = (super::f64_req(p, "x", c)?, super::f64_req(p, "y", c)?);
                    // the same area, its centre at (x, y)
                    let (cx, cy) = ((area[0] + area[2]) / 2.0, (area[1] + area[3]) / 2.0);
                    let to = [area[0] + x - cx, area[1] + y - cy, area[2] + x - cx, area[3] + y - cy];
                    let colours = sample_area_colours(s, to, range.len().max(1), c)?;
                    Some((to, colours))
                };
                let mut out = json!({});
                masks_edit(s, c, "Color Range", |masks, _| {
                    let i = find(masks, mid, c)?;
                    let Some(MaskShape::ColorRange { samples, areas, .. }) = masks[i].components.get_mut(k).map(|x| &mut x.shape) else {
                        return Err(bad(c, "not a colour range component"));
                    };
                    areas.resize(samples.len(), UNKNOWN_AREA);
                    let (start, end) = (range.start.min(samples.len()), range.end.min(samples.len()));
                    let (new_samples, new_areas): (Vec<[f64; 3]>, Vec<[f64; 4]>) = match &moved {
                        Some((to, colours)) => (colours.clone(), vec![*to; colours.len()]),
                        None => (Vec::new(), Vec::new()),
                    };
                    samples.splice(start..end, new_samples);
                    areas.splice(start..end, new_areas);
                    out = json!({"samples": samples.clone(), "pins": lcc_develop::color_range_pins(areas).len()});
                    Ok(())
                })?;
                Ok(out)
            }
        ),
        cmd!(
            "mask.sampleLuminance",
            "Sample Mask Luminance",
            [],
            None,
            "{x, y: normalized image point | rect: [x0, y0, x1, y1] (an area dragged), add?: bool (⇧: widen the current range to take it in), id?, component?}: the luminance range of the selected mask (its first luminance-range component) selects the tones there → {lo, hi}",
            has_active,
            |s, p| {
                let c = "mask.sampleLuminance";
                let area = area_param(p, c)?;
                let add = p.get("add").and_then(Value::as_bool).unwrap_or(false);
                let mid = mask_id(p, s.active_mask, c)?;
                let comp = super::index_opt(p, "component", c)?;
                let px = area_pixels(s, area, c)?;
                let mut lum: Vec<f64> = px.iter().map(|(_, l)| *l).collect();
                lum.sort_by(f64::total_cmp);
                let at = |q: f64| lum.get((lum.len().saturating_sub(1) as f64 * q).round() as usize).copied().unwrap_or(0.5);
                // a click: a band round its tone; an area: the tones in it (5th to 95th percentile)
                let (mut lo, mut hi) = if area[0] == area[2] && area[1] == area[3] {
                    let m = at(0.5);
                    (m - 0.08, m + 0.08)
                } else {
                    (at(0.05), at(0.95))
                };
                let mut out = json!({});
                masks_edit(s, c, "Luminance Range", |masks, _| {
                    let i = find(masks, mid, c)?;
                    let k = comp
                        .or_else(|| masks[i].components.iter().position(|x| matches!(x.shape, MaskShape::LuminanceRange { .. })))
                        .ok_or_else(|| bad(c, "the mask has no luminance range"))?;
                    let Some(MaskShape::LuminanceRange { lo: l0, hi: h0, .. }) = masks[i].components.get_mut(k).map(|x| &mut x.shape) else {
                        return Err(bad(c, "not a luminance range component"));
                    };
                    if add {
                        lo = lo.min(*l0);
                        hi = hi.max(*h0);
                    }
                    *l0 = lo.clamp(0.0, 0.99);
                    *h0 = hi.clamp(*l0 + 0.01, 1.0);
                    out = json!({"lo": *l0, "hi": *h0});
                    Ok(())
                })?;
                Ok(out)
            }
        ),
        cmd!(
            "mask.objectPoint",
            "Add Object Click",
            [],
            None,
            "{x, y: normalized image point, component?: index, exclude?: bool (⌥-click: leave this part out), id?: maskId}: a click on the selected mask's Object selection (its last Object component); SAM 3 re-segments the object → {include, exclude}",
            has_active,
            |s, p| {
                let c = "mask.objectPoint";
                let at = Point::new(super::f64_req(p, "x", c)?, super::f64_req(p, "y", c)?);
                if !(0.0..=1.0).contains(&at.x) || !(0.0..=1.0).contains(&at.y) {
                    return Err(bad(c, "the point is outside the photo"));
                }
                let exclude = bool_or(p, "exclude", false);
                let id = s.active().ok_or_else(|| bad(c, "no active photo"))?;
                let mid = mask_id(p, s.active_mask, c)?;
                // an old mask (on the corrected frame, as the click then is) goes onto the photo
                // first, and the click with it: the model works on the photo before lens
                // corrections, Upright and Transform, and reads every click there
                let mut at = at;
                if let Some(cur) = s.develop_of(id).filter(|d| d.masks.iter().any(|m| m.id == mid && !m.on_photo)) {
                    if let Some(f) = s.mask_frame(id, &cur) {
                        // a click on the corrected frame's blank space is off the photo, not on
                        // its edge (a hair past it, from rounding, is on the edge)
                        const EDGE: f64 = 1e-6;
                        let n = f.for_mask(true).norm_to_mask(at);
                        let on = |v: f64| v.is_finite() && (-EDGE..=1.0 + EDGE).contains(&v);
                        if !(on(n.x) && on(n.y)) {
                            return Err(bad(c, "the point is outside the photo"));
                        }
                        at = Point::new(n.x.clamp(0.0, 1.0), n.y.clamp(0.0, 1.0));
                    }
                    let mut on = (*cur).clone();
                    s.attach_masks(id, &cur, &mut on);
                    s.apply_silent(lcc_catalog::Op::SetDevelop { id, settings: std::sync::Arc::new(on), label: "Masks".into(), edited: None })?;
                }
                let comp = super::index_opt(p, "component", c)?;
                let d = s.develop_of(id).unwrap_or_default();
                let m = d.masks.iter().find(|m| m.id == mid).ok_or_else(|| bad(c, format!("no mask {mid}")))?;
                let k = match comp {
                    Some(k) if m.components.get(k).is_some_and(|c| matches!(c.shape, MaskShape::Object { .. })) => k,
                    _ => m
                        .components
                        .iter()
                        .rposition(|x| matches!(x.shape, MaskShape::Object { .. }))
                        .ok_or_else(|| bad(c, "the mask has no Object selection (add one first)"))?,
                };
                let Some(MaskShape::Object { hint, exclude: ex, edge, .. }) = m.components.get(k).map(|x| &x.shape) else {
                    return Err(bad(c, "not an Object component"));
                };
                let (mut hint, mut ex, edge) = (hint.clone(), ex.clone(), *edge);
                // clicks still on their way to the model count too
                if let Some(pending) = s.segmenter.pending_clicks().filter(|q| (q.photo, q.mask, q.comp) == (id, mid, k)) {
                    (hint, ex) = (pending.hint.clone(), pending.exclude.clone());
                }
                if hint.len() + ex.len() >= MAX_CLICKS {
                    return Err(ai_err(format!("an Object selection takes at most {MAX_CLICKS} clicks; start a new one")));
                }
                if exclude {
                    ex.push(at)
                } else {
                    hint.push(at)
                }
                if s.segmenter.background && !hint.is_empty() {
                    let out = json!({"include": hint.len(), "exclude": ex.len(), "pending": true});
                    segment_object(s, |s| s.segment_clicks_later(id, mid, k, hint, ex))?;
                    return Ok(out);
                }
                let seg = if hint.is_empty() { None } else { Some(segment_object(s, |s| s.segment_clicks(id, &clicks_of(&hint, &ex)))?) };
                let out = json!({"include": hint.len(), "exclude": ex.len()});
                masks_edit(s, c, "Object Mask", |masks, _| {
                    let i = find(masks, mid, c)?;
                    if let Some(comp) = masks[i].components.get_mut(k) {
                        comp.shape = MaskShape::Object { hint, exclude: ex, seg, detail: vec![], edge };
                    }
                    Ok(())
                })?;
                Ok(out)
            }
        ),
        cmd!(
            "mask.refineDetail",
            "Refine AI Mask Detail",
            [],
            None,
            "{id?: maskId, component?}: a zoomed-in AI pass over an Object/Describe selection (default: the mask's last one): the photo around it is analyzed again at a higher resolution, in the background; the mask updates when it's done → {started}",
            has_active,
            |s, p| {
                let c = "mask.refineDetail";
                let id = s.active().ok_or_else(|| bad(c, "no active photo"))?;
                let mid = mask_id(p, s.active_mask, c)?;
                let d = s.develop_of(id).unwrap_or_default();
                let m = d.masks.iter().find(|m| m.id == mid).ok_or_else(|| bad(c, format!("no mask {mid}")))?;
                let k = match super::index_opt(p, "component", c)? {
                    Some(k) => k,
                    None => m
                        .components
                        .iter()
                        .rposition(|x| matches!(x.shape, MaskShape::Object { .. } | MaskShape::Prompt { .. }))
                        .ok_or_else(|| bad(c, "the mask has no Object or Describe selection"))?,
                };
                if s.segmenter.require_object_model().is_err() {
                    s.segmenter.describe_model()?;
                }
                let started = s.segment_detail(id, mid, k).map_err(ai_err)?;
                Ok(json!({"started": started}))
            }
        ),
        cmd!(
            query "segment.prepare",
            "Prepare AI Masks",
            [],
            None,
            "{}: load SAM 3 and analyze the active photo in the background, so Object clicks are instant → {busy}",
            has_active,
            |s, _| {
                let c = "segment.prepare";
                let id = s.active().ok_or_else(|| bad(c, "no active photo"))?;
                segment_object(s, |s| s.segment_prepare(id))?;
                Ok(json!({"busy": s.segmenter.busy()}))
            }
        ),
        cmd!(
            query "segment.model.status",
            "AI Mask Model Status",
            [],
            None,
            "{}: whether this build has AI masks, whether the SAM 3 model is installed (and where), loaded, busy, and the download's progress → {available, installed, dir, loaded, busy, analyzing, sizeBytes, license, licenseUrl, mirrors, download: {running, done, total, file, error, finished}}",
            super::always,
            |s, _| {
                let g = &s.segmenter;
                Ok(json!({
                    "available": crate::segment::Segmenter::AVAILABLE,
                    "installed": g.installed(),
                    "dir": g.dir.as_ref().map(|d| d.display().to_string()),
                    "remote": g.remote_endpoint(),
                    "loaded": g.loaded(),
                    "busy": g.busy(),
                    "analyzing": g.analyzing(),
                    "sizeBytes": crate::segment::MODEL_BYTES,
                    "license": crate::segment::LICENSE_NAME,
                    "licenseUrl": crate::segment::LICENSE_URL,
                    "mirrors": g.mirrors().len(),
                    "download": g.download_status(),
                }))
            }
        ),
        cmd!(
            query "segment.model.download",
            "Download AI Mask Model",
            [],
            None,
            "{acknowledged: true}: download the SAM 3 model (about 3.4 GB, Meta's SAM License, not the app's) in the background, from the configured mirrors; only after the user agreed to it. Watch segment.model.status; segment.model.cancel stops it (it resumes later) → {started, installed, downloading}",
            super::always,
            |s, p| {
                let c = "segment.model.download";
                if p.get("acknowledged").and_then(Value::as_bool) != Some(true) {
                    return Err(bad(
                        c,
                        format!(
                            "the SAM 3 model is a {:.1} GB download under Meta's {} ({}): pass `acknowledged: true` once the user has agreed to download it",
                            crate::segment::MODEL_BYTES as f64 / 1e9,
                            crate::segment::LICENSE_NAME,
                            crate::segment::LICENSE_URL
                        ),
                    ));
                }
                let started = s.segmenter.start_download(&s.activity).map_err(ai_err)?;
                Ok(json!({"started": started, "installed": s.segmenter.installed(), "downloading": s.segmenter.download_status().running}))
            }
        ),
        cmd!(
            query "segment.model.cancel",
            "Cancel AI Mask Model Download",
            [],
            None,
            "{}: stop the SAM 3 download (what has arrived is kept, and a new download resumes from it) → {cancelled}",
            super::always,
            |s, _| Ok(json!({"cancelled": s.segmenter.cancel_download()}))
        ),
        cmd!(
            query "segment.subject.status",
            "Subject Mask Model Status",
            [],
            None,
            "{}: the Subject / Background model (BiRefNet_lite, MIT): whether it is installed (and where), in use by masks, and its download's progress; a finished download is put to use here → {available, installed, active, dir, sizeBytes, license, licenseUrl, download: {running, done, total, file, error, finished}}",
            super::always,
            |s, _| {
                s.segmenter.subject.sync(&s.activity);
                let g = &s.segmenter.subject;
                Ok(json!({
                    "available": crate::segment::Segmenter::AVAILABLE,
                    "installed": g.installed(),
                    "active": g.active(),
                    "dir": g.dir.as_ref().map(|d| d.display().to_string()),
                    "sizeBytes": crate::segment::subject::MODEL_BYTES,
                    "license": crate::segment::subject::LICENSE_NAME,
                    "licenseUrl": crate::segment::subject::LICENSE_URL,
                    "download": g.download_status(),
                }))
            }
        ),
        cmd!(
            query "segment.subject.download",
            "Download Subject Mask Model",
            [],
            None,
            "{acknowledged: true}: download the Subject / Background model (BiRefNet_lite, about 178 MB, MIT) in the background from its author's Hugging Face repository (pinned revision, size and SHA-256); only after the user asked for it. Without it, Subject and Background use the photo analysis. Watch segment.subject.status → {started, installed, downloading}",
            super::always,
            |s, p| {
                let c = "segment.subject.download";
                if p.get("acknowledged").and_then(Value::as_bool) != Some(true) {
                    return Err(bad(c, "the subject model is a 178 MB download: pass `acknowledged: true` once the user has asked for it"));
                }
                let started = s.segmenter.subject.start_download(&s.activity).map_err(ai_err)?;
                Ok(json!({"started": started, "installed": s.segmenter.subject.installed(), "downloading": s.segmenter.subject.download_status().running}))
            }
        ),
        cmd!(
            query "segment.subject.cancel",
            "Cancel Subject Mask Model Download",
            [],
            None,
            "{}: stop the subject model download (it resumes later) → {cancelled}",
            super::always,
            |s, _| Ok(json!({"cancelled": s.segmenter.subject.cancel_download()}))
        ),
        cmd!(
            query "ai.models.status",
            "AI Models",
            [],
            None,
            "{}: every AI mask model (SAM 3, the Subject model, SAM 2.1, the EoMT scene model, Depth Anything V2 Small): installed, folder, size, licence (and training-data credit), offered in the model dialog, download progress; which model computes Object clicks, and which computes Sky, Landscape and People → {available, objectModel: auto|sam3|sam2, objectModelInUse: sam3|sam2, sceneModel: analysis|eomt, sceneModelInUse: analysis|eomt, models: [{id, title, purpose, installed, dir, sizeBytes, license, licenseUrl, credit?, offered, download}]}",
            super::always,
            |s, _| {
                s.segmenter.subject.sync(&s.activity);
                let activity = s.activity.clone();
                s.segmenter.scene_sync(&activity);
                let g = &s.segmenter;
                Ok(json!({
                    "available": crate::segment::Segmenter::AVAILABLE,
                    "objectModel": g.object_model().key(),
                    "objectModelInUse": g.object_model_in_use(),
                    "sceneModel": g.scene_model().key(),
                    "sceneModelInUse": g.scene_model_in_use(),
                    "models": g.model_status(),
                }))
            }
        ),
        cmd!(
            query "ai.models.download",
            "Download AI Model",
            [],
            None,
            "{id: sam3|subject|sam2|scene|depth, acknowledged: true}: download that model in the background (pinned size and SHA-256), only after the user agreed to its size and licence (ai.models.status lists both). Watch ai.models.status; ai.models.cancel stops it (it resumes later) → {started, installed, downloading}",
            super::always,
            |s, p| {
                let c = "ai.models.download";
                let id = p.get("id").and_then(Value::as_str).ok_or_else(|| bad(c, "which model? `id`"))?.to_string();
                let st = s.segmenter.model_status().into_iter().find(|m| m.id == id).ok_or_else(|| bad(c, format!("unknown AI model `{id}`")))?;
                if p.get("acknowledged").and_then(Value::as_bool) != Some(true) {
                    return Err(bad(
                        c,
                        format!(
                            "{} is a {:.0} MB download under {} ({}): pass `acknowledged: true` once the user has agreed to download it",
                            st.title,
                            st.size_bytes as f64 / 1e6,
                            st.license,
                            st.license_url
                        ),
                    ));
                }
                let activity = s.activity.clone();
                let started = s.segmenter.start_model_download(&id, &activity).map_err(ai_err)?;
                let now = s.segmenter.model_status().into_iter().find(|m| m.id == id);
                Ok(json!({"started": started, "installed": now.as_ref().is_some_and(|m| m.installed), "downloading": now.is_some_and(|m| m.download.running)}))
            }
        ),
        cmd!(
            query "ai.models.cancel",
            "Cancel AI Model Download",
            [],
            None,
            "{id}: stop that model's download (what has arrived is kept, and a new download resumes from it) → {cancelled}",
            super::always,
            |s, p| {
                let id = p.get("id").and_then(Value::as_str).ok_or_else(|| bad("ai.models.cancel", "which model? `id`"))?;
                Ok(json!({"cancelled": s.segmenter.cancel_model_download(id)}))
            }
        ),
        cmd!(
            query "ai.models.setObjectModel",
            "Set Object Mask Model",
            [],
            None,
            "{model: auto|sam3|sam2}: which model computes Object (click) masks: auto (SAM 3 when installed, else SAM 2.1), SAM 3, or SAM 2.1 (Apache-2.0). Describe uses SAM 3 when installed, else the OWLv2 object finder with SAM 2.1. Saved for next time → {objectModel, objectModelInUse}",
            super::always,
            |s, p| {
                let c = "ai.models.setObjectModel";
                let v = p.get("model").and_then(Value::as_str).ok_or_else(|| bad(c, "missing `model` (auto, sam3 or sam2)"))?;
                let m = crate::segment::models::ObjectModel::parse(v).ok_or_else(|| bad(c, format!("unknown model `{v}` (auto, sam3 or sam2)")))?;
                s.segmenter.set_object_model(m).map_err(ai_err)?;
                Ok(json!({"objectModel": m.key(), "objectModelInUse": s.segmenter.object_model_in_use()}))
            }
        ),
        cmd!(
            query "ai.models.setSceneModel",
            "Set Scene Mask Model",
            [],
            None,
            "{model: analysis|eomt}: which model computes Sky, Landscape (water, vegetation) and People masks: the photo analysis (the default, no download) or the EoMT scene model (MIT, trained on COCO; ai.models.download {\"id\": \"scene\"}). EoMT chosen but not installed keeps the analysis. Masks re-render with it; saved for next time → {sceneModel, sceneModelInUse}",
            super::always,
            |s, p| {
                let c = "ai.models.setSceneModel";
                let v = p.get("model").and_then(Value::as_str).ok_or_else(|| bad(c, "missing `model` (analysis or eomt)"))?;
                let m = crate::segment::models::SceneModel::parse(v).ok_or_else(|| bad(c, format!("unknown model `{v}` (analysis or eomt)")))?;
                s.segmenter.set_scene_model(m).map_err(ai_err)?;
                let activity = s.activity.clone();
                s.segmenter.scene_sync(&activity);
                Ok(json!({"sceneModel": m.key(), "sceneModelInUse": s.segmenter.scene_model_in_use()}))
            }
        ),
        cmd!(
            "mask.refine",
            "Refine Mask Edges",
            [],
            None,
            "{id?, value: 0..100}: the mask's edges snap to the photo's (guided filter; rendered on the CPU)",
            has_active,
            |s, p| {
                let v = f64_or(p, "value", 0.0).clamp(0.0, 100.0);
                let mid = mask_id(p, s.active_mask, "mask.refine")?;
                masks_edit(s, "mask.refine", "Refine Edges", |masks, _| {
                    let i = find(masks, mid, "mask.refine")?;
                    masks[i].refine = v;
                    Ok(())
                })
            }
        ),
        cmd!(
            "mask.feather",
            "Feather Mask",
            [],
            None,
            "{id?, value: 0..100}: soften the whole mask's edges (blur up to 2 % of the long edge; rendered on the CPU)",
            has_active,
            |s, p| {
                let v = f64_or(p, "value", 0.0).clamp(0.0, 100.0);
                let mid = mask_id(p, s.active_mask, "mask.feather")?;
                masks_edit(s, "mask.feather", "Feather", |masks, _| {
                    let i = find(masks, mid, "mask.feather")?;
                    masks[i].feather = v;
                    Ok(())
                })
            }
        ),
        cmd!(
            "mask.adjust",
            "Set Mask Adjustments",
            [],
            None,
            "{id?, values: {exposure, contrast, highlights, shadows, whites, blacks, temp, tint, texture, clarity, dehaze, hue (degrees −180..180), saturation, sharpness, noise, moire, defringe, color_hue, color_sat, grade_{shadows,midtones,highlights,global}_{hue,sat,lum}, amount}}",
            has_active,
            |s, p| {
                let vals = p.get("values").cloned().ok_or_else(|| bad("mask.adjust", "missing values"))?;
                let merged = |a: LocalAdjustments| -> Result<LocalAdjustments> {
                    let mut v = serde_json::to_value(a).unwrap_or_default();
                    lcc_develop::presets::deep_merge(&mut v, &vals);
                    let a: LocalAdjustments = serde_json::from_value(v).map_err(|e| bad("mask.adjust", e.to_string()))?;
                    Ok(clamp_local(a))
                };
                // no mask selected (nor named), a tool armed or not: the settings for the next new mask
                if s.active_mask.is_none() && p.get("id").and_then(Value::as_u64).is_none() {
                    s.mask_next = merged(s.mask_next)?;
                    s.mask_next_set = true;
                    return Ok(json!({"next": s.mask_next}));
                }
                // an existing mask: only it changes, never the next mask's settings
                let mid = mask_id(p, s.active_mask, "mask.adjust")?;
                masks_edit(s, "mask.adjust", "Mask Adjustment", |masks, _| {
                    let i = find(masks, mid, "mask.adjust")?;
                    masks[i].adjust = merged(masks[i].adjust)?;
                    Ok(())
                })
            }
        ),
        cmd!(
            "mask.next",
            "New Mask Settings",
            [],
            None,
            "{resetNew?: bool (Reset Settings on New Mask: once a new mask took the next-mask settings they go back to zero), clear?: bool (zero the settings set for the next mask)}: the settings the next new mask starts with (what was dialed in while no mask was selected: mask.adjust / mask.applyEffect with no mask selected) → {next, set, resetNew}",
            super::always,
            |s, p| {
                if let Some(on) = p.get("resetNew").and_then(Value::as_bool) {
                    s.mask_reset_new = on;
                }
                if p.get("clear").and_then(Value::as_bool) == Some(true) {
                    s.mask_next = LocalAdjustments::default();
                    s.mask_next_set = false;
                }
                Ok(json!({"next": s.mask_next, "set": s.mask_next_set, "resetNew": s.mask_reset_new}))
            }
        ),
        cmd!(
            "mask.pointColorPick",
            "Add Mask Point Color Sample",
            [],
            None,
            "{id?, x, y} normalized image coords: samples the colour there into the mask's local Point Color (max 8); returns {index, lum, chroma, hue}",
            has_active,
            |s, p| {
                let c = "mask.pointColorPick";
                let (x, y) = (super::f64_req(p, "x", c)?, super::f64_req(p, "y", c)?);
                let mid = mask_id(p, s.active_mask, c)?;
                let [l, ch, h] = super::color::encoded_to_oklch(super::color::probe(s, c, x, y, super::color::after_mixer_neutral)?);
                let sample = PointColor { lum: l as f64, chroma: ch as f64, hue: (h as f64).to_degrees().rem_euclid(360.0), ..Default::default() };
                let mut index = 0;
                masks_edit(s, c, "Mask Point Color", |masks, _| {
                    let i = find(masks, mid, c)?;
                    let m = masks.get_mut(i).ok_or_else(|| bad(c, format!("no mask {mid}")))?;
                    if m.point_colors.len() >= MAX_POINT_COLORS {
                        return Err(bad(c, format!("at most {MAX_POINT_COLORS} point colours per mask")));
                    }
                    m.point_colors.push(sample);
                    index = m.point_colors.len() - 1;
                    Ok(())
                })?;
                Ok(json!({"index": index, "lum": sample.lum, "chroma": sample.chroma, "hue": sample.hue}))
            }
        ),
        cmd!(
            "mask.pointColorSet",
            "Set Mask Point Color",
            [],
            None,
            "{id?, index, values: {hue_shift, sat_shift, lum_shift, variance, range, hue_range, sat_range, lum_range}}: changes one of the mask's Point Color samples (shifts and variance −100..100, ranges 0..100)",
            has_active,
            |s, p| {
                let c = "mask.pointColorSet";
                let k = super::index_req(p, "index", c)?;
                let vals = p.get("values").cloned().ok_or_else(|| bad(c, "missing values"))?;
                let mid = mask_id(p, s.active_mask, c)?;
                masks_edit(s, c, "Mask Point Color", |masks, _| {
                    let i = find(masks, mid, c)?;
                    let m = masks.get_mut(i).ok_or_else(|| bad(c, format!("no mask {mid}")))?;
                    let pc = m.point_colors.get_mut(k).ok_or_else(|| bad(c, "no such sample"))?;
                    let mut v = serde_json::to_value(*pc).unwrap_or_default();
                    lcc_develop::presets::deep_merge(&mut v, &vals);
                    let mut n: PointColor = serde_json::from_value(v).map_err(|e| bad(c, e.to_string()))?;
                    // the sampled colour stays; the adjustments keep to their ranges (NaN → 0)
                    let clamp = |v: f64, lo: f64, hi: f64| if v.is_finite() { v.clamp(lo, hi) } else { 0.0 };
                    (n.lum, n.chroma, n.hue) = (pc.lum, pc.chroma, pc.hue);
                    for f in [&mut n.hue_shift, &mut n.sat_shift, &mut n.lum_shift, &mut n.variance] {
                        *f = clamp(*f, -100.0, 100.0);
                    }
                    for f in [&mut n.range, &mut n.hue_range, &mut n.sat_range, &mut n.lum_range] {
                        *f = clamp(*f, 0.0, 100.0);
                    }
                    *pc = n;
                    Ok(())
                })
            }
        ),
        cmd!(
            "mask.mixer",
            "Set Mask Color Mixer",
            [],
            None,
            "{id?, values?: {\"<band>.<hue|sat|lum>\": −100..100, …} (band: red, orange, yellow, green, aqua, blue, purple, magenta), reset?: bool}: the mask's local Color Mixer (applied within the mask after the photo's colour tools, before its Point Color, with the global mixer's maths); `reset` first sets every band back to 0 → {mixer}",
            has_active,
            |s, p| {
                let c = "mask.mixer";
                let vals = mixer_values(p, c)?;
                let reset = bool_or(p, "reset", false);
                if vals.is_empty() && !reset {
                    return Err(bad(c, "give `values` or `reset`"));
                }
                let mid = mask_id(p, s.active_mask, c)?;
                let mut out = Mixer::default();
                masks_edit(s, c, "Mask Color Mixer", |masks, _| {
                    let i = find(masks, mid, c)?;
                    let m = masks.get_mut(i).ok_or_else(|| bad(c, format!("no mask {mid}")))?;
                    if reset {
                        m.mixer = Mixer::default();
                    }
                    for (k, v) in &vals {
                        if let Some(f) = mixer_field(&mut m.mixer, k) {
                            *f = *v;
                        }
                    }
                    out = m.mixer;
                    Ok(())
                })?;
                Ok(json!({"mixer": mixer_json(&out)}))
            }
        ),
        cmd!(
            "mask.mixerTargeted",
            "Mask Color Mixer Targeted Adjustment",
            [],
            None,
            "{id?, target: hue|sat|lum, x, y (normalized image coords), delta (slider units)}: moves the mask's Color Mixer bands of the colour under the point, as `develop.targeted` does the photo's mixer → {changed: {\"<band>.<attr>\": value}}",
            has_active,
            |s, p| {
                let c = "mask.mixerTargeted";
                let (x, y, delta) = (super::f64_req(p, "x", c)?, super::f64_req(p, "y", c)?, super::f64_req(p, "delta", c)?);
                if !(x.is_finite() && y.is_finite() && delta.is_finite()) {
                    return Err(bad(c, "`x`, `y` and `delta` must be numbers"));
                }
                let target = str_param(p, "target").unwrap_or("hue").to_string();
                if !matches!(target.as_str(), "hue" | "sat" | "lum") {
                    return Err(bad(c, "target must be hue, sat or lum"));
                }
                let mid = mask_id(p, s.active_mask, c)?;
                if !s.active().and_then(|id| s.develop_of(id)).is_some_and(|d| d.masks.iter().any(|m| m.id == mid)) {
                    return Err(bad(c, format!("no mask {mid}")));
                }
                // the colour the masks' mixers see there: theirs and everything after them neutral
                let e = super::color::probe(s, c, x, y, |d| {
                    super::color::after_mixer_neutral(d);
                    for m in &mut d.masks {
                        m.mixer = Mixer::default();
                        m.point_colors.clear();
                    }
                })?;
                let [_, _, h] = super::color::encoded_to_oklch(e);
                let w = lcc_pipeline::colorops::band_weights(h);
                let top = w.iter().copied().fold(0.0f32, f32::max).max(1e-6);
                let delta = delta.clamp(-200.0, 200.0);
                let mut changed = serde_json::Map::new();
                masks_edit(s, c, "Mask Color Mixer", |masks, _| {
                    let i = find(masks, mid, c)?;
                    let m = masks.get_mut(i).ok_or_else(|| bad(c, format!("no mask {mid}")))?;
                    for (i, band) in MIXER_BANDS.iter().enumerate() {
                        let k = f64::from(w.get(i).copied().unwrap_or(0.0) / top);
                        if k < 0.05 {
                            continue;
                        }
                        let key = format!("{band}.{target}");
                        if let Some(f) = mixer_field(&mut m.mixer, &key) {
                            *f = (*f + delta * k).clamp(-100.0, 100.0);
                            changed.insert(key, json!(*f));
                        }
                    }
                    Ok(())
                })?;
                Ok(json!({"changed": changed}))
            }
        ),
        cmd!("mask.pointColorDelete", "Delete Mask Point Color Sample", [], None, "{id?, index}", has_active, |s, p| {
            let c = "mask.pointColorDelete";
            let k = super::index_req(p, "index", c)?;
            let mid = mask_id(p, s.active_mask, c)?;
            masks_edit(s, c, "Delete Mask Point Color", |masks, _| {
                let i = find(masks, mid, c)?;
                let m = masks.get_mut(i).ok_or_else(|| bad(c, format!("no mask {mid}")))?;
                if k >= m.point_colors.len() {
                    return Err(bad(c, "no such sample"));
                }
                m.point_colors.remove(k);
                Ok(())
            })
        }),
        cmd!("mask.select", "Select Mask", [], None, "{id|null}", has_active, |s, p| {
            s.active_mask = mask_id_param(p, "mask.select")?;
            ok()
        }),
        cmd!("mask.delete", "Delete Mask", [], None, "{id?}", has_active, |s, p| {
            let mid = mask_id(p, s.active_mask, "mask.delete")?;
            masks_edit(s, "mask.delete", "Delete Mask", |masks, active| {
                let i = find(masks, mid, "mask.delete")?;
                masks.remove(i);
                // deleting another mask (by id) keeps the selection
                if *active == Some(mid) {
                    *active = masks.last().map(|m| m.id);
                }
                Ok(())
            })
        }),
        cmd!("mask.deleteAll", "Delete All Masks", [], None, "{}", has_active, |s, _| {
            masks_edit(s, "mask.deleteAll", "Delete All Masks", |masks, active| {
                masks.clear();
                *active = None;
                Ok(())
            })
        }),
        cmd!("mask.deleteEmpty", "Delete All Empty Masks", [], None, "{}: drops the masks whose components select nothing", has_active, |s, _| {
            masks_edit(s, "mask.deleteEmpty", "Delete All Empty Masks", |masks, active| {
                masks.retain(|m| !mask_is_empty(m));
                if active.is_some_and(|a| !masks.iter().any(|m| m.id == a)) {
                    *active = masks.last().map(|m| m.id);
                }
                Ok(())
            })
        }),
        cmd!("mask.rename", "Rename Mask", [], None, "{id?, name}", has_active, |s, p| {
            let name = str_param(p, "name").ok_or_else(|| bad("mask.rename", "missing name"))?.to_string();
            let mid = mask_id(p, s.active_mask, "mask.rename")?;
            masks_edit(s, "mask.rename", "Rename Mask", |masks, _| {
                let i = find(masks, mid, "mask.rename")?;
                masks[i].name = name;
                Ok(())
            })
        }),
        cmd!("mask.invert", "Invert Mask", [], None, "{id?}", has_active, |s, p| {
            let mid = mask_id(p, s.active_mask, "mask.invert")?;
            masks_edit(s, "mask.invert", "Invert Mask", |masks, _| {
                let i = find(masks, mid, "mask.invert")?;
                masks[i].invert = !masks[i].invert;
                Ok(())
            })
        }),
        cmd!("mask.visible", "Show/Hide Mask", [], None, "{id?, visible?: bool}", has_active, |s, p| {
            let mid = mask_id(p, s.active_mask, "mask.visible")?;
            masks_edit(s, "mask.visible", "Toggle Mask", |masks, _| {
                let i = find(masks, mid, "mask.visible")?;
                masks[i].visible = bool_or(p, "visible", !masks[i].visible);
                Ok(())
            })
        }),
        cmd!(
            "mask.duplicate",
            "Duplicate Mask",
            [],
            None,
            "{id?, invert?: bool (Duplicate and Invert)}: the copy goes right after the original and is selected",
            has_active,
            |s, p| {
                let mid = mask_id(p, s.active_mask, "mask.duplicate")?;
                let invert = bool_or(p, "invert", false);
                let next = s.active().and_then(|id| s.develop_of(id)).map(|d| d.next_mask_id()).unwrap_or(1);
                let label = if invert { "Duplicate and Invert Mask" } else { "Duplicate Mask" };
                masks_edit(s, "mask.duplicate", label, |masks, active| {
                    let i = find(masks, mid, "mask.duplicate")?;
                    let mut m = masks[i].clone();
                    m.id = next;
                    m.name = format!("{} copy", m.name);
                    if invert {
                        m.invert = !m.invert;
                    }
                    masks.insert(i + 1, m);
                    *active = Some(next);
                    Ok(())
                })
            }
        ),
        cmd!(
            "mask.component",
            "Edit Mask Component",
            [],
            None,
            "{id?, component: index, action: invert|duplicate|delete|rename|op, name? (rename; empty clears), op?: add|subtract|intersect}: one component of a mask (deleting the last one deletes the mask)",
            has_active,
            |s, p| {
                let c = "mask.component";
                let mid = mask_id(p, s.active_mask, c)?;
                let k = super::index_req(p, "component", c)?;
                let action = str_param(p, "action").ok_or_else(|| bad(c, "missing action"))?.to_string();
                let op: Option<MaskOp> = match p.get("op") {
                    Some(v) => Some(serde_json::from_value(v.clone()).map_err(|e| bad(c, e.to_string()))?),
                    None => None,
                };
                let label = match action.as_str() {
                    "invert" => "Invert Component",
                    "duplicate" => "Duplicate Component",
                    "delete" => "Delete Component",
                    "rename" => "Rename Component",
                    "op" => "Change Component Mode",
                    a => return Err(bad(c, format!("unknown action {a:?} (invert|duplicate|delete|rename|op)"))),
                };
                masks_edit(s, c, label, |masks, active| {
                    let i = find(masks, mid, c)?;
                    let comps = &mut masks[i].components;
                    let comp = comps.get_mut(k).ok_or_else(|| bad(c, "no such component"))?;
                    match action.as_str() {
                        "invert" => comp.invert = !comp.invert,
                        "rename" => comp.name = str_param(p, "name").map(str::trim).filter(|n| !n.is_empty()).map(str::to_string),
                        "op" => comp.op = op.ok_or_else(|| bad(c, "missing op"))?,
                        "duplicate" => {
                            let copy = comp.clone();
                            comps.insert(k + 1, copy);
                        }
                        _ => {
                            comps.remove(k);
                            if comps.is_empty() {
                                masks.remove(i);
                                // emptying another mask (by id) keeps the selection
                                if *active == Some(mid) {
                                    *active = masks.last().map(|m| m.id);
                                }
                            }
                        }
                    }
                    Ok(())
                })
            }
        ),
        cmd!("mask.move", "Move Mask", [], None, "{id?, to?: index (0 = top), delta?: ±n}: reorder the masks list", has_active, |s, p| {
            let mid = mask_id(p, s.active_mask, "mask.move")?;
            let mut masks = s.active().and_then(|id| s.develop_of(id)).map(|d| d.masks.clone()).unwrap_or_default();
            let i = find(&mut masks, mid, "mask.move")?;
            let to = match (p.get("to").and_then(Value::as_i64), p.get("delta").and_then(Value::as_i64)) {
                (Some(t), _) => t,
                // (an agent's delta can be anything: saturate rather than overflow)
                (None, Some(d)) => (i as i64).saturating_add(d),
                (None, None) => return Err(bad("mask.move", "give `to` (index) or `delta`")),
            }
            .clamp(0, masks.len() as i64 - 1) as usize;
            if to == i {
                return Ok(json!({"activeMask": s.active_mask}));
            }
            masks_edit(s, "mask.move", "Reorder Masks", |masks, _| {
                let i = find(masks, mid, "mask.move")?;
                let m = masks.remove(i);
                masks.insert(to, m);
                Ok(())
            })
        }),
        // ---- Remove tool (spots)
        cmd!(
            "spot.add",
            "Add Remove Spot",
            [],
            None,
            "{mode?: remove|heal|clone|fill (content-aware), points: [[x,y],…], size?: fraction of long edge, feather?, opacity?, source?: [dx,dy] (default: the best match nearby; a brushed Remove stroke has none and is rebuilt from its surroundings)}: selects the new spot; returns {index}",
            has_active,
            |s, p| {
                let mode: SpotMode =
                    serde_json::from_value(p.get("mode").cloned().unwrap_or(json!("remove"))).map_err(|e| bad("spot.add", e.to_string()))?;
                let pts: Vec<Point> = p
                    .get("points")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(|q| Some(Point::new(q.get(0)?.as_f64()?, q.get(1)?.as_f64()?))).collect())
                    .unwrap_or_default();
                if pts.is_empty() {
                    return Err(bad("spot.add", "missing points"));
                }
                let d = Spot::default();
                let mut spot = Spot {
                    mode,
                    points: pts,
                    size: f64_or(p, "size", d.size).clamp(SPOT_SIZE.0, SPOT_SIZE.1),
                    feather: f64_or(p, "feather", d.feather).clamp(0.0, 100.0),
                    opacity: f64_or(p, "opacity", d.opacity).clamp(0.0, 100.0),
                    source_offset: point(p, "source"),
                    generated: None,
                    auto: None,
                };
                let id = s.active().ok_or_else(|| bad("spot.add", "no active photo"))?;
                let mut dd = (*s.develop_of(id).unwrap_or_default()).clone();
                // a brushed Remove stroke is rebuilt from its surroundings (no single source fits it)
                let synthesised = spot.mode == SpotMode::Remove && lcc_pipeline::spots::brushed(&spot);
                if spot.source_offset.is_none() && !synthesised && !matches!(spot.mode, SpotMode::Fill | SpotMode::Generate | SpotMode::Flat) {
                    // resolve the automatic source now: it gets a pin and stays put at every size
                    spot.source_offset = pick_source(s, id, &dd, &spot, None);
                }
                dd.spots.push(spot);
                let n = dd.spots.len();
                s.set_develop(id, dd, "Remove")?;
                s.active_spot = Some(n - 1);
                Ok(json!({"index": n - 1}))
            }
        ),
        cmd!("spot.select", "Select Spot", [], None, "{index|null}", has_active, |s, p| {
            let n = spots_len(s);
            s.active_spot = match super::index_opt(p, "index", "spot.select")? {
                Some(i) if i < n => Some(i),
                Some(_) => return Err(bad("spot.select", "no such spot")),
                None => None,
            };
            Ok(json!({"activeSpot": s.active_spot}))
        }),
        cmd!(
            "spot.update",
            "Edit Spot",
            [],
            None,
            "{index? (the selected spot), move?: [dx,dy] (the target, normalized), source?: [dx,dy] (offset from the target), moveSource?: [dx,dy], size?, feather?, opacity?, mode?: remove|heal|clone|fill|flat, repick?: bool (a Remove spot chooses its source again)}: moving or resizing a spot Remove Dust added makes it an ordinary spot",
            has_active,
            |s, p| {
                let c = "spot.update";
                let i = spot_index(s, p, c)?;
                let mode: Option<SpotMode> = match p.get("mode") {
                    Some(m) => Some(serde_json::from_value(m.clone()).map_err(|e| bad(c, e.to_string()))?),
                    None => None,
                };
                let id = s.active().ok_or_else(|| bad(c, "no active photo"))?;
                let before = s.develop_of(id).unwrap_or_default();
                let mut dd = (*before).clone();
                let sp = dd.spots.get_mut(i).ok_or_else(|| bad(c, "no such spot"))?;
                // a generated fill is paid for and fits only the stroke it was made for: its spot is not moved or
                // resized (a brush size or a nudge meant for the next stroke hid it, test note 5)
                if sp.shown_gen().is_some() && (p.get("move").is_some() || p.get("size").is_some()) {
                    return Err(bad(c, GENERATED_STAYS));
                }
                if let Some(d) = point(p, "move") {
                    sp.points.iter_mut().for_each(|q| *q = Point::new(q.x + d.x, q.y + d.y));
                    // the source moves along (its offset is relative to the target)
                }
                if let Some(o) = point(p, "source") {
                    sp.source_offset = Some(o);
                }
                if let Some(d) = point(p, "moveSource") {
                    let o = sp.source_offset.unwrap_or(Point::new(0.0, 0.0));
                    sp.source_offset = Some(Point::new(o.x + d.x, o.y + d.y));
                }
                if let Some(v) = p.get("size").and_then(Value::as_f64) {
                    sp.size = v.clamp(SPOT_SIZE.0, SPOT_SIZE.1);
                }
                if let Some(v) = p.get("feather").and_then(Value::as_f64) {
                    sp.feather = v.clamp(0.0, 100.0);
                }
                if let Some(v) = p.get("opacity").and_then(Value::as_f64) {
                    sp.opacity = v.clamp(0.0, 100.0);
                }
                if let Some(m) = mode {
                    sp.mode = m;
                }
                // a spot Remove Dust added that the user moves or resizes becomes theirs (a later Remove Dust keeps it)
                if p.get("move").is_some() || p.get("size").is_some() {
                    sp.auto = None;
                }
                // Remove picks its own source: asked with `repick`, a Remove spot chooses again where it
                // now is, unless this call set the source; Heal and Clone keep theirs (a dragged spot
                // keeps its source, as it always has)
                let given = p.get("source").is_some() || p.get("moveSource").is_some();
                let repick = !given && bool_or(p, "repick", false) && sp.mode == SpotMode::Remove && sp.source_offset.is_some();
                if repick {
                    let spot = sp.clone();
                    if let Some(next) = pick_source(s, id, &dd, &spot, None)
                        && let Some(t) = dd.spots.get_mut(i)
                    {
                        t.source_offset = Some(next);
                    }
                }
                // (nothing changed, e.g. a repick that found the same source: no undo step)
                if dd != *before {
                    s.set_develop(id, dd, "Edit Spot")?;
                }
                Ok(json!({"index": i}))
            }
        ),
        cmd!(
            "spot.refreshSource",
            "Refresh Source",
            [],
            None,
            "{index? (the selected spot)}: picks the next-best source away from the current one",
            has_active,
            |s, p| {
                let c = "spot.refreshSource";
                let i = spot_index(s, p, c)?;
                let id = s.active().ok_or_else(|| bad(c, "no active photo"))?;
                let dd = (*s.develop_of(id).unwrap_or_default()).clone();
                let spot = dd.spots[i].clone();
                let next = if spot.mode == SpotMode::Fill {
                    let cur = spot.source_offset.unwrap_or(lcc_geom::Point::new(0.0, 0.0));
                    lcc_geom::Point::new((cur.x + 0.03) % 0.5, (cur.y + 0.03) % 0.5)
                } else {
                    pick_source(s, id, &dd, &spot, spot.source_offset).ok_or_else(|| bad(c, "no other source fits in the photo"))?
                };
                spots_edit(s, c, "Refresh Source", |spots| {
                    spots[i].source_offset = Some(next);
                    Ok(())
                })?;
                Ok(json!({"index": i, "source": [next.x, next.y]}))
            }
        ),
        cmd!("spot.delete", "Delete Spot", [], None, "{index? (the selected spot)}", has_active, |s, p| {
            let c = "spot.delete";
            let i = spot_index(s, p, c)?;
            spots_edit(s, c, "Delete Spot", |spots| {
                spots.remove(i);
                Ok(())
            })?;
            s.active_spot = match s.active_spot {
                Some(a) if a == i => None,
                Some(a) if a > i => Some(a - 1),
                a => a,
            };
            ok()
        }),
        // ---- Red eye / pet eye
        cmd!(
            "redeye.add",
            "Add Red Eye Correction",
            [],
            None,
            "{center: [x,y] normalized, rx, ry: radii as fractions of the long edge, pet?: bool, pupilSize?: 0..100, darken?: 0..100}: the pupil inside is found automatically; returns {index}",
            has_active,
            |s, p| {
                let c = "redeye.add";
                let center = point(p, "center").ok_or_else(|| bad(c, "missing center"))?;
                let d = RedEye::default();
                let eye = RedEye {
                    center,
                    rx: f64_or(p, "rx", d.rx).clamp(1e-4, 0.5),
                    ry: f64_or(p, "ry", d.ry).clamp(1e-4, 0.5),
                    pupil_size: f64_or(p, "pupilSize", d.pupil_size).clamp(0.0, 100.0),
                    darken: f64_or(p, "darken", d.darken).clamp(0.0, 100.0),
                    pet: bool_or(p, "pet", false),
                    catchlight: None,
                };
                let id = s.active().ok_or_else(|| bad(c, "no active photo"))?;
                if s.develop_of(id).is_some_and(|d| d.red_eye.len() >= MAX_RED_EYES) {
                    return Err(bad(c, format!("at most {MAX_RED_EYES} red eye corrections")));
                }
                let mut dd = (*s.develop_of(id).unwrap_or_default()).clone();
                dd.red_eye.push(eye);
                let n = dd.red_eye.len();
                s.set_develop(id, dd, if eye.pet { "Pet Eye" } else { "Red Eye" })?;
                Ok(json!({"index": n - 1}))
            }
        ),
        cmd!(
            "redeye.update",
            "Edit Red Eye Correction",
            [],
            None,
            "{index, move?: [dx,dy] normalized (the centre stays on the photo), rx?, ry?: radii as fractions of the long edge}",
            has_active,
            |s, p| {
                let c = "redeye.update";
                let i = super::index_req(p, "index", c)?;
                let id = s.active().ok_or_else(|| bad(c, "no active photo"))?;
                let mut dd = (*s.develop_of(id).unwrap_or_default()).clone();
                let eye = dd.red_eye.get_mut(i).ok_or_else(|| bad(c, "no such eye"))?;
                if let Some(d) = point(p, "move") {
                    eye.center = Point::new((eye.center.x + d.x).clamp(0.0, 1.0), (eye.center.y + d.y).clamp(0.0, 1.0));
                }
                eye.rx = f64_or(p, "rx", eye.rx).clamp(1e-4, 0.5);
                eye.ry = f64_or(p, "ry", eye.ry).clamp(1e-4, 0.5);
                let label = if eye.pet { "Pet Eye" } else { "Red Eye" };
                s.set_develop(id, dd, label)?;
                Ok(json!({"index": i}))
            }
        ),
        cmd!(
            "redeye.catchlight",
            "Pet Eye Catchlight",
            [],
            None,
            "{index, on?: bool (default true), offset?: [dx, dy] from the pupil centre in pupil radii (default [-0.35, -0.35])}",
            has_active,
            |s, p| {
                let c = "redeye.catchlight";
                let i = super::index_req(p, "index", c)?;
                let id = s.active().ok_or_else(|| bad(c, "no active photo"))?;
                let mut dd = (*s.develop_of(id).unwrap_or_default()).clone();
                let eye = dd.red_eye.get_mut(i).ok_or_else(|| bad(c, "no such eye"))?;
                if !eye.pet {
                    return Err(bad(c, "catchlights are for pet eyes"));
                }
                let off = point(p, "offset").or(eye.catchlight).unwrap_or(Point::new(-0.35, -0.35));
                eye.catchlight = bool_or(p, "on", true).then(|| Point::new(off.x.clamp(-1.0, 1.0), off.y.clamp(-1.0, 1.0)));
                s.set_develop(id, dd, "Catchlight")?;
                ok()
            }
        ),
        cmd!("redeye.delete", "Delete Red Eye Correction", [], None, "{index}", has_active, |s, p| {
            let c = "redeye.delete";
            let i = super::index_req(p, "index", c)?;
            let id = s.active().ok_or_else(|| bad(c, "no active photo"))?;
            let mut dd = (*s.develop_of(id).unwrap_or_default()).clone();
            if i >= dd.red_eye.len() {
                return Err(bad(c, "no such eye"));
            }
            dd.red_eye.remove(i);
            s.set_develop(id, dd, "Delete Red Eye")?;
            ok()
        }),
    ]
}

/// Spot radius limits (fraction of the long edge).
pub const SPOT_SIZE: (f64, f64) = (0.001, 0.25);

fn spots_len(s: &Session) -> usize {
    s.active().and_then(|id| s.develop_of(id)).map(|d| d.spots.len()).unwrap_or(0)
}

/// `index` from the params, else the selected spot.
fn spot_index(s: &Session, p: &Value, c: &str) -> Result<usize> {
    let i = super::index_opt(p, "index", c)?.or(s.active_spot).ok_or_else(|| bad(c, "no spot selected (give `index`)"))?;
    if i >= spots_len(s) {
        return Err(bad(c, "no such spot"));
    }
    Ok(i)
}

fn spots_edit(s: &mut Session, c: &str, label: &str, f: impl FnOnce(&mut Vec<Spot>) -> Result<()>) -> Result<()> {
    let id = s.active().ok_or_else(|| bad(c, "no active photo"))?;
    let mut d = (*s.develop_of(id).unwrap_or_default()).clone();
    f(&mut d.spots)?;
    s.set_develop(id, d, label)
}

/// An automatic source for `spot` on photo `id` (a small proxy of the photo, framed by `d`).
pub(super) fn pick_source(s: &mut Session, id: crate::PhotoId, d: &lcc_develop::DevelopSettings, spot: &Spot, avoid: Option<Point>) -> Option<Point> {
    let src = s.source_now(id, crate::media::SourceLevel::Thumb).ok()?;
    let info = s.source_info(id);
    // (on the photo as the Source plug-in filters turn it)
    let aux = lcc_pipeline::source_stage::aux_source(&src, &info, d);
    lcc_pipeline::spots::pick_source(aux.as_deref().unwrap_or(&src), &info, d, spot, avoid)
}

pub(crate) fn clamp_local(mut a: LocalAdjustments) -> LocalAdjustments {
    let c = |v: &mut f64, lo: f64, hi: f64| *v = if v.is_finite() { v.clamp(lo, hi) } else { 0.0 };
    c(&mut a.exposure, -4.0, 4.0);
    for v in [
        &mut a.temp,
        &mut a.tint,
        &mut a.contrast,
        &mut a.highlights,
        &mut a.shadows,
        &mut a.whites,
        &mut a.blacks,
        &mut a.texture,
        &mut a.clarity,
        &mut a.dehaze,
        &mut a.saturation,
        &mut a.sharpness,
        &mut a.noise,
        &mut a.moire,
        &mut a.defringe,
    ] {
        c(v, -100.0, 100.0);
    }
    c(&mut a.hue, -lcc_develop::LOCAL_HUE_MAX, lcc_develop::LOCAL_HUE_MAX);
    c(&mut a.color_hue, 0.0, 360.0);
    c(&mut a.color_sat, 0.0, 100.0);
    for v in [&mut a.grade_shadows_hue, &mut a.grade_midtones_hue, &mut a.grade_highlights_hue, &mut a.grade_global_hue] {
        c(v, 0.0, 360.0);
    }
    for v in [&mut a.grade_shadows_sat, &mut a.grade_midtones_sat, &mut a.grade_highlights_sat, &mut a.grade_global_sat] {
        c(v, 0.0, 100.0);
    }
    for v in [&mut a.grade_shadows_lum, &mut a.grade_midtones_lum, &mut a.grade_highlights_lum, &mut a.grade_global_lum] {
        c(v, -100.0, 100.0);
    }
    c(&mut a.amount, 0.0, 200.0);
    a
}

/// One stroke of `mask.brushStroke` from `r` (the command's parameters, or one of its `runs`), the values `r`
/// leaves out taken from `top` (the command's parameters) or the defaults. New strokes paint with Lightroom's
/// brush unless `model: 0` asks for the legacy one.
fn brush_stroke_of(r: &Value, top: &Value) -> Result<BrushStroke> {
    let points: Vec<Point> = r
        .get("points")
        .and_then(Value::as_array)
        .ok_or_else(|| bad("mask.brushStroke", "missing points"))?
        .iter()
        .filter_map(|q| Some(Point::new(q.get(0)?.as_f64()?, q.get(1)?.as_f64()?)))
        .collect();
    let d = BrushStroke::default();
    let num = |k: &str, dflt: f64| r.get(k).and_then(Value::as_f64).filter(|v| v.is_finite()).unwrap_or_else(|| f64_or(top, k, dflt));
    let flag = |k: &str| r.get(k).and_then(Value::as_bool).unwrap_or_else(|| bool_or(top, k, false));
    let pressure = pressure_list(r.get("pressure"), points.len())?;
    let model = r.get("model").or_else(|| top.get("model")).and_then(Value::as_u64);
    Ok(BrushStroke {
        size: num("size", d.size),
        feather: num("feather", d.feather),
        flow: num("flow", d.flow),
        density: num("density", d.density),
        erase: flag("erase"),
        auto_mask: flag("autoMask"),
        // fixed with the stroke: a later change of the slider leaves it as painted
        auto_range: num("autoRange", d.auto_range).clamp(-lcc_develop::AUTO_RANGE_MAX, lcc_develop::AUTO_RANGE_MAX),
        pressure,
        model: if model == Some(0) { BRUSH_MODEL_LEGACY } else { BRUSH_MODEL_LRC },
        points,
    })
}

/// `mask.brushStroke`'s `pressure`: one number 0..1 per point (clamped), at most one per point; absent or `null`
/// is no pressure data.
fn pressure_list(v: Option<&Value>, points: usize) -> Result<Vec<f32>> {
    let Some(v) = v.filter(|v| !v.is_null()) else { return Ok(Vec::new()) };
    let list = v.as_array().ok_or_else(|| bad("mask.brushStroke", "pressure must be a list of numbers 0..1"))?;
    if list.len() > points {
        return Err(bad("mask.brushStroke", "pressure has more values than points"));
    }
    list.iter()
        .map(|q| q.as_f64().filter(|f| f.is_finite()).map(|f| f.clamp(0.0, 1.0) as f32))
        .collect::<Option<Vec<f32>>>()
        .ok_or_else(|| bad("mask.brushStroke", "pressure must be a list of numbers 0..1"))
}

#[cfg(test)]
mod tests_color_range {
    use super::*;

    /// Automation (`mask.add`/`mask.update`) used to take any number of colour-range samples; render
    /// cost is pixels x samples, so a few hundred hung a large render. The list is now capped.
    #[test]
    fn color_range_samples_are_capped() {
        let samples: Vec<Value> = (0..1000).map(|i| json!([i as f64 / 1000.0, 0.1, 0.2])).collect();
        let shape = shape_from("colorRange", &json!({ "samples": samples }), "mask.add").unwrap();
        let MaskShape::ColorRange { samples, .. } = shape else { panic!("expected ColorRange") };
        assert_eq!(samples.len(), MAX_COLOR_RANGE_SAMPLES);
        // A reasonable request is unchanged.
        let few = shape_from("colorRange", &json!({ "samples": [[0.1, 0.2, 0.3], [0.4, 0.5, 0.6]] }), "mask.add").unwrap();
        let MaskShape::ColorRange { samples, .. } = few else { panic!() };
        assert_eq!(samples.len(), 2);
        // mask.update deserializes the shape directly; it is capped the same way.
        let mut shape = shape_from("colorRange", &json!({ "samples": [[0.1, 0.2, 0.3]] }), "mask.add").unwrap();
        if let MaskShape::ColorRange { samples, .. } = &mut shape {
            samples.resize(1000, samples[0]);
        }
        cap_color_range_samples(&mut shape);
        let MaskShape::ColorRange { samples, .. } = shape else { panic!() };
        assert_eq!(samples.len(), MAX_COLOR_RANGE_SAMPLES);
    }
}

#[cfg(test)]
mod pick_tests {
    use super::*;

    // issue #1145: the mask Hue clamps to ±180°, hostile values never pass through
    #[test]
    fn mask_hue_clamps_to_half_a_turn() {
        for (bad, want) in [(f64::NAN, 0.0), (f64::INFINITY, 0.0), (1e300, 180.0), (-500.0, -180.0), (-180.0, -180.0), (120.0, 120.0)] {
            assert_eq!(clamp_local(LocalAdjustments { hue: bad, ..Default::default() }).hue, want, "{bad}");
        }
    }

    #[test]
    fn an_area_gives_its_main_colours_biggest_first() {
        // 70 % one colour, 30 % another, a few stray pixels of a third
        let (a, b, c) = ([0.8, 0.0, 0.0], [0.3, 0.1, -0.1], [0.5, -0.2, 0.2]);
        let mut px = vec![a; 70];
        px.extend(vec![b; 30]);
        px.extend(vec![c; 3]);
        let out = colour_clusters(&px, 3);
        assert_eq!(out.len(), 2, "the stray pixels make no sample: {out:?}");
        assert!(out[0].iter().zip(a).all(|(x, y)| (x - y).abs() < 0.02), "{out:?}");
        assert!(out[1].iter().zip(b).all(|(x, y)| (x - y).abs() < 0.03), "{out:?}");
        // one colour asked: the mean; nothing given: nothing
        assert_eq!(colour_clusters(&[a, a], 1).len(), 1);
        assert!(colour_clusters(&[], 3).is_empty());
        // a single colour never splits
        assert_eq!(colour_clusters(&vec![a; 50], 3).len(), 1);
    }

    #[test]
    fn colour_pins_group_the_samples_of_one_place() {
        let p = [0.2, 0.2, 0.2, 0.2];
        let area = [0.1, 0.1, 0.4, 0.3];
        let pins = lcc_develop::color_range_pins(&[UNKNOWN_AREA, p, area, area, area, [f64::NAN; 4]]);
        assert_eq!(pins, vec![(p, 1..2), (area, 2..5)]);
    }
}
