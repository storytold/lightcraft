#[cfg(feature = "denoise")]
use super::ane::{self, ANE_MAX_FALLBACKS, ANE_NAME, ANE_PARALLEL, AneSide, make_ane};
#[cfg(feature = "denoise")]
use super::gpu::{GPU_CRASHED, GPU_MAX_FALLBACKS, GPU_PARALLEL, GPU_SLOW, GPU_STUCK, card_is_faster, make_gpu, panic_text};
#[cfg(feature = "denoise")]
use super::settings::{installed_models, read_settings};
#[cfg(feature = "denoise")]
use super::state::Pace;
use super::{Arc, DenoiserManifest, Mutex, Path, TileRunner, Value, json};
#[cfg(feature = "denoise")]
use super::{AtomicBool, AtomicUsize, OnceLock, Ordering, PathBuf, PoisonError, RunError};
/// Stack of the threads that run the model (the runtime recurses through its graph).
pub(super) const STACK: usize = 16 << 20;

// ---------------------------------------------------------------------------------------------------------------------
// the model

/// Where the model runs (Settings ▸ AI Denoise ▸ Speed; `denoise.settings {runOn}`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RunOn {
    /// The Neural Engine (Apple silicon) or the graphics card where it is faster than the processor at the pace the
    /// work goes at (each is timed on this computer when it is set up), else the processor.
    #[default]
    Auto,
    /// The graphics card whenever it can run the model, even where the processor would be faster.
    Gpu,
    /// The Neural Engine whenever it can run the model (16-bit arithmetic: tiles differ from the processor's by a few
    /// thousandths of the largest value); where it cannot, as Automatic.
    Ane,
    /// Always the processor.
    Cpu,
}

impl RunOn {
    /// `auto`, `gpu`, `ane` or `cpu`.
    pub fn parse(name: &str) -> Option<RunOn> {
        match name {
            "auto" => Some(RunOn::Auto),
            "gpu" => Some(RunOn::Gpu),
            "ane" => Some(RunOn::Ane),
            "cpu" => Some(RunOn::Cpu),
            _ => None,
        }
    }

    /// The setting's name as stored and reported.
    pub fn name(self) -> &'static str {
        match self {
            RunOn::Auto => "auto",
            RunOn::Gpu => "gpu",
            RunOn::Ane => "ane",
            RunOn::Cpu => "cpu",
        }
    }
}

/// Whether the Neural Engine can be used at all in this build, on this computer, and is not switched off with
/// `LIGHTCRAFT_GPU=0` or `LIGHTCRAFT_GPU_BACKEND=off` (`denoise.models.list` → `neuralEngine`; the Settings choice shows
/// only then).
pub fn neural_engine_possible() -> bool {
    #[cfg(feature = "denoise")]
    {
        ane::possible() && ane::switched_off().is_none()
    }
    #[cfg(not(feature = "denoise"))]
    {
        false
    }
}

/// A loaded denoise model: what runs tiles, and a self-check.
pub(crate) trait Model: Send + Sync {
    /// What runs a photo's tiles, and how many to run at once, when `threads` may be used and the setting is `run_on`
    /// (a graphics card is kept busy by a few). The first time the card may be used it is set up and checked here.
    fn runner(&self, run_on: RunOn, threads: usize) -> (&dyn TileRunner, usize);
    /// Check it does something sensible: the test's result, or why not. Unless `run_on` is the processor the graphics
    /// card is set up and checked too, and the result says where the work will run.
    fn self_test(&self, run_on: RunOn) -> Result<Value, String>;
    /// Conservative scratch bound for an unknown runner.
    fn bytes_per_tile(&self) -> usize {
        512 * 1024 * 1024
    }
    /// Where the tiles run: `{kind: "gpu", adapter}`, `{kind: "cpu", reason?}` or `{kind: "pending"}` (the card is not
    /// set up yet), with the card's and the processor's time for the check tile (`cardMs`, `cpuMs`) once both were
    /// timed. Never starts anything.
    fn device(&self, _run_on: RunOn) -> Value {
        json!({"kind": "cpu"})
    }
}

/// Loads the model at a path as its manifest describes it.
pub(crate) type Loader = Arc<dyn Fn(&Path, &DenoiserManifest) -> Result<Arc<dyn Model>, String> + Send + Sync>;

