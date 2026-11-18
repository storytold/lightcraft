use rayon::prelude::*;

use super::builtin::{BuiltinModel, builtin_marker};
use super::model::{Loader, Model, ModelSlot, RunOn, STACK};
use super::settings::EXT;
use super::{
    Arc, AtomicBool, AtomicUsize, ByteReader, Condvar, Control, DenoiserManifest, Domain, HashSet, Hasher128, Layout, LinearSpace, Mutex, OnceLock,
    Ordering, Params, Path, PathBuf, Photo, PoisonError, RunError, Source, SystemTime, denoise_bayer, denoise_linear_rgb, product,
};

/// Samples at or above this (white = 1) are clipped: the model leaves them as the camera recorded them.
const CLIP: f32 = 0.99;

/// The model in use.
pub(crate) struct Active {
    pub id: String,
    pub manifest: DenoiserManifest,
    pub onnx: PathBuf,
    pub fingerprint: String,
    pub model: ModelSlot,
}

// ---------------------------------------------------------------------------------------------------------------------
// products

/// The photos whose files can be denoised at all: raw files developed from their sensor data and read from disk.
pub(crate) fn eligible(p: &Photo) -> bool {
    p.develops_raw() && matches!(p.source, Source::File { .. }) && !p.deleted
}

/// A product's name: what it was made from and with.
pub(crate) fn product_key(fingerprint: &str, p: &Photo) -> String {
    let mut h = Hasher128::new();
    h.str("lcdn").u64(u64::from(product::ALGORITHM)).str(fingerprint).str(&crate::media::content_key(p));
    h.finish().to_string()
}

pub(crate) fn product_path(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!("{key}.{EXT}"))
}

/// The `.lcdn` files in `dir`: path, size, last change.
pub(super) fn products_in(dir: &Path) -> Vec<(PathBuf, u64, SystemTime)> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == EXT))
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            m.is_file().then(|| (e.path(), m.len(), m.modified().unwrap_or(SystemTime::UNIX_EPOCH)))
        })
        .collect()
}

/// Make sure only one thread writes a product at a time: a second one waits, then finds it made.
fn making() -> &'static (Mutex<HashSet<PathBuf>>, Condvar) {
    static MAKING: OnceLock<(Mutex<HashSet<PathBuf>>, Condvar)> = OnceLock::new();
    MAKING.get_or_init(|| (Mutex::new(HashSet::new()), Condvar::new()))
}

struct Slot(PathBuf);

