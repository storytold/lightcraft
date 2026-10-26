//! How much memory this process may use: physical RAM held to the process's cgroup limit.
//!
//! Inside a container, a Flatpak, a systemd scope or service with `MemoryMax`, or a CI runner with
//! a memory limit, the process may get far less than the machine has, and is killed when it goes
//! over. On Linux, [`effective_total`] and [`available`] read `/proc/meminfo` and the memory
//! controller of every cgroup from this process's up to the root (a limit set on an ancestor binds
//! too): cgroup v2 `memory.max` / `memory.current`, and v1 `memory.limit_in_bytes` /
//! `memory.usage_in_bytes` on hosts that still mount it. Usage counts the cgroup's page cache, so
//! its inactive file pages (`memory.stat`), which the kernel reclaims before it kills anything,
//! count as room, as `MemAvailable` counts reclaimable cache. Elsewhere they return `None`: callers keep
//! their platform's own way of asking.
//!
//! Std only, and never panics: a missing, unreadable or malformed file means "no limit known
//! there". The parsing takes file contents and a root directory, so it is tested on synthetic
//! trees ([`cgroup_memory_at`]).
#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

/// Where the cgroup file systems are mounted.
pub const CGROUP_ROOT: &str = "/sys/fs/cgroup";

/// Path components followed below the root at most (the kernel nests far less in practice).
const MAX_DEPTH: usize = 64;

/// A limit this large is no limit: cgroup v1 reports "unlimited" as a page-rounded `i64::MAX`
/// (9223372036854771712), and no machine has an exabyte of RAM.
const UNLIMITED: u64 = 1 << 60;

/// The memory controller's view of this process's cgroups.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CgroupMemory {
    /// The tightest limit (bytes) on this process's cgroup or any ancestor; `None` without one.
    pub limit: Option<u64>,
    /// Bytes the process can still be charged before it reaches a limit: the smallest
    /// `limit − (usage − inactive file cache)` over the limited cgroups whose usage could be read
    /// (saturating at 0).
    pub headroom: Option<u64>,
}

