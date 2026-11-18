//! Commands that sample the developed image: Point Color samples and the targeted adjustment tool
//! (`develop.targeted`: a vertical drag on the photo raises/lowers the tone-curve region or the
//! colour-mixer bands under the pointer; the UI sends one call per drag step inside an interaction).
//!
//! Sampling renders a small proxy with every stage *after* the sampled one neutralized, so the
//! picked colour is the one the adjustment will see (e.g. Point Color samples after the colour
//! mixer, before vibrance, grading, vignette and curves).

use lcc_color::perceptual::{lab_to_lch, oklab_from_2020};
use lcc_color::spline::MonotoneCurve;
use lcc_color::transfer::srgb_to_linear;
use lcc_color::{REC2020, SRGB};
use lcc_develop::{DevelopSettings, MAX_POINT_COLORS, MIXER_BANDS, PointColor, ToneCurve, Treatment, controls};
use lcc_geom::Point;
use serde_json::{Value, json};

use super::{CommandSpec, bad, cmd, f64_req, has_active};
use crate::media::SourceLevel;
use crate::{Result, Session};

/// Long edge of the proxy rendered for sampling.
const PROBE_EDGE: usize = 384;

/// The sRGB-encoded colour (0..1) at normalized image point `(x, y)` of the active photo rendered
/// with its settings changed by `neutral`, averaged over 3 × 3 proxy pixels.
pub(crate) fn probe(s: &mut Session, c: &str, x: f64, y: f64, neutral: impl FnOnce(&mut DevelopSettings)) -> Result<[f32; 3]> {
    let id = s.active().ok_or_else(|| bad(c, "no active photo"))?;
    let src = s.source_now(id, SourceLevel::Thumb).map_err(|e| bad(c, e))?;
    let info = s.source_info(id);
    let mut d = (*s.develop_of(id).unwrap_or_default()).clone();
    neutral(&mut d);
    let req = lcc_pipeline::RenderRequest::fit(PROBE_EDGE, PROBE_EDGE);
    let plan = lcc_pipeline::plan(&src, &info, &d, &req);
    let q = plan.frame.norm_to_out(plan.w, plan.h).apply(Point::new(x.clamp(0.0, 1.0), y.clamp(0.0, 1.0)));
    let img = lcc_pipeline::render(&src, &info, &d, &req).image;
    let (cx, cy) = (q.x.floor() as isize, q.y.floor() as isize);
    let mut acc = [0.0f32; 3];
    let mut n = 0.0;
    for dy in -1..=1 {
        for dx in -1..=1 {
            let (px, py) = ((cx + dx).clamp(0, img.width as isize - 1) as usize, (cy + dy).clamp(0, img.height as isize - 1) as usize);
            let p = img.data[py * img.width + px];
            for k in 0..3 {
                acc[k] += p[k] as f32 / 255.0;
            }
            n += 1.0;
        }
    }
    Ok(acc.map(|v| v / n))
}

/// sRGB-encoded → OkLCh (lightness, chroma, hue in radians) via linear Rec.2020.
pub(crate) fn encoded_to_oklch(e: [f32; 3]) -> [f32; 3] {
    let lin = SRGB.to_space(&REC2020).apply_f32(e.map(srgb_to_linear));
    lab_to_lch(oklab_from_2020(lin))
}

/// Defringe amount the Fringe Color Selector turns a colour on at (full strength is 8 on the 0..20 slider).
const FRINGE_AMOUNT: f64 = 6.0;
/// OkLab hue spans (degrees) the Purple and Green hue sliders (0..100) cover; see `lcc_pipeline::optics`.
const PURPLE_SPAN: (f64, f64) = (240.0, 360.0);
const GREEN_SPAN: (f64, f64) = (90.0, 210.0);

/// Which Defringe colour a sampled hue belongs to and the hue slider range around it.
#[derive(Debug, PartialEq)]
pub(crate) struct FringePick {
    pub purple: bool,
    pub lo: f64,
    pub hi: f64,
}

