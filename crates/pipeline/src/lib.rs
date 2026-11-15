//! The develop pipeline (CPU reference implementation).
//!
//! Input: a scene-referred, linear Rec.2020 source image (already EXIF-oriented) at any resolution
//! (full size or a proxy), plus [`DevelopSettings`]. Output: a display-encoded sRGB image at the
//! requested size, and its histogram.
//!
//! Stage order (see `docs/pipeline.md`):
//! 1. geometry — user orientation, lens corrections (distortion, CA, vignetting), perspective, crop +
//!    straighten, flips; one resample at output resolution; then defringe
//! 2. scene-linear — white balance, spot removal, noise reduction, Smooth Skin ([`skin`]), then
//!    exposure, dehaze, local tone (highlights/shadows), texture, clarity, local adjustments (masks)
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
//! dragging a tone, colour or exposure slider re-runs only the per-pixel stage.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod auto;
pub mod auto_mask;
pub mod blemish;
pub mod colorops;
pub mod cull;
pub mod curve;
pub mod depth_model;
pub mod dust;
pub mod film_dust;
pub mod finish;
pub mod genpatch;
pub mod geometry;
pub mod grade;
pub mod grain;
pub mod local;
pub mod local_tone;
pub mod lut;
pub mod mask_space;
pub mod mask_tone;
pub mod masks;
pub mod nrc_joint;
#[cfg(test)]
mod nrc_joint_reference;
pub mod optics;
pub mod output;
pub mod plugin_filters;
pub mod profiles;
pub mod redeye;
pub mod retouch;
pub mod scene_model;
pub mod skin;
pub mod source_stage;
pub mod spots;
pub mod tone;
pub mod transform;
pub mod upright;
pub mod vignetting_calibration;
pub mod visualize;

pub use curve::{PARAM_AMOUNTS, PARAM_LUT, ParametricCurve};
pub use output::{DeepImage, DeepSamples, DisplaySpace, OutputDepth, OutputSpace, OutputTrc, Proof, ProofLut, proof_profile, register_proof_profile};
pub use visualize::{MaskView, Overlay};

use lcc_develop::{DevelopSettings, Treatment};
use std::any::Any;
use std::borrow::Cow;
use std::sync::{Arc, Mutex};

use lcc_raster::{Coverage, Histogram, Plane, Rgb32f, Rgba8, par_rows};

pub use tone::ToneMap;

/// A raw source's own colour model, for white balance: like Lightroom, a white balance
/// re-evaluates the camera's colour matrices at the chosen white (camera-space white balance, DNG
/// spec ch. 6) instead of adapting the as-shot rendering ([`local::wb_matrix_for`]).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CameraColor {
    /// The file's colour tags (its profile look left out: it is applied at load).
    pub tags: lcc_raw::ColorData,
    /// The white the source pixels were developed for.
    pub developed_for: lcc_color::Xy,
}

/// Which photo a source shows ([`SourceInfo::identity`]), whatever size it was decoded at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SourceIdentity {
    /// The original's file and content.
    pub key: u64,
    /// The original's size (px, either orientation; `[0, 0]` unknown): the shape every decode of
    /// it describes its framing on, so decodes whose sizes round differently (a 683- and a
    /// 684-px-wide preview of one photo) share the photo's analyses.
    pub size: [usize; 2],
}

/// Facts about the source the settings are interpreted against.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceInfo {
    /// Lens corrections embedded in the file (DNG opcodes), relative to the EXIF-oriented source.
    pub lens: Option<lcc_develop::EmbeddedLens>,
    /// `lens` is the file's own correction (a DNG's opcodes, a converted camera correction), not a database profile
    /// matched by the lens name: the decode read it from the file, so the catalog's copy can be brought up to date.
    pub lens_from_file: bool,
    /// Raw sources use absolute Kelvin white balance; rendered sources use a relative scale around
    /// their as-shot white.
    pub raw: bool,
    pub as_shot_temp: f64,
    pub as_shot_tint: f64,
    /// No measured camera illuminant: WB adjustments are relative to the camera's rendered look.
    pub relative_wb: bool,
    /// The Kelvin scale comes from matrices measured for the camera model, not from the file: before those were
    /// used for white balance, this source was edited on the relative scale (6500 K / 0 = as shot), so settings
    /// without a [`lcc_develop::WbScale`] are read that way ([`local::legacy_to_kelvin`]).
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
    /// Where pixel-filter plug-ins come from; `None`: the process default
    /// ([`plugin_filters::set_default_host`]).
    pub filters: Option<plugin_filters::FilterHostRef>,
    /// Long edge (px) of the original photo, when the source handed to the renderer is a smaller
    /// copy of it (the app's thumbnail and preview levels); `None`: the source is the original.
    /// Noise reduction sizes its radii in the original's pixels ([`Plan::detail_scale`]), and
    /// pixel-scale plug-in filters run at that scale ([`source_stage`]), so a preview looks like
    /// the full-size render scaled down (issue #960).
    pub native_long: Option<usize>,
    /// The raw has no colour matrices of its own, but its camera model has matrices from measured
    /// spectral sensitivities, so the photo's Profile option can start from them instead of the
    /// look fitted to its JPEG (`lcc_develop::CameraColour`).
    pub colorimetric: bool,
    /// A DNG whose "camera" is a linear RGB space (Photo Merge, Render to DNG; `lcc_raw::color::is_rendered_rgb`):
    /// no camera profile applies to it.
    pub rendered_rgb: bool,
    /// The source's straight alpha (0..1), EXIF-oriented and the size of the source buffer, when it
    /// has transparency. The pixels handed to the renderer are flattened onto white in linear light
    /// (`lcc_codecs::to_working`, issue #1212); an export that keeps transparency recovers
    /// their colour (`lcc_codecs::unflatten`) and carries this plane through the geometry
    /// beside the render ([`render_alpha`], issue #2976).
    pub alpha: Option<Arc<Plane>>,
    /// A film scanner's infrared pass ([`lcc_codecs::Decoded::infrared`]), the source's size and
    /// orientation: dark under dust, hairs and scratches. Never rendered; Find Film Dust reads it
    /// through the geometry ([`render_plane`]).
    pub infrared: Option<Arc<Plane>>,
    /// The photo's generative fill patches ([`genpatch`]), read from the library by the engine.
    pub generated: Option<genpatch::GenStore>,
    /// Areas content-aware fill never copies from (faces): `[x0, y0, x1, y1]` normalized to the
    /// EXIF-oriented source.
    pub avoid: Option<Arc<Vec<[f64; 4]>>>,
    /// Raw sources: what was done about hot, dead and stuck pixels before demosaic (`None`: not a raw).
    pub pixels: Option<PixelRepair>,
    /// The original's identity (its file and content), the same at every size it is decoded at:
    /// per-photo analyses that take seconds (the Subject and scene models' selections,
    /// [`auto_mask::for_source`]) are kept by it, so the loupe's, a 1:1 window's and an export's
    /// renders share one run. `None`: they are kept by the pixels (a source of another size
    /// runs them again).
    pub identity: Option<SourceIdentity>,
    /// Raw sources: where the developed picture sits in the active area and on the sensor (the same placement the
    /// raw's gain table uses), for a flat-field map ([`vignetting_calibration::field_for`]). `None`: not a raw.
    pub placement: Option<lcc_raw::gaintable::SourcePlacement>,
    /// The flat-field maps the settings may reference by id (`lcc_develop::Optics::vignetting_calibration`), from the catalog: the
    /// session fills it as it fills `generated`. `None`: no map renders.
    pub vignetting_maps: Option<vignetting_calibration::VignettingMapStore>,
}

