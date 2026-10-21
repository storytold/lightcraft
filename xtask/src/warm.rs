//! `cargo xtask warm`: start a new worktree's `target/` from a copy-on-write clone of a seed
//! checkout's, so its first build recompiles only the packages that differ.
//!
//! sccache can't share the workspace's own crates between worktrees (the absolute
//! `CARGO_MANIFEST_DIR` is part of every key; Rust support for `SCCACHE_BASEDIRS` is unreleased),
//! so every new worktree used to rebuild all of LightCraft. Cargo's own artifacts do carry over:
//! its hashes use paths relative to the workspace, and freshness is by modification time. So
//! `warm`:
//!
//! 1. clones the seed's `target/` (APFS `clonefile` on macOS, reflinks on Linux: seconds, and no
//!    disk until files are rewritten);
//! 2. gives every file whose bytes equal the seed's the seed's modification time, so cargo judges
//!    it exactly as it would in the seed;
//! 3. stamps every file of a package with any difference (added, removed, changed — fixtures
//!    too) with the current time, so that package and its dependents rebuild (an earlier warm may
//!    have given them old times).
//!
//! The whole warm holds the CI lock (`ci_lock`, as `--seed` and `ci` do), so a `ci` or
//! `warm --seed` run in the seed can't be writing artifacts while they are copied, and a `ci` in
//! this worktree can't start building on the clone before step 3 has stamped the changed files
//! (it waits for a running one first; a plain `cargo build` in either takes no lock).
//!
//! Reused crates keep the seed's `CARGO_MANIFEST_DIR` (`env!` uses, debuginfo paths): a reused
//! test reads its fixtures from the seed checkout, whose files equal this one's at warm time.
//! So `warm` records the seed's state (its `HEAD` and a hash of its uncommitted changes to
//! tracked files) in `target/.lightcraft-warm-seed`, and `cargo xtask ci` warns when the seed
//! has moved since: reused tests may then read fixtures newer or older than this worktree's.
//! `cargo xtask warm --force` re-warms. Keep the seed on a clean main and refresh it with
//! `cargo xtask warm --seed`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

/// The file in a warmed target directory that records the seed's state at warm time.
const RECORD: &str = ".lightcraft-warm-seed";

const USAGE: &str = "usage: cargo xtask warm [--from SEED_CHECKOUT] [--force] | cargo xtask warm --seed";

