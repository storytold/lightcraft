//! Photo Merge (HDR, Panorama, HDR Panorama, Focus Stack) in the library.
//!
//! A merge is planned from the session ([`crate::Session::plan_merge`] → [`MergeJob`], which is
//! `Send` and self-contained), run anywhere (a worker thread in the app, inline for the CLI/MCP:
//! [`MergeJob::run`], with progress and cancellation), and finished on the session
//! ([`crate::Session::finish_merge`]): the DNG is written next to the first source photo
//! (`<name>-HDR.dng`, `-Pano.dng`, `-HDR-Pano.dng`, `-Focus.dng`), imported, selected, and given Auto Settings /
//! the auto-crop rectangle when asked. A *preview* run works on ≤ 1024 px frames and returns a
//! rendered image (with the deghost overlay when asked) instead of a file.

use std::sync::Arc;

use lcc_catalog::{PhotoId, Source};
use lcc_develop::DevelopSettings;
use lcc_merge::pano::MAX_MEGAPIXELS;
use lcc_merge::{Deghost, FocusOptions, Frame, HdrOptions, PanoOptions, Projection};
use lcc_pipeline::{Quality, RenderRequest, SourceInfo};
use lcc_raster::{Rgb32f, Rgba8};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{EngineError, Result, Session};

/// Why `merge.hdrBatch` found nothing to merge.
pub const NO_BRACKET_SETS: &str =
    "no bracket sets among these photos: select stacks, or photos taken less than a second apart at different exposures";

/// Long edge of the frames of a preview merge.
pub const PREVIEW_EDGE: usize = 1024;
/// Photos a preview decodes at once.
const PREVIEW_LOADS: usize = 4;

/// HDR saturation maps, validity masks and alignment working copies above the retained RGB frames.
const HDR_WORKING_BYTES_PER_PIXEL: usize = 12;
/// The largest one-off decoder working set, per source pixel.
const DECODE_WORKING_BYTES_PER_PIXEL: usize = 16;
/// Preview frames retained between option changes (PR #2010).
const PREVIEW_CACHE_RESERVE_BYTES: usize = 512 << 20;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum MergeKind {
    Hdr {
        hdr: HdrOptions,
    },
    Panorama {
        pano: PanoOptions,
    },
    /// Merge each bracket (`bracket` consecutive photos; 0 = detect from EXIF), then stitch.
    HdrPanorama {
        hdr: HdrOptions,
        pano: PanoOptions,
        bracket: usize,
    },
    /// Photos focused at different distances → one sharp throughout.
    FocusStack {
        focus: FocusOptions,
    },
    /// The frames of a Nikon pixel-shift series (4, 8, 16 or 32 shots) → one image with full colour at every pixel,
    /// merged on the raw mosaics ([`lcc_merge::pixelshift`]).
    PixelShift,
}

impl MergeKind {
    fn suffix(&self) -> &'static str {
        match self {
            MergeKind::Hdr { .. } => "HDR",
            MergeKind::Panorama { .. } => "Pano",
            MergeKind::HdrPanorama { .. } => "HDR-Pano",
            MergeKind::FocusStack { .. } => "Focus",
            MergeKind::PixelShift => "PixelShift",
        }
    }
    fn min_photos(&self) -> usize {
        match self {
            MergeKind::HdrPanorama { .. } | MergeKind::PixelShift => 4,
            _ => 2,
        }
    }
}

/// Why a selection is not merged as a pixel-shift series. [`std::fmt::Display`] words it in English for the CLI and the
/// command layer; a front end matches on it to word it in the person's language.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum PixelShiftRefusal {
    /// A selected photo has no file the merge can read.
    #[error("{name}: Pixel Shift needs photo files")]
    NotFiles { name: String },
    /// A file could not be read.
    #[error("{name}: {reason}")]
    Unreadable { name: String, reason: String },
    /// A file is not a pixel-shift frame.
    #[error("{name}: {why}")]
    NotFrame { name: String, why: lcc_raw::pixel_shift::HeaderError },
    /// The files are not one complete series.
    #[error("Pixel Shift: {0}")]
    Series(lcc_raw::pixel_shift::SeriesError),
    /// A series with a shot count that has not been measured.
    #[error("Pixel Shift: {0}-shot series are not supported yet")]
    Unsupported(u16),
    /// The result file of this series is already there.
    #[error("A pixel-shift merge of this series already exists ({name})")]
    Exists { name: String },
}

/// Where a merge writes its result before a free name is looked for: next to the first source, or in `output_dir`.
fn merge_target(first: &str, output_dir: Option<&str>) -> String {
    match (output_dir, std::path::Path::new(first).file_name()) {
        (Some(dir), Some(name)) => std::path::Path::new(dir).join(name).to_string_lossy().to_string(),
        _ => first.to_string(),
    }
}

/// The merge sources a merged pixel-shift file's own provenance names, with each frame's photo looked up in the catalog by
/// file name and capture time (the exposure counter is the file's own record of the frame). A frame that is not in the
/// library (or is in Recently Deleted) has no photo: it stays in the list as not found.
pub(crate) fn restored_merge_sources(catalog: &lcc_catalog::Catalog, prov: &lcc_raw::merge_provenance::Provenance) -> lcc_catalog::MergeSources {
    let frames = prov
        .frames
        .iter()
        .map(|f| lcc_catalog::MergeSource {
            photo: catalog
                .photos()
                .find(|p| !p.deleted && p.file_name == f.file_name && p.captured.as_deref() == Some(f.captured.as_str()))
                .map(|p| p.id),
            index: f.index,
            file_name: f.file_name.clone(),
            captured: f.captured.clone(),
            counter: f.counter,
        })
        .collect();
    lcc_catalog::MergeSources { engine: prov.engine, shot_count: prov.shot_count, reference: prov.reference, scale: prov.scale, frames }
}

/// The name of the pixel-shift result of the series whose frame 01 is `first`, when that file is already there.
fn existing_pixel_shift_result(first: &str, output_dir: Option<&str>) -> Option<String> {
    let path = output_names(&merge_target(first, output_dir), MergeKind::PixelShift.suffix()).next()?;
    path.exists().then(|| path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default())
}

/// What planning a pixel-shift merge found out about its series: the frames in series order (frame 01 first).
#[derive(Clone, Debug, PartialEq)]
pub struct PixelShiftPlan {
    /// Shots in the series.
    pub count: u16,
    pub frames: Vec<PixelShiftFrame>,
    /// What the catalog adds to frame 01's file metadata ([`pixel_shift_metadata`]): the union of the frames' keywords,
    /// and frame 01's descriptive fields as they stand in the catalog.
    pub metadata: lcc_meta::Metadata,
}

/// The descriptive metadata a pixel-shift merge takes from the catalog, for `ids` in series order (frame 01 first).
///
/// - Keywords: those of every frame (the catalog's, hierarchy kept, each once whatever its case), written as a sidecar
///   writes them ([`lcc_catalog::keywords::to_file`]).
/// - From frame 01 as the catalog has it: creator, copyright and rights, title, headline, caption, IPTC descriptive
///   fields, location and GPS.
/// - Not taken: rating, colour label, flag, edit settings. Camera, lens, exposure and capture time come from frame 01's
///   file, not from here.
pub(crate) fn pixel_shift_metadata(catalog: &lcc_catalog::Catalog, ids: &[PhotoId]) -> lcc_meta::Metadata {
    let text = |s: &str| (!s.trim().is_empty()).then(|| s.to_string());
    let mut keywords: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for photo in ids.iter().filter_map(|id| catalog.photo(*id)) {
        for k in &photo.meta.keywords {
            if seen.insert(k.trim().to_lowercase()) {
                keywords.push(k.clone());
            }
        }
    }
    let files = lcc_catalog::keywords::to_file(&keywords);
    let mut out = lcc_meta::Metadata { keywords: files.flat, hierarchical_keywords: files.hierarchical, ..Default::default() };
    if let Some(first) = ids.first().and_then(|id| catalog.photo(*id)) {
        let m = &first.meta;
        out.copyright = text(&m.copyright);
        out.copyright_marked = m.copyright_status.marked();
        out.usage_terms = text(&m.usage_terms);
        out.copyright_url = text(&m.copyright_url);
        out.artist = text(&m.creator);
        out.contact = m.contact.clone();
        out.title = text(&m.title);
        out.headline = text(&m.headline);
        out.caption = text(&m.caption);
        out.description_writer = text(&m.description_writer);
        out.category = text(&m.category);
        out.supplemental_categories = m.supplemental_categories.clone();
        out.subject_code = m.subject_code.clone();
        out.alt_text = text(&m.alt_text);
        out.extended_description = text(&m.extended_description);
        out.sublocation = text(&m.location);
        out.city = text(&m.city);
        out.state = text(&m.state);
        out.country = text(&m.country);
        out.country_code = text(&m.country_code);
        out.gps = m.gps.map(|(latitude, longitude)| lcc_meta::Gps { latitude, longitude, altitude: None });
    }
    out
}

