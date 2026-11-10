//! Enhance ▸ Super Resolution and AI Sharpen: a photo rendered with its import defaults (none of its edits) to 16-bit
//! sRGB at full size, run through the installed upscaler (`Domain::RgbToRgb4x`, the `upscaleModel` slot of the
//! denoise models folder) and written as a new 16-bit TIFF next to the original (`<stem>-Enhanced-SR.tif`), imported
//! and stacked with it. Super Resolution gives twice the pixels across (or four times); AI Sharpen averages the
//! model's answer back down to the photo's own size.
//!
//! Like Photo Merge the work is a detached [`EnhanceJob`]: the commands plan, run and finish it on the spot (CLI, MCP,
//! control channel); the desktop UI runs the same job on a worker thread with a row in the activity stack.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use lightcraft_catalog::{PhotoId, Source};
use lightcraft_denoise_core::manifest::{DenoiserManifest, Domain};
use lightcraft_denoise_core::run::{Control, MAX_PIXELS, Params, Scales, upscale_rgb};
use lightcraft_develop::DevelopSettings;
use lightcraft_pipeline::{DeepImage, DeepSamples, OutputDepth, OutputSpace, Quality, RenderRequest};
use lightcraft_raster::Rgb32f;
use serde_json::{Value, json};

use crate::denoise::{Loader, RunOn, STACK, installed_models, read_settings};
use crate::export::{ExportFormat, ExportOptions};
use crate::merge::{ByteReader, ProgressFn};
use crate::{EngineError, Result, Session};

/// What the new file's name says it is (Lightroom's `-Enhanced-SR`).
pub const SUFFIX: &str = "Enhanced-SR";

/// The factors the result may have over the photo: AI Sharpen (1), Super Resolution (2, or 4).
pub const FACTORS: [usize; 3] = [1, 2, 4];

/// The largest 16-bit sample.
const U16_MAX: f32 = 65535.0;

/// Bytes a result pixel takes while it is made and written: the blended result and its weights (16), its 16-bit
/// samples (6) and the TIFF (6); plus the rendered photo's own pixels (12 a pixel of the photo).
const RESULT_BYTES_PER_PIXEL: usize = 28;
const PHOTO_BYTES_PER_PIXEL: usize = 12;

/// Bytes the decode of a file takes for each byte of it, and the render for each pixel decoded (its 16-bit copy and the
/// gamma-encoded floats), as the memory gate counts them.
const DECODE_BYTES_PER_FILE_BYTE: usize = 16;
const RENDER_BYTES_PER_PIXEL: usize = 18;

/// Bytes a tile in flight takes beyond the model's own working memory, for each pixel of the tile: its input planes
/// (12) and the 4× answer (12 × 16).
const TILE_BYTES_PER_PIXEL: usize = 204;

/// At most this many tiles run at once (as for finished-picture denoise).
const MAX_PARALLEL_TILES: usize = 4;

/// Where the progress of a job's stages starts and ends (render, upscale, write).
const UPSCALE_FROM: f32 = 0.05;
const UPSCALE_TO: f32 = 0.95;

/// The installed upscaler a job runs, loaded on the job's thread.
#[derive(Clone)]
struct Upscaler {
    loader: Loader,
    onnx: PathBuf,
    manifest: DenoiserManifest,
    run_on: RunOn,
}

/// A planned enhancement of one photo, detached from the session.
#[derive(Clone)]
pub struct EnhanceJob {
    photo: PhotoId,
    path: String,
    read: ByteReader,
    settings: DevelopSettings,
    metadata: Option<lightcraft_meta::Metadata>,
    upscaler: Upscaler,
    factor: usize,
}

/// What an enhancement produced: the TIFF and its size.
pub struct EnhanceOutput {
    pub tiff: Vec<u8>,
    pub width: usize,
    pub height: usize,
}

impl EnhanceJob {
    /// The factor of the result over the photo (1: AI Sharpen).
    pub fn factor(&self) -> usize {
        self.factor
    }