/// A set-up that may work another time (a crash, the time limit), unlike one the card or the model cannot do.
#[cfg(feature = "denoise")]
pub(super) fn worth_retrying(why: &str) -> bool {
    why == GPU_CRASHED || why == GPU_STUCK || why.starts_with(GPU_SLOW) || ane::worth_retrying(why)
}

/// The model on our pure-Rust CPU runner, and on the graphics card when there is one that can run it: the card is set up for the
/// first photo that may use it, checked against the CPU's answer on a check tile and timed against it, and used while
/// it keeps working; a tile it gets wrong is run on the CPU, so a photo is always finished.
#[cfg(feature = "denoise")]
pub(super) struct Network {
    pub(super) cpu: lightcraft_denoise::runtime::CpuRunner,
    pub(super) path: PathBuf,
    pub(super) tile: usize,
    pub(super) gpu: OnceLock<Result<GpuSide, String>>,
    /// The Neural Engine, set up for the first photo that may use it (Apple silicon; see `ane.rs`).
    pub(super) ane: OnceLock<Result<AneSide, String>>,
    /// The model file's hash, which names the Core ML model kept for it (computed when the Neural Engine is set up).
    pub(super) sha256: OnceLock<String>,
    /// Tiles the Neural Engine got wrong (an error, numbers that are not finite) and the CPU ran.
    pub(super) ane_fallbacks: AtomicUsize,
    /// Tiles the card got wrong (an error, numbers that are not finite) and the CPU ran.
    pub(super) fallbacks: AtomicUsize,
    /// Tiles at once of the photo started last (0: none yet), to say where the work runs.
    pub(super) last_threads: AtomicUsize,
    /// Another computer's graphics card (`LIGHTCRAFT_DENOISE_URL` + `LIGHTCRAFT_DENOISE_TOKEN`; see
    /// `docs/denoise.md` → Another computer's graphics card), greeted on first use: it takes tiles while it has a free
    /// slot, on top of this computer's runner.
    pub(super) remote: Option<RemoteSide>,
    /// Whether this computer's share runs on the card (else the CPU), as chosen for the photo started last.
    pub(super) local_card: AtomicBool,
    /// Whether this computer's share runs on the Neural Engine, as chosen for the photo started last.
    pub(super) local_ane: AtomicBool,
}

#[cfg(feature = "denoise")]
pub(super) struct RemoteSide {
    runner: lightcraft_denoise::remote::Remote,
    /// The greeting, and the server's time for the check tile there and back (ms; the faster of two runs).
    greeted: OnceLock<Result<(lightcraft_denoise::remote::ServerInfo, f64), String>>,
    /// Whether the photo started last shares tiles with the server, and if not because it is slower, the two times.
    share: AtomicBool,
    slower: Mutex<Option<(f64, f64)>>,
}

#[cfg(feature = "denoise")]
impl RemoteSide {
    /// From the environment, for the model whose file has `sha256` and tiles of `tile` cells.
    fn from_env(sha256: &str, tile: usize) -> Option<RemoteSide> {
        let var = |k: &str| std::env::var(k).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let address = var("LIGHTCRAFT_DENOISE_URL")?;
        let token = var("LIGHTCRAFT_DENOISE_TOKEN").unwrap_or_default();
        Some(RemoteSide::new(lightcraft_denoise::remote::Remote::new(&address, &token, sha256, tile)))
    }

    pub(super) fn new(runner: lightcraft_denoise::remote::Remote) -> RemoteSide {
        RemoteSide { runner, greeted: OnceLock::new(), share: AtomicBool::new(false), slower: Mutex::new(None) }
    }

    /// Greet the server and time a check tile there and back: a server behind a slow link can take seconds a tile, and
    /// handing it tiles a fast local card would finish sooner only makes the photo wait for it.
    fn greet(&self) -> Result<(lightcraft_denoise::remote::ServerInfo, f64), String> {
        let info = self.runner.hello().map_err(|e| e.to_string())?;
        let input = lightcraft_denoise::runtime::check_tile(info.tile);
        let mut best = f64::INFINITY;
        for _ in 0..2 {
            let started = web_time::Instant::now();
            match self.runner.try_run(&input) {
                Some(Ok(_)) => best = best.min(started.elapsed().as_secs_f64() * 1000.0),
                Some(Err(e)) => return Err(format!("the check tile failed: {e}")),
                None => return Err("no free slot for the check tile".into()),
            }
        }
        Ok((info, best))
    }

