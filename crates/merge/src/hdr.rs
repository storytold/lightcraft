//! HDR merge of exposure brackets.
//!
//! 1. **Order and reference.** Frames are ordered by brightness (EXIF exposure, or median
//!    luminance without EXIF; ties by content); the middle one is the reference (geometry, colour
//!    and the output's default brightness come from it). Everything below runs in that order, so
//!    the result is the same whatever order the frames come in.
//! 2. **Auto align** ([`crate::align`]): each frame is registered to the reference on small
//!    versions, the fit refined at full size ([`crate::refine`]), and the frame resampled into the
//!    reference's geometry (Lanczos-3, so the warp keeps fine detail).
//! 3. **Exposure normalisation.** The exposure ratio between brightness-adjacent frames is
//!    measured robustly — the median of log ratios over pixels well exposed in both — and chained
//!    to the reference; EXIF is the fallback when too few pixels overlap. Radiance
//!    `Rᵢ = vᵢ / kᵢ` is expressed in reference units.
//! 4. **Weights.** `wᵢ = kᵢ · (1 − smoothstep(0.80·clip, 0.96·clip, max channel within 1 px))`:
//!    longer exposures get more weight (shot-noise optimal for a linear sensor), saturated pixels
//!    none (all channels together, so a clipped channel never shifts colour), nor their neighbours,
//!    which demosaicing interpolated from clipped photosites. The darkest frame keeps a tiny weight
//!    so fully saturated pixels still get a value. Where the reference is at or near its clip, the scene is
//!    at least that bright: a frame reading far darker there saw something that moved (a star, the moon, a light) and is
//!    left out, so a clipped highlight of the reference is never replaced by content from another moment.
//! 5. **Deghost** (None/Low/Medium/High): on a ≤ 2048 px version, a per-pixel consistency test
//!    against the reference: the reference radiance (or, where the reference is clipped or black,
//!    the nearest well-exposed frame's) predicts what each frame should have recorded; frames that
//!    disagree by more than a threshold (in log units, with a noise floor) are masked there. Masks
//!    are dilated and feathered (Gaussian), upsampled, and remove the frame's weight. Their union
//!    is the deghost overlay.
//! 6. **Reference detail.** Where the reference is well exposed, the merge's finest detail (below
//!    [`DETAIL_SIGMA`]) is the reference's own: it is the one frame neither resampled nor aligned
//!    to only a fraction of a pixel ([`reference_detail`]).

use lightcraft_geom::Homography;
use lightcraft_raster::blur::gaussian;
use lightcraft_raster::{Plane, Rgb32f};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::align::{self, Alignment};
use crate::frame::{Frame, FrameColor, consensus_multipliers};
use crate::{MergeError, Progress, Result, check};
use crate::{flow, refine};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Deghost {
    #[default]
    None,
    Low,
    Medium,
    High,
}

