//! Panorama stitching.
//!
//! 1. Features ([`crate::features`]) on each frame at ≤ 1000 px, pairwise matching and RANSAC
//!    homographies, largest connected set ([`camera`]).
//! 2. Camera rotations + focal lengths: initialised from the homographies, bundle-adjusted,
//!    straightened ([`camera`]).
//! 3. Projection: spherical, cylindrical or perspective (rectilinear), or chosen automatically
//!    from the field of view ([`choose_projection`]).
//! 4. Exposure compensation: EXIF exposure, then a gain and a black offset per image solved from
//!    every overlap ([`compensate_exposure`]): Brown & Lowe's gain compensation extended to an
//!    offset, so the veiling flare of frames facing the sun (light added, not multiplied) is taken
//!    out too instead of leaving their shadows lifted next to the other frames'.
//! 5. Seams: each canvas pixel belongs to the image whose centre-weighted feather weight is
//!    highest there (Voronoi-like seams away from image borders); what one gain and offset per frame
//!    leave there (the sky's gradient, flare growing towards the sun) is leveled on a coarse grid
//!    ([`level`]); multi-band blending in log space ([`blend`]).
//! 6. Boundary warp, auto crop, fill edges ([`finish`]).

pub mod blend;
pub mod camera;
pub mod finish;
mod level;
pub mod project;
mod vignette;

use lightcraft_raster::{Plane, Rgb32f};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

pub use project::Projection;

use crate::frame::{Frame, FrameColor, consensus_multipliers};
use crate::linalg;
use crate::{MergeError, Progress, Result, check};
use blend::{Blender, Buf, push_pull};
use camera::Camera;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PanoOptions {
    pub projection: Projection,
    /// 0..100.
    pub boundary_warp: f64,
    pub auto_crop: bool,
    pub fill_edges: bool,
    /// Output size limit (megapixels); the panorama is scaled down to fit. At most
    /// [`MAX_MEGAPIXELS`] (the default: the photos' own resolution).
    pub max_megapixels: f64,
}

impl Default for PanoOptions {
    fn default() -> Self {
        PanoOptions { projection: Projection::Auto, boundary_warp: 0.0, auto_crop: false, fill_edges: false, max_megapixels: MAX_MEGAPIXELS }
    }
}

/// The largest panorama, in megapixels (Lightroom's limit too); a bigger one is scaled down.
pub const MAX_MEGAPIXELS: f64 = 512.0;

/// Most frames one stitch accepts: pair matching compares every pair of frames (n(n−1)/2 runs), so an
/// unbounded frame count is quadratic CPU as well as quadratic memory while the frames load. A panorama
/// with more than [`MAX_FRAMES`] shots is refused with an error, not attempted.
pub const MAX_FRAMES: usize = 256;

/// The longest edge of a panorama, in pixels (Lightroom's limit too); a longer one is scaled down.
pub const MAX_EDGE: f64 = 65_000.0;

/// The stitched panorama.
#[derive(Clone, Debug)]
pub struct PanoResult {
    /// Scene-linear pixels (same units as the frames, after exposure compensation).
    pub image: Rgb32f,
    /// Coverage (1 = image data, 0 = empty canvas).
    pub alpha: Plane,
    /// Auto-crop rectangle (normalised x, y, w, h), when requested.
    pub crop: Option<[f64; 4]>,
    /// The projection used (never `Auto`).
    pub projection: Projection,
    /// Indices (into the inputs) of the frames in the panorama; the others didn't connect.
    pub used: Vec<usize>,
    /// Cameras of the inputs (`None` = not used), in their full-resolution pixels, straightened.
    pub cameras: Vec<Option<Camera>>,
    /// Per-input exposure gains applied.
    pub gains: Vec<f64>,
    /// The residual vignetting divided out of every frame: `ln V = c₁r² + c₂r⁴ + c₃r⁶` for the radius
    /// `r` from a frame's centre (1 at its corners); `None` when the overlaps didn't give one.
    pub vignette: Option<[f64; 3]>,
    /// Per-input black offsets subtracted after the gain (veiling flare, in output units; 0 for the
    /// frame with the least).
    pub offsets: Vec<f64>,
    /// Bundle adjustment RMS reprojection error (pixels) before/after.
    pub rms_before: f64,
    pub rms: f64,
    /// Horizontal and vertical field of view (degrees).
    pub fov: (f64, f64),
    /// The panorama's size at the photos' own resolution (megapixels), before any limit.
    pub native_megapixels: f64,
    /// Output focal length and the surface coordinates of the canvas's top-left corner: canvas
    /// pixel (x, y) (before boundary warp) shows surface point (x + 0.5 + origin.0, y + 0.5 + origin.1).
    pub focal: f64,
    pub origin: (f64, f64),
    pub color: FrameColor,
    pub metadata: lightcraft_meta::Metadata,
    pub raw: bool,
    pub baseline_exposure: f64,
}

