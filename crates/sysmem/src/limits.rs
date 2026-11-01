//! How much memory this process may use: physical RAM held to the process's cgroup limit.
//!
//! Inside a container, a Flatpak, a systemd scope or service with `MemoryMax`, or a CI runner with
//! a memory limit, the process may get far less than the machine has, and is killed when it goes
//! over. On Linux, [`effective_total`] and [`available`] read `/proc/meminfo` and the memory
//! controller of every cgroup from this process's up to the top of its mount (a limit set on an
//! ancestor binds too): cgroup v2 `memory.max` / `memory.current`, and v1 `memory.limit_in_bytes` /
//! `memory.usage_in_bytes` on hosts that still mount it. Usage counts the cgroup's page cache, so
//! its inactive file pages (`memory.stat`), which the kernel reclaims before it kills anything,
//! count as room, as `MemAvailable` counts reclaimable cache. Elsewhere they return `None`: callers
//! keep their platform's own way of asking.
//!
//! Where the cgroups are comes from `/proc/self/mountinfo`: the `cgroup2` mount, and the v1
//! `cgroup` mount whose super options list `memory` (alone or co-mounted, e.g. `cpuset,memory`).
//! The path in `/proc/self/cgroup` is taken relative to the mount's root (a container may see only
//! its own subtree, mounted at `/sys/fs/cgroup`). When mountinfo can't be read, the conventional
//! layout is assumed (v2 at [`CGROUP_ROOT`], v1 at `CGROUP_ROOT/memory`); when it can be read and
//! lists no cgroup mount, there is no cgroup file system to read and so no limit.
//!
//! Std only, and never panics: a missing, unreadable or malformed file means "no limit known
//! there". The parsing takes file contents and a root directory, so it is tested on synthetic
//! trees ([`cgroup_memory_at`]).
#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

/// Where the cgroup file systems are mounted conventionally (used when mountinfo can't be read).
pub const CGROUP_ROOT: &str = "/sys/fs/cgroup";

/// Path components followed below a mount at most (the kernel nests far less in practice).
const MAX_DEPTH: usize = 64;

/// A limit this large is no limit: cgroup v1 reports "unlimited" as a page-rounded `i64::MAX`
/// (9223372036854771712), and no machine has an exabyte of RAM.
const UNLIMITED: u64 = 1 << 60;

/// Bytes read at most from a cgroup or `/proc` file (they are a few kB; `mountinfo` on a host
/// with tens of thousands of mounts reaches a few MB).
const READ_CAP: u64 = 16 << 20;

/// The memory controller's view of this process's cgroups.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CgroupMemory {
    /// The tightest limit (bytes) on this process's cgroup or any ancestor; `None` without one.
    pub limit: Option<u64>,
    /// Bytes the process can still be charged before it reaches a limit: the smallest
    /// `limit − (usage − inactive file cache)` over the limited cgroups (saturating at 0), with a
    /// cgroup whose usage can't be read counting its whole limit. `None` without a limit, or when
    /// usage wasn't asked for.
    pub headroom: Option<u64>,
}

/// Memory this process may use at most (bytes): physical RAM, or its cgroup limit when that is
/// smaller. Linux only; `None` elsewhere or when nothing can be read.
pub fn effective_total() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let meminfo = read(Path::new("/proc/meminfo")).unwrap_or_default();
        smaller(meminfo_bytes(&meminfo, "MemTotal"), cgroup_limit())
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
        let meminfo = read(Path::new("/proc/meminfo")).unwrap_or_default();
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
        memory_of(own_dirs(), true)
    }
    #[cfg(not(target_os = "linux"))]
    {
        CgroupMemory::default()
    }
}

/// This process's cgroup memory limit alone (Linux; nothing elsewhere): [`cgroup_memory`]
/// without reading usage, for callers that ask often (a settings page showing the budget). It
/// reads one small file per cgroup level.
pub fn cgroup_limit() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        memory_of(own_dirs(), false).limit
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// This process's cgroup directories, resolved once: cgroup membership and mounts are set when
/// a process starts (a container, a scope, a CI job) and don't change under it in practice.
#[cfg(target_os = "linux")]
fn own_dirs() -> &'static [CgroupDir] {
    static DIRS: std::sync::OnceLock<Vec<CgroupDir>> = std::sync::OnceLock::new();
    DIRS.get_or_init(|| {
        let Some(own) = read(Path::new("/proc/self/cgroup")) else {
            return Vec::new();
        };
        let mountinfo = read(Path::new("/proc/self/mountinfo"));
        cgroup_dirs(Path::new("/"), mountinfo.as_deref(), &own)
    })
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

