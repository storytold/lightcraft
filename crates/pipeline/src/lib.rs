//! The LightCraft develop pipeline (CPU reference implementation).
//!
//! Input: a scene-referred, linear Rec.2020 source image (already EXIF-oriented) at any resolution
//! (full size or a proxy), plus [`DevelopSettings`]. Output: a display-encoded sRGB image at the
//! requested size, and its histogram.
//!
//! Stage order (see `docs/pipeline.md`):
//! 1. geometry — user orientation, lens corrections (distortion, CA, vignetting), perspective, crop +
//!    straighten, flips; one resample at output resolution; then defringe
//! 2. scene-linear — white balance, exposure, dehaze, local tone (highlights/shadows), texture,
//!    clarity, local adjustments (masks)
//! 3. tone map — contrast / whites / blacks filmic curve on luminance, highlight desaturation
//! 4. colour — vibrance, saturation, colour mixer, colour grading, B&W (OkLCh)
//! 5. display — gamut map to the output space (sRGB unless [`RenderRequest::space`] says otherwise), encode, tone curves (parametric + point), vignette, grain
//!
//! Spatial parameters are specified relative to the image's long edge, so a 400 px preview and a
//! 60 MP export look alike.
//!
//! Exposure is a gain, so the spatial stages run on the un-exposed image and the per-pixel stage
//! applies it (filters on log luminance are shift-equivariant: identical result). With
//! [`render_cached`] each stage's output is reused while its inputs are unchanged ([`StageCache`]):
//! tone and colour edits reuse prepared planes and, on native, mask alphas. Exposure edits also rebuild
//! the alpha of masks whose selection reads exposure, such as Sky and range masks.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[cfg(not(target_arch = "wasm32"))]
pub mod airlight;
pub mod auto;
pub mod blemish;
pub mod colorops;
pub mod cull;
pub mod curve;
pub mod dust;
pub mod finish;
pub mod geometry;
pub mod local;
pub mod local_tone;
pub mod lut;
pub mod masks;
pub mod optics;
pub mod output;
pub mod profiles;
pub mod redeye;
#[cfg(not(target_arch = "wasm32"))]
mod sampling;
pub mod spots;
pub mod tone;
pub mod transform;
pub mod upright;
pub mod visualize;
#[cfg(not(target_arch = "wasm32"))]
mod window;

pub use curve::{PARAM_AMOUNTS, PARAM_LUT, ParametricCurve};
pub use output::{DeepImage, DeepSamples, DisplaySpace, OutputDepth, OutputSpace, OutputTrc, Proof, ProofLut, proof_profile, register_proof_profile};
pub use visualize::{MaskView, Overlay};

use lightcraft_develop::{DevelopSettings, Treatment};
use std::any::Any;
use std::borrow::Cow;
use std::sync::{Arc, Mutex};

use lightcraft_raster::{Histogram, Plane, Rgb32f, Rgba8, par_rows};

pub use tone::ToneMap;

/// A raw source's own colour model, for white balance: like Lightroom, a white balance
/// re-evaluates the camera's colour matrices at the chosen white (camera-space white balance, DNG
/// spec ch. 6) instead of adapting the as-shot rendering ([`local::wb_matrix_for`]).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CameraColor {
    /// The file's colour tags (its profile look left out: it is applied at load).
    pub tags: lightcraft_raw::ColorData,
    /// The white the source pixels were developed for.
    pub developed_for: lightcraft_color::Xy,
}

/// Facts about the source the settings are interpreted against.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceInfo {
    /// Lens corrections embedded in the file (DNG opcodes), relative to the EXIF-oriented source.
    pub lens: Option<lightcraft_develop::EmbeddedLens>,
    /// Raw sources use absolute Kelvin white balance; rendered sources use a relative scale around
    /// their as-shot white.
    pub raw: bool,
    pub as_shot_temp: f64,
    pub as_shot_tint: f64,
    /// No measured camera illuminant: WB adjustments are relative to the camera's rendered look.
    pub relative_wb: bool,
    /// The Kelvin scale comes from matrices measured for the camera model, not from the file: before those were
    /// used for white balance, this source was edited on the relative scale (6500 K / 0 = as shot), so settings
    /// without a [`lightcraft_develop::WbScale`] are read that way ([`local::legacy_to_kelvin`]).
    pub legacy_relative_wb: bool,
    /// The camera's colour model (raw files with a colour matrix); `None`: white balance adapts
    /// the developed pixels (Bradford, linear Rec.2020).
    pub camera_color: Option<Arc<CameraColor>>,
    pub camera_tone: Option<tone::CameraTone>,
    /// Segmentation mattes stored in the file (DNG semantic masks): AI masks use them.
    pub mattes: Option<Arc<masks::Mattes>>,
    /// The raw's own local tone mapping (DNG `ProfileGainTableMap`), rendered only when the
    /// photo's Profile option asks for it ([`local_tone`]).
    pub local_tone: Option<Arc<local_tone::LocalTone>>,
    /// Long edge (px) of the original photo, when the source handed to the renderer is a smaller
    /// copy of it (the app's thumbnail and preview levels); `None`: the source is the original.
    /// Noise reduction sizes its radii in the original's pixels ([`Plan::detail_scale`]), so a
    /// preview looks like the full-size render scaled down (issue #960).
    pub native_long: Option<usize>,
}

