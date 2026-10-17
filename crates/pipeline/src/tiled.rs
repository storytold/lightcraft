//! Tiled rendering: a large output (a full-size export) rendered tile by tile, so the working
//! memory is set by the tile size instead of growing with the output. The whole-image pipeline
//! holds ~60 bytes per output pixel at its peak; tiled, it holds the output, a few small
//! whole-image grids and one tile's working set.
//!
//! The result is [`crate::render`]'s, within float rounding. Every stage is one of:
//! - per pixel (white balance, red eye, the finishing stage, most masks): rendered per tile;
//! - a filter of bounded reach (noise reduction, defringe, the texture / chroma / dehaze blurs,
//!   small guided filters, the sky mask's smoothing): each tile is rendered over its region grown
//!   by a halo covering the filters' combined reach, and only its core is kept;
//! - global (the large guided filters of the highlights/shadows base and clarity, which work on a
//!   subsampled grid of the whole image, and the dehaze airlight, a percentile of the whole dark
//!   channel): a first pass over all tiles gathers them, the second applies them per pixel.
//!
//! Stages that read arbitrarily far (spot removal, Refine Edges, Auto Mask brushes, the subject
//! masks), renders with a prefilter (downscaled) or an overlay aren't tiled: [`render_tiled`]
//! declines them and the caller renders whole.

use std::sync::Arc;

use lightcraft_develop::{DevelopSettings, MaskShape};
use lightcraft_raster::blur::{box_radii, gaussian};
use lightcraft_raster::resample::{Filter, weights};
use lightcraft_raster::{Histogram, Plane, Rgb32f, Rgba8};

use crate::local::{self, BASE_EPS, CLARITY_EPS, dark_of, guided_fast, guided_fast_step, log_lum};
use crate::{DeepImage, DeepSamples, OutputDepth, Overlay, Plan, Prepared, RenderRequest, Rendered, SourceInfo, finish, masks, optics, redeye};

/// Pixels of a tile's region (core and halo) [`TileOptions::default`] aims for.
pub const REGION_PX: usize = 2_000_000;

/// Peak working bytes per region pixel (the sampled image, its planes, noise reduction's and
/// the blurs' temporaries, masks).
pub const BYTES_PER_REGION_PX: usize = 80;

/// Tile sizes of a [`render_tiled`].
#[derive(Clone, Copy, Debug)]
pub struct TileOptions {
    /// Pixels of a tile's region (its core plus the halo around it) to aim for. Halos wider than
    /// this allows grow the region: a tile's core is never narrower than its halo.
    pub region_px: usize,
    /// A fixed core edge (px) instead (tests).
    pub core: Option<usize>,
}

impl Default for TileOptions {
    fn default() -> Self {
        TileOptions { region_px: REGION_PX, core: None }
    }
}

impl TileOptions {
    /// Working bytes of a tiled render with these options, beyond the source and the output.
    pub fn working_bytes(&self) -> usize {
        self.region_px * BYTES_PER_REGION_PX
    }
}

/// Whether `s` can be rendered tiled for `req` as far as the settings go (the source and output
/// size can still rule it out: see [`render_tiled`]).
pub fn supported(s: &DevelopSettings, req: &RenderRequest) -> bool {
    if req.overlay != Overlay::None || !s.spots.is_empty() {
        return false;
    }
    s.masks.iter().filter(|m| m.visible && !m.components.is_empty()).all(|m| {
        m.refine <= 0.0
            && m.components.iter().all(|c| match &c.shape {
                MaskShape::Brush { strokes } => !strokes.iter().any(|st| st.auto_mask),
                MaskShape::Subject | MaskShape::Background | MaskShape::Object { .. } | MaskShape::People { .. } => false,
                _ => true,
            })
    })
}

/// Reach (px) of a Gaussian of `sigma` ([`gaussian`]: three box passes).
fn reach(sigma: f32) -> usize {
    if sigma <= 0.3 { 0 } else { box_radii(sigma).iter().sum() }
}

