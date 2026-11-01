use super::gpu::GPU_SETUP_MARKER;
#[cfg(feature = "denoise")]
use super::gpu::{GPU_MAX_FALLBACKS, GPU_PARALLEL, GPU_SETUP_LIMIT, card_is_faster, card_tile_ms, set_up_card};
#[cfg(feature = "denoise")]
use super::model::worth_retrying;
use super::model::{Model, RunOn};
#[cfg(feature = "denoise")]
use super::state::Pace;
#[cfg(feature = "denoise")]
use super::{Arc, AtomicUsize, Mutex, Ordering, PoisonError, RunError};
use super::{OnceLock, Path, PathBuf, TileRunner, Value, json};

/// The built-in denoiser (`lightcraft_denoise::builtin`): nothing to load, so it is what runs until a model is chosen. Its
/// filter runs on the graphics card when there is one that agrees with the CPU on a check tile and is faster, else on
/// the CPU (see [`BuiltinCard`]); the rest of it always runs on the CPU.
pub(super) struct BuiltinModel {
    runner: OnceLock<lightcraft_denoise::builtin::Builtin>,
    #[cfg(feature = "denoise")]
    card: Arc<BuiltinCard>,
    /// The runner of a photo that runs on the card.
    #[cfg(feature = "denoise")]
    hybrid: OnceLock<CardRunner>,
}

impl BuiltinModel {
    /// For a photo, with the card's state for the set-up marker `marker` (where its set-up is recorded).
    pub(super) fn new(#[cfg_attr(not(feature = "denoise"), allow(unused_variables))] marker: &Path) -> Self {
        BuiltinModel {
            runner: OnceLock::new(),
            #[cfg(feature = "denoise")]
            card: builtin_card(marker),
            #[cfg(feature = "denoise")]
            hybrid: OnceLock::new(),
        }
    }
}

impl Model for BuiltinModel {
    fn runner(&self, run_on: RunOn, threads: usize) -> (&dyn TileRunner, usize) {
        let cpu = self.runner.get_or_init(Default::default);
        #[cfg(feature = "denoise")]
        {
            let threads = threads.max(1);
            self.card.last_threads.store(threads, Ordering::Relaxed);
            if let Ok(side) = self.card.choose(run_on, threads, true)
                && let Ok(gpu) = lightcraft_gpu::bm3d::BuiltinRunner::new(side.gpu.clone(), *cpu)
            {
                let hybrid = self.hybrid.get_or_init(|| CardRunner { gpu, cpu: *cpu, card: self.card.clone() });
                return (hybrid, threads.min(GPU_PARALLEL));
            }
        }
        #[cfg(not(feature = "denoise"))]
        let _ = run_on;
        (cpu, threads)
    }

    /// Measure the whole photo's noise once, so every tile is cleaned by the photo's noise instead of its own. A photo
    /// that cannot be measured is told nothing: each tile then measures its own.
    fn prepare(&self, mosaic: &[f32], width: usize, height: usize) {
        if let Some(noise) = lightcraft_denoise::builtin::measure_noise(mosaic, width, height) {
            let _ = self.runner.set(lightcraft_denoise::builtin::Builtin::with_noise(noise));
        }
    }

    fn self_test(&self, _run_on: RunOn) -> Result<Value, String> {
        let side = 16;
        let out = lightcraft_denoise::builtin::Builtin::default().run(&vec![0.25; 4 * side * side]).map_err(|e| e.to_string())?;
        if out.len() != 12 * side * side || out.iter().any(|v| !v.is_finite()) {
            return Err("the built-in denoiser gave an unexpected result".into());
        }
        Ok(json!({"ok": true, "builtin": true}))
    }

    /// Planes, the two mosaics and the working copies of a 256-cell tile.
    fn bytes_per_tile(&self) -> usize {
        64 * 1024 * 1024
    }