/// One frame of a planned pixel-shift merge.
#[derive(Clone, Debug, PartialEq)]
pub struct PixelShiftFrame {
    /// Number in the series, 1-based.
    pub index: u16,
    pub file_name: String,
    /// Capture time as the catalog has it (ISO 8601, local).
    pub captured: String,
    pub counter: u32,
}

/// Compression of the merged DNG (measured on a 16-shot series of 16512 × 11008: see `docs/pixel-shift.md`).
const PIXEL_SHIFT_PACKING: lcc_merge::pixelshift::DngPacking = lcc_merge::pixelshift::DngPacking::LosslessJpeg;

/// Options that apply after the merge.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MergeFinish {
    /// Run Auto Settings on the result.
    pub auto_settings: bool,
    /// Stack the result on top of its sources (collapsed); the result's caption also lists them.
    pub stack: bool,
    /// Preview only: tint deghosted areas red.
    pub show_overlay: bool,
    /// HDR only: also write the aligned frames as a layered PSD/PSB ([`crate::merge_layers`]).
    pub layers: Option<crate::merge_layers::LayerOutput>,
    /// Write the result into this folder instead of next to the first source (the file name is the same).
    pub output_dir: Option<String>,
}

/// The decoded photos of recent merge previews, so a preview whose options change (another projection, a deghost
/// amount) reads and decodes nothing again. Keyed by file (path, size, modification time), preview size and
/// orientation; at most [`FRAME_CACHE_BYTES`], the least recently used out first. Full-size merges don't use it.
#[derive(Clone, Default)]
pub struct FrameCache(Arc<std::sync::Mutex<Vec<(FrameKey, Frame)>>>);

#[derive(Clone, Debug, PartialEq)]
struct FrameKey {
    path: String,
    len: u64,
    modified: Option<std::time::SystemTime>,
    edge: usize,
    orient: bool,
}

/// Memory the decoded preview photos may take (a 1024 px frame is about 9 MB).
const FRAME_CACHE_BYTES: usize = 512 << 20;

impl FrameCache {
    /// The key of `path` as it is on disk now; `None` when the file can't be looked at (no caching then).
    fn key(path: &str, edge: usize, orient: bool) -> Option<FrameKey> {
        let m = std::fs::metadata(path).ok()?;
        Some(FrameKey { path: path.to_string(), len: m.len(), modified: m.modified().ok(), edge, orient })
    }

    fn get(&self, key: &FrameKey) -> Option<Frame> {
        let mut v = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let i = v.iter().position(|(k, _)| k == key)?;
        let e = v.remove(i);
        let f = e.1.clone();
        v.push(e);
        Some(f)
    }

    fn put(&self, key: FrameKey, frame: &Frame) {
        let bytes = |f: &Frame| f.image.data.len() * std::mem::size_of::<[f32; 3]>();
        let mut v = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        v.retain(|(k, _)| *k != key);
        v.push((key, frame.clone()));
        while v.len() > 1 && v.iter().map(|(_, f)| bytes(f)).sum::<usize>() > FRAME_CACHE_BYTES {
            v.remove(0);
        }
    }