/// A rectangle of the output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x0: usize,
    pub y0: usize,
    pub w: usize,
    pub h: usize,
}

/// The highlights/shadows base or clarity plane when its guided filter runs on a subsampled grid
/// ([`guided_fast`]): the grid of the whole image is gathered by pass A, its coefficients are
/// upsampled per tile by pass B. Same taps and summation order as `guided_fast`.
struct LoGrid {
    sigma: f32,
    eps: f32,
    step: usize,
    lw: usize,
    lh: usize,
    /// Box taps whole → grid, per grid column / row.
    down_x: Vec<(usize, Vec<f32>)>,
    down_y: Vec<(usize, Vec<f32>)>,
    acc: Vec<f64>,
    /// After pass A: the blurred coefficients on the grid, and the bilinear taps grid → whole.
    coeffs: Option<(Plane, Plane)>,
    up_x: Vec<(usize, Vec<f32>)>,
    up_y: Vec<(usize, Vec<f32>)>,
}

impl LoGrid {
    fn new(sigma: f32, eps: f32, gw: usize, gh: usize) -> Option<LoGrid> {
        let step = guided_fast_step(sigma);
        if step <= 1 {
            return None;
        }
        let (lw, lh) = (gw.div_ceil(step).max(1), gh.div_ceil(step).max(1));
        Some(LoGrid {
            sigma,
            eps,
            step,
            lw,
            lh,
            down_x: weights(gw, lw, Filter::Box),
            down_y: weights(gh, lh, Filter::Box),
            acc: vec![0.0; lw * lh],
            coeffs: None,
            up_x: Vec::new(),
            up_y: Vec::new(),
        })
    }

    /// Add the core `r` of log luminance `l` (whose top-left output pixel is `at`) to the grid.
    fn add(&mut self, l: &Plane, at: (usize, usize), r: Rect) {
        let (x1, y1) = (r.x0 + r.w, r.y0 + r.h);
        let overlapping = |taps: &[(usize, Vec<f32>)], a: usize, b: usize| {
            let first = taps.partition_point(|(lo, w)| lo + w.len() <= a);
            let last = taps.partition_point(|(lo, _)| *lo < b);
            first..last
        };
        let cols = overlapping(&self.down_x, r.x0, x1);
        let rows = overlapping(&self.down_y, r.y0, y1);
        // horizontal taps within the core, per core row
        let nc = cols.len();
        let mut hsum = vec![0f64; r.h * nc];
        for y in 0..r.h {
            let src = l.row(r.y0 + y - at.1);
            for (k, i) in cols.clone().enumerate() {
                let (lo, w) = &self.down_x[i];
                let mut acc = 0f64;
                for (t, wt) in w.iter().enumerate() {
                    let x = lo + t;
                    if x >= r.x0 && x < x1 {
                        acc += *wt as f64 * src[x - at.0] as f64;
                    }
                }
                hsum[y * nc + k] = acc;
            }
        }
        for j in rows {
            let (lo, w) = &self.down_y[j];
            for (t, wt) in w.iter().enumerate() {
                let y = lo + t;
                if y < r.y0 || y >= y1 {
                    continue;
                }
                let hrow = &hsum[(y - r.y0) * nc..(y - r.y0 + 1) * nc];
                for (k, i) in cols.clone().enumerate() {
                    self.acc[j * self.lw + i] += *wt as f64 * hrow[k];
                }
            }
        }
    }

    /// The grid is complete: compute the coefficients.
    fn solve(&mut self, gw: usize, gh: usize) {
        let lo = Plane { width: self.lw, height: self.lh, data: self.acc.iter().map(|v| *v as f32).collect() };
        self.acc = Vec::new();
        self.coeffs = Some(local::guided_coeffs(&lo, self.sigma / self.step as f32, self.eps));
        self.up_x = weights(self.lw, gw, Filter::Bilinear);
        self.up_y = weights(self.lh, gh, Filter::Bilinear);
    }
}