    /// Render, upscale and encode. `progress` gets the fraction done and the stage; returning `false` cancels.
    pub fn run(&self, progress: &ProgressFn) -> std::result::Result<EnhanceOutput, String> {
        if !progress(0.0, "Rendering") {
            return Err("cancelled".into());
        }
        let image = self.render()?;
        // the result and its file are counted without waiting: waiting here, holding nothing, could only let the grid's
        // thumbnails go first, and the result is refused over `MAX_PIXELS` before the render
        let result_pixels = image.data.len().saturating_mul(self.factor.saturating_mul(self.factor));
        let _result = crate::memory::work_gate().acquire_urgent(
            result_pixels.saturating_mul(RESULT_BYTES_PER_PIXEL).saturating_add(image.data.len().saturating_mul(PHOTO_BYTES_PER_PIXEL)),
        );
        let big = self.upscale(&image, progress)?;
        if !progress(UPSCALE_TO, "Writing") {
            return Err("cancelled".into());
        }
        let tiff = self.encode(&big)?;
        Ok(EnhanceOutput { tiff, width: big.width, height: big.height })
    }

    /// The photo with its import defaults, uncropped and full size, as gamma-encoded sRGB in 0 to 1.
    fn render(&self) -> std::result::Result<Rgb32f, String> {
        let bytes = (self.read)(&self.path)?;
        // the decode waits its turn at the memory gate, as finished-picture denoise does
        let _held = crate::memory::work_gate().acquire(bytes.len().saturating_mul(DECODE_BYTES_PER_FILE_BYTE));
        let decode = crate::media::RawDecode { dcp: None, outliers: self.settings.detail.nr_outliers };
        let (src, info) = crate::files::load_vec(bytes, usize::MAX, decode)?;
        let (w, h) = lightcraft_pipeline::native_output_size(src.width, src.height, &self.settings);
        check_result_size(w, h, self.factor)?;
        let _pixels = crate::memory::work_gate().acquire_urgent(src.data.len().saturating_mul(RENDER_BYTES_PER_PIXEL));
        let req = RenderRequest {
            quality: Quality::Full,
            apply_crop: false,
            space: OutputSpace::Srgb,
            depth: OutputDepth::U16,
            ..RenderRequest::fit(w.ceil() as usize, h.ceil() as usize)
        };
        let deep = lightcraft_pipeline::render(&src, &info, &self.settings, &req).deep.ok_or("the photo could not be rendered at 16 bits")?;
        let DeepSamples::U16(samples) = deep.samples else { return Err("the photo was not rendered at 16 bits".into()) };
        let data: Vec<[f32; 3]> = samples.as_chunks::<3>().0.iter().map(|p| p.map(|v| f32::from(v) / U16_MAX)).collect();
        if data.len() != deep.width.saturating_mul(deep.height) {
            return Err("the rendered photo does not hold width × height pixels".into());
        }
        Ok(Rgb32f { width: deep.width, height: deep.height, data })
    }

    fn upscale(&self, image: &Rgb32f, progress: &ProgressFn) -> std::result::Result<Rgb32f, String> {
        let u = &self.upscaler;
        let model = (u.loader)(&u.onnx, &u.manifest)?;
        let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
        let tile = u.manifest.tile as usize;
        let scratch = model.bytes_per_tile().saturating_add(TILE_BYTES_PER_PIXEL.saturating_mul(tile).saturating_mul(tile));
        let (_, budget) = crate::memory::work_gate().usage();
        let parallel = cores.clamp(1, MAX_PARALLEL_TILES).min((budget / scratch.max(1)).max(1));
        let _tiles = crate::memory::work_gate().acquire_urgent(scratch.saturating_mul(parallel));
        let (runner, parallel) = model.runner(u.run_on, parallel);
        let params = Params { tile, overlap: u.manifest.overlap as usize, gain: u.manifest.gain, clip: None, parallel: parallel.max(1) };
        let cancel = AtomicBool::new(false);
        let report = |done: usize, total: usize| {
            let f = UPSCALE_FROM + (UPSCALE_TO - UPSCALE_FROM) * done as f32 / total.max(1) as f32;
            if !progress(f, "Upscaling") {
                cancel.store(true, Ordering::Relaxed);
            }
        };
        let ctl = Control { cancel: Some(&cancel), progress: Some(&report) };
        let scales = Scales { model: Domain::RgbToRgb4x.scale(), out: self.factor };
        // the tiles run on threads of their own, so the window's renders keep theirs
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(params.parallel)
            .stack_size(STACK)
            .thread_name(|i| format!("enhance-{i}"))
            .build()
            .map_err(|e| format!("could not start the worker threads: {e}"))?;
        pool.install(|| upscale_rgb(image, runner, &params, &ctl, scales)).map_err(|e| e.to_string())
    }