    /// Photos held.
    pub fn len(&self) -> usize {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Reads a source photo's bytes (by catalog path).
pub type ByteReader = Arc<dyn Fn(&str) -> std::result::Result<Vec<u8>, String> + Send + Sync>;

/// A planned merge, detached from the session.
#[derive(Clone)]
pub struct MergeJob {
    pub kind: MergeKind,
    pub finish: MergeFinish,
    pub sources: Vec<(PhotoId, String)>,
    /// A pixel-shift merge: the series the sources were found to be (`sources` are in series order).
    pub pixel_shift: Option<PixelShiftPlan>,
    /// `Some(edge)`: preview at that frame size; `None`: the full-resolution merge.
    pub preview: Option<usize>,
    pub read: ByteReader,
    /// Previews: the session's decoded photos ([`FrameCache`]).
    pub frames: Option<FrameCache>,
}

/// What a merge produced.
pub struct MergeOutput {
    /// The DNG (also for previews, at preview size).
    pub dng: Vec<u8>,
    /// Rendered preview (previews only).
    pub preview: Option<Rgba8>,
    /// Auto-crop rectangle (normalised x, y, w, h) for panoramas.
    pub crop: Option<[f64; 4]>,
    /// Statistics for the caller (exposures, alignment, projection, …).
    pub info: Value,
    /// The layered PSD/PSB when [`MergeFinish::layers`] asked for one (full-size runs only).
    pub layers: Option<Vec<u8>>,
}

pub type ProgressFn<'a> = dyn Fn(f32, &str) -> bool + Sync + 'a;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

impl MergeJob {
    fn load(&self, orient: bool, progress: &ProgressFn, span: (f32, f32)) -> std::result::Result<Vec<Frame>, String> {
        self.load_sources(&self.sources, orient, progress, span)
    }

    /// One photo as a merge frame, from the preview cache when it holds it.
    fn load_one(&self, path: &str, orient: bool) -> std::result::Result<Frame, String> {
        let key = self.frames.as_ref().and(self.preview).and_then(|edge| FrameCache::key(path, edge, orient));
        if let (Some(cache), Some(k)) = (&self.frames, &key)
            && let Some(f) = cache.get(k)
        {
            return Ok(f);
        }
        let bytes = (self.read)(path)?;
        // a raw without colour matrices of its own starts from the colour it has when opened alone (#965)
        let f = lcc_merge::load_frame_with(&bytes, self.preview, orient, &crate::camera_preview::starting_colour_tags)
            .map_err(|e| format!("{path}: {e}"))?;
        if let (Some(cache), Some(k)) = (&self.frames, key) {
            cache.put(k, &f);
        }
        Ok(f)
    }

    fn load_sources(
        &self,
        sources: &[(PhotoId, String)],
        orient: bool,
        progress: &ProgressFn,
        span: (f32, f32),
    ) -> std::result::Result<Vec<Frame>, String> {
        self.load_sources_as(sources, orient, progress, span, |f| f)
    }

    /// [`Self::load_sources`], each frame passed through `hold` as soon as it is decoded (an HDR bracket keeps its
    /// frames as half floats, so only one is ever at full precision).
    fn load_sources_as<T: Send>(
        &self,
        sources: &[(PhotoId, String)],
        orient: bool,
        progress: &ProgressFn,
        span: (f32, f32),
        hold: impl Fn(Frame) -> T + Sync,
    ) -> std::result::Result<Vec<T>, String> {
        use rayon::prelude::*;
        let done = std::sync::atomic::AtomicUsize::new(0);
        let n = sources.len();
        let one = |(_, path): &(PhotoId, String)| {
            let f = self.load_one(path, orient).map(&hold);
            let k = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            progress(span.0 + (span.1 - span.0) * k as f32 / n.max(1) as f32, "Reading photos");
            f
        };
        // Full-size photos are decoded one at a time (the demosaic is parallel inside): decoding several 60 MP raws at
        // once multiplied the decoders' scratch memory, and a 7-frame bracket ran out of memory. Previews a few at a
        // time: a raw that can't be binned (JPEG XL, linear DNG) still decodes at full size, and a panorama preview of
        // a dozen 48 MP photos all at once reached 40 GB.
        if self.preview.is_none() {
            return sources.iter().map(one).collect();
        }
        let mut frames = Vec::with_capacity(n);
        for chunk in sources.chunks(PREVIEW_LOADS) {
            frames.extend(chunk.par_iter().map(one).collect::<std::result::Result<Vec<_>, _>>()?);
        }
        Ok(frames)
    }

    /// A pixel-shift merge: the frames are read, decoded and placed one at a time (the merged image is the only large
    /// buffer), then written as a LinearRaw DNG carrying frame 01's colour, lens data and metadata.
    fn run_pixel_shift(&self, progress: &ProgressFn) -> std::result::Result<MergeOutput, String> {
        use lcc_merge::pixelshift;
        use lcc_raw::merge_provenance::{ENGINE, Provenance, SourceFrame, Window};
        let plan = self.pixel_shift.as_ref().ok_or("the photos were not checked as a pixel-shift series")?;
        let pattern = pixelshift::pattern_for(plan.count).ok_or_else(|| format!("{}-shot pixel-shift series are not supported yet", plan.count))?;
        if self.sources.len() != pattern.shifts.len() {
            return Err(format!("the series has {} shots but {} photos were given", plan.count, self.sources.len()));
        }
        if !progress(0.0, "Reading frame 1") {
            return Err("cancelled".into());
        }
        // frame 01 gives the result its colour, lens data, metadata and place on the sensor
        let bytes = (self.read)(&self.sources[0].1)?;
        let mut first = lcc_raw::decode(&bytes).map_err(|e| format!("{}: {e}", self.sources[0].1))?;
        let lens = first.opcodes.list3.clone();
        // the colour fit sees the raw without its lens warp, as when the photo opens
        first.opcodes.list3.retain(|op| !op.is_lens_correction());
        let tags = crate::camera_preview::starting_colour_tags(&mut first, &bytes);
        first.opcodes.list3.clone_from(&lens);
        let mut color = tags.unwrap_or_else(|| first.color.clone());
        if color.as_shot_neutral.is_none()
            && color.as_shot_white_xy.is_none()
            && let Some(m) = first.wb_multipliers.filter(|m| m.iter().all(|v| *v > 0.0 && v.is_finite()))
        {
            color.as_shot_neutral = Some(m.map(|v| 1.0 / f64::from(v)));
        }
        let window = lcc_raw::sensor_window(&bytes, [first.active_area.width, first.active_area.height]).map(|w| Window {
            sensor: w.sensor,
            origin: w.origin,
            size: w.size,
        });
        // frame 01's own JPEG goes into the file as its preview: the colour look is fitted to it when the file opens, as it
        // is for the camera's raw
        let preview = lcc_raw::embedded_preview(&bytes)
            .filter(|jpeg| jpeg.starts_with(&[0xff, 0xd8]))
            .map(|jpeg| lcc_raw::DngPreview { jpeg, color_space: lcc_raw::embedded_preview_color_space(&bytes) });
        drop(bytes);
        // the catalog's descriptive fields and keywords win; frame 01's file fills in the rest (camera, lens, exposure,
        // capture time with its sub-seconds and offset). Rating and label are not carried.
        let mut metadata = plan.metadata.clone();
        metadata.fill_missing(&first.metadata);
        metadata.rating = None;
        metadata.label = None;
        if metadata.capture_time.is_none() {
            metadata.capture_time = plan.frames.first().and_then(|f| lcc_meta::DateTime::parse_iso(&f.captured));
        }
        let orientation = first.orientation;
        let mut first = Some(first);
        let mut load = |i: usize| -> std::result::Result<lcc_raw::RawImage, String> {
            if let Some(f) = first.take().filter(|_| i == 0) {
                return Ok(f);
            }
            let path = &self.sources[i].1;
            let bytes = (self.read)(path)?;
            lcc_raw::decode(&bytes).map_err(|e| format!("{path}: {e}"))
        };
        let step = |f: f32| progress(0.02 + 0.78 * f, &format!("Merging frame {} of {}", (f * plan.count as f32) as usize + 1, plan.count));
        let merged = pixelshift::merge_series(pattern, 0..self.sources.len(), &mut load, &step).map_err(err)?;
        if !progress(0.82, "Writing DNG") {
            return Err("cancelled".into());
        }
        let (width, height, filled) = (merged.width, merged.height, merged.filled);
        let provenance = Provenance {
            engine: ENGINE,
            shot_count: plan.count,
            reference: 1,
            scale: pattern.scale as u16,
            window,
            frames: plan
                .frames
                .iter()
                .map(|f| SourceFrame { index: f.index, file_name: f.file_name.clone(), captured: f.captured.clone(), counter: f.counter })
                .collect(),
        };
        let source = pixelshift::DngSource { metadata, color, orientation, lens, provenance, preview };
        let dng = pixelshift::write_dng(merged, source, PIXEL_SHIFT_PACKING).map_err(err)?;
        progress(1.0, "Done");
        let info = json!({"kind": "pixelShift", "shots": plan.count, "width": width, "height": height, "filledNodes": filled});
        Ok(MergeOutput { dng, preview: None, crop: None, info, layers: None })
    }

    /// Run the merge. `progress(fraction, stage)` returns false to cancel.
    pub fn run(&self, progress: &ProgressFn) -> std::result::Result<MergeOutput, String> {
        if self.sources.len() < self.kind.min_photos() {
            return Err(format!("select at least {} photos", self.kind.min_photos()));
        }
        if !progress(0.0, "Reading photos") {
            return Err("cancelled".into());
        }
        let sub = |a: f32, b: f32| move |f: f32, s: &str| progress(a + (b - a) * f, s);
        // the reference frame's lens corrections (HDR); panoramas carry none
        let mut lens = Vec::new();
        let mut layers = None;
        let (image, color, orientation, metadata, baseline, samples, crop, mut info, ghost) = match &self.kind {
            MergeKind::Hdr { hdr } => {
                let frames = self.load_sources_as(&self.sources, false, progress, (0.0, 0.3), lcc_merge::HdrFrame::new)?;
                let want_layers = self.finish.layers.as_ref().filter(|_| self.preview.is_none());
                let mut r = if let Some(lo) = want_layers {
                    let p = sub(0.3, 0.6);
                    let (r, aligned) = lcc_merge::merge_hdr_frames_aligned(frames, hdr, &p).map_err(err)?;
                    let names: Vec<String> = self
                        .sources
                        .iter()
                        .map(|(_, path)| {
                            std::path::Path::new(path).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| path.clone())
                        })
                        .collect();
                    let lp = |f: f32| progress(0.6 + 0.3 * f, "Writing layers");
                    layers = Some(crate::merge_layers::write(&r, aligned, &names, lo, &lp)?);
                    r
                } else {
                    let p = sub(0.3, 0.9);
                    lcc_merge::merge_hdr_frames(frames, hdr, &p).map_err(err)?
                };
                lens = std::mem::take(&mut r.lens);
                let info = json!({
                    "reference": self.sources[r.reference].0.0,
                    "ev": r.ev, "evExif": r.ev_exif,
                    "alignment": r.alignments.iter().zip(&r.local_shift).map(|(a, l)| json!({"model": a.model, "inliers": a.inliers, "rmsPx": a.rms, "localPx": l})).collect::<Vec<_>>(),
                });
                let ghost = (!r.ghost.data.is_empty()).then(|| r.ghost.oriented(r.orientation));
                (r.radiance, r.color, r.orientation, r.metadata, r.baseline_exposure, lcc_merge::DngSamples::Half, None, info, ghost)
            }
            MergeKind::Panorama { pano } => {
                let frames = self.load(true, progress, (0.0, 0.25))?;
                let p = sub(0.25, 0.9);
                let input_pixels = frame_pixel_count(&frames)?;
                let available = merge_memory_bytes(crate::memory::total_ram(), crate::memory::budget());
                let fit = fit_in_memory(pano, input_pixels as f64, crate::memory::total_ram());
                let output_limit = panorama_output_pixel_limit(available, input_pixels)?;
                let r = lcc_merge::pano::stitch_with_output_pixel_limit(frames, &fit, output_limit, &p).map_err(err)?;
                let info = pano_info(&r, &self.sources, pano, &fit);
                (r.image, r.color, lcc_geom::Orientation::Normal, r.metadata, r.baseline_exposure, lcc_merge::DngSamples::U16, r.crop, info, None)
            }
            MergeKind::HdrPanorama { hdr, pano, bracket } => {
                let n = self.sources.len();
                let b = if *bracket >= 2 {
                    *bracket
                } else {
                    // the bracket size from the photos' EXIF exposures (read without decoding them)
                    let ev: Option<Vec<f64>> = self
                        .sources
                        .iter()
                        .map(|(_, path)| (self.read)(path).ok().and_then(|bytes| lcc_merge::frame::exif_exposure(&lcc_meta::extract(&bytes))))
                        .collect();
                    ev.as_deref().and_then(detect_bracket).ok_or("could not detect the bracket size from EXIF; set it")?
                };
                if !n.is_multiple_of(b) || n / b < 2 {
                    return Err(format!("{n} photos don't form brackets of {b} for at least two positions"));
                }
                let groups = n / b;
                let mut merged = Vec::new();
                // one bracket at a time: only its photos are held at full size, never all of them
                for (g, set) in self.sources.chunks(b).enumerate() {
                    let at = |t: f32| 0.6 * (g as f32 + t) / groups as f32;
                    let set = self.load_sources_as(set, false, progress, (at(0.0), at(0.5)), lcc_merge::HdrFrame::new)?;
                    let p = sub(at(0.5), at(1.0));
                    let r = lcc_merge::merge_hdr_frames(set, hdr, &p).map_err(err)?;
                    let mut f = Frame {
                        image: r.radiance,
                        clip: f32::INFINITY,
                        exposure: None,
                        raw: r.raw,
                        color: r.color,
                        orientation: r.orientation,
                        metadata: r.metadata,
                        baseline_exposure: r.baseline_exposure,
                        lens: Vec::new(),
                        wb_multipliers: r.wb_multipliers,
                        display_referred: false,
                    };
                    f.orient();
                    merged.push(f);
                }
                let p = sub(0.6, 0.9);
                let input_pixels = frame_pixel_count(&merged)?;
                let available = merge_memory_bytes(crate::memory::total_ram(), crate::memory::budget());
                let fit = fit_in_memory(pano, input_pixels as f64, crate::memory::total_ram());
                let output_limit = panorama_output_pixel_limit(available, input_pixels)?;
                let r = lcc_merge::pano::stitch_with_output_pixel_limit(merged, &fit, output_limit, &p).map_err(err)?;
                let mut info = pano_info(&r, &self.sources, pano, &fit);
                info["bracket"] = json!(b);
                (r.image, r.color, lcc_geom::Orientation::Normal, r.metadata, r.baseline_exposure, lcc_merge::DngSamples::Half, r.crop, info, None)
            }
            MergeKind::FocusStack { focus } => {
                use lcc_merge::MergeError;
                use lcc_merge::focus::Load;
                // frames are read twice (small to align, then full size) and never all held at once,
                // except a preview's: its small versions are what it stacks
                let small = Some(self.preview.map_or(lcc_merge::align::WORK_EDGE, |e| e.min(lcc_merge::align::WORK_EDGE)));
                let kept: std::sync::Mutex<std::collections::HashMap<usize, Frame>> = Default::default();
                let reuse = small == self.preview;
                let no_colour = |_: &mut lcc_raw::RawImage, _: &[u8]| None;
                let load = |i: usize, what: Load| {
                    if what == Load::Stack
                        && let Some(f) = kept.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&i)
                    {
                        return Ok(f);
                    }
                    let path = self.sources.get(i).map(|s| s.1.as_str()).unwrap_or_default();
                    let bytes = (self.read)(path).map_err(MergeError::Decode)?;
                    // only the reference's colour is the result's
                    let colour: &lcc_merge::RawColour =
                        if what == Load::Reference { &crate::camera_preview::starting_colour_tags } else { &no_colour };
                    let f = lcc_merge::load_frame_with(&bytes, if what == Load::Align { small } else { self.preview }, false, colour).map_err(
                        |e| match e {
                            MergeError::Decode(m) => MergeError::Decode(format!("{path}: {m}")),
                            e => e,
                        },
                    )?;
                    if reuse && what == Load::Align {
                        kept.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(i, f.clone());
                    }
                    Ok(f)
                };
                let p = sub(0.0, 0.9);
                let mut r = lcc_merge::stack_focus(self.sources.len(), &load, focus, &p).map_err(err)?;
                lens = std::mem::take(&mut r.lens);
                let id = |i: usize| self.sources.get(i).map(|s| s.0.0);
                let info = json!({
                    "reference": id(r.reference),
                    "alignment": r.alignments.iter().map(|a| json!({"model": a.model, "inliers": a.inliers, "rmsPx": a.rms})).collect::<Vec<_>>(),
                    "skipped": r.skipped.iter().filter_map(|&i| id(i)).collect::<Vec<_>>(),
                    "ev": r.ev,
                });
                (r.image, r.color, r.orientation, r.metadata, r.baseline_exposure, lcc_merge::DngSamples::U16, None, info, None)
            }
            MergeKind::PixelShift => return self.run_pixel_shift(progress),
        };
        if !progress(0.9, "Writing DNG") {
            return Err("cancelled".into());
        }
        let mut metadata = metadata;
        if self.finish.stack {
            let names: Vec<&str> =
                self.sources.iter().map(|(_, p)| std::path::Path::new(p).file_name().and_then(|n| n.to_str()).unwrap_or(p)).collect();
            metadata.caption = Some(format!("Merged from {}", names.join(", ")));
        }
        metadata.software = Some(format!("{} Photo Merge", lcc_brand::DISPLAY_NAME));
        let dng = lcc_merge::write_linear_dng(&image, &color, orientation, &metadata, baseline, samples, &lens).map_err(err)?;
        info["width"] = json!(image.width);
        info["height"] = json!(image.height);
        let preview = match self.preview {
            Some(edge) => Some(render_preview(&dng, edge, self.finish.auto_settings, crop, ghost.as_ref().filter(|_| self.finish.show_overlay))?),
            None => None,
        };
        progress(1.0, "Done");
        Ok(MergeOutput { dng, preview, crop, info, layers })
    }
}