/// A cgroup file system mount, from a `/proc/self/mountinfo` line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CgroupMount {
    /// The cgroup shown at the mount point (field 4): `/`, or e.g. `/docker/abc` in a container
    /// that sees only its own subtree.
    pub root: String,
    /// Where it is mounted (field 5), e.g. `/sys/fs/cgroup`.
    pub mount_point: String,
    /// A v1 hierarchy with the `memory` controller (`cgroup`), rather than v2 (`cgroup2`).
    pub v1: bool,
}

/// The cgroup mounts that can hold memory limits, in `/proc/self/mountinfo` order: every `cgroup2`
/// mount, and the `cgroup` (v1) mounts with `memory` among their super options. Malformed lines
/// and other file systems are skipped.
pub fn cgroup_mounts(mountinfo: &str) -> Vec<CgroupMount> {
    mountinfo.lines().filter_map(parse_mount).collect()
}

/// One mountinfo line: `id parent major:minor root mount-point options [optional…] - fstype
/// source super-options`, with spaces, tabs, newlines and backslashes in paths octal-escaped.
fn parse_mount(line: &str) -> Option<CgroupMount> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    let sep = fields.iter().position(|f| *f == "-")?;
    if sep < 6 {
        return None;
    }
    let v1 = match *fields.get(sep + 1)? {
        "cgroup2" => false,
        "cgroup" if fields.get(sep + 3)?.split(',').any(|o| o == "memory") => true,
        _ => return None,
    };
    Some(CgroupMount { root: unescape_mount_path(fields.get(3)?), mount_point: unescape_mount_path(fields.get(4)?), v1 })
}