pub fn run(root: &Path, args: &[&str]) -> Result<(), String> {
    let force = args.contains(&"--force");
    let seed_mode = args.contains(&"--seed");
    let from_at = args.iter().position(|a| *a == "--from");
    let from = from_at.map(|i| args.get(i + 1).copied().ok_or("warm: --from needs a checkout directory")).transpose()?;
    let unknown = args.iter().enumerate().find(|(i, a)| !matches!(**a, "--force" | "--seed" | "--from") && from_at.is_none_or(|f| *i != f + 1));
    if let Some((_, other)) = unknown {
        return Err(format!("warm: unknown option `{other}`\n{USAGE}"));
    }
    if seed_mode {
        return build_seed(root);
    }
    // absolute: the record below is read back by `cargo xtask ci` from any directory, so a
    // relative `--from` would resolve against the wrong one
    let seed = absolute(&match from {
        Some(f) => PathBuf::from(f),
        None => main_worktree(root)?,
    })?;
    let same = |a: &Path, b: &Path| a.canonicalize().ok().zip(b.canonicalize().ok()).is_some_and(|(a, b)| a == b);
    if same(&seed, root) {
        return Err(format!("warm: {} is the seed checkout itself; refresh its builds with `cargo xtask warm --seed`", root.display()));
    }
    let seed_target = seed.join("target");
    if !has_workspace_builds(&seed_target) {
        return Err(format!("warm: {} has no workspace builds; run `cargo xtask warm --seed` in {}", seed_target.display(), seed.display()));
    }
    let dest = target_dir(root);
    if has_workspace_builds(&dest) && !force {
        return Err(format!("warm: {} already has workspace builds; --force replaces them", dest.display()));
    }

    // Everything that can fail runs before the clone replaces `dest`, so a failure keeps the old one.
    let t = Instant::now();
    // Held for the rest of the warm: a `ci` run in the seed (or anywhere) writes artifacts, so
    // they mustn't be copied half-written, and a `ci` in this worktree mustn't start building on
    // the clone before `stamp_times` has given the changed files the current time.
    let _slot = crate::ci_lock::acquire(root, &format!("cargo xtask warm (from {})", seed.display()))?;
    // the state the comparison below sees, recorded once the clone is in place
    let seed_state = SeedState::read(&seed)?;
    let packages = crate::ci_quick::packages(root, &crate::metadata()?);
    let ours = listed_files(root)?;
    let theirs = listed_files(&seed)?;
    let mut differ: BTreeSet<&str> = ours.symmetric_difference(&theirs).map(String::as_str).collect();
    for path in ours.intersection(&theirs) {
        if !same_bytes(&root.join(path), &seed.join(path)) {
            differ.insert(path);
        }
    }
    let rebuilt: BTreeSet<&str> = differ.iter().filter_map(|p| crate::ci_quick::owner(p, &packages)).collect();
    let compared = t.elapsed();

    let t = Instant::now();
    clone_dir(&seed_target, &dest)?;
    eprintln!("cloned {} → {} in {:.1} s", seed_target.display(), dest.display(), t.elapsed().as_secs_f64());

    let t = Instant::now();
    let dirty = |path: &str| differ.contains(path) || crate::ci_quick::owner(path, &packages).is_some_and(|o| rebuilt.contains(o));
    let stamps = stamp_times(root, &seed, &ours, dirty);
    if let Some((n, first)) = &stamps.unsynced {
        // left with its checkout time, newer than the seed's outputs: its package only rebuilds
        eprintln!("warning: {n} unchanged files keep their own timestamps (first: {first}); their packages may rebuild");
    }
    if let Some((n, first)) = &stamps.unstamped {
        return Err(format!(
            "warm: could not give {n} changed files the current time (first: {first}); an older time could make a stale \
             build look fresh. Fix that and run `cargo xtask warm --force` again"
        ));
    }
    eprintln!(
        "{} unchanged files take the seed's timestamps ({} differ; {:.1} s)",
        stamps.synced,
        differ.len(),
        (compared + t.elapsed()).as_secs_f64()
    );
    let record = dest.join(RECORD);
    if let Err(e) = std::fs::write(&record, seed_state.record(&seed)) {
        eprintln!("warning: {}: {e}; `cargo xtask ci` can't tell when the seed moves", record.display());
    }
    if rebuilt.is_empty() {
        eprintln!("no package differs from the seed: builds the seed has done are fresh here");
    } else {
        eprintln!("differ from the seed, rebuilt with their dependents on the next build: {}", rebuilt.into_iter().collect::<Vec<_>>().join(", "));
    }
    Ok(())
}

/// The target directory cargo builds into from `root` (`CARGO_TARGET_DIR`, else `target/`).
fn target_dir(root: &Path) -> PathBuf {
    match std::env::var_os("CARGO_TARGET_DIR") {
        Some(d) if Path::new(&d).is_absolute() => PathBuf::from(d),
        Some(d) => root.join(d),
        None => root.join("target"),
    }
}

/// `path` resolved to an absolute path (symlinks and `..` too), so it still names the same
/// directory when `cargo xtask ci` reads it back from another working directory.
fn absolute(path: &Path) -> Result<PathBuf, String> {
    path.canonicalize().map_err(|e| format!("warm: {}: {e}", path.display()))
}

/// A seed checkout's state: what its reused test binaries read fixtures from.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SeedState {
    head: String,
    /// FNV-1a hash of `git diff HEAD` (uncommitted changes to tracked files); `None` when clean.
    changes: Option<u64>,
}

