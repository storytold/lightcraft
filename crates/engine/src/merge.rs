//! Photo Merge (HDR, Panorama, HDR Panorama) in the library.
//!
//! A merge is planned from the session ([`crate::Session::plan_merge`] → [`MergeJob`], which is
//! `Send` and self-contained), run anywhere (a worker thread in the app, inline for the CLI/MCP:
//! [`MergeJob::run`], with progress and cancellation), and finished on the session
//! ([`crate::Session::finish_merge`]): the DNG is written next to the first source photo
//! (`<name>-HDR.dng`, `-Pano.dng`, `-HDR-Pano.dng`), imported, selected, and given Auto Settings /
//! the auto-crop rectangle when asked. A *preview* run works on ≤ 1024 px frames and returns a
//! rendered image (with the deghost overlay when asked) instead of a file.

use std::sync::Arc;

use lightcraft_catalog::{PhotoId, Source};
use lightcraft_develop::DevelopSettings;
use lightcraft_merge::pano::{MAX_MEGAPIXELS, MIN_MEGAPIXELS};
use lightcraft_merge::{Deghost, Frame, HdrOptions, PanoOptions, Projection};
use lightcraft_pipeline::{Quality, RenderRequest, SourceInfo};
use lightcraft_raster::{Rgb32f, Rgba8};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{EngineError, Result, Session};

mod plan;
use plan::FRAME_BYTES;
pub use plan::PhotoSize;
pub(crate) use plan::{fill_unknown, holds, plan_memory, plan_preview};

/// Why `merge.hdrBatch` found nothing to merge.
pub const NO_BRACKET_SETS: &str =
    "no bracket sets among these photos: select stacks, or photos taken less than a second apart at different exposures";

/// Long edge of the frames of a preview merge.
pub const PREVIEW_EDGE: usize = 1024;

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
}

impl MergeKind {
    /// Refuse what the options alone rule out, before any photo is read: a panorama cap below
    /// 0.1 MP, the smallest the stitch keeps (`parse` refuses it too; a [`MergeKind`] can also be
    /// built directly), and an HDR panorama whose set bracket doesn't divide the photos into at least two
    /// positions, or (a bracket left to EXIF) can't be divided so by any bracket of two or more.
    pub(crate) fn check_before_reading(&self, photos: usize) -> std::result::Result<(), String> {
        let pano = match self {
            MergeKind::Panorama { pano } | MergeKind::HdrPanorama { pano, .. } => pano,
            MergeKind::Hdr { .. } => return Ok(()),
        };
        if pano.max_megapixels > 0.0 && pano.max_megapixels < MIN_MEGAPIXELS {
            return Err(below_least_cap(pano.max_megapixels));
        }
        if let MergeKind::HdrPanorama { bracket, .. } = self
            && *bracket >= 2
            && (!photos.is_multiple_of(*bracket) || photos / *bracket < 2)
        {
            return Err(uneven_brackets(photos, Some(*bracket)));
        }
        // a bracket left to EXIF is one of the sizes that divide the photos into at least two positions
        if let MergeKind::HdrPanorama { bracket: 0 | 1, .. } = self
            && !(2..=photos / 2).any(|b| photos.is_multiple_of(b))
        {
            return Err(uneven_brackets(photos, None));
        }
        Ok(())
    }

    fn suffix(&self) -> &'static str {
        match self {
            MergeKind::Hdr { .. } => "HDR",
            MergeKind::Panorama { .. } => "Pano",
            MergeKind::HdrPanorama { .. } => "HDR-Pano",
        }
    }
    fn min_photos(&self) -> usize {
        match self {
            MergeKind::HdrPanorama { .. } => 4,
            _ => 2,
        }
    }
}

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
}

/// Reads a source photo's bytes (by catalog path).
pub type ByteReader = Arc<dyn Fn(&str) -> std::result::Result<Vec<u8>, String> + Send + Sync>;

/// A planned merge, detached from the session.
#[derive(Clone)]
pub struct MergeJob {
    pub kind: MergeKind,
    pub finish: MergeFinish,
    pub sources: Vec<(PhotoId, String)>,
    /// `Some(edge)`: preview at that frame size; `None`: the full-resolution merge.
    pub preview: Option<usize>,
    pub read: ByteReader,
    /// Each source's size as the catalog has it (read from the file's header at import), so the
    /// merge is planned against its memory before a photo is decoded ([`plan_memory`]); `None`:
    /// read from the file's header when the merge starts.
    pub sizes: Vec<Option<PhotoSize>>,
    /// The memory the merge may use; `None`: [`merge_memory`] of this computer (tests set it).
    pub(crate) memory: Option<Memory>,
}

/// The memory a merge may use (bytes), and whether it is known: a computer whose memory can't be
/// read (Windows, for now) has a guess, which the plan follows but
/// never refuses a merge on ([`MergeJob::plan`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Memory {
    pub(crate) bytes: f64,
    pub(crate) known: bool,
}

impl Memory {
    /// This computer's: [`merge_memory`] of its RAM, a guess when that can't be read.
    fn of_this_computer() -> Memory {
        let ram = crate::memory::total_ram();
        Memory { bytes: merge_memory(ram), known: ram.is_some() }
    }
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
}

