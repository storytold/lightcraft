//! The fit: Lightroom's white points, tone curve, forward matrices, hue/saturation map and per-look look tables,
//! each fitted through our own decode and pipeline (rendered and tabulated, [`pipe::Lut`]) so the result reproduces
//! Lightroom's colour where the app actually applies it.
//!
//! Stages:
//! 1. White points: `ColorMatrix1/2` (StdA, D65) by linear least squares so each photo's camera neutral maps to
//!    Lightroom's white (its temperature and tint), regularised toward a prior (the camera database's matrices, or a
//!    neutral sRGB-like camera).
//! 2. Tone: the base look's tone curve from luminance (conditional median per scene-luminance bin), then a 1-D closed
//!    loop through the rendered pipeline until neutral greys land where Lightroom puts them.
//! 3. `ForwardMatrix1/2` by Levenberg–Marquardt on Lab error through the pipeline, weighted by each photo's
//!    calibration-1 weight, regularised toward the prior.
//! 4. `HueSatMap1/2` (90 × 30 × 1, ProPhoto HSV): each sample's target inverted through the pipeline to a scene
//!    colour, the needed correction splatted at the table input, solved with smoothness; the StdA table shrinks
//!    toward the shared one when few low-CCT photos back it. Refined for `rounds` rounds on residuals.
//! 5. Every other look (exports with another `crs:CameraProfile`): its own tone curve and a look table on top of the
//!    base camera model.
//! 6. Closed loop through the real decode ([`real`]): the candidate is installed as an imported profile, the training
//!    raws are rendered at a small size and read at the samples' positions, and the gap between the real render and
//!    the analytic model (`model` + the pipeline LUT) is folded into the targets; tone curves and tables are refitted
//!    on them for [`LOOP_ROUNDS`] rounds (a round that makes the real training error worse is undone).
//! 7. Gates: held-out ΔE00 median and p95, from real renders (the app's own colour `lc` before, the candidate after),
//!    must beat the app's current colour. When a raw can't be rendered, the analytic numbers stand in, with a note.
//!    A Lightroom fit whose base look loses to the current colour (on raws, each one's look fitted to its camera
//!    JPEG) but beats the plain colour (the prior matrices, analytic) is kept for files with colour matrices of their
//!    own only (`own_matrix_only`: converted DNGs, which get no JPEG fit; issue #1119).
use crate::dataset::{DngMatrices, PhotoSamples};
use crate::deltae;
use crate::{Error, Result};
use lcc_camdb::calibration::{Calibration, CameraModel, CctBin, DeltaE, Editor, Evidence, LICENCE, Look, LookEvidence, VERSION};
use lcc_color::{D50, D65, Mat3, SRGB, Xy};
use lcc_raw::profile::ToneCurve;

pub(crate) mod linalg;
mod model;
pub(crate) mod pipe;
mod real;
mod table;

use model::{LookModel, Model, PhotoDecode};

/// Options for the fit.
#[derive(Clone, Debug, Default)]
pub struct FitOptions {
    pub contributor: Option<String>,
    /// The editor whose exports these are. Only Lightroom's carry a temperature and tint to place the white points
    /// by; another editor's calibration keeps the prior's white points and names its looks after the editor.
    pub editor: Editor,
    /// Refinement rounds of the tables on their residuals (default 3 when 0).
    pub rounds: usize,
}

const T_A: f64 = 2856.0;
const T_D65: f64 = 6504.0;
const XY_A: Xy = Xy::new(0.44757, 0.40745);
/// Samples the matrix fit uses (spread over all photos).
const LM_SAMPLES: usize = 3000;
/// Samples a table round uses.
const TABLE_SAMPLES: usize = 40_000;
/// Closed-loop rounds through the real decode.
const LOOP_ROUNDS: usize = 2;

/// Calibration-1 (StdA) weight of a white of temperature `t` (inverse-CCT interpolation, DNG chapter 6).
fn g_of_temp(t: f64) -> f64 {
    ((1.0 / t - 1.0 / T_D65) / (1.0 / T_A - 1.0 / T_D65)).clamp(0.0, 1.0)
}

/// Lightroom's white of a photo: temperature and xy.
fn lr_white(p: &PhotoSamples) -> Option<(f64, Xy)> {
    let (t, tint) = p.white.lightroom_temp_tint?;
    if !(t.is_finite() && (1500.0..=50_000.0).contains(&t) && tint.is_finite() && tint.abs() <= 300.0) {
        return None;
    }
    let xy = lcc_color::cct::temp_tint_to_xy(t, tint);
    (xy.x.is_finite() && xy.y.is_finite() && xy.y > 0.0).then_some((t, xy))
}

/// The camera neutral, green-normalised, when usable.
fn neutral(p: &PhotoSamples) -> Option<[f64; 3]> {
    let n = p.white.neutral;
    if !n.iter().all(|v| v.is_finite() && *v > 1e-6) {
        return None;
    }
    let g = n[1];
    Some(n.map(|v| v / g))
}

fn baseline(p: &PhotoSamples) -> f64 {
    let b = p.white.baseline_exposure;
    if b.is_finite() { b.clamp(-8.0, 8.0) } else { 0.0 }
}

fn matrix_ok(m: &Mat3) -> bool {
    m.0.iter().flatten().all(|v| v.is_finite() && v.abs() < 8.0) && m.determinant().abs() > 1e-5 && m.inverse().is_some()
}