    fn device(&self, run_on: RunOn) -> Value {
        #[cfg(feature = "denoise")]
        {
            self.card.device(run_on)
        }
        #[cfg(not(feature = "denoise"))]
        {
            let _ = run_on;
            json!({"kind": "cpu", "reason": "built-in"})
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// the built-in denoiser on the graphics card

/// The noise the check tile is made with and the built-in denoiser is told for it (variance = a · signal + b).
#[cfg(feature = "denoise")]
const CHECK_NOISE: (f32, f32) = (0.01, 0.001);
/// How far the card's filter may be from the CPU's on the check tile, in the noise's deviations: at most this at one place
/// (a few blocks whose match or threshold falls either side of a tie in the two devices' roundings change by up to about a
/// tenth; a broken kernel by far more), and this on average.
#[cfg(feature = "denoise")]
const BUILTIN_AGREE_WORST: f32 = 0.3;
#[cfg(feature = "denoise")]
const BUILTIN_AGREE_MEAN: f32 = 1e-3;

/// A mosaic tile (four planes of `side` cells): a ramp, edges and some texture with noise of [`CHECK_NOISE`], the same
/// every time.
#[cfg(feature = "denoise")]
fn builtin_check_tile(side: usize) -> Vec<f32> {
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut uniform = move || {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((state >> 40) as f32 + 0.5) / (1u64 << 24) as f32
    };
    let mut out = Vec::with_capacity(4 * side * side);
    for plane in 0..4 {
        for i in 0..side * side {
            let (x, y) = (i % side, i / side);
            let step = if (x / 37 + y / 29) % 2 == 0 { 0.25 } else { 0.0 };
            let signal =
                (0.1 + 0.4 * x as f32 / side as f32 + 0.1 * y as f32 / side as f32 + step + 0.05 * ((x as f32 + 4.0 * plane as f32) * 0.7).sin())
                    .clamp(0.02, 0.95);
            let (u, v) = (uniform(), uniform());
            let noise = (-2.0 * u.ln()).sqrt() * (std::f32::consts::TAU * v).cos();
            out.push((signal + noise * (CHECK_NOISE.0 * signal + CHECK_NOISE.1).sqrt()).max(0.0));
        }
    }
    out
}

/// The card set up for the built-in denoiser: its kernels, and the check tile's time on it and on the CPU.
#[cfg(feature = "denoise")]
struct BuiltinGpu {
    gpu: Arc<lightcraft_gpu::bm3d::Bm3dGpu>,
    adapter: String,
    /// The check tile on the card, the fastest of a few runs (ms).
    card_ms: f64,
    /// The check tile on the CPU, on one thread (ms).
    cpu_ms: f64,
}

/// Set the card up for the built-in denoiser: kernels built, the check tile run on both and compared, both timed.
#[cfg(feature = "denoise")]
fn make_builtin_gpu(marker: &Path) -> Result<BuiltinGpu, String> {
    use lightcraft_denoise::builtin::{Builtin, TILE};
    let tile = TILE as usize;
    let builtin = Builtin::with_noise(CHECK_NOISE);
    let params = builtin.bm3d_params().ok_or("the built-in denoiser does not use the block-matching filter")?;
    let input = Arc::new(builtin_check_tile(tile));
    let on_card = input.clone();
    let card = move || -> Result<_, String> {
        let gpu = Arc::new(lightcraft_gpu::bm3d::Bm3dGpu::new(&params, tile)?);
        let runner = lightcraft_gpu::bm3d::BuiltinRunner::new(gpu.clone(), builtin)?;
        let agreement = runner.agreement(&on_card).map_err(|e| format!("the GPU could not run the check tile: {e}"))?;
        let whole = runner.run(&on_card).map_err(|e| format!("the GPU could not run the check tile: {e}"))?;
        let card_ms = card_tile_ms(&runner, &on_card);
        Ok((gpu, agreement, whole, card_ms))
    };
    let on_cpu = || {
        let started = web_time::Instant::now();
        (builtin.run(&input), started.elapsed().as_secs_f64() * 1000.0)
    };
    let ((gpu, agreement, whole, card_ms), (want, cpu_ms)) = set_up_card(marker, GPU_SETUP_LIMIT, card, on_cpu)?;
    let want = want.map_err(|e| format!("the CPU could not check the GPU: {e}"))?;
    if whole.len() != want.len() || !whole.iter().all(|v| v.is_finite()) {
        return Err("its picture of the check tile is not the CPU's size or has numbers that are not finite: not used".into());
    }
    if !(agreement.worst <= BUILTIN_AGREE_WORST && agreement.mean <= BUILTIN_AGREE_MEAN) {
        return Err(format!(
            "its filter's answer on the check tile is not the CPU's (off by {:.2} at most and {:.1e} on average, in noise deviations): not used",
            agreement.worst, agreement.mean
        ));
    }
    let adapter = gpu.adapter();
    Ok(BuiltinGpu { gpu, adapter, card_ms, cpu_ms })
}

/// What the built-in denoiser knows about the graphics card in this process: set up for the first photo that may use it,
/// and used while it keeps working. Kept by the marker of its set-up (see [`builtin_card`]) so that every photo shares it.
#[cfg(feature = "denoise")]
pub(super) struct BuiltinCard {
    marker: PathBuf,
    gpu: OnceLock<Result<BuiltinGpu, String>>,
    /// Tiles the card got wrong (an error, numbers that are not finite) and the CPU ran.
    fallbacks: AtomicUsize,
    /// Tiles at once of the photo started last (0: none yet), to say where the work runs.
    last_threads: AtomicUsize,
}

#[cfg(feature = "denoise")]
static BUILTIN_CARDS: Mutex<Vec<(PathBuf, Arc<BuiltinCard>)>> = Mutex::new(Vec::new());

/// The card's state for the set-up marker `marker`, made the first time it is asked for.
#[cfg(feature = "denoise")]
pub(super) fn builtin_card(marker: &Path) -> Arc<BuiltinCard> {
    let mut all = BUILTIN_CARDS.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((_, c)) = all.iter().find(|(m, _)| m == marker) {
        return c.clone();
    }
    let c = Arc::new(BuiltinCard {
        marker: marker.to_path_buf(),
        gpu: OnceLock::new(),
        fallbacks: AtomicUsize::new(0),
        last_threads: AtomicUsize::new(0),
    });
    all.push((marker.to_path_buf(), c.clone()));
    c
}

/// Forget the card's state for the marker (and the marker): the card is tried afresh for the next photo.
#[cfg(feature = "denoise")]
pub(super) fn forget_builtin_card(marker: &Path) {
    BUILTIN_CARDS.lock().unwrap_or_else(PoisonError::into_inner).retain(|(m, _)| m != marker);
    let _ = std::fs::remove_file(marker);
}

/// Where the card's set-up is recorded for the built-in denoiser whose `onnx` is its model's (made-up) path in the models
/// folder: beside it, and in the temporary folder when there is no models folder.
pub(super) fn builtin_marker(onnx: &Path) -> PathBuf {
    match onnx.parent() {
        Some(p) if !p.as_os_str().is_empty() => onnx.with_file_name(GPU_SETUP_MARKER),
        _ => std::env::temp_dir().join(format!("lightcraft-builtin-{GPU_SETUP_MARKER}")),
    }
}

#[cfg(feature = "denoise")]
impl BuiltinCard {
    /// The card when it can be used now, else why not. `start` sets it up when that has not been tried yet; without it
    /// nothing is started.
    fn card(&self, start: bool) -> Result<&BuiltinGpu, String> {
        let side = if start {
            Some(self.gpu.get_or_init(|| {
                let made = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| make_builtin_gpu(&self.marker)))
                    .unwrap_or_else(|_| Err("the GPU runner gave up".into()));
                if let Err(why) = &made {
                    log::info!("denoise: the built-in denoiser runs on the CPU ({why})");
                }
                made
            }))
        } else {
            self.gpu.get()
        };
        let g = match side {
            None => return Err("the graphics card is not set up yet".into()),
            Some(Err(why)) => return Err(why.clone()),
            Some(Ok(g)) => g,
        };
        if let Some(why) = lightcraft_gpu::nn::broken_reason() {
            return Err(format!("the graphics card stopped working ({why})"));
        }
        if self.fallbacks.load(Ordering::Relaxed) >= GPU_MAX_FALLBACKS {
            return Err(format!("{} got {GPU_MAX_FALLBACKS} tiles wrong", g.adapter));
        }
        Ok(g)
    }

    /// The card, when a photo made `threads` tiles at a time under `run_on` runs on it; else why the CPU does.
    fn choose(&self, run_on: RunOn, threads: usize, start: bool) -> Result<&BuiltinGpu, String> {
        if run_on == RunOn::Cpu {
            return Err("the processor is chosen in Settings".into());
        }
        let g = self.card(start)?;
        if run_on == RunOn::Auto && !card_is_faster(g.card_ms, g.cpu_ms, threads) {
            return Err(format!("it is faster than {} on this computer", g.adapter));
        }
        Ok(g)
    }

    fn fell_back(&self, why: &str) {
        let n = self.fallbacks.fetch_add(1, Ordering::Relaxed) + 1;
        if n == 1 {
            log::warn!("denoise: a tile came back wrong from the GPU ({why}); the CPU runs it");
        } else if n == GPU_MAX_FALLBACKS {
            log::warn!("denoise: the GPU got {n} tiles wrong; the CPU does the rest");
        }
    }

    /// Where the tiles run (see [`Model::device`]).
    pub(super) fn device(&self, run_on: RunOn) -> Value {
        if run_on != RunOn::Cpu && self.gpu.get().is_none() {
            return json!({"kind": "pending"});
        }
        let threads = match self.last_threads.load(Ordering::Relaxed) {
            0 => Pace::Full.parallel(std::thread::available_parallelism().map_or(1, |n| n.get()), None),
            n => n,
        };
        let mut v = match self.choose(run_on, threads, false) {
            Ok(g) => json!({"kind": "gpu", "adapter": g.adapter}),
            // a set-up that crashed or ran out of time can be tried again (`denoise.settings {runOn}`)
            Err(why) => json!({"kind": "cpu", "retry": worth_retrying(&why), "reason": why}),
        };
        if let (Some(Ok(g)), Some(o)) = (self.gpu.get(), v.as_object_mut()) {
            o.insert("cardMs".into(), json!(g.card_ms));
            o.insert("cpuMs".into(), json!(g.cpu_ms));
            o.insert("fellBack".into(), json!(self.fallbacks.load(Ordering::Relaxed)));
        }
        v
    }
}

/// The built-in denoiser for a photo that runs on the card: the filter there, and the CPU running any tile the card gets
/// wrong, so a photo is always finished.
#[cfg(feature = "denoise")]
struct CardRunner {
    gpu: lightcraft_gpu::bm3d::BuiltinRunner,
    cpu: lightcraft_denoise::builtin::Builtin,
    card: Arc<BuiltinCard>,
}

#[cfg(feature = "denoise")]
impl TileRunner for CardRunner {
    fn run(&self, input: &[f32]) -> Result<Vec<f32>, RunError> {
        if self.card.card(false).is_ok() {
            match self.gpu.run(input) {
                Ok(out) if out.iter().all(|v| v.is_finite()) => return Ok(out),
                Ok(_) => self.card.fell_back("numbers that are not finite"),
                Err(e) => self.card.fell_back(&e.to_string()),
            }
        }
        self.cpu.run(input)
    }
}

#[cfg(all(test, feature = "denoise"))]
mod tests;