/// Hot, dead and stuck pixels corrected on a raw's mosaic while it was decoded.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PixelRepair {
    /// Whether correction was on (the photo's `sensor.pixelFix`).
    pub on: bool,
    /// The camera body's key (`lcc_engine::sensor_maps::body_key`), when known.
    pub body: Option<String>,
    /// Samples corrected from the body's map, and found in this photo alone.
    pub from_map: u32,
    pub found: u32,
    /// The file's own orientation (the source is developed upright from it) and the developed sensor area's size
    /// before it: what turns a position on the photo into one on the sensor (sensor dust).
    pub orientation: lcc_geom::Orientation,
    pub developed: [usize; 2],
    /// The full raw data size and camera model (what a body's map is for).
    pub sensor: [usize; 2],
    pub model: String,
}

impl Default for SourceInfo {
    fn default() -> Self {
        Self {
            raw: false,
            as_shot_temp: 6500.0,
            as_shot_tint: 0.0,
            lens: None,
            lens_from_file: false,
            relative_wb: false,
            legacy_relative_wb: false,
            camera_color: None,
            camera_tone: None,
            mattes: None,
            local_tone: None,
            filters: None,
            native_long: None,
            colorimetric: false,
            rendered_rgb: false,
            alpha: None,
            infrared: None,
            generated: None,
            avoid: None,
            pixels: None,
            identity: None,
            placement: None,
            vignetting_maps: None,
        }
    }
}

/// How a render shrinks its source to the output size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Downscale {
    /// Mitchell–Netravali: soft, no visible ringing. Previews and thumbnails.
    #[default]
    Smooth,
    /// Lanczos-3: keeps the detail a smaller output can hold, as a dedicated downscale does
    /// (exports, #2970). Its slight overshoot at hard edges stays below the output sharpening's.
    Sharp,
    /// Interactive previews: [`Sharp`](Self::Sharp) for a full-quality render that shrinks its
    /// source at least [`SHARP_PREVIEW_SHRINK`]×, so sharpening judged in the loupe looks as it will
    /// export; [`Smooth`](Self::Smooth) otherwise (drafts keep their budget, and a near-1:1 view
    /// gains nothing from the wider filter).
    Preview,
}

/// How much a full-quality preview must shrink its source before it uses the export's filter
/// ([`Downscale::Preview`]).
pub const SHARP_PREVIEW_SHRINK: f64 = 2.0;

impl Downscale {
    /// The filter of a render of `quality` that shrinks its source `shrink`× (output px per source px, inverted).
    pub fn filter_for(self, quality: Quality, shrink: f64) -> lcc_raster::resample::Filter {
        let sharp = match self {
            Downscale::Smooth => false,
            Downscale::Sharp => true,
            Downscale::Preview => quality == Quality::Full && shrink >= SHARP_PREVIEW_SHRINK,
        };
        if sharp { lcc_raster::resample::Filter::Lanczos3 } else { lcc_raster::resample::Filter::Mitchell }
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
    /// Output must fit in `max_w × max_h` (aspect preserved). A box far beyond the photo is held
    /// to the render pixel ceiling (`geometry::MAX_RENDER_PIXELS`, or the photo's own pixels when
    /// larger) rather than allocated: see [`geometry::Frame::fit`].
    pub max_w: usize,
    pub max_h: usize,
    /// The box is the final export size, already fitted against the full-resolution photo.
    /// Reduced decode proxies must not fit it a second time.
    pub exact_size: bool,
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
    /// the result is that window's pixels, as in the whole render. See [`PixelWindow`].
    pub window: Option<PixelWindow>,
    /// Previews for a monitor with a display profile: render 8-bit into its primaries instead of
    /// `space` (see [`DisplaySpace`]; ignored by deep renders).
    pub display: Option<DisplaySpace>,
    /// Filter of the downscale to the output size.
    pub downscale: Downscale,
    /// The output keeps the source's transparency (exports to PNG, TIFF and WebP): the caller
    /// renders unflattened colour and asks [`render_alpha`] for the alpha (issue #2976).
    pub alpha: bool,
}

/// A window of the (virtual) full output, in its pixels: what a zoomed view needs, rendered
/// without materializing the whole frame. The output frame is the one `max_w × max_h` fits, after
/// crop, straighten and flips, as drawn. Out-of-range windows are clamped into it. Spatial stages
/// (clarity, dehaze, sharpening…) read their neighbours, so ask for a margin around what is kept.
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

/// The longest output side a render may be asked for (TIFF/PNG/JPEG all fit within it).
pub const MAX_OUTPUT_SIDE: usize = 65_535;
/// The most output pixels a render may be asked for (the box it fits into, ≈ 1.07 Gpx).
pub const MAX_OUTPUT_PIXELS: usize = 1 << 30;

/// Rejects an output box past [`MAX_OUTPUT_SIDE`] or [`MAX_OUTPUT_PIXELS`] before anything is
/// sized from it.
pub fn check_output_size(max_w: usize, max_h: usize) -> Result<(), String> {
    let pixels = max_w.checked_mul(max_h);
    if max_w > MAX_OUTPUT_SIDE || max_h > MAX_OUTPUT_SIDE || pixels.is_none_or(|n| n > MAX_OUTPUT_PIXELS) {
        return Err(format!("output size {max_w}×{max_h} is too large (at most {MAX_OUTPUT_SIDE} px per side and {MAX_OUTPUT_PIXELS} pixels)"));
    }
    Ok(())
}

/// Fails when a visible brush mask holds more dabs than its budget ([`masks::MAX_STROKE_DABS`]) at this render's
/// output size: the render would otherwise skip its later strokes (an erase among them) and show a wrong mask
/// (upstream #3681).
pub fn check_brush_budget(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings, req: &RenderRequest) -> Result<(), String> {
    let brush = |m: &lcc_develop::Mask| m.visible && m.components.iter().any(|c| matches!(c.shape, lcc_develop::MaskShape::Brush { .. }));
    if !s.masks.iter().any(brush) {
        return Ok(());
    }
    let p = plan(src, info, s, req);
    for m in s.masks.iter().filter(|m| brush(m)) {
        for c in &m.components {
            if let lcc_develop::MaskShape::Brush { strokes } = &c.shape
                && masks::brush_over_budget(strokes, &p.frame.for_mask(m.on_photo), p.w, p.h)
            {
                return Err(format!("mask \"{}\": the brush strokes hold more than {} dabs at this size", m.name, masks::MAX_STROKE_DABS));
            }
        }
    }
    Ok(())
}

impl RenderRequest {
    /// A request fitting in `max_w × max_h` (the size ceiling of [`Self::max_w`] applies).
    pub fn fit(max_w: usize, max_h: usize) -> Self {
        Self {
            max_w,
            max_h,
            exact_size: false,
            quality: Quality::Full,
            apply_crop: true,
            overlay: Overlay::None,
            space: OutputSpace::Srgb,
            depth: OutputDepth::U8,
            proof: None,
            window: None,
            display: None,
            downscale: Downscale::Smooth,
            alpha: false,
        }
    }

    /// Render to a final size calculated from the original photo, even from a rounded proxy.
    pub fn exact(width: usize, height: usize) -> Self {
        Self { exact_size: true, ..Self::fit(width, height) }
    }

