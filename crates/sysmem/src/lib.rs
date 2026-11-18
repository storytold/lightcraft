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
//! Android's bionic allocator (scudo) keeps freed pages until its own purge interval: a phone has
//! no memory to spare for that, so `mallopt(M_PURGE_ALL)` (or `M_PURGE` before Android 14) returns
//! them at once.
//!
//! [`fsync`] is the plain POSIX `fsync`, which std doesn't offer on macOS: there `File::sync_all`
//! and `sync_data` are `F_FULLFSYNC`, which SMB shares refuse with ENOTSUP while `fsync` works
//! (upstream issue 1407).
//!
//! [`lower_thread_priority`] runs the calling thread below normal priority (Windows'
//! `SetThreadPriority`), so renders leave the desktop responsive; [`lowest_thread_priority`] lower
//! still, for work done ahead of time that must yield to the renders.
//!
//! [`free_disk_space`] says how much room a drive has (Windows' `GetDiskFreeSpaceExW`; elsewhere
//! POSIX `df`, no system call of our own), for the preview cache's automatic size.
//!
//! This is one of the workspace's few crates allowed `unsafe` (two FFI calls in `macos`, one in
//! `glibc`, one in `bionic`, three in `windows`). Its API is safe; on failure or on other platforms each call simply does nothing.
//!
//! It also says how much memory the process may use ([`limits`], safe std-only code): physical RAM
//! held to the process's cgroup limit on Linux, for the engine's memory budget and `cargo xtask ci`.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod limits;

pub use limits::{available, effective_total};

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
    #[cfg(target_os = "android")]
    {
        bionic::release()
    }
    #[cfg(not(any(target_os = "macos", target_os = "android", all(target_os = "linux", target_env = "gnu"))))]
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