/// White-balanced camera → XYZ D50 for colour matrix `cm` under white `w`: `FM · 1 = D50`.
fn derived_fm(cm: &Mat3, w: Xy) -> Option<Mat3> {
    let n = cm.apply(w.to_xyz());
    let fm = lcc_color::bradford(w, D50).mul(&cm.inverse()?).mul(&Mat3::diag(n[0], n[1], n[2]));
    matrix_ok(&fm).then_some(fm)
}

/// The prior camera model: the camera database's matrices, else a camera that sees like linear sRGB.
#[derive(Clone, Debug)]
struct Prior {
    cm: [Mat3; 2],
    fm: Option<[Mat3; 2]>,
    /// The forward matrices the fit is pulled toward (the prior's, or derived from its colour matrices).
    fm_target: [Mat3; 2],
}

/// Colour (and forward) matrices ordered StdA-like first, from a file's or the database's tags; `None` when the colour
/// matrices are missing or unusable.
fn ordered_tags(illuminants: [u16; 2], cm: &[Option<Mat3>; 2], fm: &[Option<Mat3>; 2]) -> Option<([Mat3; 2], Option<[Mat3; 2]>)> {
    let temp = |i: usize| lcc_raw::color::illuminant_temperature(illuminants[i]).unwrap_or(5000.0);
    let ordered = |pair: &[Option<Mat3>; 2]| -> Option<[Mat3; 2]> {
        match pair {
            [Some(a), Some(b)] => Some(if temp(0) <= temp(1) { [*a, *b] } else { [*b, *a] }),
            [Some(a), None] | [None, Some(a)] => Some([*a, *a]),
            [None, None] => None,
        }
    };
    let cm = ordered(cm).filter(|m| m.iter().all(matrix_ok))?;
    let fm = match fm {
        [Some(_), Some(_)] => ordered(fm).filter(|m| m.iter().all(matrix_ok)),
        _ => None,
    };
    Some((cm, fm))
}

/// The prior: the raw's own matrices when it has them, else the camera database's, else a camera that sees like
/// linear sRGB.
fn prior(make: &str, model_name: &str, dng: Option<&DngMatrices>) -> Prior {
    let fallback = SRGB.from_xyz();
    let own = dng.and_then(|d| ordered_tags(d.illuminant, &d.color_matrix, &d.forward_matrix));
    let (cm, fm) = own
        .or_else(|| {
            let e = lcc_camdb::find(Some(make), model_name)?;
            ordered_tags(e.illuminants, &e.color_matrix, &e.forward_matrix)
        })
        .unwrap_or(([fallback, fallback], None));
    let derived = [derived_fm(&cm[0], XY_A), derived_fm(&cm[1], D65)];
    let srgb_fm = lcc_color::bradford(D65, D50).mul(&SRGB.to_xyz());
    let fm_target = fm.unwrap_or([derived[0].unwrap_or(srgb_fm), derived[1].unwrap_or(srgb_fm)]);
    Prior { cm, fm, fm_target }
}

/// Stage 1: colour matrices placing each photo's white where Lightroom's temperature and tint put it.
fn fit_white(photos: &[&PhotoSamples], prior: &[Mat3; 2]) -> ([Mat3; 2], u32) {
    const LAMBDA: f64 = 0.01;
    let x0: Vec<f64> = prior.iter().flat_map(|m| m.0.iter().flatten().copied()).collect();
    let mut a = vec![vec![0.0; 18]; 18];
    let b: Vec<f64> = x0.iter().map(|v| LAMBDA * v).collect();
    for (i, row) in a.iter_mut().enumerate() {
        row[i] = LAMBDA;
    }
    let mut seen: Vec<usize> = Vec::new();
    for p in photos {
        if seen.contains(&p.photo) {
            continue;
        }
        let (Some((t, xy)), Some(n)) = (lr_white(p), neutral(p)) else { continue };
        seen.push(p.photo);
        let len = n.iter().map(|v| v * v).sum::<f64>().sqrt();
        let n = n.map(|v| v / len);
        let w = xy.to_xyz();
        let g = g_of_temp(t);
        let coef = |i: usize| {
            let mut c = [0.0; 18];
            for j in 0..3 {
                c[i * 3 + j] = g * w[j];
                c[9 + i * 3 + j] = (1.0 - g) * w[j];
            }
            c
        };
        for (p_, q) in [(1usize, 2usize), (2, 0), (0, 1)] {
            let (cq, cp) = (coef(q), coef(p_));
            let row: Vec<f64> = (0..18).map(|k| n[p_] * cq[k] - n[q] * cp[k]).collect();
            for r in 0..18 {
                for c in 0..18 {
                    a[r][c] += row[r] * row[c];
                }
            }
        }
    }
    if seen.is_empty() {
        return (*prior, 0);
    }
    let Some(x) = linalg::solve(a, b) else { return (*prior, 0) };
    let m = |o: usize| Mat3(std::array::from_fn(|i| std::array::from_fn(|j| x.get(o + i * 3 + j).copied().unwrap_or(0.0))));
    let cm = [m(0), m(9)];
    if cm.iter().all(matrix_ok) { (cm, seen.len() as u32) } else { (*prior, 0) }
}

/// One colour pair ready for fitting.
#[derive(Clone, Copy, Debug)]
struct Pair {
    /// Index into the photo list the pairs were made from.
    photo: usize,
    camera: [f32; 3],
    xyz: [f64; 3],
    lab: [f64; 3],
    w: f64,
    /// The sample's position in the developed frame (normalised).
    pos: [f32; 2],
}

