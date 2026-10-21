//! Focus stacking: photos focused at different distances → one photo sharp throughout.
//!
//! Stacks can be long (dozens of frames of tens of megapixels), so full-size frames are never all
//! in memory:
//!
//! 1. **Align** on small (≤ [`align::WORK_EDGE`]) versions: each frame is registered to its
//!    neighbour (neighbours share the most sharp detail) with a similarity — focusing changes the
//!    magnification ("focus breathing"), a tripod nothing else — and the steps are chained. A step
//!    that finds no fit is bridged to one of the [`BRIDGE`] frames before; a frame that aligns to
//!    none of them is left out (and reported), and so is every frame not chained to the largest
//!    group. The frame that sees the smallest part of the scene becomes the reference, so every
//!    other frame covers it.
//! 2. **Brightness**: each frame's against the one it is chained to, measured as the median log
//!    ratio of their aligned luminance thumbnails where both are well exposed (EXIF only when that
//!    fails: auto ISO may change the tags without changing the raw data), chained to the reference.
//! 3. **Fuse** at full size, one frame at a time ([`Fuser`]), outwards from the reference: each
//!    frame's chained alignment is refined at full size against its neighbour, already aligned
//!    ([`crate::refine`], a similarity; on small versions 8× below the frame the chain was up to
//!    1.9 px off at the corners, refined 0.03 px). Each frame is brought to the reference's
//!    brightness, resampled into its geometry (Lanczos-3: bilinear lost a fifth of the texture,
//!    every frame being magnified a little differently) and decomposed into a Laplacian pyramid
//!    (in square-root space, so shadows count). Every band is the weighted mean of the frames'
//!    bands: with `e` the local energy of the band's luminance, summed over a window
//!    ([`ACTIVITY_SIGMA`]) wider than a defocused edge spreads, and `ε` the band's noise floor (a
//!    low quantile of the first frame's energy), a frame weighs 1 while `e ≤ 2ε` ([`NOISE_MARGIN`])
//!    and `(1 + e/ε − 2)^16` ([`SHARPNESS_POWER`]) above. Where frames carry detail the sharpest
//!    dominates (P. J. Burt, R. J. Kolczynski, "Enhanced image capture through fusion", ICCV 1993,
//!    with a steep soft instead of a maximum selection); where none carries more than noise they
//!    average, so the out-of-focus background gets quieter instead of keeping every frame's noise
//!    peaks (a maximum selection was noisier than any single frame). The coarsest level is the
//!    frames' average.
//!
//! Memory: one full-size frame being read, the canvas pyramid and the accumulated pyramid, however
//! many frames (5.0 GB peak for 67 MP Sony A7R VI raws); the small versions are read
//! [`PARALLEL_LOADS`] at a time and only their features and thumbnails kept.

use lightcraft_geom::{Homography, Point};
use lightcraft_raster::blur::gaussian;
use lightcraft_raster::resample::{Filter, fit};
use lightcraft_raster::{Plane, Rgb32f};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::align::{self, Alignment};
use crate::features::Features;
use crate::frame::{Frame, FrameColor};
use crate::pano::blend::{Buf, down_into, sub_up, up};
use crate::refine;
use crate::{MergeError, Progress, Result, check};

/// Gaussian window (pixels of each pyramid level) over which a band's energy is summed. 1 leaves
/// halos beside edges; 3 to 12 measure alike on the synthetic bracket (`tests/focus.rs`).
const ACTIVITY_SIGMA: f32 = 3.0;
/// How strictly the sharpest frame wins where frames carry detail: a frame's weight grows as its
/// band energy over the noise margin to this power. On a real 88-frame macro crop 16 kept 95–100 %
/// of a maximum selection's texture (noise discounted); 8 kept 90–98 %, 4 about 85 %.
const SHARPNESS_POWER: f32 = 16.0;
/// Band energy up to this many times the noise floor counts as noise: those frames weigh alike.
const NOISE_MARGIN: f32 = 2.0;
/// Quantile of the first frame's band energy taken as the band's noise floor.
const NOISE_QUANTILE: f64 = 0.1;
/// Largest search radius (full-resolution pixels) refining a frame against its neighbour: the error left by the
/// chained small-version alignment, relative to an already refined neighbour.
const REFINE_RADIUS: usize = 8;
/// How many frames back a frame whose neighbour step fails is tried against.
pub const BRIDGE: usize = 3;
/// Long edge of the luminance thumbnails brightness is measured on.
const THUMB_EDGE: usize = 320;
/// How many small versions are read at once (≈ 0.4 GB each for a 60 MP raw).
const PARALLEL_LOADS: usize = 8;
/// Brightness steps between linked frames smaller than this (EV) are taken as none.
const STEADY: f64 = 0.05;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FocusOptions {
    pub align: bool,
}