    /// The output size for `frame`. An exact box stands against a proxy that only rounds the photo's
    /// shape differently; a source of another shape (a raw's embedded preview) is fitted, not
    /// stretched, and a box the render pixel ceiling shrinks is shrunk.
    fn size(&self, frame: &geometry::Frame) -> (usize, usize) {
        let fitted = frame.fit(self.max_w, self.max_h);
        let exact = (self.max_w.max(1), self.max_h.max(1));
        let near = |a: usize, b: usize| a.abs_diff(b) <= b / 64 + 1;
        let under_ceiling = exact.0 as f64 * exact.1 as f64 <= frame.pixel_ceiling();
        if self.exact_size && under_ceiling && near(fitted.0, exact.0) && near(fitted.1, exact.1) { exact } else { fitted }
    }
}

pub struct Rendered {
    /// The result, 8-bit (for deep renders: `deep` reduced to 8 bits, display-encoded).
    pub image: Rgba8,
    pub histogram: Histogram,
    /// The high-bit-depth result when [`RenderRequest::depth`] asks for one.
    pub deep: Option<DeepImage>,
    /// What went wrong without failing the render (e.g. a plug-in filter that isn't installed or
    /// failed and was skipped).
    pub warnings: Vec<String>,
    /// Which pixels of `image` show the photo, when lens corrections or perspective leave a blank
    /// border ([`geometry::Frame::coverage`]); `None` when all of them do. `histogram` leaves the rest out.
    pub coverage: Option<Arc<Coverage>>,
    /// Straight alpha (0..1) of the output, the size of `image`, when the request keeps the
    /// source's transparency and the source has some ([`render_alpha`]).
    pub alpha: Option<Plane>,
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
    /// Log luminance blurred at the sharpen radius (the pixel-scale band sharpening boosts).
    pub sharpen_blur: Option<Arc<Plane>>,
    pub dark: Option<Arc<Plane>>,
    /// Blurred chromaticity (`rgb / Y`) for local Moiré / Noise.
    pub chroma_blur: Option<Arc<Rgb32f>>,
    /// Airlight of `dark` (before exposure).
    pub air: f32,
    pub masks: Vec<masks::Evaluated>,
    /// The masks' own Whites / Blacks tone ranges ([`Plan::mask_tone`]).
    pub mask_tone: Vec<[f32; 4]>,
    /// Output pixels per unit of the source long edge.
    pub px_per_long: f64,
}

/// Output size for a source of `src_w × src_h` under `s`, fitting `max_w × max_h`.
pub fn output_size(src_w: usize, src_h: usize, s: &DevelopSettings, req: &RenderRequest) -> (usize, usize) {
    req.size(&geometry::Frame::new(src_w, src_h, s, req.apply_crop))
}

/// Size of the cropped output of a `src_w × src_h` source under `s` at the source's own resolution
/// (what a full-size export produces).
pub fn native_output_size(src_w: usize, src_h: usize, s: &DevelopSettings) -> (f64, f64) {
    geometry::Frame::new(src_w, src_h, s, true).native_size()
}

/// The geometric frame `render` uses for `src` (lens data, automatic CA estimate included).
pub fn frame_for(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings, apply_crop: bool) -> geometry::Frame {
    let mut frame = geometry::Frame::with_lens(src.width, src.height, s, apply_crop, info.lens.as_ref());
    // (in every frame of the render: the noise model's tiles, masks, spots and Upright see the corrected light too)
    frame.add_vignetting_calibration(vignetting_calibration::field_for(info, s, frame.orient));
    if s.optics.remove_ca && s.section_enabled("optics") {
        frame.add_lateral_ca(optics::estimate_lateral_ca(src));
        if apply_crop {
            // The estimated CA moves the colour planes too: keep the crop inside every one of them.
            frame.constrain_to_image(s.geometry.constrain_crop);
        }
    }
    frame
}

/// Intermediate results of recent renders of one view, reused by [`render_cached`].
///
/// Each stage is keyed by exactly the inputs it depends on: the resampled source by the source
/// buffer, framing and output size; the white-balanced, retouched, denoised image by those plus
/// white balance, spots and noise reduction; each spatial plane by that plus its own radius. So a
/// tone or colour slider drag (or exposure) re-runs only the per-pixel stage, a clarity drag skips
/// resampling and noise reduction, and so on. Holds the last few output sizes (a draft-size and a
/// full-size render of the same view both stay warm).
pub struct StageCache {
    entries: Mutex<Vec<CacheEntry>>,
    capacity: usize,
    /// Another renderer's per-view state (the GPU renderer keeps its device-resident stages here).
    ext: Mutex<Option<Arc<dyn Any + Send + Sync>>>,
}

impl Default for StageCache {
    fn default() -> Self {
        StageCache { entries: Mutex::new(Vec::new()), capacity: 2, ext: Mutex::new(None) }
    }
}

impl StageCache {
    /// Drop everything (e.g. when memory is needed).
    pub fn clear(&self) {
        self.lock().clear();
        source_stage::clear();
        *self.ext.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// Per-view state of type `T` kept alongside this cache (created on first use). Lets another
    /// renderer of the same view (e.g. `lcc-gpu`) keep its own stage cache with this one.
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
        fn size<T>(i: &lcc_raster::Image<T>) -> usize {
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
        for e in self.lock().iter() {
            add(Arc::as_ptr(&e.sampled) as usize, size(&e.sampled));
            if let Some((_, f)) = &e.filtered {
                add(Arc::as_ptr(f) as usize, size(f));
            }
            if let Some((_, l)) = &e.lin {
                add(Arc::as_ptr(l) as usize, size(l));
            }
            if let Some((_, l)) = &e.nr {
                add(Arc::as_ptr(l) as usize, size(l));
            }
            let pl = &e.planes;
            let planes = pl.log_l.iter().chain(pl.base.iter().map(|x| &x.1)).chain(pl.clarity.iter().map(|x| &x.1));
            for p in planes.chain(pl.texture.iter().map(|x| &x.1)).chain(pl.sharpen.iter().map(|x| &x.1)).chain(pl.dark.iter().map(|x| &x.1)) {
                add(Arc::as_ptr(p) as usize, size(p));
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
    /// `sampled` after the Source plug-in filters, keyed by [`Plan::source_key`] (and draft).
    filtered: Option<(u64, Arc<Rgb32f>)>,
    lin: Option<(u64, Arc<Rgb32f>)>,
    /// The denoised image before Smooth Skin (the same buffer as `lin` when Smooth Skin is off): a
    /// Smooth Skin drag redoes only Smooth Skin.
    nr: Option<(u64, Arc<Rgb32f>)>,
    /// The joint colour NR's noise model of the whole frame ([`colour_noise`]), keyed by [`Plan::noise_key`].
    colour_noise: Option<(u64, nrc_joint::JointNoise)>,
    planes: local::Planes,
}

fn hash_of(parts: impl std::hash::Hash) -> u64 {
    use std::hash::{BuildHasher, BuildHasherDefault, DefaultHasher};
    BuildHasherDefault::<DefaultHasher>::default().hash_one(parts)
}

/// What a render resolves to before any pixel work: the effective settings (profile and Upright
/// applied), the geometric frame, the output size and the stage-cache keys. Shared by the CPU
/// renderer and the GPU renderer (`lcc-gpu`), so both key their caches identically.
pub struct Plan<'a> {
    pub settings: Cow<'a, DevelopSettings>,
    pub frame: geometry::Frame,
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
    /// Key of the white-balanced, retouched, denoised, skin-smoothed image (and of the planes
    /// computed from it).
    pub lin_key: u64,
    /// [`Self::lin_key`] of the whole output frame: the same for every window of it (a zoomed
    /// view's tiles), so what is worked out from a window's linear pixels can be kept in the
    /// whole frame's coordinates ([`Self::filter_frame`] places the window) and shared by the
    /// windows around it.
    pub frame_lin_key: u64,
    /// [`Self::lin_key`] before Smooth Skin (equal to it when Smooth Skin is off).
    pub nr_key: u64,
    /// [`Self::lin_key`] without the noise-reduction settings: draft renders reuse the smooth
    /// planes of an image that differs from this one only in its noise reduction.
    pub pre_nr_key: u64,
    /// Key of the whole frame's noise model for the joint colour NR ([`colour_noise`]): what it is
    /// measured after (everything before the colour NR), not the colour NR's own settings.
    pub noise_key: u64,
    /// Red eye / pet eye corrections with their detected pupils, in output pixels.
    pub eyes: Vec<redeye::EyeK>,
    /// The whole frame's dehaze airlight, for a windowed render (which can't see the whole frame).
    pub fixed_air: Option<f32>,
    /// The tone range each visible mask's Whites / Blacks act on, fitted to the tones under it in a fixed-size
    /// render of the whole frame ([`mask_tone`]), so previews, zoomed windows and exports agree; empty when no mask
    /// has Whites or Blacks (the masks then use [`finish::DEFAULT_MASK_TONE`]).
    pub mask_tone: Vec<[f32; 4]>,
    /// A windowed render works on a larger window when spots reach into it (see
    /// [`spots::window_for_reads`]); this is the requested window inside the rendered one, which
    /// the result is cut to.
    pub keep: Option<PixelWindow>,
    /// The source's segmentation mattes ([`SourceInfo::mattes`]).
    pub mattes: Option<Arc<masks::Mattes>>,
    /// The Source-stage plug-in filters (resolved), whose key is part of [`Self::pre_nr_key`].
    pub source_filters: Arc<plugin_filters::Stack>,
    /// Where the rendered pixels sit in the whole output frame (`x, y, full_w, full_h`), for
    /// position-dependent plug-in filters.
    pub filter_frame: lcc_filters::FrameInfo,
    /// The whole output frame (before any window), at which pixel-scale Source filters run
    /// ([`source_stage`]).
    pub whole_frame: geometry::Frame,
    /// The whole output frame's size (`full_w, full_h`; a windowed render is part of it).
    pub full_size: (usize, usize),
}

/// Long edge of the small render a windowed render estimates the whole frame's airlight from.
const AIRLIGHT_PROXY_EDGE: usize = 384;

/// Resolve `s` against `src` for `req` (see [`Plan`]).
pub fn plan<'a>(src: &Rgb32f, info: &SourceInfo, s: &'a DevelopSettings, req: &RenderRequest) -> Plan<'a> {
    // the whole-photo helpers below read the photo as the Source plug-in filters turn it
    let aux_cell = std::cell::OnceCell::new();
    let aux = || -> &Rgb32f { aux_cell.get_or_init(|| source_stage::aux_source(src, info, s)).as_deref().unwrap_or(src) };
    let mut settings: Cow<'a, DevelopSettings> = match profiles::effective(s) {
        Cow::Borrowed(b) => upright::resolve(if upright::pending(b) { aux() } else { src }, info, b),
        Cow::Owned(o) => Cow::Owned(upright::resolve(if upright::pending(&o) { aux() } else { src }, info, &o).into_owned()),
    };
    visualize::adjust_settings(req.overlay, &mut settings);
    // a window must not choose a spot's source from the pixels it happens to hold: fix them
    // from a fixed-size render of the whole frame first
    if req.window.is_some() && settings.spots.iter().any(|sp| sp.source_offset.is_none()) {
        let picked: Vec<Option<lcc_geom::Point>> =
            settings.spots.iter().map(|sp| sp.source_offset.or_else(|| spots::pick_source(aux(), info, &settings, sp, None))).collect();
        for (sp, o) in settings.to_mut().spots.iter_mut().zip(picked) {
            sp.source_offset = o;
        }
    }
    let s = &*settings;
    let mut frame = frame_for(src, info, s, req.apply_crop);
    let (full_w, full_h) = req.size(&frame);
    let shrink = frame.crop.rect_px(frame.ow, frame.oh).width() / full_w.max(1) as f64;
    frame.downscale = req.downscale.filter_for(req.quality, shrink);
    let whole_frame = frame.clone();
    // sizes and scale are those of the whole output; a window only narrows what is drawn
    let px_per_long = frame.px_per_long(full_w);
    let src_long = src.width.max(src.height);
    // (an original reported smaller than the source is the source)
    let native_long = info.native_long.unwrap_or(0).max(src_long).max(1);
    let detail_scale = local::detail_scale(px_per_long, native_long);
    let mut fixed_air = None;
    let mut keep = None;
    let mut origin = (0, 0);
    let (frame, w, h) = match req.window {
        Some(win) => {
            let win = win.clamped(full_w, full_h);
            if s.section_enabled("effects") && (s.effects.dehaze != 0.0 || s.masks.iter().any(|m| m.adjust.dehaze != 0.0)) {
                let k = (AIRLIGHT_PROXY_EDGE as f64 / full_w.max(full_h) as f64).min(1.0);
                let (pw, ph) = (((full_w as f64 * k).round() as usize).max(1), ((full_h as f64 * k).round() as usize).max(1));
                fixed_air = Some(local::frame_airlight(aux(), info, s, &frame, pw, ph));
            }
            // (spots grow the rendered window; only 8-bit renders are cut back to the request)
            let work = if req.depth == OutputDepth::U8 { spots::window_for_reads(s, &frame, full_w, full_h, px_per_long, win) } else { win };
            // and so does Smooth Skin, which reads the skin around what it changes
            let work = if req.depth == OutputDepth::U8 { grow_window(work, skin::reach(s, px_per_long), full_w, full_h) } else { work };
            // (the retouch of people's parts is per pixel: retouch::reach is 0)
            let work = if req.depth == OutputDepth::U8 { grow_window(work, retouch::reach(s, px_per_long), full_w, full_h) } else { work };
            // The joint colour NR (process V8) reads a chain of neighbours across its scales: keep
            // those reads outside the requested window too, at every depth (cut back below)
            let work = grow_window(work, joint_nr_reach(s, detail_scale), full_w, full_h);
            if work != win {
                keep = Some(PixelWindow { x: win.x - work.x, y: win.y - work.y, ..win });
            }
            origin = (work.x, work.y);
            (frame.window(full_w, full_h, work), work.w, work.h)
        }
        None => (frame, full_w, full_h),
    };
    let u = |v: usize| u32::try_from(v).unwrap_or(u32::MAX);
    let filter_frame = lcc_filters::FrameInfo { x: u(origin.0), y: u(origin.1), full_w: u(full_w), full_h: u(full_h) };
    let source_filters = Arc::new(plugin_filters::Stack::resolve(info, s, lcc_develop::FilterStage::Source));
    let geo = hash_of((format!("{frame:?}"), w, h));
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
        src_long,
        // the Source plug-in filters run before this stage
        source_filters.key,
        // and film dust removal before them
        film_dust_key(info, s),
    ));
    // a generated patch whose file appears (or goes) changes the image
    let shown: Vec<bool> = genpatch::shown(&s.spots, info.generated.as_ref()).iter().map(Option::is_some).collect();
    // (what makes the linear image besides the window: the window's own geometry and red-eye
    // positions are left out, so every window of the frame shares it)
    let frame_lin_key = hash_of((
        format!("{whole_frame:?}"),
        (full_w, full_h),
        [wb_t, wb_tint, info.as_shot_temp, info.as_shot_tint].map(f64::to_bits),
        format!("{:?}", s.spots),
        format!("{:?}", s.red_eye),
        format!("{:?}", s.optics),
        local_tone::enabled(info, s),
        src_long,
        source_filters.key,
        film_dust_key(info, s),
        &shown,
        format!("{:?}", info.avoid),
    ));
    let pre_nr_key = hash_of((pre_nr_key, shown, format!("{:?}", info.avoid)));
    // a draft's colour NR may take the half-resolution blur: its result is never a final render's
    // (the joint colour NR of V8 has no draft form)
    let joint_nr = local::joint_colour_nr(s);
    let draft_nr = req.quality == Quality::Draft && d.nr_color > 0.0 && !joint_nr;
    let nr_key = hash_of((
        pre_nr_key,
        [d.nr_luminance, d.nr_detail, d.nr_color, d.nr_color_detail, d.nr_color_smoothness].map(f64::to_bits),
        detail_scale.to_bits(),
        draft_nr,
        // the process picks the colour filter (Update to Current Process, undo)
        joint_nr,
    ));
    let noise_key = hash_of((pre_nr_key, [d.nr_luminance, d.nr_detail].map(f64::to_bits), detail_scale.to_bits()));
    let lin_key = match (skin::key(s), retouch::key(s)) {
        (None, None) => nr_key,
        (k, r) => hash_of((nr_key, k, r)),
    };
    let frame_lin_key = hash_of((
        frame_lin_key,
        [d.nr_luminance, d.nr_detail, d.nr_color, d.nr_color_detail, d.nr_color_smoothness].map(f64::to_bits),
        detail_scale.to_bits(),
        draft_nr,
        joint_nr,
        skin::key(s),
        retouch::key(s),
    ));
    // AI masks without a model: analysed once per photo look on the whole frame (cached)
    let mattes = if auto_mask::needed(s) {
        let mut m = info.mattes.as_deref().cloned().unwrap_or_default();
        m.auto = Some(auto_mask::for_source(aux(), info, s));
        Some(Arc::new(m))
    } else {
        info.mattes.clone()
    };
    let mask_tone = mask_tone::ranges(aux(), info, s, &whole_frame, mattes.as_deref());
    Plan {
        settings,
        frame,
        w,
        h,
        px_per_long,
        src_long,
        detail_scale,
        geo,
        lin_key,
        frame_lin_key,
        nr_key,
        pre_nr_key,
        noise_key,
        eyes,
        fixed_air,
        mask_tone,
        keep,
        mattes,
        source_filters,
        filter_frame,
        whole_frame,
        full_size: (full_w, full_h),
    }
}

