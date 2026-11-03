//! Give memory the system allocator keeps after frees back to the operating system.
//!
//! macOS's allocator keeps freed large blocks mapped as "reusable": they no longer count in the
//! process's memory footprint but stay in its resident size until the system runs short. After a
//! raw decode (hundreds of MB of temporaries) that made the resident size climb towards the sum of
//! everything ever decoded. `malloc_zone_pressure_relief` (libSystem) returns those pages now.
//! Elsewhere (glibc, Windows) blocks this large are unmapped when freed: nothing to do.
//!
//! It also says how much memory the process may use ([`limits`], safe std-only code): physical RAM
//! held to the process's cgroup limit on Linux, or to its job object's memory limit on Windows, for
//! the engine's memory budget and `cargo xtask ci`.
//!
//! This is LightCraft's only crate allowed `unsafe`: three FFI calls, `malloc_zone_pressure_relief`
//! (in `macos`), and `GlobalMemoryStatusEx` and `QueryInformationJobObject` (in `win32`, which only
//! reads the numbers; [`limits`] decides what they mean). Its API is safe, and a call that fails
//! releases nothing or reports "unknown".
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod limits;

pub use limits::{available, effective_total, job_limit};

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

/// The Windows calls behind [`limits::effective_total`] and [`limits::job_limit`]: each fills a
/// struct we own and reports failure as `None`.
#[cfg(windows)]
#[allow(unsafe_code)]
mod win32 {
    use windows_sys::Win32::System::JobObjects::{
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, QueryInformationJobObject,
    };
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

    use crate::limits::JobLimits;

    /// Installed physical memory (bytes): `MEMORYSTATUSEX::ullTotalPhys`. `None` if the call fails.
    pub fn total_physical() -> Option<u64> {
        let mut status = MEMORYSTATUSEX { dwLength: u32::try_from(size_of::<MEMORYSTATUSEX>()).ok()?, ..MEMORYSTATUSEX::default() };
        // SAFETY: `GlobalMemoryStatusEx` (kernel32, sysinfoapi.h) writes one `MEMORYSTATUSEX` through
        // the pointer, which points to a live, properly aligned local of that type whose `dwLength` is
        // set to its size as the function requires. It keeps no pointer after returning and has no
        // other effect; on failure it returns 0 and we don't read the fields.
        let ok = unsafe { GlobalMemoryStatusEx(&mut status) };
        (ok != 0 && status.ullTotalPhys > 0).then_some(status.ullTotalPhys)
    }

    /// The memory limits of the job object this process runs in (`None` if it is in none or the call
    /// fails). The flags say which limits are set ([`crate::limits::job_memory_limit`]).
    pub fn job_limits() -> Option<JobLimits> {
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        let len = u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()).ok()?;
        // SAFETY: `QueryInformationJobObject` (kernel32, jobapi2.h) with a null job handle queries the
        // job of the calling process; no handle is opened or needs closing. With
        // `JobObjectExtendedLimitInformation` it writes at most `len` bytes, the size of the live,
        // properly aligned `JOBOBJECT_EXTENDED_LIMIT_INFORMATION` local the pointer points to. The
        // returned-length pointer is optional and passed as null. It keeps no pointer after
        // returning; on failure (e.g. the process is in no job) it returns 0 and we don't read the
        // fields.
        let ok = unsafe {
            QueryInformationJobObject(std::ptr::null_mut(), JobObjectExtendedLimitInformation, (&raw mut info).cast(), len, std::ptr::null_mut())
        };
        if ok == 0 {
            return None;
        }
        Some(JobLimits {
            flags: info.BasicLimitInformation.LimitFlags,
            // a `usize` fits a `u64` on every Windows target; the fallback is "no limit"
            process_memory: u64::try_from(info.ProcessMemoryLimit).unwrap_or(u64::MAX),
            job_memory: u64::try_from(info.JobMemoryLimit).unwrap_or(u64::MAX),
        })
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

    /// The job query reads real limits (#3652): a child copy of this test joins three nested jobs in
    /// turn, a per-process limit, a whole-job limit, then both, and must read back exactly what each
    /// set (a nested job reports only its own). The limits are far above any machine's RAM, so they
    /// constrain nothing; the child keeps this process out of any job it makes.
    #[test]
    #[cfg(windows)]
    #[allow(unsafe_code)]
    fn a_job_with_known_limits_is_read_back_exactly() {
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };
        use windows_sys::Win32::System::Threading::GetCurrentProcess;

        use crate::limits::{JOB_OBJECT_LIMIT_JOB_MEMORY, JOB_OBJECT_LIMIT_PROCESS_MEMORY, effective_total, job_limit};

        const CHILD: &str = "LIGHTCRAFT_SYSMEM_JOB_TEST_CHILD";
        const PROCESS: u64 = 1 << 45; // 32 TiB
        const JOB: u64 = 1 << 44; // 16 TiB
        if std::env::var_os(CHILD).is_none() {
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "tests::a_job_with_known_limits_is_read_back_exactly", "--nocapture", "--test-threads=1"])
                .env(CHILD, "1")
                .output()
                .unwrap();
            let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
            assert!(out.status.success(), "child failed:\n{text}");
            assert!(text.contains("1 passed"), "the child didn't run the test:\n{text}");
            return;
        }
        let join = |flags: u32, process: u64, job: u64| {
            // SAFETY: test-only. `CreateJobObjectW` with null attributes and a null name makes an
            // unnamed job and returns its handle (null on failure, checked). `SetInformationJobObject`
            // reads one `JOBOBJECT_EXTENDED_LIMIT_INFORMATION` from a live local of that type and the
            // size passed. `AssignProcessToJobObject` puts this (child) process in the job; the
            // pseudo-handle from `GetCurrentProcess` needs no closing, and the job handle is left
            // open until the child exits. Nothing keeps a pointer past the call.
            unsafe {
                let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                assert!(!handle.is_null(), "CreateJobObjectW failed");
                let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
                info.BasicLimitInformation.LimitFlags = flags;
                info.ProcessMemoryLimit = usize::try_from(process).unwrap();
                info.JobMemoryLimit = usize::try_from(job).unwrap();
                let len = u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()).unwrap();
                assert!(
                    SetInformationJobObject(handle, JobObjectExtendedLimitInformation, (&raw const info).cast(), len) != 0,
                    "SetInformationJobObject failed"
                );
                assert!(AssignProcessToJobObject(handle, GetCurrentProcess()) != 0, "AssignProcessToJobObject failed (nested jobs need Windows 8)");
            }
        };
        let physical = crate::win32::total_physical().unwrap();
        join(JOB_OBJECT_LIMIT_PROCESS_MEMORY, PROCESS, 0);
        assert_eq!(job_limit(), Some(PROCESS), "a per-process limit");
        join(JOB_OBJECT_LIMIT_JOB_MEMORY, 0, JOB);
        assert_eq!(job_limit(), Some(JOB), "a whole-job limit (the nested job's own)");
        join(JOB_OBJECT_LIMIT_PROCESS_MEMORY | JOB_OBJECT_LIMIT_JOB_MEMORY, PROCESS, JOB);
        assert_eq!(job_limit(), Some(JOB), "both: the smaller");
        assert_eq!(effective_total(), Some(physical.min(JOB)));
    }

    #[test]
    fn concurrent_calls_are_safe() {
        let handles: Vec<_> = (0..4).map(|_| std::thread::spawn(release_free_memory)).collect();
        for h in handles {
            assert!(h.join().is_ok());
        }
    }
}