/// Run the calling thread below normal priority, so background renders and the render workers'
/// fan-out never take the processor from the desktop and other apps. Returns whether the priority
/// changed; anywhere but Windows it is `false` and nothing happens (the fallback). Safe to call from
/// any thread, any number of times.
pub fn lower_thread_priority() -> bool {
    #[cfg(windows)]
    {
        windows::lower()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Run the calling thread at the lowest priority short of idle, below the render workers
/// ([`lower_thread_priority`]): for work done ahead of time (preparing the photos a user will step to),
/// which must take only the processor time the photo on screen leaves. Returns whether the priority
/// changed; anywhere but Windows it is `false` and nothing happens (the fallback).
pub fn lowest_thread_priority() -> bool {
    #[cfg(windows)]
    {
        windows::lowest()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Bytes free for this user on the drive that holds `path` (the folder itself, or the nearest
/// folder above it that exists), `None` when it can't be told. Windows asks `GetDiskFreeSpaceExW`;
/// other systems run POSIX `df -Pk`. Blocking (a few ms; `df` a process start): not for a UI frame.
pub fn free_disk_space(path: &std::path::Path) -> Option<u64> {
    let mut at = Some(path);
    while let Some(p) = at {
        if p.is_dir() {
            break;
        }
        at = p.parent();
    }
    let dir = at.filter(|p| !p.as_os_str().is_empty())?;
    #[cfg(windows)]
    {
        windows::free_space(dir)
    }
    #[cfg(all(not(windows), not(target_arch = "wasm32")))]
    {
        df_free(dir)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = dir;
        None
    }
}

/// `df -Pk dir`: the fourth column of its second line is the space available, in KiB.
#[cfg(all(not(windows), not(target_arch = "wasm32")))]
fn df_free(dir: &std::path::Path) -> Option<u64> {
    let out = std::process::Command::new("df").arg("-Pk").arg(dir).output().ok()?;
    if !out.status.success() {
        return None;
    }
    parse_df(&String::from_utf8_lossy(&out.stdout))
}

/// The available KiB of `df -P`'s output, in bytes.
#[cfg_attr(any(windows, target_arch = "wasm32"), allow(dead_code))]
fn parse_df(text: &str) -> Option<u64> {
    let line = text.lines().nth(1)?;
    // (a mount point may hold spaces; the numbers come before it)
    let kib: u64 = line.split_whitespace().nth(3)?.parse().ok()?;
    kib.checked_mul(1024)
}

#[cfg(target_os = "android")]
#[allow(unsafe_code)]
mod bionic {
    use std::ffi::c_int;

    /// bionic malloc.h: purge every cache and free page the allocator holds, the thread caches
    /// included (Android 14, API 34).
    const M_PURGE_ALL: c_int = -104;
    /// bionic malloc.h: purge the free pages (Android 9, API 28: the oldest the app runs on).
    const M_PURGE: c_int = -101;

    unsafe extern "C" {
        /// bionic malloc: set an allocator option. Returns 1 when the option was taken, 0 when it
        /// is unknown to this Android or the allocator in use.
        fn mallopt(param: c_int, value: c_int) -> c_int;
    }

    pub fn release() -> usize {
        // SAFETY: `mallopt` is a documented, thread-safe bionic function (malloc.h, every API
        // level); M_PURGE (API 28) and M_PURGE_ALL (API 34) only return already-free pages to the
        // system. It takes no pointers, reads and writes no memory we own, and an option this
        // Android does not know is refused with 0, nothing else.
        let all = unsafe { mallopt(M_PURGE_ALL, 0) };
        let released = if all != 0 {
            all
        } else {
            // SAFETY: as above.
            unsafe { mallopt(M_PURGE, 0) }
        };
        usize::from(released != 0)
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

#[cfg(windows)]
#[allow(unsafe_code)]
mod windows {
    use std::ffi::c_void;

    /// `THREAD_PRIORITY_BELOW_NORMAL` (processthreadsapi.h).
    const BELOW_NORMAL: i32 = -1;
    /// `THREAD_PRIORITY_LOWEST` (processthreadsapi.h).
    const LOWEST: i32 = -2;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        /// The calling thread's pseudo handle (needs no closing).
        fn GetCurrentThread() -> *mut c_void;
        /// Nonzero on success.
        fn SetThreadPriority(thread: *mut c_void, priority: i32) -> i32;
    }

    pub fn lower() -> bool {
        // SAFETY: both are documented, thread-safe kernel32 functions. `GetCurrentThread` returns a
        // constant pseudo handle for the calling thread (never null, never closed), which is what
        // `SetThreadPriority` takes; the priority is a documented constant. Neither reads or writes
        // memory we own, and a failure only leaves the priority as it was.
        unsafe { SetThreadPriority(GetCurrentThread(), BELOW_NORMAL) != 0 }
    }

    pub fn lowest() -> bool {
        // SAFETY: as in `lower`: documented, thread-safe kernel32 functions on the calling thread's
        // pseudo handle with a documented priority constant; a failure leaves the priority as it was.
        unsafe { SetThreadPriority(GetCurrentThread(), LOWEST) != 0 }
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        /// Nonzero on success. `dir` is a NUL-terminated UTF-16 folder name; each out pointer may be null.
        fn GetDiskFreeSpaceExW(dir: *const u16, free_to_caller: *mut u64, total: *mut u64, total_free: *mut u64) -> i32;
    }

    pub fn free_space(dir: &std::path::Path) -> Option<u64> {
        use std::os::windows::ffi::OsStrExt;
        if dir.as_os_str().encode_wide().any(|c| c == 0) {
            return None;
        }
        let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        let mut free = 0u64;
        // SAFETY: `GetDiskFreeSpaceExW` is a documented, thread-safe kernel32 function. `wide` is a
        // NUL-terminated UTF-16 string (no NUL inside, checked above) that lives for the whole call;
        // `free` is a live, aligned u64 owned by this frame that it writes; the other two out
        // pointers are null, which the function allows. Nothing is kept after it returns.
        let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, std::ptr::null_mut(), std::ptr::null_mut()) } != 0;
        ok.then_some(free)
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
    fn the_free_space_of_the_drive_of_a_folder_is_known() {
        let dir = std::env::temp_dir();
        let free = super::free_disk_space(&dir);
        assert!(free.is_some_and(|b| b > 0), "{free:?}");
        // a folder that isn't there: the drive of the nearest one above it
        assert_eq!(super::free_disk_space(&dir.join("no-such-folder/below")).is_some(), free.is_some());
        assert_eq!(super::free_disk_space(std::path::Path::new("")), None);
    }

    // Given df's output, odd or cut short, the available bytes or nothing (never a panic)
    #[test]
    fn df_output_is_read_or_refused() {
        let ok = "Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/x 1000 400 600 40% /my drive\n";
        assert_eq!(super::parse_df(ok), Some(600 * 1024));
        for bad in ["", "header only", "h\n/dev/x 1 2", "h\n/dev/x 1 2 -5 9%", "h\n/dev/x 1 2 99999999999999999999 1%"] {
            assert_eq!(super::parse_df(bad), None, "{bad:?}");
        }
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
    fn lowered_threads_keep_working() {
        // a thread at the lowest priority keeps working
        {
            let (changed, sum) = std::thread::spawn(|| (super::lowest_thread_priority(), (0..1000u64).sum::<u64>())).join().unwrap();
            assert_eq!(sum, 499_500);
            assert_eq!(changed, cfg!(windows));
        }
        // a lowered thread keeps working
        {
            let (changed, again, sum) = std::thread::spawn(|| {
                let changed = super::lower_thread_priority();
                let again = super::lower_thread_priority();
                (changed, again, (0..1000u64).sum::<u64>())
            })
            .join()
            .unwrap();
            assert_eq!(sum, 499_500);
            assert_eq!(changed, cfg!(windows));
            assert_eq!(again, cfg!(windows));
        }
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
}