impl Deghost {
    /// (log-ratio threshold, dilation/feather radius in deghost-map pixels).
    fn params(self) -> Option<(f32, usize)> {
        match self {
            Deghost::None => None,
            Deghost::Low => Some((1.0, 2)),
            Deghost::Medium => Some((0.6, 3)),
            Deghost::High => Some((0.35, 5)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct HdrOptions {
    pub align: bool,
    pub deghost: Deghost,
}

impl Default for HdrOptions {
    fn default() -> Self {
        HdrOptions { align: true, deghost: Deghost::None }
    }
}

/// The merged result, in the reference frame's geometry and units.
#[derive(Clone, Debug)]
pub struct HdrResult {
    /// Scene-linear radiance; 1.0 = the reference frame's white level.
    pub radiance: Rgb32f,
    /// Deghost overlay (0..1, the union of the masks) at the radiance's size; empty without deghosting.
    pub ghost: Plane,
    /// Index (into the inputs) of the reference frame.
    pub reference: usize,
    /// Exposure of each input relative to the reference, in EV (as used for the merge).
    pub ev: Vec<f64>,
    /// The same from EXIF, when every frame had it.
    pub ev_exif: Option<Vec<f64>>,
    pub alignments: Vec<Alignment>,
    /// Per input: the largest local shift (pixels) left after its alignment and corrected tile by tile
    /// ([`crate::flow`]); 0 for the reference and frames not aligned.
    pub local_shift: Vec<f32>,
    pub color: FrameColor,
    pub raw: bool,
    pub orientation: lightcraft_geom::Orientation,
    pub metadata: lightcraft_meta::Metadata,
    pub baseline_exposure: f64,
    /// The reference frame's lens corrections, for `radiance` ([`Frame::lens`]).
    pub lens: Vec<lightcraft_raw::Opcode>,
    /// The inputs' as-shot white-balance multipliers they agree on ([`crate::frame::consensus_multipliers`]), already in
    /// `color`; kept so merged brackets stitched into a panorama can vote as frames of their own.
    pub wb_multipliers: Option<[f32; 3]>,
}

/// Long edge (at most) of the frames the deghost test compares: small moving things (a star at 200 mm) still span a few
/// pixels there.
const DEGHOST_EDGE: usize = 2048;

#[inline]
fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[inline]
fn maxc(p: [f32; 3]) -> f32 {
    p[0].max(p[1]).max(p[2])
}

/// Merge exposure brackets. Frames must share their pixel size (and orientation).
pub fn merge_hdr(mut frames: Vec<Frame>, opts: &HdrOptions, progress: &Progress) -> Result<HdrResult> {
    let n = frames.len();
    if n < 2 {
        return Err(MergeError::TooFew(n, 2));
    }
    let (w, h) = (frames[0].width(), frames[0].height());
    if frames.iter().any(|f| f.width() != w || f.height() != h) {
        return Err(MergeError::Mismatch("HDR merge needs photos of the same size".into()));
    }
    if frames.iter().any(|f| f.orientation != frames[0].orientation) {
        for f in frames.iter_mut() {
            f.orient();
        }
        let (w0, h0) = (frames[0].width(), frames[0].height());
        if frames.iter().any(|f| f.width() != w0 || f.height() != h0) {
            return Err(MergeError::Mismatch("HDR merge needs photos of the same size".into()));
        }
    }
    let (w, h) = (frames[0].width(), frames[0].height());
    check(progress, 0.02, "Analysing exposures")?;

    // 1. brightness order and reference
    let exif: Option<Vec<f64>> = frames.iter().map(|f| f.exposure).collect();
    let medians: Vec<f32> = frames.par_iter().map(|f| align::median_luma(&f.image, f.clip)).collect();
    let brightness: Vec<f64> = match &exif {
        Some(e) if e.iter().any(|v| (v - e[0]).abs() > 0.1) => e.clone(),
        _ => medians.iter().map(|m| (*m as f64).log2()).collect(),
    };
    // ties (equal brightness) by content, never by position
    let fingerprint: Vec<f64> =
        frames.par_iter().map(|f| f.image.data.iter().step_by(997).map(|p| p[0] as f64 + 2.0 * p[1] as f64 + 3.0 * p[2] as f64).sum()).collect();
    let mut input: Vec<usize> = (0..n).collect();
    input.sort_by(|&a, &b| {
        brightness[a].total_cmp(&brightness[b]).then(medians[a].total_cmp(&medians[b])).then(fingerprint[a].total_cmp(&fingerprint[b]))
    });
    // From here on frames are in brightness order (`input[p]` is the caller's index of position `p`): alignment seeds,
    // sums and deghost masks no longer depend on the order the caller passed. Per-frame results go back at the end.
    let mut slots: Vec<Option<Frame>> = frames.into_iter().map(Some).collect();
    let mut frames: Vec<Frame> = input.iter().filter_map(|&i| slots.get_mut(i).and_then(Option::take)).collect();
    if frames.len() != n {
        return Err(MergeError::Mismatch("HDR merge lost a frame while ordering them".into()));
    }
    let exif: Option<Vec<f64>> = exif.map(|e| input.iter().map(|&i| e[i]).collect());
    let medians: Vec<f32> = input.iter().map(|&i| medians[i]).collect();
    let brightness: Vec<f64> = input.iter().map(|&i| brightness[i]).collect();
    let order: Vec<usize> = (0..n).collect();
    let reference = n / 2;

    // 2. alignment
    let mut alignments = vec![Alignment::identity(); n];
    let mut local_shift = vec![0f32; n];
    let mut valid: Vec<Option<Vec<bool>>> = vec![None; n];
    if opts.align {
        check(progress, 0.05, "Aligning")?;
        let g0 = 0.18 / medians[reference].max(1e-6);
        let rel: Vec<f32> = (0..n).map(|i| 2f64.powf(brightness[reference] - brightness[i]) as f32).collect();
        let rel: Vec<f32> = if exif.is_some() && brightness.iter().any(|b| (b - brightness[0]).abs() > 0.1) {
            rel
        } else {
            (0..n).map(|i| medians[reference] / medians[i].max(1e-6)).collect()
        };
        let feats: Vec<(crate::features::Features, f64)> = frames
            .par_iter()
            .enumerate()
            .map(|(i, f)| {
                let (p, s) = align::work_image(&f.image, g0 * rel[i], align::WORK_EDGE);
                (crate::features::detect(&p, 1500), s)
            })
            .collect();
        check(progress, 0.25, "Aligning")?;
        let al: Vec<(usize, Alignment)> = (0..n)
            .into_par_iter()
            .filter(|&i| i != reference)
            .map(|i| (i, align::align_pair(&feats[reference].0, &feats[i].0, feats[i].1, w, h, i as u64 + 1)))
            .collect();
        for (i, a) in al {
            alignments[i] = a;
        }
        check(progress, 0.3, "Refining the alignment")?;
        // the fit on small versions refined at full size (a tenth of a work pixel is most of a full one), then the
        // frame resampled; one frame at a time (both are parallel inside): each warped copy replaces its frame
        // before the next is made
        let ref_luma = refine::luma(&frames[reference].image, g0 * rel[reference], frames[reference].clip);
        for i in (0..n).filter(|&i| i != reference) {
            check(progress, 0.3 + 0.15 * i as f32 / n as f32, "Refining the alignment")?;
            let luma = refine::luma(&frames[i].image, g0 * rel[i], frames[i].clip);
            // the small versions' fit refined at full size; where they found nothing (a bright frame mostly clipped),
            // patches at full size still may, searching wider
            let (start, radius) = if alignments[i].model == "identity" {
                (Homography::IDENTITY, refine::RECOVER_RADIUS)
            } else {
                (alignments[i].h, refine::radius_for(feats[i].1))
            };
            if let Some(a) = refine::fit(&ref_luma, &luma, &start, radius, i as u64 + 1, &refine::ALL_MODELS) {
                alignments[i] = a;
            }
            if alignments[i].model == "identity" {
                continue;
            }
            check(progress, 0.3 + 0.15 * i as f32 / n as f32, "Resampling")?;
            // what the one model leaves (parallax, distortion): shifts measured tile by tile
            let fl = flow::estimate(&ref_luma, &luma, &alignments[i].h, flow::RADIUS);
            drop(luma);
            local_shift[i] = fl.max();
            let (img, v) = flow::warp(&frames[i].image, &alignments[i].h, &fl);
            frames[i].image = img;
            valid[i] = Some(v);
        }
    }
    check(progress, 0.45, "Normalising exposures")?;

    // 3. exposure ratios between brightness neighbours, chained to the reference (ln units)
    let mut step_ln = vec![0f64; n.saturating_sub(1)];
    for p in 0..n - 1 {
        let (a, b) = (order[p], order[p + 1]);
        let est = log_ratio(&frames[a], valid[a].as_deref(), &frames[b], valid[b].as_deref());
        step_ln[p] = match (est, &exif) {
            (Some(r), _) => r,
            (None, Some(e)) => (e[b] - e[a]) * std::f64::consts::LN_2,
            (None, None) => ((medians[b] / medians[a].max(1e-6)) as f64).max(1e-6).ln(),
        };
    }
    let mut ln_k = vec![0f64; n];
    let rpos = n / 2;
    for p in (0..rpos).rev() {
        ln_k[order[p]] = ln_k[order[p + 1]] - step_ln[p];
    }
    for p in rpos + 1..n {
        ln_k[order[p]] = ln_k[order[p - 1]] + step_ln[p - 1];
    }
    let k: Vec<f32> = ln_k.iter().map(|v| v.exp() as f32).collect();
    let ev: Vec<f64> = ln_k.iter().map(|v| v / std::f64::consts::LN_2).collect();
    let ev_exif = exif.as_ref().map(|e| e.iter().map(|v| v - e[reference]).collect());

    // 4./5. deghost maps (small scale)
    check(progress, 0.55, "Deghosting")?;
    let ghost_small: Option<(Vec<Plane>, usize)> = opts.deghost.params().map(|(thr, r)| deghost_maps(&frames, &valid, &k, &order, reference, thr, r));
    check(progress, 0.7, "Merging")?;

    let darkest = order[0];
    let clips: Vec<f32> = frames.iter().map(|f| f.clip).collect();
    // how close each pixel of each frame is to saturation: its brightest channel within 1 px (see `saturation`)
    let sat: Vec<Vec<f32>> = frames.par_iter().map(|f| saturation(&f.image)).collect();
    let mut out = Rgb32f::new(w, h);
    let fr = &frames;
    let vd = &valid;
    let gs = ghost_small.as_ref();
    out.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let i_px = y * w + x;
            let mut acc = [0f64; 3];
            let mut ws = 0f64;
            // the reference at or near its clip: the radiance is at least this
            let mr = maxc(fr[reference].image.data[i_px]);
            let bound = if mr >= 0.80 * clips[reference] { mr / k[reference] } else { 0.0 };
            for i in 0..n {
                if vd[i].as_ref().is_some_and(|v| !v[i_px]) {
                    continue;
                }
                let v = fr[i].image.data[i_px];
                let m = sat[i][i_px];
                let mut wt = k[i] * (1.0 - smoothstep(0.80 * clips[i], 0.96 * clips[i], m));
                if i == darkest {
                    wt += 1e-6;
                }
                if i == reference {
                    wt += 1e-9;
                } else if bound > 0.0 {
                    // a frame far darker than that saw something that moved there
                    wt *= smoothstep(0.35, 0.6, m / k[i] / bound);
                }
                if i != reference
                    && let Some((maps, fac)) = gs
                {
                    let g = sample_plane(&maps[i], (x as f32 + 0.5) / *fac as f32, (y as f32 + 0.5) / *fac as f32);
                    wt *= 1.0 - g;
                }
                if wt <= 0.0 {
                    continue;
                }
                let inv = 1.0 / k[i];
                for c in 0..3 {
                    acc[c] += (wt * v[c] * inv) as f64;
                }
                ws += wt as f64;
            }
            if ws > 0.0 {
                row[x] = acc.map(|a| (a / ws) as f32);
            } else {
                row[x] = fr[reference].image.data[i_px];
            }
        }
    });
    check(progress, 0.9, "Merging")?;
    // from here on only the reference's pixels and saturation are read: free the other frames' full-size buffers
    // before the detail pass allocates its own
    let mut sat = sat;
    let sat_ref = sat.get_mut(reference).map(std::mem::take).unwrap_or_default();
    drop(sat);
    drop(valid);
    for (i, f) in frames.iter_mut().enumerate() {
        if i != reference {
            f.image = Rgb32f::new(0, 0);
        }
    }
    // 6. the reference's own fine detail where it is well exposed
    reference_detail(&mut out, &frames[reference], &sat_ref);
    check(progress, 0.92, "Merging")?;

    let ghost = match &ghost_small {
        Some((maps, fac)) => {
            let fac = *fac as f32;
            let mut g = Plane::new(w, h);
            g.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
                for (x, o) in row.iter_mut().enumerate() {
                    let (sx, sy) = ((x as f32 + 0.5) / fac, (y as f32 + 0.5) / fac);
                    *o = (0..n).filter(|&i| i != reference).map(|i| sample_plane(&maps[i], sx, sy)).fold(0.0, f32::max);
                }
            });
            g
        }
        None => Plane::new(0, 0),
    };
    // per-frame results in the caller's order
    let mut ev_in = vec![0.0; n];
    let mut alignments_in = vec![Alignment::identity(); n];
    let mut local_in = vec![0f32; n];
    for (p, &i) in input.iter().enumerate() {
        ev_in[i] = ev[p];
        alignments_in[i] = alignments[p].clone();
        local_in[i] = local_shift[p];
    }
    let ev_exif = ev_exif.map(|e: Vec<f64>| {
        let mut v = vec![0.0; n];
        for (p, &i) in input.iter().enumerate() {
            v[i] = e[p];
        }
        v
    });
    let rf = &frames[reference];
    // the merge's white balance is the median of its inputs', as a panorama's: a CR2's or NEF's colour tags have none
    let wb_multipliers = consensus_multipliers(&frames);
    let mut color = rf.color.clone();
    if let Some(multipliers) = wb_multipliers {
        color.set_as_shot_multipliers(multipliers);
    }
    Ok(HdrResult {
        radiance: out,
        ghost,
        reference: input[reference],
        ev: ev_in,
        ev_exif,
        alignments: alignments_in,
        local_shift: local_in,
        color,
        raw: rf.raw,
        orientation: rf.orientation,
        metadata: rf.metadata.clone(),
        baseline_exposure: rf.baseline_exposure,
        lens: rf.lens.clone(),
        wb_multipliers,
    })
}