impl Slot {
    fn enter(path: &Path) -> Slot {
        let (set, cv) = making();
        let mut g = set.lock().unwrap_or_else(PoisonError::into_inner);
        while g.contains(path) {
            g = cv.wait(g).unwrap_or_else(PoisonError::into_inner);
        }
        g.insert(path.to_path_buf());
        Slot(path.to_path_buf())
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let (set, cv) = making();
        set.lock().unwrap_or_else(PoisonError::into_inner).remove(&self.0);
        cv.notify_all();
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// making a product

/// Everything a thread needs to make one product, detached from the session.
#[derive(Clone)]
pub(crate) struct JobSpec {
    /// The original file.
    pub path: String,
    pub read: Option<ByteReader>,
    pub product: PathBuf,
    pub key: String,
    pub manifest: DenoiserManifest,
    pub onnx: PathBuf,
    pub model: ModelSlot,
    pub loader: Loader,
    /// Tiles run at once.
    pub parallel: usize,
    /// Where the model runs (the setting).
    pub run_on: RunOn,
}

/// Why a product was not made.
#[derive(Debug, PartialEq)]
pub(crate) enum MakeError {
    /// Denoise does not apply to this file (not a Bayer raw): nothing is wrong.
    Unsupported(String),
    Failed(String),
    Cancelled,
}

impl From<RunError> for MakeError {
    fn from(e: RunError) -> Self {
        match e {
            RunError::Cancelled => MakeError::Cancelled,
            other => MakeError::Failed(other.to_string()),
        }
    }
}

/// Progress and cancel of a running job.
#[derive(Default)]
pub(crate) struct Progress {
    pub(super) done: AtomicUsize,
    pub(super) total: AtomicUsize,
    cancel: AtomicBool,
}

impl Progress {
    /// Ask the job to stop at the next tile.
    pub(crate) fn cancel_now(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// The loaded model, loading it first when it is not (kept for the next photo).
pub(super) fn loaded(spec: &JobSpec) -> Result<Arc<dyn Model>, MakeError> {
    if spec.manifest.id == lightcraft_denoise::builtin::ID {
        return Ok(Arc::new(BuiltinModel::new(&builtin_marker(&spec.onnx))));
    }
    if let Some(m) = spec.model.lock().unwrap_or_else(PoisonError::into_inner).clone() {
        return Ok(m);
    }
    let m = (spec.loader)(&spec.onnx, &spec.manifest).map_err(|e| MakeError::Failed(format!("the denoise model could not be loaded: {e}")))?;
    *spec.model.lock().unwrap_or_else(PoisonError::into_inner) = Some(m.clone());
    Ok(m)
}

/// The cell `[(0,0), (1,0), (0,1), (1,1)]` colours of a Bayer mosaic and so its layout.
fn layout_of(cfa: &lightcraft_raw::Cfa) -> Option<Layout> {
    Layout::from_cell([cfa.color_at(0, 0), cfa.color_at(1, 0), cfa.color_at(0, 1), cfa.color_at(1, 1)])
}

struct LinearRec2020Transform {
    inv_matrix: lightcraft_color::Mat3,
    wb: [f32; 3],
}

enum DenoiseInput {
    Bayer {
        mosaic: lightcraft_raw::Normalized,
        layout: Layout,
    },
    LinearRgb {
        camera_rgb: lightcraft_raster::Rgb32f,
        color_transform: Option<LinearRec2020Transform>,
        /// `max(R, G, B)` per pixel in the camera's own domain, taken before the colour transform: which pixels the
        /// sensor clipped is not readable from the converted values the model sees.
        clip_from: Option<Vec<f32>>,
    },
}

/// Bytes the memory gate holds while a raw picture becomes a product, from the file's header, before anything is
/// allocated: the model's scratch, and the picture's own allocations at their peak.
///
/// For a Bayer model that is the mosaic and the tiles. For a linear-RGB model the demosaic's own needs (its mosaic beside
/// the 12 B/px RGB it makes) and the tiles' (the model's input and the result accumulator at 12 B/px each, its weights at
/// 4 B/px and, for a converted input, the camera-domain clip map at 4 B/px) do not overlap in time, but the RGB lives
/// through both, so the larger of the two is reserved.
fn reservation(domain: Domain, space: LinearSpace, pixels: usize, cpp: usize, scratch: usize, parallel: usize) -> usize {
    let (tile_bytes, demosaic_bytes) = match domain {
        Domain::BayerToRgb => (20, 0),
        Domain::LinearRgb => (if space.is_default() { 28 } else { 32 }, if cpp == 3 { 24 } else { 16 }),
    };
    let tiles = pixels.saturating_mul(tile_bytes).saturating_add(scratch.saturating_mul(parallel));
    if demosaic_bytes == 0 {
        return tiles;
    }
    tiles.max(pixels.saturating_mul(demosaic_bytes).saturating_add(16 * 1024 * 1024))
}

/// Make the product of `spec` unless it is there: decode the photo, run the model over its mosaic, write the result.
/// `Ok(true)` when it was made now, `Ok(false)` when it already existed.
pub(crate) fn make_product(spec: &JobSpec, progress: Option<&Progress>) -> Result<bool, MakeError> {
    let _slot = Slot::enter(&spec.product);
    if product::is_current(&spec.product, &spec.key) {
        return Ok(false);
    }
    let began = web_time::Instant::now();
    let cancelled = || progress.is_some_and(|p| p.cancel.load(Ordering::Relaxed));
    if cancelled() {
        return Err(MakeError::Cancelled);
    }
    let bytes = match &spec.read {
        Some(r) => r(&spec.path).map_err(MakeError::Failed)?,
        None => std::fs::read(&spec.path).map_err(|e| MakeError::Failed(format!("{}: {e}", spec.path)))?,
    };
    if lightcraft_raw::probe(&bytes).is_none() {
        return Err(MakeError::Unsupported("not a raw file".into()));
    }
    // headers only: how big the picture is decides what making it reserves, before it is allocated
    let info = match lightcraft_raw::probe_info(&bytes) {
        Ok(info) => info,
        Err(lightcraft_raw::RawError::Unsupported(why)) => return Err(MakeError::Unsupported(why)),
        Err(e) => return Err(MakeError::Failed(e.to_string())),
    };
    let model = loaded(spec)?;
    // Packed input, returned RGB and the preceding group's RGB overlap while tiles run.
    let scratch =
        model.bytes_per_tile().saturating_add(128usize.saturating_mul(spec.manifest.tile as usize).saturating_mul(spec.manifest.tile as usize));
    let (_, budget) = crate::memory::work_gate().usage();
    let parallel = spec.parallel.clamp(1, 4).min((budget / scratch.max(1)).max(1));
    // one reservation for the whole making, taken before the first allocation: the samples the decode leaves and the RGB
    // the demosaic makes are charged from the moment they exist, never held uncharged while the model loads or the tiles
    // wait for their turn
    let pixels = info.active_area.width.saturating_mul(info.active_area.height);
    let reserved = reservation(spec.manifest.domain, spec.manifest.space, pixels, info.cpp, scratch, parallel);
    let _held = crate::memory::work_gate().acquire(reserved.max(bytes.len().saturating_mul(4)));
    let read = began.elapsed();
    let demosaic_started = web_time::Instant::now();
    let input = {
        let raw = match lightcraft_raw::decode(&bytes) {
            Ok(r) => r,
            Err(lightcraft_raw::RawError::Unsupported(why)) => return Err(MakeError::Unsupported(why)),
            Err(e) => return Err(MakeError::Failed(e.to_string())),
        };
        drop(bytes);
        match spec.manifest.domain {
            Domain::BayerToRgb => {
                if raw.cpp != 1 || !raw.cfa.as_ref().is_some_and(lightcraft_raw::Cfa::is_bayer) {
                    return Err(MakeError::Unsupported(
                        "only Bayer raw files can be denoised with a Bayer model (this one has another sensor layout or is already demosaiced)"
                            .into(),
                    ));
                }
                let n = raw.normalized().map_err(|e| MakeError::Failed(e.to_string()))?;
                drop(raw);
                let layout = n
                    .cfa
                    .as_ref()
                    .and_then(layout_of)
                    .ok_or_else(|| MakeError::Unsupported("the colour filter layout is not a Bayer pattern".into()))?;
                DenoiseInput::Bayer { mosaic: n, layout }
            }
            Domain::LinearRgb => {
                if raw.cpp != 1 && raw.cpp != 3 {
                    return Err(MakeError::Unsupported("only raw files with 1 or 3 samples per pixel can be denoised".into()));
                }
                let color_transform_info = match spec.manifest.space {
                    LinearSpace::CameraRgb => None,
                    LinearSpace::LinRec2020 => {
                        let as_shot_xy = lightcraft_raw::color::as_shot_white_xy(&raw);
                        let (matrix, wb) = lightcraft_raw::color::camera_to_rec2020(&raw, as_shot_xy);
                        if wb.iter().any(|&w| !w.is_finite() || w <= 0.0) {
                            return Err(MakeError::Failed("invalid camera white balance multipliers".into()));
                        }
                        let inv_matrix = matrix.inverse().ok_or_else(|| MakeError::Failed("camera color matrix is not invertible".into()))?;
                        Some((matrix, inv_matrix, wb))
                    }
                };
                let n = raw.normalized().map_err(|e| MakeError::Failed(e.to_string()))?;
                drop(raw);
                let mut camera_rgb = lightcraft_raw::demosaic(&n, lightcraft_raw::Method::Ahd);
                drop(n);
                let (color_transform, clip_from) = match color_transform_info {
                    None => (None, None),
                    Some((matrix, inv_matrix, wb)) => {
                        // Clipping is a property of the sensor, so measure it before the white balance and the colour
                        // matrix move the values into Rec.2020, where "at white" no longer means the camera clipped.
                        let clip_from: Vec<f32> = camera_rgb.data.iter().map(|px| px[0].max(px[1]).max(px[2])).collect();
                        let m = matrix.to_f32();
                        camera_rgb.data.par_iter_mut().for_each(|px| {
                            let balanced = [px[0] * wb[0], px[1] * wb[1], px[2] * wb[2]];
                            let r = m[0][0] * balanced[0] + m[0][1] * balanced[1] + m[0][2] * balanced[2];
                            let g = m[1][0] * balanced[0] + m[1][1] * balanced[1] + m[1][2] * balanced[2];
                            let b = m[2][0] * balanced[0] + m[2][1] * balanced[1] + m[2][2] * balanced[2];
                            *px = [r, g, b];
                        });
                        (Some(LinearRec2020Transform { inv_matrix, wb }), Some(clip_from))
                    }
                };
                DenoiseInput::LinearRgb { camera_rgb, color_transform, clip_from }
            }
        }
    };
    let decoded = demosaic_started.elapsed();
    let tiled = web_time::Instant::now();
    // the built-in denoiser measures the whole photo's noise once, from its mosaic; a linear-RGB model has no mosaic to measure
    if let DenoiseInput::Bayer { mosaic, .. } = &input {
        model.prepare(&mosaic.data, mosaic.width, mosaic.height);
    }
    if cancelled() {
        return Err(MakeError::Cancelled);
    }
    let (runner, parallel) = model.runner(spec.run_on, parallel);
    let params = Params {
        tile: spec.manifest.tile as usize,
        overlap: spec.manifest.overlap as usize,
        gain: spec.manifest.gain,
        clip: Some(CLIP),
        parallel: parallel.max(1),
    };
    let report = |done: usize, total: usize| {
        if let Some(p) = progress {
            p.total.store(total, Ordering::Relaxed);
            p.done.store(done, Ordering::Relaxed);
        }
    };
    let ctl = Control { cancel: progress.map(|p| &p.cancel), progress: Some(&report) };
    // the tiles run on threads of their own, so a busy window's renders keep theirs
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(params.parallel)
        .stack_size(STACK)
        .thread_name(|i| format!("denoise-{i}"))
        .build()
        .map_err(|e| MakeError::Failed(format!("could not start the worker threads: {e}")))?;
    let (mut rgb, color_transform) = match input {
        DenoiseInput::Bayer { mosaic, layout } => {
            (pool.install(|| denoise_bayer(&mosaic.data, mosaic.width, mosaic.height, layout, runner, &params, &ctl))?, None)
        }
        DenoiseInput::LinearRgb { camera_rgb, color_transform, clip_from } => {
            (pool.install(|| denoise_linear_rgb(&camera_rgb, clip_from.as_deref(), runner, &params, &ctl))?, color_transform)
        }
    };
    if let Some(transform) = color_transform {
        let inv_m = transform.inv_matrix.to_f32();
        let wb = transform.wb;
        rgb.data.par_iter_mut().for_each(|px| {
            let r = inv_m[0][0] * px[0] + inv_m[0][1] * px[1] + inv_m[0][2] * px[2];
            let g = inv_m[1][0] * px[0] + inv_m[1][1] * px[1] + inv_m[1][2] * px[2];
            let b = inv_m[2][0] * px[0] + inv_m[2][1] * px[1] + inv_m[2][2] * px[2];
            // Negatives are kept, as the CameraRgb and Bayer paths keep them: the inverse matrix puts colours outside the
            // camera's gamut below zero and noise does the same in the dark, and clipping them here would move hue; only a
            // sample that is not a number is replaced. The display transform clips, not this step.
            let cr = r / wb[0];
            let cg = g / wb[1];
            let cb = b / wb[2];
            *px = [if cr.is_finite() { cr } else { 0.0 }, if cg.is_finite() { cg } else { 0.0 }, if cb.is_finite() { cb } else { 0.0 }];
        });
    }
    if cancelled() {
        return Err(MakeError::Cancelled);
    }
    let modelled = tiled.elapsed();
    let ran = began.elapsed();
    product::write(&spec.product, &rgb, &spec.key).map_err(|e| MakeError::Failed(format!("could not save the denoised picture: {e}")))?;
    if std::env::var_os("LIGHTCRAFT_PROFILE").is_some() {
        eprintln!(
            "[denoise] load + read (with the memory wait) {:.0} ms, decode + demosaic {:.0} ms, model (set-up, tiles, blend) {:.0} ms, write {:.0} ms",
            read.as_secs_f64() * 1e3,
            decoded.as_secs_f64() * 1e3,
            modelled.as_secs_f64() * 1e3,
            (began.elapsed() - ran).as_secs_f64() * 1e3
        );
    }
    Ok(true)
}

#[cfg(test)]
mod tests;
