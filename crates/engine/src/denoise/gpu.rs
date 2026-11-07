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
pub(super) const GPU_SETUP_LIMIT: Duration = Duration::from_secs(30);

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
/// The file holds an operating-system lock for as long as the set-up runs, so that another LightCraft process can tell a
/// set-up under way (locked) from one that a crash left behind (not locked: the system frees the lock of a dead process).
#[cfg(feature = "denoise")]
struct SetUpEnded(PathBuf, Option<std::fs::File>);

#[cfg(feature = "denoise")]
impl Drop for SetUpEnded {
    fn drop(&mut self) {
        drop(self.1.take()); // release the lock before the file goes
        let _ = std::fs::remove_file(&self.0);
        SETTING_UP.lock().unwrap_or_else(PoisonError::into_inner).retain(|m| m != &self.0);
    }
}

/// A marker that another process holds locked is that process's set-up under way, not a crash's: wait for it to end (up to
/// `limit`). It returns `Ok` when the marker is gone. A marker that nobody holds is one a crash left behind — unless it
/// disappears within a moment, which is the process that has just ended removing it (it unlocks before it deletes: Windows
/// cannot delete a file another handle still holds open, so the two are not atomic).
#[cfg(feature = "denoise")]
fn wait_for_other_set_up(marker: &Path, limit: Duration) -> Result<(), String> {
    let began = web_time::Instant::now();
    loop {
        let Ok(file) = std::fs::OpenOptions::new().read(true).open(marker) else { return Ok(()) };
        match file.try_lock() {
            Ok(()) => {
                // nobody holds it: either it is about to go or a crash left it
                drop(file);
                if began.elapsed() >= Duration::from_secs(1) {
                    return Err(GPU_CRASHED.into());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                if began.elapsed() >= limit {
                    return Err(GPU_STUCK.into());
                }
                drop(file);
                std::thread::sleep(Duration::from_millis(100));
            }
            // locks are not supported here: the marker alone decides, as before
            Err(std::fs::TryLockError::Error(_)) => return Err(GPU_CRASHED.into()),
        }
    }
}

/// Set the graphics card up with `card` on a thread of its own while `meanwhile` runs here, and give up on the card when
/// it takes longer than `limit` (a driver building kernels for minutes, or stuck). The `marker` file exists while `card`
/// runs: one found before it starts is another process's set-up, waited for (see `wait_for_other_set_up`), or one left by
/// a LightCraft that closed during a set-up (a driver crash takes the app with it), so the card is not tried again until
/// the user chooses where denoise runs (`denoise.settings {runOn}`).
#[cfg(feature = "denoise")]
pub(super) fn set_up_card<T: Send + 'static, R>(
    marker: &Path,
    limit: Duration,
    card: impl FnOnce() -> Result<T, String> + Send + 'static,
    meanwhile: impl FnOnce() -> R,
) -> Result<(T, R), String> {
    // the models folder may not exist yet; a marker that cannot be written only loses the crash guard
    if let Some(parent) = marker.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let ended = loop {
        {
            let mut running = SETTING_UP.lock().unwrap_or_else(PoisonError::into_inner);
            if running.iter().any(|m| m == marker) {
                return Err(GPU_STUCK.into());
            }
            // the marker is made new and locked at once; one that exists is another process's (see wait_for_other_set_up)
            match std::fs::OpenOptions::new().write(true).create_new(true).open(marker) {
                Ok(mut file) => {
                    use std::io::Write;
                    let _ = file.lock();
                    let secs = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs());
                    let _ = file.write_all(format!("setting up the graphics card for AI Denoise (unix time {secs})\n").as_bytes());
                    running.push(marker.to_path_buf());
                    break SetUpEnded(marker.to_path_buf(), Some(file));
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => {
                    // a marker that cannot be written only loses the crash guard
                    running.push(marker.to_path_buf());
                    break SetUpEnded(marker.to_path_buf(), None);
                }
            }
        }
        wait_for_other_set_up(marker, limit)?;
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
pub(super) fn card_tile_ms(runner: &dyn TileRunner, input: &[f32]) -> f64 {
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
    let input = if cpu.net().in_channels == 3 {
        Arc::new(lightcraft_denoise::runtime::check_tile_linear_rgb(tile))
    } else {
        Arc::new(lightcraft_denoise::runtime::check_tile(tile))
    };
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