/// Field of view (degrees) of the cameras' union, measured in the spherical projection.
fn fov(frames: &[Frame], cams: &[Option<Camera>]) -> (f64, f64) {
    let (mut tmin, mut tmax, mut pmin, mut pmax) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
    for (f, c) in frames.iter().zip(cams) {
        let Some(c) = c else { continue };
        for (x, y) in border(f.width(), f.height(), 16) {
            if let Some((t, p)) = Projection::Spherical.forward(c.ray(x, y), 1.0) {
                tmin = tmin.min(t);
                tmax = tmax.max(t);
                pmin = pmin.min(p);
                pmax = pmax.max(p);
            }
        }
    }
    ((tmax - tmin).to_degrees(), (pmax - pmin).to_degrees())
}

/// Points along an image's border.
fn border(w: usize, h: usize, per_edge: usize) -> Vec<(f64, f64)> {
    let (w, h) = (w as f64, h as f64);
    let mut v = Vec::new();
    for k in 0..=per_edge {
        let t = k as f64 / per_edge as f64;
        v.extend([(t * w, 0.0), (t * w, h), (0.0, t * h), (w, t * h)]);
    }
    v
}

/// Surface bounding box of every used image in `proj` (output focal `f`).
fn bounds(frames: &[Frame], cams: &[Option<Camera>], proj: Projection, f: f64) -> Option<(f64, f64, f64, f64)> {
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for (fr, c) in frames.iter().zip(cams) {
        let Some(c) = c else { continue };
        for (x, y) in border(fr.width(), fr.height(), 24) {
            let (u, v) = proj.forward(c.ray(x, y), f)?;
            x0 = x0.min(u);
            y0 = y0.min(v);
            x1 = x1.max(u);
            y1 = y1.max(v);
        }
    }
    (x1 > x0 && y1 > y0).then_some((x0, y0, x1, y1))
}

/// Auto projection: perspective when the field of view is moderate and the rectilinear canvas
/// stays compact; cylindrical for wide but not tall panoramas; spherical otherwise.
pub fn choose_projection(frames: &[Frame], cams: &[Option<Camera>]) -> Projection {
    let (h, v) = fov(frames, cams);
    let area: f64 = frames.iter().zip(cams).filter(|(_, c)| c.is_some()).map(|(f, _)| (f.width() * f.height()) as f64).sum();
    let fmed = median(cams.iter().flatten().map(|c| c.f).collect());
    if h < 110.0
        && v < 110.0
        && let Some((x0, y0, x1, y1)) = bounds(frames, cams, Projection::Perspective, fmed)
        && (x1 - x0) * (y1 - y0) < 2.5 * area
    {
        return Projection::Perspective;
    }
    if v < 90.0 { Projection::Cylindrical } else { Projection::Spherical }
}