/// Below this scale (Gaussian σ, pixels) the merge's detail comes from the reference where it is well exposed.
const DETAIL_SIGMA: f32 = 2.0;

/// `|blur(d)|` relative to the local `√Y` (see [`reference_detail`]) from which the detail pass fades out: something
/// moved there. On a static handheld bracket (33 MP, after alignment) it stays under 0.045; where the wind moved
/// leaves between frames it reaches 0.1 and more.
const MOVED_FROM: f32 = 0.04;
/// … and from which it is off.
const MOVED_TO: f32 = 0.10;
/// How far (pixels) the pass stays away from such areas: its own blur reaches about this far.
const MOVED_MARGIN: usize = 4;

/// Give `out` (the merged radiance, in the reference's units) the reference frame's finest detail where the reference
/// is well exposed. Every other frame was resampled into the reference's geometry (Lanczos-3 still softens the finest
/// detail a little) and is aligned to a fraction of a pixel, not exactly; the brightest frames carry the most weight
/// in the midtones and shadows, so the merge was a little softer than the reference alone. On luminance in √ space:
/// `√Y' = √Y + α · (d − blur(d))` with `d = √Y_ref − √Y`, i.e. the merge's detail below [`DETAIL_SIGMA`] swapped for the
/// reference's, and every channel scaled by `Y'/Y` so colour stays the merge's. `α` fades out where the reference nears
/// its clip (its highlights come from darker frames) and where it is dark (there its detail is mostly noise, and the
/// brighter frames' is cleaner). It also fades out where something moved between the frames and no deghosting took
/// it out (leaves in the wind): there the merge is a blend of things in different places, `d` is far from zero over
/// whole areas rather than alternating as detail does, and the reference's edges laid over the blend came out as
/// hard, stepped double edges.
fn reference_detail(out: &mut Rgb32f, rf: &Frame, sat: &[f32]) {
    let (w, h) = (out.width, out.height);
    if rf.image.width != w || rf.image.height != h || sat.len() != w * h || w == 0 {
        return;
    }
    let y = |p: [f32; 3]| align::luma(p).max(0.0);
    let c = rf.clip;
    let (lo, hi) = if rf.raw { (0.01, 0.05) } else { (0.04, 0.12) };
    let d_at = |i: usize| {
        let (m, r) = (out.data.get(i).copied().unwrap_or_default(), rf.image.data.get(i).copied().unwrap_or_default());
        let dv = y(r).sqrt() - y(m).sqrt();
        if dv.is_finite() { dv } else { 0.0 }
    };
    // α per pixel, and α·d (d = √Y_ref − √Y_merge)
    let mut alpha = Plane::new(w, h);
    let mut ad = Plane::new(w, h);
    alpha.data.par_chunks_mut(w).zip(ad.data.par_chunks_mut(w)).enumerate().for_each(|(row, (arow, drow))| {
        for (x, (a, dv)) in arow.iter_mut().zip(drow.iter_mut()).enumerate() {
            let i = row * w + x;
            let (Some(&s), Some(&r)) = (sat.get(i), rf.image.data.get(i)) else { continue };
            *a = smoothstep(lo * c, hi * c, maxc(r)) * (1.0 - smoothstep(0.8 * c, 0.95 * c, s));
            *dv = *a * d_at(i);
        }
    });
    // d's blur over the pixels it applies to only (normalised by α's blur): the reference's clipped highlights,
    // where d is far below zero, must not pull their neighbours' blur down (that left a bright rim around them)
    let (ba, bad) = (gaussian(&alpha, DETAIL_SIGMA), gaussian(&ad, DETAIL_SIGMA));
    // where something moved: |blur(d)| against the local √Y (`ad`'s buffer reused), spread by the margin
    let mut moved = ad;
    moved.data.par_chunks_mut(w).enumerate().for_each(|(row, mrow)| {
        for (x, v) in mrow.iter_mut().enumerate() {
            let i = row * w + x;
            let (Some(&wa), Some(&wd)) = (ba.data.get(i), bad.data.get(i)) else { continue };
            let (m, r) = (out.data.get(i).copied().unwrap_or_default(), rf.image.data.get(i).copied().unwrap_or_default());
            let s = 0.5 * (y(m).sqrt() + y(r).sqrt()) + 1e-3;
            let rel = if wa > 1e-4 { (wd / wa).abs() / s } else { 0.0 };
            *v = if rel.is_finite() { rel } else { 0.0 };
        }
    });
    max_filter(&mut moved, MOVED_MARGIN);
    alpha.data.par_iter_mut().zip(moved.data.par_iter()).for_each(|(a, m)| *a *= 1.0 - smoothstep(MOVED_FROM, MOVED_TO, *m));
    drop(moved);
    out.data.par_chunks_mut(w).enumerate().for_each(|(row, orow)| {
        for (x, p) in orow.iter_mut().enumerate() {
            let i = row * w + x;
            let (Some(&a), Some(&wa), Some(&wd)) = (alpha.data.get(i), ba.data.get(i), bad.data.get(i)) else { continue };
            let ym = y(*p);
            if a <= 0.0 || wa <= 1e-4 || ym <= 1e-12 {
                continue;
            }
            let dv = {
                let (m, r) = (*p, rf.image.data.get(i).copied().unwrap_or_default());
                let v = y(r).sqrt() - y(m).sqrt();
                if v.is_finite() { v } else { 0.0 }
            };
            let sq = (ym.sqrt() + a * (dv - wd / wa)).max(0.0);
            let scale = sq * sq / ym;
            if scale.is_finite() {
                *p = p.map(|v| v * scale);
            }
        }
    });
}