impl Default for FocusOptions {
    fn default() -> Self {
        FocusOptions { align: true }
    }
}

/// The stacked result, in the reference frame's geometry and units.
#[derive(Clone, Debug)]
pub struct FocusResult {
    pub image: Rgb32f,
    /// Index (into the inputs) of the reference frame.
    pub reference: usize,
    /// Per input: the mapping from reference to frame pixels (full resolution) and how it was found
    /// (`model`: "reference", "similarity", "none" with Auto Align off, "failed" for a frame left
    /// out); `rms` in full-resolution pixels.
    pub alignments: Vec<Alignment>,
    /// Inputs left out because they could not be aligned.
    pub skipped: Vec<usize>,
    /// Per input: its brightness relative to the reference, in EV, as used (0 for those left out).
    pub ev: Vec<f64>,
    pub color: FrameColor,
    pub raw: bool,
    pub orientation: lightcraft_geom::Orientation,
    pub metadata: lightcraft_meta::Metadata,
    pub baseline_exposure: f64,
    /// The reference frame's lens corrections ([`Frame::lens`]).
    pub lens: Vec<lightcraft_raw::Opcode>,
}

/// What [`stack_focus`] asks its loader for. Frames are never turned to their orientation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Load {
    /// A version of at most [`align::WORK_EDGE`] pixels, to align and measure brightness on.
    Align,
    /// The frame at the size to stack at; only its pixels are used.
    Stack,
    /// The reference frame at the size to stack at: its colour, metadata and lens corrections are
    /// the result's.
    Reference,
}

/// Loads input `i`.
pub type Loader<'a> = dyn Fn(usize, Load) -> Result<Frame> + Sync + 'a;

/// What pass 1 keeps of a frame.
struct Small {
    features: Features,
    width: usize,
    height: usize,
    exposure: Option<f64>,
    /// Linear luminance, ≤ [`THUMB_EDGE`] px, and the frame's clipping level.
    thumb: Plane,
    clip: f32,
    /// Brings the frame's median luminance to 0.18 (for matching at full size, [`refine::luma`]).
    gain: f32,
}

fn no_pixels(i: usize) -> MergeError {
    MergeError::Decode(format!("photo {} has no pixels", i + 1))
}