/// `asked` is what the command asked for, `fit` what ran (held to memory): when the panorama came
/// out smaller than the photos' own resolution, `limit` says why (`memory`, `size`: Lightroom's
/// 512 MP / 65,000 px, or `asked`).
fn pano_info(r: &lcc_merge::PanoResult, sources: &[(PhotoId, String)], asked: &PanoOptions, fit: &PanoOptions) -> Value {
    let megapixels = (r.image.width * r.image.height) as f64 / 1e6;
    let native = r.native_megapixels;
    let why = if fit.max_megapixels < asked.max_megapixels && fit.max_megapixels < native {
        "memory"
    } else if asked.max_megapixels < MAX_MEGAPIXELS && asked.max_megapixels < native {
        "asked"
    } else {
        "size"
    };
    let limit = (megapixels < native * 0.98).then_some(why);
    json!({
        "projection": r.projection,
        "used": r.used.iter().filter_map(|&i| sources.get(i).map(|s| s.0.0)).collect::<Vec<_>>(),
        "fov": [r.fov.0, r.fov.1],
        "rmsPx": r.rms,
        "gains": r.gains,
        "offsets": r.offsets,
        "vignette": r.vignette,
        "crop": r.crop,
        "megapixels": megapixels,
        "nativeMegapixels": native,
        "limit": limit,
    })
}

/// Peak memory of a panorama per pixel of its output (bytes), measured as the peak resident size
/// of synthetic stitches of 20–81 MP from 38–154 MP of photos, rounded up from the smaller ones
/// (the larger need less): while the photos are projected they are still held, beside the tiles
/// ([`PANO_TILE_BYTES`] on top of the photos' own 12 bytes a pixel); the blend that follows, the
/// photos freed, peaks at [`PANO_BLEND_BYTES`]. Writing the DNG takes less than either.
const PANO_TILE_BYTES: usize = 36;
const PANO_BLEND_BYTES: usize = 68;

/// What a merge may use of the computer's memory ([`crate::memory::total_ram`]: a cgroup's memory
/// limit when smaller): three quarters of it (the rest is the system's and the other apps'), less
/// the app's own memory budget (its caches).
pub(crate) fn merge_memory_bytes(ram: Option<usize>, cache_budget: usize) -> usize {
    // a computer whose memory we can't read (Windows) is taken to have 8 GB
    let ram = ram.unwrap_or_else(|| unknown_ram_fallback(usize::MAX));
    // 3/4 RAM without overflowing `usize`, including on 32-bit targets.
    let three_quarters = (ram / 4) * 3 + (ram % 4) * 3 / 4;
    three_quarters.saturating_sub(cache_budget)
}

pub(crate) fn unknown_ram_fallback(target_max: usize) -> usize {
    usize::try_from(8_u64 << 30).unwrap_or(target_max).min(target_max)
}

fn merge_memory(ram: Option<usize>, budget: usize) -> f64 {
    merge_memory_bytes(ram, budget) as f64
}

/// `pano` with its size held to what fits in memory beside its photos (`frame_px` pixels, 12
/// bytes each, loaded already): a panorama of five 51 MP photos may have up to 166 MP on a 16 GB
/// computer, 355 MP on 32 GB. Never below 40 megapixels, the size every panorama had before it
/// followed the photos' resolution, and never above what was asked.
pub(crate) fn fit_in_memory(pano: &PanoOptions, frame_px: f64, ram: Option<usize>) -> PanoOptions {
    fit_in_memory_beside(pano, frame_px, ram, crate::memory::budget())
}

/// [`fit_in_memory`] beside an app memory budget of `budget` bytes (tests give their own: the
/// process's follows the computer and `{ENV_PREFIX}_MEMORY_MB`).
pub(crate) fn fit_in_memory_beside(pano: &PanoOptions, frame_px: f64, ram: Option<usize>, budget: usize) -> PanoOptions {
    let room = merge_memory(ram, budget);
    let fits = ((room - frame_px * 12.0) / PANO_TILE_BYTES as f64).min(room / PANO_BLEND_BYTES as f64) / 1e6;
    PanoOptions { max_megapixels: pano.max_megapixels.min(fits.max(MIN_FITTED_MEGAPIXELS)), ..pano.clone() }
}

#[derive(Default)]
struct MergeDimensions {
    count: usize,
    largest_source: usize,
    frame_pixels: usize,
    largest_frame: usize,
}