/// Memory this process may use at most (bytes): physical RAM, or its cgroup limit when that is
/// smaller. Linux only; `None` elsewhere or when nothing can be read.
pub fn effective_total() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let meminfo = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
        smaller(meminfo_bytes(&meminfo, "MemTotal"), cgroup_memory().limit)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Memory this process could get now (bytes): what the system has available (`MemAvailable`,
/// free plus reclaimable cache), or the room left under its cgroup limit when that is smaller.
/// Linux only; `None` elsewhere or when nothing can be read.
pub fn available() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let meminfo = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
        smaller(meminfo_bytes(&meminfo, "MemAvailable"), cgroup_memory().headroom)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// This process's cgroup memory limit and headroom (Linux; nothing elsewhere).
pub fn cgroup_memory() -> CgroupMemory {
    #[cfg(target_os = "linux")]
    {
        match std::fs::read_to_string("/proc/self/cgroup") {
            Ok(own) => cgroup_memory_at(Path::new(CGROUP_ROOT), &own),
            Err(_) => CgroupMemory::default(),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        CgroupMemory::default()
    }
}

/// The smaller of two optional amounts; either alone when the other is unknown.
pub fn smaller(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// A `/proc/meminfo` field (e.g. `MemTotal`) in bytes; `None` when absent or malformed.
pub fn meminfo_bytes(meminfo: &str, key: &str) -> Option<u64> {
    let rest = meminfo.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix(':'))?;
    let mut words = rest.split_whitespace();
    let n: u64 = words.next()?.parse().ok()?;
    match words.next() {
        Some("kB") => n.checked_mul(1024),
        None => Some(n),
        Some(_) => None,
    }
}

/// A cgroup memory limit file (`memory.max`, `memory.limit_in_bytes`): bytes, or `None` for
/// `max`, "unlimited" sentinels, 0 and anything unparsable.
pub fn parse_limit(contents: &str) -> Option<u64> {
    contents.trim().parse::<u64>().ok().filter(|&n| n > 0 && n < UNLIMITED)
}

/// A cgroup usage file (`memory.current`, `memory.usage_in_bytes`): bytes, if it parses.
pub fn parse_usage(contents: &str) -> Option<u64> {
    contents.trim().parse().ok()
}

/// A field of a cgroup `memory.stat` file (`key value` lines), if present and numeric.
pub fn stat_value(stat: &str, key: &str) -> Option<u64> {
    stat.lines().find_map(|l| {
        let mut words = l.split_whitespace();
        (words.next()? == key).then(|| words.next()?.parse().ok())?
    })
}

/// The memory limit and headroom of the process whose `/proc/self/cgroup` is `own`, with the
/// cgroup file systems under `root` (normally [`CGROUP_ROOT`]): the cgroup v2 entry (`0::/path`
/// → `root/path/memory.max`) and a v1 `memory` controller (`N:memory:/path` →
/// `root/memory/path/memory.limit_in_bytes`), each walked up to its root.
pub fn cgroup_memory_at(root: &Path, own: &str) -> CgroupMemory {
    let mut out = CgroupMemory::default();
    for line in own.lines() {
        let mut fields = line.trim().splitn(3, ':');
        let (Some(_id), Some(controllers), Some(path)) = (fields.next(), fields.next(), fields.next()) else {
            continue;
        };
        let (base, limit_file, usage_file, inactive_key) = if controllers.is_empty() {
            (root.to_path_buf(), "memory.max", "memory.current", "inactive_file")
        } else if controllers.split(',').any(|c| c == "memory") {
            (root.join("memory"), "memory.limit_in_bytes", "memory.usage_in_bytes", "total_inactive_file")
        } else {
            continue;
        };
        for dir in ancestors(&base, path) {
            let Some(limit) = read(&dir.join(limit_file)).as_deref().and_then(parse_limit) else {
                continue;
            };
            out.limit = smaller(out.limit, Some(limit));
            if let Some(used) = read(&dir.join(usage_file)).as_deref().and_then(parse_usage) {
                let stat = read(&dir.join("memory.stat")).unwrap_or_default();
                let used = used.saturating_sub(stat_value(&stat, inactive_key).unwrap_or(0));
                out.headroom = smaller(out.headroom, Some(limit.saturating_sub(used)));
            }
        }
    }
    out
}

/// `base` joined with each prefix of the cgroup `path`, deepest first, `base` itself last. A path
/// that is not plain (`..`, `.`) yields `base` alone; at most [`MAX_DEPTH`] components are followed.
fn ancestors(base: &Path, path: &str) -> Vec<PathBuf> {
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).take(MAX_DEPTH).collect();
    let mut dirs = vec![base.to_path_buf()];
    if parts.iter().all(|p| *p != ".." && *p != ".") {
        let mut dir = base.to_path_buf();
        for part in parts {
            dir.push(part);
            dirs.push(dir.clone());
        }
    }
    dirs.reverse();
    dirs
}

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let dir = std::env::temp_dir().join(format!("lightcraft-sysmem-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }

        fn write(&self, rel: &str, contents: &str) {
            let path = self.0.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const GIB: u64 = 1 << 30;

    #[test]
    fn limit_files_parse_numbers_and_treat_max_and_garbage_as_no_limit() {
        assert_eq!(parse_limit("2147483648\n"), Some(2 * GIB));
        assert_eq!(parse_limit("  2147483648  \n\n"), Some(2 * GIB));
        assert_eq!(parse_limit("max\n"), None);
        assert_eq!(parse_limit("max"), None);
        assert_eq!(parse_limit(""), None);
        assert_eq!(parse_limit("\n"), None);
        assert_eq!(parse_limit("2G"), None);
        assert_eq!(parse_limit("-1"), None);
        assert_eq!(parse_limit("0"), None);
        assert_eq!(parse_limit("12 34"), None);
        assert_eq!(parse_limit("\u{fffd}\u{0}"), None);
        // over u64, and v1's "unlimited" sentinel
        assert_eq!(parse_limit("99999999999999999999999"), None);
        assert_eq!(parse_limit("9223372036854771712"), None);
        assert_eq!(parse_usage("123\n"), Some(123));
        assert_eq!(parse_usage("lots"), None);
    }

    #[test]
    fn meminfo_fields_are_read_in_bytes() {
        let info = "MemTotal:       22548580 kB\nMemFree:  100 kB\nMemAvailable:   16000000 kB\nHugePages_Total:       0\n";
        assert_eq!(meminfo_bytes(info, "MemTotal"), Some(22548580 * 1024));
        assert_eq!(meminfo_bytes(info, "MemAvailable"), Some(16000000 * 1024));
        assert_eq!(meminfo_bytes(info, "HugePages_Total"), Some(0));
        assert_eq!(meminfo_bytes(info, "Mem"), None, "a prefix of a key is not the key");
        assert_eq!(meminfo_bytes(info, "SwapTotal"), None);
        assert_eq!(meminfo_bytes("MemTotal: lots kB\n", "MemTotal"), None);
        assert_eq!(meminfo_bytes("MemTotal: 5 MB\n", "MemTotal"), None);
        assert_eq!(meminfo_bytes("MemTotal: 99999999999999999999 kB\n", "MemTotal"), None);
        assert_eq!(meminfo_bytes("MemTotal: 18446744073709551615 kB\n", "MemTotal"), None, "overflow");
        assert_eq!(meminfo_bytes("", "MemTotal"), None);
    }

    #[test]
    fn smaller_takes_the_known_minimum() {
        assert_eq!(smaller(Some(3), Some(2)), Some(2));
        assert_eq!(smaller(Some(3), None), Some(3));
        assert_eq!(smaller(None, Some(2)), Some(2));
        assert_eq!(smaller(None, None), None);
    }

    #[test]
    fn a_v2_limit_on_the_own_cgroup_is_found_with_its_headroom() {
        let s = Scratch::new("v2-own");
        s.write("user.slice/app.scope/memory.max", "2147483648\n");
        s.write("user.slice/app.scope/memory.current", "536870912\n");
        s.write("user.slice/memory.max", "max\n");
        s.write("user.slice/memory.current", "9999999999\n");
        let got = cgroup_memory_at(&s.0, "0::/user.slice/app.scope\n");
        assert_eq!(got, CgroupMemory { limit: Some(2 * GIB), headroom: Some(GIB + GIB / 2) });
    }

    #[test]
    fn a_limit_on_an_ancestor_only_binds_the_nested_cgroup() {
        let s = Scratch::new("v2-ancestor");
        s.write("a/memory.max", "4294967296");
        s.write("a/memory.current", "1073741824");
        s.write("a/b/memory.max", "max");
        s.write("a/b/c/d/memory.max", "max");
        s.write("a/b/c/d/memory.current", "10");
        let got = cgroup_memory_at(&s.0, "0::/a/b/c/d");
        assert_eq!(got, CgroupMemory { limit: Some(4 * GIB), headroom: Some(3 * GIB) });
    }

    #[test]
    fn the_tightest_limit_and_headroom_win_across_levels() {
        let s = Scratch::new("v2-tightest");
        // a looser parent nearly full: its headroom is the smaller one
        s.write("p/memory.max", &(8 * GIB).to_string());
        s.write("p/memory.current", &(7 * GIB).to_string());
        s.write("p/c/memory.max", &(4 * GIB).to_string());
        s.write("p/c/memory.current", &GIB.to_string());
        let got = cgroup_memory_at(&s.0, "0::/p/c");
        assert_eq!(got, CgroupMemory { limit: Some(4 * GIB), headroom: Some(GIB) });
    }

    #[test]
    fn inactive_file_cache_counts_as_room() {
        assert_eq!(stat_value("anon 5\nfile 9\ninactive_file 7\n", "inactive_file"), Some(7));
        assert_eq!(stat_value("anon 5\ninactive_file lots\n", "inactive_file"), None);
        assert_eq!(stat_value("total_inactive_file 3\n", "inactive_file"), None, "whole keys only");
        assert_eq!(stat_value("", "inactive_file"), None);

        let s = Scratch::new("v2-cache");
        s.write("x/memory.max", &(4 * GIB).to_string());
        s.write("x/memory.current", &(3 * GIB).to_string());
        s.write("x/memory.stat", &format!("anon {}\nfile {}\ninactive_file {}\nactive_file 1\n", GIB, 2 * GIB, GIB));
        assert_eq!(cgroup_memory_at(&s.0, "0::/x"), CgroupMemory { limit: Some(4 * GIB), headroom: Some(2 * GIB) });
        // a malformed stat file counts no cache, and more cache than usage leaves the whole limit
        s.write("x/memory.stat", "inactive_file -1\n");
        assert_eq!(cgroup_memory_at(&s.0, "0::/x").headroom, Some(GIB));
        s.write("x/memory.stat", &format!("inactive_file {}\n", 9 * GIB));
        assert_eq!(cgroup_memory_at(&s.0, "0::/x").headroom, Some(4 * GIB));

        // v1 counts the hierarchy's cache under `total_inactive_file`
        s.write("memory/y/memory.limit_in_bytes", &(2 * GIB).to_string());
        s.write("memory/y/memory.usage_in_bytes", &(2 * GIB).to_string());
        s.write("memory/y/memory.stat", &format!("inactive_file 1\ntotal_inactive_file {}\n", GIB));
        assert_eq!(cgroup_memory_at(&s.0, "4:memory:/y").headroom, Some(GIB));
    }

    #[test]
    fn usage_over_the_limit_leaves_no_headroom_and_unreadable_usage_none() {
        let s = Scratch::new("v2-over");
        s.write("x/memory.max", "1000");
        s.write("x/memory.current", "5000");
        assert_eq!(cgroup_memory_at(&s.0, "0::/x"), CgroupMemory { limit: Some(1000), headroom: Some(0) });
        s.write("x/memory.current", "garbage");
        assert_eq!(cgroup_memory_at(&s.0, "0::/x"), CgroupMemory { limit: Some(1000), headroom: None });
    }

    #[test]
    fn no_limit_anywhere_or_missing_files_mean_no_limit() {
        let s = Scratch::new("v2-none");
        s.write("u/memory.max", "max\n");
        s.write("u/memory.current", "123");
        assert_eq!(cgroup_memory_at(&s.0, "0::/u\n"), CgroupMemory::default());
        // the cgroup directory is gone (or deleted) and the root has no memory.max
        assert_eq!(cgroup_memory_at(&s.0, "0::/gone/away (deleted)\n"), CgroupMemory::default());
        // no cgroup file system at all
        assert_eq!(cgroup_memory_at(&s.0.join("absent"), "0::/u\n"), CgroupMemory::default());
        // garbage /proc/self/cgroup
        assert_eq!(cgroup_memory_at(&s.0, ""), CgroupMemory::default());
        assert_eq!(cgroup_memory_at(&s.0, "nonsense\n\n::\n7"), CgroupMemory::default());
    }

    #[test]
    fn a_namespaced_root_with_its_own_limit_is_read() {
        // In a container with a cgroup namespace, /proc/self/cgroup says "/" and the mount's root
        // is the container's cgroup, limit included.
        let s = Scratch::new("v2-ns");
        s.write("memory.max", "1073741824\n");
        s.write("memory.current", "0\n");
        assert_eq!(cgroup_memory_at(&s.0, "0::/\n"), CgroupMemory { limit: Some(GIB), headroom: Some(GIB) });
    }

    #[test]
    fn a_v1_only_host_reads_the_memory_controller() {
        let s = Scratch::new("v1");
        s.write("memory/docker/abc/memory.limit_in_bytes", "536870912\n");
        s.write("memory/docker/abc/memory.usage_in_bytes", "268435456\n");
        s.write("memory/docker/memory.limit_in_bytes", "9223372036854771712\n");
        s.write("memory/memory.limit_in_bytes", "9223372036854771712\n");
        // a v2 file in the v1 layout is not read
        s.write("docker/abc/memory.max", "1");
        let own = "12:pids:/docker/abc\n11:memory:/docker/abc\n4:cpu,cpuacct:/docker/abc\n";
        assert_eq!(cgroup_memory_at(&s.0, own), CgroupMemory { limit: Some(GIB / 2), headroom: Some(GIB / 4) });
        // memory co-mounted with other controllers
        assert_eq!(cgroup_memory_at(&s.0, "3:cpuset,memory:/docker/abc\n"), CgroupMemory { limit: Some(GIB / 2), headroom: Some(GIB / 4) });
        // a v1 host without a memory limit
        assert_eq!(cgroup_memory_at(&s.0, "11:memory:/other\n"), CgroupMemory::default());
    }

    #[test]
    fn a_v1_container_whose_host_path_does_not_exist_reads_the_mount_root() {
        let s = Scratch::new("v1-ns");
        s.write("memory/memory.limit_in_bytes", "268435456");
        s.write("memory/memory.usage_in_bytes", "0");
        assert_eq!(cgroup_memory_at(&s.0, "9:memory:/kubepods/pod1/abc\n"), CgroupMemory { limit: Some(GIB / 4), headroom: Some(GIB / 4) });
    }

    #[test]
    fn a_hybrid_host_takes_the_tightest_of_both_hierarchies() {
        let s = Scratch::new("hybrid");
        s.write("memory/a/memory.limit_in_bytes", &(3 * GIB).to_string());
        s.write("a/memory.max", &(2 * GIB).to_string());
        let got = cgroup_memory_at(&s.0, "5:memory:/a\n0::/a\n");
        assert_eq!(got.limit, Some(2 * GIB));
        assert_eq!(got.headroom, None, "no usage files");
    }

    #[test]
    fn paths_that_climb_out_are_not_followed() {
        let s = Scratch::new("dotdot");
        let outside = Scratch::new("dotdot-outside");
        outside.write("memory.max", "1");
        let rel = format!("../{}", outside.0.file_name().and_then(|n| n.to_str()).unwrap());
        assert_eq!(cgroup_memory_at(&s.0, &format!("0::/{rel}\n")), CgroupMemory::default());
        assert_eq!(ancestors(Path::new("/r"), "/a/./b"), vec![PathBuf::from("/r")]);
    }

    #[test]
    fn the_walk_is_bounded_and_ordered_deepest_first() {
        assert_eq!(ancestors(Path::new("/r"), "/a//b/"), vec![PathBuf::from("/r/a/b"), PathBuf::from("/r/a"), PathBuf::from("/r")]);
        let deep = "/x".repeat(10_000);
        assert_eq!(ancestors(Path::new("/r"), &deep).len(), MAX_DEPTH + 1);
    }

    /// What this process sees; run by hand inside and outside a limited scope, e.g.
    /// `systemd-run --user --scope -p MemoryMax=2G -p MemorySwapMax=0 cargo test -p
    /// lightcraft-sysmem --lib -- --ignored --nocapture prints_what_this_process_sees`.
    #[test]
    #[ignore = "diagnostic: prints the memory this process may use"]
    fn prints_what_this_process_sees() {
        let mib = |b: Option<u64>| b.map(|b| b >> 20);
        let info = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
        println!("MemTotal      {:?} MiB", mib(meminfo_bytes(&info, "MemTotal")));
        println!("MemAvailable  {:?} MiB", mib(meminfo_bytes(&info, "MemAvailable")));
        let cg = cgroup_memory();
        println!("cgroup limit  {:?} MiB, headroom {:?} MiB", mib(cg.limit), mib(cg.headroom));
        println!("effective_total {:?} MiB", mib(effective_total()));
        println!("available       {:?} MiB", mib(available()));
    }
}
