use super::model::{Loader, Model, ModelSlot, RunOn, STACK};
use super::settings::EXT;
use super::{
    Arc, AtomicBool, AtomicUsize, ByteReader, Condvar, Control, DenoiserManifest, HashSet, Hasher128, Layout, Mutex, OnceLock, Ordering, Params,
    Path, PathBuf, Photo, PoisonError, RunError, Source, SystemTime, denoise_bayer, product,
};
use lightcraft_denoise::manifest::Domain;

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

/// Which kind of model photo `p` takes: a raw developed from its sensor data takes a Bayer model, a finished picture
/// (JPEG, PNG, TIFF …) a model for finished pictures; anything else (a video, a raw shown from its embedded preview)
/// none.
pub(crate) fn domain_of(p: &Photo) -> Option<Domain> {
    if p.develops_raw() {
        return Some(Domain::BayerToRgb);
    }
    (p.kind == lightcraft_catalog::MediaKind::Image).then_some(Domain::RgbToRgb)
}

/// The photos whose files can be denoised at all: raw files developed from their sensor data and finished pictures,
/// read from disk.
pub(crate) fn eligible(p: &Photo) -> bool {
    domain_of(p).is_some() && matches!(p.source, Source::File { .. }) && !p.deleted
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
    if spec.manifest.domain == Domain::RgbToRgb {
        return make_rgb_product(spec, bytes, progress, began);
    }
    // the decode and the normalised mosaic wait their turn at the process-wide memory gate, as every decode does
    let (mosaic, layout) = {
        let _held = crate::memory::work_gate().acquire(bytes.len().saturating_mul(4));
        if lightcraft_raw::probe(&bytes).is_none() {
            return Err(MakeError::Unsupported("not a raw file".into()));
        }
        let raw = match lightcraft_raw::decode(&bytes) {
            Ok(r) => r,
            Err(lightcraft_raw::RawError::Unsupported(why)) => return Err(MakeError::Unsupported(why)),
            Err(e) => return Err(MakeError::Failed(e.to_string())),
        };
        drop(bytes);
        if raw.cpp != 1 || !raw.cfa.as_ref().is_some_and(lightcraft_raw::Cfa::is_bayer) {
            return Err(MakeError::Unsupported(
                "only Bayer raw files can be denoised (this one has another sensor layout or is already demosaiced)".into(),
            ));
        }
        let n = raw.normalized().map_err(|e| MakeError::Failed(e.to_string()))?;
        drop(raw);
        let layout =
            n.cfa.as_ref().and_then(layout_of).ok_or_else(|| MakeError::Unsupported("the colour filter layout is not a Bayer pattern".into()))?;
        (n, layout)
    };
    if cancelled() {
        return Err(MakeError::Cancelled);
    }
    let decoded = began.elapsed();
    let model = loaded(spec)?;
    // Packed input, returned RGB and the preceding group's RGB overlap while tiles run.
    let scratch =
        model.bytes_per_tile().saturating_add(128usize.saturating_mul(spec.manifest.tile as usize).saturating_mul(spec.manifest.tile as usize));
    let (_, budget) = crate::memory::work_gate().usage();
    let parallel = spec.parallel.clamp(1, 4).min((budget / scratch.max(1)).max(1));
    let _held = crate::memory::work_gate().acquire(mosaic.data.len().saturating_mul(20).saturating_add(scratch.saturating_mul(parallel)));
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
    let rgb = pool.install(|| denoise_bayer(&mosaic.data, mosaic.width, mosaic.height, layout, runner, &params, &ctl))?;
    drop(mosaic);
    if cancelled() {
        return Err(MakeError::Cancelled);
    }
    let ran = began.elapsed();
    product::write(&spec.product, &rgb, &spec.key).map_err(|e| MakeError::Failed(format!("could not save the denoised picture: {e}")))?;
    if std::env::var_os("LIGHTCRAFT_PROFILE").is_some() {
        eprintln!(
            "[denoise] read + decode {:.0} ms, model (set-up, tiles, blend) {:.0} ms, write {:.0} ms",
            decoded.as_secs_f64() * 1e3,
            (ran - decoded).as_secs_f64() * 1e3,
            (began.elapsed() - ran).as_secs_f64() * 1e3
        );
    }
    Ok(true)
}