pub type ProgressFn<'a> = dyn Fn(f32, &str) -> bool + Sync + 'a;

/// How a merge decodes its photos ([`MergeJob::plan`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Load {
    /// The long edge they are held to (`None`: full size).
    pub(crate) edge: Option<usize>,
    /// Several at once.
    pub(crate) parallel: bool,
    /// Turned upright (panoramas).
    pub(crate) orient: bool,
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

impl MergeJob {
    /// Decode the sources, each held to `edge` on its long side (`None`: full size), several at
    /// once when `parallel` (the demosaic is parallel inside). `check(i, bytes)` runs on each file
    /// before it is decoded.
    fn load(
        &self,
        load: Load,
        check: &(dyn Fn(usize, &[u8]) -> std::result::Result<(), String> + Sync),
        progress: &ProgressFn,
        span: (f32, f32),
    ) -> std::result::Result<Vec<Frame>, String> {
        use rayon::prelude::*;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        let (done, stop) = (AtomicUsize::new(0), AtomicBool::new(false));
        let n = self.sources.len();
        let one = |(i, (_, path)): (usize, &(PhotoId, String))| {
            // cancelled: a photo not started yet isn't read (several are in flight in a parallel preview)
            if stop.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            let bytes = (self.read)(path)?;
            check(i, &bytes)?;
            // a raw without colour matrices of its own starts from the colour it has when opened alone (#965)
            let f = lightcraft_merge::load_frame_with(
                &bytes,
                load.edge,
                self.preview.is_some(),
                load.orient,
                &crate::camera_preview::starting_colour_tags,
            )
            .map_err(|e| format!("{path}: {e}"));
            let k = done.fetch_add(1, Ordering::Relaxed) + 1;
            if !progress(span.0 + (span.1 - span.0) * k as f32 / n as f32, "Reading photos") {
                stop.store(true, Ordering::Relaxed);
                return Err("cancelled".into());
            }
            f
        };
        // Full-size photos are decoded one at a time: decoding several 60 MP raws at once multiplied the decoders'
        // scratch memory, and a 7-frame bracket ran out of memory. A preview's are too, when several don't fit or it is
        // unplanned.
        if load.parallel { self.sources.par_iter().enumerate().map(one).collect() } else { self.sources.iter().enumerate().map(one).collect() }
    }

    /// The sources' sizes: the catalog's, else the file's header (read, not decoded), else the
    /// largest known ([`fill_unknown`]); `None` when none is known (the merge runs unplanned).
    fn known_sizes(&self) -> std::result::Result<Option<Vec<PhotoSize>>, String> {
        let mut sizes = Vec::with_capacity(self.sources.len());
        for (i, (_, path)) in self.sources.iter().enumerate() {
            // a size of a pixel or less, or without its file size, is unknown ([`PhotoSize::new`])
            let given = self.sizes.get(i).copied().flatten().and_then(|s| PhotoSize::new(s.width, s.height, s.bytes));
            sizes.push(match given {
                Some(s) => Some(s),
                None => PhotoSize::from_header(&(self.read)(path)?),
            });
        }
        Ok(fill_unknown(&sizes))
    }

    /// Plan the merge against `memory` before anything is decoded: how the photos are decoded, and
    /// what each was planned at (empty: unplanned). With the memory unknown (a guess) it is planned
    /// the same, but what the guess would refuse runs as it did before merges were planned: a
    /// full-size merge at full size, a preview one photo at a time.
    pub(crate) fn plan(&self, memory: Memory) -> std::result::Result<(Load, Vec<PhotoSize>), String> {
        let orient = matches!(self.kind, MergeKind::Panorama { .. });
        let Some(sizes) = self.known_sizes()? else {
            // unplanned: one photo at a time, a preview's too (several full-size decodes at once are
            // what the plan would have had to allow)
            return Ok((Load { edge: self.preview, parallel: false, orient }, Vec::new()));
        };
        let room = memory.bytes;
        let load = match self.preview {
            Some(edge) => {
                let parallel = match plan_preview(&self.kind, &sizes, room, rayon::current_num_threads()) {
                    Err(_) if !memory.known => false,
                    p => p?,
                };
                Load { edge: Some(edge), parallel, orient }
            }
            None => {
                let edge = match plan_memory(&self.kind, &sizes, room) {
                    Err(_) if !memory.known => None,
                    e => e?,
                };
                Load { edge, parallel: false, orient }
            }
        };
        Ok((load, sizes))
    }