/// Points Defringe at a sampled colour (OkLCh chroma, hue in degrees): the nearer of the purple and
/// green hue spans, a window of about +-12 degrees around the hue. `None` for a grey, non-finite or
/// unreadable sample.
pub(crate) fn fringe_pick(chroma: f32, hue_deg: f32) -> Option<FringePick> {
    if !chroma.is_finite() || !hue_deg.is_finite() || chroma < 0.015 {
        return None;
    }
    let h = (hue_deg as f64).rem_euclid(360.0);
    // angular distance from the hue to a span (0 inside it)
    let dist = |(a, b): (f64, f64)| {
        let ang = |x: f64, y: f64| {
            let d = (x - y).rem_euclid(360.0);
            d.min(360.0 - d)
        };
        if (a..=b).contains(&h) || (a..=b).contains(&(h + 360.0)) { 0.0 } else { ang(h, a).min(ang(h, b)) }
    };
    let purple = dist(PURPLE_SPAN) <= dist(GREEN_SPAN);
    let (a, b) = if purple { PURPLE_SPAN } else { GREEN_SPAN };
    // the hue as a position in the span (clamped to it when it lies just outside)
    let at = [h, h + 360.0].into_iter().find(|v| (a..=b).contains(v)).unwrap_or_else(|| {
        let ends = [a, b].into_iter().flat_map(|e| [(h - e).abs(), (h + 360.0 - e).abs()].map(|d| (d, e)));
        ends.min_by(|x, y| x.0.total_cmp(&y.0)).map_or(a, |(_, e)| e)
    });
    let centre = (at - a) / (b - a) * 100.0;
    let half = 12.0 / (b - a) * 100.0;
    Some(FringePick { purple, lo: (centre - half).round().clamp(0.0, 90.0), hi: (centre + half).round().clamp(10.0, 100.0) })
}

/// Everything after the colour mixer, neutralized (the colour Point Color sees).
pub(crate) fn after_mixer_neutral(d: &mut DevelopSettings) {
    d.color.vibrance = 0.0;
    d.color.saturation = 0.0;
    d.point_colors.clear();
    d.treatment = Treatment::Color;
    d.grading = Default::default();
    d.vignette.amount = 0.0;
    d.grain.amount = 0.0;
    d.curve = Default::default();
}