impl Default for SourceInfo {
    fn default() -> Self {
        Self {
            raw: false,
            as_shot_temp: 6500.0,
            as_shot_tint: 0.0,
            lens: None,
            relative_wb: false,
            legacy_relative_wb: false,
            camera_color: None,
            camera_tone: None,
            mattes: None,
            local_tone: None,
            native_long: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Quality {
    /// Interactive drags: skips the most expensive refinements.
    Draft,
    #[default]
    Full,
}

#[derive(Clone, Copy, Debug)]
pub struct RenderRequest {
    /// Output must fit in `max_w × max_h` (aspect preserved).
    pub max_w: usize,
    pub max_h: usize,
    pub quality: Quality,
    /// Show the crop frame's content only (true) or the whole uncropped image (crop tool active).
    pub apply_crop: bool,
    /// A diagnostic view drawn over the result (e.g. Point Color's "visualize range").
    pub overlay: Overlay,
    /// Colour space of the result (exports; previews stay sRGB).
    pub space: OutputSpace,
    /// Sample format: 8-bit only, or also a 16-bit / linear float [`Rendered::deep`] (exports).
    pub depth: OutputDepth,
    /// Soft proofing (a proof against an output-device profile renders on the CPU; see [`Proof`]).
    pub proof: Option<Proof>,
    /// Render only this window of the output that `max_w × max_h` describes (a zoomed view):
    /// See [`PixelWindow`] for native filter context and the web margin policy.
    pub window: Option<PixelWindow>,
    /// Previews for a monitor with a display profile: render 8-bit into its primaries instead of
    /// `space` (see [`DisplaySpace`]; ignored by deep renders).
    pub display: Option<DisplaySpace>,
    /// A window rendered as main did (the caller's margin and the 384 px Dehaze proxy): a quick
    /// first look while the exact window follows. Leaves the exact path's caches alone.
    pub bounded_window: bool,
}

/// A window of the (virtual) full output, in its pixels: what a zoomed view needs, rendered
/// with full filter context on native. Dehaze can prepare the full frame within a memory
/// budget, then falls back to the proxy. Oversized filter support uses the bounded margin.
/// Requested work images above the pixel limit return an error. Web keeps the earlier margin policy
/// and Dehaze proxy, so its windows can differ from full renders.
/// The frame is the one `max_w × max_h` fits, after
/// crop, straighten and flips, as drawn. Out-of-range windows are clamped into it.
/// The native renderer supplies the context read by local filters and returns only this window.
/// Floating-point filter sums can still round differently from a full render.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelWindow {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

impl PixelWindow {
    /// This window inside a `full_w × full_h` frame: at least one pixel, wholly inside.
    pub fn clamped(self, full_w: usize, full_h: usize) -> PixelWindow {
        let (fw, fh) = (full_w.max(1), full_h.max(1));
        let x = self.x.min(fw - 1);
        let y = self.y.min(fh - 1);
        PixelWindow { x, y, w: self.w.clamp(1, fw - x), h: self.h.clamp(1, fh - y) }
    }
}

impl RenderRequest {
    /// Temporary native 8-bit windows only. Full frames, deep output and web
    /// keep their existing rendering and cache policy.
    pub fn is_bounded_window(&self) -> bool {
        cfg!(not(target_arch = "wasm32")) && self.window.is_some() && self.depth == OutputDepth::U8 && self.bounded_window
    }

    /// Use the temporary local grid and proxy policy for a native 8-bit drag.
    pub fn is_draft_window(&self) -> bool {
        cfg!(not(target_arch = "wasm32")) && self.window.is_some() && self.depth == OutputDepth::U8 && self.quality == Quality::Draft
    }

    pub fn fit(max_w: usize, max_h: usize) -> Self {
        Self {
            max_w,
            max_h,
            quality: Quality::Full,
            apply_crop: true,
            overlay: Overlay::None,
            space: OutputSpace::Srgb,
            depth: OutputDepth::U8,
            proof: None,
            window: None,
            display: None,
            bounded_window: false,
        }
    }
}

pub struct Rendered {
    /// The result, 8-bit (for deep renders: `deep` reduced to 8 bits, display-encoded).
    pub image: Rgba8,
    pub histogram: Histogram,
    /// The high-bit-depth result when [`RenderRequest::depth`] asks for one.
    pub deep: Option<DeepImage>,
}

/// Everything the per-pixel stage needs, precomputed at output resolution.
///
/// The image and planes are computed *before exposure* (so they can be reused while exposure is
/// dragged): the per-pixel stage multiplies the image by `gain` and shifts log planes by `ev`.
/// Spatial filters on log luminance are shift-equivariant, so this equals filtering after exposure.
pub(crate) struct Prepared {
    pub img: Arc<Rgb32f>,
    /// Log2 luminance relative to middle grey (before exposure: add `ev`).
    pub log_l: Arc<Plane>,
    pub base: Arc<Plane>,
    pub clarity_blur: Option<Arc<Plane>>,
    pub texture_blur: Option<Arc<Plane>>,
    pub dark: Option<Arc<Plane>>,
    /// Blurred chromaticity (`rgb / Y`) for local Moiré / Noise.
    pub chroma_blur: Option<Arc<Rgb32f>>,
    /// Airlight of `dark` (before exposure).
    pub air: f32,
    pub masks: Vec<masks::Evaluated>,
    /// Output pixels per unit of the source long edge.
    pub px_per_long: f64,
}

/// Output size for a source of `src_w × src_h` under `s`, fitting `max_w × max_h`.
pub fn output_size(src_w: usize, src_h: usize, s: &DevelopSettings, req: &RenderRequest) -> (usize, usize) {
    geometry::Frame::new(src_w, src_h, s, req.apply_crop).fit(req.max_w, req.max_h)
}

/// Size of the cropped output of a `src_w × src_h` source under `s` at the source's own resolution
/// (what a full-size export produces).
pub fn native_output_size(src_w: usize, src_h: usize, s: &DevelopSettings) -> (f64, f64) {
    geometry::Frame::new(src_w, src_h, s, true).native_size()
}

/// The geometric frame `render` uses for `src` (lens data, automatic CA estimate included).
pub fn frame_for(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings, apply_crop: bool) -> geometry::Frame {
    let mut frame = geometry::Frame::with_lens(src.width, src.height, s, apply_crop, info.lens.as_ref());
    if s.optics.remove_ca && s.section_enabled("optics") {
        frame.add_lateral_ca(optics::estimate_lateral_ca(src));
    }
    frame
}

/// Intermediate results of recent renders of one view, reused by [`render_cached`].
///
/// Each stage is keyed by exactly the inputs it depends on: the resampled source by the source
/// buffer, framing and output size; the white-balanced, retouched, denoised image by those plus
/// white balance, spots and noise reduction; each spatial plane by that plus its own radius; each
/// mask's alpha by its definition, the framing and, for masks that read them, that image and the
/// exposure ([`masks::alpha_key`]). Tone and colour edits reuse these stages. Exposure edits
/// rebuild only alphas that read exposure, such as Sky and range masks. A clarity drag skips
/// resampling and noise reduction. Holds the last few output sizes so draft and full-size renders
/// of the same view both stay warm.
pub struct StageCache {
    entries: Mutex<Vec<CacheEntry>>,
    capacity: usize,
    #[cfg(not(target_arch = "wasm32"))]
    airlight: airlight::AirlightCache,
    #[cfg(not(target_arch = "wasm32"))]
    airlight_frame: airlight::FrameCache<airlight::Full>,
    /// Another renderer's per-view state (the GPU renderer keeps its device-resident stages here).
    ext: Mutex<Option<Arc<dyn Any + Send + Sync>>>,
}

impl Default for StageCache {
    fn default() -> Self {
        StageCache {
            entries: Mutex::new(Vec::new()),
            capacity: 2,
            #[cfg(not(target_arch = "wasm32"))]
            airlight: airlight::AirlightCache::default(),
            #[cfg(not(target_arch = "wasm32"))]
            airlight_frame: airlight::FrameCache::default(),
            ext: Mutex::new(None),
        }
    }
}

impl StageCache {
    /// Drop everything (e.g. when memory is needed).
    pub fn clear(&self) {
        self.trim_images();
        #[cfg(not(target_arch = "wasm32"))]
        self.airlight.clear();
        *self.ext.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// Drop CPU image buffers, retaining source-keyed scalars and the renderer extension.
    /// The extension's owner must trim its buffers too.
    pub fn trim_images(&self) {
        self.lock().clear();
        #[cfg(not(target_arch = "wasm32"))]
        self.airlight_frame.clear();
    }

    /// Per-view state of type `T` kept alongside this cache (created on first use). Lets another
    /// renderer of the same view (e.g. `lightcraft-gpu`) keep its own stage cache with this one.
    pub fn extension<T: Any + Send + Sync + Default>(&self) -> Arc<T> {
        let mut g = self.ext.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(e) = g.as_ref()
            && let Ok(t) = e.clone().downcast::<T>()
        {
            return t;
        }
        let t = Arc::new(T::default());
        *g = Some(t.clone());
        t
    }

    /// The per-view state of type `T` if there is one (unlike [`Self::extension`], never creates it).
    pub fn peek_extension<T: Any + Send + Sync>(&self) -> Option<Arc<T>> {
        self.ext.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|e| e.clone().downcast::<T>().ok())
    }

    /// Bytes of the intermediate images held (each buffer counted once; the sources they were
    /// made from belong to their owner and are not counted).
    pub fn bytes(&self) -> usize {
        fn size<T>(i: &lightcraft_raster::Image<T>) -> usize {
            i.data.len() * std::mem::size_of::<T>()
        }
        let mut seen: Vec<usize> = Vec::new();
        let mut total = 0;
        let mut add = |p: usize, n: usize| {
            if !seen.contains(&p) {
                seen.push(p);
                total += n;
            }
        };
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(full) = self.airlight_frame.value() {
            if let Some(sampled) = &full.sampled {
                add(Arc::as_ptr(sampled) as usize, size(sampled));
            }
            add(Arc::as_ptr(&full.lin) as usize, size(&full.lin));
            add(Arc::as_ptr(&full.dark) as usize, size(&full.dark));
        }
        for e in self.lock().iter() {
            if !Arc::ptr_eq(&e.sampled, &e.src) {
                add(Arc::as_ptr(&e.sampled) as usize, size(&e.sampled));
            }
            if let Some((_, l)) = &e.lin {
                add(Arc::as_ptr(l) as usize, size(l));
            }
            let pl = &e.planes;
            let planes = pl.log_l.iter().chain(pl.base.iter().map(|x| &x.1)).chain(pl.clarity.iter().map(|x| &x.1));
            let planes = planes.chain(pl.texture.iter().map(|x| &x.1)).chain(pl.dark.iter().map(|x| &x.1));
            #[cfg(not(target_arch = "wasm32"))]
            let planes = planes.chain(e.masks.planes()).chain(e.masks.refinement_planes());
            for p in planes {
                add(Arc::as_ptr(p) as usize, size(p));
            }
            #[cfg(not(target_arch = "wasm32"))]
            if let Some((_, c)) = &pl.chroma {
                add(Arc::as_ptr(c) as usize, size(c));
            }
        }
        total
    }

    /// Number of cached output sizes.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<CacheEntry>> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn get(&self, src: &Arc<Rgb32f>, geo: u64) -> Option<CacheEntry> {
        self.lock().iter().find(|e| e.geo == geo && Arc::ptr_eq(&e.src, src)).cloned()
    }

    fn put(&self, e: CacheEntry) {
        let mut v = self.lock();
        v.retain(|o| !(o.geo == e.geo && Arc::ptr_eq(&o.src, &e.src)));
        v.push(e);
        while v.len() > self.capacity {
            v.remove(0);
        }
    }
}

#[derive(Clone)]
struct CacheEntry {
    src: Arc<Rgb32f>,
    geo: u64,
    sampled: Arc<Rgb32f>,
    lin: Option<(u64, Arc<Rgb32f>)>,
    #[cfg(not(target_arch = "wasm32"))]
    full_airlight: Option<u64>,
    planes: local::Planes,
    /// Alphas of the last render's masks (kept across changes to the image: those of masks that
    /// don't read it stay valid).
    #[cfg(not(target_arch = "wasm32"))]
    masks: masks::AlphaCache,
}

fn hash_of(parts: impl std::hash::Hash) -> u64 {
    use std::hash::{BuildHasher, BuildHasherDefault, DefaultHasher};
    BuildHasherDefault::<DefaultHasher>::default().hash_one(parts)
}

/// What a render resolves to before any pixel work: the effective settings (profile and Upright
/// applied), the geometric frame, the output size and the stage-cache keys. Shared by the CPU
/// renderer and the GPU renderer (`lightcraft-gpu`), so both key their caches identically.
/// On wasm32 planning also computes main's small Dehaze proxy for a window.
pub struct Plan<'a> {
    pub settings: Cow<'a, DevelopSettings>,
    pub frame: geometry::Frame,
    /// Full-frame grid for exact windows; local grid for temporary windows.
    pub guidance_view: Option<geometry::ViewWindow>,
    pub w: usize,
    pub h: usize,
    /// Output pixels per unit of the oriented source's long edge.
    pub px_per_long: f64,
    /// Long edge of the source buffer (px).
    pub src_long: usize,
    /// Output pixels per pixel of the original photo ([`SourceInfo::native_long`], else the source
    /// buffer), at most 1: the scale of noise reduction, whose radii are in the original's pixels.
    pub detail_scale: f32,
    /// Key of the resampled source (together with the source buffer's identity).
    pub geo: u64,
    /// Key of the white-balanced, retouched, denoised image (and of the planes computed from it).
    pub lin_key: u64,
    /// [`Self::lin_key`] without the noise-reduction settings: draft renders reuse the smooth
    /// planes of an image that differs from this one only in its noise reduction.
    pub pre_nr_key: u64,
    /// Red eye / pet eye corrections with their detected pupils, in output pixels.
    pub eyes: Vec<redeye::EyeK>,
    /// Whether this window needs the full-frame Dehaze statistic. Planning does no pixel work.
    #[cfg(not(target_arch = "wasm32"))]
    pub needs_airlight: bool,
    /// Main's whole-frame Dehaze proxy on wasm32.
    #[cfg(target_arch = "wasm32")]
    pub fixed_air: Option<f32>,
    /// A windowed render prepares a larger window for spatial filters and spot reads.
    /// This is the requested window inside that work buffer, which the result is cut to.
    pub keep: Option<PixelWindow>,
    /// The source's segmentation mattes ([`SourceInfo::mattes`]).
    pub mattes: Option<Arc<masks::Mattes>>,
}

/// Resolve `s` against `src` for `req` (see [`Plan`]).
pub fn plan<'a>(src: &Rgb32f, info: &SourceInfo, s: &'a DevelopSettings, req: &RenderRequest) -> Plan<'a> {
    let mut settings: Cow<'a, DevelopSettings> = match profiles::effective(s) {
        Cow::Borrowed(b) => upright::resolve(src, info, b),
        Cow::Owned(o) => Cow::Owned(upright::resolve(src, info, &o).into_owned()),
    };
    visualize::adjust_settings(req.overlay, &mut settings);
    // a window must not choose a spot's source from the pixels it happens to hold: fix them
    // from a fixed-size render of the whole frame first
    if req.window.is_some() && settings.spots.iter().any(|sp| sp.source_offset.is_none()) {
        let picked: Vec<Option<lightcraft_geom::Point>> =
            settings.spots.iter().map(|sp| sp.source_offset.or_else(|| spots::pick_source(src, info, &settings, sp, None))).collect();
        for (sp, o) in settings.to_mut().spots.iter_mut().zip(picked) {
            sp.source_offset = o;
        }
    }
    let s = &*settings;
    let frame = frame_for(src, info, s, req.apply_crop);
    let (full_w, full_h) = frame.fit(req.max_w, req.max_h);
    // sizes and scale are those of the whole output; a window only narrows what is drawn
    let px_per_long = frame.px_per_long(full_w);
    let src_long = src.width.max(src.height);
    // (an original reported smaller than the source is the source)
    let native_long = info.native_long.unwrap_or(0).max(src_long).max(1);
    let detail_scale = local::detail_scale(px_per_long, native_long);
    #[cfg(not(target_arch = "wasm32"))]
    let needs_airlight = req.window.is_some() && local::context_plane_sigmas(s, px_per_long, req.quality).dark.is_some();
    #[cfg(target_arch = "wasm32")]
    let mut fixed_air = None;
    let mut keep = None;
    let (frame, w, h) = match req.window {
        Some(win) => {
            let win = win.clamped(full_w, full_h);
            // wasm32 keeps main's margin and proxy policy to preserve its memory use.
            #[cfg(target_arch = "wasm32")]
            let work = {
                if s.section_enabled("effects") && (s.effects.dehaze != 0.0 || s.masks.iter().any(|m| m.adjust.dehaze != 0.0)) {
                    const AIRLIGHT_PROXY_EDGE: usize = 384;
                    let k = (AIRLIGHT_PROXY_EDGE as f64 / full_w.max(full_h) as f64).min(1.0);
                    let (pw, ph) = (((full_w as f64 * k).round() as usize).max(1), ((full_h as f64 * k).round() as usize).max(1));
                    fixed_air = Some(local::frame_airlight(src, info, s, &frame, pw, ph));
                }
                if req.depth == OutputDepth::U8 { spots::window_for_reads(s, &frame, full_w, full_h, px_per_long, win) } else { win }
            };
            #[cfg(not(target_arch = "wasm32"))]
            let work = {
                let margin = window::margin(s, px_per_long, detail_scale, full_w.max(full_h), req.quality, info.mattes.as_deref());
                let work = window::expand(win, full_w, full_h, margin);
                // Spots upstream of the filters must supply the expanded context too.
                let work = spots::window_for_reads_checked(s, &frame, full_w, full_h, px_per_long, work).unwrap_or(PixelWindow {
                    x: 0,
                    y: 0,
                    w: full_w,
                    h: full_h,
                });
                // Defringe also surrounds the source patches read by spots.
                let work = window::expand(work, full_w, full_h, optics::defringe_radius(s, px_per_long / optics::DEFRINGE_REF_LONG));
                // (drafts follow a drag: main's window is enough for them, and keeps them as fast)
                if req.is_bounded_window() || req.is_draft_window() || airlight::check_pixels(work.w, work.h).is_err() {
                    // Preserve main's bounded window when complete support cannot
                    // fit. Dehaze's full-frame budget check selects its proxy too.
                    if req.depth == OutputDepth::U8 { spots::window_for_reads(s, &frame, full_w, full_h, px_per_long, win) } else { win }
                } else {
                    work
                }
            };
            if work != win {
                keep = Some(PixelWindow { x: win.x - work.x, y: win.y - work.y, ..win });
            }
            (frame.window(full_w, full_h, work), work.w, work.h)
        }
        None => (frame, full_w, full_h),
    };
    // A whole-frame work buffer has the same sampling and upstream cache keys as window=None.
    let cache_frame = frame.clone();
    #[cfg(not(target_arch = "wasm32"))]
    let cache_frame = {
        let mut cache_frame = cache_frame;
        if frame.view.is_some_and(|v| v.x == 0.0 && v.y == 0.0 && v.full_w == w as f64 && v.full_h == h as f64) {
            cache_frame.view = None;
        }
        cache_frame
    };
    let geo = hash_of((format!("{cache_frame:?}"), w, h));
    let temporary_window = req.is_bounded_window() || req.is_draft_window();
    let geo = if temporary_window { hash_of((geo, "local window guidance")) } else { geo };
    let guidance_view = if temporary_window { None } else { frame.view };
    let (wb_t, wb_tint) = local::effective_wb(info, s);
    let eyes = redeye::resolve(src, &s.red_eye, s.orientation, &frame, w, h, px_per_long);
    let d = &s.detail;
    let pre_nr_key = hash_of((
        geo,
        [wb_t, wb_tint, info.as_shot_temp, info.as_shot_tint].map(f64::to_bits),
        format!("{:?}", s.spots),
        format!("{eyes:?}"),
        // defringe runs in this stage
        format!("{:?}", s.optics),
        // and the local tone mapping
        local_tone::enabled(info, s),
        #[cfg(not(target_arch = "wasm32"))]
        local::wb_matrix_for(info, s).map(|m| m.map(|row| row.map(f32::to_bits))),
        // Metadata can change while the source Arc stays the same.
        #[cfg(not(target_arch = "wasm32"))]
        local_tone::enabled(info, s).then(|| format!("{:?}", info.local_tone)),
        src_long,
    ));
    let lin_key = hash_of((
        pre_nr_key,
        [d.nr_luminance, d.nr_detail, d.nr_color, d.nr_color_detail, d.nr_color_smoothness].map(f64::to_bits),
        detail_scale.to_bits(),
    ));
    Plan {
        settings,
        frame,
        guidance_view,
        w,
        h,
        px_per_long,
        src_long,
        detail_scale,
        geo,
        lin_key,
        pre_nr_key,
        eyes,
        #[cfg(not(target_arch = "wasm32"))]
        needs_airlight,
        #[cfg(target_arch = "wasm32")]
        fixed_air,
        keep,
        mattes: info.mattes.clone(),
    }
}