    /// Run the merge. `progress(fraction, stage)` returns false to cancel.
    pub fn run(&self, progress: &ProgressFn) -> std::result::Result<MergeOutput, String> {
        if self.sources.len() < self.kind.min_photos() {
            return Err(format!("select at least {} photos", self.kind.min_photos()));
        }
        // what can be refused from the options alone is refused before any photo is read
        self.kind.check_before_reading(self.sources.len())?;
        if !progress(0.0, "Reading photos") {
            return Err("cancelled".into());
        }
        let sub = |a: f32, b: f32| move |f: f32, s: &str| progress(a + (b - a) * f, s);
        let memory = self.memory.unwrap_or_else(Memory::of_this_computer);
        let room = memory.bytes;
        // planned before anything is decoded: it runs as it is, with smaller frames (panoramas), or
        // is refused (never on a guessed memory)
        let (load, planned) = self.plan(memory)?;
        // just before each photo is decoded its header is read again, and the plan checked with it
        // (and every photo checked so far): a raw's sensor can be larger than its picture, which the
        // catalog keeps, and a file can have changed since it was imported
        let seen = std::sync::Mutex::new(planned.clone());
        // each photo's own size: its header's, read before its decode, else the catalog's (never a
        // stand-in the plan took for one it didn't know, [`fill_unknown`])
        let real = std::sync::Mutex::new(
            (0..self.sources.len())
                .map(|i| self.sizes.get(i).copied().flatten().and_then(|s| PhotoSize::new(s.width, s.height, s.bytes)))
                .collect::<Vec<_>>(),
        );
        let threads = self.preview.map(|_| if load.parallel { rayon::current_num_threads() } else { 1 });
        let check = |i: usize, bytes: &[u8]| -> std::result::Result<(), String> {
            let header = PhotoSize::from_header(bytes);
            if let Some(now) = header
                && let Some(s) = real.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get_mut(i)
            {
                *s = Some(now);
            }
            if !memory.known {
                return Ok(());
            }
            let (Some(&was), Some(now)) = (planned.get(i), header) else { return Ok(()) };
            let mut sizes = seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(s) = sizes.get_mut(i) {
                *s = now;
            }
            let Err(e) = holds(&self.kind, &sizes, room, load.edge, threads) else { return Ok(()) };
            let path = self.sources.get(i).map_or("a photo", |s| s.1.as_str());
            // the cause the user can act on, apart: a file that changed since it was imported
            let changed = if changed_since_import(was, now) {
                ". It looks changed since it was imported: synchronize its folder, then merge again"
            } else {
                ""
            };
            Err(format!("{path} needs more memory to decode than expected: {e}{changed}"))
        };
        // the photos' own pixels beside each frame's, when the plan made them smaller
        let reduced = self.preview.is_none() && load.edge.is_some() && planned.len() == self.sources.len();
        let areas = |frames: &[Frame], groups: usize| -> Vec<(f64, f64)> {
            if !reduced {
                return Vec::new();
            }
            let real = real.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let b = (real.len() / frames.len().max(1)).max(1).min(groups.max(1));
            photo_areas(&real, &frames.iter().map(|f| (f.width() * f.height()) as f64).collect::<Vec<_>>(), b)
        };
        // the reference frame's lens corrections (HDR); panoramas carry none
        let mut lens = Vec::new();
        let (image, color, orientation, metadata, baseline, samples, crop, mut info, ghost) = match &self.kind {
            MergeKind::Hdr { hdr } => {
                let frames = self.load(load, &check, progress, (0.0, 0.3))?;
                let p = sub(0.3, 0.9);
                let mut r = lightcraft_merge::merge_hdr(frames, hdr, &p).map_err(err)?;
                lens = std::mem::take(&mut r.lens);
                let info = json!({
                    "reference": self.sources[r.reference].0.0,
                    "ev": r.ev, "evExif": r.ev_exif,
                    "alignment": r.alignments.iter().map(|a| json!({"model": a.model, "inliers": a.inliers, "rmsPx": a.rms})).collect::<Vec<_>>(),
                });
                let ghost = (!r.ghost.data.is_empty()).then(|| r.ghost.oriented(r.orientation));
                (r.radiance, r.color, r.orientation, r.metadata, r.baseline_exposure, lightcraft_merge::DngSamples::Half, None, info, ghost)
            }
            MergeKind::Panorama { pano } => {
                let frames = self.load(load, &check, progress, (0.0, 0.25))?;
                let p = sub(0.25, 0.9);
                let scale = areas(&frames, 1);
                let (fit, floor) = fit_decoded(pano, pixels(&frames), &scale, memory, self.preview.is_some())?;
                let r = lightcraft_merge::stitch(frames, &fit, &p).map_err(err)?;
                reaches_floor(&r, pano.max_megapixels, floor, &scale, memory)?;
                let info = pano_info(&r, &self.sources, pano, &fit, &scale);
                (
                    r.image,
                    r.color,
                    lightcraft_geom::Orientation::Normal,
                    r.metadata,
                    r.baseline_exposure,
                    lightcraft_merge::DngSamples::U16,
                    r.crop,
                    info,
                    None,
                )
            }
            MergeKind::HdrPanorama { hdr, pano, bracket } => {
                let frames = self.load(load, &check, progress, (0.0, 0.2))?;
                let b =
                    if *bracket >= 2 { *bracket } else { detect_bracket(&frames).ok_or("could not detect the bracket size from EXIF; set it")? };
                if !frames.len().is_multiple_of(b) || frames.len() / b < 2 {
                    return Err(uneven_brackets(frames.len(), Some(b)));
                }
                let groups = frames.len() / b;
                let mut merged = Vec::new();
                let mut it = frames.into_iter();
                for g in 0..groups {
                    let set: Vec<Frame> = it.by_ref().take(b).collect();
                    let p = sub(0.2 + 0.4 * g as f32 / groups as f32, 0.2 + 0.4 * (g + 1) as f32 / groups as f32);
                    let r = lightcraft_merge::merge_hdr(set, hdr, &p).map_err(err)?;
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
                    };
                    f.orient();
                    merged.push(f);
                }
                let p = sub(0.6, 0.9);
                let scale = areas(&merged, b);
                let (fit, floor) = fit_decoded(pano, pixels(&merged), &scale, memory, self.preview.is_some())?;
                let r = lightcraft_merge::stitch(merged, &fit, &p).map_err(err)?;
                reaches_floor(&r, pano.max_megapixels, floor, &scale, memory)?;
                let mut info = pano_info(&r, &self.sources, pano, &fit, &scale);
                info["bracket"] = json!(b);
                (
                    r.image,
                    r.color,
                    lightcraft_geom::Orientation::Normal,
                    r.metadata,
                    r.baseline_exposure,
                    lightcraft_merge::DngSamples::Half,
                    r.crop,
                    info,
                    None,
                )
            }
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
        metadata.software = Some("LightCraft Photo Merge".into());
        let dng = lightcraft_merge::write_linear_dng(&image, &color, orientation, &metadata, baseline, samples, &lens).map_err(err)?;
        info["width"] = json!(image.width);
        info["height"] = json!(image.height);
        let preview = match self.preview {
            Some(edge) => Some(render_preview(&dng, edge, self.finish.auto_settings, crop, ghost.as_ref().filter(|_| self.finish.show_overlay))?),
            None => None,
        };
        progress(1.0, "Done");
        Ok(MergeOutput { dng, preview, crop, info })
    }
}

