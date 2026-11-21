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

/// The file beside a model while the graphics card is set up for it (see [`set_up_card`]): it is there while a set-up
/// runs and after one that a crash cut short, and gone otherwise.
pub(crate) const GPU_SETUP_MARKER: &str = "gpu-setup.marker";

/// The file beside a model that serialises everything done to its marker: a set-up, a reset and a look at the marker hold
/// its operating-system lock, so no two of them can create, see and delete the marker in the wrong order. It stays after a
/// set-up ends (deleting a lock file would bring back the very window it exists to close) and carries nothing.
pub(crate) const GPU_SETUP_LOCK: &str = "gpu-setup.lock";

/// Why the card is not tried when a set-up marker is found.
#[cfg(feature = "denoise")]
pub(super) const GPU_CRASHED: &str = "LightCraft closed while it was setting up the graphics card for AI Denoise last time";

/// How long choosing where denoise runs waits for another process's set-up to give the lock back before saying it could
/// not clear the marker: a set-up that is running keeps it for as long as the driver takes.
const FORGET_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// Why the card is not used when its set-up ran out of time (the start of it).
#[cfg(feature = "denoise")]
pub(super) const GPU_SLOW: &str = "setting up the graphics card took longer than";

/// Why the card is not used when its set-up lock is there but cannot be opened: another process may be setting it up, and
/// nothing says which.
#[cfg(feature = "denoise")]
pub(super) const GPU_BUSY: &str = "the graphics card's set-up is in use by another LightCraft, or its lock cannot be opened";

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

/// The lock file that serialises a model's set-ups (see [`GPU_SETUP_LOCK`]).
fn lock_beside(marker: &std::path::Path) -> std::path::PathBuf {
    marker.with_file_name(GPU_SETUP_LOCK)
}

/// Remove a set-up's marker when the set-up ends, however it ends — but not when the process dies inside the driver. The
/// marker is removed while the set-up lock is still held, so a reset or a set-up of another process cannot look at the
/// path in between.
#[cfg(feature = "denoise")]
struct SetUpEnded {
    marker: PathBuf,
    /// Whether this set-up made the marker (and so removes it): one it could not make removes nothing, since the file at
    /// that path, if there is one, is another process's.
    made: bool,
    /// The lock that kept this set-up's marker its own, held until the marker is gone.
    _lock: Option<std::fs::File>,
}

#[cfg(feature = "denoise")]
impl Drop for SetUpEnded {
    fn drop(&mut self) {
        if self.made {
            let _ = std::fs::remove_file(&self.marker);
        }
        SETTING_UP.lock().unwrap_or_else(PoisonError::into_inner).retain(|m| m != &self.marker);
    }
}

/// Open the set-up lock, making it when it is not there yet: a reset and a set-up have to serialise even on a marker that
/// no set-up of this build has touched (one an older build left). A lock that is only read is enough for a shared lock,
/// which is what a reset takes; a set-up makes the file when it creates the lock it holds exclusively.
fn open_set_up_lock(lock: &std::path::Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new().read(true).open(lock).or_else(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            std::fs::OpenOptions::new().read(true).write(true).truncate(false).create(true).open(lock)
        } else {
            Err(e)
        }
    })
}

/// Clear the marker a crash left behind — but not one another process's set-up is running beside: its own end removes
/// that. The marker goes while the set-up lock is held, so a set-up cannot make one at the path in between, and removing
/// needs the folder's permission only (the marker itself is never read).
///
/// `Err` when the marker could not be removed: the caller is told rather than left saying the card may be tried again
/// while it may not.
pub(super) fn forget_marker(marker: &std::path::Path) -> Result<(), String> {
    // A shared lock is all a reset needs: it never makes a marker, and a set-up's exclusive lock still keeps it out (so no
    // set-up can be between making its marker and owning it), while a read-only handle works on every file system. A
    // set-up holding the lock is waited for a moment: one that is running keeps its own marker, and one that has just
    // found a crash's lets go at once; reporting success while a marker stays would leave the card off with nothing
    // saying why.
    let lock = match open_set_up_lock(&lock_beside(marker)) {
        Ok(file) => {
            let began = web_time::Instant::now();
            let mut held = false;
            loop {
                match file.try_lock_shared() {
                    Ok(()) => {
                        held = true;
                        break;
                    }
                    // locks are not supported here: the marker alone decides, as it always has
                    Err(std::fs::TryLockError::Error(_)) => break,
                    Err(std::fs::TryLockError::WouldBlock) if began.elapsed() < FORGET_WAIT => {
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                    Err(std::fs::TryLockError::WouldBlock) => {
                        return Err(format!(
                            "the graphics card is being set up right now, so its marker {} is in use: try again in a moment",
                            marker.display()
                        ));
                    }
                }
            }
            if held { Some(file) } else { None }
        }
        // A lock file that is there but out of reach may belong to a set-up that is running: the marker is left and the
        // caller told. One that cannot be made at all (a models folder that takes no new file) serialises nothing, and the
        // marker alone decides, as it always has.
        Err(e) if lock_beside(marker).exists() => {
            return Err(format!("the AI Denoise set-up marker {} cannot be cleared: {e}", marker.display()));
        }
        Err(_) => None,
    };
    let removed = std::fs::remove_file(marker);
    drop(lock);
    match removed {
        Ok(()) => Ok(()),
        // gone already: there is nothing to forget
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("the AI Denoise set-up marker {} cannot be removed: {e}", marker.display())),
    }
}