fn median(mut v: Vec<f64>) -> f64 {
    if v.is_empty() {
        return 1.0;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    v[v.len() / 2]
}

/// One image resampled onto its canvas tile.
struct Tile {
    x0: usize,
    y0: usize,
    w: usize,
    h: usize,
    rgb: Vec<[f32; 3]>,
    /// Feather weight (0 = outside the image).
    wgt: Vec<f32>,
    /// Squared radius from the frame's centre, normalised to 1 at its corners, × 65535 ([`vignette`]).
    r2: Vec<u16>,
}

/// Register the frames: cameras for the connected set.
pub fn register(frames: &[Frame], progress: &Progress) -> Result<(Vec<Option<Camera>>, f64, f64)> {
    let n = frames.len();
    let work: Vec<(crate::features::Features, f64)> = frames
        .par_iter()
        .map(|f| {
            let med = crate::align::median_luma(&f.image, f.clip);
            let (p, s) = crate::align::work_image(&f.image, 0.18 / med.max(1e-6), 1000);
            (crate::features::detect(&p, 1200), s)
        })
        .collect();
    check(progress, 0.2, "Matching")?;
    let feats: Vec<_> = work.iter().map(|(f, _)| f.clone()).collect();
    let scales: Vec<f64> = work.iter().map(|(_, s)| *s).collect();
    let pairs = camera::match_pairs(&feats, &scales);
    let ids = camera::largest_component(n, &pairs);
    if ids.len() < 2 {
        return Err(MergeError::NoOverlap("the photos don't overlap enough to be stitched".into()));
    }
    check(progress, 0.3, "Aligning")?;
    // focal length: EXIF 35 mm equivalent, else the homographies, else ~50° horizontal
    let sizes: Vec<(usize, usize)> = frames.iter().map(|f| (f.width(), f.height())).collect();
    let mut fs = Vec::new();
    for p in &pairs {
        if !(ids.contains(&p.i) && ids.contains(&p.j)) {
            continue;
        }
        let ci = (sizes[p.i].0 as f64 / 2.0, sizes[p.i].1 as f64 / 2.0);
        let cj = (sizes[p.j].0 as f64 / 2.0, sizes[p.j].1 as f64 / 2.0);
        let ti = lightcraft_geom::Homography([1.0, 0.0, ci.0, 0.0, 1.0, ci.1, 0.0, 0.0, 1.0]);
        let tj = lightcraft_geom::Homography([1.0, 0.0, -cj.0, 0.0, 1.0, -cj.1, 0.0, 0.0, 1.0]);
        let hc = tj.mul(&p.h).mul(&ti);
        if let (Some(a), Some(b)) = camera::focals_from_homography(&hc) {
            fs.push((a * b).sqrt());
        }
    }
    let long = sizes[ids[0]].0.max(sizes[ids[0]].1) as f64;
    let exif_f = frames[ids[0]].metadata.focal_length_35mm.filter(|v| *v > 1.0).map(|f35| f35 / 36.0 * long);
    let hom_f = (!fs.is_empty()).then(|| median(fs.clone())).filter(|f| *f > 0.1 * long && *f < 20.0 * long);
    let f0 = exif_f.or(hom_f).unwrap_or(long * 1.07);
    let cams = camera::initial_cameras(&ids, &sizes, &pairs, f0);
    let anchor = ids.iter().copied().find(|&i| cams[i].is_some()).unwrap_or(ids[0]);
    check(progress, 0.35, "Aligning")?;
    let adj = camera::bundle_adjust(&cams, &pairs, anchor, 60);
    let mut cams = adj.cameras;
    camera::straighten(&mut cams);
    Ok((cams, adj.rms_before, adj.rms))
}

/// Stitch `frames` (oriented, linear) into a panorama.
pub fn stitch(mut frames: Vec<Frame>, opts: &PanoOptions, progress: &Progress) -> Result<PanoResult> {
    let n = frames.len();
    if n < 2 {
        return Err(MergeError::TooFew(n, 2));
    }
    if n > MAX_FRAMES {
        return Err(MergeError::TooMany(n, MAX_FRAMES));
    }
    check(progress, 0.01, "Finding features")?;
    let (cams, rms_before, rms) = register(&frames, progress)?;
    let used: Vec<usize> = (0..n).filter(|&i| cams[i].is_some()).collect();
    let fov = fov(&frames, &cams);
    let projection = match opts.projection {
        Projection::Auto => choose_projection(&frames, &cams),
        p => p,
    };
    let fmed = median(used.iter().filter_map(|&i| cams[i].map(|c| c.f)).collect());
    let Some((bx0, by0, bx1, by1)) = bounds(&frames, &cams, projection, fmed) else {
        return Err(MergeError::NoOverlap(format!("the field of view ({:.0}°) is too wide for a perspective projection", fov.0)));
    };
    let area: f64 = used.iter().map(|&i| (frames[i].width() * frames[i].height()) as f64).sum();
    if projection == Projection::Perspective && (bx1 - bx0) * (by1 - by0) > 12.0 * area {
        return Err(MergeError::NoOverlap(format!("the field of view ({:.0}°) is too wide for a perspective projection", fov.0)));
    }
    // output scale: native resolution, limited to max_megapixels and the longest edge
    let max_px = if opts.max_megapixels.is_nan() { MAX_MEGAPIXELS } else { opts.max_megapixels.clamp(0.1, MAX_MEGAPIXELS) } * 1e6;
    let s = (max_px / ((bx1 - bx0) * (by1 - by0))).sqrt().min(MAX_EDGE / (bx1 - bx0).max(by1 - by0)).min(1.0);
    let f_out = fmed * s;
    let (ox, oy) = (bx0 * s, by0 * s);
    let (cw, ch) = (((bx1 - bx0) * s).ceil() as usize + 1, ((by1 - by0) * s).ceil() as usize + 1);
    let levels = ((cw.min(ch) as f64).log2() - 3.0).floor().clamp(1.0, 7.0) as usize;
    let unit = 1usize << levels;
    let (pw, ph) = (cw.div_ceil(unit) * unit, ch.div_ceil(unit) * unit);
    check(progress, 0.45, "Projecting")?;

    // exposure from EXIF relative to the first used frame
    let ev0 = frames[used[0]].exposure;
    let mut gains: Vec<f64> = (0..n)
        .map(|i| match (frames[i].exposure, ev0) {
            (Some(e), Some(e0)) => 2f64.powf(e0 - e),
            _ => 1.0,
        })
        .collect();

    // resample every used frame onto its tile
    let tiles: Vec<Option<Tile>> = (0..n)
        .into_par_iter()
        .map(|i| {
            let c = cams[i]?;
            let fr = &frames[i];
            let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
            for (x, y) in border(fr.width(), fr.height(), 32) {
                let (u, v) = projection.forward(c.ray(x, y), f_out)?;
                x0 = x0.min(u - ox);
                y0 = y0.min(v - oy);
                x1 = x1.max(u - ox);
                y1 = y1.max(v - oy);
            }
            let pad = 2.0 * unit as f64;
            let tx0 = (((x0 - pad).max(0.0) as usize) / unit) * unit;
            let ty0 = (((y0 - pad).max(0.0) as usize) / unit) * unit;
            let tx1 = (((x1 + pad).max(0.0) as usize).min(pw)).div_ceil(unit) * unit;
            let ty1 = (((y1 + pad).max(0.0) as usize).min(ph)).div_ceil(unit) * unit;
            let (tw, th) = (tx1.min(pw).saturating_sub(tx0), ty1.min(ph).saturating_sub(ty0));
            if tw == 0 || th == 0 {
                return None;
            }
            let (fw, fh) = (fr.width() as f64, fr.height() as f64);
            let mut rgb = vec![[0f32; 3]; tw * th];
            let mut wgt = vec![0f32; tw * th];
            let mut r2 = vec![0u16; tw * th];
            let diag2 = fw * fw + fh * fh;
            rgb.par_chunks_mut(tw).zip(wgt.par_chunks_mut(tw)).zip(r2.par_chunks_mut(tw)).enumerate().for_each(|(ty, ((row, wrow), rrow))| {
                let v = (ty0 + ty) as f64 + 0.5 + oy;
                for tx in 0..tw {
                    let u = (tx0 + tx) as f64 + 0.5 + ox;
                    let d = projection.inverse(u, v, f_out);
                    let Some((px, py)) = c.project(d) else { continue };
                    if px < 0.0 || py < 0.0 || px > fw || py > fh {
                        continue;
                    }
                    let wx = 1.0 - (2.0 * px / fw - 1.0).abs();
                    let wy = 1.0 - (2.0 * py / fh - 1.0).abs();
                    let w = (wx * wy).max(1e-6) as f32;
                    row[tx] = fr.image.sample_lanczos3(px as f32, py as f32);
                    wrow[tx] = w;
                    let (dx, dy) = (2.0 * px - fw, 2.0 * py - fh);
                    rrow[tx] = ((dx * dx + dy * dy) / diag2 * u16::MAX as f64).round().clamp(0.0, u16::MAX as f64) as u16;
                }
            });
            Some(Tile { x0: tx0, y0: ty0, w: tw, h: th, rgb, wgt, r2 })
        })
        .collect();
    check(progress, 0.6, "Exposure compensation")?;
    // the vignetting the frames' own correction left (shared by all, fitted from the overlaps), divided out first
    let mut tiles = tiles;
    let clips: Vec<f32> = frames.iter().map(|f| f.clip).collect();
    let vignette = vignette::estimate(&tiles, &clips);
    if let Some(c) = &vignette {
        vignette::correct(&mut tiles, c);
    }
    let offsets = compensate_exposure(&tiles, &mut gains, &frames, vignette.as_ref());
    // the tiles hold every pixel the blend needs: the frames' own go now, before the blend
    // takes its memory (12 bytes a pixel of every photo: 3 GB for five 51 MP frames)
    for f in &mut frames {
        f.image = Rgb32f::new(0, 0);
    }

    // seams: label = max feather weight
    let mut best = vec![0f32; pw * ph];
    let mut label = vec![u16::MAX; pw * ph];
    for (i, t) in tiles.iter().enumerate() {
        let Some(t) = t else { continue };
        for ty in 0..t.h {
            let row = (t.y0 + ty) * pw + t.x0;
            for tx in 0..t.w {
                let w = t.wgt[ty * t.w + tx];
                if w > best[row + tx] {
                    best[row + tx] = w;
                    label[row + tx] = i as u16;
                }
            }
        }
    }
    check(progress, 0.7, "Blending")?;
    // each frame as the blend takes it: gain, then the flare offset in the camera's white (flare is the light's colour,
    // not grey in camera RGB: equal R, G and B pushed a channel of dark pixels to zero, a pink seam), never more than
    // half a pixel's value
    let neutral: Vec<[f32; 3]> = frames
        .iter()
        .map(|f| {
            let n = match &f.color {
                FrameColor::Camera(c) => c.as_shot_neutral.unwrap_or([1.0; 3]).map(|v| v as f32),
                FrameColor::Linear { .. } => [1.0; 3],
            };
            let l = 0.25 * n[0] + 0.6 * n[1] + 0.15 * n[2];
            if l > 0.0 && n.iter().all(|v| v.is_finite() && *v > 0.0) { n.map(|v| v / l) } else { [1.0; 3] }
        })
        .collect();
    let compensate = |i: usize, p: [f32; 3]| -> [f32; 3] {
        compensated(
            p,
            gains.get(i).copied().unwrap_or(1.0) as f32,
            offsets.get(i).copied().unwrap_or(0.0) as f32,
            neutral.get(i).copied().unwrap_or([1.0; 3]),
        )
    };
    // what one gain and offset per frame leave at the seams (the sky's gradient, flare that grows towards the sun)
    let leveling = level::level(&tiles, &label, pw, ph, compensate);
    let mut blender = Blender::new(pw, ph, levels);
    for (i, t) in tiles.into_iter().enumerate() {
        let Some(t) = t else { continue };
        let mut vals = Buf::new(t.w, t.h, 3);
        let mut mask = Buf::new(t.w, t.h, 1);
        for ty in 0..t.h {
            for tx in 0..t.w {
                let k = ty * t.w + tx;
                if t.wgt[k] > 0.0 {
                    let v = compensate(i, t.rgb[k]);
                    let u = leveling.at(i, t.x0 + tx, t.y0 + ty);
                    for c in 0..3 {
                        vals.data[k * 3 + c] = v[c].max(1e-6).ln() + u[c];
                    }
                }
                if label[(t.y0 + ty) * pw + t.x0 + tx] == i as u16 {
                    mask.data[k] = 1.0;
                }
            }
        }
        push_pull(&mut vals, &t.wgt);
        blender.add(t.x0, t.y0, vals, mask);
        check(progress, 0.7 + 0.2 * (i + 1) as f32 / n as f32, "Blending")?;
    }
    let out = blender.finish();
    let mut image = Rgb32f::new(cw, ch);
    let mut alpha = Plane::new(cw, ch);
    for y in 0..ch {
        for x in 0..cw {
            let k = y * pw + x;
            if label[k] != u16::MAX {
                image.data[y * cw + x] = [0, 1, 2].map(|c| out.data[k * 3 + c].exp());
                alpha.data[y * cw + x] = 1.0;
            }
        }
    }
    check(progress, 0.92, "Finishing")?;
    let (mut image, alpha) = finish::boundary_warp(&image, &alpha, opts.boundary_warp / 100.0);
    let crop = if opts.auto_crop {
        finish::auto_crop(&alpha).map(|(x, y, w, h)| [x as f64 / cw as f64, y as f64 / ch as f64, w as f64 / cw as f64, h as f64 / ch as f64])
    } else {
        None
    };
    if opts.fill_edges {
        image = finish::fill_edges(&image, &alpha);
    }
    let r = &frames[used[0]];
    // the panorama's white balance is the median of the frames that are in it, not whichever frame came first
    let mut color = r.color.clone();
    if let Some(multipliers) = consensus_multipliers(used.iter().filter_map(|&i| frames.get(i))) {
        color.set_as_shot_multipliers(multipliers);
    }
    Ok(PanoResult {
        image,
        alpha,
        crop,
        projection,
        used,
        cameras: cams,
        gains,
        vignette,
        offsets,
        rms_before,
        rms,
        fov,
        native_megapixels: (bx1 - bx0) * (by1 - by0) / 1e6,
        focal: f_out,
        origin: (ox, oy),
        color,
        metadata: r.metadata.clone(),
        raw: r.raw,
        baseline_exposure: r.baseline_exposure,
    })
}

/// A frame's pixel `p` with its gain `g` and flare offset `o` taken out: the offset in the colour of the camera's white
/// `n` (luminance 1), never more than half of any channel, so the darkest pixels keep their colour.
fn compensated(p: [f32; 3], g: f32, o: f32, n: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|c| {
        let v = p[c] * g;
        (v - o * n[c]).max(0.5 * v)
    })
}

