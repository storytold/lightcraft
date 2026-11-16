//! Memory accounting: what each cache holds (`library.memory`, and `ui.inspect` → `memory` with the
//! frontend's own caches added).
//!
//! The numbers are the caches' own bookkeeping (pixels held), not the process's resident size: the
//! allocator keeps freed pages for reuse and the GPU driver maps device buffers, so `ps`/`time -l`
//! report more. A binary built with a heap profiler installs [`set_heap_stats`] (e.g.
//! the CLI with `--features dhat-heap`) to add live/peak heap bytes.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};

use serde::Serialize;

/// The memory budget: one number from which every cache's share is derived.
///
/// - half for the engine's caches (decoded sources and rendered previews), evicted least recently
///   used *across* them ([`crate::media::MediaCache`]);
/// - a quarter for the working memory of decodes in flight ([`work_gate`]);
/// - an eighth for the GPU renderer's pool of recycled buffers (trimmed when the app is idle).
///
/// The rest is headroom for what isn't cached (renders in progress, textures, the UI). An export
/// batch may, in addition, hold up to twice the budget (or 45 % of the RAM, if more) in photos rendering side by side
/// ([`export_gate`]): transient memory, given back when the batch ends. Default:
/// a quarter of the machine's RAM ([`total_ram`]: a container's or cgroup's memory limit when
/// smaller), at most [`AUTO_MAX`] ([`auto_budget`]; `{ENV_PREFIX}_MEMORY_MB` overrides it, and a
/// budget set by hand may go up to [`max_budget`]).
pub fn default_budget() -> usize {
    if let Some(mb) = lcc_brand::env("MEMORY_MB").and_then(|v| v.trim().parse::<usize>().ok()).filter(|m| *m >= 64) {
        return mb.min(1 << 20).saturating_mul(1 << 20);
    }
    auto_budget(total_ram())
}

/// The most the automatic budget takes. It was 1.5 GiB on every machine, so a 64 GB computer evicted
/// decoded sources and previews almost at once and re-rendered every photo it went back to.
pub const AUTO_MAX: usize = clamp_to_usize(16 << 30);

/// `bytes` as a `usize`, held to `usize::MAX` on 32-bit targets (wasm32).
const fn clamp_to_usize(bytes: u64) -> usize {
    if bytes > usize::MAX as u64 { usize::MAX } else { bytes as usize }
}

/// The budget when it is unknown how much memory the computer has.
const UNKNOWN_RAM_BUDGET: usize = 3 << 29; // 1.5 GiB

/// The automatic budget for a computer with `ram` bytes: a quarter of it, at most [`AUTO_MAX`] (8 GB → 2 GB,
/// 32 GB → 8 GB, 64 GB and more → 16 GB). No floor: under a small cgroup limit (128 MiB) a floor above the quarter
/// would let the caches alone outgrow the limit, and the kernel kills the process (upstream #3072); at least a byte,
/// since 0 means "not computed yet" to [`budget`].
pub fn auto_budget(ram: Option<usize>) -> usize {
    match ram {
        Some(ram) => (ram / 4).clamp(1, AUTO_MAX),
        None => UNKNOWN_RAM_BUDGET,
    }
}

/// The most a budget chosen by hand may be ([`max_budget_of`] this computer's memory).
pub fn max_budget() -> usize {
    max_budget_of(total_ram())
}

/// The most a budget chosen by hand may be on a computer with `ram` bytes: three quarters of it (the
/// rest for the system, the GPU driver, renders in progress and other apps), at least 512 MiB.
/// Unknown memory: 1 TiB.
pub fn max_budget_of(ram: Option<usize>) -> usize {
    match ram {
        Some(ram) => (ram / 4).saturating_mul(3).max(512 << 20),
        None => clamp_to_usize(1 << 40),
    }
}

/// Memory the process may use (bytes), when the platform tells us without native calls: physical
/// RAM, held on Linux to the memory limit of the process's cgroup (a container, a Flatpak, a
/// systemd scope with `MemoryMax`, a CI runner) when that is smaller.
pub(crate) fn total_ram() -> Option<usize> {
    // (read once: on macOS, FreeBSD and Windows it is a process to run, and an export asks for
    // every photo)
    static RAM: OnceLock<Option<usize>> = OnceLock::new();
    *RAM.get_or_init(read_total_ram)
}