impl MergeDimensions {
    fn add(
        &mut self,
        width: u32,
        height: u32,
        decoder_width: u32,
        decoder_height: u32,
        preview_edge: Option<usize>,
    ) -> std::result::Result<(), String> {
        if width == 0 || height == 0 {
            return Err("Photo Merge cannot estimate memory for a photo with zero width or height".into());
        }
        if decoder_width == 0 || decoder_height == 0 {
            return Err("Photo Merge cannot estimate memory for a photo with zero source width or height".into());
        }
        let source_pixels = (decoder_width as usize)
            .checked_mul(decoder_height as usize)
            .ok_or_else(|| "Photo Merge cannot safely estimate memory: decoder dimensions overflow".to_string())?;
        let (frame_width, frame_height) = match preview_edge {
            Some(edge) => preview_dimensions(width, height, edge)?,
            None => (width, height),
        };
        let frame_pixels = (frame_width as usize)
            .checked_mul(frame_height as usize)
            .ok_or_else(|| "Photo Merge cannot safely estimate memory: photo dimensions overflow".to_string())?;
        self.count = self.count.checked_add(1).ok_or_else(|| "too many Photo Merge inputs to estimate safely".to_string())?;
        self.largest_source = self.largest_source.max(source_pixels);
        self.frame_pixels = self
            .frame_pixels
            .checked_add(frame_pixels)
            .ok_or_else(|| "Photo Merge cannot safely estimate memory: input dimensions overflow".to_string())?;
        self.largest_frame = self.largest_frame.max(frame_pixels);
        Ok(())
    }

    fn working_bytes(&self, kind: &MergeKind, preview: bool, available: usize) -> std::result::Result<usize, String> {
        let input_bytes = self
            .frame_pixels
            .checked_mul(std::mem::size_of::<[f32; 3]>())
            .ok_or_else(|| "Photo Merge cannot safely estimate memory: input dimensions overflow".to_string())?;
        // Current previews use Rayon workers; this also conservatively covers PR #2010's four-photo chunks.
        let decoder_slots = if preview { self.count.min(rayon::current_num_threads()) } else { 1 };
        let decoder_pixels = self
            .largest_source
            .checked_mul(decoder_slots)
            .ok_or_else(|| "Photo Merge cannot safely estimate memory: decoder dimensions overflow".to_string())?;
        let decoder_bytes = decoder_pixels
            .checked_mul(DECODE_WORKING_BYTES_PER_PIXEL)
            .ok_or_else(|| "Photo Merge cannot safely estimate memory: decoder working set overflow".to_string())?;
        let cache_bytes = if preview { PREVIEW_CACHE_RESERVE_BYTES } else { 0 };
        let hdr_peak = || -> std::result::Result<usize, String> {
            let hdr_work = self
                .frame_pixels
                .checked_mul(HDR_WORKING_BYTES_PER_PIXEL)
                .ok_or_else(|| "Photo Merge cannot safely estimate memory: HDR working set overflow".to_string())?;
            let hdr_output_bytes = self
                .largest_frame
                .checked_mul(std::mem::size_of::<[f32; 3]>() + 4)
                .ok_or_else(|| "Photo Merge cannot safely estimate memory: HDR output dimensions overflow".to_string())?;
            input_bytes
                .checked_add(hdr_work)
                .and_then(|bytes| bytes.checked_add(hdr_output_bytes))
                .and_then(|bytes| bytes.checked_add(decoder_bytes))
                .ok_or_else(|| "Photo Merge cannot safely estimate memory: HDR working set overflow".to_string())
        };

        let panorama_peak = |pano: &PanoOptions, input_pixels_for_fit: usize| -> std::result::Result<usize, String> {
            let output_mp = if pano.max_megapixels.is_finite() && pano.max_megapixels > 0.0 {
                pano.max_megapixels.min(MAX_MEGAPIXELS)
            } else {
                return Err("Photo Merge cannot safely estimate memory for an invalid panorama size".into());
            };
            // Match fit_in_memory's cap using the same memory budget. Do not cap output at total input
            // area: perspective stitching can produce a larger canvas than the sum of its sources.
            let room = available as f64;
            let input_bytes_for_fit = input_pixels_for_fit as f64 * std::mem::size_of::<[f32; 3]>() as f64;
            let fitting_megapixels = ((room - input_bytes_for_fit) / PANO_TILE_BYTES as f64).min(room / PANO_BLEND_BYTES as f64) / 1_000_000.0;
            let fitted_mp = output_mp.min(fitting_megapixels.max(MIN_FITTED_MEGAPIXELS));
            let output_pixel_count = (fitted_mp * 1_000_000.0).floor();
            if !output_pixel_count.is_finite() || output_pixel_count < 1.0 || output_pixel_count > usize::MAX as f64 {
                return Err("Photo Merge cannot safely estimate memory: panorama output dimensions overflow".into());
            }
            let requested_output_pixels = output_pixel_count as usize;
            // stitch() rejects perspective bounds above twelve times the area of all inputs. Auto
            // can select perspective, so use that explicit geometric limit as its conservative cap;
            // spherical/cylindrical projections use their requested and memory-fitted limit alone.
            let output_pixels = if matches!(pano.projection, Projection::Auto | Projection::Perspective) {
                requested_output_pixels.min(
                    self.frame_pixels
                        .checked_mul(12)
                        .ok_or_else(|| "Photo Merge cannot safely estimate memory: projected panorama dimensions overflow".to_string())?,
                )
            } else {
                requested_output_pixels
            };
            let tile_bytes = output_pixels
                .checked_mul(PANO_TILE_BYTES)
                .and_then(|bytes| bytes.checked_add(input_bytes))
                .ok_or_else(|| "Photo Merge cannot safely estimate memory: panorama working set overflow".to_string())?;
            let blend_bytes = output_pixels
                .checked_mul(PANO_BLEND_BYTES)
                .ok_or_else(|| "Photo Merge cannot safely estimate memory: panorama working set overflow".to_string())?;
            let load_bytes = input_bytes
                .checked_add(decoder_bytes)
                .ok_or_else(|| "Photo Merge cannot safely estimate memory: panorama loading working set overflow".to_string())?;
            Ok(tile_bytes.max(blend_bytes).max(load_bytes))
        };

        let operation_peak = match kind {
            MergeKind::Hdr { .. } => hdr_peak()?,
            // frames stream through the stack one at a time: the running result, its weights and one frame
            MergeKind::FocusStack { .. } => self
                .largest_frame
                .checked_mul(4 * std::mem::size_of::<[f32; 3]>())
                .and_then(|bytes| bytes.checked_add(decoder_bytes))
                .ok_or_else(|| "Photo Merge cannot safely estimate memory: focus stack working set overflow".to_string())?,
            MergeKind::Panorama { pano } => panorama_peak(pano, self.frame_pixels)?,
            // Until the HDR panorama work frees each input bracket, allow for every source to remain resident. This
            // remains safe with the one-bracket-at-a-time loader in PR #2010, though it can be conservative there.
            // The stitched HDR frames are smaller than the source bracket set. Do not subtract
            // source pixels when bounding their fitted canvas; zero input gives an upper bound.
            MergeKind::HdrPanorama { pano, .. } => hdr_peak()?.max(panorama_peak(pano, 0)?),
            // frames stream in one at a time: the merged image (6 bytes a node, 4 nodes per sensor pixel), its
            // bookkeeping byte and, while the DNG is written, about as much again
            MergeKind::PixelShift => self
                .largest_source
                .checked_mul(4 * 12)
                .and_then(|bytes| bytes.checked_add(decoder_bytes))
                .ok_or_else(|| "Photo Merge cannot safely estimate memory: pixel shift working set overflow".to_string())?,
        };
        operation_peak.checked_add(cache_bytes).ok_or_else(|| "Photo Merge cannot safely estimate memory: preview working set overflow".to_string())
    }
}

fn preview_dimensions(width: u32, height: u32, edge: usize) -> std::result::Result<(u32, u32), String> {
    let edge = u32::try_from(edge).map_err(|_| "Photo Merge preview size is invalid".to_string())?;
    if edge == 0 {
        return Err("Photo Merge preview size is invalid".into());
    }
    let largest = width.max(height);
    if largest <= edge {
        return Ok((width, height));
    }
    let scaled = |side: u32| -> std::result::Result<u32, String> {
        let numerator = u64::from(side).checked_mul(u64::from(edge)).ok_or_else(|| "Photo Merge preview dimensions overflow".to_string())?;
        let divisor = u64::from(largest);
        let quotient = numerator / divisor;
        let rounded =
            quotient.checked_add(u64::from(numerator % divisor != 0)).ok_or_else(|| "Photo Merge preview dimensions overflow".to_string())?;
        Ok(u32::try_from(rounded).map_err(|_| "Photo Merge preview dimensions overflow".to_string())?.max(1))
    };
    if width >= height { Ok((edge, scaled(height)?)) } else { Ok((scaled(width)?, edge)) }
}

#[cfg(test)]
pub(crate) fn estimate_merge_working_bytes(
    kind: &MergeKind,
    dimensions: &[(u32, u32)],
    preview_edge: Option<usize>,
) -> std::result::Result<usize, String> {
    estimate_merge_working_bytes_with_budget(kind, dimensions, preview_edge, usize::MAX)
}

