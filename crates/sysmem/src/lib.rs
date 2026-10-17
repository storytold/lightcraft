//! Give memory the system allocator keeps after frees back to the operating system.
//!
//! macOS's allocator keeps freed large blocks mapped as "reusable": they no longer count in the
//! process's memory footprint but stay in its resident size until the system runs short. After a
//! raw decode (hundreds of MB of temporaries) that made the resident size climb towards the sum of
//! everything ever decoded. `malloc_zone_pressure_relief` (libSystem) returns those pages now.
//! Elsewhere (glibc, Windows) blocks this large are unmapped when freed: nothing to do.
//!
//! [`fsync`] is the plain POSIX `fsync`, which std doesn't offer on macOS: there `File::sync_all`
//! and `sync_data` are `F_FULLFSYNC`, which SMB shares refuse with ENOTSUP while `fsync` works
//! (issue #1407).
//!
//! This is LightCraft's only crate allowed `unsafe` (two FFI calls, in `macos`). Its API is safe;
//! on failure or on other platforms `release_free_memory` simply releases nothing.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

/// Return free allocator pages to the system. Returns the number of bytes released (always 0 on
/// platforms where freed large blocks are already unmapped). Safe to call at any time, from any
/// thread.
pub fn release_free_memory() -> usize {
    #[cfg(target_os = "macos")]
    {
        macos::release()
    }
    #[cfg(not(target_os = "macos"))]
    {
        0
    }
}

/// Flush `file` to the storage device with a plain `fsync`. On macOS that is the libSystem call
/// (std's `sync_all` is `F_FULLFSYNC`, which some volumes refuse); elsewhere it is `sync_all`.
pub fn fsync(file: &std::fs::File) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        macos::fsync(file)
    }
    #[cfg(not(target_os = "macos"))]
    {
        file.sync_all()
    }
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod macos {
    use std::os::fd::AsRawFd;

    unsafe extern "C" {
        /// libSystem (unistd.h): flush the open file `fd` to the device. 0, or -1 and `errno`.
        #[link_name = "fsync"]
        fn c_fsync(fd: std::ffi::c_int) -> std::ffi::c_int;
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

    pub fn fsync(file: &std::fs::File) -> std::io::Result<()> {
        loop {
            // SAFETY: `fsync` (libSystem, unistd.h) takes a file descriptor by value and touches no
            // memory of ours. The descriptor is open for the whole call: `file` is borrowed.
            if unsafe { c_fsync(file.as_raw_fd()) } == 0 {
                return Ok(());
            }
            let e = std::io::Error::last_os_error();
            if e.kind() != std::io::ErrorKind::Interrupted {
                return Err(e);
            }
        }
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
        if cfg!(not(target_os = "macos")) {
            assert_eq!(released, 0);
        }
        // The allocator must still work normally afterwards.
        let again = vec![7u8; 16 << 20];
        assert_eq!(again[again.len() - 1], 7);
    }

    #[test]
    fn fsync_flushes_an_open_file_and_reports_errors() {
        use std::io::Write;
        let p = std::env::temp_dir().join(format!("lc-sysmem-fsync-{}", std::process::id()));
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(b"data").unwrap();
        super::fsync(&f).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"data");
        let _ = std::fs::remove_file(&p);
        // a directory opened read-only can be synced too; what fails is an error, not a panic
        if let Ok(d) = std::fs::File::open(std::env::temp_dir()) {
            let _ = super::fsync(&d);
        }
    }

    #[test]
    fn concurrent_calls_are_safe() {
        let handles: Vec<_> = (0..4).map(|_| std::thread::spawn(release_free_memory)).collect();
        for h in handles {
            assert!(h.join().is_ok());
        }
    }
}