fn read_total_ram() -> Option<usize> {
    #[cfg(target_os = "linux")]
    {
        lcc_sysmem::effective_total().map(|b| usize::try_from(b).unwrap_or(usize::MAX))
    }
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("/usr/sbin/sysctl").args(["-n", "hw.memsize"]).output().ok()?;
        String::from_utf8_lossy(&out.stdout).trim().parse().ok()
    }
    #[cfg(target_os = "freebsd")]
    {
        let out = std::process::Command::new("/sbin/sysctl").args(["-n", "hw.physmem"]).output().ok()?;
        String::from_utf8_lossy(&out.stdout).trim().parse().ok()
    }
    #[cfg(windows)]
    {
        // (no native call: CIM through PowerShell, once per process)
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: no console flashes up from the desktop app
        let out = std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", "(Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory"])
            .creation_flags(0x0800_0000)
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout).trim().parse().ok()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "freebsd", windows)))]
    {
        None
    }
}

static BUDGET: AtomicUsize = AtomicUsize::new(0);

/// The current budget (bytes).
pub fn budget() -> usize {
    match BUDGET.load(Ordering::Relaxed) {
        0 => {
            let b = default_budget();
            BUDGET.store(b, Ordering::Relaxed);
            apply(b);
            b
        }
        b => b,
    }
}

/// Change the budget (bytes, at least 64 MiB) for the process: the decode gate and the GPU pool
/// follow at once; sessions apply their cache share with [`crate::Session::set_memory_budget`].
pub fn set_budget(bytes: usize) -> usize {
    let b = bytes.max(64 << 20);
    BUDGET.store(b, Ordering::Relaxed);
    apply(b);
    b
}

fn apply(b: usize) {
    if let Some(g) = GATE.get() {
        g.set_limit(b / 4);
    }
    if let Some(g) = EXPORT_GATE.get() {
        g.set_limit(export_share(b));
    }
    lcc_gpu::set_pool_limit((b / 8) as u64);
    // (a 45 MP photo's 1:1 renders need more pooled than that: freeing their buffers stalled the
    // window on NVIDIA's Vulkan driver)
    lcc_gpu::set_pool_vram_share(GPU_POOL_VRAM_SHARE);
}

/// Each card's GPU buffer pool may hold up to 1/n of the card's memory while it works.
const GPU_POOL_VRAM_SHARE: u64 = 3;

/// Bytes a full-size decode of an 8-bit file holds per pixel at its peak: the decoder's RGB bytes, the linear picture
/// (`[f32; 3]`) and the working-space copy made from it, all alive at once.
const DECODE_PEAK_BYTES_PER_PX: u64 = 3 + 12 + 12;

/// The long edge to decode a `width × height` original at for the window (the loupe zoomed in, a crop, the warm
/// full-size source), when its full size would not fit the memory budget: the largest of a half, a quarter or an
/// eighth of it whose decode fits (a JPEG decodes those straight from its DCT, never at full size first), at least the
/// preview's edge. `None`: the original fits and is decoded whole. A 200 MP phone JPEG decoded whole holds about
/// 5.4 GB, which on a phone ended with the system killing the app; exports still read the original whole.
pub fn interactive_edge_cap(width: u32, height: u32) -> Option<usize> {
    edge_cap_for(budget(), width, height)
}

/// [`interactive_edge_cap`] for a budget of `budget` bytes.
fn edge_cap_for(budget: usize, width: u32, height: u32) -> Option<usize> {
    let (w, h) = (u64::from(width), u64::from(height));
    let fits = |k: u64| (w / k).saturating_mul(h / k).saturating_mul(DECODE_PEAK_BYTES_PER_PX) <= budget as u64;
    if fits(1) {
        return None;
    }
    let long = w.max(h);
    let k = [2, 4].into_iter().find(|&k| fits(k)).unwrap_or(8);
    Some(usize::try_from(long / k).unwrap_or(usize::MAX).max(crate::media::SourceLevel::Preview.max_edge()))
}

/// Share of the budget for the engine's caches.
pub fn cache_share(budget: usize) -> usize {
    budget / 2
}

/// A weighted semaphore over bytes of working memory: [`WorkGate::acquire`] waits while other
/// holders already use the limit (one holder may always proceed, whatever its size).
pub struct WorkGate {
    /// Bytes held and the limit. Held is a `u128`: callers weigh work with saturating maths, so a
    /// permit may hold `usize::MAX`, and the exact total of several such permits still fits, so
    /// releasing one in any order leaves the others counted (issue #2576).
    state: Mutex<(u128, usize)>,
    cv: Condvar,
    /// Acquires that had to wait, and their total wait (ms): what a benchmark reports as memory
    /// holding work back.
    waits: std::sync::atomic::AtomicU64,
    waited_ms: std::sync::atomic::AtomicU64,
}

