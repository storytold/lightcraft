//! Give memory the system allocator keeps after frees back to the operating system.
//!
//! macOS's allocator keeps freed large blocks mapped as "reusable": they no longer count in the
//! process's memory footprint but stay in its resident size until the system runs short. After a
//! raw decode (hundreds of MB of temporaries) that made the resident size climb towards the sum of
//! everything ever decoded. `malloc_zone_pressure_relief` (libSystem) returns those pages now.
//!
//! glibc keeps them too: its mmap threshold rises with every large block freed (up to 32 MiB on
//! 64-bit), so decode buffers end up on the heap, and each worker thread's arena only shrinks from
//! its top. After a background preview build the resident size stayed near its peak.
//! `malloc_trim(0)` returns the free pages of every arena. Elsewhere (musl, Windows, FreeBSD)
//! nothing is called.
//!
//! One FFI call per platform (in `macos` and `glibc`), the workspace's `unsafe` exception with
//! `wincolor`. Its API is safe, and on failure or on other platforms it simply releases nothing.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

/// Return free allocator pages to the system. Returns the number of bytes released on macOS; on
/// glibc, 1 when anything was released (glibc doesn't say how much); 0 elsewhere. Safe to call at
/// any time, from any thread.
pub fn release_free_memory() -> usize {
    #[cfg(target_os = "macos")]
    {
        macos::release()
    }
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        glibc::release()
    }
    #[cfg(not(any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))))]
    {
        0
    }
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod macos {
    unsafe extern "C" {
        /// libSystem malloc: release free memory of `zone` (all zones when null) back to the
        /// system, up to `goal` bytes (0 = as much as possible). Returns the bytes released.
        fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
    }

    pub fn release() -> usize {
        // SAFETY: `malloc_zone_pressure_relief` is a documented, thread-safe libSystem function
        // (malloc/malloc.h). A null zone means "every zone" and a goal of 0 means "as much as
        // possible". It only returns already-free pages to the system: it reads and writes no
        // memory we own, takes no pointers we must keep alive, and has no failure mode beyond
        // releasing 0 bytes.
        unsafe { malloc_zone_pressure_relief(std::ptr::null_mut(), 0) }
    }
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
#[allow(unsafe_code)]
mod glibc {
    unsafe extern "C" {
        /// glibc malloc: return free memory at the top of the heap, and the free whole pages inside
        /// every arena, to the system, leaving `pad` bytes untrimmed. Returns 1 when memory was
        /// released, 0 otherwise.
        fn malloc_trim(pad: usize) -> std::ffi::c_int;
    }

    pub fn release() -> usize {
        // SAFETY: `malloc_trim` is a documented, thread-safe glibc function (malloc.h, glibc 2.0+;
        // trimming every arena since 2.8). It only returns already-free pages to the system: it
        // takes no pointers, reads and writes no memory we own, and has no failure mode beyond
        // releasing nothing.
        let released = unsafe { malloc_trim(0) };
        usize::from(released != 0)
    }
}

#[cfg(test)]
mod tests {
    use super::release_free_memory;

    #[test]
    fn releasing_with_nothing_freed_is_harmless() {
        // Twice in a row: the second call usually has nothing left to release.
        let _ = release_free_memory();
        let _ = release_free_memory();
    }

    #[test]
    fn freed_large_blocks_can_be_released_and_memory_stays_usable() {
        // Allocate and free well over the allocator's large-block threshold, then release.
        let blocks: Vec<Vec<u8>> = (0..8).map(|i| vec![i as u8; 16 << 20]).collect();
        let sum: u64 = blocks.iter().map(|b| u64::from(b[b.len() - 1])).sum();
        assert_eq!(sum, (0..8u64).sum());
        drop(blocks);
        let released = release_free_memory();
        if cfg!(not(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))) {
            assert_eq!(released, 0);
        }
        // The allocator must still work normally afterwards.
        let again = vec![7u8; 16 << 20];
        assert_eq!(again[again.len() - 1], 7);
    }

    /// What a background preview build leaves behind: decode-sized buffers allocated and freed on
    /// worker threads (their own arenas), below the raised mmap threshold. glibc keeps those pages
    /// until `malloc_trim` runs; before it was called, nothing was released here.
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    #[test]
    fn glibc_releases_what_worker_threads_freed() {
        // free one mmapped block first: glibc then raises its mmap threshold past this size, so
        // the blocks below come from the arenas, as decode buffers do after the first few
        drop(vec![1u8; 20 << 20]);
        // each worker leaves a small block above its large ones (a cached thumbnail, say), so the
        // free pages below it can't go back by trimming the heap's top: only `malloc_trim` frees them
        let workers: Vec<_> = (0..4)
            .map(|t| {
                std::thread::spawn(move || {
                    let blocks: Vec<Vec<u8>> = (0..6).map(|i| vec![(t * 6 + i) as u8; 12 << 20]).collect();
                    let kept = vec![t as u8; 4096];
                    (blocks.iter().map(|b| u64::from(b[b.len() - 1])).sum::<u64>(), kept)
                })
            })
            .collect();
        let done: Vec<(u64, Vec<u8>)> = workers.into_iter().filter_map(|h| h.join().ok()).collect();
        assert_eq!(done.iter().map(|d| d.0).sum::<u64>(), (0..24u64).sum());
        assert_eq!(release_free_memory(), 1, "freed arena pages were returned");
        drop(done);
        let again = vec![9u8; 12 << 20];
        assert_eq!(again[again.len() - 1], 9);
    }

    #[test]
    fn concurrent_calls_are_safe() {
        let handles: Vec<_> = (0..4).map(|_| std::thread::spawn(release_free_memory)).collect();
        for h in handles {
            assert!(h.join().is_ok());
        }
    }
}
