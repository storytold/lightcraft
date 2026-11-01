//! **Memory per test.** libtest runs several tests at once in one process, and a test that sizes
//! its caches for the whole machine, as an app does, takes that much once per test running. A
//! test harness sizes them for its share instead: [`per_test_bytes`] is the memory available now
//! divided by the number of tests libtest runs at once, clamped to [`MIN_PER_TEST`] ..=
//! [`MAX_PER_TEST`] (issue #3066).
//!
//! The memory available is `MemAvailable` from `/proc/meminfo` on Linux; elsewhere, or when it
//! can't be read, [`FALLBACK_AVAILABLE`]. It doesn't yet honour a cgroup memory limit (a container
//! or CI runner): #3072 adds that to `lightcraft-sysmem`. The tests at once are counted as libtest
//! counts them: the test binary's `--test-threads` argument, else `RUST_TEST_THREADS` (as `cargo
//! xtask ci` sets it), else the machine's parallelism.

/// The least memory one test is given, however little is available.
pub const MIN_PER_TEST: u64 = 1 << 30;

/// The most memory one test is given, however much is available.
pub const MAX_PER_TEST: u64 = 16 << 30;

/// The memory assumed available where the platform doesn't say (bytes).
pub const FALLBACK_AVAILABLE: u64 = 8 << 30;

/// Bytes of memory one test may use: the memory available now ÷ the tests libtest runs at once,
/// clamped to [`MIN_PER_TEST`] ..= [`MAX_PER_TEST`]. Reads `/proc/meminfo` on every call: a
/// caller that wants one value for the whole run keeps the first.
pub fn per_test_bytes() -> u64 {
    per_test(available_bytes(), test_threads())
}

/// [`per_test_bytes`] for `available` bytes (`None`: unknown, [`FALLBACK_AVAILABLE`]) shared by
/// `threads` tests at once (0 counts as 1).
pub fn per_test(available: Option<u64>, threads: usize) -> u64 {
    let threads = u64::try_from(threads.max(1)).unwrap_or(u64::MAX);
    (available.unwrap_or(FALLBACK_AVAILABLE) / threads).clamp(MIN_PER_TEST, MAX_PER_TEST)
}

/// How many tests libtest runs at once in this process, in libtest's order: the binary's
/// `--test-threads` argument, else `RUST_TEST_THREADS`, else the machine's parallelism, else 1.
pub fn test_threads() -> usize {
    // args_os: `std::env::args` panics on an argument that isn't Unicode
    let args: Vec<String> = std::env::args_os().skip(1).filter_map(|a| a.into_string().ok()).collect();
    let env = std::env::var("RUST_TEST_THREADS").ok();
    threads(test_threads_arg(&args), env.as_deref(), std::thread::available_parallelism().ok().map(|n| n.get()))
}

/// The value of `--test-threads` in a test binary's arguments (without the program name):
/// `--test-threads=N` or `--test-threads N`, before any `--` (after it, libtest reads filters).
pub fn test_threads_arg(args: &[String]) -> Option<&str> {
    let mut args = args.iter().map(String::as_str).take_while(|a| *a != "--");
    while let Some(arg) = args.next() {
        if let Some(value) = arg.strip_prefix("--test-threads=") {
            return Some(value);
        }
        if arg == "--test-threads" {
            return args.next();
        }
    }
    None
}

/// [`test_threads`] from the `--test-threads` argument, the variable's value and the machine's
/// parallelism, the first that is set winning. A value that isn't a positive whole number is
/// ignored (libtest refuses it too).
pub fn threads(arg: Option<&str>, rust_test_threads: Option<&str>, parallelism: Option<usize>) -> usize {
    let count = |v: &str| v.trim().parse::<usize>().ok().filter(|n| *n > 0);
    arg.and_then(count).or(rust_test_threads.and_then(count)).or(parallelism).unwrap_or(1).max(1)
}

