#[cfg(feature = "denoise")]
use super::model::{GpuSide, STACK};
#[cfg(feature = "denoise")]
use super::{Arc, Duration, Mutex, Path, PathBuf, PoisonError, SystemTime, TileRunner};

/// Tiles run at once on a graphics card: enough to keep it fed while others are packed and blended.
#[cfg(feature = "denoise")]
pub(super) const GPU_PARALLEL: usize = 4;

/// How far the GPU's answer on the check tile may be from the CPU's (of the answer's largest value) before it is not used.
#[cfg(feature = "denoise")]
const GPU_AGREE: f32 = 1e-3;

/// Longest the card may take to set up (build its kernels, run the check tile, time it) before the processor does the
/// work instead.
#[cfg(feature = "denoise")]
const GPU_SETUP_LIMIT: Duration = Duration::from_secs(30);

/// Tiles the card may get wrong (an error, numbers that are not finite) before it stops being used for the model.
#[cfg(feature = "denoise")]
pub(super) const GPU_MAX_FALLBACKS: usize = 3;

/// The file beside a model while the graphics card is set up for it (see [`set_up_card`]).
pub(crate) const GPU_SETUP_MARKER: &str = "gpu-setup.marker";

/// Why the card is not tried when a set-up marker is found.
#[cfg(feature = "denoise")]
pub(super) const GPU_CRASHED: &str = "LightCraft closed while it was setting up the graphics card for AI Denoise last time";

/// Why the card is not used when its set-up ran out of time (the start of it).
#[cfg(feature = "denoise")]
pub(super) const GPU_SLOW: &str = "setting up the graphics card took longer than";

/// Why the card is not used while a set-up that ran out of time is still going.
#[cfg(feature = "denoise")]
pub(super) const GPU_STUCK: &str = "the graphics card is still setting up from before (it took too long)";

/// Whether the card (`card_ms` a tile) is faster than the CPU running `threads` tiles at once, from the CPU's time for
/// one tile alone (`cpu_ms`). Tiles at once share the memory and the cores' second threads, so each one more counts as
/// half a tile's speed: on a 16-core laptop one tile alone took 1.4 s and sixteen at once finished one every 0.2 s.
#[cfg(feature = "denoise")]
pub(super) fn card_is_faster(card_ms: f64, cpu_ms: f64, threads: usize) -> bool {
    let at_once = 1.0 + 0.5 * threads.saturating_sub(1) as f64;
    card_ms.is_finite() && card_ms <= cpu_ms / at_once
}

/// Card set-ups of this process that have not finished, by marker (their markers are not a crash's).
#[cfg(feature = "denoise")]
pub(super) static SETTING_UP: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Removes a set-up's marker when the set-up ends, however it ends — but not when the process dies inside the driver.
#[cfg(feature = "denoise")]
struct SetUpEnded(PathBuf);

#[cfg(feature = "denoise")]
impl Drop for SetUpEnded {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        SETTING_UP.lock().unwrap_or_else(PoisonError::into_inner).retain(|m| m != &self.0);
    }
}

/// Set the graphics card up with `card` on a thread of its own while `meanwhile` runs here, and give up on the card when
/// it takes longer than `limit` (a driver building kernels for minutes, or stuck). The `marker` file exists while `card`
/// runs: one found before it starts was left by a LightCraft that closed during a set-up (a driver crash takes the app
/// with it), so the card is not tried again until the user chooses where denoise runs (`denoise.settings {runOn}`).
#[cfg(feature = "denoise")]
pub(super) fn set_up_card<T: Send + 'static, R>(
    marker: &Path,
    limit: Duration,
    card: impl FnOnce() -> Result<T, String> + Send + 'static,
    meanwhile: impl FnOnce() -> R,
) -> Result<(T, R), String> {
    let ended = {
        let mut running = SETTING_UP.lock().unwrap_or_else(PoisonError::into_inner);
        if running.iter().any(|m| m == marker) {
            return Err(GPU_STUCK.into());
        }
        if marker.exists() {
            return Err(GPU_CRASHED.into());
        }
        let secs = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        // a marker that cannot be written only loses the crash guard
        let _ = std::fs::write(marker, format!("setting up the graphics card for AI Denoise (unix time {secs})\n"));
        running.push(marker.to_path_buf());
        SetUpEnded(marker.to_path_buf())
    };
    let started = web_time::Instant::now();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("denoise-gpu-setup".into())
        .stack_size(STACK)
        .spawn(move || {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(card)).unwrap_or_else(|_| Err("the GPU runner gave up".into()));
            drop(ended);
            let _ = tx.send(r);
        })
        .map_err(|e| format!("could not start setting up the graphics card: {e}"))?;
    let here = meanwhile();
    match rx.recv_timeout(limit.saturating_sub(started.elapsed())) {
        Ok(r) => r.map(|t| (t, here)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(format!("{GPU_SLOW} {} s, so it is not used", limit.as_secs_f64())),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err("the GPU runner gave up".into()),
    }
}

/// The card's time for one tile once it is warm: the fastest of up to three runs, stopping after about two seconds (a
/// slow card is slow enough to tell at once).
#[cfg(feature = "denoise")]
fn card_tile_ms(runner: &dyn TileRunner, input: &[f32]) -> f64 {
    if runner.run(input).is_err() {
        return f64::INFINITY;
    }
    let began = web_time::Instant::now();
    let mut best = f64::INFINITY;
    for _ in 0..3 {
        let one = web_time::Instant::now();
        if runner.run(input).is_err() {
            break;
        }
        best = best.min(one.elapsed().as_secs_f64() * 1000.0);
        if began.elapsed() > Duration::from_secs(2) {
            break;
        }
    }
    best
}

/// Set the card up for the model at `path`: kernels built, the check tile run on both and compared, both timed.
#[cfg(feature = "denoise")]
pub(super) fn make_gpu(cpu: &lightcraft_denoise::runtime::CpuRunner, path: &Path, tile: usize) -> Result<GpuSide, String> {
    let input = Arc::new(lightcraft_denoise::runtime::check_tile(tile));
    let (on_card, net) = (input.clone(), cpu.net().clone());
    let card = move || -> Result<_, String> {
        let runner = lightcraft_gpu::nn::runner(&net, tile)?;
        let got = runner.run(&on_card).map_err(|e| format!("the GPU could not run the check tile: {e}"))?;
        let card_ms = card_tile_ms(&runner, &on_card);
        Ok((runner, got, card_ms))
    };
    let on_cpu = || {
        let started = web_time::Instant::now();
        (cpu.run(&input), started.elapsed().as_secs_f64() * 1000.0)
    };
    let ((runner, got, card_ms), (want, cpu_ms)) = set_up_card(&path.with_file_name(GPU_SETUP_MARKER), GPU_SETUP_LIMIT, card, on_cpu)?;
    let want = want.map_err(|e| format!("the CPU could not check the GPU: {e}"))?;
    let scale = want.iter().fold(0f32, |m, v| m.max(v.abs())).max(1e-12);
    let worst = want.iter().zip(&got).fold(0f32, |m, (a, b)| m.max((a - b).abs())) / scale;
    if got.len() != want.len() || !got.iter().all(|v| v.is_finite()) || worst > GPU_AGREE {
        return Err(format!("its answer on the check tile is not the CPU's (off by {worst:.1e} of the largest value): not used"));
    }
    Ok(GpuSide { adapter: runner.adapter(), runner: Box::new(runner), card_ms, cpu_ms })
}