/// Stack `n` frames, read through `load` (twice each: small, then full size).
pub fn stack_focus(n: usize, load: &Loader, opts: &FocusOptions, progress: &Progress) -> Result<FocusResult> {
    if n < 2 {
        return Err(MergeError::TooFew(n, 2));
    }
    check(progress, 0.0, "Aligning")?;

    // 1. features and brightness thumbnails of the small versions (in parallel)
    let done = std::sync::atomic::AtomicUsize::new(0);
    let one = |i: usize| -> Result<Small> {
        let f = load(i, Load::Align)?;
        if f.width() == 0 || f.height() == 0 {
            return Err(no_pixels(i));
        }
        let gain = 0.18 / align::median_luma(&f.image, f.clip).max(1e-6);
        // features are found on the work image, so its size is the small frame's (a loader may hand over more than
        // WORK_EDGE pixels)
        let (p, _) = align::work_image(&f.image, gain, align::WORK_EDGE);
        let thumb = fit(&f.image, THUMB_EDGE, THUMB_EDGE, Filter::Box).map(align::luma);
        let k = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        check(progress, 0.25 * k as f32 / n as f32, "Aligning")?;
        Ok(Small {
            features: crate::features::detect(&p, 1500),
            width: p.width,
            height: p.height,
            exposure: f.exposure.filter(|e| e.is_finite()),
            thumb,
            clip: f.clip,
            gain,
        })
    };
    // a few at a time: reading a raw takes the whole sensor's samples, binned or not
    let mut small: Vec<Small> = Vec::with_capacity(n);
    for chunk in (0..n).collect::<Vec<_>>().chunks(PARALLEL_LOADS) {
        let part: Vec<Small> = chunk.par_iter().map(|&i| one(i)).collect::<Result<_>>()?;
        small.extend(part);
    }
    let (sw, sh) = (small[0].width, small[0].height);
    if small.iter().any(|s| s.width != sw || s.height != sh) {
        return Err(MergeError::Mismatch("focus stacking needs photos of the same size".into()));
    }

    // links: `link[i] = (j, j → i)` with j < i, the nearest earlier frame i aligns to
    let mut link: Vec<Option<(usize, Alignment)>> = vec![None; n];
    if opts.align {
        let pair = |j: usize, i: usize| {
            // a similarity only: a homography fitted to features crowded into the sharp slice of
            // the scene extrapolates badly to the corners
            let a = align::align_pair_with(&refine::SIMILARITY, &small[j].features, &small[i].features, 1.0, sw, sh, (j * n + i) as u64);
            (a.model != "identity").then_some((j, a))
        };
        let steps: Vec<Option<(usize, Alignment)>> = (1..n).into_par_iter().map(|i| pair(i - 1, i)).collect();
        for (i, s) in (1..n).zip(steps) {
            link[i] = s;
        }
        let bridged: Vec<(usize, Option<(usize, Alignment)>)> = (1..n)
            .into_par_iter()
            .filter(|&i| link[i].is_none())
            .map(|i| (i, (2..=BRIDGE).filter_map(|d| i.checked_sub(d)).find_map(|j| pair(j, i))))
            .collect();
        for (i, l) in bridged {
            link[i] = l;
        }
    } else {
        for (i, l) in link.iter_mut().enumerate().skip(1) {
            *l = Some((i - 1, Alignment { model: "none", ..Alignment::identity() }));
        }
    }
    check(progress, 0.27, "Aligning")?;

    // the largest group of frames chained together, each mapped from its first frame's pixels
    let mut root = (0..n).collect::<Vec<_>>();
    let mut chain = vec![Homography::IDENTITY; n];
    for i in 1..n {
        if let Some((j, a)) = &link[i] {
            root[i] = root[*j];
            chain[i] = a.h.mul(&chain[*j]);
        }
    }
    let mut size = vec![0usize; n];
    for &r in &root {
        size[r] += 1;
    }
    let best = (0..n).max_by(|&a, &b| size[a].cmp(&size[b]).then(b.cmp(&a))).unwrap_or(0);
    if size[best] < 2 {
        return Err(MergeError::NoOverlap(
            "the photos could not be aligned (too little in common); turn off Auto Align to stack them as they are".into(),
        ));
    }
    let used: Vec<usize> = (0..n).filter(|&i| root[i] == best).collect();
    let skipped: Vec<usize> = (0..n).filter(|&i| root[i] != best).collect();

    // the reference sees the least of the scene: the smallest footprint in the group's first frame
    let footprint = |h: &Homography| {
        let Some(inv) = h.inverse() else { return f64::INFINITY };
        let (w, ht) = (sw as f64, sh as f64);
        let c: Vec<Point> = [(0.0, 0.0), (w, 0.0), (w, ht), (0.0, ht)].iter().map(|&(x, y)| inv.apply(Point::new(x, y))).collect();
        let s: f64 = (0..4).map(|k| c[k].x * c[(k + 1) % 4].y - c[(k + 1) % 4].x * c[k].y).sum();
        let a = s.abs() / 2.0;
        if a.is_finite() { a } else { f64::INFINITY }
    };
    let reference = used.iter().copied().min_by(|&a, &b| footprint(&chain[a]).total_cmp(&footprint(&chain[b])).then(a.cmp(&b))).unwrap_or(best);
    let to_ref = chain[reference].inverse().unwrap_or(Homography::IDENTITY);

    // 2. brightness along the links (ln), relative to the reference
    let mut lb = vec![0f64; n];
    for &i in &used {
        if let Some((j, a)) = &link[i] {
            let measured = log_ratio((&small[*j].thumb, small[*j].clip), (&small[i].thumb, small[i].clip), &a.h, sw);
            let exif = small[i].exposure.zip(small[*j].exposure).map(|(e, f)| (e - f) * std::f64::consts::LN_2);
            // a camera changes exposure in steps of ⅓ EV or more; below [`STEADY`] it is measuring
            // noise, which chained over a long stack would wander
            let step = measured.or(exif).unwrap_or(0.0);
            lb[i] = lb[*j] + if step.abs() < STEADY * std::f64::consts::LN_2 { 0.0 } else { step };
        }
    }
    let ev: Vec<f64> = (0..n).map(|i| if root[i] == best { (lb[i] - lb[reference]) / std::f64::consts::LN_2 } else { 0.0 }).collect();
    check(progress, 0.3, "Stacking")?;

    // 3. fuse at full size, one frame at a time, outwards from the reference: each frame's chained alignment is
    //    refined at full size against its neighbour, already aligned (neighbours share the most sharp detail)
    let mut fuser: Option<Fuser> = None;
    let mut alignments = vec![Alignment { model: "failed", ..Alignment::identity() }; n];
    let mut reference_frame: Option<Frame> = None;
    let below: Vec<usize> = used.iter().rev().copied().filter(|&i| i < reference).collect();
    let above: Vec<usize> = used.iter().copied().filter(|&i| i > reference).collect();
    let order: Vec<usize> = std::iter::once(reference).chain(below.iter().copied()).chain(above.iter().copied()).collect();
    // the reference's luminance, and the last frame's with its alignment (reference → that frame): what the next
    // frame is refined against, each in its own geometry
    let mut ref_luma: Option<Plane> = None;
    let mut prev: Option<(Plane, Homography)> = None;
    for (k, &i) in order.iter().enumerate() {
        if above.first() == Some(&i) {
            prev = None;
        }
        let mut f = load(i, if i == reference { Load::Reference } else { Load::Stack })?;
        let (w, h) = (f.width(), f.height());
        if w == 0 || h == 0 {
            return Err(no_pixels(i));
        }
        let fu = match &mut fuser {
            Some(fu) => fu,
            None => fuser.insert(Fuser::new(w, h)?),
        };
        if (w, h) != (fu.fw, fu.fh) {
            return Err(MergeError::Mismatch("focus stacking needs photos of the same size".into()));
        }
        let scale = sw as f64 / w as f64;
        let mut hfull = align::scale_homography(&chain[i].mul(&to_ref), scale);
        let (model, inliers, rms) = match (i == reference, &link[i]) {
            (true, _) => ("reference", 0, 0.0),
            (false, Some((_, a))) => (a.model, a.inliers, a.rms / scale),
            // the group's first frame: aligned through the frames chained to it
            (false, None) => (if opts.align { "similarity" } else { "none" }, 0, 0.0),
        };
        alignments[i] = Alignment { h: hfull, model, inliers, rms };
        let gain = (-ev[i] * std::f64::consts::LN_2).exp().clamp(1.0 / 16.0, 16.0) as f32;
        if opts.align {
            let l = refine::luma(&f.image, small[i].gain, f.clip);
            if i == reference {
                ref_luma = Some(l);
            } else {
                // against the neighbour (the reference for the first frame on each side): its pixels map to this frame's
                // through hfull · h_neighbour⁻¹, and the fit composed with h_neighbour is this frame's alignment
                let neighbour = prev.as_ref().map(|(p, h)| (p, *h)).or_else(|| ref_luma.as_ref().map(|p| (p, Homography::IDENTITY)));
                if let Some((p, hn)) = neighbour
                    && let Some(hn_inv) = hn.inverse()
                    && let Some(r) =
                        refine::fit(p, &l, &hfull.mul(&hn_inv), refine::radius_for(scale).min(REFINE_RADIUS), (i + 1) as u64, &refine::SIMILARITY)
                {
                    hfull = r.h.mul(&hn);
                    alignments[i] = Alignment { h: hfull, ..r };
                }
                prev = Some((l, hfull));
                // the reference is the neighbour of the first frame on each side only: free it after its last use
                if above.first() == Some(&i) || (above.is_empty() && below.first() == Some(&i)) {
                    ref_luma = None;
                }
            }
        }
        fu.fill(&f.image, (i != reference).then_some(&hfull), gain);
        // only the reference's colour and metadata are kept, never its pixels
        f.image = Rgb32f::new(0, 0);
        fu.add();
        if i == reference {
            reference_frame = Some(f);
        }
        check(progress, 0.3 + 0.65 * (k + 1) as f32 / used.len() as f32, "Stacking")?;
    }
    let (Some(fu), Some(r)) = (fuser, reference_frame) else {
        return Err(MergeError::TooFew(used.len(), 2));
    };
    let (w, h) = (fu.fw, fu.fh);
    let fused = fu.finish();
    let mut image = Rgb32f::new(w, h);
    image.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, p) in row.iter_mut().enumerate() {
            let o = (y * fused.w + x) * 3;
            *p = std::array::from_fn(|c| fused.data.get(o + c).copied().unwrap_or(0.0).max(0.0).powi(2));
        }
    });
    check(progress, 1.0, "Stacking")?;
    Ok(FocusResult {
        image,
        reference,
        alignments,
        skipped,
        ev,
        color: r.color,
        raw: r.raw,
        orientation: r.orientation,
        metadata: r.metadata,
        baseline_exposure: r.baseline_exposure,
        lens: r.lens,
    })
}