/// How far (px) the joint colour NR of `s` reads around a pixel at `detail_scale`
/// ([`local::nrc_params`], [`nrc_joint::reach`]: the GPU's band halo): a window is grown by it and
/// cut back. 0 when it is off.
fn joint_nr_reach(s: &DevelopSettings, detail_scale: f32) -> f64 {
    if !local::joint_colour_nr(s) {
        return 0.0;
    }
    // the plan of an image larger than any window (a smaller one only stops the plan sooner)
    local::nrc_params(s, detail_scale)
        .and_then(|p| p.passes.last().map(|p| nrc_joint::reach(&nrc_joint::scale_plan(p.sigma, usize::MAX, usize::MAX)) as f64))
        .unwrap_or(0.0)
}

/// The joint colour NR's noise model of the whole frame, measured on fixed tiles of it
/// ([`nrc_joint::tiles`]) so that every window of it is filtered alike. The tiles are resampled,
/// tone-mapped (the camera's local tone map), white-balanced and luminance-denoised like the frame;
/// the retouching and defringing are left out. `None` when the joint colour NR is off, when the
/// render is the whole frame and that is a single tile (the noise is then measured on the image
/// itself, as the CPU and the GPU do), or when Source plug-in filters run (the tiles can't take
/// them; the noise is measured on the rendered pixels).
pub fn colour_noise(src: &Rgb32f, info: &SourceInfo, p: &Plan<'_>) -> Option<nrc_joint::JointNoise> {
    let s = &*p.settings;
    if !local::joint_colour_nr(s) || local::nrc_params(s, p.detail_scale).is_none() || !p.source_filters.is_empty() {
        return None;
    }
    let full = &p.whole_frame;
    let (fw, fh) = p.full_size;
    let windows = nrc_joint::tiles(fw, fh);
    if (p.w, p.h) == (fw, fh) && windows.len() == 1 {
        return None;
    }
    let t = profiling().then(std::time::Instant::now);
    let (lum, _) = local::nr_params(s, p.detail_scale);
    // the source oriented and prefiltered once for all tiles
    let sampled = full.sample_windows(src, fw, fh, &windows);
    let tiles: Vec<Rgb32f> = windows
        .into_iter()
        .zip(sampled)
        .map(|(t, mut img)| {
            let frame = if (t.w, t.h) == (fw, fh) { full.clone() } else { full.window(fw, fh, t) };
            local_tone::apply_frame(&mut img, info, s, &frame);
            local::white_balance(&mut img, info, s);
            local::apply_nr(&mut img, lum, None, false);
            img
        })
        .collect();
    let noise = nrc_joint::noise_of_tiles(&tiles);
    if let Some(t) = t {
        eprintln!("    nr colour noise model ({} tiles): {:.0} ms", tiles.len(), t.elapsed().as_secs_f64() * 1e3);
    }
    Some(noise)
}