/// Whether a file's header says another photo than the catalog recorded at import: another size
/// (in either orientation: the catalog keeps a photo's size upright, a raw's header before it is
/// turned) or another file size.
pub(crate) fn changed_since_import(catalog: PhotoSize, header: PhotoSize) -> bool {
    let shape = |s: PhotoSize| (s.width.min(s.height), s.width.max(s.height), s.bytes);
    shape(catalog) != shape(header)
}

/// Each stitched frame's photo pixels (the largest of its `group` photos: an HDR panorama's
/// bracket) and its own pixels (`frames`). A photo whose own size isn't known counts as its frame,
/// so `nativeMegapixels` is never made up from the size the plan took for it.
pub(crate) fn photo_areas(real: &[Option<PhotoSize>], frames: &[f64], group: usize) -> Vec<(f64, f64)> {
    real.chunks(group.max(1))
        .zip(frames)
        .map(|(set, &frame)| {
            let photos = set.iter().try_fold(0.0f64, |m, s| s.map(|s| m.max(f64::from(s.width) * f64::from(s.height))));
            (photos.unwrap_or(frame), frame)
        })
        .collect()
}

/// `asked` is what the command asked for, `fit` what ran (held to memory): when the panorama came
/// out smaller than the photos' own resolution, `limit` says why (`memory`, `size`: Lightroom's
/// 512 MP / 65,000 px, or `asked`). `areas`: each stitched frame's photo pixels and its own, when
/// [`plan_memory`] decoded the photos smaller (memory decided that); empty otherwise.
fn pano_info(r: &lightcraft_merge::PanoResult, sources: &[(PhotoId, String)], asked: &PanoOptions, fit: &PanoOptions, areas: &[(f64, f64)]) -> Value {
    let megapixels = (r.image.width * r.image.height) as f64 / 1e6;
    let shrunk = shrunk(r, areas);
    // the resolution of the photos themselves, and (smaller frames) memory held it to the frames'
    let (native, fit_mp) = if shrunk > 1.0 {
        (r.native_megapixels * shrunk, fit.max_megapixels.min(r.native_megapixels))
    } else {
        (r.native_megapixels, fit.max_megapixels)
    };
    let limit = size_limit(asked.max_megapixels, fit_mp, native, megapixels);
    json!({
        "projection": r.projection,
        "used": r.used.iter().filter_map(|&i| sources.get(i).map(|s| s.0.0)).collect::<Vec<_>>(),
        "fov": [r.fov.0, r.fov.1],
        "rmsPx": r.rms,
        "gains": r.gains,
        "crop": r.crop,
        "megapixels": megapixels,
        "nativeMegapixels": native,
        "limit": limit,
    })
}

/// How many times their frames' pixels the photos the panorama used have (`areas`: each stitched
/// frame's photo pixels and its own, [`photo_areas`]; 1 when the photos weren't made smaller).
fn shrunk(r: &lightcraft_merge::PanoResult, areas: &[(f64, f64)]) -> f64 {
    let (photos, frames) = r.used.iter().filter_map(|&i| areas.get(i)).fold((0.0, 0.0), |(p, f), &(a, b)| (p + a, f + b));
    if frames > 0.0 { (photos / frames).max(1.0) } else { 1.0 }
}