/// Whether the scene-linear stage needs work only the CPU does (defringe, spot removal).
pub fn lin_needs_cpu(s: &DevelopSettings, info: &SourceInfo) -> bool {
    let o = &s.optics;
    let defringe = s.section_enabled("optics") && (o.defringe_purple_amount > 0.0 || o.defringe_green_amount > 0.0);
    defringe || !s.spots.is_empty() || local_tone::enabled(info, s)
}

/// The white-balanced, defringed, retouched image (before noise reduction): the CPU part of the
/// scene-linear stage, in place.
pub fn lin_cpu(img: &mut Rgb32f, info: &SourceInfo, p: &Plan<'_>) {
    #[cfg(test)]
    airlight::tests::LINEAR_PASSES.with(|n| n.set(n.get() + 1));
    let s = &*p.settings;
    // (before the white balance: the gain's input is the as-shot development, as in the file)
    local_tone::apply(img, info, p);
    local::white_balance(img, info, s);
    optics::defringe(img, s, p.px_per_long / optics::DEFRINGE_REF_LONG);
    spots::apply(img, &s.spots, &p.frame, p.px_per_long);
    redeye::apply(img, &p.eyes);
}

/// The settings a render of `s` for `req` uses: the HDR edit for an [`OutputDepth::F32Hdr`]
/// render, otherwise its SDR rendition (see [`DevelopSettings::sdr_rendition`]). Every renderer
/// (CPU and GPU) goes through this, so an SDR render of an HDR edit looks the same everywhere.
pub fn settings_for<'a>(s: &'a DevelopSettings, req: &RenderRequest) -> std::borrow::Cow<'a, DevelopSettings> {
    if req.depth == OutputDepth::F32Hdr { std::borrow::Cow::Borrowed(s) } else { s.sdr_rendition() }
}