/// The usable pairs of `photos`, at most `cap` (evenly strided), each with its photo's index.
fn pairs(photos: &[&PhotoSamples], cap: usize) -> Vec<Pair> {
    let total: usize = photos.iter().map(|p| p.samples.len()).sum();
    let stride = total.div_ceil(cap.max(1)).max(1);
    let mut out = Vec::new();
    let mut k = 0usize;
    for (i, p) in photos.iter().enumerate() {
        if neutral(p).is_none() {
            continue;
        }
        for s in &p.samples {
            k += 1;
            if !k.is_multiple_of(stride) {
                continue;
            }
            let xyz = s.lightroom_xyz.map(f64::from);
            let ok = s.camera.iter().all(|v| v.is_finite() && *v >= 0.0 && *v <= 4.0)
                && xyz.iter().all(|v| v.is_finite() && *v >= 0.0 && *v <= 2.0)
                && xyz[1] > 1e-4
                && s.weight.is_finite()
                && s.weight > 0.0;
            if ok {
                out.push(Pair { photo: i, camera: s.camera, xyz, lab: deltae::lab(xyz), w: f64::from(s.weight).min(1e3), pos: s.pos });
            }
        }
    }
    out
}

fn decodes(model: &Model, look: &LookModel, photos: &[&PhotoSamples]) -> Vec<Option<PhotoDecode>> {
    photos.iter().map(|p| neutral(p).map(|n| model.photo(look, baseline(p), n))).collect()
}

fn luminance(scene: [f64; 3]) -> f64 {
    let l = lcc_color::LUMA_2020;
    f64::from(l[0]) * scene[0] + f64::from(l[1]) * scene[1] + f64::from(l[2]) * scene[2]
}

/// ΔE00 of every pair through `model`, `look` and the pipeline `lut`.
fn errors(model: &Model, look: &LookModel, lut: &pipe::Lut, photos: &[&PhotoSamples], pairs: &[Pair]) -> Vec<f64> {
    let dec = decodes(model, look, photos);
    pairs
        .iter()
        .filter_map(|s| {
            let d = dec.get(s.photo)?.as_ref()?;
            Some(deltae::de2000(deltae::lab(lut.eval(d.scene(s.camera))), s.lab))
        })
        .collect()
}

/// Stage 2: a tone curve (DNG `ProfileToneCurve` points) taking scene luminance to Lightroom's, closed through the
/// pipeline on neutral greys. `pairs` are (scene Y, Lightroom Y, weight).
fn fit_tone(pairs: &[(f64, f64, f64)]) -> Result<Vec<[f32; 2]>> {
    const LO: f64 = -12.0;
    const STEP: f64 = 0.25;
    const BINS: usize = 60;
    let mut bins: Vec<Vec<(f64, f64)>> = vec![Vec::new(); BINS];
    for &(x, y, w) in pairs {
        if !(x > 0.0 && x.is_finite() && y.is_finite() && w > 0.0) {
            continue;
        }
        let i = ((x.log2() - LO) / STEP).floor();
        if i >= 0.0
            && let Some(b) = bins.get_mut(i as usize)
        {
            b.push((y, w));
        }
    }
    // (log2 scene, Lightroom Y) at each bin with enough samples: weighted median
    let mut known: Vec<(f64, f64)> = Vec::new();
    for (i, b) in bins.iter_mut().enumerate() {
        if b.len() < 4 {
            continue;
        }
        b.sort_by(|a, c| a.0.total_cmp(&c.0));
        let half = b.iter().map(|v| v.1).sum::<f64>() / 2.0;
        let mut acc = 0.0;
        let med = b.iter().find(|v| {
            acc += v.1;
            acc >= half
        });
        if let Some((y, _)) = med {
            known.push((LO + (i as f64 + 0.5) * STEP, *y));
        }
    }
    if known.len() < 3 {
        return Err(Error("too few colour samples spread over brightness to fit a tone curve: add photos with darker and brighter areas".into()));
    }
    // monotone
    let mut run = 0.0f64;
    for k in known.iter_mut() {
        run = run.max(k.1);
        k.1 = run;
    }
    let target = |x: f64| -> f64 {
        let l = x.max(1e-9).log2();
        let first = known.first().copied().unwrap_or((0.0, 0.0));
        let last = known.last().copied().unwrap_or((0.0, 0.0));
        let y = if l <= first.0 {
            first.1 * 2f64.powf(l - first.0)
        } else if l >= last.0 {
            (last.1 * 2f64.powf((l - last.0) * 0.5)).min(1.0)
        } else {
            let i = known.partition_point(|k| k.0 <= l).clamp(1, known.len() - 1);
            let (a, b) = (known.get(i - 1).copied().unwrap_or(first), known.get(i).copied().unwrap_or(last));
            let t = (l - a.0) / (b.0 - a.0).max(1e-9);
            a.1 + t * (b.1 - a.1)
        };
        y.clamp(0.0, 0.9995)
    };
    let xs: Vec<f64> = (0..32).map(|i| 2f64.powf(-12.0 + 12.0 * i as f64 / 31.0)).collect();
    let want: Vec<f64> = xs.iter().map(|x| target(*x)).collect();
    let mut ys = want.clone();
    let curve = |ys: &[f64]| -> Vec<[f32; 2]> {
        let mut pts = vec![[0.0f32, 0.0f32]];
        let mut prev = 0.0f32;
        for (x, y) in xs.iter().zip(ys) {
            let y = (*y as f32).clamp(prev, 0.9995);
            prev = y;
            pts.push([*x as f32, y]);
        }
        pts
    };
    for _ in 0..6 {
        let pts = curve(&ys);
        let tone = pipe::camera_tone(&ToneCurve { points: pts }).ok_or_else(|| Error("tone fit: invalid curve".into()))?;
        let got = pipe::gray_response(&xs, Some(&tone))?;
        for ((y, w), g) in ys.iter_mut().zip(&want).zip(&got) {
            if *g > 1e-6 && *w > 1e-6 {
                *y = (*y * (w / g).clamp(0.5, 2.0)).clamp(0.0, 0.9995);
            }
        }
    }
    Ok(curve(&ys))
}