/// ln(brightness of the frame of `b` / that of `a`): the ratio of their luminance thumbnails'
/// sums (with their frames' clipping levels) over the pixels both see away from clipping, `a` at
/// pixel q against `b` at `h(q)` (`h` maps the small frames' pixels, `small_w` wide, of `a` to `b`).
/// Sums, not a median of per-pixel ratios: blur keeps a sum, while the per-pixel ratios of two
/// differently blurred frames lean one way, and a stack chains a hundred such steps. `None` when
/// too few pixels compare.
fn log_ratio((a, clip_a): (&Plane, f32), (b, clip_b): (&Plane, f32), h: &Homography, small_w: usize) -> Option<f64> {
    let ht = align::scale_homography(h, a.width as f64 / small_w.max(1) as f64);
    // clipped highlights, with the margin a blur spreads them over
    let (near_a, near_b) = (local_max(a, 2), local_max(b, 2));
    let (sa, sb, n) = (0..a.height)
        .into_par_iter()
        .map(|y| {
            let (mut sa, mut sb, mut n) = (0f64, 0f64, 0usize);
            for x in 0..a.width {
                let q = ht.apply(Point::new(x as f64 + 0.5, y as f64 + 0.5));
                if !(q.x >= 0.0 && q.y >= 0.0 && q.x < b.width as f64 && q.y < b.height as f64) {
                    continue;
                }
                let (bx, by) = (q.x as usize, q.y as usize);
                if near_a.get(x, y) < 0.9 * clip_a && near_b.get(bx, by) < 0.9 * clip_b {
                    sa += f64::from(a.get(x, y));
                    sb += f64::from(b.get(bx, by));
                    n += 1;
                }
            }
            (sa, sb, n)
        })
        .reduce(|| (0.0, 0.0, 0), |p, q| (p.0 + q.0, p.1 + q.1, p.2 + q.2));
    (n >= 200 && sa > 0.0 && sb > 0.0).then(|| (sb / sa).ln()).filter(|r| r.is_finite())
}