/// `win` grown by `reach` pixels on every side, inside the `full_w × full_h` output.
fn grow_window(win: PixelWindow, reach: f64, full_w: usize, full_h: usize) -> PixelWindow {
    if !(reach.is_finite() && reach >= 1.0) {
        return win;
    }
    let r = reach.min((full_w.max(full_h)) as f64) as usize;
    let (x0, y0) = (win.x.saturating_sub(r), win.y.saturating_sub(r));
    let (x1, y1) = ((win.x + win.w).saturating_add(r).min(full_w), (win.y + win.h).saturating_add(r).min(full_h));
    PixelWindow { x: x0, y: y0, w: x1.saturating_sub(x0).max(1), h: y1.saturating_sub(y0).max(1) }
}

/// Whether Smooth Skin or the retouch of people's parts (Teeth) changes this render (they run after noise
/// reduction; the GPU renderer does Smooth Skin itself and the retouch on the CPU).
pub fn skin_needs_cpu(s: &DevelopSettings) -> bool {
    skin::analysis(s).is_some() || retouch::active(s)
}

/// Smooth Skin, then the retouch of people's parts, on the noise-reduced scene-linear image (both
/// renderers run this, so they match).
pub fn retouch_people(img: &mut Rgb32f, s: &DevelopSettings, frame: &geometry::Frame, px_per_long: f64) {
    timed("smooth skin", || skin::apply(img, s, frame, px_per_long));
    timed("retouch", || retouch::apply(img, s, frame, px_per_long));
}

/// Whether the scene-linear stage needs work only the CPU does (content-aware Fill, Generate and
/// sourceless Remove spots, Even Out, the camera's local tone map).
pub fn lin_needs_cpu(s: &DevelopSettings, info: &SourceInfo) -> bool {
    // defringe and Clone / Heal / sourced Remove spots have GPU kernels; spots rebuilt from their
    // surroundings (Fill, Generate, a Remove without a source), Even Out and the camera's local
    // tone map don't
    s.spots.iter().any(|sp| spots::synthesises(sp) || sp.mode == lcc_develop::SpotMode::Flat) || local_tone::enabled(info, s)
}