/// A finished picture as the model for finished pictures takes it: decoded at full size, unrotated (the product is the
/// file's own pixels, as a raw's is its mosaic's), transparency flattened onto white as the plain picture's is
/// ([`lightcraft_codecs::Decoded::to_working`]), in gamma-encoded sRGB clipped to 0 to 1 (colours outside sRGB and
/// values above white are clipped: the denoised picture of a wide-gamut or HDR file is the sRGB part of it).
pub(crate) fn srgb_encoded(d: &lightcraft_codecs::Decoded) -> lightcraft_raster::Rgb32f {
    let m = d.space.to_space(&lightcraft_color::SRGB).to_f32();
    let enc = lightcraft_color::transfer::linear_to_srgb;
    let mut image = d.image.clone();
    if let Some(a) = d.alpha.as_ref().filter(|a| a.width == image.width && a.height == image.height) {
        // white is (1, 1, 1) in every space here, so it is flattened before the conversion as after it
        for (p, &av) in image.data.iter_mut().zip(&a.data) {
            let av = av.clamp(0.0, 1.0);
            *p = p.map(|c| c * av + (1.0 - av));
        }
    }
    image.map(|p| {
        let q = [
            m[0][0] * p[0] + m[0][1] * p[1] + m[0][2] * p[2],
            m[1][0] * p[0] + m[1][1] * p[1] + m[1][2] * p[2],
            m[2][0] * p[0] + m[2][1] * p[1] + m[2][2] * p[2],
        ];
        q.map(|v| enc(v.clamp(0.0, 1.0)))
    })
}

/// [`make_product`] for a finished picture: decode it whole, run the model over it, write what it gives.
fn make_rgb_product(spec: &JobSpec, bytes: Vec<u8>, progress: Option<&Progress>, began: web_time::Instant) -> Result<bool, MakeError> {
    let cancelled = || progress.is_some_and(|p| p.cancel.load(Ordering::Relaxed));
    let image = {
        let _held = crate::memory::work_gate().acquire(bytes.len().saturating_mul(16));
        if lightcraft_raw::probe(&bytes).is_some() {
            return Err(MakeError::Unsupported("a raw file takes a model for raw files".into()));
        }
        let d = lightcraft_codecs::decode(&bytes, lightcraft_codecs::DecodeOptions::default())
            .map_err(|e| MakeError::Unsupported(format!("not a picture this model can read: {e}")))?;
        drop(bytes);
        // the decode and its sRGB copy (12 bytes a pixel each), counted once their size is known; it does not wait (a
        // second wait on the gate while this job holds a permit could wait for itself)
        let _pixels = crate::memory::work_gate().acquire_urgent(d.image.data.len().saturating_mul(24));
        srgb_encoded(&d)
    };
    if cancelled() {
        return Err(MakeError::Cancelled);
    }
    let model = loaded(spec)?;
    let t = spec.manifest.tile as usize;
    let scratch = model.bytes_per_tile().saturating_add(32usize.saturating_mul(t).saturating_mul(t));
    let (_, budget) = crate::memory::work_gate().usage();
    let parallel = spec.parallel.clamp(1, 4).min((budget / scratch.max(1)).max(1));
    // the picture, the blended result and its weights wait their turn (32 bytes a pixel); the colour restore's two
    // blurred copies and their working buffers (48 more at the peak) are counted without waiting, so a big picture does
    // not hold the whole gate (and the grid's thumbnails) for its whole run
    let _held = crate::memory::work_gate().acquire(image.data.len().saturating_mul(32).saturating_add(scratch.saturating_mul(parallel)));
    let _restore = crate::memory::work_gate().acquire_urgent(image.data.len().saturating_mul(48));
    let (runner, parallel) = model.runner(spec.run_on, parallel);
    let params = Params { tile: t, overlap: spec.manifest.overlap as usize, gain: spec.manifest.gain, clip: None, parallel: parallel.max(1) };
    let report = |done: usize, total: usize| {
        if let Some(p) = progress {
            p.total.store(total, Ordering::Relaxed);
            p.done.store(done, Ordering::Relaxed);
        }
    };
    let ctl = Control { cancel: progress.map(|p| &p.cancel), progress: Some(&report) };
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(params.parallel)
        .stack_size(STACK)
        .thread_name(|i| format!("denoise-{i}"))
        .build()
        .map_err(|e| MakeError::Failed(format!("could not start the worker threads: {e}")))?;
    let rgb = pool.install(|| lightcraft_denoise::run::denoise_rgb(&image, runner, &params, &ctl))?;
    drop(image);
    if cancelled() {
        return Err(MakeError::Cancelled);
    }
    product::write(&spec.product, &rgb, &spec.key).map_err(|e| MakeError::Failed(format!("could not save the denoised picture: {e}")))?;
    if std::env::var_os("LIGHTCRAFT_PROFILE").is_some() {
        eprintln!("[denoise] finished picture {}×{}: {:.0} ms", rgb.width, rgb.height, began.elapsed().as_secs_f64() * 1e3);
    }
    Ok(true)
}
