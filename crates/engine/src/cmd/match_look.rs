//! Match Look (`develop.matchLook`): the selected photos take the active photo's overall look, as Capture One's
//! Smart Adjustments / Normalize do: each photo's Exposure, Contrast and global colour-grading tint are solved so
//! its rendered picture has the reference's mean lightness, lightness spread and mean colour cast.
//!
//! An original method: statistics are taken in Oklab (Björn Ottosson's published formulas, public domain /
//! MIT) on small renders through the real pipeline; a finite-difference Jacobian of those statistics against the
//! sliders is measured once per photo, then a few damped Newton steps move the sliders. Only those three controls
//! change, so the result stays an ordinary, editable edit.

use lcc_catalog::{Op, PhotoId};
use lcc_develop::DevelopSettings;
use serde_json::{Value, json};

use super::{CommandSpec, bad, cmd, has_selection};
use crate::{Result, Session, media::SourceLevel};

/// Long edge of the renders the statistics are measured on.
const EDGE: usize = 160;
/// Newton steps after the Jacobian.
const STEPS: usize = 4;

/// Mean Oklab L, the standard deviation of L, mean a and mean b of an sRGB image; `None` for an empty one.
pub(crate) fn look_stats(img: &lcc_raster::Rgba8) -> Option<[f64; 4]> {
    fn lin(v: u8) -> f64 {
        let c = f64::from(v) / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    }
    let (mut n, mut sl, mut sll, mut sa, mut sb) = (0.0f64, 0.0, 0.0, 0.0, 0.0);
    for p in &img.data {
        let (r, g, b) = (lin(p[0]), lin(p[1]), lin(p[2]));
        let l = (0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b).cbrt();
        let m = (0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b).cbrt();
        let s = (0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b).cbrt();
        let ll = 0.210_454_255_3 * l + 0.793_617_785 * m - 0.004_072_046_8 * s;
        let a = 1.977_998_495_1 * l - 2.428_592_205 * m + 0.450_593_709_9 * s;
        let bb = 0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766 * s;
        n += 1.0;
        sl += ll;
        sll += ll * ll;
        sa += a;
        sb += bb;
    }
    if n < 1.0 {
        return None;
    }
    let mean = sl / n;
    let out = [mean, (sll / n - mean * mean).max(0.0).sqrt(), sa / n, sb / n];
    out.iter().all(|v| v.is_finite()).then_some(out)
}

/// The sliders Match Look moves: Exposure (EV), Contrast, and the global colour-grading wheel as a vector.
#[derive(Clone, Copy)]
enum Knob {
    Exposure,
    Contrast,
    TintX,
    TintY,
}

impl Knob {
    fn get(self, d: &DevelopSettings) -> f64 {
        let w = &d.grading.global;
        match self {
            Knob::Exposure => d.light.exposure,
            Knob::Contrast => d.light.contrast,
            Knob::TintX => w.sat * w.hue.to_radians().cos(),
            Knob::TintY => w.sat * w.hue.to_radians().sin(),
        }
    }
    fn set(self, d: &mut DevelopSettings, v: f64) {
        let v = if v.is_finite() { v } else { 0.0 };
        match self {
            Knob::Exposure => d.light.exposure = v.clamp(-5.0, 5.0),
            Knob::Contrast => d.light.contrast = v.clamp(-100.0, 100.0),
            Knob::TintX | Knob::TintY => {
                let (mut x, mut y) = (Knob::TintX.get(d), Knob::TintY.get(d));
                if matches!(self, Knob::TintX) {
                    x = v
                } else {
                    y = v
                }
                let sat = x.hypot(y).min(100.0);
                let w = &mut d.grading.global;
                w.sat = sat;
                if sat > 1e-9 {
                    w.hue = y.atan2(x).to_degrees().rem_euclid(360.0);
                }
            }
        }
    }
    /// The finite-difference step.
    fn step(self) -> f64 {
        match self {
            Knob::Exposure => 0.5,
            Knob::Contrast => 20.0,
            Knob::TintX | Knob::TintY => 15.0,
        }
    }
    /// The statistic this knob mainly drives (index into [`look_stats`]).
    fn stat(self) -> usize {
        match self {
            Knob::Exposure => 0,
            Knob::Contrast => 1,
            Knob::TintX => 2,
            Knob::TintY => 3,
        }
    }
}

/// Solves `m · x = r` (n ≤ 4) by Gaussian elimination with partial pivoting; `None` when singular.
fn solve(mut m: Vec<Vec<f64>>, mut r: Vec<f64>) -> Option<Vec<f64>> {
    let n = r.len();
    for c in 0..n {
        let p = (c..n).max_by(|a, b| m[*a][c].abs().total_cmp(&m[*b][c].abs()))?;
        if m[p][c].abs() < 1e-12 {
            return None;
        }
        m.swap(c, p);
        r.swap(c, p);
        for k in c + 1..n {
            let f = m[k][c] / m[c][c];
            for j in c..n {
                m[k][j] -= f * m[c][j];
            }
            r[k] -= f * r[c];
        }
    }
    let mut x = vec![0.0; n];
    for c in (0..n).rev() {
        let s: f64 = (c + 1..n).map(|j| m[c][j] * x[j]).sum();
        x[c] = (r[c] - s) / m[c][c];
    }
    x.iter().all(|v| v.is_finite()).then_some(x)
}