/// A panorama stitched from frames the plan made smaller must still reach its `floor`
/// (megapixels), or its own resolution when that is less: the plan keeps enough of their pixels
/// for it ([`plan_memory`]), but their overlap is known only once they are stitched, and a stitch
/// makes no more pixels than its frames give. Refused when it doesn't (never on a guessed memory),
/// naming the panorama's size as the `maxMegapixels` that would have kept it, when one would
/// (`asked`: the one asked for, [`pano_remedy`]).
fn reaches_floor(r: &lightcraft_merge::PanoResult, asked: f64, floor: f64, areas: &[(f64, f64)], memory: Memory) -> std::result::Result<(), String> {
    let canvas = r.native_megapixels;
    if !memory.known || !below_floor(canvas, shrunk(r, areas), floor) {
        return Ok(());
    }
    Err(format!(
        "not enough memory for this panorama: its photos had to be decoded smaller to fit the merge's memory budget, and the panorama of them has {canvas:.1} MP, under the {} MP it is made at the least; {}",
        round_megapixels(floor),
        pano_remedy(asked, floor, canvas)
    ))
}

/// Whether a panorama of `canvas` megapixels, stitched from frames `shrink` times smaller than its
/// photos, falls under its `floor`, or under its own resolution (`canvas × shrink`) when that is
/// less (the stitch's own rounding aside). Never when its photos weren't made smaller.
pub(crate) fn below_floor(canvas: f64, shrink: f64, floor: f64) -> bool {
    shrink > 1.0 && canvas < floor.min(canvas * shrink) * 0.98
}

/// Why a panorama of `megapixels` came out smaller than its photos' resolution (`native`), if it
/// did: `memory` when [`fit_with_floor`] held it below the size `asked` for, `asked`, or `size`.
pub(crate) fn size_limit(asked: f64, fit: f64, native: f64, megapixels: f64) -> Option<&'static str> {
    let why = if fit < asked && fit < native {
        "memory"
    } else if asked < MAX_MEGAPIXELS && asked < native {
        "asked"
    } else {
        "size"
    };
    (megapixels < native * 0.98).then_some(why)
}

/// Peak memory of a panorama per pixel of its output (bytes), measured as the peak resident size
/// of synthetic stitches of 20–81 MP from 38–154 MP of photos, rounded up from the smaller ones
/// (the larger need less): while the photos are projected they are still held, beside the tiles
/// ([`PANO_TILE_BYTES`] on top of the photos' own 12 bytes a pixel); the blend that follows, the
/// photos freed, peaks at [`PANO_BLEND_BYTES`]. Writing the DNG takes less than either.
const PANO_TILE_BYTES: f64 = 36.0;
const PANO_BLEND_BYTES: f64 = 68.0;

/// What a merge may use of the computer's memory: three quarters of it (the rest is the system's
/// and the other apps'), less the app's own memory budget (its caches).
fn merge_memory(ram: Option<usize>) -> f64 {
    // a computer whose memory we can't read (Windows) is taken to have 8 GB
    let ram = ram.unwrap_or(8 << 30) as f64;
    (ram * 0.75 - crate::memory::budget() as f64).max(0.0)
}

/// `pano` with its size held to what fits in `room` bytes (the merge's memory, [`merge_memory`])
/// beside its photos (`frame_px` pixels, 12 bytes each, loaded already): a panorama of five 51 MP
/// photos may have up to 166 MP on a 16 GB computer, 355 MP on 32 GB. Never below its floor
/// ([`panorama_floor`]): a merge whose memory can't hold that is refused, naming the memory it
/// needs and has (issue #3708: the floor used to be kept whatever the memory, and a 40 MP stitch
/// peaks near 2.7 GB, more than a small computer or a memory limit allows; a refusal is preferred
/// to a panorama quietly far below it). Between the floor and the full size the result says
/// `limit: "memory"`. Never above what was asked. The floor (megapixels) is given:
/// [`panorama_floor`] of the photos' own pixels, also when the plan decoded them smaller than they
/// are, so smaller frames don't lower it.
pub(crate) fn fit_with_floor(pano: &PanoOptions, frame_px: f64, floor: f64, room: f64) -> std::result::Result<PanoOptions, String> {
    let fits = ((room - frame_px * FRAME_BYTES) / PANO_TILE_BYTES).min(room / PANO_BLEND_BYTES) / 1e6;
    // a NaN `fits` (a NaN `room`) is refused too
    if fits.is_nan() || fits < floor {
        let need = (frame_px * FRAME_BYTES + floor * 1e6 * PANO_TILE_BYTES).max(floor * 1e6 * PANO_BLEND_BYTES);
        return Err(format!(
            "not enough memory for this panorama: at {} MP, the smallest it is made, it needs about {:.0} MB, its photos included, {}; {}",
            round_megapixels(floor),
            mb(need).ceil(),
            merge_budget(mb(room.max(0.0)).floor()),
            pano_remedy(pano.max_megapixels, floor, fits)
        ));
    }
    Ok(PanoOptions { max_megapixels: pano.max_megapixels.min(fits), ..pano.clone() })
}