/// Brightness bins each overlap's paired pixels are averaged into.
const EXPOSURE_BINS: usize = 8;

/// Exposure compensation: a gain `gᵢ` and a black offset `oᵢ` per image, `v' = gᵢ·v − oᵢ`.
///
/// In every overlap the pixels both images see (unclipped) are paired, sorted by brightness and
/// averaged in [`EXPOSURE_BINS`] bins of equal count: pairs of levels `(aₖ, bₖ)` from shadows to
/// highlights. Minimised, in units of the mean overlap level `M`:
/// Σ wₖ (gᵢ aₖ − oᵢ − gⱼ bₖ + oⱼ)² / σN² + Σ Wᵢ [ (gᵢ/g⁰ᵢ − 1)² / σg² + oᵢ² / σo² ]
/// (σN = 0.1; σg = 1 and σo = 0.2, weak priors that only fix the overall level and offset; `g⁰`
/// the EXIF-based gains). Brown & Lowe's gain compensation matched the overlaps' means only: a frame
/// facing the sun records the scene plus veiling flare, a pure gain lines up its midtones and leaves
/// its shadows lifted, a visible step at the seams. Offsets are then made relative to the frame
/// with the least (flare only ever adds light), so only the extra is taken out. Returns the offsets
/// (in output units); on failure the EXIF gains stay and every offset is 0. `vignette_c` is the correction the
/// tiles already carry ([`vignette::correct`]), so clipping is tested on the values the frames recorded.
fn compensate_exposure(tiles: &[Option<Tile>], gains: &mut [f64], frames: &[Frame], vignette_c: Option<&[f64; 3]>) -> Vec<f64> {
    let n = tiles.len();
    let none = vec![0f64; n];
    let lum = |p: [f32; 3]| 0.25 * p[0] as f64 + 0.6 * p[1] as f64 + 0.15 * p[2] as f64;
    // (i, j, aₖ, bₖ, weight) for i < j
    let mut pairs: Vec<(usize, usize, f64, f64, f64)> = Vec::new();
    for i in 0..n {
        let Some(a) = &tiles[i] else { continue };
        for j in i + 1..n {
            let Some(b) = &tiles[j] else { continue };
            let (x0, y0) = (a.x0.max(b.x0), a.y0.max(b.y0));
            let (x1, y1) = ((a.x0 + a.w).min(b.x0 + b.w), (a.y0 + a.h).min(b.y0 + b.h));
            let mut v: Vec<(f64, f64)> = Vec::new();
            for y in (y0..y1).step_by(2) {
                for x in (x0..x1).step_by(2) {
                    let ka = (y - a.y0) * a.w + x - a.x0;
                    let kb = (y - b.y0) * b.w + x - b.x0;
                    let (Some(&wa), Some(&wb), Some(&pa), Some(&pb)) = (a.wgt.get(ka), b.wgt.get(kb), a.rgb.get(ka), b.rgb.get(kb)) else { continue };
                    // clipped is judged on the values the frame recorded: the vignetting correction scaled them by `f`
                    let (fa, fb) = vignette_c.map_or((1.0, 1.0), |c| {
                        (vignette::factor(c, a.r2.get(ka).copied().unwrap_or(0)), vignette::factor(c, b.r2.get(kb).copied().unwrap_or(0)))
                    });
                    if wa > 0.0
                        && wb > 0.0
                        && pa.iter().all(|v| *v < frames[i].clip * 0.95 * fa)
                        && pb.iter().all(|v| *v < frames[j].clip * 0.95 * fb)
                    {
                        v.push((lum(pa), lum(pb)));
                    }
                }
            }
            if v.len() < 50 * EXPOSURE_BINS {
                continue;
            }
            v.sort_by(|p, q| (p.0 + p.1).total_cmp(&(q.0 + q.1)));
            let per = v.len() / EXPOSURE_BINS;
            for bin in v.chunks(per).take(EXPOSURE_BINS) {
                let k = bin.len() as f64;
                let (sa, sb) = bin.iter().fold((0.0, 0.0), |(x, y), p| (x + p.0, y + p.1));
                pairs.push((i, j, sa / k, sb / k, k));
            }
        }
    }
    if std::env::var_os("LIGHTCRAFT_PROFILE").is_some() {
        for &(i, j, a, b, w) in &pairs {
            eprintln!("[profile] overlap {i}-{j}: levels {a:.5} / {b:.5} ({w:.0} px)");
        }
    }
    let total: f64 = pairs.iter().map(|p| p.4).sum();
    let m = pairs.iter().map(|&(i, j, a, b, w)| w * (a * gains[i] + b * gains[j]) / 2.0).sum::<f64>() / total.max(1.0);
    if pairs.is_empty() || !(m > 0.0 && m.is_finite()) {
        return none;
    }
    // unknowns: g₀…gₙ₋₁, o₀…oₙ₋₁ (offsets in units of m)
    let (sn, sg, so) = (0.1f64, 1.0f64, 0.2f64);
    let dim = 2 * n;
    let g0: Vec<f64> = gains.to_vec();
    let mut ata = vec![0f64; dim * dim];
    let mut atb = vec![0f64; dim];
    let mut add = |coef: &[(usize, f64)], rhs: f64, w: f64| {
        for &(r, cr) in coef {
            for &(c, cc) in coef {
                ata[r * dim + c] += w * cr * cc;
            }
            atb[r] += w * cr * rhs;
        }
    };
    let mut seen = vec![0f64; n];
    for &(i, j, a, b, w) in &pairs {
        add(&[(i, a / m), (n + i, -1.0), (j, -b / m), (n + j, 1.0)], 0.0, w / (sn * sn));
        seen[i] += w;
        seen[j] += w;
    }
    for i in 0..n {
        // images in no overlap keep their EXIF gain and no offset
        let wp = if seen[i] > 0.0 { seen[i] } else { 1.0 };
        let g = if g0[i] > 0.0 { g0[i] } else { 1.0 };
        add(&[(i, 1.0 / (sg * g))], 1.0 / sg, wp);
        add(&[(n + i, 1.0 / so)], 0.0, wp);
    }
    let Some(x) = linalg::solve(ata, atb, dim) else { return none };
    let (g, o) = x.split_at(n);
    if !g.iter().zip(&seen).all(|(v, s)| v.is_finite() && (*v > 0.0 || *s == 0.0)) || !o.iter().all(|v| v.is_finite()) {
        return none;
    }
    gains.copy_from_slice(g);
    // relative to the frame with the least flare, in output units
    let least = o.iter().zip(&seen).filter(|(_, s)| **s > 0.0).map(|(v, _)| *v).fold(f64::INFINITY, f64::min);
    let least = if least.is_finite() { least } else { 0.0 };
    o.iter().zip(&seen).map(|(v, s)| if *s > 0.0 { ((v - least) * m).max(0.0) } else { 0.0 }).collect()
}