fn tone_pairs(model: &Model, look: &LookModel, photos: &[&PhotoSamples], pairs: &[Pair]) -> Vec<(f64, f64, f64)> {
    let dec = decodes(model, look, photos);
    pairs.iter().filter_map(|s| Some((luminance(dec.get(s.photo)?.as_ref()?.scene(s.camera)), s.xyz[1], s.w))).collect()
}

fn lut_for(tone: &[[f32; 2]]) -> Result<pipe::Lut> {
    let t = pipe::camera_tone(&ToneCurve { points: tone.to_vec() }).ok_or_else(|| Error("invalid tone curve".into()))?;
    pipe::Lut::build(Some(&t))
}

fn d50_xyz() -> [f64; 3] {
    D50.to_xyz()
}

/// Rows of each forward matrix sum to the D50 white (FM maps the camera's neutral to D50).
fn project_fm(x: &mut [f64]) {
    let d = d50_xyz();
    for m in 0..2 {
        for i in 0..3 {
            let o = m * 9 + i * 3;
            let Some(row) = x.get_mut(o..o + 3) else { continue };
            let k = (d[i] - row.iter().sum::<f64>()) / 3.0;
            for v in row.iter_mut() {
                *v += k;
            }
        }
    }
}

fn fm_of(x: &[f64]) -> [Mat3; 2] {
    let m = |o: usize| Mat3(std::array::from_fn(|i| std::array::from_fn(|j| x.get(o + i * 3 + j).copied().unwrap_or(0.0))));
    [m(0), m(9)]
}

/// Stage 3: forward matrices by Levenberg–Marquardt on Lab error through the pipeline.
fn fit_fm(model: &Model, start: [Mat3; 2], target: [Mat3; 2], photos: &[&PhotoSamples], pairs: &[Pair], lut: &pipe::Lut) -> [Mat3; 2] {
    let flat = |m: &[Mat3; 2]| -> Vec<f64> { m.iter().flat_map(|m| m.0.iter().flatten().copied()).collect() };
    let x0 = flat(&start);
    let xt = flat(&target);
    let reg = 3.0 * (pairs.len() as f64 / 100.0).sqrt().max(0.3);
    let look = LookModel::default();
    let f = |x: &[f64]| -> Vec<f64> {
        let m = Model { fm: Some(fm_of(x)), ..model.clone() };
        let dec = decodes(&m, &look, photos);
        let mut r = Vec::with_capacity(pairs.len() * 3 + 18);
        for s in pairs {
            let lab = dec.get(s.photo).and_then(|d| d.as_ref()).map(|d| deltae::lab(lut.eval(d.scene(s.camera))));
            let sw = s.w.sqrt();
            match lab {
                Some(l) => r.extend((0..3).map(|i| (l[i] - s.lab[i]) * sw)),
                None => r.extend([0.0; 3]),
            }
        }
        r.extend(x.iter().zip(&xt).map(|(a, b)| reg * (a - b)));
        r
    };
    let x = linalg::levenberg_marquardt(x0, 12, 1e-4, f, project_fm);
    let fm = fm_of(&x);
    if fm.iter().all(matrix_ok) { fm } else { start }
}

/// Which table a round fits.
#[derive(Clone, Copy, PartialEq)]
enum Which {
    HueSat,
    Look,
}

/// Stages 4 and 5: one refinement round of the hue/saturation map (`Which::HueSat`, dual by white) or of a look table.
fn table_round(model: &mut Model, look: &mut LookModel, which: Which, photos: &[&PhotoSamples], pairs: &[Pair], lut: &pipe::Lut) {
    let dec = decodes(model, look, photos);
    let (mut shared, mut a, mut d) = (table::Splat::default(), table::Splat::default(), table::Splat::default());
    for s in pairs {
        let Some(Some(pd)) = dec.get(s.photo) else { continue };
        let st = pd.stages(s.camera);
        let Some(target) = lut.invert(s.xyz, st.scene) else { continue };
        let tp = pd.to_prophoto(target);
        match which {
            Which::HueSat => {
                let gain = pd.gain().max(1e-6);
                let out = st.look_in.map(|v| v / gain);
                let tp = tp.map(|v| v / gain);
                shared.add(st.hsm_in, out, tp, s.w);
                a.add(st.hsm_in, out, tp, s.w * pd.g);
                d.add(st.hsm_in, out, tp, s.w * (1.0 - pd.g));
            }
            Which::Look => shared.add(st.look_in, pd.to_prophoto(st.scene), tp, s.w),
        }
    }
    const SMOOTH: f64 = 0.5;
    let cs = shared.solve(SMOOTH);
    match which {
        Which::HueSat => {
            let total = shared.total().max(1e-9);
            let blend = |own: &table::Splat| -> Vec<[f64; 3]> {
                let c = own.solve(SMOOTH);
                let k = own.total() / (own.total() + 0.3 * total);
                cs.iter().zip(&c).map(|(s, o)| std::array::from_fn(|i| s[i] + (o[i] - s[i]) * k)).collect()
            };
            let (ca, cd) = (blend(&a), blend(&d));
            let [ta, td] = model.hsm.clone().unwrap_or([table::identity(), table::identity()]);
            model.hsm = Some([table::compose(&ta, &ca, 1.0), table::compose(&td, &cd, 1.0)]);
        }
        Which::Look => {
            let t = look.table.clone().unwrap_or_else(table::identity);
            look.table = Some(table::compose(&t, &cs, 1.0));
        }
    }
}