#[cfg(test)]
pub(crate) fn estimate_merge_working_bytes_with_budget(
    kind: &MergeKind,
    dimensions: &[(u32, u32)],
    preview_edge: Option<usize>,
    available: usize,
) -> std::result::Result<usize, String> {
    let mut estimate = MergeDimensions::default();
    for &(width, height) in dimensions {
        estimate.add(width, height, width, height, preview_edge)?;
    }
    if estimate.count == 0 {
        return Err("Photo Merge needs at least one photo to estimate memory".into());
    }
    estimate.working_bytes(kind, preview_edge.is_some(), available)
}

pub(crate) fn check_merge_memory_budget(needed: usize, available: usize) -> std::result::Result<(), String> {
    if needed > available {
        let gib = 1_u64 << 30;
        Err(format!(
            "Photo Merge needs an estimated {:.1} GiB of working memory, above the available merge memory budget of {:.1} GiB. Select fewer photos or use lower-resolution originals.",
            needed as f64 / gib as f64,
            available as f64 / gib as f64
        ))
    } else {
        Ok(())
    }
}

/// The pixels of `frames`.
fn frame_pixel_count(frames: &[Frame]) -> std::result::Result<usize, String> {
    frames.iter().try_fold(0usize, |total, frame| {
        let pixels = frame
            .width()
            .checked_mul(frame.height())
            .ok_or_else(|| "Photo Merge cannot safely estimate memory: frame dimensions overflow".to_string())?;
        total.checked_add(pixels).ok_or_else(|| "Photo Merge cannot safely estimate memory: frame dimensions overflow".to_string())
    })
}

fn panorama_output_pixel_limit(available: usize, frame_pixels: usize) -> std::result::Result<usize, String> {
    let input_bytes = frame_pixels
        .checked_mul(std::mem::size_of::<[f32; 3]>())
        .ok_or_else(|| "Photo Merge cannot safely estimate memory: panorama input dimensions overflow".to_string())?;
    let tile_limit = available.saturating_sub(input_bytes) / PANO_TILE_BYTES;
    let blend_limit = available / PANO_BLEND_BYTES;
    Ok(tile_limit.min(blend_limit))
}

/// The smallest size [`fit_in_memory`] holds a panorama to (megapixels).
pub(crate) const MIN_FITTED_MEGAPIXELS: f64 = 40.0;

/// Bracket size from EXIF: the smallest period of the exposure sequence (≥ 2).
pub fn detect_bracket(ev: &[f64]) -> Option<usize> {
    let n = ev.len();
    (2..=n / 2).find(|&b| n.is_multiple_of(b) && (0..n).all(|k| (ev[k] - ev[k % b]).abs() < 0.2) && (1..b).any(|k| (ev[k] - ev[0]).abs() > 0.5))
}

/// Render a merged DNG like the library will show it (default raw settings, optionally Auto
/// Settings and the auto crop), with the deghost overlay tinted red.
fn render_preview(
    dng: &[u8],
    edge: usize,
    auto: bool,
    crop: Option<[f64; 4]>,
    ghost: Option<&lcc_raster::Plane>,
) -> std::result::Result<Rgba8, String> {
    let (src, info) = crate::files::load_bytes(dng, edge)?;
    let mut s = DevelopSettings::for_raw(info.as_shot_temp, info.as_shot_tint);
    if auto {
        apply_auto(&mut s, &src, &info);
    }
    if let Some(c) = crop {
        s.crop.geometry.rect = lcc_geom::Rect::from_xywh(c[0], c[1], c[2], c[3]);
    }
    let req = RenderRequest { quality: Quality::Full, apply_crop: false, ..RenderRequest::fit(edge, edge) };
    let mut img = lcc_pipeline::render(&src, &info, &s, &req).image;
    if let Some(g) = ghost {
        let (w, h) = (img.width, img.height);
        for y in 0..h {
            for x in 0..w {
                let gx = ((x as f32 + 0.5) / w as f32 * g.width as f32) as usize;
                let gy = ((y as f32 + 0.5) / h as f32 * g.height as f32) as usize;
                let a = g.get(gx.min(g.width - 1), gy.min(g.height - 1)).clamp(0.0, 1.0) * 0.6;
                if a > 0.01 {
                    let p = &mut img.data[y * w + x];
                    p[0] = (p[0] as f32 * (1.0 - a) + 255.0 * a) as u8;
                    p[1] = (p[1] as f32 * (1.0 - a)) as u8;
                    p[2] = (p[2] as f32 * (1.0 - a)) as u8;
                }
            }
        }
    }
    if let Some(c) = crop {
        // draw the auto-crop frame
        let (w, h) = (img.width as f64, img.height as f64);
        let (x0, y0, x1, y1) = ((c[0] * w) as usize, (c[1] * h) as usize, ((c[0] + c[2]) * w) as usize, ((c[1] + c[3]) * h) as usize);
        for y in y0..y1.min(img.height) {
            for x in x0..x1.min(img.width) {
                if y == y0 || y + 1 == y1 || x == x0 || x + 1 == x1 {
                    img.data[y * img.width + x] = [255, 255, 255, 255];
                }
            }
        }
    }
    Ok(img)
}

fn apply_auto(s: &mut DevelopSettings, src: &Rgb32f, info: &SourceInfo) {
    let a = lcc_pipeline::auto::auto_tone(src, info, s);
    s.light.exposure = a.exposure;
    s.light.contrast = a.contrast;
    s.light.highlights = a.highlights;
    s.light.shadows = a.shadows;
    s.light.whites = a.whites;
    s.light.blacks = a.blacks;
    s.color.vibrance = a.vibrance;
    s.color.saturation = a.saturation;
}

/// `dir/<stem>-<suffix>.dng`, then `-2`, `-3`… (the candidates for a merge's output).
fn output_names(first: &str, suffix: &str) -> impl Iterator<Item = std::path::PathBuf> {
    output_names_ext(first, suffix, "dng")
}

fn output_names_ext(first: &str, suffix: &str, ext: &'static str) -> impl Iterator<Item = std::path::PathBuf> {
    let p = std::path::Path::new(first);
    let dir = p.parent().map(|d| d.to_path_buf()).unwrap_or_default();
    let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "Merge".into());
    let suffix = suffix.to_string();
    (1u64..).map(move |k| dir.join(if k == 1 { format!("{stem}-{suffix}.{ext}") } else { format!("{stem}-{suffix}-{k}.{ext}") }))
}

/// The options of a merge started from the app (the Photo Merge menu or its dialog). The dialog's Auto Settings and
/// Create Stack belong to HDR, panorama and focus-stack merges: a pixel-shift merge always starts like the camera's own
/// file (default settings, no Auto Settings) and sits on its frames in a stack, whatever the dialog or the options saved
/// from an earlier merge say.
pub fn parse_dialog(id: &str, p: &Value) -> Result<(MergeKind, MergeFinish)> {
    let (kind, mut finish) = parse(id, p)?;
    if id == "merge.pixelShift" {
        finish.auto_settings = false;
        finish.stack = true;
    }
    Ok((kind, finish))
}

/// Parse `merge.*` command parameters into a kind and finishing options.
pub fn parse(id: &str, p: &Value) -> Result<(MergeKind, MergeFinish)> {
    let bad = |m: String| EngineError::BadParams { cmd: id.into(), msg: m };
    let deghost: Deghost = serde_json::from_value(p.get("deghost").cloned().unwrap_or(json!("none"))).map_err(|e| bad(format!("deghost: {e}")))?;
    let hdr = HdrOptions { align: p.get("align").and_then(Value::as_bool).unwrap_or(true), deghost };
    let projection: Projection =
        serde_json::from_value(p.get("projection").cloned().unwrap_or(json!("auto"))).map_err(|e| bad(format!("projection: {e}")))?;
    let pano = PanoOptions {
        projection,
        boundary_warp: p.get("boundaryWarp").and_then(Value::as_f64).unwrap_or(0.0).clamp(0.0, 100.0),
        auto_crop: p.get("autoCrop").and_then(Value::as_bool).unwrap_or(false),
        fill_edges: p.get("fillEdges").and_then(Value::as_bool).unwrap_or(false),
        // the photos' own resolution unless asked (held to what fits in memory when it runs)
        max_megapixels: p.get("maxMegapixels").and_then(Value::as_f64).filter(|v| *v > 0.0).unwrap_or(MAX_MEGAPIXELS).min(MAX_MEGAPIXELS),
    };
    let kind = match id {
        "merge.hdr" => MergeKind::Hdr { hdr },
        "merge.panorama" => MergeKind::Panorama { pano },
        "merge.focusStack" => MergeKind::FocusStack { focus: FocusOptions { align: hdr.align } },
        "merge.pixelShift" => MergeKind::PixelShift,
        _ => MergeKind::HdrPanorama { hdr, pano, bracket: p.get("bracket").and_then(Value::as_u64).unwrap_or(0) as usize },
    };
    // a pixel-shift merge starts like the camera's own files (no Auto Settings) and sits on its frames in a stack
    let pixel_shift = id == "merge.pixelShift";
    let finish = MergeFinish {
        auto_settings: p.get("autoSettings").and_then(Value::as_bool).unwrap_or(!pixel_shift),
        stack: p.get("stack").and_then(Value::as_bool).unwrap_or(pixel_shift),
        show_overlay: p.get("showOverlay").and_then(Value::as_bool).unwrap_or(false),
        layers: layer_output(id, p).map_err(bad)?,
        output_dir: p.get("outputDir").and_then(Value::as_str).filter(|d| !d.is_empty()).map(str::to_string),
    };
    Ok((kind, finish))
}