/// A tiled render planned: the tiles, their halos and the whole-image values both passes need.
/// [`render_tiled`] renders it on the CPU; the GPU renderer (`lightcraft-gpu`) drives the same
/// plan with its own kernels, so both tile alike.
pub struct TilePlan<'a> {
    /// The whole render's plan (output size `gw × gh`).
    pub plan: Plan<'a>,
    pub gw: usize,
    pub gh: usize,
    pub sig: local::PlaneSigmas,
    /// Halo (px) around a tile's core: what the scene-linear stage reads around a pixel, and
    /// what the planes and masks read around one on top of that.
    pub halo_lin: usize,
    pub halo_planes: usize,
    /// The tiles' cores, row by row; they cover the output once.
    pub cores: Vec<Rect>,
    /// Edge of a (full) core.
    core: usize,
    /// Highlights/shadows base (0) and clarity (1) planes filtered on a grid of the whole image.
    grids: [Option<LoGrid>; 2],
    air_samples: Vec<f32>,
    /// The dehaze airlight (after [`TilePlan::solve`]; 1 without dehaze).
    pub air: f32,
}

impl<'a> TilePlan<'a> {
    /// Plan `src` rendered with `s` for `req` in tiles of about `opts`. `None` when it can't be
    /// tiled ([`supported`], or a prefilter: a downscaled render).
    pub fn new(src: &Rgb32f, info: &SourceInfo, s: &'a DevelopSettings, req: &RenderRequest, opts: &TileOptions) -> Option<TilePlan<'a>> {
        let plan = crate::plan(src, info, s, req);
        let (gw, gh) = (plan.w, plan.h);
        if !supported(&plan.settings, req) || plan.frame.sample_plan(src.width, src.height, gw, gh).prefilter.is_some() {
            return None;
        }
        let s = &*plan.settings;
        let ppl = plan.px_per_long;
        let sig = local::plane_sigmas(s, ppl, req.quality);
        // halos: the scene-linear stage (defringe, then noise reduction: a guided filter of
        // luminance, two Gaussians deep, then a chroma blur), and the planes and masks from it
        let (nr_l, nr_c) = local::nr_params(s, plan.detail_scale);
        let halo_lin =
            optics::defringe_reach(s, ppl / optics::DEFRINGE_REF_LONG) + nr_l.map_or(0, |n| 2 * reach(n.sigma)) + nr_c.map_or(0, |n| reach(n.sigma));
        let grids: [Option<LoGrid>; 2] =
            [sig.base.and_then(|g| LoGrid::new(g, BASE_EPS, gw, gh)), sig.clarity.and_then(|g| LoGrid::new(g, CLARITY_EPS, gw, gh))];
        // a guided filter too small to subsample runs on the region: two Gaussians deep
        let guided_reach = |sigma: Option<f32>, grid: &Option<LoGrid>| match (sigma, grid) {
            (Some(g), None) => 2 * reach(g),
            _ => 0,
        };
        let sky = s.masks.iter().filter(|m| m.visible).flat_map(|m| &m.components).any(|c| matches!(c.shape, MaskShape::Sky));
        let halo_planes = [
            sig.texture.map_or(0, reach),
            sig.chroma.map_or(0, reach),
            sig.dark.map_or(0, reach),
            // (process V2's base filters the median of log luminance: its radius on top)
            guided_reach(sig.base, &grids[0]) + if sig.base.is_some() { sig.base_detail } else { 0 },
            guided_reach(sig.clarity, &grids[1]),
            if sky { reach((0.01 * ppl as f32).max(0.5)) } else { 0 },
        ]
        .into_iter()
        .max()
        .unwrap_or(0);
        let halo = halo_lin + halo_planes;
        let core = opts.core.unwrap_or_else(|| ((opts.region_px as f64).sqrt() as usize).saturating_sub(2 * halo).max(halo).max(256)).max(1);
        let cores: Vec<Rect> = (0..gh)
            .step_by(core)
            .flat_map(|y0| (0..gw).step_by(core).map(move |x0| Rect { x0, y0, w: core.min(gw - x0), h: core.min(gh - y0) }))
            .collect();
        if crate::profiling() {
            eprintln!("  tiled: {gw}×{gh} in {} tiles of {core} px, halo {halo_lin} + {halo_planes} px", cores.len());
        }
        let air = 1.0;
        Some(TilePlan { plan, gw, gh, sig, halo_lin, halo_planes, cores, core, grids, air_samples: Vec::new(), air })
    }

    /// The region of core `c`: the core grown by the halo, within the output. Every region has
    /// the same size (a full core and its halo, or the whole output's width / height): at the
    /// output's edges a region is moved inwards rather than cut, so it has a wider halo on its
    /// inner side. (Equal sizes let the GPU reuse its buffers tile after tile.)
    pub fn region(&self, c: Rect) -> Rect {
        let h = self.halo_lin + self.halo_planes;
        let (rw, rh) = ((self.core + 2 * h).min(self.gw), (self.core + 2 * h).min(self.gh));
        let x0 = c.x0.saturating_sub(h).min(self.gw - rw);
        let y0 = c.y0.saturating_sub(h).min(self.gh - rh);
        Rect { x0, y0, w: rw, h: rh }
    }

    /// The frame of region `r` (a tile of the whole output).
    pub fn frame_of(&self, r: Rect) -> crate::geometry::Frame {
        self.plan.frame.for_tile(r.x0, r.y0, self.gw, self.gh)
    }

    /// Whether a first pass ([`TilePlan::gather`] over every core, then [`TilePlan::solve`])
    /// must gather whole-image values before tiles can be finished.
    pub fn needs_gather(&self) -> bool {
        self.grids.iter().any(Option::is_some) || self.sig.dark.is_some()
    }

    /// Pass A, core `c` (in region `r`): its log luminance, what the highlights/shadows base
    /// filters (`base_l`: process V2's median of it, [`local::median_hv`]; `None`: the log
    /// luminance itself) and, with dehaze, its blurred dark channel (all over `r`, exact within
    /// the core).
    pub fn gather(&mut self, c: Rect, r: Rect, log_l: &Plane, base_l: Option<&Plane>, dark: Option<&Plane>) {
        let [base, clarity] = &mut self.grids;
        if let Some(g) = base {
            g.add(base_l.unwrap_or(log_l), (r.x0, r.y0), c);
        }
        if let Some(g) = clarity {
            g.add(log_l, (r.x0, r.y0), c);
        }
        if let Some(d) = dark {
            let gw = self.gw;
            for y in c.y0..c.y0 + c.h {
                // every AIRLIGHT_STEP-th value of the whole dark channel, row-major
                let first = (y * gw + c.x0).next_multiple_of(local::AIRLIGHT_STEP);
                for i in (first..y * gw + c.x0 + c.w).step_by(local::AIRLIGHT_STEP) {
                    self.air_samples.push(d.data[(y - r.y0) * r.w + (i - y * gw) - r.x0]);
                }
            }
        }
    }

    /// End of pass A: the grids' coefficients and the airlight.
    pub fn solve(&mut self) {
        let (gw, gh) = (self.gw, self.gh);
        for g in self.grids.iter_mut().flatten() {
            g.solve(gw, gh);
        }
        if self.sig.dark.is_some() {
            self.air = local::airlight_of(std::mem::take(&mut self.air_samples));
        }
    }

    /// The base (`clarity` false) or clarity plane when it comes from a whole-image grid: the
    /// grid's coefficients and taps (see [`GridView`]). `None`: none, or computed per region.
    pub fn grid(&self, clarity: bool) -> Option<GridView<'_>> {
        let g = self.grids[clarity as usize].as_ref()?;
        let (a, b) = g.coeffs.as_ref()?;
        Some(GridView { a, b, up_x: &g.up_x, up_y: &g.up_y })
    }