/// A mountinfo path with its `\ooo` octal escapes (`\040` space, `\011` tab, `\012` newline,
/// `\134` backslash) decoded; anything else, including a `\` not followed by three octal digits
/// of a byte, is kept as it is.
pub fn unescape_mount_path(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        let escaped = (b == b'\\').then(|| bytes.get(i + 1..i + 4).and_then(octal_byte)).flatten();
        match escaped {
            Some(v) => {
                out.push(v);
                i += 4;
            }
            None => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Three octal digits as a byte (`None` past `\377` or for any other digit).
fn octal_byte(digits: &[u8]) -> Option<u8> {
    let v = digits.iter().try_fold(0u16, |acc, &d| matches!(d, b'0'..=b'7').then(|| acc * 8 + u16::from(d - b'0')))?;
    u8::try_from(v).ok()
}

/// A cgroup directory whose memory files are read.
#[derive(Clone, Debug, PartialEq, Eq)]
struct CgroupDir {
    path: PathBuf,
    /// v1 file names (`memory.limit_in_bytes`, …) rather than v2 (`memory.max`, …).
    v1: bool,
}

/// The memory limit and headroom of the process whose `/proc/self/cgroup` is `own`, with
/// `/proc/self/mountinfo` reading `mountinfo` (`None`: unreadable, so the conventional layout)
/// and the file system's root at `fs_root` (normally `/`).
pub fn cgroup_memory_at(fs_root: &Path, mountinfo: Option<&str>, own: &str) -> CgroupMemory {
    memory_of(&cgroup_dirs(fs_root, mountinfo, own), true)
}

/// The directories whose memory files bind this process: for the cgroup v2 entry (`0::/path`)
/// and a v1 entry with the `memory` controller (`N:cpuset,memory:/path`), the cgroup's directory
/// and each parent up to the mount point, deepest first, in every mount of that hierarchy that
/// shows the cgroup ([`views`]). A cgroup seen through two mounts (a bind mount of a subtree beside
/// the whole hierarchy) is listed once.
fn cgroup_dirs(fs_root: &Path, mountinfo: Option<&str>, own: &str) -> Vec<CgroupDir> {
    let mounts = match mountinfo {
        Some(info) => cgroup_mounts(info),
        None => vec![
            CgroupMount { root: "/".into(), mount_point: CGROUP_ROOT.into(), v1: false },
            CgroupMount { root: "/".into(), mount_point: format!("{CGROUP_ROOT}/memory"), v1: true },
        ],
    };
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for line in own.lines() {
        let mut fields = line.trim().splitn(3, ':');
        let (Some(_id), Some(controllers), Some(path)) = (fields.next(), fields.next(), fields.next()) else {
            continue;
        };
        let v1 = if controllers.is_empty() {
            false
        } else if controllers.split(',').any(|c| c == "memory") {
            true
        } else {
            continue;
        };
        for view in views(&mounts, v1, path) {
            let Some(point) = plain_components(&view.mount.mount_point) else {
                continue;
            };
            let mut base = fs_root.to_path_buf();
            base.extend(point);
            for (up, dir) in ancestors(&base, &view.rel).into_iter().enumerate() {
                // the cgroup this directory shows: the mount's root, then `rel` less `up` levels
                let cgroup = match &view.root {
                    Some(root) => {
                        let rel = view.rel.iter().take(view.rel.len().saturating_sub(up));
                        let parts: Vec<&str> = root.iter().map(String::as_str).chain(rel.map(String::as_str)).collect();
                        format!("/{}", parts.join("/"))
                    }
                    // a root that isn't a plain path: known only by where it is mounted
                    None => format!("mount:{}", dir.display()),
                };
                if seen.insert((v1, cgroup)) {
                    out.push(CgroupDir { path: dir, v1 });
                }
            }
        }
    }
    out
}

/// The cgroup `path` as one mount shows it.
struct View<'m> {
    mount: &'m CgroupMount,
    /// The cgroup at the mount point (the mount's root), when it is a plain path.
    root: Option<Vec<String>>,
    /// `path` below the mount's root (at most [`MAX_DEPTH`] components).
    rel: Vec<String>,
}

/// Every mount of the right version whose root holds the cgroup `path`, with `path` relative to
/// it: a limit on an ancestor binds even when a closer mount (a bind mount of a subtree) shows
/// the cgroup too, so each is read. When no mount's root holds `path` (a container without a
/// cgroup namespace sees the host's path), the first mount's top alone: what such a container
/// mounts is its own cgroup, and a limit read there can only make the result tighter, where
/// reading nothing would size the process from the machine's RAM.
fn views<'m>(mounts: &'m [CgroupMount], v1: bool, path: &str) -> Vec<View<'m>> {
    let own = plain_components(path);
    let owned = |parts: &[&str]| parts.iter().map(|p| (*p).to_owned()).collect::<Vec<String>>();
    let mut out = Vec::new();
    let mut fallback = None;
    for mount in mounts.iter().filter(|m| m.v1 == v1) {
        fallback = fallback.or(Some(mount));
        let (Some(own), Some(root)) = (own.as_ref(), plain_components(&mount.root)) else {
            continue;
        };
        if own.starts_with(&root) {
            let rel = own.get(root.len()..).unwrap_or_default().iter().take(MAX_DEPTH).map(|p| (*p).to_owned()).collect();
            out.push(View { mount, root: Some(owned(&root)), rel });
        }
    }
    if out.is_empty()
        && let Some(mount) = fallback
    {
        out.push(View { mount, root: plain_components(&mount.root).map(|r| owned(&r)), rel: Vec::new() });
    }
    out
}

/// The non-empty components of a `/`-separated path, or `None` if any is `.` or `..` (never
/// followed: a path must not climb out of its mount).
fn plain_components(path: &str) -> Option<Vec<&str>> {
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    parts.iter().all(|p| *p != ".." && *p != ".").then_some(parts)
}

/// `base` joined with each prefix of `rel`, deepest first, `base` itself last.
fn ancestors(base: &Path, rel: &[String]) -> Vec<PathBuf> {
    let mut dirs = vec![base.to_path_buf()];
    let mut dir = base.to_path_buf();
    for part in rel.iter().take(MAX_DEPTH) {
        dir.push(part);
        dirs.push(dir.clone());
    }
    dirs.reverse();
    dirs
}