/// The white-balanced, defringed, retouched image (before noise reduction): the CPU part of the
/// scene-linear stage, in place.
pub fn lin_cpu(img: &mut Rgb32f, info: &SourceInfo, p: &Plan<'_>) {
    let s = &*p.settings;
    // generated patches are source pixels: before anything else touches them
    let patches = genpatch::shown(&s.spots, info.generated.as_ref());
    for (sp, layer) in s.spots.iter().zip(&patches) {
        if let (Some(g), Some(layer)) = (sp.shown_gen(), layer) {
            genpatch::apply_layer(img, layer, &p.frame, genpatch::seed_of(&g.id, g.variant));
        }
    }
    // (before the white balance: the gain's input is the as-shot development, as in the file)
    local_tone::apply(img, info, p);
    local::white_balance(img, info, s);
    optics::defringe(img, s, p.px_per_long / optics::DEFRINGE_REF_LONG);
    // a Generative spot changes nothing until its patch is there (not generated yet, moved since,
    // or its file is missing): no content-aware stand-in that looks like a poor AI result
    let skip: Vec<bool> = s.spots.iter().zip(&patches).map(|(sp, p)| p.is_some() || sp.mode == lcc_develop::SpotMode::Generate).collect();
    let avoid = genpatch::avoid_boxes(info.avoid.as_deref().map_or(&[][..], Vec::as_slice), &p.frame, img.width, img.height);
    spots::apply_skipping(img, &s.spots, &p.frame, p.px_per_long, &skip, &avoid);
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
/// whole HDR range ([`Hdr::MAX_EV_LIMIT`](lcc_develop::Hdr::MAX_EV_LIMIT) stops above SDR
/// white, whatever the headroom limit, so the scale stays put) on the right ([`Histogram::of_hdr`]). A preview of
/// an HDR edit shows its SDR rendition; this says how far its highlights go. `None` for SDR edits,
/// high-bit-depth, windowed and proofed requests (their histogram stays the image's).
pub fn hdr_histogram(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings, req: &RenderRequest) -> Option<Histogram> {
    hdr_histogram_with(s, req, |hreq| render(src, info, s, hreq))
}

/// [`hdr_histogram`], its render reusing (and refreshing) the intermediate results in `cache`, as
/// [`render_cached`] does: the same histogram. A slider drag of an HDR edit then redoes only the
/// stages the slider feeds (a loupe rendered on the GPU leaves its cache's CPU entries free).
pub fn hdr_histogram_cached(src: &Arc<Rgb32f>, info: &SourceInfo, s: &DevelopSettings, req: &RenderRequest, cache: &StageCache) -> Option<Histogram> {
    hdr_histogram_with(s, req, |hreq| render_cached(src, info, s, hreq, cache))
}

fn hdr_histogram_with(s: &DevelopSettings, req: &RenderRequest, render: impl FnOnce(&RenderRequest) -> Rendered) -> Option<Histogram> {
    // a drag's loupe-sized drafts keep their own histogram (a second render per tick would slow
    // them down); a small draft (the drag's own histogram request, `HDR_HISTOGRAM_DRAG_EDGE`) gets
    // a draft-grade HDR pass of its size
    let small = req.max_w.max(req.max_h) <= HDR_HISTOGRAM_DRAG_EDGE;
    if !s.hdr.enabled || req.depth != OutputDepth::U8 || req.window.is_some() || req.proof.is_some() || (req.quality == Quality::Draft && !small) {
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
    let r = render(&hreq);
    let deep = r.deep?;
    let DeepSamples::F32(v) = &deep.samples else { return None };
    Some(Histogram::of_hdr_image(v.as_chunks::<3>().0, deep.width, r.coverage.as_deref(), lcc_develop::Hdr::MAX_EV_LIMIT as f32))
}

/// Render `src` with settings `s`.
pub fn render(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings, req: &RenderRequest) -> Rendered {
    render_impl(Src::Borrowed(src), info, s, req, None)
}

/// [`render`], reusing (and refreshing) the intermediate results in `cache`. The output is
/// identical to [`render`]'s.
pub fn render_cached(src: &Arc<Rgb32f>, info: &SourceInfo, s: &DevelopSettings, req: &RenderRequest, cache: &StageCache) -> Rendered {
    render_impl(Src::Shared(src), info, s, req, Some(cache))
}

/// The source's alpha ([`SourceInfo::alpha`]) through the same geometry as [`render`] of the same
/// arguments: orientation, crop, straighten, flips, lens corrections, Transform / Upright and the
/// output size. Develop settings never touch it; areas the warp brings in from outside the source
/// are transparent. `None` when the source has no alpha or a plane that does not match `src`.
pub fn render_alpha(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings, req: &RenderRequest) -> Option<Plane> {
    render_plane(src, info.alpha.as_deref()?, info, s, req)
}

/// A plane of the source's size (its alpha, a scanner's infrared pass) through the same geometry
/// as [`render`] of the same arguments (see [`render_alpha`]). `None` when the plane is not the
/// size of `src`.
pub fn render_plane(src: &Rgb32f, plane: &Plane, info: &SourceInfo, s: &DevelopSettings, req: &RenderRequest) -> Option<Plane> {
    if plane.width != src.width || plane.height != src.height {
        return None;
    }
    let effective = s.effective();
    let rendition = settings_for(&effective, req);
    let plan = plan(src, info, &rendition, req);
    let a = plan.frame.sample_alpha(plane, plan.w, plan.h);
    // (a grown window is cut back to the request, as in `render_impl`)
    Some(match plan.keep {
        Some(k) => a.crop(k.x, k.y, k.w, k.h),
        None => a,
    })
}

/// What film dust removal does to a render: nothing without the pass or while off, else its settings.
fn film_dust_key(info: &SourceInfo, s: &DevelopSettings) -> u64 {
    match info.infrared.as_ref().filter(|_| s.film_dust.on()) {
        Some(ir) => hash_of((Arc::as_ptr(ir) as usize, ir.width, ir.height, s.film_dust.sensitivity.to_bits(), s.film_dust.expand.to_bits())),
        None => 0,
    }
}

enum Src<'a> {
    Borrowed(&'a Rgb32f),
    Shared(&'a Arc<Rgb32f>),
}

fn render_impl(src: Src<'_>, info: &SourceInfo, s: &DevelopSettings, req: &RenderRequest, cache: Option<&StageCache>) -> Rendered {
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
    let Plan { ref frame, w, h, px_per_long, detail_scale, geo, lin_key, nr_key, .. } = plan;
    let s = &*plan.settings;
    let mut warnings = Vec::new();

    let shared = match (&src, cache) {
        (Src::Shared(a), Some(c)) => Some((*a, c)),
        _ => None,
    };
    let cached = shared.and_then(|(a, c)| c.get(a, geo));
    let sampled = match &cached {
        Some(e) => e.sampled.clone(),
        None => Arc::new(frame.sample(src_img, w, h)),
    };
    lap("sample", &mut t);
    // film dust: the infrared pass's marks filled in before anything reads the pixels
    let sampled = if s.film_dust.on() && info.infrared.is_some() {
        let mut img = (*sampled).clone();
        film_dust::stage(&mut img, src_img, info, s, frame);
        lap("film dust", &mut t);
        Arc::new(img)
    } else {
        sampled
    };

    // Source plug-in filters: cached by their stack; a draft that can't reuse a result runs them
    // on a reduced copy (its key marked, so a full render never takes it, nor what follows it)
    let stack = &plan.source_filters;
    let draft = req.quality == Quality::Draft;
    let draft_key = hash_of((stack.key, "draft"));
    let mut filtered = None;
    let mut draft_src = false;
    if !stack.is_empty() {
        let hit = cached.as_ref().and_then(|e| e.filtered.clone()).filter(|(k, _)| *k == stack.key || (draft && *k == draft_key));
        filtered = Some(match hit {
            Some(h) => h,
            None => source_stage::run(src_img, info, &plan, &sampled, draft, draft_key, &mut warnings),
        });
        draft_src = filtered.as_ref().is_some_and(|(k, _)| *k == draft_key);
        lap("plug-in filters (source)", &mut t);
    }
    let (lin_key, nr_key) = if draft_src { (hash_of((lin_key, "draft source")), hash_of((nr_key, "draft source"))) } else { (lin_key, nr_key) };
    let base = filtered.as_ref().map_or(&sampled, |(_, i)| i);

    let cached_nr = cached.as_ref().and_then(|e| e.nr.clone()).filter(|(k, _)| *k == nr_key);
    let mut colour_noise = cached.as_ref().and_then(|e| e.colour_noise).filter(|(k, _)| *k == plan.noise_key);
    let (lin, nr) = match cached.as_ref().and_then(|e| e.lin.clone()).filter(|(k, _)| *k == lin_key) {
        Some((_, img)) => (img, cached_nr.map(|(_, n)| n)),
        None => {
            let nr = match cached_nr {
                Some((_, img)) => img,
                None => {
                    // Without a cache (or filters) the resampled buffer is ours: work on it in place.
                    let mut img = if shared.is_some() || filtered.is_some() { (**base).clone() } else { Arc::unwrap_or_clone(sampled.clone()) };
                    lin_cpu(&mut img, info, &plan);
                    if colour_noise.is_none() {
                        colour_noise = self::colour_noise(src_img, info, &plan).map(|n| (plan.noise_key, n));
                    }
                    local::denoise_with(&mut img, s, detail_scale, req.quality == Quality::Draft, colour_noise.as_ref().map(|c| &c.1));
                    Arc::new(img)
                }
            };
            if lin_key == nr_key {
                (nr.clone(), Some(nr))
            } else {
                let mut img = (*nr).clone();
                retouch_people(&mut img, s, frame, px_per_long);
                (Arc::new(img), Some(nr))
            }
        }
    };
    lap("wb/spots/nr/skin", &mut t);
    let mut planes = match cached.map(|e| e.planes) {
        Some(p) if p.key == lin_key => p,
        // A draft while noise reduction changes (a slider drag): the wide, edge-aware planes of the
        // image before the change stand in (noise reduction barely moves a blur that wide); the
        // per-pixel log luminance and the fine texture band are redone. Keyed so a full-quality
        // render never takes them.
        Some(p) if req.quality == Quality::Draft && p.pre_nr == plan.pre_nr_key => p.for_other_nr(),
        _ => local::Planes { key: lin_key, pre_nr: plan.pre_nr_key, ..Default::default() },
    };
    let mut prep = local::prepare(lin.clone(), s, frame, px_per_long, req.quality, &mut planes, plan.mattes.as_ref());
    if let Some(a) = plan.fixed_air {
        prep.air = a;
    }
    prep.mask_tone = plan.mask_tone.clone();
    lap("prepare", &mut t);
    if let Some((a, c)) = shared {
        // (a render whose filters stopped short at its cancel keeps nothing: its pixels aren't the stack's)
        if !was_cut() {
            c.put(CacheEntry {
                src: a.clone(),
                geo,
                sampled,
                filtered,
                lin: Some((lin_key, lin)),
                nr: nr.map(|n| (nr_key, n)),
                colour_noise,
                planes,
            });
        }
    }
    let display_filters = plugin_filters::Stack::resolve(info, s, lcc_develop::FilterStage::Display);
    let post = finish::Post { stack: &display_filters, frame: plan.filter_frame, warnings: std::cell::RefCell::new(&mut warnings) };
    if req.depth != OutputDepth::U8 {
        let deep = finish::finish_deep(&prep, s, frame, info, req.space, req.depth, req.proof, &post);
        // (a window the joint colour NR grew is cut back to the request)
        let deep = match plan.keep {
            Some(k) => deep.crop(k.x, k.y, k.w, k.h),
            None => deep,
        };
        let image = deep.to_rgba8();
        let coverage = frame.coverage(src_img.width, src_img.height, w, h).map(|c| match plan.keep {
            Some(k) => Arc::new(c.crop(k.x, k.y, k.w, k.h)),
            None => Arc::new(c),
        });
        let histogram = Histogram::of_srgb8_covered(&image, coverage.as_deref());
        lap("finish (deep)", &mut t);
        return Rendered { image, histogram, deep: Some(deep), warnings, coverage, alpha: None };
    }
    let image = finish::finish(&prep, s, frame, info, req.space, req.display.as_ref(), req.proof, &post);
    lap("finish", &mut t);
    let cut = |i: &Rgba8| match plan.keep {
        Some(k) => i.crop(k.x, k.y, k.w, k.h),
        None => i.clone(),
    };
    // the blank border past the photo's edge is shown but isn't part of the photo (issue #211)
    let coverage = frame.coverage(src_img.width, src_img.height, w, h).map(|c| match plan.keep {
        Some(k) => Arc::new(c.crop(k.x, k.y, k.w, k.h)),
        None => Arc::new(c),
    });
    let histogram = match plan.keep {
        Some(_) => Histogram::of_srgb8_covered(&cut(&image), coverage.as_deref()),
        None => Histogram::of_srgb8_covered(&image, coverage.as_deref()),
    };
    lap("histogram", &mut t);
    let mut image = image;
    let mask = if matches!(req.overlay, Overlay::FilmDust | Overlay::Infrared) {
        film_dust::mask_for(info, src_img, s, frame, w, h)
    } else {
        overlay_alpha(req.overlay, &plan, &prep)
    };
    let masks = overlay_alphas_all(req.overlay, &plan, &prep);
    visualize::apply(&mut image, req.overlay, &plan, mask.as_ref(), masks.as_deref());
    if req.overlay == Overlay::Infrared
        && let Some(ir) = film_dust::infrared_for(info, frame, w, h)
    {
        film_dust::paint_infrared(&mut image, &ir, mask.as_ref());
    }
    let mut image = if plan.keep.is_some() { cut(&image) } else { image };
    if req.overlay == Overlay::HdrRange && edit.hdr.enabled {
        // the HDR rendition of the same request says how far above SDR white each pixel goes
        let hreq = RenderRequest { depth: OutputDepth::F32Hdr, overlay: Overlay::None, proof: None, ..*req };
        if let Some(hdr) = render(src_img, info, edit, &hreq).deep {
            visualize::hdr_range(&mut image, &hdr, req.space.luma());
        }
        lap("visualize hdr", &mut t);
    }
    Rendered { image, histogram, deep: None, warnings, coverage, alpha: None }
}

/// The colour a colour-range mask would sample at normalized image point `p` (OkLab of the
/// exposed scene colours, as [`MaskShape::ColorRange`](lcc_develop::MaskShape) compares
/// them), averaged over 3 × 3 pixels of a render fitting `req`. `None` outside the image.
pub fn color_range_sample(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings, req: &RenderRequest, p: lcc_geom::Point) -> Option<[f64; 3]> {
    // Pick from the same settings as the render: disabled panels contribute nothing, and an
    // SDR preview of an HDR edit includes the SDR rendition's exposure offset.
    let effective = s.effective();
    let rendition = settings_for(&effective, req);
    let plan = plan(src, info, &rendition, req);
    let mut img = plan.frame.sample(src, plan.w, plan.h);
    if !plan.source_filters.is_empty() {
        img = Arc::unwrap_or_clone(source_stage::run(src, info, &plan, &img, false, 0, &mut Vec::new()).1);
    }
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
            let lab = lcc_color::perceptual::oklab_from_2020(masks::tonemap_for_select(c));
            for k in 0..3 {
                acc[k] += lab[k] as f64;
            }
            n += 1.0;
        }
    }
    Some(acc.map(|v| v / n))
}

/// What the Colour Range and Luminance Range pickers see in `area` (normalized `[x0, y0, x1, y1]`
/// of the oriented, cropped frame; a point, `x0 == x1`, reads the 3 × 3 pixels round it): each
/// pixel's OkLab colour as the colour range compares it, and its luminance on the luminance
/// range's 0..1 scale (−8..+4 EV, exposure included). Rendered as `req` asks (a small preview is
/// plenty); at most 65 536 pixels, evenly spread over a large area. `None` when the area misses
/// the photo.
pub fn range_area_pixels(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings, req: &RenderRequest, area: [f64; 4]) -> Option<Vec<([f64; 3], f64)>> {
    if !area.iter().all(|v| v.is_finite()) {
        return None;
    }
    let effective = s.effective();
    let rendition = settings_for(&effective, req);
    let plan = plan(src, info, &rendition, req);
    if plan.w == 0 || plan.h == 0 {
        return None;
    }
    let mut img = plan.frame.sample(src, plan.w, plan.h);
    lin_cpu(&mut img, info, &plan);
    let to = plan.frame.norm_to_out(plan.w, plan.h);
    let (a, b) = (to.apply(lcc_geom::Point::new(area[0], area[1])), to.apply(lcc_geom::Point::new(area[2], area[3])));
    let (mut x0, mut x1) = (a.x.min(b.x).floor() as i64, a.x.max(b.x).floor() as i64);
    let (mut y0, mut y1) = (a.y.min(b.y).floor() as i64, a.y.max(b.y).floor() as i64);
    if x0 == x1 && y0 == y1 {
        // a click: the pixels round it
        (x0, x1, y0, y1) = (x0 - 1, x1 + 1, y0 - 1, y1 + 1);
    }
    let (x0, y0) = (x0.max(0), y0.max(0));
    let (x1, y1) = (x1.min(plan.w as i64 - 1), y1.min(plan.h as i64 - 1));
    if x0 > x1 || y0 > y1 {
        return None;
    }
    let (n_x, n_y) = ((x1 - x0 + 1) as u64, (y1 - y0 + 1) as u64);
    // every k-th pixel each way keeps a large area to about 65 536 of them
    let step = ((n_x.saturating_mul(n_y) as f64 / 65_536.0).sqrt().ceil() as i64).max(1);
    let ev = plan.settings.light.exposure as f32;
    let gain = ev.exp2();
    let mut out = Vec::new();
    let mut y = y0;
    while y <= y1 {
        let mut x = x0;
        while x <= x1 {
            if let Some(c) = img.data.get(y as usize * plan.w + x as usize) {
                let lab = lcc_color::perceptual::oklab_from_2020(masks::tonemap_for_select(c.map(|v| v * gain)));
                let lum = ((local::log_lum(*c) + ev + 8.0) / 12.0).clamp(0.0, 1.0);
                if lab.iter().all(|v| v.is_finite()) && lum.is_finite() {
                    out.push((lab.map(f64::from), f64::from(lum)));
                }
            }
            x += step;
        }
        y += step;
    }
    (!out.is_empty()).then_some(out)
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
    b.wb = lcc_develop::WhiteBalance { mode: lcc_develop::WbMode::AsShot, ..b.wb };
    b
}

pub(crate) fn is_bw(s: &DevelopSettings) -> bool {
    s.treatment == Treatment::Bw || s.profile.id == "lc.mono" || s.profile.id.starts_with("lc.bw.")
}

thread_local! {
    /// The flag that says this thread's render is no longer wanted ([`with_cancel`]), and whether a
    /// step stopped short because of it.
    static CANCEL: std::cell::RefCell<(Option<Arc<std::sync::atomic::AtomicBool>>, bool)> = const { std::cell::RefCell::new((None, false)) };
}

/// Runs `f` (a render on this thread) with `flag` as its cancel: once it is set, the plug-in
/// filters still to run stop (a render a newer edit replaced would otherwise run its whole stack,
/// beside the new one, each edit adding one more). The second value is true when something stopped
/// short: the result is then not the picture asked for, and nothing of it was cached.
pub fn with_cancel<R>(flag: Option<Arc<std::sync::atomic::AtomicBool>>, f: impl FnOnce() -> R) -> (R, bool) {
    let before = CANCEL.with(|c| c.replace((flag, false)));
    let r = f();
    let (_, cut) = CANCEL.with(|c| c.replace(before));
    (r, cut)
}

/// This thread's render's cancel flag ([`with_cancel`]).
pub(crate) fn cancel_flag() -> Option<Arc<std::sync::atomic::AtomicBool>> {
    CANCEL.with(|c| c.borrow().0.clone())
}

/// A step of this thread's render stopped short at its cancel.
pub(crate) fn note_cut() {
    CANCEL.with(|c| c.borrow_mut().1 = true);
}

/// Whether a step of this thread's render stopped short ([`note_cut`]): what it made is not to be kept.
pub(crate) fn was_cut() -> bool {
    CANCEL.with(|c| c.borrow().1)
}

/// `{ENV_PREFIX}_PROFILE` is set: print per-stage timings to stderr.
pub fn profiling() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| lcc_brand::env_is_set("PROFILE"))
}