/// How a refusal names the merge's memory (`mb`, [`merge_memory`]): a budget, not the memory free
/// now, so closing other apps doesn't raise it.
fn merge_budget(mb: f64) -> String {
    format!("but the merge's budget is {mb:.0} MB (three quarters of this computer's memory, less the app's caches)")
}

/// What a panorama refused for its `floor` can change: fewer or smaller photos, and, when the floor
/// is 40 MP or the size `asked` for (not the photos' own pixels) and a smaller size `fits`
/// (megapixels), a `maxMegapixels` up to that (a command option: the desktop's Merge dialog has no
/// size control).
fn pano_remedy(asked: f64, floor: f64, fits: f64) -> String {
    let cap = MIN_PANORAMA_MEGAPIXELS.min(asked).max(MIN_MEGAPIXELS);
    let fits = (fits * 10.0).floor() / 10.0;
    if floor >= cap && fits >= MIN_MEGAPIXELS && fits < floor {
        format!("merge fewer or smaller photos, or set maxMegapixels to {fits} or less (command line or MCP)")
    } else {
        "merge fewer or smaller photos".to_owned()
    }
}

/// [`fit_with_floor`] for decoded frames of `frame_px` pixels, the floor following the photos' own
/// pixels (`areas`, each stitched frame's photo pixels and its own, [`photo_areas`]; empty: the
/// frames are the photos' size). On a guessed memory a panorama it would refuse is made at its
/// floor, as before merges were planned ([`Memory::known`]). A `preview`'s floor is at most
/// [`MIN_PREVIEW_MEGAPIXELS`]. Returns the floor too.
fn fit_decoded(
    pano: &PanoOptions,
    frame_px: f64,
    areas: &[(f64, f64)],
    memory: Memory,
    preview: bool,
) -> std::result::Result<(PanoOptions, f64), String> {
    let photos = if areas.is_empty() { frame_px } else { areas.iter().map(|a| a.0).sum() };
    let floor = panorama_floor(pano.max_megapixels, photos);
    let floor = if preview { floor.min(MIN_PREVIEW_MEGAPIXELS) } else { floor };
    let fit = match fit_with_floor(pano, frame_px, floor, memory.bytes) {
        Err(_) if !memory.known => PanoOptions { max_megapixels: pano.max_megapixels.min(floor), ..pano.clone() },
        fit => fit?,
    };
    Ok((fit, floor))
}

/// [`fit_with_floor`] with the floor of `frame_px` (frames of the photos' own size; tests).
#[cfg(test)]
pub(crate) fn fit_in_memory(pano: &PanoOptions, frame_px: f64, room: f64) -> std::result::Result<PanoOptions, String> {
    fit_with_floor(pano, frame_px, panorama_floor(pano.max_megapixels, frame_px), room)
}

/// The smallest a panorama is made to fit its memory (megapixels): [`MIN_PANORAMA_MEGAPIXELS`],
/// or less when less was `asked` for (`min` ignores a NaN one; never under the 0.1 MP cap the
/// stitch keeps at the least, [`MIN_MEGAPIXELS`]), or when its photos (`frame_px` pixels,
/// standing for the panorama's own resolution, known only once they are stitched) have less,
/// even under 0.1 MP: the stitch never scales photos up, so a floor above them couldn't be made.
pub(crate) fn panorama_floor(asked: f64, frame_px: f64) -> f64 {
    MIN_PANORAMA_MEGAPIXELS.min(asked).max(MIN_MEGAPIXELS).min(frame_px / 1e6)
}

/// The refusal of an HDR panorama of `photos` that don't split into two or more brackets of
/// `bracket` photos (`None`: left to EXIF, of any one size of two or more), in countable terms.
fn uneven_brackets(photos: usize, bracket: Option<usize>) -> String {
    match bracket {
        Some(b) => format!(
            "{photos} photos can't be split into brackets of {b}: an HDR panorama needs two or more brackets of the same size, so a multiple of {b} photos, at least {}",
            b.saturating_mul(2)
        ),
        None => format!(
            "{photos} photos can't be split into two or more brackets of the same size, two photos or more each: an HDR panorama needs 4, 6, 8, 9, 10 or another such number of photos"
        ),
    }
}

/// The refusal of a `maxMegapixels` of `v`, below [`MIN_MEGAPIXELS`] (the stitch would raise it).
fn below_least_cap(v: f64) -> String {
    format!("maxMegapixels: {v} is below {MIN_MEGAPIXELS} MP, the smallest size limit the stitch keeps")
}

/// `mp` megapixels as the refusals name them: to 0.1 MP, or two significant digits below 0.1 MP
/// (a floor of tiny photos), so `0.05` isn't named as 0.1 MP.
pub(crate) fn round_megapixels(mp: f64) -> f64 {
    if !(mp > 0.0 && mp < 0.1) {
        return (mp * 10.0).round() / 10.0;
    }
    // mp > 0, so the exponent is finite; held to 12 digits for a subnormal one
    let digits = (1.0 - mp.log10().floor()).min(12.0) as i32;
    let k = 10f64.powi(digits);
    (mp * k).round() / k
}