/// The maximum of `p` over the `(2r + 1)²` neighbourhood of each pixel.
fn local_max(p: &Plane, r: usize) -> Plane {
    let rows =
        Plane::from_fn(p.width, p.height, |x, y| (x.saturating_sub(r)..=(x + r).min(p.width - 1)).map(|i| p.get(i, y)).fold(f32::MIN, f32::max));
    Plane::from_fn(p.width, p.height, |x, y| (y.saturating_sub(r)..=(y + r).min(p.height - 1)).map(|j| rows.get(x, j)).fold(f32::MIN, f32::max))
}

/// Soft max-activity pyramid fusion (see the module docs), one frame at a time: [`Fuser::fill`]
/// puts a frame on the canvas, [`Fuser::add`] adds its pyramid to the weighted sums.
struct Fuser {
    /// Frame size, and the canvas (multiples of 2^levels).
    fw: usize,
    fh: usize,
    levels: usize,
    /// Per band, weighted sums of the frames' coefficients (3 channels); the coarsest level last.
    acc: Vec<Buf>,
    /// Their weight sums.
    wsum: Vec<Buf>,
    /// Per band, the noise floor of the energy (set by the first frame).
    floor: Vec<f32>,
    /// The frame being added: its Gaussian pyramid (√values, then bands in place) and validity.
    g: Vec<Buf>,
    m: Vec<Buf>,
    /// Per band, the largest log weight so far (the sums are kept relative to it).
    top: Vec<Buf>,
}