/// Bytes held from a [`WorkGate`] until dropped.
pub struct Permit<'a> {
    gate: &'a WorkGate,
    bytes: usize,
}

impl WorkGate {
    pub fn new(limit: usize) -> WorkGate {
        WorkGate { state: Mutex::new((0, limit)), cv: Condvar::new(), waits: Default::default(), waited_ms: Default::default() }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, (u128, usize)> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set_limit(&self, limit: usize) {
        self.lock().1 = limit;
        self.cv.notify_all();
    }

    /// Acquires that waited so far, and their total wait (ms).
    pub fn waits(&self) -> (u64, u64) {
        (self.waits.load(Ordering::Relaxed), self.waited_ms.load(Ordering::Relaxed))
    }

    /// Bytes held now (at most `usize::MAX`) and the limit.
    pub fn usage(&self) -> (usize, usize) {
        let g = self.lock();
        (usize::try_from(g.0).unwrap_or(usize::MAX), g.1)
    }

    /// Hold `bytes`, waiting until they fit (or nothing else is held). Even `usize::MAX` bytes on
    /// a gate whose limit is `usize::MAX` make the next holder wait.
    pub fn acquire(&self, bytes: usize) -> Permit<'_> {
        let mut g = self.lock();
        let over = |g: &(u128, usize)| g.0 > 0 && g.0.saturating_add(bytes as u128) > g.1 as u128;
        let start = over(&g).then(web_time::Instant::now);
        while over(&g) {
            g = self.cv.wait(g).unwrap_or_else(|e| e.into_inner());
        }
        if let Some(t) = start {
            self.waits.fetch_add(1, Ordering::Relaxed);
            self.waited_ms.fetch_add(u64::try_from(t.elapsed().as_millis()).unwrap_or(u64::MAX), Ordering::Relaxed);
        }
        g.0 = g.0.saturating_add(bytes as u128);
        Permit { gate: self, bytes }
    }

    /// Hold `bytes` without waiting (interactive work): counted, so background work waits for it.
    pub fn acquire_urgent(&self, bytes: usize) -> Permit<'_> {
        let mut g = self.lock();
        g.0 = g.0.saturating_add(bytes as u128);
        Permit { gate: self, bytes }
    }
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        let mut g = self.gate.lock();
        g.0 = g.0.saturating_sub(self.bytes as u128);
        drop(g);
        self.gate.cv.notify_all();
    }
}

thread_local! {
    static BACKGROUND: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `f` as background work (grid thumbnails, neighbour prefetch): its decodes wait at the
/// [`work_gate`] while interactive work holds the memory.
pub fn in_background<R>(f: impl FnOnce() -> R) -> R {
    let was = BACKGROUND.with(|b| b.replace(true));
    let r = f();
    BACKGROUND.with(|b| b.set(was));
    r
}

/// Is this thread running background work ([`in_background`])?
pub fn is_background() -> bool {
    BACKGROUND.with(|b| b.get())
}

/// The process-wide gate for decodes and import probes (limit: a quarter of the budget).
pub fn work_gate() -> &'static WorkGate {
    GATE.get_or_init(|| WorkGate::new(budget() / 4))
}

static GATE: OnceLock<WorkGate> = OnceLock::new();

/// The process-wide gate for the photos of an export batch rendering side by side
/// ([`crate::export::run_batch`]; limit: twice the budget). Separate from [`work_gate`], which the
/// decodes inside each export take: one gate held across the other's acquire could deadlock.
pub fn export_gate() -> &'static WorkGate {
    EXPORT_GATE.get_or_init(|| WorkGate::new(export_share(budget())))
}

fn export_share(budget: usize) -> usize {
    export_share_of(budget, total_ram())
}

/// Twice the budget, or (when the computer's memory `ram` is known) [`EXPORT_RAM_PERCENT`] of it
/// if that is more: the budget is the caches', and an export's photos are transient (a 45 MP gain
/// map export holds ~4 GB of working memory each).
fn export_share_of(budget: usize, ram: Option<usize>) -> usize {
    let by_ram = ram.map_or(0, |r| r / 100 * EXPORT_RAM_PERCENT);
    budget.saturating_mul(2).max(by_ram)
}