    fn encode(&self, image: &Rgb32f) -> std::result::Result<Vec<u8>, String> {
        let samples = image.data.iter().flatten().map(|v| (v.clamp(0.0, 1.0) * U16_MAX).round() as u16).collect();
        let deep = DeepImage { width: image.width, height: image.height, space: OutputSpace::Srgb, samples: DeepSamples::U16(samples) };
        crate::export::encode_deep(&deep, &tiff_options(), self.metadata.as_ref())
    }
}

/// Refuse a result over [`MAX_PIXELS`] before the full-size render of a `w` × `h` photo rather than after it. The
/// render may come out a pixel smaller, so only a result too large even then is refused here; the tiling checks the
/// rest.
fn check_result_size(w: f64, h: f64, factor: usize) -> std::result::Result<(), String> {
    let least = |v: f64| if v.is_finite() { (v.floor() as usize).saturating_sub(1) } else { 0 };
    let pixels = least(w).saturating_mul(least(h)).saturating_mul(factor.saturating_mul(factor));
    if pixels > MAX_PIXELS {
        return Err(format!("the result would have {} megapixels, more than the {} a result may have", pixels / 1_000_000, MAX_PIXELS / 1_000_000));
    }
    Ok(())
}

fn tiff_options() -> ExportOptions {
    ExportOptions { format: ExportFormat::Tiff, ..Default::default() }
}

fn fail(e: impl std::fmt::Display) -> EngineError {
    EngineError::Other(e.to_string())
}

impl Session {
    /// Plan an enhancement of photo `id` at `factor` (one of [`FACTORS`]) with the upscaler in use.
    pub fn plan_enhance(&self, id: PhotoId, factor: usize) -> Result<EnhanceJob> {
        if !FACTORS.contains(&factor) {
            return Err(fail(format!("the result can be 1, 2 or 4 times the photo's size, not {factor}")));
        }
        let p = self.catalog.photo(id).ok_or(lightcraft_catalog::CatalogError::NoPhoto(id))?;
        if p.deleted {
            return Err(fail(format!("{} is in Recently Deleted: restore it first", p.file_name)));
        }
        let Source::File { path } = &p.source else {
            return Err(fail(format!("{} is a demo photo; Super Resolution needs a photo file", p.file_name)));
        };
        let dir = self.denoise.models_dir.as_deref().ok_or_else(|| fail("this build has nowhere to keep AI models (the desktop app does)"))?;
        let settings = read_settings(dir);
        let chosen = settings.upscale_model.as_deref().ok_or_else(|| {
            fail("no upscaler is installed: add one in Settings ▸ AI Denoise (`denoise.models.list` shows the ones LightCraft can fetch)")
        })?;
        let installed = installed_models(dir)
            .into_iter()
            .find(|i| i.manifest.id == chosen && i.manifest.domain == Domain::RgbToRgb4x)
            .ok_or_else(|| fail(format!("the upscaler `{chosen}` is not installed any more: choose another in Settings ▸ AI Denoise")))?;
        let upscaler =
            Upscaler { loader: self.denoise.loader.clone(), onnx: installed.onnx, manifest: installed.manifest, run_on: settings.run_on() };
        let metadata = crate::export::export_metadata(p, &self.catalog, &tiff_options());
        Ok(EnhanceJob { photo: id, path: path.clone(), read: self.byte_reader(), settings: p.import_defaults(), metadata, upscaler, factor })
    }