/// One look's fitted parts.
#[derive(Clone)]
struct FittedLook {
    name: String,
    look: LookModel,
    tone: Vec<[f32; 2]>,
    lut: pipe::Lut,
}

/// Fit the calibration from the collected photos (all of one camera model). The result passes
/// `Calibration::validate` and carries held-out ΔE00 before/after per look in `evidence`.
/// A look's name for `editor`: Lightroom's profile names as they are; another editor's look (an export folder's
/// name) led by the editor's name, so "Standard" from Capture One becomes "Capture One Standard" and never takes a
/// Lightroom look's id.
pub fn look_name_for(editor: Editor, name: &str) -> String {
    let name = name.trim();
    if editor == Editor::Lightroom || name.to_lowercase().starts_with(&editor.name().to_lowercase()) {
        return name.to_string();
    }
    if name.is_empty() { editor.name().to_string() } else { format!("{} {name}", editor.name()) }
}

pub fn fit(photos: &[PhotoSamples], opts: &FitOptions) -> Result<Calibration> {
    let renamed: Vec<PhotoSamples>;
    let photos = if opts.editor == Editor::Lightroom {
        photos
    } else {
        renamed = photos
            .iter()
            .cloned()
            .map(|mut p| {
                p.white.lightroom_temp_tint = None;
                p.look = look_name_for(opts.editor, &p.look);
                p
            })
            .collect();
        &renamed
    };
    let first = photos.first().ok_or_else(|| Error("no photos to fit: collect pairs of raws and exports first".into()))?;
    let (make, model_name) = (first.make.trim().to_string(), first.model.trim().to_string());
    if photos.iter().any(|p| p.make.trim() != make || p.model.trim() != model_name) {
        return Err(Error("the photos come from more than one camera model: fit one model at a time".into()));
    }
    let rounds = if opts.rounds == 0 { 3 } else { opts.rounds.min(8) };
    // looks: the base is Adobe Standard when there, else the most common
    let mut names: Vec<(String, usize)> = Vec::new();
    for p in photos {
        let n = p.look.trim().to_string();
        match names.iter_mut().find(|(m, _)| *m == n) {
            Some(e) => e.1 += 1,
            None => names.push((n, 1)),
        }
    }
    if names.iter().any(|(n, _)| lcc_camdb::calibration::look_id(n) == lcc_camdb::calibration::LOOK_PREFIX) {
        return Err(Error("an export has no usable profile name (crs:CameraProfile)".into()));
    }
    let base = names
        .iter()
        .find(|(n, _)| n == "Adobe Standard")
        .or_else(|| names.iter().max_by_key(|(_, c)| *c))
        .map(|(n, _)| n.clone())
        .ok_or_else(|| Error("no looks".into()))?;
    let train: Vec<&PhotoSamples> = photos.iter().filter(|p| !p.held_out).collect();
    let base_train: Vec<&PhotoSamples> = train.iter().copied().filter(|p| p.look.trim() == base).collect();
    if base_train.is_empty() {
        return Err(Error(format!("no training photos for the base look {base:?}: hold out fewer photos")));
    }

    // 1. white points
    let prior = prior(&make, &model_name, train.iter().find_map(|p| p.dng.as_ref()));
    let (cm, white_points) = fit_white(&train, &prior.cm);
    let mut model = Model { cm, fm: Some(prior.fm_target), hsm: None };

    // 2–3. tone and forward matrices, alternated once
    let base_pairs = pairs(&base_train, LM_SAMPLES);
    if base_pairs.len() < 50 {
        return Err(Error(format!("only {} usable colour samples for {base:?}: add more photos", base_pairs.len())));
    }
    let no_look = LookModel::default();
    let mut tone = fit_tone(&tone_pairs(&model, &no_look, &base_train, &base_pairs))?;
    let mut lut = lut_for(&tone)?;
    for _ in 0..2 {
        let fm = fit_fm(&model, model.fm.unwrap_or(prior.fm_target), prior.fm_target, &base_train, &base_pairs, &lut);
        model.fm = Some(fm);
        tone = fit_tone(&tone_pairs(&model, &no_look, &base_train, &base_pairs))?;
        lut = lut_for(&tone)?;
    }

    // 4. hue/saturation map
    let table_pairs = pairs(&base_train, TABLE_SAMPLES);
    let mut base_look = LookModel::default();
    for _ in 0..rounds {
        table_round(&mut model, &mut base_look, Which::HueSat, &base_train, &table_pairs, &lut);
    }
    if model.hsm.as_ref().is_some_and(|[a, b]| table::is_identity(a) && table::is_identity(b)) {
        model.hsm = None;
    }
    let mut looks = vec![FittedLook { name: base.clone(), look: base_look, tone, lut }];

    // 5. the other looks
    for (name, _) in names.iter().filter(|(n, _)| *n != base) {
        let set: Vec<&PhotoSamples> = train.iter().copied().filter(|p| p.look.trim() == name).collect();
        let lp = pairs(&set, TABLE_SAMPLES);
        if lp.len() < 50 {
            continue;
        }
        let tone = fit_tone(&tone_pairs(&model, &no_look, &set, &lp))?;
        let lut = lut_for(&tone)?;
        let mut look = LookModel::default();
        for _ in 0..rounds {
            table_round(&mut model.clone(), &mut look, Which::Look, &set, &lp, &lut);
        }
        if look.table.as_ref().is_some_and(table::is_identity) {
            look.table = None;
        }
        looks.push(FittedLook { name: name.clone(), look, tone, lut });
    }
    // default order: Adobe Color first when fitted, else the base
    if let Some(i) = looks.iter().position(|l| l.name == "Adobe Color") {
        let l = looks.remove(i);
        looks.insert(0, l);
    }

    // 6. closed loop through the real decode
    let prior_fm = prior.fm_target;
    let who = Who { make: &make, model: &model_name, contributor: opts.contributor.as_deref(), editor: opts.editor };
    closed_loop(&mut model, &mut looks, &train, &base, &who, prior_fm);

    // 7. evidence and gates
    let candidate = assemble(&who, &model, &looks, prior_fm, Evidence::default());
    let before_model = Model { cm: prior.cm, fm: prior.fm, hsm: None };
    let before_lut = pipe::Lut::build(None)?;
    let mut rows = Vec::new();
    for l in &looks {
        let of_look = |held: bool| -> Vec<&PhotoSamples> { photos.iter().filter(|p| p.held_out == held && p.look.trim() == l.name).collect() };
        let (held, trained) = (of_look(true), of_look(false));
        let set = if held.is_empty() { &trained } else { &held };
        let ps = pairs(set, usize::MAX);
        let real = candidate
            .looks
            .iter()
            .find(|c| c.name == l.name)
            .ok_or_else(|| Error("look missing from the candidate".into()))
            .and_then(|cl| Ok((real_outputs(set, &ps, None)?, real_outputs(set, &ps, Some(candidate.look_dcp(cl)))?)));
        let (mut before, mut after) = (Vec::new(), Vec::new());
        match real {
            Ok((rb, ra)) => {
                for ((s, b), a) in ps.iter().zip(&rb).zip(&ra) {
                    if let (Some(b), Some(a)) = (b, a) {
                        before.push(deltae::de2000(deltae::lab(*b), s.lab));
                        after.push(deltae::de2000(deltae::lab(*a), s.lab));
                    }
                }
            }
            Err(e) => eprintln!("note: {}: ΔE00 from the analytic model, a raw could not be rendered ({e})", l.name),
        }
        // the analytic model: the prior matrices without any per-photo look (what a file with its own matrices and
        // no JPEG fit starts from) against the candidate
        let plain = (
            deltae::summary(&errors(&before_model, &LookModel::default(), &before_lut, set, &ps)),
            deltae::summary(&errors(&model, &l.look, &l.lut, set, &ps)),
        );
        let (b, a) = if before.is_empty() { plain } else { (deltae::summary(&before), deltae::summary(&after)) };
        let evidence = LookEvidence { name: l.name.clone(), photos: count_photos(&trained), held_out: count_photos(&held), before: b, after: a };
        rows.push((evidence, plain, !held.is_empty()));
    }
    let gated = rows.iter().filter(|r| r.2).count();
    // the base look decides the calibration's reach: beating the app's current colour serves every file; losing to
    // it (on raws that is the look fitted to each one's camera JPEG) but beating the plain colour serves only files
    // with matrices of their own, which get no such fit (issue #1119); losing to both is refused
    let mut own_matrix_only = false;
    if let Some((e, plain, true)) = rows.iter().find(|r| r.0.name == base) {
        match gate_verdict((e.before, e.after), *plain) {
            Verdict::Beats => {}
            Verdict::OwnMatrixOnly if opts.editor == Editor::Lightroom => {
                own_matrix_only = true;
                eprintln!(
                    "note: {}: on the held-out photos the fitted colour (ΔE00 median {:.2}, p95 {:.2}) does not beat the per-photo colour \
                     these raws get from their camera JPEG (median {:.2}, p95 {:.2}), but beats the plain colour (median {:.2} vs {:.2}): \
                     kept as the default only for files with colour matrices of their own (converted DNGs)",
                    e.name, e.after.median, e.after.p95, e.before.median, e.before.p95, plain.1.median, plain.0.median
                );
            }
            _ => {
                return Err(Error(format!(
                    "{}: on the held-out photos the fitted colour (ΔE00 median {:.2}, p95 {:.2}) does not beat the app's current colour \
                     (median {:.2}, p95 {:.2}). Add more photos with varied colours and light (daylight, shade and tungsten), and check \
                     the exports used the baseline preset",
                    e.name, e.after.median, e.after.p95, e.before.median, e.before.p95
                )));
            }
        }
    }
    let mut look_evidence = Vec::new();
    let mut dropped: Vec<String> = Vec::new();
    for (e, plain, held) in rows {
        if held && e.name != base {
            let verdict = gate_verdict((e.before, e.after), plain);
            let kept = verdict == Verdict::Beats || (own_matrix_only && verdict == Verdict::OwnMatrixOnly);
            if !kept {
                // another look that a fixed look can't match (an adaptive profile changes with each photo) is left out
                eprintln!(
                    "note: look {:?} left out: on the held-out photos it does not beat the app's current colour \
                     (ΔE00 median {:.2} vs {:.2}, p95 {:.2} vs {:.2}); adaptive profiles change with each photo and can't be matched",
                    e.name, e.after.median, e.before.median, e.after.p95, e.before.p95
                );
                dropped.push(e.name.clone());
                continue;
            }
        }
        look_evidence.push(e);
    }
    looks.retain(|l| !dropped.contains(&l.name));
    if gated == 0 {
        return Err(Error("no held-out photos to check the fit on: collect with a hold-out share above 0, or add photos".into()));
    }

    let evidence = Evidence {
        photos: count_photos(&train),
        samples: train.iter().map(|p| p.samples.len() as u64).sum(),
        cct: cct_bins(&train, &model),
        hue_coverage: hue_coverage(&train),
        white_points,
        looks: look_evidence,
    };
    let mut cal = assemble(&who, &model, &looks, prior_fm, evidence);
    cal.own_matrix_only = own_matrix_only;
    cal.validate().map_err(|e| Error(format!("the fitted calibration is not valid: {e}")))?;
    Ok(cal)
}