impl Fuser {
    /// A fuser for `w × h` frames (≥ 1 × 1).
    fn new(w: usize, h: usize) -> Result<Fuser> {
        let levels = ((w.min(h) as f64).log2() - 3.0).floor().clamp(1.0, 8.0) as usize;
        let m = 1usize << levels;
        let too_big = || MergeError::Mismatch(format!("{w} × {h} pixels is too large to stack"));
        let (cw, ch) = (w.div_ceil(m).checked_mul(m).ok_or_else(too_big)?, h.div_ceil(m).checked_mul(m).ok_or_else(too_big)?);
        cw.checked_mul(ch).and_then(|p| p.checked_mul(3)).ok_or_else(too_big)?;
        let pyramid = |c: usize| (0..=levels).map(|l| Buf::new(cw >> l, ch >> l, c)).collect::<Vec<_>>();
        Ok(Fuser { fw: w, fh: h, levels, acc: pyramid(3), wsum: pyramid(1), floor: Vec::new(), g: pyramid(3), m: pyramid(1), top: pyramid(1) })
    }

    /// Put a frame (`fw × fh`) on the canvas: √(value × `gain`) and validity, resampled through `h`
    /// (reference → frame pixels) when given; beyond the frame's edges the canvas repeats them.
    fn fill(&mut self, img: &Rgb32f, h: Option<&Homography>, gain: f32) {
        let (Some(g), Some(m)) = (self.g.first_mut(), self.m.first_mut()) else { return };
        let (w, ht, cw) = (img.width, img.height, g.w);
        if w == 0 || ht == 0 {
            return;
        }
        let sq = |v: f32| {
            let v = v * gain;
            if v.is_finite() { v.max(0.0).sqrt() } else { 0.0 }
        };
        g.data.par_chunks_mut(cw * 3).zip(m.data.par_chunks_mut(cw)).enumerate().for_each(|(y, (grow, mrow))| {
            let sy = y.min(ht - 1);
            for x in 0..cw {
                let sx = x.min(w - 1);
                let (p, ok) = match h {
                    None => (img.get(sx, sy), true),
                    Some(h) => {
                        let q = h.apply(Point::new(sx as f64 + 0.5, sy as f64 + 0.5));
                        let ok = q.x >= 0.0 && q.y >= 0.0 && q.x <= w as f64 && q.y <= ht as f64;
                        (img.sample_lanczos3(q.x as f32, q.y as f32), ok)
                    }
                };
                for c in 0..3 {
                    grow[x * 3 + c] = sq(p[c]);
                }
                mrow[x] = if ok { 1.0 } else { 0.0 };
            }
        });
    }