#[cfg(test)]
mod exposure_tests {
    use super::*;

    fn tile(x0: usize, f: impl Fn(usize) -> f32) -> Tile {
        let (w, h) = (200usize, 100usize);
        Tile { x0, y0: 0, w, h, rgb: (0..w * h).map(|k| [f(x0 + k % w); 3]).collect(), wgt: vec![1.0; w * h], r2: vec![0; w * h] }
    }

    /// The second frame records the scene plus veiling flare (light added, not multiplied): the offset is found for it
    /// and none for the first, and taking both out lines up the shadows as well as the highlights.
    #[test]
    fn flare_is_an_offset_not_a_gain() {
        let scene = |x: usize| 0.002 + 0.3 * ((x as f32 * 0.07).sin() * 0.5 + 0.5).powi(3);
        let (a, b) = (tile(0, scene), tile(100, |x| 1.2 * scene(x) + 0.01));
        let mut frame = Frame::from_linear_rec2020(Rgb32f::new(1, 1), None);
        frame.clip = f32::INFINITY;
        let frames = vec![frame.clone(), frame];
        let mut gains = vec![1.0, 1.0];
        let offsets = compensate_exposure(&[Some(a), Some(b)], &mut gains, &frames, None);
        assert!(offsets[0].abs() < 1e-4 && (offsets[1] - 0.01 * gains[1]).abs() < 2e-3, "{offsets:?} {gains:?}");
        // where the flare is less than half the pixel (deeper in the shadows at most half is taken out, on purpose)
        for x in (100..200).filter(|&x| 0.01 < 0.5 * (1.2 * scene(x) + 0.01)) {
            let (pa, pb) = (scene(x) * gains[0] as f32, compensated([1.2 * scene(x) + 0.01; 3], gains[1] as f32, offsets[1] as f32, [1.0; 3])[0]);
            assert!((pa - pb).abs() < 0.1 * pa + 2e-3, "x {x}: {pa} {pb}");
        }
    }