/// What a look's held-out gate says ([`gate_verdict`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    /// It beats the app's current colour (median and p95): it serves every file.
    Beats,
    /// It loses to the current colour but beats the plain colour (the prior matrices without a per-photo look): it
    /// serves only files with matrices of their own, which get no per-photo JPEG fit (issue #1119).
    OwnMatrixOnly,
    /// It beats neither.
    Fails,
}

/// The held-out gate on `(before, after)` ΔE00 against the app's current colour (`current`) and against the plain
/// colour (`plain`, analytic): after must be below before at both the median and p95.
fn gate_verdict(current: (DeltaE, DeltaE), plain: (DeltaE, DeltaE)) -> Verdict {
    let beats = |(b, a): &(DeltaE, DeltaE)| a.median < b.median && a.p95 < b.p95;
    if beats(&current) {
        Verdict::Beats
    } else if beats(&plain) {
        Verdict::OwnMatrixOnly
    } else {
        Verdict::Fails
    }
}

/// Who the calibration is for and by.
struct Who<'a> {
    make: &'a str,
    model: &'a str,
    contributor: Option<&'a str>,
    editor: Editor,
}

/// The calibration file of a fitted model and looks.
fn assemble(who: &Who, model: &Model, looks: &[FittedLook], prior_fm: [Mat3; 2], evidence: Evidence) -> Calibration {
    Calibration {
        version: VERSION,
        make: who.make.to_string(),
        model: who.model.to_string(),
        aliases: Vec::new(),
        lens: None,
        editor: who.editor,
        own_matrix_only: false,
        method: who.editor.method(),
        tool_version: env!("CARGO_PKG_VERSION").into(),
        contributor: who.contributor.map(|c| c.trim().to_string()).filter(|c| !c.is_empty()),
        licence: LICENCE.into(),
        camera: CameraModel {
            illuminants: [17, 21],
            color_matrix: model.cm,
            forward_matrix: model.fm.unwrap_or(prior_fm),
            hue_sat_map: model.hsm.clone(),
        },
        looks: looks
            .iter()
            .map(|l| Look {
                name: l.name.clone(),
                baseline_exposure_offset: l.look.offset,
                look_table: l.look.table.clone(),
                tone_curve: l.tone.clone(),
            })
            .collect(),
        evidence,
    }
}