/// Share of the computer's memory an export batch may hold in photos side by side.
const EXPORT_RAM_PERCENT: usize = 45;

#[cfg(test)]
mod edge_cap_tests {
    use super::edge_cap_for;

    /// A huge original is decoded for the window at a DCT step that fits the budget; one that fits is left whole.
    #[test]
    fn a_huge_original_is_decoded_smaller_for_the_window() {
        let gb = 1usize << 30;
        // a 24 MP raw on a 1.5 GiB budget, a 200 MP JPEG on a 64 GB computer's 16 GB: whole
        assert_eq!(edge_cap_for(3 * gb / 2, 6000, 4000), None);
        assert_eq!(edge_cap_for(16 * gb, 12240, 16320), None);
        // the 200 MP JPEG on a phone's 1.5 GiB: at a half or less, and never past what fits
        let cap = edge_cap_for(3 * gb / 2, 12240, 16320).unwrap();
        assert!(cap <= 16320 / 2, "{cap}");
        let (w, h) = (12240 * cap / 16320, cap);
        assert!(w * h * 27 <= 3 * gb / 2);
        // a tiny budget: an eighth, but never below the preview
        assert_eq!(edge_cap_for(1 << 20, 24480, 32640), Some(32640 / 8));
        assert_eq!(edge_cap_for(1 << 20, 12240, 16320), Some(2560));
        assert_eq!(edge_cap_for(0, u32::MAX, u32::MAX).map(|e| e > 0), Some(true), "no overflow");
    }
}

#[cfg(test)]
mod export_share_tests {
    use super::export_share_of;

    /// The export gate follows the computer's memory, not only the caches' budget.
    #[test]
    fn exports_may_use_a_share_of_the_ram() {
        let gb = 1usize << 30;
        assert_eq!(export_share_of(3 * gb / 2, Some(64 * gb)), 64 * gb / 100 * 45);
        // a large budget still counts double
        assert_eq!(export_share_of(20 * gb, Some(64 * gb)), 40 * gb);
        // memory unknown: twice the budget
        assert_eq!(export_share_of(gb, None), 2 * gb);
        assert_eq!(export_share_of(usize::MAX, Some(usize::MAX)), usize::MAX, "no overflow");
    }
}

static EXPORT_GATE: OnceLock<WorkGate> = OnceLock::new();

/// Entries and bytes held by one cache.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Usage {
    pub count: usize,
    pub bytes: usize,
}

impl Usage {
    pub fn new(count: usize, bytes: usize) -> Usage {
        Usage { count, bytes }
    }
}

/// Heap bytes as counted by an instrumented allocator.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HeapUsage {
    /// Live heap bytes now.
    pub current: u64,
    /// Highest live heap bytes so far.
    pub peak: u64,
}

/// Device buffers of the GPU renderer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GpuUsage {
    /// Every device buffer that exists.
    pub allocated: u64,
    /// Of which recycled buffers waiting in the free pool.
    pub pooled: u64,
    /// Of which buffers released since their thread's last submit.
    pub retired: u64,
}

/// What the engine's caches hold.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryReport {
    /// Decoded thumbnail-level sources (≤ 512 px, linear float).
    pub thumb_sources: Usage,
    /// Decoded preview-level sources (≤ 2560 px, linear float) of the current and prefetched photos.
    pub preview_sources: Usage,
    /// The last full-resolution original (exports, 1:1).
    pub full_source: Usage,
    /// Rendered thumbnails and view renders (8-bit) in memory.
    pub rendered: Usage,
    /// Sum of the above.
    pub engine_bytes: usize,
    pub gpu: GpuUsage,
    /// The memory budget (see [`budget`]) and the engine caches' share of it.
    pub budget: usize,
    pub cache_budget: usize,
    /// The automatic budget on this computer ([`default_budget`]) and the most one set by hand may be
    /// ([`max_budget`]).
    pub auto_budget: usize,
    pub max_budget: usize,
    /// Working memory of decodes / probes in flight and its limit ([`work_gate`]).
    pub work_bytes: usize,
    pub work_limit: usize,
    /// Live/peak heap when the binary counts allocations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heap: Option<HeapUsage>,
}

static HEAP: OnceLock<fn() -> HeapUsage> = OnceLock::new();
static RELEASE: OnceLock<fn()> = OnceLock::new();