/// The settings that give photo `id` (rendered from `src`) the statistics `goal`, moving only `knobs`.
fn matched(
    src: &lcc_raster::Rgb32f,
    info: &lcc_pipeline::SourceInfo,
    start: &DevelopSettings,
    goal: [f64; 4],
    knobs: &[Knob],
) -> Option<DevelopSettings> {
    let req = lcc_pipeline::RenderRequest::fit(EDGE, EDGE);
    let stats = |d: &DevelopSettings| look_stats(&lcc_pipeline::render(src, info, d, &req).image);
    let mut d = start.clone();
    let mut cur = stats(&d)?;
    // the Jacobian: rows = the knobs' statistics, columns = the knobs
    let mut jac = vec![vec![0.0; knobs.len()]; knobs.len()];
    for (c, k) in knobs.iter().enumerate() {
        let mut probe = d.clone();
        let x = k.get(&probe);
        // step away from the nearer limit
        let h = if x > 0.0 { -k.step() } else { k.step() };
        k.set(&mut probe, x + h);
        let moved = k.get(&probe) - x;
        let after = stats(&probe)?;
        for (r, kr) in knobs.iter().enumerate() {
            jac[r][c] = if moved.abs() > 1e-9 { (after[kr.stat()] - cur[kr.stat()]) / moved } else { 0.0 };
        }
    }
    for _ in 0..STEPS {
        let r: Vec<f64> = knobs.iter().map(|k| goal[k.stat()] - cur[k.stat()]).collect();
        if r.iter().all(|v| v.abs() < 1e-3) {
            break;
        }
        let Some(dx) = solve(jac.clone(), r) else { break };
        let mut next = d.clone();
        for (k, step) in knobs.iter().zip(&dx) {
            // damped, and no single step past twice the finite-difference stride
            let step = (0.9 * step).clamp(-4.0 * k.step(), 4.0 * k.step());
            let x = k.get(&next);
            k.set(&mut next, x + step);
        }
        let Some(s) = stats(&next) else { break };
        let err = |s: &[f64; 4]| knobs.iter().map(|k| ((goal[k.stat()] - s[k.stat()]) / k.scale()).powi(2)).sum::<f64>();
        if err(&s) > err(&cur) {
            break;
        }
        d = next;
        cur = s;
    }
    Some(d)
}

impl Knob {
    /// Typical size of a visible difference in this knob's statistic, to weigh the errors against each other.
    fn scale(self) -> f64 {
        match self {
            Knob::Exposure | Knob::Contrast => 0.02,
            Knob::TintX | Knob::TintY => 0.005,
        }
    }
}

pub fn specs() -> Vec<CommandSpec> {
    vec![cmd!(
        "develop.matchLook",
        "Match Look",
        ["Photo", "Develop Settings"],
        None,
        "{ids?, tone?: bool (true), color?: bool (true)}: set each selected photo's Exposure and Contrast (tone) and global colour-grading tint (color) so its overall lightness, contrast and colour cast match the active photo's (as Capture One's Smart Adjustments); one undo step; photos that can't be read are skipped → {changed, skipped}",
        has_selection,
        |s, p| run(s, p)
    )]
}

fn run(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "develop.matchLook";
    let flag = |k: &str| p.get(k).and_then(Value::as_bool).unwrap_or(true);
    let mut knobs = Vec::new();
    if flag("tone") {
        knobs.extend([Knob::Exposure, Knob::Contrast]);
    }
    if flag("color") {
        knobs.extend([Knob::TintX, Knob::TintY]);
    }
    if knobs.is_empty() {
        return Err(bad(C, "nothing to match: tone and color are both off"));
    }
    let reference = s.active().ok_or_else(|| bad(C, "no active photo"))?;
    let req = lcc_pipeline::RenderRequest::fit(EDGE, EDGE);
    let goal = {
        let src = s.source_now(reference, SourceLevel::Thumb).map_err(|e| bad(C, e))?;
        let info = s.source_info(reference);
        let d = s.develop_of(reference).unwrap_or_default();
        look_stats(&lcc_pipeline::render(&src, &info, &d, &req).image).ok_or_else(|| bad(C, "the active photo rendered empty"))?
    };
    let ids: Vec<PhotoId> = s.targets(p)?.into_iter().filter(|x| *x != reference).collect();
    let (mut ops, mut skipped) = (Vec::new(), Vec::new());
    for id in ids {
        let (Ok(src), Some(d)) = (s.source_now(id, SourceLevel::Thumb), s.develop_of(id)) else {
            skipped.push(id.0);
            continue;
        };
        let info = s.source_info(id);
        let Some(nd) = matched(&src, &info, &d, goal, &knobs) else {
            skipped.push(id.0);
            continue;
        };
        ops.extend(s.develop_op(id, nd, "Match Look"));
    }
    let n = ops.len();
    if n > 0 {
        s.commit("Match Look", Op::Batch { ops })?;
    }
    Ok(json!({"changed": n, "skipped": skipped}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_of_hostile_images_are_none_or_finite() {
        let empty = lcc_raster::Rgba8 { width: 0, height: 0, data: Vec::new() };
        assert!(look_stats(&empty).is_none());
        let black = lcc_raster::Rgba8 { width: 2, height: 1, data: vec![[0, 0, 0, 255]; 2] };
        let s = look_stats(&black).unwrap();
        assert!(s.iter().all(|v| v.is_finite()) && s[0] < 1e-6);
    }

    #[test]
    fn singular_systems_are_refused() {
        assert!(solve(vec![vec![0.0, 0.0], vec![0.0, 0.0]], vec![1.0, 1.0]).is_none());
        let x = solve(vec![vec![2.0, 0.0], vec![0.0, 4.0]], vec![1.0, 1.0]).unwrap();
        assert!((x[0] - 0.5).abs() < 1e-12 && (x[1] - 0.25).abs() < 1e-12);
    }
}