    /// Write a finished enhancement next to its photo, import it, stack it with the photo (on top) and select it.
    /// Returns `{id, path, bytes, width, height, factor}`.
    pub fn finish_enhance(&mut self, job: &EnhanceJob, out: EnhanceOutput) -> Result<Value> {
        // a new file under a free name (never replacing one), complete and synced before it appears
        let path = lightcraft_catalog::safe_file::write_new_unique(&mut crate::merge::output_names(&job.path, SUFFIX, "tif"), &out.tiff)
            .map_err(|e| fail(format!("could not write the enhanced TIFF next to {}: {e}", job.path)))?
            .to_string_lossy()
            .to_string();
        let report = match crate::import::import(self, std::slice::from_ref(&path), crate::import::ImportMode::Add) {
            Ok(report) => report,
            Err(e) => {
                // nothing took the file: a retry would only leave another one beside it
                let _ = std::fs::remove_file(&path);
                return Err(e);
            }
        };
        let (id, path) = match (report.imported.first(), report.duplicates.first()) {
            (Some(&id), _) => (PhotoId(id), path),
            // the upscaler gives the same answer for the same photo: a photo with these bytes is the result already, and
            // the copy just written (ours, under a new name) goes again
            (None, Some(crate::import::Duplicate { existing: Some(existing), existing_deleted: false, .. })) => {
                let existing = PhotoId(*existing);
                std::fs::remove_file(&path).map_err(|e| fail(format!("could not remove the repeated result {path}: {e}")))?;
                let existing_path = match self.catalog.photo(existing).map(|p| &p.source) {
                    Some(Source::File { path }) => path.clone(),
                    _ => path,
                };
                (existing, existing_path)
            }
            // an earlier result in Recently Deleted came back: it is the result, on the file just written if it was
            // relinked to it (its own was gone), else on its own file, and the copy goes
            (None, None) if let Some(&restored) = report.restored.first() => {
                let restored = PhotoId(restored);
                let restored_path = match self.catalog.photo(restored).map(|p| &p.source) {
                    Some(Source::File { path }) => path.clone(),
                    _ => path.clone(),
                };
                if restored_path != path {
                    std::fs::remove_file(&path).map_err(|e| fail(format!("could not remove the repeated result {path}: {e}")))?;
                }
                (restored, restored_path)
            }
            _ => {
                let _ = std::fs::remove_file(&path);
                return Err(fail(format!("the enhanced file {path} could not be imported: {:?}", report.failed)));
            }
        };
        // a result reused from an earlier run may sit in a stack of its own now: its photos don't join the original's
        let own_stack = self.catalog.stack_of(id).map(|s| s.id);
        let joins = own_stack.is_none() || own_stack == self.catalog.stack_of(job.photo).map(|s| s.id);
        if joins && let Some(op) = self.catalog.stack_with_ops(id, &[job.photo]) {
            self.commit("Stack with Original", op)?;
        }
        self.selection = crate::Selection::single(id);
        Ok(json!({"id": id.0, "path": path, "bytes": out.tiff.len(), "width": out.width, "height": out.height, "factor": job.factor}))
    }

    /// Plan, run and finish an enhancement on the spot (CLI, MCP, control channel).
    pub fn enhance_now(&mut self, id: PhotoId, factor: usize) -> Result<Value> {
        let job = self.plan_enhance(id, factor)?;
        let out = job.run(&|_, _| true).map_err(EngineError::Other)?;
        self.finish_enhance(&job, out)
    }
}

#[cfg(test)]
mod tests {
    use super::check_result_size;

    #[test]
    fn a_result_too_large_is_refused_before_the_render() {
        // 45 MP: too large at 2×, fine as AI Sharpen
        assert!(check_result_size(8256.0, 5504.0, 2).unwrap_err().contains("more than the 110"));
        assert!(check_result_size(8256.0, 5504.0, 1).is_ok());
        // 27.5 MP at 2× (110 MP) may come out a pixel smaller after the render: the tiling decides
        assert!(check_result_size(6400.0, 4297.5, 2).is_ok());
        assert!(check_result_size(f64::NAN, f64::INFINITY, 4).is_ok());
    }
}