/// Each pair's colour in a real render of its raw: with the app's own colour (`candidate` `None`) or with `candidate`
/// installed as an imported profile. `None` where the position falls off the render.
fn real_outputs(set: &[&PhotoSamples], pairs: &[Pair], candidate: Option<lcc_raw::dcp::Dcp>) -> Result<Vec<Option<[f64; 3]>>> {
    let mut positions: Vec<Vec<[f32; 2]>> = vec![Vec::new(); set.len()];
    let mut slot: Vec<Option<(usize, usize)>> = Vec::with_capacity(pairs.len());
    for s in pairs {
        slot.push(positions.get_mut(s.photo).map(|v| {
            v.push(s.pos);
            (s.photo, v.len() - 1)
        }));
    }
    let mut job_of: Vec<Option<usize>> = vec![None; set.len()];
    let mut jobs: Vec<(&std::path::Path, Vec<[f32; 2]>)> = Vec::new();
    for (i, (p, pos)) in set.iter().zip(positions).enumerate() {
        if !pos.is_empty()
            && let Some(j) = job_of.get_mut(i)
        {
            *j = Some(jobs.len());
            jobs.push((p.raw_path.as_path(), pos));
        }
    }
    let got = match candidate {
        Some(d) => real::with_candidate(d, || real::read_back(&jobs, real::CANDIDATE))?,
        None => real::read_back(&jobs, "lc")?,
    };
    Ok(slot
        .iter()
        .map(|s| {
            let (photo, k) = (*s)?;
            let j = (*job_of.get(photo)?)?;
            got.get(j)?.get(k).copied().flatten()
        })
        .collect())
}