/// The memory available to start new work without swapping (bytes), when the platform says.
pub fn available_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/meminfo").ok().as_deref().and_then(mem_available)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// `MemAvailable` from the text of `/proc/meminfo`, in bytes. `None` when the line is missing
/// (kernels before 3.14), malformed, in a unit other than kB, or too large.
pub fn mem_available(meminfo: &str) -> Option<u64> {
    let line = meminfo.lines().find_map(|l| l.strip_prefix("MemAvailable:"))?;
    let mut fields = line.split_whitespace();
    let kb = fields.next()?.parse::<u64>().ok()?;
    match fields.next() {
        Some("kB") => kb.checked_mul(1024),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    #[test]
    fn the_memory_available_is_shared_by_the_tests_at_once() {
        assert_eq!(per_test(Some(16 * GIB), 4), 4 * GIB);
        assert_eq!(per_test(Some(16 * GIB), 8), 2 * GIB);
        assert_eq!(per_test(Some(12 * GIB), 3), 4 * GIB);
    }

    #[test]
    fn the_share_stays_in_its_range() {
        assert_eq!(per_test(Some(4 * GIB), 16), MIN_PER_TEST, "little available");
        assert_eq!(per_test(Some(0), 4), MIN_PER_TEST, "nothing available");
        assert_eq!(per_test(Some(512 * GIB), 1), MAX_PER_TEST, "a big machine");
        assert_eq!(per_test(Some(u64::MAX), 0), MAX_PER_TEST, "no threads counts as one");
        assert_eq!(per_test(Some(8 * GIB), usize::MAX), MIN_PER_TEST);
    }

    #[test]
    fn unknown_memory_assumes_the_fallback() {
        assert_eq!(per_test(None, 4), FALLBACK_AVAILABLE / 4);
        assert_eq!(per_test(None, 64), MIN_PER_TEST);
    }

    #[test]
    fn the_tests_at_once_come_from_rust_test_threads_else_the_machine() {
        assert_eq!(threads(None, Some("4"), Some(16)), 4);
        assert_eq!(threads(None, Some(" 2\n"), Some(16)), 2);
        assert_eq!(threads(None, None, Some(16)), 16);
        assert_eq!(threads(None, Some("0"), Some(16)), 16, "libtest refuses 0");
        assert_eq!(threads(None, Some("four"), Some(8)), 8);
        assert_eq!(threads(None, Some("-1"), None), 1);
        assert_eq!(threads(None, Some("99999999999999999999999"), Some(6)), 6, "too large to count");
        assert_eq!(threads(None, None, None), 1);
        assert_eq!(threads(None, Some(""), Some(0)), 1);
    }

    #[test]
    fn the_test_threads_argument_wins_over_the_variable_and_the_machine() {
        assert_eq!(threads(Some("32"), Some("4"), Some(8)), 32);
        assert_eq!(threads(Some("32"), None, Some(8)), 32);
        assert_eq!(threads(Some("1"), Some("4"), Some(8)), 1);
        assert_eq!(threads(Some("0"), Some("4"), Some(8)), 4, "libtest refuses 0");
        assert_eq!(threads(Some("many"), None, Some(8)), 8);
    }

    #[test]
    fn the_test_threads_argument_is_read_as_libtest_reads_it() {
        let args = |a: &[&str]| a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(test_threads_arg(&args(&["--test-threads=32"])), Some("32"));
        assert_eq!(test_threads_arg(&args(&["tests_masking", "--test-threads", "6", "--nocapture"])), Some("6"));
        assert_eq!(test_threads_arg(&args(&["--nocapture", "--test-threads="])), Some(""));
        assert_eq!(test_threads_arg(&args(&["--test-threads"])), None, "no value");
        assert_eq!(test_threads_arg(&args(&["--", "--test-threads=32"])), None, "a filter after --");
        assert_eq!(test_threads_arg(&args(&["--test-threadsX=3", "test-threads=3"])), None);
        assert_eq!(test_threads_arg(&args(&[])), None);
        assert_eq!(test_threads_arg(&args(&["--test-threads=2", "--test-threads=9"])), Some("2"));
    }

    #[test]
    fn mem_available_is_read_from_meminfo() {
        let meminfo = "MemTotal:       22544024 kB\nMemFree:         2072936 kB\nMemAvailable:   16888300 kB\nBuffers:          123 kB\n";
        assert_eq!(mem_available(meminfo), Some(16_888_300 * 1024));
        assert_eq!(mem_available("MemAvailable: 1 kB"), Some(1024));
    }

    #[test]
    fn hostile_meminfo_is_unknown_not_a_panic() {
        for text in [
            "",
            "MemTotal: 1024 kB\n",
            "MemAvailable:\n",
            "MemAvailable: kB\n",
            "MemAvailable: -5 kB\n",
            "MemAvailable: 12.5 kB\n",
            "MemAvailable: 1024 MB\n",
            "MemAvailable: 1024\n",
            "MemAvailable: 18446744073709551615 kB\n",
            "MemAvailable: 99999999999999999999999999 kB\n",
            "MemAvailableX 1024 kB\n",
            "\u{0}\u{ffff}MemAvailable: 1 kB",
        ] {
            assert_eq!(mem_available(text), None, "{text:?}");
        }
    }

    #[test]
    fn this_machine_gives_a_share_in_range() {
        let share = per_test_bytes();
        assert!((MIN_PER_TEST..=MAX_PER_TEST).contains(&share), "{share}");
        assert!(test_threads() >= 1);
        // /proc can be missing or masked (a sandbox, an old kernel): the share is then the fallback's
        if available_bytes().is_none() {
            assert_eq!(share, per_test(None, test_threads()));
        }
    }
}