/// `layers: true` and its options (`layersFormat`, `bitDepth`, `colorSpace`, `includeMerged`, `deghostMask`).
fn layer_output(id: &str, p: &Value) -> std::result::Result<Option<crate::merge_layers::LayerOutput>, String> {
    use crate::merge_layers::{LayerOutput, LayersFormat};
    if !p.get("layers").and_then(Value::as_bool).unwrap_or(false) {
        return Ok(None);
    }
    if id != "merge.hdr" {
        return Err("layers: Merge to Layers is for HDR merges only".into());
    }
    let format = match p.get("layersFormat").and_then(Value::as_str).unwrap_or("psd") {
        "psd" => LayersFormat::Psd,
        "psb" => LayersFormat::Psb,
        "tiff" => return Err("layersFormat: layered TIFF is not supported; use psd or psb".into()),
        other => return Err(format!("layersFormat: unknown `{other}` (psd, psb)")),
    };
    let bit_depth = match p.get("bitDepth").and_then(Value::as_u64).unwrap_or(16) {
        8 => 8,
        16 => 16,
        other => return Err(format!("bitDepth: {other} (8 or 16)")),
    };
    let color_space = crate::cmd::edit_in::space(p.get("colorSpace").and_then(Value::as_str).unwrap_or("srgb"))?;
    Ok(Some(LayerOutput {
        format,
        bit_depth,
        color_space,
        include_merged: p.get("includeMerged").and_then(Value::as_bool).unwrap_or(false),
        deghost_mask: p.get("deghostMask").and_then(Value::as_bool).unwrap_or(false),
    }))
}

impl Session {
    /// Plan a merge of `ids` (catalog order of the given ids is kept). A photo named more than once is merged once
    /// (issue #1947): it adds nothing to the merge, and each copy would be decoded and held in memory.
    pub fn plan_merge(&self, kind: MergeKind, finish: MergeFinish, ids: &[PhotoId], preview: bool) -> Result<MergeJob> {
        let available = merge_memory_bytes(crate::memory::total_ram(), crate::memory::budget());
        self.plan_merge_with_budget(kind, finish, ids, preview, available)
    }

    pub(crate) fn plan_merge_with_budget(
        &self,
        kind: MergeKind,
        finish: MergeFinish,
        ids: &[PhotoId],
        preview: bool,
        available: usize,
    ) -> Result<MergeJob> {
        let mut sources = Vec::new();
        let preview_edge = preview.then_some(PREVIEW_EDGE);
        let mut dimensions = MergeDimensions::default();
        let read: ByteReader = match &self.media.file_bytes {
            Some(r) => r.clone(),
            None => Arc::new(|path: &str| std::fs::read(path).map_err(|e| format!("{path}: {e}"))),
        };
        let mut seen = std::collections::HashSet::new();
        for id in ids.iter().filter(|id| seen.insert(**id)) {
            let p = self.catalog.photo(*id).ok_or(lcc_catalog::CatalogError::NoPhoto(*id))?;
            match &p.source {
                Source::File { path } => {
                    // the catalog's (cropped) size bounds the decode: a raw's default crop trims only its margins
                    let (decoder_width, decoder_height) = (p.width, p.height);
                    dimensions.add(p.width, p.height, decoder_width, decoder_height, preview_edge).map_err(EngineError::Other)?;
                    sources.push((*id, path.clone()));
                }
                Source::Demo { .. } => return Err(EngineError::Other(format!("{} is a demo photo; Photo Merge needs photo files", p.file_name))),
            }
        }
        if sources.len() < kind.min_photos() {
            return Err(EngineError::Other(format!("select at least {} photos to merge", kind.min_photos())));
        }
        if matches!(kind, MergeKind::FocusStack { .. }) {
            // each frame is aligned to its neighbour: shooting order, whatever order they were picked in
            let key = |id: &PhotoId| self.catalog.photo(*id).map(|p| (p.captured.clone(), p.file_name.clone()));
            sources.sort_by_cached_key(|(id, _)| key(id));
        }
        let mut pixel_shift = None;
        if matches!(kind, MergeKind::PixelShift) {
            if preview {
                return Err(EngineError::Other("Pixel Shift has no preview".into()));
            }
            let (ordered, plan) = self.plan_pixel_shift_checked(&sources).map_err(|e| EngineError::Other(e.to_string()))?;
            // checked before anything is decoded: a result already there is neither replaced nor merged again
            if let Some(name) = ordered.first().and_then(|(_, first)| existing_pixel_shift_result(first, finish.output_dir.as_deref())) {
                return Err(EngineError::Other(PixelShiftRefusal::Exists { name }.to_string()));
            }
            sources = ordered;
            pixel_shift = Some(plan);
        }
        let estimated = dimensions.working_bytes(&kind, preview, available).map_err(EngineError::Other)?;
        check_merge_memory_budget(estimated, available).map_err(EngineError::Other)?;
        if let Some(lo) = finish.layers.as_ref().filter(|_| !preview) {
            if !matches!(kind, MergeKind::Hdr { .. }) {
                return Err(EngineError::Other("Merge to Layers is for HDR merges only".into()));
            }
            lo.check_size(sources.len(), dimensions.largest_frame).map_err(EngineError::Other)?;
        }
        let frames = preview.then(|| self.media.merge_frames.clone());
        Ok(MergeJob { kind, finish, sources, pixel_shift, preview: preview_edge, read, frames })
    }

    /// Whether `ids` can be merged as one pixel-shift series: their headers are read (not their pixels) and the result's
    /// file name is looked for, so a front end can refuse before anything is decoded.
    pub fn pixel_shift_check(&self, ids: &[PhotoId], output_dir: Option<&str>) -> std::result::Result<(), PixelShiftRefusal> {
        let mut sources = Vec::with_capacity(ids.len());
        for id in ids {
            let Some(p) = self.catalog.photo(*id) else { continue };
            match &p.source {
                Source::File { path } => sources.push((*id, path.clone())),
                _ => return Err(PixelShiftRefusal::NotFiles { name: p.file_name.clone() }),
            }
        }
        let (ordered, _) = self.plan_pixel_shift_checked(&sources)?;
        match ordered.first().and_then(|(_, first)| existing_pixel_shift_result(first, output_dir)) {
            Some(name) => Err(PixelShiftRefusal::Exists { name }),
            None => Ok(()),
        }
    }

    /// The frames Delete Merge Sources would delete for the merged photo `id`: those other than the reference frame that
    /// are still in the library (not deleted), and how many of those frames are gone. `None` when `id` is not a
    /// merged pixel-shift photo.
    pub fn merge_sources_to_delete(&self, id: PhotoId) -> Option<(Vec<PhotoId>, usize)> {
        let sources = self.catalog.photo(id)?.merge_sources.clone()?;
        let (mut present, mut missing) = (Vec::new(), 0);
        for f in sources.frames.iter().filter(|f| f.index != sources.reference) {
            match f.photo.filter(|p| self.catalog.photo(*p).is_some_and(|p| !p.deleted)) {
                Some(p) => present.push(p),
                None => missing += 1,
            }
        }
        Some((present, missing))
    }

    /// Check that `sources` are one complete pixel-shift series and put them in series order (frame 01 first). The
    /// message of a refusal says what is wrong with the selection.
    pub fn plan_pixel_shift(&self, sources: &[(PhotoId, String)]) -> Result<(Vec<(PhotoId, String)>, PixelShiftPlan)> {
        self.plan_pixel_shift_checked(sources).map_err(|e| EngineError::Other(e.to_string()))
    }