/// Each value of `p` replaced by the largest within `r` pixels (a square), in place: a row pass then a column pass.
/// Not `lightcraft_raster::morph::max_filter`: that one returns a new plane through a copy and two transposes, two
/// more full-size planes (≈ 130 MB each at 33 MP) on top of the four the detail pass already holds, for a radius of only
/// [`MOVED_MARGIN`].
fn max_filter(p: &mut Plane, r: usize) {
    let (w, h) = (p.width, p.height);
    if w == 0 || h == 0 || r == 0 {
        return;
    }
    let mut rows = Plane::new(w, h);
    rows.data.par_chunks_mut(w).zip(p.data.par_chunks(w)).for_each(|(o, row)| {
        for (x, v) in o.iter_mut().enumerate() {
            let span = row.get(x.saturating_sub(r)..=(x + r).min(w - 1)).unwrap_or_default();
            *v = span.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        }
    });
    p.data.par_chunks_mut(w).enumerate().for_each(|(y, o)| {
        o.fill(f32::NEG_INFINITY);
        for src in y.saturating_sub(r)..=(y + r).min(h - 1) {
            let Some(row) = rows.data.get(src * w..(src + 1) * w) else { continue };
            for (v, s) in o.iter_mut().zip(row) {
                *v = v.max(*s);
            }
        }
    });
}

