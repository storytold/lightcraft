use super::builtin::{BuiltinModel, builtin_marker};
use super::model::{Loader, Model, ModelSlot, RunOn, STACK};
use super::settings::EXT;
use super::{
    Arc, AtomicBool, AtomicUsize, ByteReader, Condvar, Control, DenoiserManifest, HashSet, Hasher128, Layout, Mutex, OnceLock, Ordering, Params,
    Path, PathBuf, Photo, PoisonError, RunError, Source, SystemTime, denoise_bayer, product,
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
pub(crate) fn product_key_for(fingerprint: &str, content: &str) -> String {
    let mut h = Hasher128::new();
    h.str("lcdn").u64(u64::from(product::ALGORITHM)).str(fingerprint).str(content);
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
    fn try_enter(path: &Path) -> Option<Slot> {
        let (set, _) = making();
        let mut set = set.lock().unwrap_or_else(PoisonError::into_inner);
        set.insert(path.to_path_buf()).then(|| Slot(path.to_path_buf()))
    }
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

pub(crate) fn migrate_product(
    dir: &Path,
    model: &str,
    keys: &[String],
    hash: &str,
    ticket: &crate::cache_identity::SourceTicket,
    validate: impl Fn() -> bool,
) {
    let new_key = product_key_for(model, hash);
    let target = product_path(dir, &new_key);
    for old in keys {
        let old_key = product_key_for(model, old);
        if old_key == new_key {
            continue;
        }
        let source = product_path(dir, &old_key);
        let Some(_source_slot) = Slot::try_enter(&source) else { continue };
        let Some(_target_slot) = Slot::try_enter(&target) else { continue };
        if !ticket.current() || !validate() {
            return;
        }
        let copied = product::copy_rekey(&source, &target, &old_key, &new_key, |tmp| {
            ticket
                .publish(|| {
                    if !validate() {
                        return Err(product::ProductError::Stale);
                    }
                    let _writing = PRODUCT_WRITE.lock().unwrap_or_else(PoisonError::into_inner);
                    if product::is_current(&target, &new_key) {
                        return Ok(());
                    }
                    std::fs::rename(tmp, &target).map_err(product::ProductError::from)
                })
                .unwrap_or(Err(product::ProductError::Stale))
        })
        .is_ok();
        if copied {
            break;
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// making a product

/// Everything a thread needs to make one product, detached from the session.
#[derive(Clone)]
pub(crate) struct JobSpec {
    /// The original file.
    pub path: String,
    pub read: ByteReader,
    pub content_fingerprint: Option<String>,
    pub published: Arc<Mutex<Option<(PathBuf, String)>>>,
    pub source_ticket: Option<crate::cache_identity::SourceTicket>,
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

static PRODUCT_WRITE: Mutex<()> = Mutex::new(());

/// Make the product of `spec` unless it is there: decode the photo, run the model over its mosaic, write the result.
/// `Ok(true)` when it was made now, `Ok(false)` when it already existed.
pub(crate) fn make_product(spec: &JobSpec, progress: Option<&Progress>) -> Result<bool, MakeError> {
    if spec.source_ticket.as_ref().is_some_and(|ticket| !ticket.current()) {
        return Err(MakeError::Cancelled);
    }
    let _slot = Slot::enter(&spec.product);
    if product_current(&spec.product, &spec.key) {
        return Ok(false);
    }
    let began = web_time::Instant::now();
    let cancelled = || progress.is_some_and(|p| p.cancel.load(Ordering::Relaxed));
    if cancelled() {
        return Err(MakeError::Cancelled);
    }
    let capture = crate::originals::ReadCapture::new();
    let bytes = (spec.read)(&spec.path).map_err(MakeError::Failed)?;
    let target = capture
        .fingerprint(&spec.path)
        .zip(spec.content_fingerprint.as_ref())
        .and_then(|(fp, model)| {
            let key = product_key_for(model, &fp.hash.to_string());
            spec.product.parent().map(|dir| (product_path(dir, &key), key))
        })
        .unwrap_or_else(|| (spec.product.clone(), spec.key.clone()));
    if product_current(&target.0, &target.1) {
        let publish = || {
            *spec.published.lock().unwrap_or_else(PoisonError::into_inner) = Some(target.clone());
        };
        if let Some(ticket) = &spec.source_ticket {
            ticket.publish(publish).ok_or(MakeError::Cancelled)?;
        } else {
            publish();
        }
        return Ok(false);
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
    model.prepare(&mosaic.data, mosaic.width, mosaic.height);
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
    let rgb = pool.install(|| denoise_bayer(&mosaic.data, mosaic.width, mosaic.height, layout, runner, &params, &ctl))?;
    drop(mosaic);
    if cancelled() {
        return Err(MakeError::Cancelled);
    }
    let ran = began.elapsed();
    let write = || -> Result<(), MakeError> {
        let _write = PRODUCT_WRITE.lock().unwrap_or_else(PoisonError::into_inner);
        if !product_current(&target.0, &target.1) {
            product::write(&target.0, &rgb, &target.1).map_err(|e| MakeError::Failed(format!("could not save the denoised picture: {e}")))?;
        }
        *spec.published.lock().unwrap_or_else(PoisonError::into_inner) = Some(target.clone());
        Ok(())
    };
    match &spec.source_ticket {
        Some(ticket) => ticket.publish(write).ok_or(MakeError::Cancelled)??,
        None => write()?,
    };
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

#[cfg(test)]
thread_local! { static PRODUCT_CHECKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
#[cfg(test)]
pub(crate) fn product_checks() -> usize {
    PRODUCT_CHECKS.get()
}
pub(super) fn product_current(path: &Path, key: &str) -> bool {
    #[cfg(test)]
    PRODUCT_CHECKS.set(PRODUCT_CHECKS.get().saturating_add(1));
    product::is_current(path, key)
}