    /// Add the frame on the canvas to the sums.
    fn add(&mut self) {
        let levels = self.levels;
        for l in 0..levels {
            let (lo, hi) = self.g.split_at_mut(l + 1);
            down_into(&lo[l], &mut hi[0]);
            let (lo, hi) = self.m.split_at_mut(l + 1);
            down_into(&lo[l], &mut hi[0]);
        }
        for l in 0..levels {
            let (lo, hi) = self.g.split_at_mut(l + 1);
            let band = &mut lo[l];
            sub_up(band, &hi[0]);
            // energy of the band's luminance, summed over the window
            let mut e = Plane::new(band.w, band.h);
            e.data.par_iter_mut().zip(band.data.par_chunks(3)).for_each(|(e, b)| {
                let y = 0.25 * b[0] + 0.6 * b[1] + 0.15 * b[2];
                *e = y * y;
            });
            let e = gaussian(&e, ACTIVITY_SIGMA);
            if self.floor.len() <= l {
                self.floor.push(quantile(&e.data, NOISE_QUANTILE).max(1e-8));
            }
            let floor = self.floor[l];
            let (acc, ws, valid, top) = (&mut self.acc[l], &mut self.wsum[l], &self.m[l], &mut self.top[l]);
            let bw = band.w;
            acc.data
                .par_chunks_mut(bw * 3)
                .zip(ws.data.par_chunks_mut(bw))
                .zip(band.data.par_chunks(bw * 3))
                .zip(e.data.par_chunks(bw).zip(valid.data.par_chunks(bw)))
                .zip(top.data.par_chunks_mut(bw))
                .for_each(|((((arow, wrow), brow), (erow, mrow)), trow)| {
                    for x in 0..bw {
                        // inside the frame (with the ~2 pixels a band's support reaches)
                        if mrow[x] <= 0.999 {
                            continue;
                        }
                        // log weight; the sums are kept relative to the largest so far, so a
                        // steep power can't overflow
                        let r = erow[x] / floor;
                        let lw = if r > NOISE_MARGIN { SHARPNESS_POWER * (1.0 + r - NOISE_MARGIN).ln() } else { 0.0 };
                        let w = if lw > trow[x] {
                            let s = (trow[x] - lw).exp();
                            for v in &mut arow[x * 3..x * 3 + 3] {
                                *v *= s;
                            }
                            wrow[x] *= s;
                            trow[x] = lw;
                            1.0
                        } else {
                            (lw - trow[x]).exp()
                        };
                        for c in 0..3 {
                            arow[x * 3 + c] += w * brow[x * 3 + c];
                        }
                        wrow[x] += w;
                    }
                });
        }
        let (g, m) = (&self.g[levels], &self.m[levels]);
        let (acc, ws) = (&mut self.acc[levels], &mut self.wsum[levels]);
        for (i, &wt) in m.data.iter().enumerate() {
            for c in 0..3 {
                acc.data[i * 3 + c] += g.data[i * 3 + c] * wt;
            }
            ws.data[i] += wt;
        }
    }

    /// The weighted means, collapsed (√values on the canvas).
    fn finish(self) -> Buf {
        // the canvas pyramids go first: collapsing needs room for two levels of its own
        let Fuser { fw, fh, mut acc, wsum, g, m, top, .. } = self;
        drop((g, m, top));
        for (a, ws) in acc.iter_mut().zip(&wsum) {
            a.data.par_chunks_mut(3).zip(ws.data.par_iter()).for_each(|(p, &s)| {
                for v in p {
                    *v = if s > 0.0 { *v / s } else { 0.0 };
                }
            });
        }
        drop(wsum);
        let Some(mut res) = acc.pop() else { return Buf::new(fw, fh, 3) };
        while let Some(band) = acc.pop() {
            let mut u = up(&res, band.w, band.h);
            drop(res);
            u.data.par_iter_mut().zip(band.data.par_iter()).for_each(|(a, b)| *a += b);
            res = u;
        }
        res
    }
}