/// Bilinear sample of a plane at continuous coordinates (pixel centres at +0.5).
pub(crate) fn sample_plane(p: &Plane, x: f32, y: f32) -> f32 {
    if p.width == 0 || p.height == 0 {
        return 0.0;
    }
    crate::features::bilinear(p, x, y)
}

/// Median of ln(b / a) on the green channel over pixels well exposed in both (≥ 500 samples).
fn log_ratio(a: &Frame, va: Option<&[bool]>, b: &Frame, vb: Option<&[bool]>) -> Option<f64> {
    let len = a.image.data.len();
    let step = (len / 400_000).max(1);
    let (ca, cb) = (a.clip, b.clip);
    let lo = |f: &Frame| if f.raw { 0.01 } else { 0.04 };
    let (la, lb) = (lo(a) * ca, lo(b) * cb);
    let mut v: Vec<f32> = (0..len)
        .into_par_iter()
        .step_by(step)
        .filter_map(|i| {
            if va.is_some_and(|v| !v[i]) || vb.is_some_and(|v| !v[i]) {
                return None;
            }
            let (pa, pb) = (a.image.data[i], b.image.data[i]);
            if maxc(pa) >= 0.8 * ca || maxc(pb) >= 0.8 * cb || pa[1] <= la || pb[1] <= lb {
                return None;
            }
            Some((pb[1] / pa[1]).ln())
        })
        .collect();
    if v.len() < 500 {
        return None;
    }
    v.sort_by(|x, y| x.total_cmp(y));
    Some(v[v.len() / 2] as f64)
}