    /// [`Session::plan_pixel_shift`] with the refusal as a value, for a front end that words it in the person's language.
    pub fn plan_pixel_shift_checked(
        &self,
        sources: &[(PhotoId, String)],
    ) -> std::result::Result<(Vec<(PhotoId, String)>, PixelShiftPlan), PixelShiftRefusal> {
        use lcc_raw::pixel_shift::{HEADER_PREFIX_BYTES, detect, read_header, read_header_with};
        use std::io::Read;
        let mut headers = Vec::with_capacity(sources.len());
        for (id, path) in sources {
            let name = self.catalog.photo(*id).map(|p| p.file_name.clone()).unwrap_or_else(|| path.clone());
            let unreadable = |reason: String| PixelShiftRefusal::Unreadable { name: name.clone(), reason };
            let header = match &self.media.file_bytes {
                Some(read) => read(path).map_err(unreadable).map(|bytes| read_header(&bytes)),
                None => (|| -> std::io::Result<_> {
                    let mut file = std::fs::File::open(path)?;
                    let len = file.metadata()?.len();
                    let mut prefix = vec![0u8; usize::try_from(len).unwrap_or(usize::MAX).min(HEADER_PREFIX_BYTES)];
                    file.read_exact(&mut prefix)?;
                    Ok(read_header_with(&prefix, len, || std::fs::read(path).ok()))
                })()
                .map_err(|e| unreadable(e.to_string())),
            }?;
            headers.push(header.map_err(|why| PixelShiftRefusal::NotFrame { name: name.clone(), why })?);
        }
        let series = detect(&headers).map_err(PixelShiftRefusal::Series)?;
        if lcc_merge::pixelshift::pattern_for(series.count).is_none() {
            return Err(PixelShiftRefusal::Unsupported(series.count));
        }
        let ordered: Vec<(PhotoId, String)> = series.order.iter().map(|&i| sources[i].clone()).collect();
        let frames = series
            .order
            .iter()
            .map(|&i| {
                let h = &headers[i];
                let p = self.catalog.photo(sources[i].0);
                PixelShiftFrame {
                    index: h.index.unwrap_or(0),
                    file_name: p.map(|p| p.file_name.clone()).unwrap_or_default(),
                    captured: p.and_then(|p| p.captured.clone()).unwrap_or_default(),
                    counter: h.counter.unwrap_or(0),
                }
            })
            .collect();
        let ids: Vec<PhotoId> = ordered.iter().map(|(id, _)| *id).collect();
        let metadata = pixel_shift_metadata(&self.catalog, &ids);
        Ok((ordered, PixelShiftPlan { count: series.count, frames, metadata }))
    }

    /// Write a finished (non-preview) merge next to its first source, import it, select it and
    /// apply Auto Settings / the auto crop. Returns `{id, path, …info}`.
    pub fn finish_merge(&mut self, job: &MergeJob, out: MergeOutput) -> Result<Value> {
        let first = &job.sources.first().ok_or_else(|| EngineError::Other("nothing merged".into()))?.1;
        let output_hash = lcc_preview::hash_bytes(&out.dng).to_string();
        // Import deduplicates by content. Reuse an existing merge only while its live source has
        // matching bytes; stale, missing or deleted rows fail before another file is published.
        let candidates: Vec<(PhotoId, String, bool)> = self
            .catalog
            .photos()
            .filter_map(|p| {
                if p.content_hash.as_deref() != Some(output_hash.as_str()) {
                    return None;
                }
                let Source::File { path } = &p.source else { return None };
                Some((p.id, path.clone(), p.deleted))
            })
            .collect();
        let valid_existing = candidates
            .iter()
            .find(|(_, path, deleted)| {
                if *deleted {
                    return false;
                }
                let bytes = match &self.media.file_bytes {
                    Some(read) => read(path).ok(),
                    None => std::fs::read(path).ok(),
                };
                bytes.is_some_and(|bytes| lcc_preview::hash_bytes(&bytes).to_string() == output_hash)
            })
            .cloned();
        let (id, path, reused) = if let Some((id, path, _)) = valid_existing {
            (id, path, true)
        } else if let Some((id, path, deleted)) = candidates.first() {
            let state = if *deleted { "deleted" } else { "missing or changed" };
            return Err(EngineError::Other(format!(
                "existing merge result {id:?} has a {state} source at {path}; relink or remove it before repeating the merge"
            )));
        } else {
            // A new file under a free name (never replacing one), complete and synced before it appears.
            let target = merge_target(first, job.finish.output_dir.as_deref());
            let path = lcc_catalog::safe_file::write_new_unique(&mut output_names(&target, job.kind.suffix()), &out.dng)
                .map_err(|e| EngineError::Other(format!("could not write the merged DNG next to {first}: {e}")))?
                .to_string_lossy()
                .to_string();
            let report = match crate::import::import(self, std::slice::from_ref(&path), crate::import::ImportMode::Add) {
                Ok(report) => report,
                Err(e) => {
                    let _ = std::fs::remove_file(&path);
                    return Err(e);
                }
            };
            if let Some(id) = report.imported.first().copied().map(PhotoId) {
                (id, path, false)
            } else {
                let _ = std::fs::remove_file(&path);
                return Err(EngineError::Other(format!("the merged file {path} could not be imported: {:?}", report.failed)));
            }
        };
        let layered = match (&out.layers, &job.finish.layers) {
            (Some(bytes), Some(lo)) => Some(self.add_merge_layers(first, bytes, lo.format.extension())?),
            _ => None,
        };
        let mut under: Vec<PhotoId> = Vec::new();
        if !reused && job.finish.stack {
            under.extend(job.sources.iter().map(|(p, _)| *p));
        }
        if let Some((lid, _)) = &layered {
            under.push(*lid);
        }
        let mut ops: Vec<lcc_catalog::Op> = Vec::new();
        if !under.is_empty()
            && let Some(op) = self.catalog.stack_with_ops(id, &under)
        {
            ops.push(op);
        }
        // a pixel-shift merge remembers its frames (the DNG's own XMP says the same)
        if !reused && let (MergeKind::PixelShift, Some(plan)) = (&job.kind, &job.pixel_shift) {
            let frames = plan
                .frames
                .iter()
                .zip(&job.sources)
                .map(|(f, (photo, _))| lcc_catalog::MergeSource {
                    photo: Some(*photo),
                    index: f.index,
                    file_name: f.file_name.clone(),
                    captured: f.captured.clone(),
                    counter: f.counter,
                })
                .collect();
            let sources = lcc_catalog::MergeSources {
                engine: lcc_raw::merge_provenance::ENGINE,
                shot_count: plan.count,
                reference: 1,
                scale: lcc_merge::pixelshift::pattern_for(plan.count).map_or(1, |p| p.scale as u16),
                frames,
            };
            ops.push(lcc_catalog::Op::SetMergeSources { id, sources: Some(Box::new(sources)) });
        }
        if !ops.is_empty() {
            self.commit("Stack with Sources", lcc_catalog::Op::Batch { ops })?;
        }
        self.selection = crate::Selection::single(id);
        if !reused && job.finish.auto_settings {
            self.execute("develop.auto", &json!({}))?;
        }
        if !reused
            && let Some(c) = out.crop
            && let Some(d) = self.develop_of(id)
        {
            let mut d = (*d).clone();
            d.crop.geometry.rect = lcc_geom::Rect::from_xywh(c[0], c[1], c[2], c[3]);
            self.set_develop(id, d, "Auto Crop")?;
        }
        let mut info = out.info;
        info["id"] = json!(id.0);
        info["path"] = json!(path);
        info["bytes"] = json!(out.dng.len());
        // a repeated identical merge gives the verified result it already made (no new file, edits kept)
        info["reused"] = json!(reused);
        if let Some((lid, lpath)) = layered {
            info["layers"] = json!({"id": lid.0, "path": lpath, "bytes": out.layers.as_ref().map(Vec::len)});
        }
        Ok(info)
    }

    /// Write a Merge to Layers document next to `first` under a free name and import it.
    fn add_merge_layers(&mut self, first: &str, bytes: &[u8], ext: &'static str) -> Result<(PhotoId, String)> {
        let path = lcc_catalog::safe_file::write_new_unique(&mut output_names_ext(first, "HDR-Layers", ext), bytes)
            .map_err(|e| EngineError::Other(format!("could not write the layered file next to {first}: {e}")))?
            .to_string_lossy()
            .to_string();
        let report = match crate::import::import(self, std::slice::from_ref(&path), crate::import::ImportMode::Add) {
            Ok(report) => report,
            Err(e) => {
                let _ = std::fs::remove_file(&path);
                return Err(e);
            }
        };
        match report.imported.first().copied().map(PhotoId) {
            Some(id) => Ok((id, path)),
            None => {
                let _ = std::fs::remove_file(&path);
                Err(EngineError::Other(format!("the layered file {path} could not be imported: {:?}", report.failed)))
            }
        }
    }

    /// Plan, run and finish a merge synchronously (CLI, MCP, control channel).
    pub fn merge_now(&mut self, kind: MergeKind, finish: MergeFinish, ids: &[PhotoId]) -> Result<Value> {
        let job = self.plan_merge(kind, finish, ids, false)?;
        let out = job.run(&|_, _| true).map_err(EngineError::Other)?;
        self.finish_merge(&job, out)
    }
}