    /// The server when it can take tiles now (greeting it the first time), else why not.
    fn usable(&self, start: bool) -> Result<&lightcraft_denoise::remote::Remote, String> {
        let greeted = if start { Some(self.greeted.get_or_init(|| self.greet())) } else { self.greeted.get() };
        match greeted {
            None => Err("not contacted yet".into()),
            Some(Err(why)) => Err(why.clone()),
            Some(Ok(_)) if !self.runner.healthy() => {
                Err(format!("it failed {} tiles ({})", self.runner.failures(), self.runner.last_error().unwrap_or_default()))
            }
            Some(Ok(_)) => Ok(&self.runner),
        }
    }

    /// The server's time for a tile, once greeted.
    fn tile_ms(&self) -> Option<f64> {
        self.greeted.get().and_then(|g| g.as_ref().ok()).map(|g| g.1)
    }

    fn describe(&self) -> Value {
        match self.greeted.get() {
            None => json!({"address": self.runner.address(), "state": "pending"}),
            Some(Err(why)) => json!({"address": self.runner.address(), "state": "unavailable", "reason": why}),
            Some(Ok((info, ms))) => {
                let slower = *self.slower.lock().unwrap_or_else(PoisonError::into_inner);
                let state = match () {
                    _ if !self.runner.healthy() => "given up",
                    _ if slower.is_some() => "slower",
                    _ if self.share.load(Ordering::Relaxed) => "used",
                    _ => "idle",
                };
                let mut v =
                    json!({"address": info.address, "device": info.device, "state": state, "remoteMs": ms, "failures": self.runner.failures()});
                if let (Some((_, local)), Some(o)) = (slower, v.as_object_mut()) {
                    o.insert("localMs".into(), json!(local));
                }
                if let (Some(e), Some(o)) = (self.runner.last_error(), v.as_object_mut()) {
                    o.insert("lastError".into(), json!(e));
                }
                v
            }
        }
    }
}

#[cfg(feature = "denoise")]
pub(super) struct GpuSide {
    pub(super) runner: Box<dyn TileRunner + Send>,
    pub(super) adapter: String,
    /// The check tile on the card, the fastest of a few runs (ms).
    pub(super) card_ms: f64,
    /// The check tile on the CPU, on one thread (ms).
    pub(super) cpu_ms: f64,
}