/// Install a hook that hands memory the allocator keeps after frees back to the system (first
/// call wins). The macOS allocator keeps freed large blocks resident (reusable, but counted in
/// the resident size) until the system runs short, and glibc keeps the free pages of each thread's
/// arena; the apps install `lcc_sysmem::release_free_memory` (`malloc_zone_pressure_relief`,
/// `malloc_trim`) so the resident size follows what is actually held.
pub fn set_release_hook(f: fn()) {
    let _ = RELEASE.set(f);
}

/// Return freed memory to the system now (after large temporaries were dropped: a decode, an
/// import). No-op without a hook.
pub fn release() {
    if let Some(f) = RELEASE.get() {
        f();
    }
}

/// Install the heap counter of an instrumented binary (first call wins).
pub fn set_heap_stats(f: fn() -> HeapUsage) {
    let _ = HEAP.set(f);
}

/// Live/peak heap bytes, when the binary installed a counter.
pub fn heap_stats() -> Option<HeapUsage> {
    HEAP.get().map(|f| f())
}

/// The GPU renderer's device buffers.
pub fn gpu_usage() -> GpuUsage {
    let g = lcc_gpu::memory();
    GpuUsage { allocated: g.allocated, pooled: g.pooled, retired: g.retired }
}

impl crate::Session {
    /// What the engine's caches hold now.
    pub fn memory_report(&self) -> MemoryReport {
        let (thumb_sources, preview_sources, full_source) = self.media.usage();
        let (n, b) = self.media.rendered.mem_usage();
        let rendered = Usage::new(n, b);
        MemoryReport {
            thumb_sources,
            preview_sources,
            full_source,
            rendered,
            engine_bytes: thumb_sources.bytes + preview_sources.bytes + full_source.bytes + rendered.bytes,
            gpu: gpu_usage(),
            budget: budget(),
            cache_budget: self.media.budget(),
            auto_budget: default_budget(),
            max_budget: max_budget(),
            work_bytes: work_gate().usage().0,
            work_limit: work_gate().usage().1,
            heap: heap_stats(),
        }
    }

    /// Set the memory budget (bytes; process-wide) and apply this session's cache share.
    pub fn set_memory_budget(&mut self, bytes: usize) -> usize {
        let b = set_budget(bytes.min(max_budget()));
        self.media.set_budget(cache_share(b));
        b
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// Inside a container or a systemd scope with `MemoryMax`, the budget is sized from the limit,
    /// not the machine's RAM (#3072).
    #[test]
    fn total_ram_honours_the_cgroup_limit() {
        let ram = total_ram().unwrap();
        let info = std::fs::read_to_string("/proc/meminfo").unwrap();
        let physical = lcc_sysmem::limits::meminfo_bytes(&info, "MemTotal").unwrap();
        assert!(ram as u64 <= physical);
        let Some(limit) = lcc_sysmem::limits::cgroup_memory().limit else {
            lcc_testkit::skip!(System, "no cgroup memory limit on this process (run it under `systemd-run --user --scope -p MemoryMax=2G`)");
            return;
        };
        assert!(ram as u64 <= limit, "total_ram {ram} above the cgroup limit {limit}");
    }
}

#[cfg(test)]
mod budget_tests {
    use super::{AUTO_MAX, auto_budget, max_budget_of};

    /// The automatic budget grows with the computer's memory (it stopped at 1.5 GiB), up to a safe
    /// ceiling; a budget set by hand may go further, but never past three quarters of it.
    #[test]
    fn the_budget_scales_with_the_memory_up_to_a_safe_ceiling() {
        let gb = 1usize << 30;
        assert!(auto_budget(Some(32 * gb)) > auto_budget(Some(8 * gb)));
        assert_eq!(auto_budget(Some(512 * gb)), AUTO_MAX, "a safe ceiling");
        // no floor: under a small cgroup limit the caches must not outgrow the limit (upstream #3072)
        assert_eq!(auto_budget(Some(128 << 20)), 32 << 20, "a quarter of a 128 MiB limit");
        assert_eq!(auto_budget(Some(0)), 1, "never the 0 that means not computed");
        assert!(max_budget_of(Some(64 * gb)) < 64 * gb);
        assert!(max_budget_of(Some(64 * gb)) > auto_budget(Some(64 * gb)));
        assert!(max_budget_of(Some(usize::MAX)) < usize::MAX, "no overflow");
        assert!(max_budget_of(None) >= 64 * gb);
    }
}