/// Run `f`, printing its duration under `{ENV_PREFIX}_PROFILE`.
pub fn timed<R>(what: &str, f: impl FnOnce() -> R) -> R {
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

/// `plan` must keep the output box the caller (export) already decided instead of
/// re-fitting it: `Frame::fit` passes a box through unchanged only when it matches
/// the *decoded* frame's aspect within rounding slack, so a reduced source whose aspect
/// rounds differently (a 149 x 100 proxy of a 240 x 160 photo) makes `plan` return a size
/// a pixel off from the box.
#[cfg(test)]
#[test]
fn plan_preserves_matching_box_size() {
    let src = Rgb32f::from_fn(149, 100, |_, _| [0.5; 3]);
    let info = SourceInfo { native_long: Some(240), ..SourceInfo::default() };
    let s = DevelopSettings::default();
    let fitted = plan(&src, &info, &s, &RenderRequest::fit(240, 160));
    assert_eq!((fitted.w, fitted.h), (238, 160));
    let req = RenderRequest::exact(240, 160);
    let p = plan(&src, &info, &s, &req);
    assert_eq!((p.w, p.h), (240, 160));
    // an embedded preview of another shape keeps its shape: an exact box must not stretch it
    let preview = Rgb32f::from_fn(120, 100, |_, _| [0.5; 3]);
    let p = plan(&preview, &info, &s, &req);
    assert_eq!((p.w, p.h), (192, 160));
}

/// An exact box is still held to the render pixel ceiling: one just past it (here 512.3 MP, which
/// the ceiling shrinks by less than the slack an exact box is allowed) comes back fitted under it.
#[cfg(test)]
#[test]
fn an_exact_box_past_the_pixel_ceiling_is_shrunk() {
    let s = DevelopSettings::default();
    let (w, h) = output_size(3000, 2000, &s, &RenderRequest::exact(27_720, 18_480));
    assert!(w as f64 * h as f64 <= geometry::MAX_RENDER_PIXELS, "{w}x{h} is past the render pixel ceiling");
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_dehaze;
#[cfg(test)]
mod tests_geometry;
#[cfg(test)]
mod tests_hdr;
#[cfg(test)]
mod tests_local_curve;
#[cfg(test)]
mod tests_mask_geometry;
#[cfg(test)]
mod tests_mask_point_color;
#[cfg(test)]
mod tests_nr_scale;
#[cfg(test)]
mod tests_pixel_scale;
#[cfg(test)]
mod tests_plugin_filters;
#[cfg(test)]
mod tests_retouch;
#[cfg(test)]
mod tests_sharpen;
#[cfg(test)]
mod tests_skin;
#[cfg(test)]
mod tests_tone;
#[cfg(test)]
mod tests_window;