    /// The scene-linear image over region `r`, as `render` computes it (white balance, defringe,
    /// red eye, noise reduction): exact within `halo_lin` of the region's inner edges.
    fn lin(&self, src: &Rgb32f, info: &SourceInfo, r: Rect) -> Rgb32f {
        let (s, plan) = (&*self.plan.settings, &self.plan);
        let mut img = self.frame_of(r).sample(src, r.w, r.h);
        local::white_balance(&mut img, info, s);
        optics::defringe(&mut img, s, plan.px_per_long / optics::DEFRINGE_REF_LONG);
        redeye::apply_at(&mut img, &plan.eyes, r.x0, r.y0);
        local::denoise(&mut img, s, plan.detail_scale);
        img
    }
}

/// A whole-image guided filter's blurred coefficients on its subsampled grid, and the bilinear
/// taps (grid → output) per output column / row: the plane at output pixel (x, y) is
/// `up(a)·log_l + up(b)`, upsampling horizontally then vertically (`resample::resize` order).
pub struct GridView<'g> {
    pub a: &'g Plane,
    pub b: &'g Plane,
    pub up_x: &'g [(usize, Vec<f32>)],
    pub up_y: &'g [(usize, Vec<f32>)],
}

impl GridView<'_> {
    /// The filtered plane over `r`, from its log luminance `l` (as `guided_fast`: coefficients
    /// upsampled horizontally, then vertically, then `a·l + b`).
    pub fn apply(&self, l: &Plane, r: Rect) -> Plane {
        let up_y = &self.up_y[r.y0..r.y0 + r.h];
        let j0 = up_y.iter().map(|(lo, _)| *lo).min().unwrap_or(0);
        let j1 = up_y.iter().map(|(lo, w)| lo + w.len()).max().unwrap_or(0);
        let up = |c: &Plane| {
            // rows j0..j1 of the grid, upsampled horizontally over the columns of `r`
            let mut tmp = Plane::new(r.w, j1 - j0);
            for j in j0..j1 {
                let src = c.row(j);
                for x in 0..r.w {
                    let (lo, w) = &self.up_x[r.x0 + x];
                    let mut acc = 0.0f32;
                    for (k, wt) in w.iter().enumerate() {
                        acc += src[lo + k] * wt;
                    }
                    tmp.data[(j - j0) * r.w + x] = acc;
                }
            }
            let mut out = Plane::new(r.w, r.h);
            for (y, (lo, w)) in up_y.iter().enumerate() {
                let row = &mut out.data[y * r.w..(y + 1) * r.w];
                for (k, wt) in w.iter().enumerate() {
                    let src = tmp.row(lo + k - j0);
                    for (o, s) in row.iter_mut().zip(src) {
                        *o += s * wt;
                    }
                }
            }
            out
        };
        let (a, b) = (up(self.a), up(self.b));
        let mut q = Plane::new(r.w, r.h);
        for (i, v) in q.data.iter_mut().enumerate() {
            *v = a.data[i] * l.data[i] + b.data[i];
        }
        q
    }
}