    /// Frames whose edges the vignetting fit darkens (here by 0.8 everywhere, both frames at the same radius): the
    /// first clipped its highlights, the second didn't. Corrected, the clipped pixels sit at 0.8 of the clip level,
    /// under the test's 0.95: compared there they would pair a clipped frame with a bright one and pull the gains
    /// apart. Clipping is judged on the values the frame recorded, so the gains stay equal.
    #[test]
    fn clipping_is_judged_before_the_vignetting_correction() {
        let c = [1.25f64.ln(), 0.0, 0.0];
        let scene = |x: usize| 0.05 + 1.2 * ((x as f32 * 0.05).sin() * 0.5 + 0.5);
        let recorded = |clip: f32| move |x: usize| (1.25 * scene(x)).min(clip);
        let (mut a, mut b) = (tile(0, recorded(1.0)), tile(100, recorded(f32::INFINITY)));
        a.r2.fill(u16::MAX);
        b.r2.fill(u16::MAX);
        let mut tiles = [Some(a), Some(b)];
        vignette::correct(&mut tiles, &c);
        let mut frames = vec![Frame::from_linear_rec2020(Rgb32f::new(1, 1), None); 2];
        frames[0].clip = 1.0;
        frames[1].clip = f32::INFINITY;
        let mut gains = vec![1.0, 1.0];
        let offsets = compensate_exposure(&tiles, &mut gains, &frames, Some(&c));
        assert!((gains[0] / gains[1] - 1.0).abs() < 0.01, "{gains:?} {offsets:?}");
        assert!(offsets.iter().all(|o| o.abs() < 1e-3), "{gains:?} {offsets:?}");
    }

    /// An offset larger than a dark pixel never empties a channel (that left a pink line along a seam): it takes at
    /// most half, in the camera white's colour.
    #[test]
    fn a_large_offset_keeps_dark_pixels_coloured() {
        let p = compensated([0.004, 0.01, 0.006], 1.0, 0.05, [0.5, 1.0, 0.7]);
        assert_eq!(p, [0.002, 0.005, 0.003]);
        assert!(compensated([0.0; 3], 1.0, 0.05, [1.0; 3]).iter().all(|v| *v == 0.0));
    }
}