#[cfg(feature = "denoise")]
impl Network {
    /// The card when it can be used now, else why not. `start` sets it up when that has not been tried yet; without it
    /// nothing is started.
    fn card(&self, start: bool) -> Result<&GpuSide, String> {
        let side = if start {
            Some(self.gpu.get_or_init(|| {
                let made = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| make_gpu(&self.cpu, &self.path, self.tile)))
                    .unwrap_or_else(|_| Err("the GPU runner gave up".into()));
                if let Err(why) = &made {
                    log::info!("denoise: running on the CPU ({why})");
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

    /// The card, when a photo made `threads` tiles at a time under `run_on` runs on it; else why the CPU does. The
    /// Neural Engine's choice falls back to what Automatic would do.
    fn choose(&self, run_on: RunOn, threads: usize, start: bool) -> Result<&GpuSide, String> {
        if run_on == RunOn::Cpu {
            return Err("the processor is chosen in Settings".into());
        }
        let g = self.card(start)?;
        if run_on != RunOn::Gpu && !card_is_faster(g.card_ms, g.cpu_ms, threads) {
            return Err(format!("it is faster than {} on this computer", g.adapter));
        }
        Ok(g)
    }

    /// The Neural Engine when it can be used now, else why not. `start` sets it up when that has not been tried yet.
    fn ane(&self, start: bool) -> Result<&AneSide, String> {
        let side = if start {
            Some(self.ane.get_or_init(|| {
                let made = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let sha256 = match self.sha256.get() {
                        Some(h) => h.clone(),
                        None => {
                            let h = lightcraft_denoise::hash::sha256_file(&self.path).map_err(|e| format!("{}: {e}", self.path.display()))?;
                            self.sha256.get_or_init(|| h).clone()
                        }
                    };
                    make_ane(&self.cpu, &self.path, &sha256, self.tile)
                }))
                .unwrap_or_else(|p| {
                    log::error!("denoise: setting up the Neural Engine panicked: {}", panic_text(&*p));
                    Err("the Neural Engine runner gave up".into())
                });
                if let Err(why) = &made {
                    log::info!("denoise: not running on the Neural Engine ({why})");
                }
                made
            }))
        } else {
            self.ane.get()
        };
        let a = match side {
            None => return Err("the Neural Engine is not set up yet".into()),
            Some(Err(why)) => return Err(why.clone()),
            Some(Ok(a)) => a,
        };
        if self.ane_fallbacks.load(Ordering::Relaxed) >= ANE_MAX_FALLBACKS {
            return Err(format!("the Neural Engine got {ANE_MAX_FALLBACKS} tiles wrong"));
        }
        Ok(a)
    }

    /// The Neural Engine, when a photo made `threads` tiles at a time under `run_on` runs on it; else why not.
    /// Automatic uses it where it is faster than the processor (and than the card, when the card was timed: it is set up
    /// only when the Neural Engine is not used or the card is chosen, so on a fresh session it has not been).
    fn choose_ane(&self, run_on: RunOn, threads: usize, start: bool) -> Result<&AneSide, String> {
        if !matches!(run_on, RunOn::Auto | RunOn::Ane) {
            return Err("it is not chosen in Settings".into());
        }
        if !ane::possible() {
            return Err("this computer has no Neural Engine".into());
        }
        if let Some(why) = ane::switched_off() {
            return Err(why.into());
        }
        let a = self.ane(start)?;
        if run_on == RunOn::Auto {
            if !card_is_faster(a.ane_ms, a.cpu_ms, threads) {
                return Err(format!("it is faster than the {ANE_NAME} on this computer"));
            }
            if let Some(Ok(g)) = self.gpu.get()
                && g.card_ms < a.ane_ms
            {
                return Err(format!("{} is faster than the {ANE_NAME}", g.adapter));
            }
        }
        Ok(a)
    }

    /// Why the Neural Engine cannot be used when its set-up has finished badly or it got too many tiles wrong (not when it
    /// merely measured slower), else `None`.
    fn ane_failure(&self) -> Option<String> {
        match self.ane.get() {
            Some(Err(why)) => Some(why.clone()),
            Some(Ok(_)) if self.ane_fallbacks.load(Ordering::Relaxed) >= ANE_MAX_FALLBACKS => {
                Some(format!("the Neural Engine got {ANE_MAX_FALLBACKS} tiles wrong"))
            }
            _ => None,
        }
    }

    /// Counts a tile the Neural Engine got wrong, and says so once.
    fn ane_fell_back(&self, why: &str) {
        let n = self.ane_fallbacks.fetch_add(1, Ordering::Relaxed) + 1;
        if n == 1 {
            log::warn!("denoise: a tile came back wrong from the Neural Engine ({why}); the CPU runs it");
        } else if n == ANE_MAX_FALLBACKS {
            log::warn!("denoise: the Neural Engine got {n} tiles wrong; the CPU does the rest");
        }
    }

    fn fell_back(&self, why: &str) {
        let n = self.fallbacks.fetch_add(1, Ordering::Relaxed) + 1;
        if n == 1 {
            log::warn!("denoise: a tile came back wrong from the GPU ({why}); the CPU runs it");
        } else if n == GPU_MAX_FALLBACKS {
            log::warn!("denoise: the GPU got {n} tiles wrong; the CPU does the rest");
        }
    }
}

/// A remote server's free slot first (when there is one), then the card when this photo runs on it, with the CPU
/// running any tile either gets wrong. Handed out for photos that run on the card or have a server to share.
#[cfg(feature = "denoise")]
impl TileRunner for Network {
    /// Runs one tile where the denoise engine chose: a remote server, the Neural Engine, the graphics card or the processor, with the processor as the fallback.
    fn run(&self, input: &[f32]) -> Result<Vec<f32>, RunError> {
        if let Some(Ok(remote)) = self.remote.as_ref().filter(|r| r.share.load(Ordering::Relaxed)).map(|r| r.usable(false))
            && let Some(Ok(out)) = remote.try_run(input)
        {
            return Ok(out);
        }
        if self.local_ane.load(Ordering::Relaxed) {
            if let Ok(a) = self.ane(false) {
                match a.runner.run(input) {
                    Ok(out) if out.iter().all(|v| v.is_finite()) => return Ok(out),
                    Ok(_) => self.ane_fell_back("numbers that are not finite"),
                    Err(e) => self.ane_fell_back(&e.to_string()),
                }
            }
            return self.cpu.run(input);
        }
        if !self.local_card.load(Ordering::Relaxed) {
            return self.cpu.run(input);
        }
        if let Ok(g) = self.card(false) {
            match g.runner.run(input) {
                Ok(out) if out.iter().all(|v| v.is_finite()) => return Ok(out),
                Ok(_) => self.fell_back("numbers that are not finite"),
                Err(e) => self.fell_back(&e.to_string()),
            }
        }
        self.cpu.run(input)
    }
}

#[cfg(feature = "denoise")]
impl Model for Network {
    fn bytes_per_tile(&self) -> usize {
        self.cpu.bytes_per_tile()
    }
    /// The runner to use for `run_on` and how many tiles may run at once; the Neural Engine is tried first, then the graphics card.
    fn runner(&self, run_on: RunOn, threads: usize) -> (&dyn TileRunner, usize) {
        let threads = threads.max(1);
        self.last_threads.store(threads, Ordering::Relaxed);
        // the Neural Engine first (Apple silicon); the graphics card is only set up when it is not used
        let on_ane = self.choose_ane(run_on, threads, true).is_ok();
        let on_card = !on_ane && self.choose(run_on, threads, true).is_ok();
        let local = match (on_ane, on_card) {
            (true, _) => threads.min(ANE_PARALLEL),
            (_, true) => threads.min(GPU_PARALLEL),
            _ => threads,
        };
        let card = on_card;
        self.local_ane.store(on_ane, Ordering::Relaxed);
        self.local_card.store(card, Ordering::Relaxed);
        if let Some(r) = &self.remote {
            // this computer's time for a tile: the Neural Engine's or the card's, or the CPU's with `threads` tiles
            // sharing it (as `card_is_faster` counts them); unknown when no device was ever set up (then the server
            // is used)
            let shared = |cpu_ms: f64| cpu_ms / (1.0 + 0.5 * threads.saturating_sub(1) as f64);
            let local_ms = match (self.ane.get(), self.gpu.get()) {
                (Some(Ok(a)), _) if on_ane => a.ane_ms,
                (_, Some(Ok(g))) if card => g.card_ms,
                (Some(Ok(a)), _) => shared(a.cpu_ms),
                (_, Some(Ok(g))) => shared(g.cpu_ms),
                _ => f64::INFINITY,
            };
            let share = match (r.usable(true), r.tile_ms()) {
                (Ok(_), Some(ms)) if ms < local_ms => {
                    *r.slower.lock().unwrap_or_else(PoisonError::into_inner) = None;
                    true
                }
                (Ok(_), Some(ms)) => {
                    *r.slower.lock().unwrap_or_else(PoisonError::into_inner) = Some((ms, local_ms));
                    false
                }
                _ => false,
            };
            r.share.store(share, Ordering::Relaxed);
            if let (true, Ok(remote)) = (share, r.usable(false)) {
                // the server's slots come on top of this computer's own tiles at once
                return (self, local + remote.slots());
            }
        }
        if on_ane || card { (self, local) } else { (&self.cpu, local) }
    }