/// Step 6: render the training raws with the candidate, fold the gap between the real render and the analytic model
/// into the targets and refit tone curves and tables; a round that makes the real training error worse is undone.
/// A raw that can't be rendered stops the loop (the analytic fit stands, with a note).
fn closed_loop(model: &mut Model, looks: &mut [FittedLook], train: &[&PhotoSamples], base: &str, who: &Who, prior_fm: [Mat3; 2]) {
    let no_look = LookModel::default();
    let mut best: Option<(f64, Model, Vec<FittedLook>)> = None;
    for round in 0..=LOOP_ROUNDS {
        let cal = assemble(who, model, looks, prior_fm, Evidence::default());
        let mut errs = Vec::new();
        let mut refits: Vec<(usize, Vec<&PhotoSamples>, Vec<Pair>)> = Vec::new();
        for (li, l) in looks.iter().enumerate() {
            let set: Vec<&PhotoSamples> = train.iter().copied().filter(|p| p.look.trim() == l.name).collect();
            let lp = pairs(&set, TABLE_SAMPLES);
            let Some(cl) = cal.looks.iter().find(|c| c.name == l.name) else { continue };
            let real = match real_outputs(&set, &lp, Some(cal.look_dcp(cl))) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("note: closed loop through the real decode skipped, a raw could not be rendered ({e}); the analytic fit stands");
                    if let Some((_, m, l)) = best {
                        *model = m;
                        looks.clone_from_slice(&l);
                    }
                    return;
                }
            };
            let dec = decodes(model, &l.look, &set);
            let mut adj = Vec::with_capacity(lp.len());
            for (s, r) in lp.iter().zip(&real) {
                let (Some(r), Some(Some(d))) = (r, dec.get(s.photo)) else { continue };
                errs.push(deltae::de2000(deltae::lab(*r), s.lab));
                let pred = l.lut.eval(d.scene(s.camera));
                let xyz: [f64; 3] = std::array::from_fn(|i| (s.xyz[i] + pred[i] - r[i]).clamp(0.0, 2.0));
                if xyz.iter().all(|v| v.is_finite()) && xyz[1] > 1e-4 {
                    adj.push(Pair { xyz, lab: deltae::lab(xyz), ..*s });
                }
            }
            refits.push((li, set, adj));
        }
        let median = deltae::median(&errs);
        if errs.is_empty() || !median.is_finite() {
            return;
        }
        eprintln!("closed loop round {round}: real-render training ΔE00 median {median:.2}");
        if let Some((b, m, l)) = &best
            && median >= *b
        {
            *model = m.clone();
            looks.clone_from_slice(l);
            return;
        }
        best = Some((median, model.clone(), looks.to_vec()));
        if round == LOOP_ROUNDS {
            return;
        }
        // the base look first: its hue/saturation map is shared by the others
        refits.sort_by_key(|(li, _, _)| looks.get(*li).is_none_or(|l| l.name != base));
        for (li, set, adj) in refits {
            if adj.len() < 50 {
                continue;
            }
            let Some(l) = looks.get_mut(li) else { continue };
            let Ok(tone) = fit_tone(&tone_pairs(model, &no_look, &set, &adj)) else { continue };
            let Ok(lut) = lut_for(&tone) else { continue };
            l.tone = tone;
            l.lut = lut;
            let FittedLook { look, lut, name, .. } = l;
            if name == base {
                table_round(model, look, Which::HueSat, &set, &adj, lut);
            } else {
                table_round(&mut model.clone(), look, Which::Look, &set, &adj, lut);
            }
        }
    }
}

fn count_photos(set: &[&PhotoSamples]) -> u32 {
    let mut ids: Vec<usize> = set.iter().map(|p| p.photo).collect();
    ids.sort_unstable();
    ids.dedup();
    ids.len() as u32
}

/// Photos per 1000 K of white balance (Lightroom's, else the fitted model's reading of the camera neutral).
fn cct_bins(set: &[&PhotoSamples], model: &Model) -> Vec<CctBin> {
    let mut seen: Vec<usize> = Vec::new();
    let mut bins: Vec<CctBin> = Vec::new();
    for p in set {
        if seen.contains(&p.photo) {
            continue;
        }
        seen.push(p.photo);
        let t = lr_white(p).map(|(t, _)| t).or_else(|| {
            let n = neutral(p)?;
            let white = model.photo(&LookModel::default(), 0.0, n).white;
            Some(lcc_color::cct::xy_to_temp_tint(white).0)
        });
        let Some(t) = t.filter(|t| t.is_finite() && *t > 0.0) else { continue };
        let from_k = ((t / 1000.0).floor() * 1000.0).clamp(0.0, 50_000.0) as u32;
        match bins.iter_mut().find(|b| b.from_k == from_k) {
            Some(b) => b.photos += 1,
            None => bins.push(CctBin { from_k, photos: 1 }),
        }
    }
    bins.sort_by_key(|b| b.from_k);
    bins.truncate(64);
    bins
}

/// Share of 36 hue sectors with at least a few saturated Lightroom colours.
fn hue_coverage(set: &[&PhotoSamples]) -> f32 {
    let mut counts = [0u32; 36];
    for p in set {
        for s in &p.samples {
            let xyz = s.lightroom_xyz.map(f64::from);
            if !xyz.iter().all(|v| v.is_finite()) {
                continue;
            }
            let [_, a, b] = deltae::lab(xyz);
            if a.hypot(b) < 15.0 {
                continue;
            }
            let h = b.atan2(a).to_degrees().rem_euclid(360.0);
            if let Some(c) = counts.get_mut(((h / 10.0) as usize).min(35)) {
                *c += 1;
            }
        }
    }
    counts.iter().filter(|c| **c >= 5).count() as f32 / 36.0
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod editor_tests {
    use super::*;

    #[test]
    fn look_names_follow_the_editor() {
        assert_eq!(look_name_for(Editor::Lightroom, "Adobe Color"), "Adobe Color");
        assert_eq!(look_name_for(Editor::CaptureOne, "Standard"), "Capture One Standard");
        assert_eq!(look_name_for(Editor::CaptureOne, "capture one film standard"), "capture one film standard");
        assert_eq!(look_name_for(Editor::Darktable, "  "), "darktable");
        assert_ne!(
            lcc_camdb::calibration::look_id(&look_name_for(Editor::Darktable, "Adobe Color")),
            lcc_camdb::calibration::look_id("Adobe Color"),
            "another editor's look never takes a Lightroom look's id"
        );
    }
}