impl SeedState {
    fn read(seed: &Path) -> Result<Self, String> {
        let git = |args: &[&str]| -> Result<Vec<u8>, String> {
            let out = Command::new("git")
                .args(args)
                .current_dir(seed)
                .output()
                .map_err(|e| format!("git {} in {}: {e}", args.join(" "), seed.display()))?;
            if !out.status.success() {
                return Err(format!("git {} in {}: {}", args.join(" "), seed.display(), String::from_utf8_lossy(&out.stderr).trim()));
            }
            Ok(out.stdout)
        };
        let head = String::from_utf8_lossy(&git(&["rev-parse", "HEAD"])?).trim().to_owned();
        let diff = git(&["diff", "HEAD", "--no-ext-diff", "--no-textconv", "--no-color", "--binary"])?;
        Ok(SeedState { head, changes: (!diff.is_empty()).then(|| fnv1a(&diff)) })
    }

    fn describe(&self) -> String {
        let short: String = self.head.chars().take(10).collect();
        if self.changes.is_some() { format!("{short} with uncommitted changes") } else { format!("{short}, clean") }
    }

    /// The record file's text: `key<TAB>value` lines.
    fn record(&self, seed: &Path) -> String {
        let changes = self.changes.map_or_else(|| "clean".to_owned(), |h| format!("{h:016x}"));
        format!("seed\t{}\nhead\t{}\nchanges\t{changes}\n", seed.display(), self.head)
    }

    /// Parse [`SeedState::record`]'s text: the seed checkout and its state, `None` if malformed.
    fn parse(text: &str) -> Option<(PathBuf, SeedState)> {
        let field = |key: &str| text.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix('\t'));
        let seed = PathBuf::from(field("seed")?);
        let head = field("head")?.to_owned();
        let changes = match field("changes")? {
            "clean" => None,
            h => Some(u64::from_str_radix(h, 16).ok()?),
        };
        (!head.is_empty()).then_some((seed, SeedState { head, changes }))
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3))
}

/// The warning for a target warmed from `seed` in state `then`, now in `now` (`Err`: unreadable).
fn drift_warning(seed: &Path, then: &SeedState, now: Result<SeedState, String>) -> Option<String> {
    let now = match now {
        Ok(now) if now == *then => return None,
        Ok(now) => now.describe(),
        Err(e) => format!("unknown ({e})"),
    };
    Some(format!(
        "warning: this target/ was warmed from {} at {}; the seed is now at {now}.\n  \
         Test binaries reused from the warm read their fixtures from the seed checkout, so they may see \
         files that differ from this worktree's.\n  Run `cargo xtask warm --force` to re-warm (after \
         `cargo xtask warm --seed` in the seed, if its builds are stale).",
        seed.display(),
        then.describe()
    ))
}

/// For `cargo xtask ci`: a warning when this checkout's target directory was warmed and the seed
/// has moved since (see the module docs). Never fails.
pub fn seed_drift(root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(target_dir(root).join(RECORD)).ok()?;
    let (seed, then) = SeedState::parse(&text)?;
    drift_warning(&seed, &then, SeedState::read(&seed))
}

/// `--seed`: the builds `cargo xtask ci` does first (clippy, the workspace tests), with its
/// settings, into this checkout's `target/`.
fn build_seed(root: &Path) -> Result<(), String> {
    let _slot = crate::ci_lock::acquire(root, "cargo xtask warm --seed")?;
    for args in [&["clippy", "--workspace", "--all-targets"][..], &["test", "--workspace", "--no-run"][..]] {
        let mut c = crate::cargo();
        c.args(args).env_remove("CARGO_TARGET_DIR");
        crate::ci_env(&mut c);
        crate::run(c, &format!("cargo {}", args.join(" ")))?;
    }
    eprintln!("seed ready: `cargo xtask warm` in a new worktree now starts from {}", root.join("target").display());
    Ok(())
}