/// Render like [`crate::render`], tile by tile (see the module docs). `None` when this render
/// can't be tiled: the caller renders it whole.
pub fn render_tiled(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings, req: &RenderRequest, opts: &TileOptions) -> Option<Rendered> {
    let mut tp = TilePlan::new(src, info, s, req, opts)?;
    let (gw, gh, sig) = (tp.gw, tp.gh, tp.sig);

    // pass A: the global values (only when something needs them)
    if tp.needs_gather() {
        for c in tp.cores.clone() {
            let r = tp.region(c);
            let img = tp.lin(src, info, r);
            let log_l = img.map(log_lum);
            let base_l = (sig.base.is_some() && sig.base_detail > 0).then(|| local::median_hv(&log_l, sig.base_detail));
            let dark = sig.dark.map(|sg| gaussian(&img.map(dark_of), sg));
            tp.gather(c, r, &log_l, base_l.as_ref(), dark.as_ref());
        }
        tp.solve();
    }

    // pass B: each tile, finished into the output
    let s = &*tp.plan.settings;
    let deep_depth = req.depth != OutputDepth::U8;
    let mut image = Rgba8::new(gw, gh);
    let mut deep = deep_depth.then(|| match req.depth {
        OutputDepth::F32Linear => DeepSamples::F32(vec![0.0; gw * gh * 3]),
        _ => DeepSamples::U16(vec![0; gw * gh * 3]),
    });
    let ev = s.light.exposure as f32;
    for &c in &tp.cores {
        let r = tp.region(c);
        let img = tp.lin(src, info, r);
        let log_l = img.map(log_lum);
        let (cx, cy) = (c.x0 - r.x0, c.y0 - r.y0);
        let crop = |p: &Plane| Arc::new(p.crop(cx, cy, c.w, c.h));
        let core_l = log_l.crop(cx, cy, c.w, c.h);
        let plane = |sigma: Option<f32>, clarity: bool, eps: f32| match (sigma, tp.grid(clarity)) {
            (Some(_), Some(g)) => Some(Arc::new(g.apply(&core_l, c))),
            (Some(sg), None) => Some(crop(&guided_fast(&log_l, sg, eps))),
            (None, _) => None,
        };
        // process V2: the base filters the median of log luminance (local::tone_base)
        let base = match (sig.base, tp.grid(false)) {
            (Some(_), Some(g)) if sig.base_detail > 0 => {
                Some(Arc::new(g.apply(&local::median_hv(&log_l, sig.base_detail).crop(cx, cy, c.w, c.h), c)))
            }
            (Some(sg), None) => Some(crop(&local::tone_base(&log_l, sg, sig.base_detail))),
            _ => plane(sig.base, false, BASE_EPS),
        }
        .unwrap_or_else(|| Arc::new(core_l.clone()));
        let clarity_blur = plane(sig.clarity, true, CLARITY_EPS);
        let texture_blur = sig.texture.map(|sg| crop(&gaussian(&log_l, sg)));
        let dark = sig.dark.map(|sg| crop(&gaussian(&img.map(dark_of), sg)));
        let chroma_blur = sig.chroma.map(|sg| Arc::new(gaussian(&img.map(masks::chromaticity), sg).crop(cx, cy, c.w, c.h)));
        let masks = masks::evaluate(&s.masks, &tp.frame_of(r), r.w, r.h, &img, &log_l, ev, info.mattes.as_deref())
            .into_iter()
            .map(|m| masks::Evaluated { alpha: m.alpha.crop(cx, cy, c.w, c.h), ..m })
            .collect();
        let prep = Prepared {
            img: Arc::new(img.crop(cx, cy, c.w, c.h)),
            log_l: Arc::new(core_l),
            base,
            clarity_blur,
            texture_blur,
            dark,
            chroma_blur,
            air: tp.air,
            masks,
            px_per_long: tp.plan.px_per_long,
        };
        let cframe = tp.frame_of(c);
        let tile = match &mut deep {
            Some(samples) => {
                let d = finish::finish_deep(&prep, s, &cframe, info, req.space, req.depth, req.proof);
                match (samples, &d.samples) {
                    (DeepSamples::U16(all), DeepSamples::U16(part)) => put(all, part, gw, c, 3),
                    (DeepSamples::F32(all), DeepSamples::F32(part)) => put(all, part, gw, c, 3),
                    _ => {}
                }
                d.to_rgba8()
            }
            None => finish::finish(&prep, s, &cframe, info, req.space, req.display.as_ref(), req.proof),
        };
        put(&mut image.data, &tile.data, gw, c, 1);
    }
    let histogram = Histogram::of_srgb8(&image);
    let deep = deep.map(|samples| DeepImage { width: gw, height: gh, space: req.space, samples });
    Some(Rendered { image, histogram, deep })
}

/// Copy the rows of core `c` (`n` values per pixel) into the whole output `all` (`gw` wide).
pub fn put<T: Copy>(all: &mut [T], part: &[T], gw: usize, c: Rect, n: usize) {
    for y in 0..c.h {
        let to = ((c.y0 + y) * gw + c.x0) * n;
        all[to..to + c.w * n].copy_from_slice(&part[y * c.w * n..(y + 1) * c.w * n]);
    }
}