/// Take the set-up lock beside `marker`, waiting up to `limit` for a set-up another process is running to end. `Ok(None)`
/// when there is no lock to hold (a folder that takes no lock file, or a system without locks): the set-up then runs with
/// the marker alone, as it did before there were locks.
#[cfg(feature = "denoise")]
fn lock_set_up(marker: &Path, limit: Duration) -> Result<Option<std::fs::File>, String> {
    let began = web_time::Instant::now();
    let file = match std::fs::OpenOptions::new().read(true).write(true).truncate(false).create(true).open(lock_beside(marker)) {
        Ok(file) => file,
        // A lock that is there but cannot be opened may be another set-up's: this one does not run beside it. One that
        // cannot be made at all (the models folder may not exist yet, or may take no new file) serialises nothing: the
        // marker alone decides, and a marker that cannot be written only loses the crash guard.
        Err(_) if lock_beside(marker).exists() => return Err(GPU_BUSY.into()),
        Err(_) => return Ok(None),
    };
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(Some(file)),
            Err(std::fs::TryLockError::Error(_)) => return Ok(None),
            Err(std::fs::TryLockError::WouldBlock) => {
                // another process is setting the card up: wait for it, but never past the one deadline of this call
                if began.elapsed() >= limit {
                    return Err(GPU_STUCK.into());
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

/// Set the graphics card up with `card` on a thread of its own while `meanwhile` runs here, and give up on the card when
/// it takes longer than `limit` (a driver building kernels for minutes, or stuck). The `marker` file exists while `card`
/// runs: one found before it starts was left by a LightCraft that closed during a set-up (a driver crash takes the app
/// with it), so the card is not tried again until the user chooses where denoise runs (`denoise.settings {runOn}`). The
/// set-up lock beside it keeps this process, other LightCraft processes and the reset that choosing makes from creating,
/// seeing and deleting the marker in the wrong order.
#[cfg(feature = "denoise")]
pub(super) fn set_up_card<T: Send + 'static, R>(
    marker: &Path,
    limit: Duration,
    card: impl FnOnce() -> Result<T, String> + Send + 'static,
    meanwhile: impl FnOnce() -> R,
) -> Result<(T, R), String> {
    if let Some(parent) = marker.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // one deadline for the whole coordination: waiting for another process's set-up and starting this one must fit in the
    // same `limit`, or a handoff from one set-up to the next would give this caller a fresh one each time
    let coordination_began = web_time::Instant::now();
    if SETTING_UP.lock().unwrap_or_else(PoisonError::into_inner).iter().any(|m| m == marker) {
        return Err(GPU_STUCK.into());
    }
    // the lock is taken before the marker is looked at, and any wait for another process's set-up happens outside the
    // list of this process's set-ups, so one model's card never holds another's up
    let lock = lock_set_up(marker, limit.saturating_sub(coordination_began.elapsed()))?;
    // the other process may have given the lock up after the deadline passed: this caller has spent its one limit
    // waiting, so it does not start a set-up now
    if coordination_began.elapsed() >= limit {
        return Err(GPU_STUCK.into());
    }
    let mut running = SETTING_UP.lock().unwrap_or_else(PoisonError::into_inner);
    // a thread of this process may have taken the marker up while this call waited for the lock
    if running.iter().any(|m| m == marker) {
        return Err(GPU_STUCK.into());
    }
    // with the lock held, a set-up that exists is a crash's: another process's would hold the lock, and this process's
    // are listed above
    if marker.exists() {
        return Err(GPU_CRASHED.into());
    }
    // made and described while the lock is held, so a reset never finds a marker it could take for a crash's
    let secs = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let made = match std::fs::OpenOptions::new().write(true).create_new(true).open(marker) {
        Ok(mut file) => {
            use std::io::Write;
            let _ = file.write_all(format!("setting up the graphics card for AI Denoise (unix time {secs})\n").as_bytes());
            true
        }
        // a marker came to the path between the look above and this make (an older build's set-up, which holds no lock):
        // that is a set-up under way as far as this one can tell, so the card is left alone
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Err(GPU_CRASHED.into()),
        // a marker that cannot be written only loses the crash guard
        Err(_) => false,
    };
    running.push(marker.to_path_buf());
    let ended = SetUpEnded { marker: marker.to_path_buf(), made, _lock: lock };
    drop(running);
    // the deadline may have passed while the marker was made (a slow file system): this call has spent its one limit, so
    // it starts no worker, and the marker it just made goes with the guard
    if coordination_began.elapsed() >= limit {
        drop(ended);
        return Err(GPU_STUCK.into());
    }
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