/// The `q` quantile of `v` (on at most ~1 M samples), 0 when empty.
fn quantile(v: &[f32], q: f64) -> f32 {
    let step = (v.len() / 1_000_000).max(1);
    let mut s: Vec<f32> = v.iter().step_by(step).copied().filter(|x| x.is_finite()).collect();
    if s.is_empty() {
        return 0.0;
    }
    let k = ((s.len() - 1) as f64 * q) as usize;
    *s.select_nth_unstable_by(k, f32::total_cmp).1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texture(w: usize, h: usize) -> Rgb32f {
        Rgb32f::from_fn(w, h, |x, y| {
            let v = 0.3 + 0.2 * ((x as f32 * 0.9).sin() * (y as f32 * 0.7).cos()) + 0.1 * ((x * 7 + y * 13) % 5) as f32 / 5.0;
            [v, v * 0.8, v * 0.6]
        })
    }

    fn fuse(frames: &[Rgb32f]) -> Rgb32f {
        let (w, h) = (frames[0].width, frames[0].height);
        let mut fu = Fuser::new(w, h).unwrap();
        for f in frames {
            fu.fill(f, None, 1.0);
            fu.add();
        }
        let b = fu.finish();
        Rgb32f::from_fn(w, h, |x, y| std::array::from_fn(|c| b.data[(y * b.w + x) * 3 + c].max(0.0).powi(2)))
    }

    fn mae(a: &Rgb32f, b: &Rgb32f) -> f32 {
        a.data.iter().zip(&b.data).map(|(p, q)| (0..3).map(|c| (p[c] - q[c]).abs()).sum::<f32>()).sum::<f32>() / a.data.len() as f32
    }

    /// Deterministic noise, roughly N(0, σ).
    fn noisy(img: &Rgb32f, sigma: f32, seed: u64) -> Rgb32f {
        let mut s = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
        let mut unit = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 40) as f32 / (1u64 << 24) as f32
        };
        let mut out = img.clone();
        for v in out.data.iter_mut().flat_map(|p| p.iter_mut()) {
            *v += sigma * ((0..4).map(|_| unit()).sum::<f32>() - 2.0) * 1.7;
        }
        out
    }

    #[test]
    fn identical_frames_reproduce() {
        let t = texture(100, 70);
        let out = fuse(&[t.clone(), t.clone(), t.clone()]);
        assert!(mae(&out, &t) < 1e-4, "{}", mae(&out, &t));
    }

    #[test]
    fn the_sharp_half_of_each_frame_wins() {
        let t = texture(128, 96);
        let b = gaussian(&t, 2.5);
        let left = Rgb32f::from_fn(128, 96, |x, y| if x < 64 { t.get(x, y) } else { b.get(x, y) });
        let right = Rgb32f::from_fn(128, 96, |x, y| if x >= 64 { t.get(x, y) } else { b.get(x, y) });
        let out = fuse(&[left.clone(), right]);
        eprintln!("blurred {} left-sharp {} fused {}", mae(&b, &t), mae(&left, &t), mae(&out, &t));
        assert!(mae(&out, &t) < 0.2 * mae(&b, &t));
    }

    #[test]
    fn noise_in_equally_blurred_areas_averages_out() {
        // a maximum selection kept every frame's noise peaks: noisier than any frame
        let flat = Rgb32f::from_fn(96, 64, |_, _| [0.3, 0.25, 0.2]);
        let frames: Vec<Rgb32f> = (0..8).map(|k| noisy(&flat, 0.01, k + 1)).collect();
        let out = fuse(&frames);
        let single = mae(&frames[0], &flat);
        let stacked = mae(&out, &flat);
        eprintln!("single frame noise {single}, stack {stacked}");
        assert!(stacked < 0.7 * single, "stack {stacked} vs a frame {single}");
    }

    #[test]
    fn a_non_finite_pixel_stays_where_it_is() {
        let t = texture(64, 48);
        let mut bad = t.clone();
        bad.data[20 * 64 + 30] = [f32::INFINITY, f32::NAN, -f32::INFINITY];
        let out = fuse(&[t.clone(), bad, t.clone()]);
        assert!(out.data.iter().all(|p| p.iter().all(|v| v.is_finite())));
        let changed = out.data.iter().zip(&t.data).filter(|(a, b)| (0..3).any(|c| (a[c] - b[c]).abs() > 0.02)).count();
        assert!(changed < 30, "{changed} pixels changed");
    }

    #[test]
    fn tiny_frames_stack() {
        for (w, h) in [(1, 1), (2, 3), (1, 40), (40, 1), (7, 5)] {
            let t = texture(w, h);
            let out = fuse(&[t.clone(), t.clone()]);
            assert_eq!((out.width, out.height), (w, h));
            assert!(mae(&out, &t) < 1e-3, "{w}×{h}: {}", mae(&out, &t));
        }
    }
}
