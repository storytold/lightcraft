//! The memory budget on a phone or tablet.
//!
//! The engine's default budget is for a desktop: a quarter of RAM, up to 1.5 GiB. iOS stops an
//! app that goes over its own limit (jetsam), which is much lower than RAM. Android has no hard
//! per-app limit for native memory, but the system kills big apps first. So the budget comes
//! from what the platform allows the process: 40% of it, between 256 MiB and 1 GiB. The rest is
//! headroom for renders in progress, textures and the UI. The caches shrink to match: 3 decoded
//! previews instead of 4, and a 512 MB thumbnail disk cache unless the user chose a size.
//!
//! Full-resolution work (exports, 1:1) is capped at what fits in half the process's limit
//! ([`lightcraft_engine::media::MediaCache::max_working_pixels`]). A full-size export over that
//! cap is rendered tile by tile instead, which needs about a third of the memory
//! ([`lightcraft_engine::media::MediaCache::max_tiled_pixels`]). Only an original too large even
//! for that (or with edits that can't be tiled) is exported smaller, and the export says so,
//! rather than the system stopping the app.
//!
//! When the system runs short (iOS memory warning, Android `onTrimMemory`), [`warn`] lowers the
//! budget by a quarter at once: the decode gate and the GPU pool follow, the pool is trimmed and
//! free allocator pages go back to the system. It then wakes the UI, and on that frame [`poll`]
//! applies the new budget to the session's caches, evicting least recently used entries.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use lightcraft_engine::Session;

const MIN: usize = 256 << 20;
const MAX: usize = 1 << 30;
/// Peak memory of a full-resolution render per pixel, measured on a 48 MP JPEG (2.87 GB).
const BYTES_PER_WORKING_PIXEL: usize = 60;
/// Peak memory of a tiled full-size export per pixel besides the tiles' own working set (the
/// decode, the source and the output), measured on a 60 MP DNG (2.0 GB) and a 48 MP JPEG
/// (1.17 GB).
const BYTES_PER_TILED_PIXEL: usize = 34;

/// The system asked for memory back.
static PRESSURE: AtomicBool = AtomicBool::new(false);

/// Woken by [`warn`] (an idle app draws no frames).
static CONTEXT: OnceLock<egui::Context> = OnceLock::new();

/// The system is short of memory: lower the budget now, and the session's caches on the next
/// frame. Any thread.
pub fn warn() {
    let before = lightcraft_engine::memory::budget();
    let b = lightcraft_engine::memory::set_budget((before / 4 * 3).max(MIN));
    lightcraft_gpu::trim_pool(0);
    lightcraft_engine::memory::release();
    log::warn!("memory: the system is short; budget {} → {} MB", before >> 20, b >> 20);
    PRESSURE.store(true, Ordering::Relaxed);
    if let Some(ctx) = CONTEXT.get() {
        ctx.request_repaint();
    }
}

/// The budget for a process allowed `limit` bytes (`None`: not known).
fn budget_for(limit: Option<usize>) -> usize {
    limit.map_or(512 << 20, |l| l / 5 * 2).clamp(MIN, MAX)
}

/// How much memory the platform lets this process use, when it says.
fn process_limit() -> Option<usize> {
    #[cfg(target_os = "ios")]
    {
        crate::ios_scene::process_memory_limit()
    }
    #[cfg(target_os = "android")]
    {
        // no per-app limit for native memory: half the RAM, as the system keeps the rest
        let s = std::fs::read_to_string("/proc/meminfo").ok()?;
        let kb: usize = s.lines().find(|l| l.starts_with("MemTotal:"))?.split_whitespace().nth(1)?.parse().ok()?;
        Some(kb * 1024 / 2)
    }
}

/// Set the mobile budget and cache sizes for `session` (after its library opened); `ctx` is
/// woken by memory warnings.
pub fn configure(session: &mut Session, ctx: &egui::Context) {
    let _ = CONTEXT.set(ctx.clone());
    lightcraft_engine::memory::set_release_hook(release);
    let limit = process_limit();
    let b = session.set_memory_budget(budget_for(limit));
    session.media.preview_capacity = 3;
    // full-resolution renders peak near 60 bytes a pixel (decode plus the float pipeline): keep
    // one within half of what the process may use, so a big original can't get the app killed
    let max_px = limit.map_or(0, |l| l / 2 / BYTES_PER_WORKING_PIXEL);
    session.media.max_working_pixels = max_px;
    let tiles = lightcraft_pipeline::tiled::TileOptions::default().working_bytes();
    let tiled_px = limit.map_or(0, |l| (l / 2).saturating_sub(tiles) / BYTES_PER_TILED_PIXEL);
    session.media.max_tiled_pixels = tiled_px;
    // tiles on the GPU need more: only exports that leave room for that use it
    let gpu_tiles = lightcraft_gpu::tiled_working_bytes(&lightcraft_pipeline::tiled::TileOptions::default());
    session.media.max_gpu_tiled_pixels = limit.map_or(0, |l| (l / 2).saturating_sub(gpu_tiles) / BYTES_PER_TILED_PIXEL);
    if session.cache_mb == 0
        && let Err(e) = session.set_cache_mb(512)
    {
        log::warn!("thumbnail cache: {e}");
    }
    log::info!(
        "memory: process limit {} MB → budget {} MB, full-resolution work up to {} MP, tiled exports up to {} MP ({} MP with GPU tiles)",
        limit.map_or(0, |l| l >> 20),
        b >> 20,
        max_px / 1_000_000,
        tiled_px / 1_000_000,
        session.media.max_gpu_tiled_pixels / 1_000_000
    );
}

/// Act on a memory warning, if one came in since the last frame.
pub fn poll(session: &mut Session) {
    if !PRESSURE.swap(false, Ordering::Relaxed) {
        return;
    }
    // the budget `warn` lowered: the session's cache share follows, evicting what's over it
    let b = session.set_memory_budget(lightcraft_engine::memory::budget());
    lightcraft_engine::memory::release();
    log::info!("memory: caches trimmed to the {} MB budget", b >> 20);
}

fn release() {
    let _ = lightcraft_sysmem::release_free_memory();
}