/// Box-downsample by `fac` (with the fraction of valid source pixels).
fn shrink(img: &Rgb32f, valid: Option<&[bool]>, fac: usize) -> (Rgb32f, Vec<f32>) {
    let (w, h) = (img.width.div_ceil(fac), img.height.div_ceil(fac));
    let mut out = Rgb32f::new(w, h);
    let mut vf = vec![0f32; w * h];
    out.data.par_chunks_mut(w).zip(vf.par_chunks_mut(w)).enumerate().for_each(|(y, (row, vrow))| {
        for x in 0..w {
            let mut s = [0f32; 3];
            let (mut nv, mut nt) = (0usize, 0usize);
            for sy in y * fac..((y + 1) * fac).min(img.height) {
                for sx in x * fac..((x + 1) * fac).min(img.width) {
                    let i = sy * img.width + sx;
                    nt += 1;
                    if valid.is_none_or(|v| v[i]) {
                        let p = img.data[i];
                        for c in 0..3 {
                            s[c] += p[c];
                        }
                        nv += 1;
                    }
                }
            }
            if nv > 0 {
                row[x] = s.map(|v| v / nv as f32);
            }
            vrow[x] = nv as f32 / nt.max(1) as f32;
        }
    });
    (out, vf)
}

/// Per-frame ghost masks (0..1) at 1/`fac` scale; the reference's is all zero.
fn deghost_maps(
    frames: &[Frame],
    valid: &[Option<Vec<bool>>],
    k: &[f32],
    order: &[usize],
    reference: usize,
    thr: f32,
    r: usize,
) -> (Vec<Plane>, usize) {
    let n = frames.len();
    let (w, h) = (frames[0].width(), frames[0].height());
    let fac = w.max(h).div_ceil(DEGHOST_EDGE).max(1);
    // the dilation/feather radius was chosen for a 1024 px map: keep it the same share of the picture
    let r = (r as f32 * (w.max(h) as f32 / fac as f32 / 1024.0).max(1.0)).round() as usize;
    let small: Vec<(Rgb32f, Vec<f32>)> = (0..n).into_par_iter().map(|i| shrink(&frames[i].image, valid[i].as_deref(), fac)).collect();
    let (sw, sh) = (small[0].0.width, small[0].0.height);
    let rpos = order.iter().position(|&i| i == reference).unwrap_or(0);
    // `dark`: the share of the clip below which a frame's (averaged) pixel is too noisy to predict from
    let good = |i: usize, p: usize, dark: f32| -> bool {
        let f = &frames[i];
        let m = maxc(small[i].0.data[p]);
        small[i].1[p] > 0.99 && m < 0.8 * f.clip && m > dark * f.clip
    };
    let dark = |i: usize| if frames[i].raw { 0.01 } else { 0.04 };
    // predicted reference radiance per pixel
    let pred: Vec<[f32; 3]> = (0..sw * sh)
        .into_par_iter()
        .map(|p| {
            let rad = |i: usize| small[i].0.data[p].map(|v| v / k[i]);
            // the reference predicts down to a tenth of the other frames' floor (the map's averaging tames its noise): in a
            // dark sky the brighter frames must not predict themselves, or their ghosts would look consistent
            if good(reference, p, dark(reference) * 0.1) {
                return rad(reference);
            }
            let bright = maxc(small[reference].0.data[p]) >= 0.8 * frames[reference].clip;
            if bright {
                for q in (0..rpos).rev() {
                    if good(order[q], p, dark(order[q])) {
                        return rad(order[q]);
                    }
                }
                rad(order[0])
            } else {
                for &i in &order[rpos + 1..] {
                    if good(i, p, dark(i)) {
                        return rad(i);
                    }
                }
                rad(reference)
            }
        })
        .collect();
    let maps: Vec<Plane> = (0..n)
        .into_par_iter()
        .map(|i| {
            if i == reference {
                return Plane::new(sw, sh);
            }
            let f = &frames[i];
            let e = if f.raw { 0.01 } else { 0.03 } * f.clip;
            let mut m = Plane::new(sw, sh);
            for p in 0..sw * sh {
                if small[i].1[p] < 0.5 {
                    continue;
                }
                let obs = small[i].0.data[p];
                let mut d = 0f32;
                for c in 0..3 {
                    let o = obs[c].min(f.clip);
                    let pr = (pred[p][c] * k[i]).min(f.clip);
                    d = d.max(((o + e) / (pr + e)).ln().abs());
                }
                m.data[p] = smoothstep(thr * 0.7, thr, d);
            }
            // dilate (max filter, square r) then feather
            let dil = dilate(&m, r);
            let mut g = gaussian(&dil, r as f32);
            for v in g.data.iter_mut() {
                *v = v.clamp(0.0, 1.0);
            }
            g
        })
        .collect();
    (maps, fac)
}