    /// Checks the processor's answers, then sets up the device `run_on` asks for and reports how it did.
    fn self_test(&self, run_on: RunOn) -> Result<Value, String> {
        let t = self.cpu.self_test();
        if !t.ok {
            let failed: Vec<&str> = t.checks.iter().filter(|(_, ok)| !*ok).map(|(c, _)| c.as_str()).collect();
            return Err(format!("the model failed its self-test: it does not {}", failed.join(", and does not ")));
        }
        let mut v = serde_json::to_value(&t).map_err(|e| e.to_string())?;
        // the devices are set up and checked here, so the test says where the work will run
        if run_on != RunOn::Cpu {
            let threads = Pace::Full.parallel(std::thread::available_parallelism().map_or(1, |n| n.get()), None);
            if self.choose_ane(run_on, threads, true).is_err() {
                let _ = self.card(true);
            }
        }
        if let Some(o) = v.as_object_mut() {
            o.insert("device".into(), self.device(run_on));
        }
        Ok(v)
    }

    /// Where tiles run now: `{kind: ane | gpu | cpu | pending, …}` with the measured times and, for the Neural Engine, why it was not used.
    fn device(&self, run_on: RunOn) -> Value {
        let threads = match self.last_threads.load(Ordering::Relaxed) {
            0 => Pace::Full.parallel(std::thread::available_parallelism().map_or(1, |n| n.get()), None),
            n => n,
        };
        let ane = self.choose_ane(run_on, threads, false);
        if let Ok(a) = &ane {
            let mut v = json!({"kind": "ane", "adapter": ANE_NAME, "aneMs": a.ane_ms, "cpuMs": a.cpu_ms, "fellBack": self.ane_fallbacks.load(Ordering::Relaxed)});
            if let (Some(r), Some(o)) = (&self.remote, v.as_object_mut()) {
                o.insert("remote".into(), r.describe());
            }
            return v;
        }
        // nothing is set up yet: the first photo decides
        if run_on != RunOn::Cpu && self.gpu.get().is_none() {
            return json!({"kind": "pending"});
        }
        // the Neural Engine was wanted (chosen, or tried first by Automatic) and could not be used because its set-up
        // failed or it got too many tiles wrong, not because it is simply slower: say so, so the user can retry
        let ane_why = if matches!(run_on, RunOn::Ane | RunOn::Auto) { self.ane_failure() } else { None };
        let mut v = match self.choose(run_on, threads, false) {
            Ok(g) => json!({"kind": "gpu", "adapter": g.adapter}),
            // a set-up that crashed or ran out of time can be tried again (`denoise.settings {runOn}`)
            Err(why) => json!({"kind": "cpu", "retry": worth_retrying(&why), "reason": why}),
        };
        if let (Some(Ok(g)), Some(o)) = (self.gpu.get(), v.as_object_mut()) {
            // measured on this computer, on the check tile: the card's best run, and one tile alone on the CPU
            o.insert("cardMs".into(), json!(g.card_ms));
            o.insert("cpuMs".into(), json!(g.cpu_ms));
            o.insert("fellBack".into(), json!(self.fallbacks.load(Ordering::Relaxed)));
        }
        if let Some(why) = &ane_why {
            let on_cpu = v["kind"] == "cpu";
            if let Some(o) = v.as_object_mut() {
                o.insert("neuralEngineReason".into(), json!(why));
                // retry only where the processor runs the photo: the card's device is in use, so no retry is offered; the
                // flag of its own says it is the Neural Engine's failure that a retry may cure (a wrong answer on the
                // check tile is not), so the button names the right device even when the card's failure is retryable too
                let retryable = on_cpu && worth_retrying(why);
                o.insert("neuralEngineRetry".into(), json!(retryable));
                if retryable {
                    o.insert("retry".into(), json!(true));
                }
            }
        }
        if let (Some(r), Some(o)) = (&self.remote, v.as_object_mut()) {
            o.insert("remote".into(), r.describe());
        }
        v
    }
}

/// The loader of this build: our pure-Rust executor with the `denoise` feature, else one that says why nothing can run.
pub(crate) fn default_loader() -> Loader {
    #[cfg(feature = "denoise")]
    {
        Arc::new(|path, manifest| {
            // the server must run this very file: identified by the manifest's hash, else the file's
            let sha256 = match &manifest.sha256 {
                Some(h) => Some(h.clone()),
                None if std::env::var_os("LIGHTCRAFT_DENOISE_URL").is_some() => lightcraft_denoise::hash::sha256_file(path).ok(),
                None => None,
            };
            let remote = sha256.and_then(|h| RemoteSide::from_env(&h, manifest.tile as usize));
            Ok(Arc::new(network(path, manifest, remote)?) as Arc<dyn Model>)
        })
    }
    #[cfg(not(feature = "denoise"))]
    {
        Arc::new(|_, _| Err("this build cannot run denoise models".to_string()))
    }
}

/// The model at `path` on this computer, sharing tiles with `remote` when there is one.
#[cfg(feature = "denoise")]
pub(super) fn network(path: &Path, manifest: &DenoiserManifest, remote: Option<RemoteSide>) -> Result<Network, String> {
    let cpu = lightcraft_denoise::runtime::CpuRunner::load(path, manifest).map_err(|e| e.to_string())?;
    // the manifest's hash names the kept Core ML model; without one the file is hashed when the Neural Engine is set up
    let sha256 = OnceLock::new();
    if let Some(h) = &manifest.sha256 {
        let _ = sha256.set(h.clone());
    }
    Ok(Network {
        remote,
        local_card: AtomicBool::new(true),
        cpu,
        path: path.to_path_buf(),
        tile: manifest.tile as usize,
        gpu: OnceLock::new(),
        ane: OnceLock::new(),
        sha256,
        ane_fallbacks: AtomicUsize::new(0),
        local_ane: AtomicBool::new(false),
        fallbacks: AtomicUsize::new(0),
        last_threads: AtomicUsize::new(0),
    })
}

/// A model ready to be served to other computers (`lightcraft-cli denoise-serve`).
#[cfg(feature = "denoise")]
pub struct Serving {
    pub model: lightcraft_denoise::remote::ModelId,
    pub tile: usize,
    /// Where its tiles run here: the graphics card's name, or why the CPU does.
    pub device: String,
    pub run: lightcraft_denoise::remote::RunTile,
}

/// The installed model `id` (default: the one selected in Settings) in the models folder `dir` (default: this user's),
/// set up to run on `run_on` as the app would: the card checked against the CPU first, the CPU when the card can't.
#[cfg(feature = "denoise")]
pub fn serving(dir: Option<&Path>, id: Option<&str>, run_on: RunOn) -> Result<Serving, String> {
    let dir = match dir {
        Some(d) => d.to_path_buf(),
        None => crate::config::default_denoise_models_dir().ok_or("no denoise models folder (set LIGHTCRAFT_DENOISE_MODELS)")?,
    };
    let want = match id {
        Some(i) => i.to_string(),
        None => read_settings(&dir).model.ok_or_else(|| {
            format!("no denoise model is selected in {} (install one in Settings ▸ AI Denoise, or name it with --model)", dir.display())
        })?,
    };
    let installed = installed_models(&dir)
        .into_iter()
        .find(|m| m.manifest.id == want)
        .ok_or_else(|| format!("no installed denoise model `{want}` in {}", dir.display()))?;
    let m = &installed.manifest;
    // the same identity a client derives (see `default_loader`)
    let sha256 = match &m.sha256 {
        Some(h) => h.clone(),
        None => lightcraft_denoise::hash::sha256_file(&installed.onnx).map_err(|e| format!("{}: {e}", installed.onnx.display()))?,
    };
    let net = Arc::new(network(&installed.onnx, m, None)?);
    let tile = m.tile as usize;
    // set the card up now (and say where the tiles run) rather than with the first client's tile
    let _ = net.runner(run_on, GPU_PARALLEL);
    let d = net.device(run_on);
    let device = match d["kind"].as_str() {
        Some("gpu") => format!("GPU: {}", d["adapter"].as_str().unwrap_or("?")),
        Some("ane") => "Neural Engine".to_string(),
        _ => format!("CPU ({})", d["reason"].as_str().unwrap_or("chosen")),
    };
    let run: lightcraft_denoise::remote::RunTile = Arc::new(move |input: &[f32]| net.runner(run_on, GPU_PARALLEL).0.run(input));
    Ok(Serving { model: lightcraft_denoise::remote::ModelId { id: m.id.clone(), version: m.version.clone(), sha256 }, tile, device, run })
}

/// A model that is loaded on first use and kept, shared by the background job and the exports.
pub(crate) type ModelSlot = Arc<Mutex<Option<Arc<dyn Model>>>>;