/// Long edge (pixels) of the small HDR render [`hdr_histogram`] measures.
pub const HDR_HISTOGRAM_EDGE: usize = 384;

/// Long edge of the render that gives a slider drag its live HDR histogram (drafts have none: a
/// second render per tick would slow them down), run beside the drafts by the UI.
pub const HDR_HISTOGRAM_DRAG_EDGE: usize = 192;

/// The histogram of an HDR edit's HDR rendition for a preview request `req`: a small float HDR
/// render of the same framing ([`HDR_HISTOGRAM_EDGE`]), binned with SDR on the left half and the
/// whole HDR range ([`Hdr::MAX_EV_LIMIT`](lightcraft_develop::Hdr::MAX_EV_LIMIT) stops above SDR
/// white, whatever the headroom limit, so the scale stays put) on the right ([`Histogram::of_hdr`]). A preview of
/// an HDR edit shows its SDR rendition; this says how far its highlights go. `None` for SDR edits,
/// high-bit-depth, windowed and proofed requests (their histogram stays the image's).
pub fn hdr_histogram(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings, req: &RenderRequest) -> Option<Histogram> {
    // a drag's drafts keep their own histogram: a second render per tick would slow them down
    if !s.hdr.enabled || req.depth != OutputDepth::U8 || req.window.is_some() || req.proof.is_some() || req.quality == Quality::Draft {
        return None;
    }
    let k = (HDR_HISTOGRAM_EDGE as f64 / req.max_w.max(req.max_h).max(1) as f64).min(1.0);
    let size = |v: usize| ((v as f64 * k).round() as usize).max(1);
    let hreq = RenderRequest {
        max_w: size(req.max_w),
        max_h: size(req.max_h),
        depth: OutputDepth::F32Hdr,
        overlay: Overlay::None,
        proof: None,
        window: None,
        display: None,
        ..*req
    };
    let deep = render(src, info, s, &hreq).ok()?.deep?;
    let DeepSamples::F32(v) = &deep.samples else { return None };
    Some(Histogram::of_hdr(v.as_chunks::<3>().0, lightcraft_develop::Hdr::MAX_EV_LIMIT as f32))
}

