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
use lightcraft_merge::pano::MAX_MEGAPIXELS;
use lightcraft_merge::{Deghost, Frame, HdrOptions, PanoOptions, Projection};
use lightcraft_pipeline::{Quality, RenderRequest, SourceInfo};
use lightcraft_raster::{Rgb32f, Rgba8};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{EngineError, Result, Session};

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

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

impl MergeJob {
    fn load(&self, orient: bool, progress: &ProgressFn, span: (f32, f32)) -> std::result::Result<Vec<Frame>, String> {
        use rayon::prelude::*;
        let done = std::sync::atomic::AtomicUsize::new(0);
        let n = self.sources.len();
        let one = |(_, path): &(PhotoId, String)| {
            let bytes = (self.read)(path)?;
            // a raw without colour matrices of its own starts from the colour it has when opened alone (#965)
            let f = lightcraft_merge::load_frame_with(&bytes, self.preview, orient, &crate::camera_preview::starting_colour_tags)
                .map_err(|e| format!("{path}: {e}"));
            let k = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            progress(span.0 + (span.1 - span.0) * k as f32 / n as f32, "Reading photos");
            f
        };
        // Full-size photos are decoded one at a time (the demosaic is parallel inside): decoding several 60 MP raws at
        // once multiplied the decoders' scratch memory, and a 7-frame bracket ran out of memory.
        if self.preview.is_some() { self.sources.par_iter().map(one).collect() } else { self.sources.iter().map(one).collect() }
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
        let (image, color, orientation, metadata, baseline, samples, crop, mut info, ghost) = match &self.kind {
            MergeKind::Hdr { hdr } => {
                let frames = self.load(false, progress, (0.0, 0.3))?;
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
                let frames = self.load(true, progress, (0.0, 0.25))?;
                let p = sub(0.25, 0.9);
                let fit = fit_in_memory(pano, pixels(&frames), crate::memory::total_ram());
                let r = lightcraft_merge::stitch(frames, &fit, &p).map_err(err)?;
                let info = pano_info(&r, &self.sources, pano, &fit);
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
                let frames = self.load(false, progress, (0.0, 0.2))?;
                let b =
                    if *bracket >= 2 { *bracket } else { detect_bracket(&frames).ok_or("could not detect the bracket size from EXIF; set it")? };
                if !frames.len().is_multiple_of(b) || frames.len() / b < 2 {
                    return Err(format!("{} photos don't form brackets of {b} for at least two positions", frames.len()));
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
                let fit = fit_in_memory(pano, pixels(&merged), crate::memory::total_ram());
                let r = lightcraft_merge::stitch(merged, &fit, &p).map_err(err)?;
                let mut info = pano_info(&r, &self.sources, pano, &fit);
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

/// `asked` is what the command asked for, `fit` what ran (held to memory): when the panorama came
/// out smaller than the photos' own resolution, `limit` says why (`memory`, `size`: Lightroom's
/// 512 MP / 65,000 px, or `asked`).
fn pano_info(r: &lightcraft_merge::PanoResult, sources: &[(PhotoId, String)], asked: &PanoOptions, fit: &PanoOptions) -> Value {
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
const PANO_TILE_BYTES: f64 = 36.0;
const PANO_BLEND_BYTES: f64 = 68.0;

/// What a merge may use of the computer's memory: three quarters of it (the rest is the system's
/// and the other apps'), less the app's own memory budget (its caches).
fn merge_memory(ram: Option<usize>) -> f64 {
    // a computer whose memory we can't read (Windows) is taken to have 8 GB
    let ram = ram.unwrap_or(8 << 30) as f64;
    (ram * 0.75 - crate::memory::budget() as f64).max(0.0)
}

/// `pano` with its size held to what fits in memory beside its photos (`frame_px` pixels, 12
/// bytes each, loaded already): a panorama of five 51 MP photos may have up to 166 MP on a 16 GB
/// computer, 355 MP on 32 GB. Never below 40 megapixels, the size every panorama had before it
/// followed the photos' resolution, and never above what was asked.
pub(crate) fn fit_in_memory(pano: &PanoOptions, frame_px: f64, ram: Option<usize>) -> PanoOptions {
    let room = merge_memory(ram);
    let fits = ((room - frame_px * 12.0) / PANO_TILE_BYTES).min(room / PANO_BLEND_BYTES) / 1e6;
    PanoOptions { max_megapixels: pano.max_megapixels.min(fits.max(MIN_FITTED_MEGAPIXELS)), ..pano.clone() }
}

/// The pixels of `frames`.
fn pixels(frames: &[Frame]) -> f64 {
    frames.iter().map(|f| (f.width() * f.height()) as f64).sum()
}

/// The smallest size [`fit_in_memory`] holds a panorama to (megapixels).
pub(crate) const MIN_FITTED_MEGAPIXELS: f64 = 40.0;

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
    /// Plan a merge of `ids`. Loose ids keep the order given; members of a collapsed stack are in capture order.
    /// A photo named more than once is merged once (issue #1947): it adds nothing to the merge, and each copy would
    /// be decoded and held in memory. Naming a photo in a collapsed stack names all its members (in capture order,
    /// once per stack), since only its top photo is visible. A selection holding more than one merge group
    /// (more than one collapsed stack, or a stack plus loose photos) is refused rather than silently dropping
    /// groups; use [`Session::plan_merges`] for one job per group.
    pub fn plan_merge(&self, kind: MergeKind, finish: MergeFinish, ids: &[PhotoId], preview: bool) -> Result<MergeJob> {
        let groups = self.merge_groups(ids);
        let read = self.merge_reader();
        let mut seen = std::collections::HashSet::new();
        let mut planned: Vec<Vec<(PhotoId, String)>> = Vec::new();
        for group in groups {
            let sources = self.merge_sources(&group, &mut seen)?;
            if !sources.is_empty() {
                planned.push(sources);
            }
        }
        if planned.len() > 1 {
            return Err(EngineError::Other("selection holds more than one merge group; plan one merge per stack with plan_merges".into()));
        }
        let Some(sources) = planned.pop() else {
            return Err(EngineError::Other(format!("select at least {} photos to merge", kind.min_photos())));
        };
        if sources.len() < kind.min_photos() {
            return Err(EngineError::Other(format!("select at least {} photos to merge", kind.min_photos())));
        }
        Ok(MergeJob { kind, finish, sources, preview: preview.then_some(PREVIEW_EDGE), read })
    }

    /// Plan one merge job per collapsed stack in `ids`, plus one job for the photos that are not in a collapsed
    /// stack (kept together, as before). Each stack's members are in capture order, once per stack, and a photo
    /// named more than once is merged once. A group with fewer than the merge needs is skipped (reported by
    /// [`Session::merge_now`] as `skipped`) so one undersized loose photo does not stop a valid stack merging;
    /// if every group is undersized the selection is refused.
    pub fn plan_merges(&self, kind: MergeKind, finish: MergeFinish, ids: &[PhotoId], preview: bool) -> Result<Vec<MergeJob>> {
        Ok(self.plan_merges_inner(kind, finish, ids, preview)?.0)
    }

    fn plan_merges_inner(&self, kind: MergeKind, finish: MergeFinish, ids: &[PhotoId], preview: bool) -> Result<(Vec<MergeJob>, Vec<Vec<PhotoId>>)> {
        let groups = self.merge_groups(ids);
        let read = self.merge_reader();
        let mut jobs = Vec::new();
        let mut skipped = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for group in groups {
            let sources = self.merge_sources(&group, &mut seen)?;
            if sources.is_empty() {
                continue;
            }
            if sources.len() < kind.min_photos() {
                skipped.push(group);
                continue;
            }
            jobs.push(MergeJob { kind: kind.clone(), finish: finish.clone(), sources, preview: preview.then_some(PREVIEW_EDGE), read: read.clone() });
        }
        if jobs.is_empty() {
            return Err(EngineError::Other(format!("select at least {} photos to merge", kind.min_photos())));
        }
        Ok((jobs, skipped))
    }

    fn merge_reader(&self) -> ByteReader {
        match &self.media.file_bytes {
            Some(r) => r.clone(),
            None => Arc::new(|path: &str| std::fs::read(path).map_err(|e| format!("{path}: {e}"))),
        }
    }

    fn merge_sources(&self, group: &[PhotoId], seen: &mut std::collections::HashSet<PhotoId>) -> Result<Vec<(PhotoId, String)>> {
        let mut sources = Vec::new();
        for id in group.iter().filter(|id| seen.insert(**id)) {
            let p = self.catalog.photo(*id).ok_or(lightcraft_catalog::CatalogError::NoPhoto(*id))?;
            match &p.source {
                Source::File { path } => sources.push((*id, path.clone())),
                Source::Demo { .. } => return Err(EngineError::Other(format!("{} is a demo photo; Photo Merge needs photo files", p.file_name))),
            }
        }
        Ok(sources)
    }

    /// Groups for a merge: one per collapsed stack (members in capture order), plus one loose group in given order.
    fn merge_groups(&self, ids: &[PhotoId]) -> Vec<Vec<PhotoId>> {
        let mut groups: Vec<Vec<PhotoId>> = Vec::new();
        let mut loose: Option<usize> = None;
        let mut expanded_stacks = std::collections::HashSet::new();
        for id in ids {
            // Check whether this stack was already expanded using only its id, before cloning its members:
            // naming every member of a large stack must not clone the stack once per member.
            let stack = self.catalog.stack_of(*id).filter(|st| st.collapsed);
            match stack {
                Some(st) => {
                    if !expanded_stacks.insert(st.id) {
                        continue;
                    }
                    let members = st.photos.clone();
                    // Capture order, not stack order (top first): an HDR panorama's bracket
                    // detection expects the periodic exposure sequence from the earliest capture.
                    // File names do not reflect capture sequence (renamed files, several cameras), so
                    // ties on capture date fall back to PhotoId (catalog order), not the file name.
                    // Keys are computed once per member, not once per comparison.
                    let mut keyed: Vec<(String, PhotoId)> = members
                        .iter()
                        .map(|id| {
                            let date = self.catalog.photo(*id).map(|p| p.date().to_string()).unwrap_or_default();
                            (date, *id)
                        })
                        .collect();
                    keyed.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
                    groups.push(keyed.into_iter().map(|(_, id)| id).collect());
                }
                None => {
                    let at = match loose {
                        Some(at) => at,
                        None => {
                            groups.push(Vec::new());
                            let at = groups.len() - 1;
                            loose = Some(at);
                            at
                        }
                    };
                    groups[at].push(*id);
                }
            }
        }
        groups
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

    /// Plan, run and finish a merge synchronously (CLI, MCP, control channel). One job per
    /// collapsed stack in `ids` (see [`Session::plan_merges`]): each is run and finished in turn.
    /// The result is the first job's result; when there is more than one job it also carries
    /// `merged`, the authoritative list of every job's result in order (`merged[0]` duplicates the
    /// top-level fields). Consumers should read `merged` when present rather than counting both.
    pub fn merge_now(&mut self, kind: MergeKind, finish: MergeFinish, ids: &[PhotoId]) -> Result<Value> {
        let (jobs, skipped) = self.plan_merges_inner(kind, finish, ids, false)?;
        let mut merged = Vec::new();
        for job in &jobs {
            let out = job.run(&|_, _| true).map_err(EngineError::Other)?;
            merged.push(self.finish_merge(job, out)?);
        }
        let first_id = merged.first().and_then(|v| v.get("id")).and_then(Value::as_u64).map(PhotoId);
        let mut first = merged.first().cloned().unwrap_or(Value::Null);
        if !skipped.is_empty()
            && let Some(obj) = first.as_object_mut()
        {
            obj.insert("skipped".into(), json!(skipped.iter().map(|g| g.iter().map(|p| p.0).collect::<Vec<_>>()).collect::<Vec<_>>()));
        }
        if merged.len() > 1 {
            // finish_merge selected each result in turn; leave the selection on the returned (first) result.
            if let Some(id) = first_id {
                self.selection = crate::Selection::single(id);
            }
            if let Some(obj) = first.as_object_mut() {
                obj.insert("merged".into(), Value::Array(merged));
            }
        }
        Ok(first)
    }
}