/// The pixels of `frames`.
fn pixels(frames: &[Frame]) -> f64 {
    frames.iter().map(|f| (f.width() * f.height()) as f64).sum()
}

/// The smallest panorama [`fit_with_floor`] plans (megapixels): the size every panorama had before
/// it followed its photos' resolution. A merge whose memory can't hold it (or the smaller
/// [`panorama_floor`]) is refused rather than made smaller.
pub(crate) const MIN_PANORAMA_MEGAPIXELS: f64 = 40.0;

/// A preview's floor (megapixels): it shows how the merge will look, so it is made down to this
/// (or less, as [`panorama_floor`] allows) rather than refused for the floor of the panorama it
/// previews.
pub(crate) const MIN_PREVIEW_MEGAPIXELS: f64 = 1.0;

/// Bytes in MB (MiB).
fn mb(bytes: f64) -> f64 {
    bytes / (1u64 << 20) as f64
}

/// Bracket size from EXIF: the smallest period of the exposure sequence (≥ 2).
pub fn detect_bracket(frames: &[Frame]) -> Option<usize> {
    let ev: Vec<f64> = frames.iter().map(|f| f.exposure).collect::<Option<_>>()?;
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
    ghost: Option<&lightcraft_raster::Plane>,
) -> std::result::Result<Rgba8, String> {
    let (src, info) = crate::files::load_bytes(dng, edge)?;
    let mut s = DevelopSettings::for_raw(info.as_shot_temp, info.as_shot_tint);
    if auto {
        apply_auto(&mut s, &src, &info);
    }
    if let Some(c) = crop {
        s.crop.geometry.rect = lightcraft_geom::Rect::from_xywh(c[0], c[1], c[2], c[3]);
    }
    let req = RenderRequest { quality: Quality::Full, apply_crop: false, ..RenderRequest::fit(edge, edge) };
    let mut img = lightcraft_pipeline::render(&src, &info, &s, &req).image;
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
    let a = lightcraft_pipeline::auto::auto_tone(src, info, s);
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
    let p = std::path::Path::new(first);
    let dir = p.parent().map(|d| d.to_path_buf()).unwrap_or_default();
    let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "Merge".into());
    let suffix = suffix.to_string();
    (1u64..).map(move |k| dir.join(if k == 1 { format!("{stem}-{suffix}.dng") } else { format!("{stem}-{suffix}-{k}.dng") }))
}

/// Parse `merge.*` command parameters into a kind and finishing options.
pub fn parse(id: &str, p: &Value) -> Result<(MergeKind, MergeFinish)> {
    let bad = |m: String| EngineError::BadParams { cmd: id.into(), msg: m };
    let deghost: Deghost = serde_json::from_value(p.get("deghost").cloned().unwrap_or(json!("none"))).map_err(|e| bad(format!("deghost: {e}")))?;
    let hdr = HdrOptions { align: p.get("align").and_then(Value::as_bool).unwrap_or(true), deghost };
    let projection: Projection =
        serde_json::from_value(p.get("projection").cloned().unwrap_or(json!("auto"))).map_err(|e| bad(format!("projection: {e}")))?;
    // the stitch raises a cap below 0.1 MP to 0.1 MP, so a smaller one couldn't be kept: refused, not exceeded
    let max_megapixels = match p.get("maxMegapixels").and_then(Value::as_f64) {
        Some(v) if v > 0.0 && v < MIN_MEGAPIXELS => return Err(bad(below_least_cap(v))),
        Some(v) if v > 0.0 => v.min(MAX_MEGAPIXELS),
        _ => MAX_MEGAPIXELS,
    };
    let pano = PanoOptions {
        projection,
        boundary_warp: p.get("boundaryWarp").and_then(Value::as_f64).unwrap_or(0.0).clamp(0.0, 100.0),
        auto_crop: p.get("autoCrop").and_then(Value::as_bool).unwrap_or(false),
        fill_edges: p.get("fillEdges").and_then(Value::as_bool).unwrap_or(false),
        // the photos' own resolution unless asked (held to what fits in memory when it runs)
        max_megapixels,
    };
    let kind = match id {
        "merge.hdr" => MergeKind::Hdr { hdr },
        "merge.panorama" => MergeKind::Panorama { pano },
        _ => MergeKind::HdrPanorama { hdr, pano, bracket: p.get("bracket").and_then(Value::as_u64).unwrap_or(0) as usize },
    };
    let finish = MergeFinish {
        auto_settings: p.get("autoSettings").and_then(Value::as_bool).unwrap_or(true),
        stack: p.get("stack").and_then(Value::as_bool).unwrap_or(false),
        show_overlay: p.get("showOverlay").and_then(Value::as_bool).unwrap_or(false),
    };
    Ok((kind, finish))
}