/// The targeted adjustment tool: the controls (and their new values) a vertical drag of `delta`
/// at `(x, y)` changes, computed on `d` (applied by the caller).
fn targeted(s: &mut Session, p: &Value) -> Result<Value> {
    let c = "develop.targeted";
    let (x, y, delta) = (f64_req(p, "x", c)?, f64_req(p, "y", c)?, f64_req(p, "delta", c)?);
    let target = p.get("target").and_then(Value::as_str).unwrap_or("curve").to_string();
    let id = s.active().ok_or_else(|| bad(c, "no active photo"))?;
    let mut d = (*s.develop_of(id).unwrap_or_default()).clone();
    let mut changed = serde_json::Map::new();
    fn nudge(changed: &mut serde_json::Map<String, Value>, d: &mut DevelopSettings, ctl: &str, by: f64) {
        let v = controls::get(d, ctl).unwrap_or(0.0) + by;
        controls::set(d, ctl, v);
        changed.insert(ctl.to_string(), json!(controls::get(d, ctl)));
    }
    let label;
    match target.as_str() {
        "curve" => {
            // the encoded value the tone curve sees there (curves and grain neutralized)
            let e = probe(s, c, x, y, |d| {
                d.curve = ToneCurve { refine_saturation: d.curve.refine_saturation, ..Default::default() };
                d.grain.amount = 0.0;
            })?;
            let channel = p.get("channel").and_then(Value::as_str).unwrap_or("parametric");
            label = "Tone Curve";
            if channel == "parametric" {
                let v = (0.2126 * e[0] + 0.7152 * e[1] + 0.0722 * e[2]) as f64 * 100.0;
                let cv = &d.curve;
                let region = if v < cv.split_shadows {
                    "curve.shadows"
                } else if v < cv.split_mid {
                    "curve.darks"
                } else if v < cv.split_highlights {
                    "curve.lights"
                } else {
                    "curve.highlights"
                };
                nudge(&mut changed, &mut d, region, delta);
            } else {
                // point curve: move the point at the sampled input up/down (or add one there)
                let (pts, v) = match channel {
                    "red" => (&mut d.curve.red, e[0]),
                    "green" => (&mut d.curve.green, e[1]),
                    "blue" => (&mut d.curve.blue, e[2]),
                    _ => (&mut d.curve.master, 0.2126 * e[0] + 0.7152 * e[1] + 0.0722 * e[2]),
                };
                let v = v as f64;
                if pts.is_empty() {
                    *pts = vec![Point::new(0.0, 0.0), Point::new(1.0, 1.0)];
                }
                let curve = MonotoneCurve::new(&pts.iter().map(|q| (q.x, q.y)).collect::<Vec<_>>());
                let dy = delta / 255.0;
                match pts.iter().position(|q| (q.x - v).abs() < 0.03) {
                    Some(i) => pts[i].y = (pts[i].y + dy).clamp(0.0, 1.0),
                    None => {
                        let i = pts.iter().position(|q| q.x > v).unwrap_or(pts.len());
                        pts.insert(i, Point::new(v, (curve.eval(v) + dy).clamp(0.0, 1.0)));
                    }
                }
                changed.insert(format!("curve.{channel}"), json!(pts.iter().map(|q| [q.x, q.y]).collect::<Vec<_>>()));
            }
        }
        "hue" | "sat" | "lum" => {
            // the hue the colour mixer sees there (the mixer and everything after it neutralized)
            let e = probe(s, c, x, y, |d| {
                d.mixer = Default::default();
                d.bw_mix = Default::default();
                after_mixer_neutral(d);
            })?;
            let [_, _, h] = encoded_to_oklch(e);
            let w = lcc_pipeline::colorops::band_weights(h);
            let top = w.iter().cloned().fold(0.0f32, f32::max).max(1e-6);
            let bw = d.treatment == Treatment::Bw || d.profile.id == "lc.mono";
            label = if bw { "B&W Mix" } else { "Color Mixer" };
            for (i, band) in MIXER_BANDS.iter().enumerate() {
                let k = (w[i] / top) as f64;
                if k < 0.05 {
                    continue;
                }
                let ctl = if bw { format!("bw.{band}") } else { format!("mixer.{band}.{target}") };
                nudge(&mut changed, &mut d, &ctl, delta * k);
            }
        }
        _ => return Err(bad(c, "target must be curve, hue, sat or lum")),
    }
    s.set_develop(id, d, label)?;
    Ok(Value::Object(changed))
}