fn dilate(p: &Plane, r: usize) -> Plane {
    let (w, h) = (p.width, p.height);
    let mut tmp = Plane::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let (a, b) = (x.saturating_sub(r), (x + r).min(w - 1));
            tmp.data[y * w + x] = p.data[y * w + a..=y * w + b].iter().copied().fold(0.0, f32::max);
        }
    }
    let mut out = Plane::new(w, h);
    for y in 0..h {
        let (a, b) = (y.saturating_sub(r), (y + r).min(h - 1));
        for x in 0..w {
            out.data[y * w + x] = (a..=b).map(|yy| tmp.data[yy * w + x]).fold(0.0, f32::max);
        }
    }
    out
}

/// A frame's saturation per pixel: the brightest channel within 1 px (3 × 3). A demosaiced pixel next to clipped
/// photosites is interpolated from them and reads low in the channel that clipped (green, in camera RGB), so it loses
/// its weight with them. On real ARW brackets the brightest frame's green was 3–5 % low within 2 px of clipped pixels
/// (and linear to 1 % elsewhere), and as that frame carries the most weight, bright windows got magenta fringes.
fn saturation(img: &Rgb32f) -> Vec<f32> {
    let (w, h) = (img.width, img.height);
    let m: Vec<f32> = img.data.iter().map(|p| maxc(*p)).collect();
    if w == 0 || h == 0 {
        return m;
    }
    let mut rows = vec![0f32; w * h];
    rows.par_chunks_mut(w).zip(m.par_chunks(w)).for_each(|(out, src)| {
        for (x, o) in out.iter_mut().enumerate() {
            *o = src[x.saturating_sub(1)..=(x + 1).min(w - 1)].iter().copied().fold(0.0, f32::max);
        }
    });
    let mut out = vec![0f32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let (a, b) = (y.saturating_sub(1), (y + 1).min(h - 1));
        for (x, v) in row.iter_mut().enumerate() {
            *v = (a..=b).map(|yy| rows[yy * w + x]).fold(0.0, f32::max);
        }
    });
    out
}