impl Session {
    /// Plan a merge of `ids` (catalog order of the given ids is kept). A photo named more than once is merged once
    /// (issue #1947): it adds nothing to the merge, and each copy would be decoded and held in memory.
    pub fn plan_merge(&self, kind: MergeKind, finish: MergeFinish, ids: &[PhotoId], preview: bool) -> Result<MergeJob> {
        let mut sources = Vec::new();
        let mut sizes = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for id in ids.iter().filter(|id| seen.insert(**id)) {
            let p = self.catalog.photo(*id).ok_or(lightcraft_catalog::CatalogError::NoPhoto(*id))?;
            match &p.source {
                Source::File { path } => {
                    sources.push((*id, path.clone()));
                    sizes.push(PhotoSize::new(p.width, p.height, p.file_size));
                }
                Source::Demo { .. } => return Err(EngineError::Other(format!("{} is a demo photo; Photo Merge needs photo files", p.file_name))),
            }
        }
        if sources.len() < kind.min_photos() {
            return Err(EngineError::Other(format!("select at least {} photos to merge", kind.min_photos())));
        }
        let read: ByteReader = match &self.media.file_bytes {
            Some(r) => r.clone(),
            None => Arc::new(|path: &str| std::fs::read(path).map_err(|e| format!("{path}: {e}"))),
        };
        Ok(MergeJob { kind, finish, sources, preview: preview.then_some(PREVIEW_EDGE), read, sizes, memory: None })
    }

    /// Write a finished (non-preview) merge next to its first source, import it, select it and
    /// apply Auto Settings / the auto crop. Returns `{id, path, …info}`.
    pub fn finish_merge(&mut self, job: &MergeJob, out: MergeOutput) -> Result<Value> {
        let first = &job.sources.first().ok_or_else(|| EngineError::Other("nothing merged".into()))?.1;
        // a new file under a free name (never replacing one), complete and synced before it appears
        let path = lightcraft_catalog::safe_file::write_new_unique(&mut output_names(first, job.kind.suffix()), &out.dng)
            .map_err(|e| EngineError::Other(format!("could not write the merged DNG next to {first}: {e}")))?
            .to_string_lossy()
            .to_string();
        let report = crate::import::import(self, std::slice::from_ref(&path), crate::import::ImportMode::Add)?;
        let id = report
            .imported
            .first()
            .copied()
            .map(PhotoId)
            .ok_or_else(|| EngineError::Other(format!("the merged file {path} could not be imported: {:?}", report.failed)))?;
        if job.finish.stack {
            let sources: Vec<PhotoId> = job.sources.iter().map(|(p, _)| *p).collect();
            if let Some(op) = self.catalog.stack_with_ops(id, &sources) {
                self.commit("Stack with Sources", op)?;
            }
        }
        self.selection = crate::Selection::single(id);
        if job.finish.auto_settings {
            self.execute("develop.auto", &json!({}))?;
        }
        if let Some(c) = out.crop
            && let Some(d) = self.develop_of(id)
        {
            let mut d = (*d).clone();
            d.crop.geometry.rect = lightcraft_geom::Rect::from_xywh(c[0], c[1], c[2], c[3]);
            self.set_develop(id, d, "Auto Crop")?;
        }
        let mut info = out.info;
        info["id"] = json!(id.0);
        info["path"] = json!(path);
        info["bytes"] = json!(out.dng.len());
        Ok(info)
    }

    /// Plan, run and finish a merge synchronously (CLI, MCP, control channel).
    pub fn merge_now(&mut self, kind: MergeKind, finish: MergeFinish, ids: &[PhotoId]) -> Result<Value> {
        let job = self.plan_merge(kind, finish, ids, false)?;
        let out = job.run(&|_, _| true).map_err(EngineError::Other)?;
        self.finish_merge(&job, out)
    }
}

#[cfg(test)]
mod tests {
    /// A computer whose memory we can't read is taken to have 8 GB.
    #[test]
    fn unknown_memory_is_taken_as_8_gb() {
        assert_eq!(super::merge_memory(None), super::merge_memory(Some(8 << 30)));
    }

    /// A cap below 0.1 MP, the smallest the stitch keeps, is refused, not exceeded (#3708).
    #[test]
    fn a_max_megapixels_below_the_stitch_minimum_is_refused() {
        use super::{MergeKind, parse};
        use serde_json::json;
        let cap = |p: serde_json::Value| match parse("merge.panorama", &p).map(|(kind, _)| kind) {
            Ok(MergeKind::Panorama { pano }) => Ok(pano.max_megapixels),
            Ok(_) => Err("not a panorama".to_owned()),
            Err(e) => Err(e.to_string()),
        };
        let refused = cap(json!({"maxMegapixels": 0.05})).unwrap_err();
        assert!(refused.contains("maxMegapixels") && refused.contains("0.1 MP"), "{refused}");
        assert_eq!(cap(json!({"maxMegapixels": 0.1})), Ok(0.1));
        assert_eq!(cap(json!({"maxMegapixels": 24.0})), Ok(24.0));
        let max = lightcraft_merge::pano::MAX_MEGAPIXELS;
        assert_eq!(cap(json!({})), Ok(max), "missing: the default");
        assert_eq!(cap(json!({"maxMegapixels": 0})), Ok(max), "0: the default, as before");
        assert_eq!(cap(json!({"maxMegapixels": 1e9})), Ok(max), "held to the maximum");
    }
}