/// The tightest limit over `dirs`, and with `usage` the smallest headroom under them.
fn memory_of(dirs: &[CgroupDir], usage: bool) -> CgroupMemory {
    let mut out = CgroupMemory::default();
    for dir in dirs {
        let (limit_file, usage_file, inactive_key) = if dir.v1 {
            ("memory.limit_in_bytes", "memory.usage_in_bytes", "total_inactive_file")
        } else {
            ("memory.max", "memory.current", "inactive_file")
        };
        let Some(limit) = read(&dir.path.join(limit_file)).as_deref().and_then(parse_limit) else {
            continue;
        };
        out.limit = smaller(out.limit, Some(limit));
        if !usage {
            continue;
        }
        // usage unreadable: the limit still caps the room (or `available` would fall back to the
        // host's MemAvailable)
        let room = match read(&dir.path.join(usage_file)).as_deref().and_then(parse_usage) {
            Some(used) => {
                let stat = read(&dir.path.join("memory.stat")).unwrap_or_default();
                limit.saturating_sub(used.saturating_sub(stat_value(&stat, inactive_key).unwrap_or(0)))
            }
            None => limit,
        };
        out.headroom = smaller(out.headroom, Some(room));
    }
    out
}

/// A text file, at most [`READ_CAP`] bytes of it (a cut-off last line is dropped), invalid UTF-8
/// replaced.
fn read(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path).ok()?.take(READ_CAP).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 == READ_CAP {
        let end = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
        bytes.truncate(end);
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
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

        /// Write `rel` under the conventional cgroup mount (`sys/fs/cgroup/` below the scratch root).
        fn write(&self, rel: &str, contents: &str) {
            self.write_at(&format!("sys/fs/cgroup/{rel}"), contents);
        }

        /// Write `rel` below the scratch root (the synthetic file system's `/`).
        fn write_at(&self, rel: &str, contents: &str) {
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
        let got = cgroup_memory_at(&s.0, None, "0::/user.slice/app.scope\n");
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
        let got = cgroup_memory_at(&s.0, None, "0::/a/b/c/d");
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
        let got = cgroup_memory_at(&s.0, None, "0::/p/c");
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
        assert_eq!(cgroup_memory_at(&s.0, None, "0::/x"), CgroupMemory { limit: Some(4 * GIB), headroom: Some(2 * GIB) });
        // a malformed stat file counts no cache, and more cache than usage leaves the whole limit
        s.write("x/memory.stat", "inactive_file -1\n");
        assert_eq!(cgroup_memory_at(&s.0, None, "0::/x").headroom, Some(GIB));
        s.write("x/memory.stat", &format!("inactive_file {}\n", 9 * GIB));
        assert_eq!(cgroup_memory_at(&s.0, None, "0::/x").headroom, Some(4 * GIB));

        // v1 counts the hierarchy's cache under `total_inactive_file`
        s.write("memory/y/memory.limit_in_bytes", &(2 * GIB).to_string());
        s.write("memory/y/memory.usage_in_bytes", &(2 * GIB).to_string());
        s.write("memory/y/memory.stat", &format!("inactive_file 1\ntotal_inactive_file {}\n", GIB));
        assert_eq!(cgroup_memory_at(&s.0, None, "4:memory:/y").headroom, Some(GIB));
    }

    #[test]
    fn usage_over_the_limit_leaves_no_headroom_and_unreadable_usage_the_limit() {
        let s = Scratch::new("v2-over");
        s.write("x/memory.max", "1000");
        s.write("x/memory.current", "5000");
        assert_eq!(cgroup_memory_at(&s.0, None, "0::/x"), CgroupMemory { limit: Some(1000), headroom: Some(0) });
        // without usage the limit still caps the headroom, so `available` doesn't fall back to
        // the host's MemAvailable
        s.write("x/memory.current", "garbage");
        assert_eq!(cgroup_memory_at(&s.0, None, "0::/x"), CgroupMemory { limit: Some(1000), headroom: Some(1000) });
        // and a child whose usage can't be read is not hidden by a parent whose usage can
        s.write("memory.max", "5000");
        s.write("memory.current", "100");
        assert_eq!(cgroup_memory_at(&s.0, None, "0::/x"), CgroupMemory { limit: Some(1000), headroom: Some(1000) });
    }

    #[test]
    fn the_limit_only_walk_skips_usage() {
        let s = Scratch::new("v2-limit-only");
        s.write("a/memory.max", &(4 * GIB).to_string());
        s.write("a/memory.current", &GIB.to_string());
        s.write("a/b/memory.max", &(2 * GIB).to_string());
        assert_eq!(memory_of(&cgroup_dirs(&s.0, None, "0::/a/b\n"), false), CgroupMemory { limit: Some(2 * GIB), headroom: None });
        // /a has 3 GiB of room; /b's usage can't be read, so its whole 2 GiB limit caps it
        assert_eq!(memory_of(&cgroup_dirs(&s.0, None, "0::/a/b\n"), true), CgroupMemory { limit: Some(2 * GIB), headroom: Some(2 * GIB) });
    }

    #[test]
    fn no_limit_anywhere_or_missing_files_mean_no_limit() {
        let s = Scratch::new("v2-none");
        s.write("u/memory.max", "max\n");
        s.write("u/memory.current", "123");
        assert_eq!(cgroup_memory_at(&s.0, None, "0::/u\n"), CgroupMemory::default());
        // the cgroup directory is gone (or deleted) and the root has no memory.max
        assert_eq!(cgroup_memory_at(&s.0, None, "0::/gone/away (deleted)\n"), CgroupMemory::default());
        // no cgroup file system at all
        assert_eq!(cgroup_memory_at(&s.0.join("absent"), None, "0::/u\n"), CgroupMemory::default());
        // garbage /proc/self/cgroup
        assert_eq!(cgroup_memory_at(&s.0, None, ""), CgroupMemory::default());
        assert_eq!(cgroup_memory_at(&s.0, None, "nonsense\n\n::\n7"), CgroupMemory::default());
    }

    #[test]
    fn a_namespaced_root_with_its_own_limit_is_read() {
        // In a container with a cgroup namespace, /proc/self/cgroup says "/" and the mount's root
        // is the container's cgroup, limit included.
        let s = Scratch::new("v2-ns");
        s.write("memory.max", "1073741824\n");
        s.write("memory.current", "0\n");
        assert_eq!(cgroup_memory_at(&s.0, None, "0::/\n"), CgroupMemory { limit: Some(GIB), headroom: Some(GIB) });
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
        assert_eq!(cgroup_memory_at(&s.0, None, own), CgroupMemory { limit: Some(GIB / 2), headroom: Some(GIB / 4) });
        // memory co-mounted with other controllers
        assert_eq!(cgroup_memory_at(&s.0, None, "3:cpuset,memory:/docker/abc\n"), CgroupMemory { limit: Some(GIB / 2), headroom: Some(GIB / 4) });
        // a v1 host without a memory limit
        assert_eq!(cgroup_memory_at(&s.0, None, "11:memory:/other\n"), CgroupMemory::default());
    }

    #[test]
    fn a_v1_container_whose_host_path_does_not_exist_reads_the_mount_root() {
        let s = Scratch::new("v1-ns");
        s.write("memory/memory.limit_in_bytes", "268435456");
        s.write("memory/memory.usage_in_bytes", "0");
        assert_eq!(cgroup_memory_at(&s.0, None, "9:memory:/kubepods/pod1/abc\n"), CgroupMemory { limit: Some(GIB / 4), headroom: Some(GIB / 4) });
    }

    #[test]
    fn a_hybrid_host_takes_the_tightest_of_both_hierarchies() {
        let s = Scratch::new("hybrid");
        s.write("memory/a/memory.limit_in_bytes", &(3 * GIB).to_string());
        s.write("a/memory.max", &(2 * GIB).to_string());
        let got = cgroup_memory_at(&s.0, None, "5:memory:/a\n0::/a\n");
        assert_eq!(got.limit, Some(2 * GIB));
        assert_eq!(got.headroom, Some(2 * GIB), "no usage files: the limit caps it");
    }

    #[test]
    fn paths_that_climb_out_are_not_followed() {
        let s = Scratch::new("dotdot");
        let outside = Scratch::new("dotdot-outside");
        outside.write("memory.max", "1");
        let name = outside.0.file_name().and_then(|n| n.to_str()).unwrap();
        // from <s>/sys/fs/cgroup, four levels up is the temp dir holding both scratch roots
        let own = format!("0::/../../../../{name}/sys/fs/cgroup\n");
        assert_eq!(cgroup_memory_at(&s.0, None, &own), CgroupMemory::default());
        assert_eq!(plain_components("/a/./b"), None);
        assert_eq!(plain_components("/a/../b"), None);
        // a mount point or root that climbs is not used either
        outside.write_at("m/memory.max", "1");
        let info = format!("1 0 0:1 / /../{name}/m rw - cgroup2 cgroup2 rw\n");
        assert_eq!(cgroup_memory_at(&s.0, Some(&info), "0::/\n"), CgroupMemory::default());
    }

    #[test]
    fn the_walk_is_bounded_and_ordered_deepest_first() {
        let rel = |p: &str| plain_components(p).unwrap().into_iter().map(str::to_owned).collect::<Vec<_>>();
        assert_eq!(ancestors(Path::new("/r"), &rel("/a//b/")), vec![PathBuf::from("/r/a/b"), PathBuf::from("/r/a"), PathBuf::from("/r")]);
        let deep = "/x".repeat(10_000);
        assert_eq!(ancestors(Path::new("/r"), &rel(&deep)).len(), MAX_DEPTH + 1);
        assert_eq!(cgroup_dirs(Path::new("/"), None, &format!("0::{deep}")).len(), MAX_DEPTH + 1);
    }

    /// A mountinfo line for a cgroup mount: `root` shown at `point`.
    fn mount(id: u32, root: &str, point: &str, fstype: &str, opts: &str) -> String {
        format!("{id} 1 0:{id} {root} {point} rw,nosuid,nodev,noexec,relatime shared:{id} - {fstype} cgroup {opts}\n")
    }

    #[test]
    fn mountinfo_lines_are_parsed_with_escapes_optional_fields_and_garbage() {
        let info = [
            "22 1 8:1 / / rw,relatime - ext4 /dev/sda1 rw".to_owned(),
            mount(30, "/", "/sys/fs/cgroup", "cgroup2", "rw,nsdelegate,memory_recursiveprot"),
            // no optional fields, an escaped space and backslash in the mount point
            "31 1 0:31 /a\\134b /mnt/cg\\040root rw - cgroup2 cgroup2 rw".to_owned(),
            // several optional fields
            "32 1 0:32 / /sys/fs/cgroup/memory rw shared:9 master:3 propagate_from:2 - cgroup cgroup rw,memory".to_owned(),
            // v1 without the memory controller, and one whose options only contain the word
            mount(33, "/", "/sys/fs/cgroup/cpu", "cgroup", "rw,cpu,cpuacct"),
            mount(34, "/", "/sys/fs/cgroup/x", "cgroup", "rw,name=memory"),
            // garbage: no separator, separator too early, nothing after it, a bad escape kept as is
            "35 1 0:35 / /x rw cgroup2 cgroup2 rw".to_owned(),
            "36 1 - cgroup2 cgroup2 rw".to_owned(),
            "37 1 0:37 / /y rw -".to_owned(),
            "38 1 0:38 /\\777 /z\\04 rw - cgroup2 cgroup2 rw".to_owned(),
            String::new(),
            "\u{fffd}\u{0} - - - - - - -".to_owned(),
        ]
        .join("\n");
        let m = |root: &str, point: &str, v1| CgroupMount { root: root.into(), mount_point: point.into(), v1 };
        assert_eq!(
            cgroup_mounts(&info),
            vec![
                m("/", "/sys/fs/cgroup", false),
                m("/a\\b", "/mnt/cg root", false),
                m("/", "/sys/fs/cgroup/memory", true),
                m("/\\777", "/z\\04", false),
            ]
        );
        assert_eq!(unescape_mount_path("a\\011b\\012c"), "a\tb\nc");
        assert_eq!(unescape_mount_path("\\400\\"), "\\400\\", "past a byte, or cut short");
        assert_eq!(unescape_mount_path("\\303\\251"), "\u{e9}", "escaped UTF-8 bytes");
        assert_eq!(unescape_mount_path("\\377"), "\u{fffd}", "an invalid byte is replaced");
    }

    #[test]
    fn a_plain_v2_host_is_found_through_mountinfo() {
        let s = Scratch::new("mi-v2");
        s.write("user.slice/app.scope/memory.max", &(2 * GIB).to_string());
        s.write("user.slice/app.scope/memory.current", &GIB.to_string());
        s.write("user.slice/memory.max", &GIB.to_string());
        let info = mount(30, "/", "/sys/fs/cgroup", "cgroup2", "rw,nsdelegate");
        let got = cgroup_memory_at(&s.0, Some(&info), "0::/user.slice/app.scope\n");
        assert_eq!(got, CgroupMemory { limit: Some(GIB), headroom: Some(GIB) });
    }

    #[test]
    fn a_co_mounted_v1_memory_controller_is_found_where_it_is_mounted() {
        // `cpuset,memory` mounted together, with no `memory` directory beside it
        let s = Scratch::new("mi-comount");
        s.write("cpuset,memory/docker/abc/memory.limit_in_bytes", &GIB.to_string());
        s.write("cpuset,memory/docker/abc/memory.usage_in_bytes", &(GIB / 4).to_string());
        s.write("cpuset,memory/memory.limit_in_bytes", "9223372036854771712");
        let info = [
            mount(30, "/", "/sys/fs/cgroup/cpu,cpuacct", "cgroup", "rw,cpu,cpuacct"),
            mount(31, "/", "/sys/fs/cgroup/cpuset,memory", "cgroup", "rw,cpuset,memory"),
        ]
        .concat();
        let own = "4:cpu,cpuacct:/docker/abc\n3:cpuset,memory:/docker/abc\n";
        assert_eq!(cgroup_memory_at(&s.0, Some(&info), own), CgroupMemory { limit: Some(GIB), headroom: Some(GIB / 4 * 3) });
        // the conventional layout alone misses it
        assert_eq!(cgroup_memory_at(&s.0, None, own), CgroupMemory::default());
    }

    #[test]
    fn a_subtree_mounted_at_the_mount_point_is_read_relative_to_its_root() {
        // A container that sees only /docker/abc of the host's hierarchy at /sys/fs/cgroup, while
        // /proc/self/cgroup still names the host path: the 1 GiB child binds, not the 4 GiB top.
        let s = Scratch::new("mi-subtree");
        s.write("memory.max", &(4 * GIB).to_string());
        s.write("child/memory.max", &GIB.to_string());
        s.write("child/memory.current", "0");
        let info = mount(30, "/docker/abc", "/sys/fs/cgroup", "cgroup2", "rw");
        let own = "0::/docker/abc/child\n";
        assert_eq!(cgroup_memory_at(&s.0, Some(&info), own), CgroupMemory { limit: Some(GIB), headroom: Some(GIB) });
        // the conventional layout alone (the code before #3072's mountinfo reading) reads the top
        assert_eq!(cgroup_memory_at(&s.0, None, own).limit, Some(4 * GIB));
        // the same for a v1 memory hierarchy
        s.write("memory/memory.limit_in_bytes", &(4 * GIB).to_string());
        s.write("memory/child/memory.limit_in_bytes", &(GIB / 2).to_string());
        let info = mount(31, "/docker/abc", "/sys/fs/cgroup/memory", "cgroup", "rw,memory");
        assert_eq!(cgroup_memory_at(&s.0, Some(&info), "7:memory:/docker/abc/child\n").limit, Some(GIB / 2));
        // a membership path outside the mount's root reads the mount point alone
        assert_eq!(
            cgroup_memory_at(&s.0, Some(&mount(30, "/docker/abc", "/sys/fs/cgroup", "cgroup2", "rw")), "0::/elsewhere/child\n").limit,
            Some(4 * GIB)
        );
    }

    #[test]
    fn every_mount_whose_root_holds_the_path_is_read() {
        let s = Scratch::new("mi-several");
        s.write_at("a/memory.max", &(8 * GIB).to_string());
        // /b shows cgroup /p, so /b/x is the same cgroup as /a/p/x
        for x in ["a/p/x", "b/x"] {
            s.write_at(&format!("{x}/memory.max"), &(2 * GIB).to_string());
        }
        s.write_at("c/memory.max", &GIB.to_string());
        let info = [mount(30, "/", "/a", "cgroup2", "rw"), mount(31, "/p", "/b", "cgroup2", "rw"), mount(32, "/q", "/c", "cgroup2", "rw")].concat();
        // /p/x is under the roots of /a (as /a/p/x) and of /b (as /b/x), not of /c (root /q)
        let dirs: Vec<PathBuf> = cgroup_dirs(&s.0, Some(&info), "0::/p/x\n").into_iter().map(|d| d.path).collect();
        let expected: Vec<PathBuf> = ["a/p/x", "a/p", "a"].iter().map(|d| s.0.join(d)).collect();
        assert_eq!(dirs, expected, "/p/x and /p are read once, through the first mount that shows them");
        assert_eq!(cgroup_memory_at(&s.0, Some(&info), "0::/p/x\n").limit, Some(2 * GIB), "/c's 1 GiB is another cgroup");
        let dirs: Vec<PathBuf> =
            cgroup_dirs(&s.0, Some(&[mount(31, "/p", "/b", "cgroup2", "rw"), mount(30, "/", "/a", "cgroup2", "rw")].concat()), "0::/p/x\n")
                .into_iter()
                .map(|d| d.path)
                .collect();
        let expected: Vec<PathBuf> = ["b/x", "b", "a"].iter().map(|d| s.0.join(d)).collect();
        assert_eq!(dirs, expected, "the other order: /a shows only / that /b doesn't");
    }

    #[test]
    fn a_bind_mounted_subtree_does_not_hide_an_ancestor_limit() {
        // The whole hierarchy at /sys/fs/cgroup and /p/c bind-mounted at /mnt/c: /p's 1 GiB binds
        // the process in /p/c/child although /p/c (seen at /mnt/c, closest) says 4 GiB.
        let s = Scratch::new("mi-bind");
        s.write("p/memory.max", &GIB.to_string());
        s.write("p/memory.current", &(GIB / 2).to_string());
        for c in ["sys/fs/cgroup/p/c", "mnt/c"] {
            s.write_at(&format!("{c}/memory.max"), &(4 * GIB).to_string());
            s.write_at(&format!("{c}/memory.current"), &(GIB / 4).to_string());
            s.write_at(&format!("{c}/child/memory.max"), "max");
        }
        let full = mount(30, "/", "/sys/fs/cgroup", "cgroup2", "rw");
        let bind = mount(31, "/p/c", "/mnt/c", "cgroup2", "rw");
        let own = "0::/p/c/child\n";
        for info in [format!("{full}{bind}"), format!("{bind}{full}")] {
            assert_eq!(cgroup_memory_at(&s.0, Some(&info), own), CgroupMemory { limit: Some(GIB), headroom: Some(GIB / 2) }, "{info}");
            // /p/c/child, /p/c, /p and / once each
            assert_eq!(cgroup_dirs(&s.0, Some(&info), own).len(), 4, "{info}");
        }
    }

    #[test]
    fn a_mountinfo_without_cgroup_mounts_means_no_limit() {
        // Readable mountinfo is the truth: files at /sys/fs/cgroup that aren't on a cgroup mount
        // aren't cgroups. Only an unreadable mountinfo falls back to the conventional layout.
        let s = Scratch::new("mi-none");
        s.write("memory.max", &GIB.to_string());
        let info = "22 1 8:1 / / rw,relatime - ext4 /dev/sda1 rw\n23 22 0:5 / /proc rw - proc proc rw\n";
        assert_eq!(cgroup_memory_at(&s.0, Some(info), "0::/\n"), CgroupMemory::default());
        assert_eq!(cgroup_memory_at(&s.0, Some(""), "0::/\n"), CgroupMemory::default());
        assert_eq!(cgroup_memory_at(&s.0, None, "0::/\n").limit, Some(GIB));
    }

    #[test]
    fn a_read_stops_at_the_cap_on_a_whole_line() {
        let s = Scratch::new("cap");
        // 1000-byte lines: the cap falls inside one, which is dropped
        let line = "x".repeat(999) + "\n";
        let body = line.repeat(usize::try_from(READ_CAP / 1000).unwrap() + 2);
        s.write_at("big", &body);
        let got = read(&s.0.join("big")).unwrap();
        assert_eq!(got.len() as u64, READ_CAP / 1000 * 1000);
        assert!(got.ends_with('\n'));
        assert_eq!(read(&s.0.join("absent")), None);
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
        assert_eq!(cgroup_limit(), cg.limit);
        println!("cgroup limit  {:?} MiB, headroom {:?} MiB", mib(cg.limit), mib(cg.headroom));
        println!("effective_total {:?} MiB", mib(effective_total()));
        println!("available       {:?} MiB", mib(available()));
    }
}