/// Render `src` with settings `s`. Native windows that exceed the allocation
/// bounds return an error. Web keeps main's window policy and allocation path.
pub fn render(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings, req: &RenderRequest) -> Result<Rendered, String> {
    render_impl(Src::Borrowed(src), info, s, req, None)
}

/// [`render`], reusing (and refreshing) the intermediate results in `cache`.
/// The output is identical to [`render`]'s.
pub fn render_cached(src: &Arc<Rgb32f>, info: &SourceInfo, s: &DevelopSettings, req: &RenderRequest, cache: &StageCache) -> Result<Rendered, String> {
    render_impl(Src::Shared(src), info, s, req, Some(cache))
}

enum Src<'a> {
    Borrowed(&'a Rgb32f),
    Shared(&'a Arc<Rgb32f>),
}

#[cfg(not(target_arch = "wasm32"))]
fn sample_frame(src: &Src<'_>, image: &Rgb32f, frame: &geometry::Frame, w: usize, h: usize) -> Arc<Rgb32f> {
    let sampling = frame.sample_plan(image.width, image.height, w, h);
    match src {
        Src::Shared(a)
            if frame.orient == lightcraft_geom::Orientation::Normal
                && sampling.prefilter.is_none()
                && matches!(sampling.mode, geometry::SampleMode::Copy) =>
        {
            (*a).clone()
        }
        _ => Arc::new(frame.sample(image, w, h)),
    }
}

fn render_impl(src: Src<'_>, info: &SourceInfo, s: &DevelopSettings, req: &RenderRequest, cache: Option<&StageCache>) -> Result<Rendered, String> {
    // sections switched off with their eye render as if at their defaults (issue #316)
    let effective = s.effective();
    let edit: &DevelopSettings = &effective;
    let rendition = settings_for(edit, req);
    let s: &DevelopSettings = &rendition;
    // `Instant::now()` panics on wasm32-unknown-unknown: only read the clock when profiling.
    let lap = |what: &str, t: &mut Option<std::time::Instant>| {
        if let Some(t) = t {
            eprintln!("  {what}: {:.1} ms", t.elapsed().as_secs_f64() * 1e3);
            *t = std::time::Instant::now();
        }
    };
    let mut t = profiling().then(std::time::Instant::now);
    let src_img: &Rgb32f = match &src {
        Src::Borrowed(r) => r,
        Src::Shared(a) => a,
    };
    let plan = plan(src_img, info, s, req);
    #[cfg(not(target_arch = "wasm32"))]
    let full = plan.needs_airlight.then(|| self::plan(src_img, info, s, &RenderRequest { window: None, ..*req }));
    #[cfg(not(target_arch = "wasm32"))]
    if req.window.is_some() {
        airlight::check_pixels(plan.w, plan.h)?;
    }
    let shared = match (&src, cache) {
        (Src::Shared(a), Some(c)) => Some((*a, c)),
        _ => None,
    };
    let cached = shared.and_then(|(a, c)| c.get(a, plan.geo));
    #[cfg(not(target_arch = "wasm32"))]
    let cached = cached.filter(|e| e.full_airlight.is_some() == plan.needs_airlight);
    #[cfg(not(target_arch = "wasm32"))]
    let mut prepared_work = None;
    #[cfg(target_arch = "wasm32")]
    let fixed_air = plan.fixed_air;
    #[cfg(not(target_arch = "wasm32"))]
    let fixed_air = if let Some(full) = full.as_ref().filter(|full| req.is_bounded_window() || full.lin_key != plan.lin_key) {
        let budget = shared.map_or(airlight::PREPARE_BUDGET, |(_, c)| c.airlight.budget());
        if req.is_bounded_window() {
            Some(airlight::proxy(src_img, info, full))
        } else if req.is_draft_window() {
            Some(shared.and_then(|(src, cache)| cache.airlight.get(src, full.lin_key)).unwrap_or_else(|| airlight::proxy(src_img, info, full)))
        } else if airlight::check_prepare_budget(full, Some(&plan), budget, false).is_err()
            || airlight::check_pixels(src_img.width, src_img.height).is_err()
        {
            if let Some((_, cache)) = shared {
                cache.airlight_frame.clear();
            }
            Some(airlight::proxy(src_img, info, full))
        } else {
            // Keep cropping a matching full preparation when its work pixels are missing.
            // After trimming that preparation, the source-keyed scalar still avoids a full pass.
            let saved = shared.and_then(|(a, c)| c.airlight_frame.get(a));
            let saved_matches = saved.as_ref().is_some_and(|(key, _)| *key == full.lin_key);
            let entry_matches =
                cached.as_ref().is_some_and(|e| e.full_airlight == Some(full.lin_key) && e.lin.as_ref().is_some_and(|(k, _)| *k == plan.lin_key));
            let ready = shared.and_then(|(src, cache)| cache.airlight.get(src, full.lin_key)).filter(|_| !saved_matches || entry_matches);
            if let Some(air) = ready {
                Some(air)
            } else {
                let prepared = match saved {
                    Some((key, value)) if key == full.lin_key => value,
                    other => {
                        let sampled = other.filter(|(_, data)| data.geo == full.geo).and_then(|(_, data)| data.sampled.clone());
                        if let Some((_, cache)) = shared {
                            cache.airlight_frame.clear();
                        }
                        let data = Arc::new(airlight::prepare(src_img, info, full, Some(&plan), sampled)?);
                        if let Some((src, cache)) = shared {
                            cache.airlight_frame.put(src, full.lin_key, data.clone(), data.bytes());
                        }
                        data
                    }
                };
                let air = prepared.air;
                prepared_work = Some(prepared.window(&plan));
                if let Some((src, cache)) = shared {
                    cache.airlight.remember(src, full.lin_key, air);
                }
                Some(air)
            }
        }
    } else {
        None
    };
    let Plan { ref frame, w, h, px_per_long, detail_scale, geo, lin_key, .. } = plan;
    let s = &*plan.settings;

    #[cfg(target_arch = "wasm32")]
    let sampled = match &cached {
        Some(e) => e.sampled.clone(),
        None => Arc::new(frame.sample(src_img, w, h)),
    };
    #[cfg(not(target_arch = "wasm32"))]
    let sampled = match (prepared_work.as_ref().and_then(|work| work.sampled.clone()), &cached) {
        (Some(sampled), _) => sampled,
        (_, Some(e)) => e.sampled.clone(),
        _ => sample_frame(&src, src_img, frame, w, h),
    };
    lap("sample", &mut t);

    #[cfg(not(target_arch = "wasm32"))]
    let prepared_lin = prepared_work
        .as_ref()
        .map(|work| (lin_key, work.lin.clone()))
        .or_else(|| cached.as_ref().and_then(|e| e.lin.clone()).filter(|(k, _)| *k == lin_key));
    #[cfg(target_arch = "wasm32")]
    let prepared_lin = cached.as_ref().and_then(|e| e.lin.clone()).filter(|(k, _)| *k == lin_key);
    let lin = match prepared_lin {
        Some((_, img)) => img,
        None => {
            // Without a cache the resampled buffer is ours: work on it in place.
            let mut img = if shared.is_some() { (*sampled).clone() } else { Arc::unwrap_or_clone(sampled.clone()) };
            lin_cpu(&mut img, info, &plan);
            local::denoise(&mut img, s, detail_scale);
            Arc::new(img)
        }
    };
    lap("wb/spots/nr", &mut t);
    #[cfg(not(target_arch = "wasm32"))]
    let mut alphas = cached.as_ref().map(|e| e.masks.clone()).unwrap_or_default();
    #[cfg(target_arch = "wasm32")]
    let mut alphas = masks::AlphaCache::default();
    let mut planes = match cached.map(|e| e.planes) {
        Some(p) if p.key == lin_key => p,
        // A draft while noise reduction changes (a slider drag): the wide, edge-aware planes of the
        // image before the change stand in (noise reduction barely moves a blur that wide); the
        // per-pixel log luminance and the fine texture band are redone. Keyed so a full-quality
        // render never takes them.
        Some(p) if req.quality == Quality::Draft && p.pre_nr == plan.pre_nr_key => p.for_other_nr(),
        _ => local::Planes { key: lin_key, pre_nr: plan.pre_nr_key, ..Default::default() },
    };
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(work) = prepared_work.take() {
        let sigma = (0.02 * px_per_long as f32).max(1.0);
        planes.dark = Some((sigma.to_bits(), work.dark, fixed_air.unwrap_or(1.0)));
    }
    let keys = masks::AlphaKeys { geo, image: lin_key };
    let mut prep = local::prepare_with_guidance(
        lin.clone(),
        s,
        frame,
        plan.guidance_view,
        px_per_long,
        req.quality,
        &mut planes,
        plan.mattes.as_ref(),
        (&mut alphas, keys),
    );
    #[cfg(not(target_arch = "wasm32"))]
    if let Some((src, cache)) = shared
        && !req.is_bounded_window()
        && !req.is_draft_window()
        && (req.window.is_none() || full.as_ref().is_some_and(|p| p.lin_key == lin_key))
        && prep.dark.is_some()
        && planes.key == lin_key
    {
        cache.airlight.remember(src, lin_key, prep.air);
    }
    if let Some(a) = fixed_air {
        prep.air = a;
    }
    lap("prepare", &mut t);
    // (a bounded window is a stand-in: its stages would only evict the exact window's)
    if let Some((a, c)) = shared.filter(|_| !req.is_bounded_window()) {
        c.put(CacheEntry {
            src: a.clone(),
            geo,
            sampled,
            lin: Some((lin_key, lin)),
            planes,
            #[cfg(not(target_arch = "wasm32"))]
            masks: alphas,
            #[cfg(not(target_arch = "wasm32"))]
            full_airlight: full.as_ref().map(|p| p.lin_key),
        });
    }
    if req.depth != OutputDepth::U8 {
        let deep = finish::finish_deep(&prep, s, frame, info, (req.space, req.depth), req.proof, plan.keep);
        let image = deep.to_rgba8();
        let histogram = Histogram::of_srgb8(&image);
        lap("finish (deep)", &mut t);
        return Ok(Rendered { image, histogram, deep: Some(deep) });
    }
    // Overlays may read neighbouring finished pixels; ordinary windows finish only their output.
    let finish_keep = plan.keep.filter(|_| !cfg!(target_arch = "wasm32") && req.overlay == Overlay::None);
    let crop = plan.keep.filter(|_| finish_keep.is_none());
    let image = finish::finish(&prep, s, frame, info, req.space, req.display.as_ref(), req.proof, finish_keep);
    lap("finish", &mut t);
    let cut = |i: &Rgba8| match crop {
        Some(k) => i.crop(k.x, k.y, k.w, k.h),
        None => i.clone(),
    };
    let histogram = match crop {
        Some(_) => Histogram::of_srgb8(&cut(&image)),
        None => Histogram::of_srgb8(&image),
    };
    lap("histogram", &mut t);
    let mut image = image;
    let mask = overlay_alpha(req.overlay, &plan, &prep);
    let masks = overlay_alphas_all(req.overlay, &plan, &prep);
    visualize::apply(&mut image, req.overlay, &plan, mask.as_ref(), masks.as_deref());
    let mut image = if crop.is_some() { cut(&image) } else { image };
    if req.overlay == Overlay::HdrRange && edit.hdr.enabled {
        // the HDR rendition of the same request says how far above SDR white each pixel goes
        let hreq = RenderRequest { depth: OutputDepth::F32Hdr, overlay: Overlay::None, proof: None, ..*req };
        if let Some(hdr) = render_impl(Src::Borrowed(src_img), info, edit, &hreq, None)?.deep {
            visualize::hdr_range(&mut image, &hdr, req.space.luma());
        }
        lap("visualize hdr", &mut t);
    }
    Ok(Rendered { image, histogram, deep: None })
}

/// The colour a colour-range mask would sample at normalized image point `p` (OkLab of the
/// exposed scene colours, as [`MaskShape::ColorRange`](lightcraft_develop::MaskShape) compares
/// them), averaged over 3 × 3 pixels of a render fitting `req`. `None` outside the image.
pub fn color_range_sample(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings, req: &RenderRequest, p: lightcraft_geom::Point) -> Option<[f64; 3]> {
    // Pick from the same settings as the render: disabled panels contribute nothing, and an
    // SDR preview of an HDR edit includes the SDR rendition's exposure offset.
    let effective = s.effective();
    let rendition = settings_for(&effective, req);
    let plan = plan(src, info, &rendition, req);
    let mut img = plan.frame.sample(src, plan.w, plan.h);
    lin_cpu(&mut img, info, &plan);
    let o = plan.frame.norm_to_out(plan.w, plan.h).apply(p);
    let (cx, cy) = (o.x.floor() as i64, o.y.floor() as i64);
    if cx < 0 || cy < 0 || cx >= plan.w as i64 || cy >= plan.h as i64 {
        return None;
    }
    let gain = (plan.settings.light.exposure as f32).exp2();
    let mut acc = [0f64; 3];
    let mut n = 0.0;
    for y in (cy - 1).max(0)..=(cy + 1).min(plan.h as i64 - 1) {
        for x in (cx - 1).max(0)..=(cx + 1).min(plan.w as i64 - 1) {
            let c = img.data[y as usize * plan.w + x as usize].map(|v| v * gain);
            let lab = lightcraft_color::perceptual::oklab_from_2020(masks::tonemap_for_select(c));
            for k in 0..3 {
                acc[k] += lab[k] as f64;
            }
            n += 1.0;
        }
    }
    Some(acc.map(|v| v / n))
}

/// The alpha plane a mask overlay shows: the one the render evaluated, or (for a hidden mask) a
/// fresh evaluation.
fn overlay_alpha(o: Overlay, plan: &Plan<'_>, prep: &Prepared) -> Option<Plane> {
    let m = o.mask(&plan.settings)?;
    if let Some(e) = prep.masks.iter().find(|e| e.id == m.id) {
        return Some((*e.alpha).clone());
    }
    let ev = plan.settings.light.exposure as f32;
    Some(masks::evaluate_one(m, &plan.frame, plan.w, plan.h, &prep.img, &prep.log_l, ev, plan.mattes.as_deref()))
}

/// The alpha planes an `Overlay::AllMasks` shows: one per visible mask that has components,
/// reusing the render's own evaluation where available (a fresh evaluation otherwise).
fn overlay_alphas_all(o: Overlay, plan: &Plan<'_>, prep: &Prepared) -> Option<Vec<Plane>> {
    if !matches!(o, Overlay::AllMasks { .. }) {
        return None;
    }
    let ev = plan.settings.light.exposure as f32;
    let mut out = Vec::new();
    for m in plan.settings.masks.iter().filter(|m| m.visible && !m.components.is_empty()) {
        if let Some(e) = prep.masks.iter().find(|e| e.id == m.id) {
            out.push((*e.alpha).clone());
        } else {
            out.push(masks::evaluate_one(m, &plan.frame, plan.w, plan.h, &prep.img, &prep.log_l, ev, plan.mattes.as_deref()));
        }
    }
    Some(out)
}

/// Convenience: render a before/after pair side by side is up to the UI; this renders "before"
/// (default look, keeping the crop so framing matches, under the photo's own rendering process).
pub fn before_settings(s: &DevelopSettings) -> DevelopSettings {
    let mut b = DevelopSettings { crop: s.crop, orientation: s.orientation, process: s.process, ..DevelopSettings::default() };
    b.wb = lightcraft_develop::WhiteBalance { mode: lightcraft_develop::WbMode::AsShot, ..b.wb };
    b
}

pub(crate) fn is_bw(s: &DevelopSettings) -> bool {
    s.treatment == Treatment::Bw || s.profile.id == "lc.mono" || s.profile.id.starts_with("lc.bw.")
}

/// `LIGHTCRAFT_PROFILE` is set: print per-stage timings to stderr.
pub fn profiling() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("LIGHTCRAFT_PROFILE").is_some())
}

/// Run `f`, printing its duration under `LIGHTCRAFT_PROFILE`.
pub(crate) fn timed<R>(what: &str, f: impl FnOnce() -> R) -> R {
    if !profiling() {
        return f();
    }
    let t = std::time::Instant::now();
    let r = f();
    eprintln!("    {what}: {:.1} ms", t.elapsed().as_secs_f64() * 1e3);
    r
}

pub(crate) fn for_rows<T: Send>(data: &mut [T], w: usize, f: impl Fn(usize, &mut [T]) + Sync + Send) {
    par_rows(data, w, f)
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_geometry;
#[cfg(test)]
mod tests_hdr;
#[cfg(test)]
mod tests_local;
#[cfg(test)]
mod tests_nr_scale;
#[cfg(test)]
mod tests_tone;
#[cfg(test)]
mod tests_window;