fn index(p: &Value, c: &str) -> Result<usize> {
    super::index_req(p, "index", c)
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "pointColor.pick",
            "Add Point Color Sample",
            [],
            None,
            "{x, y} normalized image coords: samples the colour there (max 8); returns {index, lum, chroma, hue}",
            has_active,
            |s, p| {
                let c = "pointColor.pick";
                let (x, y) = (f64_req(p, "x", c)?, f64_req(p, "y", c)?);
                let id = s.active().ok_or_else(|| bad(c, "no active photo"))?;
                if s.develop_of(id).is_some_and(|d| d.point_colors.len() >= MAX_POINT_COLORS) {
                    return Err(bad(c, format!("at most {MAX_POINT_COLORS} point colours")));
                }
                let [l, ch, h] = encoded_to_oklch(probe(s, c, x, y, after_mixer_neutral)?);
                let sample = PointColor { lum: l as f64, chroma: ch as f64, hue: (h as f64).to_degrees().rem_euclid(360.0), ..Default::default() };
                let mut d = (*s.develop_of(id).unwrap_or_default()).clone();
                d.point_colors.push(sample);
                let i = d.point_colors.len() - 1;
                s.set_develop(id, d, "Point Color")?;
                Ok(json!({"index": i, "lum": sample.lum, "chroma": sample.chroma, "hue": sample.hue}))
            }
        ),
        cmd!(
            "develop.fringePick",
            "Fringe Color Selector",
            [],
            None,
            "{x, y} normalized image coords: samples the colour there (3 x 3 proxy pixels) and points Defringe at it: sets the Purple or Green hue range around that hue, and an amount when it was off → {applied: true, colour: purple|green, hueLo, hueHi, amount}, or {applied: false, message} for a spot without colour (nothing changes)",
            has_active,
            |s, p| {
                let c = "develop.fringePick";
                let (x, y) = (f64_req(p, "x", c)?, f64_req(p, "y", c)?);
                if !x.is_finite() || !y.is_finite() {
                    return Err(bad(c, "x and y must be finite numbers"));
                }
                let id = s.active().ok_or_else(|| bad(c, "no active photo"))?;
                // the colour as the fringe is in the photo: Defringe itself switched off
                let e = probe(s, c, x, y, |d| {
                    d.optics.defringe_purple_amount = 0.0;
                    d.optics.defringe_green_amount = 0.0;
                })?;
                let [_, chroma, hue] = encoded_to_oklch(e);
                let Some(pick) = fringe_pick(chroma, hue.to_degrees()) else {
                    return Ok(json!({"applied": false, "message": "This spot has no colour to defringe: click on the fringe itself"}));
                };
                let mut d = (*s.develop_of(id).unwrap_or_default()).clone();
                let o = &mut d.optics;
                let amount = if pick.purple {
                    o.defringe_purple_hue_lo = pick.lo;
                    o.defringe_purple_hue_hi = pick.hi;
                    o.defringe_purple_amount = o.defringe_purple_amount.max(FRINGE_AMOUNT);
                    o.defringe_purple_amount
                } else {
                    o.defringe_green_hue_lo = pick.lo;
                    o.defringe_green_hue_hi = pick.hi;
                    o.defringe_green_amount = o.defringe_green_amount.max(FRINGE_AMOUNT);
                    o.defringe_green_amount
                };
                s.set_develop(id, d, "Fringe Color Selector")?;
                Ok(
                    json!({"applied": true, "colour": if pick.purple { "purple" } else { "green" }, "hueLo": pick.lo, "hueHi": pick.hi, "amount": amount}),
                )
            }
        ),
        cmd!(
            "develop.targeted",
            "Targeted Adjustment",
            [],
            None,
            "{target: curve|hue|sat|lum, x, y (normalized image coords), delta (slider units; point curves: output levels 0..255), channel?: parametric|master|red|green|blue (curve)}: adjusts the curve region / colour-mixer bands under the point; returns the changed controls",
            has_active,
            targeted
        ),
        cmd!("pointColor.delete", "Delete Point Color Sample", [], None, "{index}", has_active, |s, p| {
            let c = "pointColor.delete";
            let i = index(p, c)?;
            let id = s.active().ok_or_else(|| bad(c, "no active photo"))?;
            let mut d = (*s.develop_of(id).unwrap_or_default()).clone();
            if i >= d.point_colors.len() {
                return Err(bad(c, "no such sample"));
            }
            d.point_colors.remove(i);
            s.set_develop(id, d, "Delete Point Color")?;
            Ok(Value::Null)
        }),
    ]
}

#[cfg(test)]
mod fringe_tests {
    use super::*;

    #[test]
    fn fringe_pick_chooses_colour_and_window_and_refuses_unreadable_samples() {
        // fringe pick chooses colour and window
        {
            let p = fringe_pick(0.1, 300.0).unwrap();
            assert!(p.purple && p.lo < 50.0 && p.hi > 50.0 && p.lo >= 0.0 && p.hi <= 100.0, "{p:?}");
            let g = fringe_pick(0.1, 150.0).unwrap();
            assert!(!g.purple && g.lo < 50.0 && g.hi > 50.0, "{g:?}");
            // wraps: 10 degrees is nearer the purple span's end (360) than green's start (90)
            assert!(fringe_pick(0.1, 10.0).unwrap().purple);
            assert!(fringe_pick(0.1, -60.0).unwrap().purple);
        }
        // fringe pick refuses unreadable samples
        {
            for (c, h) in [(f32::NAN, 100.0), (0.1, f32::NAN), (0.1, f32::INFINITY), (0.0, 100.0), (-1.0, 100.0), (f32::INFINITY, f32::NAN)] {
                assert!(fringe_pick(c, h).is_none(), "{c} {h}");
            }
            // any finite hue gives an in-range window
            for h in [-1e9f32, -720.5, 0.0, 359.99, 1e9] {
                let p = fringe_pick(0.2, h).unwrap();
                assert!(p.lo >= 0.0 && p.hi <= 100.0 && p.lo <= p.hi, "{h}: {p:?}");
            }
        }
    }
}