/// The main checkout of the repository: the first entry of `git worktree list`.
fn main_worktree(root: &Path) -> Result<PathBuf, String> {
    let out =
        Command::new("git").args(["worktree", "list", "--porcelain"]).current_dir(root).output().map_err(|e| format!("git worktree list: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .find_map(|l| l.strip_prefix("worktree "))
        .map(PathBuf::from)
        .ok_or_else(|| "warm: no main worktree found; pass --from SEED_CHECKOUT".to_owned())
}

/// Whether `target` holds builds of workspace packages (not only xtask's own).
fn has_workspace_builds(target: &Path) -> bool {
    std::fs::read_dir(target.join("debug").join(".fingerprint"))
        .map(|dir| dir.flatten().any(|e| e.file_name().to_string_lossy().starts_with("lightcraft-")))
        .unwrap_or(false)
}

/// Clone `from` to `to` copy-on-write, replacing `to`. The old directory is renamed aside first:
/// the running xtask binary may live in it (fine to unlink on Unix).
fn clone_dir(from: &Path, to: &Path) -> Result<(), String> {
    let name = to.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "target".into());
    let tmp = to.with_file_name(format!("{name}.warm-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let mut c = if cfg!(target_os = "macos") {
        let mut c = Command::new("cp");
        c.arg("-c");
        c
    } else if cfg!(target_os = "linux") {
        // GNU cp, unlike macOS `cp -c`, gives copies the current time unless asked: a seed whose
        // sources are newer than its outputs would then look fresh in the clone
        let mut c = Command::new("cp");
        c.args(["--reflink=always", "--preserve=timestamps"]);
        c
    } else {
        return Err("warm: needs copy-on-write clones (macOS APFS, or reflinks on Linux btrfs / XFS)".into());
    };
    c.arg("-R").arg(from).arg(&tmp);
    let out = c.output().map_err(|e| format!("cp: {e}"))?;
    if !out.status.success() {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(format!(
            "warm: copy-on-write clone failed (does this file system support it?): {}",
            String::from_utf8_lossy(&out.stderr).lines().next().unwrap_or("")
        ));
    }
    let old = to.with_file_name(format!("{name}.old-{}", std::process::id()));
    if to.exists() {
        std::fs::rename(to, &old).map_err(|e| format!("warm: move {} aside: {e}", to.display()))?;
    }
    std::fs::rename(&tmp, to).map_err(|e| format!("warm: {}: {e}", to.display()))?;
    let _ = std::fs::remove_dir_all(&old);
    Ok(())
}

/// Tracked and untracked (not ignored) files of a checkout, relative paths.
fn listed_files(checkout: &Path) -> Result<BTreeSet<String>, String> {
    let out = Command::new("git")
        .args(["ls-files", "-z", "--cached", "--others", "--exclude-standard"])
        .current_dir(checkout)
        .output()
        .map_err(|e| format!("git ls-files in {}: {e}", checkout.display()))?;
    if !out.status.success() {
        return Err(format!("git ls-files in {}: {}", checkout.display(), String::from_utf8_lossy(&out.stderr).trim()));
    }
    // deleted but still in the index: not a file here
    Ok(String::from_utf8_lossy(&out.stdout).split('\0').filter(|p| !p.is_empty() && checkout.join(p).is_file()).map(str::to_owned).collect())
}

fn same_bytes(a: &Path, b: &Path) -> bool {
    let (Ok(ma), Ok(mb)) = (std::fs::metadata(a), std::fs::metadata(b)) else { return false };
    ma.len() == mb.len() && matches!((std::fs::read(a), std::fs::read(b)), (Ok(x), Ok(y)) if x == y)
}

/// What [`stamp_times`] did: how many files took the seed's time, and the files it couldn't
/// stamp (count, first path) among the unchanged and the dirty ones.
#[derive(Debug, Default)]
struct Stamps {
    synced: usize,
    unsynced: Option<(usize, String)>,
    unstamped: Option<(usize, String)>,
}

/// Give each of `files` (relative to `root`) the current time if it is `dirty`, else the time of
/// the same file in `seed`.
fn stamp_times(root: &Path, seed: &Path, files: &BTreeSet<String>, dirty: impl Fn(&str) -> bool) -> Stamps {
    fn note(slot: &mut Option<(usize, String)>, path: &str) {
        match slot {
            Some((n, _)) => *n += 1,
            None => *slot = Some((1, path.to_owned())),
        }
    }
    let now = std::time::SystemTime::now();
    let mut stamps = Stamps::default();
    for path in files {
        if dirty(path) {
            if set_mtime(&root.join(path), now).is_err() {
                note(&mut stamps.unstamped, path);
            }
        } else if copy_mtime(&seed.join(path), &root.join(path)).is_ok() {
            stamps.synced += 1;
        } else {
            note(&mut stamps.unsynced, path);
        }
    }
    stamps
}

fn copy_mtime(from: &Path, to: &Path) -> std::io::Result<()> {
    set_mtime(to, std::fs::metadata(from)?.modified()?)
}

fn set_mtime(path: &Path, time: std::time::SystemTime) -> std::io::Result<()> {
    std::fs::File::options().write(true).open(path)?.set_modified(time)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_record_round_trips() {
        let seed = Path::new("/work/lightcraft");
        for state in [SeedState { head: "abc123".into(), changes: None }, SeedState { head: "def456".into(), changes: Some(0x0123_4567_89ab_cdef) }] {
            assert_eq!(SeedState::parse(&state.record(seed)), Some((seed.to_path_buf(), state)));
        }
        for bad in ["", "seed\t/x\nhead\t\nchanges\tclean\n", "seed\t/x\nhead\tabc\n", "seed\t/x\nhead\tabc\nchanges\tzz\n", "\0\u{fffd}"] {
            assert_eq!(SeedState::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_recorded_seed_path_is_absolute() {
        // a relative `--from` left as it is would be resolved from wherever `cargo xtask ci` runs
        let here = absolute(Path::new(".")).unwrap();
        assert!(here.is_absolute(), "{}", here.display());
        let src = absolute(Path::new("src")).unwrap(); // relative to this package's directory
        assert!(src.is_absolute() && src.ends_with("src"), "{}", src.display());
        let record = SeedState { head: "aaaa".into(), changes: None }.record(&src);
        assert_eq!(SeedState::parse(&record).unwrap().0, src);
        assert!(absolute(&here.join("does-not-exist")).is_err());
    }

    #[test]
    fn drift_warns_only_when_the_seed_moved() {
        let seed = Path::new("/work/lightcraft");
        let then = SeedState { head: "aaaa".into(), changes: None };
        assert_eq!(drift_warning(seed, &then, Ok(then.clone())), None);
        let moved = drift_warning(seed, &then, Ok(SeedState { head: "bbbb".into(), changes: None })).expect("moved");
        assert!(moved.contains("aaaa, clean") && moved.contains("bbbb, clean") && moved.contains("warm --force"), "{moved}");
        let edited = drift_warning(seed, &then, Ok(SeedState { head: "aaaa".into(), changes: Some(1) })).expect("edited");
        assert!(edited.contains("uncommitted changes"), "{edited}");
        let gone = drift_warning(seed, &then, Err("no such directory".into())).expect("unreadable");
        assert!(gone.contains("no such directory"), "{gone}");
    }

    #[test]
    fn seed_state_follows_commits_and_edits() {
        let dir = std::env::temp_dir().join(format!("xtask-warm-seed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
                .args(args)
                .current_dir(&dir)
                .output()
        };
        if !git(&["init", "-q"]).is_ok_and(|o| o.status.success()) {
            eprintln!("skipped seed_state_follows_commits_and_edits: no git");
            return;
        }
        std::fs::write(dir.join("fixture.txt"), "one").unwrap();
        git(&["add", "."]).unwrap();
        git(&["commit", "-q", "-m", "one"]).unwrap();
        let clean = SeedState::read(&dir).unwrap();
        assert_eq!(clean.changes, None);
        assert_eq!(SeedState::read(&dir).unwrap(), clean);
        std::fs::write(dir.join("fixture.txt"), "two").unwrap();
        let edited = SeedState::read(&dir).unwrap();
        assert!(edited.head == clean.head && edited.changes.is_some());
        std::fs::write(dir.join("fixture.txt"), "three").unwrap();
        assert_ne!(SeedState::read(&dir).unwrap(), edited, "a further edit of a dirty file");
        git(&["commit", "-q", "-am", "three"]).unwrap();
        let committed = SeedState::read(&dir).unwrap();
        assert!(committed.head != clean.head && committed.changes.is_none());
        // the record a warm writes, read back and compared with the seed as it is now
        let (seed, then) = SeedState::parse(&committed.record(&dir)).unwrap();
        assert_eq!(drift_warning(&seed, &then, SeedState::read(&seed)), None);
        std::fs::write(dir.join("fixture.txt"), "four").unwrap();
        assert!(drift_warning(&seed, &then, SeedState::read(&seed)).is_some());
        assert!(SeedState::read(&dir.join("missing")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn workspace_builds_are_told_from_xtask_only() {
        let dir = std::env::temp_dir().join(format!("xtask-warm-{}", std::process::id()));
        let fp = dir.join("debug").join(".fingerprint");
        std::fs::create_dir_all(fp.join("xtask-0123")).unwrap();
        assert!(!has_workspace_builds(&dir));
        std::fs::create_dir_all(fp.join("lightcraft-geom-4567")).unwrap();
        assert!(has_workspace_builds(&dir));
        assert!(!has_workspace_builds(&dir.join("missing")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mtime_and_bytes_helpers() {
        let dir = std::env::temp_dir().join(format!("xtask-warm-files-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b, c) = (dir.join("a"), dir.join("b"), dir.join("c"));
        std::fs::write(&a, "same").unwrap();
        std::fs::write(&b, "same").unwrap();
        std::fs::write(&c, "diff").unwrap();
        assert!(same_bytes(&a, &b) && !same_bytes(&a, &c) && !same_bytes(&a, &dir.join("none")));
        let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(946_684_800);
        std::fs::File::options().write(true).open(&a).unwrap().set_modified(old).unwrap();
        copy_mtime(&a, &b).unwrap();
        assert_eq!(std::fs::metadata(&b).unwrap().modified().unwrap(), old);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stamp_failures_are_counted() {
        let dir = std::env::temp_dir().join(format!("xtask-warm-stamps-{}", std::process::id()));
        let (root, seed) = (dir.join("root"), dir.join("seed"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&seed).unwrap();
        let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(946_684_800);
        for name in ["dirty", "kept"] {
            std::fs::write(root.join(name), name).unwrap();
            std::fs::write(seed.join(name), name).unwrap();
            std::fs::File::options().write(true).open(seed.join(name)).unwrap().set_modified(old).unwrap();
            std::fs::File::options().write(true).open(root.join(name)).unwrap().set_modified(old).unwrap();
        }
        std::fs::write(root.join("no-seed"), "x").unwrap();
        let files: BTreeSet<String> = ["dirty", "gone", "gone-too", "kept", "no-seed"].map(str::to_owned).into();
        let stamps = stamp_times(&root, &seed, &files, |p| p == "dirty" || p.starts_with("gone"));
        assert_eq!(stamps.synced, 1);
        assert_eq!(std::fs::metadata(root.join("kept")).unwrap().modified().unwrap(), old);
        assert!(std::fs::metadata(root.join("dirty")).unwrap().modified().unwrap() > old);
        assert_eq!(stamps.unsynced, Some((1, "no-seed".to_owned())));
        assert_eq!(stamps.unstamped, Some((2, "gone".to_owned())));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clone_keeps_file_times_and_replaces_the_target() {
        let dir = std::env::temp_dir().join(format!("xtask-warm-clone-{}", std::process::id()));
        let (from, to) = (dir.join("from"), dir.join("to"));
        std::fs::create_dir_all(from.join("sub")).unwrap();
        std::fs::write(from.join("sub").join("f"), "x").unwrap();
        let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(946_684_800);
        std::fs::File::options().write(true).open(from.join("sub").join("f")).unwrap().set_modified(old).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        std::fs::write(to.join("stale"), "y").unwrap();
        if let Err(e) = clone_dir(&from, &to) {
            // no copy-on-write file system under the temp dir (tmpfs, ext4)
            eprintln!("skipped clone_keeps_file_times_and_replaces_the_target: {e}");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        assert_eq!(std::fs::metadata(to.join("sub").join("f")).unwrap().modified().unwrap(), old);
        assert!(!to.join("stale").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
